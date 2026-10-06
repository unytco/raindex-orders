import { afterEach, describe, expect, it, vi } from 'vitest'
import { asBuild, BUILDS } from './testing/builds'

const HASH = '0xabc'

/** A wallet whose receipts for HASH are `receipts`, one per poll. */
async function polling(receipts: unknown[]) {
	vi.useFakeTimers()
	const request = vi.fn(async () => receipts.shift() ?? null)
	vi.stubGlobal('window', { ethereum: { request, on: vi.fn() } })
	const { waitForTransaction } = await asBuild(BUILDS.mainnet, () => import('./ethereum'))
	return waitForTransaction
}

afterEach(() => {
	vi.useRealTimers()
	vi.unstubAllGlobals()
})

describe('waitForTransaction', () => {
	it('resolves with the receipt of a transaction that succeeded, once mined', async () => {
		const wait = await polling([null, { status: '0x1' }])

		const receipt = wait(HASH)
		await vi.advanceTimersByTimeAsync(2000)

		await expect(receipt).resolves.toEqual({ status: '0x1' })
	})

	it('rejects for a transaction that reverted, so no success is shown', async () => {
		const wait = await polling([{ status: '0x0' }])

		await expect(wait(HASH)).rejects.toThrow(`Transaction ${HASH} reverted`)
	})
})
