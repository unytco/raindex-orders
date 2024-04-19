// getCoupon.ts
import { parseEther, encodePacked, keccak256, type Hex } from 'viem'
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts'
import { createWalletClient, http } from 'viem'
import { sepolia } from 'viem/chains'
import { getAccount } from '@wagmi/core'
import { getContext } from 'svelte'
import { formatDate } from './utils'

export type Order = {
	owner: Hex
	handleIO: boolean
	evaluable: {
		interpreter: Hex
		store: Hex
		expression: Hex
	}
	validInputs: Array<{
		token: Hex
		decimals: number
		vaultId: bigint
	}>
	validOutputs: Array<{
		token: Hex
		decimals: number
		vaultId: bigint
	}>
}

type CouponConfig = {
	ORDER_HASH: string
	ORDERBOOK: string
	CLAIM_TOKEN: string
	OUTPUT_VAULT_ID: string
	withdrawAmount: number
	order: Order
}

type SignedContextV1Struct = {
	signer: string
	signature: string
	context: bigint[]
}

type FullContext = {
	signedContext: SignedContextV1Struct
	renderedValues: {
		'Recipient Address': string
		'Withdraw Amount': number
		'Order Expires': string
	}
}
export const getCoupon = async ({
	withdrawAmount,
	ORDER_HASH,
	ORDERBOOK,
	CLAIM_TOKEN,
	OUTPUT_VAULT_ID,
	order
}: CouponConfig): Promise<FullContext> => {
	console.log('coupon', order)
	const web3ContextKey = 'web3Context'
	const { config, modal } = getContext(web3ContextKey)
	console.log('hi!')
	console.log(getAccount(config).address)
	const coupon: [bigint, bigint, bigint, bigint, bigint, bigint, bigint, bigint, bigint] = [
		BigInt(getAccount(config).address as string),
		BigInt(parseEther(withdrawAmount.toString())),
		BigInt(2687375409),
		BigInt(ORDER_HASH),
		BigInt(order?.owner || 0),
		BigInt(ORDERBOOK),
		BigInt(CLAIM_TOKEN),
		BigInt(OUTPUT_VAULT_ID),
		BigInt(generatePrivateKey()) // getting a random 32 bytes to use as a nonce
	]

	const message = keccak256(
		encodePacked(
			[
				'uint256',
				'uint256',
				'uint256',
				'uint256',
				'uint256',
				'uint256',
				'uint256',
				'uint256',
				'uint256'
			],
			coupon
		)
	)

	const client = createWalletClient({
		chain: sepolia,
		transport: http()
	})

	const account = privateKeyToAccount(
		'0xdcbe53cbf4cbee212fe6339821058f2787c7726ae0684335118cdea2e8adaafd'
	)

	const signature = await client.signMessage({
		account,
		message: { raw: message }
	})

	const signedContext = {
		signer: '0x8E72b7568738da52ca3DCd9b24E178127A4E7d37',
		signature,
		context: coupon
	}

	const fullContext = {
		signedContext,
		renderedValues: {
			'Recipient Address': getAccount(config).address,
			'Withdraw Amount': withdrawAmount,
			'Order Expires': formatDate(signedContext.context[2])
		}
	}

	return fullContext
}
