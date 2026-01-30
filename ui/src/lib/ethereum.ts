import { writable, type Writable } from 'svelte/store'

export interface EthereumState {
	isConnected: boolean
	account: string | null
	chainId: number | null
	isLoading: boolean
	error: string | null
}

const initialState: EthereumState = {
	isConnected: false,
	account: null,
	chainId: null,
	isLoading: false,
	error: null
}

export const ethereumStore: Writable<EthereumState> = writable(initialState)

let ethereum: any = null

export function getEthereum() {
	return ethereum || (typeof window !== 'undefined' ? (window as any).ethereum : null)
}

export async function initEthereum(): Promise<boolean> {
	if (typeof window === 'undefined') return false

	const eth = (window as any).ethereum
	if (!eth) {
		ethereumStore.update((s) => ({ ...s, error: 'Please install MetaMask!' }))
		return false
	}

	ethereum = eth

	// Check if already connected
	try {
		const accounts = await eth.request({ method: 'eth_accounts' })
		const chainId = await eth.request({ method: 'eth_chainId' })

		if (accounts.length > 0) {
			ethereumStore.set({
				isConnected: true,
				account: accounts[0],
				chainId: parseInt(chainId, 16),
				isLoading: false,
				error: null
			})
		}
	} catch (err) {
		console.error('Error checking existing connection:', err)
	}

	// Set up event listeners
	eth.on('accountsChanged', handleAccountsChanged)
	eth.on('chainChanged', handleChainChanged)

	return true
}

function handleAccountsChanged(accounts: string[]) {
	if (accounts.length === 0) {
		ethereumStore.update((s) => ({
			...s,
			isConnected: false,
			account: null
		}))
	} else {
		ethereumStore.update((s) => ({
			...s,
			isConnected: true,
			account: accounts[0]
		}))
	}
}

function handleChainChanged(chainId: string) {
	ethereumStore.update((s) => ({
		...s,
		chainId: parseInt(chainId, 16)
	}))
}

export async function connectWallet(): Promise<string | null> {
	const eth = getEthereum()
	if (!eth) {
		ethereumStore.update((s) => ({ ...s, error: 'Please install MetaMask!' }))
		return null
	}

	ethereumStore.update((s) => ({ ...s, isLoading: true, error: null }))

	try {
		const accounts = await eth.request({ method: 'eth_requestAccounts' })
		const chainId = await eth.request({ method: 'eth_chainId' })

		ethereumStore.set({
			isConnected: true,
			account: accounts[0],
			chainId: parseInt(chainId, 16),
			isLoading: false,
			error: null
		})

		return accounts[0]
	} catch (err: any) {
		if (err.code === 4001) {
			ethereumStore.update((s) => ({
				...s,
				isLoading: false,
				error: 'Connection rejected by user'
			}))
		} else {
			ethereumStore.update((s) => ({
				...s,
				isLoading: false,
				error: err.message || 'Failed to connect'
			}))
		}
		return null
	}
}

export async function switchToSepolia(): Promise<boolean> {
	const eth = getEthereum()
	if (!eth) return false

	const sepoliaChainId = '0xaa36a7' // 11155111 in hex

	try {
		await eth.request({
			method: 'wallet_switchEthereumChain',
			params: [{ chainId: sepoliaChainId }]
		})
		return true
	} catch (err: any) {
		// Chain not added, try to add it
		if (err.code === 4902) {
			try {
				await eth.request({
					method: 'wallet_addEthereumChain',
					params: [
						{
							chainId: sepoliaChainId,
							chainName: 'Sepolia Testnet',
							nativeCurrency: {
								name: 'Sepolia ETH',
								symbol: 'ETH',
								decimals: 18
							},
							rpcUrls: ['https://rpc.sepolia.org'],
							blockExplorerUrls: ['https://sepolia.etherscan.io']
						}
					]
				})
				return true
			} catch (addErr) {
				console.error('Failed to add Sepolia network:', addErr)
				return false
			}
		}
		console.error('Failed to switch network:', err)
		return false
	}
}

export async function sendTransaction(params: {
	to: string
	data: string
	value?: string
}): Promise<string> {
	const eth = getEthereum()
	if (!eth) throw new Error('No ethereum provider')

	let state: EthereumState
	ethereumStore.subscribe((s) => (state = s))()

	if (!state!.account) throw new Error('Not connected')

	const txHash = await eth.request({
		method: 'eth_sendTransaction',
		params: [
			{
				from: state!.account,
				to: params.to,
				data: params.data,
				value: params.value || '0x0'
			}
		]
	})

	return txHash
}

export async function waitForTransaction(txHash: string): Promise<any> {
	const eth = getEthereum()
	if (!eth) throw new Error('No ethereum provider')

	return new Promise((resolve, reject) => {
		const checkReceipt = async () => {
			try {
				const receipt = await eth.request({
					method: 'eth_getTransactionReceipt',
					params: [txHash]
				})

				if (receipt) {
					resolve(receipt)
				} else {
					setTimeout(checkReceipt, 2000)
				}
			} catch (err) {
				reject(err)
			}
		}
		checkReceipt()
	})
}

// Contract interaction helpers using raw ethereum calls
export async function readContract(params: {
	address: string
	abi: any[]
	functionName: string
	args?: any[]
}): Promise<any> {
	const eth = getEthereum()
	if (!eth) throw new Error('No ethereum provider')

	const { encodeFunctionData, decodeFunctionResult } = await import('viem')

	const data = encodeFunctionData({
		abi: params.abi,
		functionName: params.functionName,
		args: params.args || []
	})

	const result = await eth.request({
		method: 'eth_call',
		params: [{ to: params.address, data }, 'latest']
	})

	const abiItem = params.abi.find(
		(item) => item.type === 'function' && item.name === params.functionName
	)

	if (abiItem && abiItem.outputs && abiItem.outputs.length > 0) {
		const decoded = decodeFunctionResult({
			abi: params.abi,
			functionName: params.functionName,
			data: result
		})
		return decoded
	}

	return result
}

export async function writeContract(params: {
	address: string
	abi: any[]
	functionName: string
	args?: any[]
	value?: bigint
}): Promise<string> {
	const eth = getEthereum()
	if (!eth) throw new Error('No ethereum provider')

	const { encodeFunctionData } = await import('viem')

	const data = encodeFunctionData({
		abi: params.abi,
		functionName: params.functionName,
		args: params.args || []
	})

	let state: EthereumState
	ethereumStore.subscribe((s) => (state = s))()

	if (!state!.account) throw new Error('Not connected')

	const txHash = await eth.request({
		method: 'eth_sendTransaction',
		params: [
			{
				from: state!.account,
				to: params.address,
				data,
				value: params.value ? '0x' + params.value.toString(16) : '0x0'
			}
		]
	})

	return txHash
}
