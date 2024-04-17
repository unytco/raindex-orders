import { getAccount } from '@wagmi/core'
import { sepolia } from 'viem/chains'
import { createWalletClient, http, parseEther, encodePacked, keccak256, type Hex } from 'viem'
import { privateKeyToAccount, generatePrivateKey } from 'viem/accounts'

export type GetCouponArgs = {
	config: any // Replace 'any' with more specific type as per your context
	withdrawAmount: number
	ORDER_HASH: Hex
	ORDERBOOK: Hex
	CLAIM_TOKEN: Hex
	OUTPUT_VAULT_ID: Hex
	owner: any
}

export const getCoupon = async (getCouponArgs: GetCouponArgs): Promise<SignedContextV1Struct> => {
	console.log('getCouponArgs', getCouponArgs)

	const { config, withdrawAmount, owner, ORDER_HASH, ORDERBOOK, CLAIM_TOKEN, OUTPUT_VAULT_ID } =
		getCouponArgs

	/**
	 *  Our "coupon" (the SignedContext array) will be:
	 *  [0] recipient address
	 *  [1] amount
	 *  [2] expiry timestamp in seconds
	 *  Plus some domain separators
	 *  [3] order hash
	 *  [4] order owner
	 *  [5] orderbook address
	 *  [6] token address
	 *  [7] output vault id
	 *  [8] nonce
	 */

	const coupon: [bigint, bigint, bigint, bigint, bigint, bigint, bigint, bigint, bigint] = [
		BigInt(getAccount(config).address as string),
		BigInt(parseEther(withdrawAmount.toString())),
		BigInt(2687375409),
		BigInt(ORDER_HASH),
		BigInt(owner),
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

	console.log('result', signedContext)

	return signedContext
}
