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
