export enum TransactionStatus {
	IDLE = 'Idle',
	IPFS_SUCCESS = 'IPFS upload successful!',
	PENDING_WALLET = 'Waiting for wallet confirmation...',
	PENDING_TX = 'Confirming transaction...',
	SUCCESS = 'Success! Transaction confirmed',
	ERROR = 'Something went wrong'
}
