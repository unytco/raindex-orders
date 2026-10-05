import { derived, get, writable, type Readable, type Writable } from 'svelte/store'
import { numberToHex, type Abi, type Hex } from 'viem'
import { bridge } from './config'
import { errorMessage } from './utils'

export interface EthereumState {
	isConnected: boolean
	account: string | null
	chainId: number | null
	isLoading: boolean
	error: string | null
}

export type Eip1193Provider = {
	request(args: { method: string; params?: unknown[] }): Promise<unknown>
	on(event: 'accountsChanged', listener: (accounts: string[]) => void): void
	on(event: 'chainChanged', listener: (chainId: string) => void): void
}

const initialState: EthereumState = {
	isConnected: false,
	account: null,
	chainId: null,
	isLoading: false,
	error: null
}

export const ethereumStore: Writable<EthereumState> = writable(initialState)

/** A connected wallet on a chain other than this build's network. */
export const onWrongNetwork: Readable<boolean> = derived(
	ethereumStore,
	s => s.isConnected && s.chainId !== bridge.chain.id
)

let ethereum: Eip1193Provider | null = null

function injectedProvider(): Eip1193Provider | null {
	if (typeof window === 'undefined') return null
	return (window as { ethereum?: Eip1193Provider }).ethereum ?? null
}

export function getEthereum(): Eip1193Provider | null {
	return ethereum || injectedProvider()
}

export async function initEthereum(): Promise<boolean> {
	if (typeof window === 'undefined') return false

	const eth = injectedProvider()
	if (!eth) {
		ethereumStore.update(s => ({ ...s, error: 'Please install MetaMask!' }))
		return false
	}

	ethereum = eth

	// Check if already connected
	try {
		const accounts = (await eth.request({ method: 'eth_accounts' })) as string[]
		const chainId = (await eth.request({ method: 'eth_chainId' })) as string

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
		ethereumStore.update(s => ({
			...s,
			isConnected: false,
			account: null
		}))
	} else {
		ethereumStore.update(s => ({
			...s,
			isConnected: true,
			account: accounts[0]
		}))
	}
}

function handleChainChanged(chainId: string) {
	ethereumStore.update(s => ({
		...s,
		chainId: parseInt(chainId, 16)
	}))
}

export async function connectWallet(): Promise<string | null> {
	const eth = getEthereum()
	if (!eth) {
		ethereumStore.update(s => ({ ...s, error: 'Please install MetaMask!' }))
		return null
	}

	ethereumStore.update(s => ({ ...s, isLoading: true, error: null }))

	try {
		const accounts = (await eth.request({ method: 'eth_requestAccounts' })) as string[]
		const chainId = (await eth.request({ method: 'eth_chainId' })) as string

		ethereumStore.set({
			isConnected: true,
			account: accounts[0],
			chainId: parseInt(chainId, 16),
			isLoading: false,
			error: null
		})

		return accounts[0]
	} catch (err) {
		const rejected = (err as { code?: number } | null)?.code === 4001
		ethereumStore.update(s => ({
			...s,
			isLoading: false,
			error: rejected ? 'Connection rejected by user' : errorMessage(err, 'Failed to connect')
		}))
		return null
	}
}

/** Asks the wallet to switch to this build's network, adding Sepolia if it lacks it. */
export async function switchNetwork(): Promise<boolean> {
	const eth = getEthereum()
	if (!eth) return false

	const chainId = numberToHex(bridge.chain.id)
	try {
		await eth.request({ method: 'wallet_switchEthereumChain', params: [{ chainId }] })
		return true
	} catch (err) {
		const unknownChain = (err as { code?: number } | null)?.code === 4902
		if (unknownChain && bridge.addToWallet) {
			try {
				await eth.request({
					method: 'wallet_addEthereumChain',
					params: [{ chainId, ...bridge.addToWallet }]
				})
				return true
			} catch (addErr) {
				console.error(`Failed to add ${bridge.networkName}:`, addErr)
				return false
			}
		}
		console.error('Failed to switch network:', err)
		return false
	}
}

export async function waitForTransaction(txHash: string): Promise<unknown> {
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
	abi: Abi
	functionName: string
	args?: readonly unknown[]
}): Promise<unknown> {
	const eth = getEthereum()
	if (!eth) throw new Error('No ethereum provider')

	const { encodeFunctionData, decodeFunctionResult } = await import('viem')

	const data = encodeFunctionData({
		abi: params.abi,
		functionName: params.functionName,
		args: params.args || []
	})

	const result = (await eth.request({
		method: 'eth_call',
		params: [{ to: params.address, data }, 'latest']
	})) as Hex

	const abiItem = params.abi.find(
		item => item.type === 'function' && item.name === params.functionName
	)

	if (abiItem && abiItem.type === 'function' && abiItem.outputs.length > 0) {
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
	abi: Abi
	functionName: string
	args?: readonly unknown[]
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

	const { account } = get(ethereumStore)
	if (!account) throw new Error('Not connected')
	const walletChain = parseInt((await eth.request({ method: 'eth_chainId' })) as string, 16)
	if (walletChain !== bridge.chain.id) {
		throw new Error(`Switch your wallet to ${bridge.networkName} first`)
	}

	const txHash = (await eth.request({
		method: 'eth_sendTransaction',
		params: [
			{
				from: account,
				to: params.address,
				data,
				value: params.value ? '0x' + params.value.toString(16) : '0x0'
			}
		]
	})) as string

	return txHash
}
