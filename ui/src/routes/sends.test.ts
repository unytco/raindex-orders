// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { maxUint256 } from 'viem'
import type { SvelteComponent } from 'svelte'
import { asBuild, BUILDS, type Network } from '$lib/testing/builds'
import { CHAIN, connected } from '$lib/testing/wallet'
import { PAUSED_TEXT, STATUS_TIMEOUT_MS } from '$lib/pause'
import { SEPOLIA_REDEEMED } from './api/coupon-status/fixtures'

type Page = new (options: { target: Element }) => SvelteComponent
type Answer = (url: string, init: RequestInit) => Promise<Response>

const AGENT = 'u' + btoa(String.fromCharCode(0x84, 0x20, 0x24, ...Array(36).fill(7)))
const STATUS_FAILED = 'Could not check that the bridge is open.'
const READS: Record<string, unknown> = {
	balanceOf: 10n ** 21n,
	allowance: maxUint256,
	symbol: 'HOT',
	decimals: 18,
	minLockAmount: 1n,
	orderExists: true,
	vaultBalance: 10n ** 21n
}
const PAUSED: Answer = async () => Response.json({ paused: true })
const OPEN: Answer = async () => Response.json({ paused: false })
const UNREACHABLE: Answer = async () => Promise.reject(new TypeError('fetch failed'))
const FAILURES: [string, Answer][] = [
	['cannot be reached', UNREACHABLE],
	[
		'answers an error, even one reading not paused',
		async () => Response.json({ paused: false }, { status: 502 })
	],
	['answers no JSON', async () => new Response('<html>', { status: 200 })],
	['answers null', async () => Response.json(null)],
	['answers no status', async () => Response.json({})],
	['answers something else', async () => Response.json({ paused: 'no' })]
]

let reads: Record<string, unknown>
const wallet = {
	readContract: vi.fn(async ({ functionName }: { functionName: string }) => reads[functionName]),
	writeContract: vi.fn<[{ functionName: string }], Promise<string>>(async () => '0xabc'),
	waitForTransaction: vi.fn(async () => ({ status: '0x1' }))
}
let target: HTMLElement
let page: SvelteComponent | undefined

beforeEach(() => {
	reads = { ...READS }
	vi.spyOn(console, 'error').mockImplementation(() => {})
	target = document.body.appendChild(document.createElement('div'))
})
afterEach(() => {
	vi.useRealTimers()
	page?.$destroy()
	target.remove()
	vi.unstubAllGlobals()
	vi.restoreAllMocks()
	vi.clearAllMocks()
})

async function mount(network: Network, path: './lock/+page.svelte' | './claim/+page.svelte') {
	const Page = await asBuild(BUILDS[network], async () => {
		vi.doMock('$lib/ethereum', async original => ({
			...(await original<typeof import('$lib/ethereum')>()),
			...wallet
		}))
		const { ethereumStore } = await import('$lib/ethereum')
		ethereumStore.set(connected(CHAIN[network]))
		return (await import(path)).default as Page
	})
	page = new Page({ target })
}

/** Stubs every `fetch`: each call takes the next answer, and the last repeats. */
function stubFetch(...answers: Answer[]) {
	let calls = 0
	const fetch = vi.fn((url: string, init: RequestInit) =>
		answers[Math.min(calls++, answers.length - 1)](url, init)
	)
	vi.stubGlobal('fetch', fetch)
	return fetch
}

function type(selector: string, value: string) {
	const input = target.querySelector<HTMLInputElement>(selector)!
	input.value = value
	input.dispatchEvent(new Event('input'))
}

const button = (label: string) =>
	[...target.querySelectorAll('button')].find(b => b.textContent?.includes(label))

// Each page is imported afresh, which a loaded machine can slow past waitFor's 1 s default.
const until = (assertion: () => void) => vi.waitFor(assertion, { timeout: 5_000 })

async function click(label: string) {
	await until(() => {
		expect(button(label)?.disabled).toBe(false)
		button(label)!.click()
	})
}

const shown = () => target.textContent!.replace(/\s+/g, ' ')
const written = () => wallet.writeContract.mock.calls.map(([call]) => call.functionName)
const order = (mock: { mock: { invocationCallOrder: number[] } }) => mock.mock.invocationCallOrder

async function lock(network: Network) {
	await mount(network, './lock/+page.svelte')
	type('#amount', '5')
	type('#agent', AGENT)
	await click('Lock')
}

describe.each(['sepolia', 'mainnet'] as const)('on a %s build, Lock', network => {
	it('sends nothing, and shows the stop text, while the bridge is paused', async () => {
		const fetch = stubFetch(PAUSED)

		await lock(network)

		await until(() => expect(shown()).toContain(PAUSED_TEXT))
		expect(target.querySelector('a[href="mailto:info@unyt.co"]')).not.toBeNull()
		expect(fetch).toHaveBeenCalledWith('/api/status', expect.anything())
		expect(wallet.writeContract).not.toHaveBeenCalled()
	})

	it.each(FAILURES)('sends nothing, and shows an error, when /api/status %s', async (_, answer) => {
		stubFetch(answer)

		await lock(network)

		await until(() => expect(shown()).toContain(STATUS_FAILED))
		expect(shown()).not.toContain(PAUSED_TEXT)
		expect(button('Lock')?.disabled).toBe(false)
		expect(wallet.writeContract).not.toHaveBeenCalled()
	})

	it.each(FAILURES)(
		'sends no lock after its approve, and shows an error, when /api/status then %s',
		async (_, answer) => {
			reads.allowance = 0n
			stubFetch(OPEN, answer)

			await lock(network)

			await until(() => expect(shown()).toContain(STATUS_FAILED))
			expect(written()).toEqual(['approve'])
			expect(button('Lock')?.disabled).toBe(false)
		}
	)

	it('gives up on a read that never answers, and sends nothing', async () => {
		const fetch = stubFetch(
			(_, init) =>
				new Promise((_, reject) =>
					init.signal?.addEventListener('abort', () =>
						reject(new DOMException('Aborted', 'AbortError'))
					)
				)
		)
		await mount(network, './lock/+page.svelte')
		type('#amount', '5')
		type('#agent', AGENT)
		await until(() => expect(button('Lock')?.disabled).toBe(false))
		// Fake timers make waitFor advance the clock on each poll, so the clock moves by hand.
		vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })

		button('Lock')!.click()
		await vi.advanceTimersByTimeAsync(STATUS_TIMEOUT_MS - 1)

		expect(fetch).toHaveBeenCalledWith(
			'/api/status',
			expect.objectContaining({ signal: expect.any(AbortSignal) })
		)
		expect(shown()).not.toContain(STATUS_FAILED)
		expect(button('Lock')?.disabled).toBe(true)

		await vi.advanceTimersByTimeAsync(1)

		await until(() => expect(shown()).toContain(STATUS_FAILED))
		expect(button('Lock')?.disabled).toBe(false)
		expect(wallet.writeContract).not.toHaveBeenCalled()
	})

	it('locks once /api/status reads not paused', async () => {
		const fetch = stubFetch(OPEN)

		await lock(network)

		await until(() => expect(written()).toEqual(['lock']))
		expect(order(fetch)[0]).toBeLessThan(order(wallet.writeContract)[0])
	})

	it('reads /api/status before the approve, and sends neither while paused', async () => {
		reads.allowance = 0n
		stubFetch(PAUSED)

		await lock(network)

		await until(() => expect(shown()).toContain(PAUSED_TEXT))
		expect(wallet.writeContract).not.toHaveBeenCalled()
	})

	it('sends no lock when the bridge pauses while its approve is mined', async () => {
		reads.allowance = 0n
		const fetch = stubFetch(OPEN, PAUSED)

		await lock(network)

		await until(() => expect(shown()).toContain(PAUSED_TEXT))
		expect(written()).toEqual(['approve'])
		expect(order(fetch)[0]).toBeLessThan(order(wallet.writeContract)[0])
		expect(order(fetch)[1]).toBeGreaterThan(order(wallet.waitForTransaction)[0])
		expect(shown()).not.toContain('Confirming transaction')
		expect(shown()).not.toContain('Waiting for your manual confirmation')
		expect(button('Lock')?.disabled).toBe(false)
	})

	it('shows an error, not the stop text, when a retry after a pause cannot read the status', async () => {
		stubFetch(PAUSED, UNREACHABLE)
		await lock(network)
		await until(() => expect(shown()).toContain(PAUSED_TEXT))

		await click('Lock')

		await until(() => expect(shown()).toContain(STATUS_FAILED))
		expect(shown()).not.toContain(PAUSED_TEXT)
		expect(wallet.writeContract).not.toHaveBeenCalled()
	})

	it('clears an old error once a retry reads the bridge open', async () => {
		stubFetch(UNREACHABLE, OPEN)
		wallet.writeContract.mockImplementationOnce(() => new Promise<string>(() => {}))
		await lock(network)
		await until(() => expect(shown()).toContain(STATUS_FAILED))

		await click('Lock')

		await until(() => expect(written()).toEqual(['lock']))
		expect(shown()).not.toContain(STATUS_FAILED)
	})

	it('shows a later input error in place of the stop text', async () => {
		stubFetch(PAUSED)
		await lock(network)
		await until(() => expect(shown()).toContain(PAUSED_TEXT))

		type('#amount', '1.1234567')
		await click('Lock')

		await until(() => expect(shown()).toContain('at most 6 decimal places'))
		expect(shown()).not.toContain(PAUSED_TEXT)
	})
})

describe('Claim', () => {
	it('sends with no status request, paused or not', async () => {
		const fetch = stubFetch(PAUSED)
		await mount('sepolia', './claim/+page.svelte')

		type('#coupon', SEPOLIA_REDEEMED[0].coupon)
		await click('Claim')

		await until(() => expect(written()).toEqual(['takeOrders']))
		expect(fetch).not.toHaveBeenCalled()
	})
})
