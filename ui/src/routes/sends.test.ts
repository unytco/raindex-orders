// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { encodeFunctionResult, maxUint256 } from 'viem'
import type { SvelteComponent } from 'svelte'
import { asBuild, BUILDS, type Network } from '$lib/testing/builds'
import { ACCOUNT, CHAIN, connected, injectWallet } from '$lib/testing/wallet'
import { deserializeSignedContext } from '$lib/coupon'
import { orderbookAbi } from '../generated'
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
	waitForTransaction: vi.fn(async () => ({ status: 'success', transactionHash: '0xabc' }))
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

/** Returns the `$lib/ethereum` the page was built against, not the one a top-level import gives. */
async function mount(network: Network, path: './lock/+page.svelte' | './claim/+page.svelte') {
	const { Page, ethereum } = await asBuild(BUILDS[network], async () => {
		vi.doMock('$lib/ethereum', async original => ({
			...(await original<typeof import('$lib/ethereum')>()),
			...wallet
		}))
		const ethereum = await import('$lib/ethereum')
		ethereum.ethereumStore.set(connected(CHAIN[network]))
		return { Page: (await import(path)).default as Page, ethereum }
	})
	page = new Page({ target })
	return ethereum
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
const alerted = () => target.querySelector('[role="alert"]')?.textContent?.trim()
const dialog = () => target.querySelector('[role="dialog"]')?.textContent ?? ''
const logged = () => vi.mocked(console.error).mock.calls
const written = () => wallet.writeContract.mock.calls.map(([call]) => call.functionName)
const order = (mock: { mock: { invocationCallOrder: number[] } }) => mock.mock.invocationCallOrder

async function submitLock() {
	type('#amount', '5')
	type('#agent', AGENT)
	await click('Lock')
}

async function lock(network: Network) {
	await mount(network, './lock/+page.svelte')
	await submitLock()
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

	it('sends nothing, and says so, for an amount that is not a plain number', async () => {
		const fetch = stubFetch(OPEN)
		await mount(network, './lock/+page.svelte')
		type('#amount', '1e5')
		type('#agent', AGENT)
		await click('Lock')

		await until(() => expect(alerted()).toBe('Amount must be a plain number, such as 5 or 2.5.'))
		expect(fetch).not.toHaveBeenCalled()
		expect(wallet.writeContract).not.toHaveBeenCalled()
		expect(logged()).toEqual([])
	})
})

const TOKEN = { sepolia: 'mock HOT', mainnet: 'HOT' }
const NOT_SENT =
	"Your wallet did not send the transaction. If your wallet's activity shows no lock, try again."
const LOCK_FAILED =
	"Something went wrong with your lock. Check your wallet's activity before you lock again, and contact Unyt support if it keeps failing."
const USER_DENIED = {
	code: 4001,
	message: 'MetaMask Tx Signature: User denied transaction signature.'
}
const RPC_FAILURE = { code: -32603, message: 'Internal JSON-RPC error.' }
const VIEM_FAILURE = new Error(
	'An internal error was received.\n\nURL: http://localhost\nRequest body: {"method":"eth_sendTransaction"}\n\nDetails: Internal JSON-RPC error.\nVersion: viem@2.45.0',
	{ cause: RPC_FAILURE }
)

describe.each(['sepolia', 'mainnet'] as const)('on a %s build, a Lock that fails', network => {
	const DECLINED = `You declined the lock in your wallet, so no ${TOKEN[network]} was locked.`

	beforeEach(() => {
		reads.allowance = 0n
		stubFetch(OPEN)
	})

	describe.each(['approve', 'lock'] as const)('in its %s transaction', step => {
		const sends = step === 'approve' ? ['approve'] : ['approve', 'lock']

		it.each([
			['declined in the wallet', USER_DENIED, DECLINED, [USER_DENIED]],
			['not sent by the wallet', RPC_FAILURE, NOT_SENT, [RPC_FAILURE]],
			['not sent, with a long error', VIEM_FAILURE, NOT_SENT, [VIEM_FAILURE, RPC_FAILURE]]
		])('%s says so plainly, and logs the raw error once', async (_case, failure, message, raw) => {
			wallet.writeContract.mockImplementation(async ({ functionName }) => {
				if (functionName === step) throw failure
				return '0xabc'
			})

			await lock(network)

			await until(() => expect(alerted()).toBe(message))
			expect(dialog()).toContain(message)
			expect(shown()).not.toContain(failure.message.replace(/\s+/g, ' '))
			expect(written()).toEqual(sends)
			expect(button('Lock')?.disabled).toBe(false)
			expect(logged()).toEqual([
				[
					`Lock failed in its ${step} transaction: ${message}`,
					{
						step,
						account: ACCOUNT,
						chainId: CHAIN[network],
						causes: [expect.objectContaining({ message }), ...raw]
					}
				]
			])
			expect(logged()[0][1].causes[1]).toBe(failure)
		})

		it('sent while the wallet is on another network says to switch', async () => {
			const ethereum = await mount(network, './lock/+page.svelte')
			const wrongNetwork = new Error(ethereum.SWITCH_NETWORK)
			wallet.writeContract.mockImplementation(async ({ functionName }) => {
				if (functionName === step) throw wrongNetwork
				return '0xabc'
			})
			await submitLock()

			await until(() => expect(alerted()).toBe(ethereum.SWITCH_NETWORK))
			expect(logged()[0][1]).toMatchObject({ step, causes: [expect.anything(), wrongNetwork] })
		})

		it.each([
			['reverted', '❌'],
			['replaced', '❌'],
			['pending', '⏳']
		] as const)(
			'that ends %s keeps its outcome and its landed hash, and logs it',
			async (outcome, mark) => {
				const ethereum = await mount(network, './lock/+page.svelte')
				const failure = new ethereum.TransactionOutcomeError(outcome, {
					sentHash: '0xabc',
					hash: '0xdef'
				})
				if (step === 'lock') {
					wallet.waitForTransaction.mockResolvedValueOnce({
						status: 'success',
						transactionHash: '0xabc'
					})
				}
				wallet.waitForTransaction.mockRejectedValueOnce(failure)
				await submitLock()

				await until(() => expect(alerted()).toBe(failure.message))
				expect(dialog()).toContain(mark)
				expect(target.querySelector('[role="dialog"] a[href$="/tx/0xdef"]')).not.toBeNull()
				expect(written()).toEqual(sends)
				expect(logged()).toEqual([
					[
						`Lock failed in its ${step} transaction: ${failure.message}`,
						{ step, account: ACCOUNT, chainId: CHAIN[network], causes: [failure] }
					]
				])
			}
		)
	})

	it.each([
		['declined in the wallet', USER_DENIED, DECLINED],
		['not sent by the wallet', RPC_FAILURE, NOT_SENT]
	])('with no approve needed, %s says so, and logs its lock', async (_case, failure, message) => {
		reads.allowance = maxUint256
		wallet.writeContract.mockRejectedValueOnce(failure)

		await lock(network)

		await until(() => expect(alerted()).toBe(message))
		expect(written()).toEqual(['lock'])
		expect(logged()).toEqual([
			[
				`Lock failed in its lock transaction: ${message}`,
				{
					step: 'lock',
					account: ACCOUNT,
					chainId: CHAIN[network],
					causes: [expect.objectContaining({ message }), failure]
				}
			]
		])
	})

	it('for more than the balance it reads at send says so, and sends neither the approve nor the lock', async () => {
		await mount(network, './lock/+page.svelte')
		await until(() => expect(shown()).toContain('1000 HOT'))
		reads.balanceOf = 4_250_000_000_000_000_000n

		await submitLock()

		await until(() =>
			expect(alerted()).toBe('You have 4.25 HOT, which is less than the 5 you entered.')
		)
		expect(wallet.writeContract).not.toHaveBeenCalled()
		expect(dialog()).toBe('')
		expect(logged()).toEqual([])
		expect(button('Lock')?.disabled).toBe(false)
	})

	it('for more than the balance after its approve sends no lock', async () => {
		wallet.waitForTransaction.mockImplementationOnce(async () => {
			reads.balanceOf = 4_250_000_000_000_000_000n
			return { status: 'success', transactionHash: '0xabc' }
		})

		await lock(network)

		await until(() =>
			expect(alerted()).toBe('You have 4.25 HOT, which is less than the 5 you entered.')
		)
		expect(written()).toEqual(['approve'])
		expect(dialog()).toBe('')
		expect(button('Lock')?.disabled).toBe(false)
	})

	it('for exactly the balance sends', async () => {
		reads.balanceOf = 5n * 10n ** 18n

		await lock(network)

		await until(() => expect(written()).toEqual(['approve', 'lock']))
		expect(alerted()).toBeUndefined()
	})

	it('whose balance cannot be read sends anyway, and logs the read error', async () => {
		const failure = new Error('Failed to fetch')
		await mount(network, './lock/+page.svelte')
		await until(() => expect(shown()).toContain('1000 HOT'))
		wallet.readContract.mockRejectedValueOnce(failure)

		await submitLock()

		await until(() => expect(written()).toEqual(['approve', 'lock']))
		expect(logged()).toEqual([['Error reading the balance before the lock:', failure]])
	})

	it('for a cause it does not know says so in general terms, and logs the cause', async () => {
		const failure = new Error('No ethereum provider')
		wallet.waitForTransaction.mockRejectedValueOnce(failure)

		await lock(network)

		await until(() => expect(alerted()).toBe(LOCK_FAILED))
		expect(shown()).not.toContain(failure.message)
		expect(logged()).toEqual([
			[
				`Lock failed in its approve transaction: ${LOCK_FAILED}`,
				{ step: 'approve', account: ACCOUNT, chainId: CHAIN[network], causes: [failure] }
			]
		])
	})
})

describe('Claim', () => {
	it('sends with no status request, paused or not', async () => {
		const fetch = stubFetch(PAUSED)
		const { coupon } = SEPOLIA_REDEEMED[0]
		const amount = deserializeSignedContext(coupon).context[1]
		const pays = encodeFunctionResult({
			abi: orderbookAbi,
			functionName: 'takeOrders',
			result: [amount, 0n]
		})
		const injected = injectWallet(CHAIN.sepolia, { eth_call: pays })
		await mount('sepolia', './claim/+page.svelte')

		type('#coupon', coupon)
		await click('Claim')

		await until(() => expect(written()).toEqual(['takeOrders']))
		expect(injected).toHaveBeenCalledWith(expect.objectContaining({ method: 'eth_call' }))
		expect(fetch).not.toHaveBeenCalled()
	})
})
