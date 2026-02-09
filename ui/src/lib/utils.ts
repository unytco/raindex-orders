import moment from 'moment'

export function formatDate(timestamp: string): string {
	return moment(parseInt(timestamp) * 1000).fromNow()
}

export function truncateEthAddress(address: string, length: number = 3): string {
	if (!address.startsWith('0x') || address.length !== 42) {
		throw new Error('Invalid Ethereum address')
	}

	if (address.length <= 2 * length + 2) {
		return address
	}

	return `${address.substring(0, length + 2)}...${address.substring(address.length - length)}`
}

/**
 * Check if a string looks like a Holochain agent key (base64-encoded, starts with "uhCA").
 */
export function isHolochainKey(key: string): boolean {
	if (!key) return false
	return key.startsWith('uhCA')
}

/**
 * Decode a base64 (or base64url) string into a Uint8Array using browser-native atob().
 */
function decodeBase64ToBytes(base64Str: string): Uint8Array {
	// Convert base64url characters to standard base64
	const standardBase64 = base64Str.replace(/-/g, '+').replace(/_/g, '/')
	const binaryString = atob(standardBase64)
	const bytes = new Uint8Array(binaryString.length)
	for (let i = 0; i < binaryString.length; i++) {
		bytes[i] = binaryString.charCodeAt(i)
	}
	return bytes
}

/**
 * Convert a Holochain agent key (e.g. "uhCAk...") to a 0x-prefixed 32-byte hex string.
 *
 * The "u" prefix is a HoloHash encoding marker (indicates base64url) and is NOT
 * part of the base64 payload. After stripping it, the remaining bytes are:
 *   3-byte type prefix + 32-byte Ed25519 key + 4-byte DHT hash = 39 bytes.
 * We extract bytes 3–35 (the Ed25519 public key) and return them as hex.
 */
export function holochainKeyTo32ByteHex(holoKey: string): string {
	// Strip the "u" HoloHash encoding prefix before base64 decoding
	const base64Payload = holoKey.startsWith('u') ? holoKey.slice(1) : holoKey
	const decoded = decodeBase64ToBytes(base64Payload)

	if (decoded.length < 35) {
		throw new Error(
			`Invalid Holochain key: decoded length is ${decoded.length}, expected at least 35 bytes`
		)
	}

	// Extract the 32-byte Ed25519 public key (bytes 3-35)
	const rawKey = decoded.slice(3, 35)

	const hexString = Array.from(rawKey)
		.map(b => b.toString(16).padStart(2, '0'))
		.join('')

	return `0x${hexString}`
}
