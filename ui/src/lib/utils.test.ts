import { describe, expect, it } from 'vitest'
import { causeChain } from './utils'

describe('causeChain', () => {
	it('lists an error and each error that caused it', () => {
		const wallet = { code: -32603, message: 'Internal JSON-RPC error.' }
		const viem = new Error('An internal error was received.', { cause: wallet })
		const shown = new Error('Your wallet did not send the transaction.', { cause: viem })

		expect(causeChain(shown)).toEqual([shown, viem, wallet])
	})

	it('stops at ten links on an error that causes itself', () => {
		const looped = new Error('looped')
		looped.cause = looped

		expect(causeChain(looped)).toHaveLength(10)
	})
})
