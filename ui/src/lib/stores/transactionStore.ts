import { writable } from 'svelte/store'
import type { Abi } from 'viem'

export enum TransactionStatus {
	IDLE = 'Idle',
	IPFS_SUCCESS = 'IPFS upload successful!',
	CHECKING = 'Checking your coupon…',
	PENDING_WALLET = 'Waiting for your manual confirmation in your blockchain wallet.',
	PENDING_TX = 'Confirming transaction...',
	SUCCESS = 'Success! Transaction confirmed',
	UNCONFIRMED = 'Not confirmed yet',
	ERROR = 'Something went wrong'
}

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
	hash?: string
	unconfirmed?: boolean
}

const initialState = {
	status: TransactionStatus.IDLE,
	error: { message: '' },
	hash: '',
	message: '',
	data: null,
	isLockTransaction: false,
	dismissed: false
}

export const createTransactionStore = () => {
	const { subscribe, set, update } = writable(initialState)
	const reset = () => set(initialState)
	const dismiss = () => update(state => ({ ...state, dismissed: true }))
	const enter = (step: Partial<typeof initialState>) =>
		update(state => ({ ...state, ...step, dismissed: false }))
	const awaitCheck = () => set({ ...initialState, status: TransactionStatus.CHECKING })
	const awaitWalletConfirmation = (isLockTransaction = false) =>
		enter({ status: TransactionStatus.PENDING_WALLET, isLockTransaction, hash: '' })
	const awaitTxReceipt = (hash: string) => enter({ status: TransactionStatus.PENDING_TX, hash })
	const transactionSuccess = (hash: string) => enter({ status: TransactionStatus.SUCCESS, hash })
	const transactionError = (txError: TxError) =>
		update(state => ({
			...state,
			status: txError.unconfirmed ? TransactionStatus.UNCONFIRMED : TransactionStatus.ERROR,
			error: { message: txError.message },
			hash: txError.hash ?? state.hash,
			dismissed: false
		}))

	return {
		subscribe,
		reset,
		dismiss,
		awaitCheck,
		awaitWalletConfirmation,
		awaitTxReceipt,
		transactionSuccess,
		transactionError
	}
}

export const transactionStore = createTransactionStore()
