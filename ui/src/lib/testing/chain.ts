import type { Address, Hex } from 'viem'

/** What happens to a sent transaction, as the wallet's node reports it. */
export type Fate =
	| { mined: '0x1' | '0x0'; afterPolls?: number }
	| { replacedBy: 'repriced' | 'cancelled' | 'replaced'; status?: '0x1' | '0x0' }
	| { dropped: true }
	| { receiptFails: true }

export type SentTransaction = { hash: Hex; from: Address; to: Address; input: Hex }

export const REPLACEMENT_HASH: Hex = `0x${'ab'.repeat(32)}`
const BLOCK_HASH: Hex = `0x${'cd'.repeat(32)}`

/** A node's answers to the requests viem makes while waiting for `sent`, or undefined for others. */
export function chainAnswers(sent: SentTransaction, fate: Fate) {
	let block = 0x10
	let receiptPolls = 0
	const pending = { ...sent, nonce: '0x5', value: '0x0', type: '0x2', blockNumber: null }
	const replacement =
		'replacedBy' in fate
			? {
					...pending,
					hash: REPLACEMENT_HASH,
					blockNumber: '0x11',
					blockHash: BLOCK_HASH,
					...(fate.replacedBy === 'cancelled' ? { to: sent.from, input: '0x' } : {}),
					...(fate.replacedBy === 'replaced' ? { input: '0xdeadbeef' } : {})
				}
			: undefined
	const receipt = (transactionHash: Hex, status: string) => ({
		transactionHash,
		status,
		blockNumber: '0x11',
		blockHash: BLOCK_HASH,
		logs: []
	})

	return (method: string, params: unknown[] = []): unknown => {
		const hash = params[0]
		switch (method) {
			case 'eth_blockNumber':
				return `0x${(block++).toString(16)}`
			case 'eth_getTransactionByHash':
				if (hash === REPLACEMENT_HASH) return replacement ?? null
				return 'dropped' in fate ? null : pending
			case 'eth_getTransactionReceipt':
				if ('receiptFails' in fate) throw { code: -32603, message: 'Internal JSON-RPC error.' }
				if ('mined' in fate && hash === sent.hash && receiptPolls++ >= (fate.afterPolls ?? 0)) {
					return receipt(sent.hash, fate.mined)
				}
				if (replacement && hash === REPLACEMENT_HASH) {
					return receipt(REPLACEMENT_HASH, ('status' in fate && fate.status) || '0x1')
				}
				return null
			case 'eth_getBlockByNumber':
				return { number: hash, hash: BLOCK_HASH, transactions: replacement ? [replacement] : [] }
			default:
				return undefined
		}
	}
}
