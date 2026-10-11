import { vi } from 'vitest'
import { numberToHex } from 'viem'
import type { EthereumState } from '$lib/ethereum'

export const CHAIN = { sepolia: 11155111, mainnet: 1 }
export const OTHER_CHAIN = { sepolia: 1, mainnet: 11155111 }
export const ACCOUNT = '0x1111111111111111111111111111111111111111'

export const connected = (chainId: number): EthereumState => ({
	isConnected: true,
	account: ACCOUNT,
	chainId,
	isLoading: false,
	error: null
})

/** Injects a wallet on `chainId` that answers each other method from `answers`. */
export function injectWallet(chainId: number, answers: Record<string, unknown>) {
	const request = vi.fn(async ({ method }: { method: string }) => {
		if (method === 'eth_chainId') return numberToHex(chainId)
		if (method in answers) return answers[method]
		throw new Error(`unexpected ${method}`)
	})
	vi.stubGlobal('ethereum', { request, on: vi.fn() })
	return request
}
