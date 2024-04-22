// getCoupon.ts
import { parseEther, encodePacked, keccak256, type Hex, type Address } from 'viem'
import { privateKeyToAccount } from 'viem/accounts'
import { createWalletClient, http } from 'viem'
import { sepolia } from 'viem/chains'

export type CouponConfig = {
	recipient: Address
	orderHash: Hex
	orderbookAddress: Address
	claimTokenAddress: Address
	outputVaultId: Hex
	withdrawAmount: bigint
	orderOwner: Address
	nonce: bigint
	expiryTimestamp: number
}

export type SignedContextV1Struct = {
	signer: Hex
	signature: Hex
	context: bigint[]
}
	

export const generateSignedContext = async ({
	recipient,
	withdrawAmount,
	orderHash,
	orderbookAddress,
	claimTokenAddress,
	outputVaultId,
	orderOwner,
	nonce,
	expiryTimestamp
}: CouponConfig): Promise<SignedContextV1Struct> => {

	const coupon: [bigint, bigint, bigint, bigint, bigint, bigint, bigint, bigint, bigint] = [
		BigInt(recipient),
		withdrawAmount,
		BigInt(expiryTimestamp),
		BigInt(orderHash),
		BigInt(orderOwner),
		BigInt(orderbookAddress),
		BigInt(claimTokenAddress),
		BigInt(outputVaultId),
		BigInt(nonce)
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

	return signedContext
}

export const parseCoupon = (signedContext: SignedContextV1Struct): CouponConfig => {
	const [
		recipient,
		withdrawAmount,
		expiryTimestamp,
		orderHash,
		orderOwner,
		orderbookAddress,
		claimTokenAddress,
		outputVaultId,
		nonce
	] = signedContext.context

	return {
		recipient: `0x${recipient.toString(16)}`,
		withdrawAmount,
		expiryTimestamp: Number(expiryTimestamp),
		orderHash: `0x${orderHash.toString(16)}`,
		orderOwner: `0x${orderOwner.toString(16)}`,
		orderbookAddress: `0x${orderbookAddress.toString(16)}`,
		claimTokenAddress: `0x${claimTokenAddress.toString(16)}`,
		outputVaultId: `0x${outputVaultId.toString(16)}`,
		nonce
	}
}

export const serializeSignedContext = (signedContext: SignedContextV1Struct): string => {
	// we can't use JSON.stringify because the context is an array of BigInts
	// but we need to serialize all of it as a string
	const serialized = signedContext.context.map((n) => n.toString()).join(',')
	return `${signedContext.signer},${signedContext.signature},${serialized}`
}

export const deserializeSignedContext = (serialized: string): SignedContextV1Struct => {
	const [signer, signature, ...context] = serialized.split(',')
	return {
		signer,
		signature,
		context: context.map((n) => BigInt(n))
	}
}