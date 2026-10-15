export function isHolochainKey(key: string): boolean {
	if (!key) return false
	return key.startsWith('uhCA')
}

function decodeBase64ToBytes(base64Str: string): Uint8Array {
	const standardBase64 = base64Str.replace(/-/g, '+').replace(/_/g, '/')
	const binaryString = atob(standardBase64)
	const bytes = new Uint8Array(binaryString.length)
	for (let i = 0; i < binaryString.length; i++) {
		bytes[i] = binaryString.charCodeAt(i)
	}
	return bytes
}

/**
 * The "u" prefix is a HoloHash encoding marker (indicates base64url) and is NOT
 * part of the base64 payload. After stripping it, the remaining bytes are:
 *   3-byte type prefix + 32-byte Ed25519 key + 4-byte DHT location = 39 bytes.
 */
export function holochainKeyTo32ByteHex(holoKey: string): string {
	const base64Payload = holoKey.startsWith('u') ? holoKey.slice(1) : holoKey
	const decoded = decodeBase64ToBytes(base64Payload)

	if (decoded.length < 35) {
		throw new Error(
			`Invalid Holochain key: decoded length is ${decoded.length}, expected at least 35 bytes`
		)
	}

	const rawKey = decoded.slice(3, 35)

	const hexString = Array.from(rawKey)
		.map(b => b.toString(16).padStart(2, '0'))
		.join('')

	return `0x${hexString}`
}

/** The message a thrown value carries, as wallets throw plain objects as well as Errors. */
export function errorMessage(error: unknown, fallback: string): string {
	return (error as { message?: string } | null)?.message || fallback
}

export function causeChain(err: unknown): unknown[] {
	const chain: unknown[] = []
	for (let link = err; link != null && chain.length < 10; link = (link as Error).cause) {
		chain.push(link)
	}
	return chain
}

export const READ_TIMEOUT_MS = 15_000

export async function readJson(url: string): Promise<unknown> {
	// Not AbortSignal.timeout: it needs Safari 16, and Vite 5's default build target is Safari 14.
	const timeout = new AbortController()
	const timer = setTimeout(() => timeout.abort(), READ_TIMEOUT_MS)
	try {
		const response = await fetch(url, { signal: timeout.signal })
		if (!response.ok) throw new Error(`${url} answered ${response.status}`)
		return await response.json()
	} finally {
		clearTimeout(timer)
	}
}
