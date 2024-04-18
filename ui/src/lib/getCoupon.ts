// getCoupon.ts
import { parseEther, encodePacked, keccak256 } from 'viem'
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts'
import { createWalletClient, http } from 'viem'
import { sepolia } from 'viem/chains'
import { getAccount } from '@wagmi/core'

type CouponConfig = {
	config: any // Define the type according to your application specifics
	ORDER_HASH: string
	ORDERBOOK: string
	CLAIM_TOKEN: string
	OUTPUT_VAULT_ID: string
	withdrawAmount: number
}

type SignedContextV1Struct = {
	signer: string
	signature: any // Define according to the data structure used for signatures
	context: bigint[]
}

export async function getCoupon({
	config,
	ORDER_HASH,
	ORDERBOOK,
	CLAIM_TOKEN,
	OUTPUT_VAULT_ID,
	withdrawAmount
}: CouponConfig): Promise<SignedContextV1Struct> {
	console.log(config, ORDER_HASH, ORDERBOOK, CLAIM_TOKEN, OUTPUT_VAULT_ID, withdrawAmount)
    console.log(getAccount(config).address)
	const coupon: bigint[] = [
		BigInt(getAccount(config).address as string),
		BigInt(parseEther(withdrawAmount.toString())),
		BigInt(2687375409),
		BigInt(ORDER_HASH),
		BigInt(0), // Placeholder for 'order owner', adjust as needed
		BigInt(ORDERBOOK),
		BigInt(CLAIM_TOKEN),
		BigInt(OUTPUT_VAULT_ID),
		BigInt(generatePrivateKey()) // Random nonce
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

	const privateKey = '0xdcbe53cbf4cbee212fe6339821058f2787c7726ae0684335118cdea2e8adaafd'
	const account = privateKeyToAccount(privateKey)

	const signature = await client.signMessage({
		account,
		message: { raw: message }
	})

	return {
		signer: '0x8E72b7568738da52ca3DCd9b24E178127A4E7d37',
		signature,
		context: coupon
	}
}
