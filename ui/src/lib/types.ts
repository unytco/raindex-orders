import { type Web3Modal } from '@web3modal/wagmi'
import { type Config } from '@wagmi/core'

export enum TransactionStatus {
	IDLE = 'Idle',
	IPFS_SUCCESS = 'IPFS upload successful!',
	PENDING_WALLET = 'Waiting for wallet confirmation...',
	PENDING_TX = 'Confirming transaction...',
	SUCCESS = 'Success! Transaction confirmed',
	ERROR = 'Something went wrong'
}

export interface Web3Context {
	config: Config // Specify a more detailed type based on what `config` actually is.
	modal: Web3Modal // Specify a more detailed type for `modal` as well.
}
