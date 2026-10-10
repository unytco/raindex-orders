import { afterEach, describe, expect, it, vi } from 'vitest'
import { asBuild, BUILDS } from './testing/builds'
import { chainAnswers, REPLACEMENT_HASH, type Fate } from './testing/chain'

const SENT = {
	hash: `0x${'12'.repeat(32)}`,
	from: '0x1111111111111111111111111111111111111111',
	to: '0x2222222222222222222222222222222222222222',
	input: '0xabcdef'
} as const

/**
 * `waitForTransaction(SENT)` on a Sepolia wallet whose node reports `fate`, with the clock run
 * until it settles. While `away()` the wallet is on a chain 5,000 blocks ahead.
 */
async function waitFor(fate: Fate, away = () => false) {
	vi.useFakeTimers()
	const answer = chainAnswers(SENT, fate)
	const asked: string[] = []
	const request = vi.fn(async ({ method, params }: { method: string; params?: unknown[] }) => {
		asked.push(method)
		if (method === 'eth_chainId') return away() ? '0x1' : '0xaa36a7'
		if (method === 'eth_blockNumber' && away()) return '0x1398'
		return answer(method, params)
	})
	vi.stubGlobal('window', { ethereum: { request, on: vi.fn() } })
	const { waitForTransaction, CONFIRMATION_TIMEOUT_MS } = await asBuild(
		BUILDS.sepolia,
		() => import('./ethereum')
	)
	let settled = false
	const waiting = waitForTransaction(SENT.hash)
	waiting.then(
		() => (settled = true),
		() => (settled = true)
	)
	const limit = Date.now() + CONFIRMATION_TIMEOUT_MS + 60_000
	while (!settled && Date.now() < limit) await vi.advanceTimersByTimeAsync(1_000)
	return { waiting, asked }
}

afterEach(() => {
	vi.useRealTimers()
	vi.unstubAllGlobals()
})

const MINED = { blockNumber: 0x11n, gasUsed: 447_599n, gasLimit: 849_182n }

describe('waitForTransaction', () => {
	it('resolves with the receipt once the transaction is mined', async () => {
		const { waiting } = await waitFor({ mined: '0x1', afterPolls: 2 })

		await expect(waiting).resolves.toMatchObject({ transactionHash: SENT.hash, status: 'success' })
	})

	it('follows a transaction the wallet sped up to its new hash', async () => {
		const { waiting } = await waitFor({ replacedBy: 'repriced' })

		await expect(waiting).resolves.toMatchObject({
			transactionHash: REPLACEMENT_HASH,
			status: 'success'
		})
	})

	it('keeps waiting through receipt requests that fail for a while', async () => {
		const { waiting } = await waitFor({ mined: '0x1', afterPolls: 1, failingReceiptRequests: 12 })

		await expect(waiting).resolves.toMatchObject({ transactionHash: SENT.hash })
	})

	it('waits out a wallet that visits another chain, without walking its blocks', async () => {
		let start = 0
		const { waiting, asked } = await waitFor({ mined: '0x1', afterPolls: 3 }, () => {
			start ||= Date.now()
			return Date.now() - start < 20_000
		})

		await expect(waiting).resolves.toMatchObject({ transactionHash: SENT.hash })
		expect(asked.length).toBeLessThan(500)
	})

	it.each([
		['reverted', { mined: '0x0' }, SENT.hash],
		['reverted', { replacedBy: 'repriced', status: '0x0' }, REPLACEMENT_HASH],
		['cancelled', { replacedBy: 'cancelled' }, REPLACEMENT_HASH],
		['replaced', { replacedBy: 'replaced' }, REPLACEMENT_HASH]
	] as const)('rejects as %s, with what was mined, when %o', async (outcome, fate, hash) => {
		const { waiting } = await waitFor(fate)

		await expect(waiting).rejects.toMatchObject({
			outcome,
			hash,
			sentHash: SENT.hash,
			mined: MINED
		})
	})

	it.each([
		['pending', { dropped: true }],
		['unreadable', { receiptFails: true }]
	] as const)('rejects as %s at the time limit when %o', async (outcome, fate) => {
		const { waiting } = await waitFor(fate)

		await expect(waiting).rejects.toMatchObject({ outcome, hash: SENT.hash, mined: undefined })
	})

	it('never shows a bare hash as its message', async () => {
		const { waiting } = await waitFor({ mined: '0x0' })

		await expect(waiting).rejects.toMatchObject({
			message: 'The transaction failed on the network, so nothing changed. Try again.'
		})
	})
})
