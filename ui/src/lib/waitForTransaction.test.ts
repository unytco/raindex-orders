import { afterEach, describe, expect, it, vi } from 'vitest'
import { asBuild, BUILDS } from './testing/builds'
import { chainAnswers, REPLACEMENT_HASH, type Fate } from './testing/chain'

const SENT = {
	hash: `0x${'12'.repeat(32)}`,
	from: '0x1111111111111111111111111111111111111111',
	to: '0x2222222222222222222222222222222222222222',
	input: '0xabcdef'
} as const

/** `waitForTransaction(SENT)` on a wallet whose node reports `fate`, settled with the clock run on. */
async function waitFor(fate: Fate) {
	vi.useFakeTimers()
	const answer = chainAnswers(SENT, fate)
	const request = vi.fn(async ({ method, params }: { method: string; params?: unknown[] }) =>
		answer(method, params)
	)
	vi.stubGlobal('window', { ethereum: { request, on: vi.fn() } })
	const { waitForTransaction, CONFIRMATION_TIMEOUT_MS } = await asBuild(
		BUILDS.sepolia,
		() => import('./ethereum')
	)
	const waiting = waitForTransaction(SENT.hash)
	waiting.catch(() => {})
	await vi.advanceTimersByTimeAsync(CONFIRMATION_TIMEOUT_MS + 1_000)
	return waiting
}

afterEach(() => {
	vi.useRealTimers()
	vi.unstubAllGlobals()
})

describe('waitForTransaction', () => {
	it('resolves with the receipt once the transaction is mined', async () => {
		await expect(waitFor({ mined: '0x1', afterPolls: 2 })).resolves.toMatchObject({
			transactionHash: SENT.hash,
			status: 'success'
		})
	})

	it('follows a transaction the wallet sped up to its new hash', async () => {
		await expect(waitFor({ replacedBy: 'repriced' })).resolves.toMatchObject({
			transactionHash: REPLACEMENT_HASH,
			status: 'success'
		})
	})

	it.each([
		['reverted', { mined: '0x0' }, SENT.hash],
		['reverted', { replacedBy: 'repriced', status: '0x0' }, REPLACEMENT_HASH],
		['cancelled', { replacedBy: 'cancelled' }, REPLACEMENT_HASH],
		['replaced', { replacedBy: 'replaced' }, REPLACEMENT_HASH],
		['pending', { dropped: true }, SENT.hash],
		['unreadable', { receiptFails: true }, SENT.hash]
	] as const)('rejects as %s when %o', async (outcome, fate, hash) => {
		await expect(waitFor(fate)).rejects.toMatchObject({ outcome, hash, sentHash: SENT.hash })
	})

	it('never shows a bare hash as its message', async () => {
		const reverted = waitFor({ mined: '0x0' })

		await expect(reverted).rejects.toMatchObject({
			message: 'The transaction failed on the network, so nothing changed. Try again.'
		})
		await expect(reverted).rejects.not.toMatchObject({ message: expect.stringContaining('0x') })
	})
})
