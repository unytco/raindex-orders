import { derived, get, writable, type Readable, type Writable } from 'svelte/store'
import {
	createPublicClient,
	custom,
	numberToHex,
	WaitForTransactionReceiptTimeoutError,
	type Abi,
	type Hex,
	type ReplacementReason,
	type TransactionReceipt
} from 'viem'
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

async function handleAccountsChanged(accounts: string[]) {
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
		await readWalletChain()
	}
}

/** Takes the chain from the wallet, which reports no change for a chain it is already on. */
async function readWalletChain() {
	const eth = getEthereum()
	if (!eth) return
	try {
		handleChainChanged((await eth.request({ method: 'eth_chainId' })) as string)
	} catch (err) {
		console.error('Error reading the wallet chain:', err)
		ethereumStore.update(s => ({ ...s, error: CHAIN_READ_FAILED }))
	}
}

const CHAIN_READ_FAILED = 'Could not read your wallet network'

function handleChainChanged(chainId: string) {
	ethereumStore.update(s => ({
		...s,
		chainId: parseInt(chainId, 16),
		error: s.error === CHAIN_READ_FAILED ? null : s.error
	}))
}

export const userRejected = (err: unknown) => (err as { code?: number } | null)?.code === 4001

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
		ethereumStore.update(s => ({
			...s,
			isLoading: false,
			error: userRejected(err)
				? 'Connection rejected by user'
				: errorMessage(err, 'Failed to connect')
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
		await readWalletChain()
		return true
	} catch (err) {
		const unknownChain = (err as { code?: number } | null)?.code === 4902
		if (unknownChain && bridge.addToWallet) {
			try {
				await eth.request({
					method: 'wallet_addEthereumChain',
					params: [{ chainId, ...bridge.addToWallet }]
				})
				await readWalletChain()
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

export const CONFIRMATION_TIMEOUT_MS = 5 * 60_000

export type TransactionOutcome = 'reverted' | 'cancelled' | 'replaced' | 'pending' | 'unreadable'

const OUTCOME_MESSAGES: Record<TransactionOutcome, string> = {
	reverted: 'The transaction failed on the network, so nothing changed. Try again.',
	cancelled: 'You cancelled the transaction in your wallet, so nothing changed.',
	replaced:
		"Your wallet replaced the transaction with another one. Check your wallet's activity before you try again.",
	pending:
		"The transaction is still waiting to be confirmed. Check your wallet's activity before you try again.",
	unreadable:
		"The transaction was sent, but its result could not be read. Check your wallet's activity before you try again."
}

/** A sent transaction that did not end in success. `hash` is the one that landed, if another did. */
export class TransactionOutcomeError extends Error {
	readonly outcome: TransactionOutcome
	readonly hash: string
	readonly sentHash: string

	constructor(
		outcome: TransactionOutcome,
		sentHash: string,
		hash = sentHash,
		options?: ErrorOptions
	) {
		super(OUTCOME_MESSAGES[outcome], options)
		this.outcome = outcome
		this.sentHash = sentHash
		this.hash = hash
	}

	get unconfirmed() {
		return this.outcome === 'pending' || this.outcome === 'unreadable'
	}
}

/** The receipt of `sentHash`, or of the transaction the wallet sped it up with. */
export async function waitForTransaction(sentHash: string): Promise<TransactionReceipt> {
	const eth = getEthereum()
	if (!eth) throw new Error('No ethereum provider')
	const client = createPublicClient({ transport: custom(eth) })

	let replacement: ReplacementReason | undefined
	let receipt: TransactionReceipt
	try {
		receipt = await client.waitForTransactionReceipt({
			hash: sentHash as Hex,
			pollingInterval: 2_000,
			timeout: CONFIRMATION_TIMEOUT_MS,
			onReplaced: replaced => {
				replacement = replaced.reason
			}
		})
	} catch (err) {
		const outcome = err instanceof WaitForTransactionReceiptTimeoutError ? 'pending' : 'unreadable'
		throw new TransactionOutcomeError(outcome, sentHash, sentHash, { cause: err })
	}

	const landed = receipt.transactionHash
	if (replacement === 'cancelled' || replacement === 'replaced') {
		throw new TransactionOutcomeError(replacement, sentHash, landed)
	}
	if (receipt.status !== 'success') throw new TransactionOutcomeError('reverted', sentHash, landed)
	return receipt
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

export const SWITCH_NETWORK = `Switch your wallet to ${bridge.networkName} first`

/** The chain the wallet itself is on, which can differ from the one it last reported. */
export async function walletChainId(eth: Eip1193Provider) {
	return parseInt((await eth.request({ method: 'eth_chainId' })) as string, 16)
}

export async function requireBridgeChain(eth: Eip1193Provider) {
	if ((await walletChainId(eth)) !== bridge.chain.id) throw new Error(SWITCH_NETWORK)
}

export async function writeContract(params: {
	address: string
	abi: Abi
	functionName: string
	args?: readonly unknown[]
	value?: bigint
	from?: string
}): Promise<string> {
	const eth = getEthereum()
	if (!eth) throw new Error('No ethereum provider')

	const { encodeFunctionData } = await import('viem')

	const data = encodeFunctionData({
		abi: params.abi,
		functionName: params.functionName,
		args: params.args || []
	})

	const account = params.from ?? get(ethereumStore).account
	if (!account) throw new Error('Not connected')
	await requireBridgeChain(eth)

	const txHash = (await eth.request({
		method: 'eth_sendTransaction',
		params: [
			{
				from: account,
				chainId: numberToHex(bridge.chain.id),
				to: params.address,
				data,
				value: params.value ? '0x' + params.value.toString(16) : '0x0'
			}
		]
	})) as string

	return txHash
}
