import { writable } from 'svelte/store'
import type { Abi } from 'viem'
import { TransactionStatus } from '$lib/types'

export type InitiateTransactionArgs = {
	contractAddress: string
	abi: Abi
	functionName: string
	args: never[]
	ipfsUpload: boolean
}

export type TxError = {
	message: string
	details?: string
}

const initialState = {
	status: TransactionStatus.IDLE,
	error: { message: '' },
	hash: '',
	message: '',
	data: null
}

// TODO: Add a timeout on all transactions
const transactionStore = () => {
	const { subscribe, set, update } = writable(initialState)
	const reset = () => set(initialState)
	const awaitWalletConfirmation = () =>
		update(state => ({ ...state, status: TransactionStatus.PENDING_WALLET }))
	const awaitTxReceipt = (txHash: string) =>
		update(state => ({ ...state, status: TransactionStatus.PENDING_TX, hash: txHash }))
	const transactionSuccess = (hash: string) =>
		update(state => ({
			...state,
			status: TransactionStatus.SUCCESS,
			hash: hash
		}))
	const transactionError = (txError: TxError) =>
		update(state => ({
			...state,
			status: TransactionStatus.ERROR,
			error: { message: txError.message }
		}))

	return {
		subscribe,
		reset,
		awaitWalletConfirmation,
		awaitTxReceipt,
		transactionSuccess,
		transactionError
	}
}

export default transactionStore()
