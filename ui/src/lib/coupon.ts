// coupon.ts — client-safe coupon helpers.
// Coupon SIGNING is done server-side only (the bridge orchestrator, see
// bridge-orchestrator/src/signer.rs). No signing key lives in this module or any
// client-bundled code (issue #14).
import { isAddress, isHex, numberToHex, type Hex, type Address } from 'viem'

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

	const address = (word: bigint) => numberToHex(word, { size: 20 })
	return {
		recipient: address(recipient),
		withdrawAmount,
		expiryTimestamp: Number(expiryTimestamp),
		orderHash: numberToHex(orderHash, { size: 32 }),
		orderOwner: address(orderOwner),
		orderbookAddress: address(orderbookAddress),
		claimTokenAddress: address(claimTokenAddress),
		outputVaultId: numberToHex(outputVaultId, { size: 32 }),
		nonce
	}
}

export const serializeSignedContext = (signedContext: SignedContextV1Struct): string => {
	// we can't use JSON.stringify because the context is an array of BigInts
	// but we need to serialize all of it as a string
	const serialized = signedContext.context.map(n => n.toString()).join(',')
	return `${signedContext.signer},${signedContext.signature},${serialized}`
}

export const deserializeSignedContext = (serialized: string): SignedContextV1Struct => {
	const [signer, signature, ...context] = serialized.split(',')
	if (!isAddress(signer, { strict: false }) || !isHex(signature, { strict: true })) {
		throw new Error('Not a coupon: its signer or signature is malformed')
	}
	return {
		signer: signer.toLowerCase() as Address,
		signature,
		context: context.map(n => BigInt(n))
	}
}
