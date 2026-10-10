// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { maxUint256 } from 'viem'
import type { SvelteComponent } from 'svelte'
import { asBuild, BUILDS } from '$lib/testing/builds'
import { PAUSED_TEXT } from '$lib/pause'
import { SEPOLIA_REDEEMED } from './api/coupon-status/fixtures'

type Network = 'sepolia' | 'mainnet'
type Page = new (options: { target: Element }) => SvelteComponent

const CHAIN = { sepolia: 11155111, mainnet: 1 }
const ACCOUNT = '0x1111111111111111111111111111111111111111'
const AGENT = 'u' + btoa(String.fromCharCode(0x84, 0x20, 0x24, ...Array(36).fill(7)))
const READS: Record<string, unknown> = {
	balanceOf: 10n ** 21n,
	allowance: maxUint256,
	symbol: 'HOT',
	decimals: 18,
	minLockAmount: 1n,
	orderExists: true,
	vaultBalance: 10n ** 21n
}

const wallet = {
	readContract: vi.fn(async ({ functionName }: { functionName: string }) => READS[functionName]),
	writeContract: vi.fn(async () => '0xabc'),
	waitForTransaction: vi.fn(async () => ({ status: '0x1' }))
}
let target: HTMLElement
let page: SvelteComponent | undefined

beforeEach(() => {
	vi.spyOn(console, 'error').mockImplementation(() => {})
	target = document.body.appendChild(document.createElement('div'))
})
afterEach(() => {
	page?.$destroy()
	target.remove()
	vi.unstubAllGlobals()
	vi.restoreAllMocks()
	vi.clearAllMocks()
})

/** The page at `path`, mounted on a `network` build with the wallet connected on its chain. */
async function mount(network: Network, path: './lock/+page.svelte' | './claim/+page.svelte') {
	const Page = await asBuild(BUILDS[network], async () => {
		vi.doMock('$lib/ethereum', async original => ({
			...(await original<typeof import('$lib/ethereum')>()),
			...wallet
		}))
		const { ethereumStore } = await import('$lib/ethereum')
		ethereumStore.set({
			isConnected: true,
			account: ACCOUNT,
			chainId: CHAIN[network],
			isLoading: false,
			error: null
		})
		return (await import(path)).default as Page
	})
	page = new Page({ target })
}

/** The site's `fetch`, answering `/api/status` with `answer`. */
function status(answer: () => Promise<Response>) {
	const fetch = vi.fn(answer)
	vi.stubGlobal('fetch', fetch)
	return fetch
}

function type(selector: string, value: string) {
	const input = target.querySelector<HTMLInputElement>(selector)!
	input.value = value
	input.dispatchEvent(new Event('input'))
}

async function click(label: string) {
	await vi.waitFor(() => {
		const button = [...target.querySelectorAll('button')].find(b => b.textContent?.includes(label))
		expect(button?.disabled).toBe(false)
		button!.click()
	})
}

const shown = () => target.textContent!.replace(/\s+/g, ' ')

async function lock(network: Network) {
	await mount(network, './lock/+page.svelte')
	type('#amount', '5')
	type('#agent', AGENT)
	await click('Lock')
}

describe.each(['sepolia', 'mainnet'] as const)('on a %s build, Lock', network => {
	it('sends nothing, and shows the stop text, while the bridge is paused', async () => {
		const fetch = status(async () => Response.json({ paused: true }))

		await lock(network)

		await vi.waitFor(() => expect(shown()).toContain(PAUSED_TEXT))
		expect(target.querySelector('a[href="mailto:info@unyt.co"]')).not.toBeNull()
		expect(fetch).toHaveBeenCalledWith('/api/status')
		expect(wallet.writeContract).not.toHaveBeenCalled()
	})

	it.each([
		['cannot be reached', async () => Promise.reject(new TypeError('fetch failed'))],
		['answers an error', async () => new Response('Bad gateway', { status: 502 })],
		['answers something else', async () => Response.json({ paused: 'no' })]
	])('sends nothing, and shows an error, when /api/status %s', async (_, answer) => {
		status(answer)

		await lock(network)

		await vi.waitFor(() => expect(shown()).toContain('Could not check that the bridge is open.'))
		expect(shown()).not.toContain(PAUSED_TEXT)
		expect(wallet.writeContract).not.toHaveBeenCalled()
	})

	it('locks once /api/status reads not paused', async () => {
		const fetch = status(async () => Response.json({ paused: false }))

		await lock(network)

		await vi.waitFor(() =>
			expect(wallet.writeContract).toHaveBeenCalledWith(
				expect.objectContaining({ functionName: 'lock' })
			)
		)
		expect(fetch.mock.invocationCallOrder[0]).toBeLessThan(
			wallet.writeContract.mock.invocationCallOrder[0]
		)
	})
})

describe('Claim', () => {
	it('sends with no status request, paused or not', async () => {
		const fetch = status(async () => Response.json({ paused: true }))
		await mount('sepolia', './claim/+page.svelte')

		type('#coupon', SEPOLIA_REDEEMED[0].coupon)
		await click('Claim')

		await vi.waitFor(() =>
			expect(wallet.writeContract).toHaveBeenCalledWith(
				expect.objectContaining({ functionName: 'takeOrders' })
			)
		)
		expect(fetch).not.toHaveBeenCalled()
	})
})
