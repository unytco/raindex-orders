// coupon.ts — client-safe coupon helpers.
// Coupon SIGNING is done server-side only (the bridge orchestrator, see
// bridge-orchestrator/src/signer.rs). No signing key lives in this module or any
// client-bundled code (issue #14).
import { type Hex, type Address } from 'viem'

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
		signer: signer as Hex,
		signature: signature as Hex,
		context: context.map((n) => BigInt(n))
	}
}