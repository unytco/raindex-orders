// What a Safe owner signs for a HOT claim coupon, as docs/signer-service.md
// describes it: the reference a signer service can compare its own code with.
// bridge-orchestrator/src/signer/safe.rs computes the same hashes in Rust, and
// test/TestSafeCouponSignature.t.sol pins them against Safe 1.4.1 on chain.
import { encodePacked, hashTypedData, keccak256 } from 'viem'
import { privateKeyToAccount } from 'viem/accounts'

/** The digest Raindex checks a coupon's signature over: the EIP-191 hash of keccak256 of the nine packed words. */
export function couponDigest(words) {
	const packed = keccak256(encodePacked(Array(9).fill('bytes32'), words))
	return keccak256(
		encodePacked(['string', 'bytes32'], ['\x19Ethereum Signed Message:\n32', packed])
	)
}

/**
 * The coupon as EIP-712 typed data for the Safe: `SafeMessage(bytes message)`, the
 * message being the digest's 32 bytes, under the Safe's domain on its chain. A
 * wallet's eth_signTypedData_v4 of this signs `safeMessageHash`.
 */
export function safeMessageTypedData({ chainId, safe, digest }) {
	return {
		domain: { chainId, verifyingContract: safe },
		types: { SafeMessage: [{ name: 'message', type: 'bytes' }] },
		primaryType: 'SafeMessage',
		message: { message: digest }
	}
}

/** The hash each owner signs: the EIP-712 hash of `safeMessageTypedData`. */
export function safeMessageHash({ chainId, safe, digest }) {
	return hashTypedData(safeMessageTypedData({ chainId, safe, digest }))
}

/** An owner's 65-byte signature of `hash`: r, s, then v as 27 or 28, with no prefix. */
export async function signAsOwner(key, hash) {
	return privateKeyToAccount(key).sign({ hash })
}
