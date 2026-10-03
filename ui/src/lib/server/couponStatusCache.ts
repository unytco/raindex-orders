import { keccak256, stringToHex } from 'viem'

export type ReadStatus = 'redeemed' | 'unredeemed' | 'expired'

/** What a chain read said about one coupon, and when (ms since the epoch). */
export type CachedRead = { status: ReadStatus; readAt: number }

/** The part of a Workers Cache this module uses. */
export type CouponCache = {
	match(key: string): Promise<{ json(): Promise<unknown> } | undefined>
	put(key: string, response: Response): Promise<void>
}

/** How long an `unredeemed` read is served before the chain is read again. */
export const UNREDEEMED_TTL_MS = 60_000

// An entry also proves its coupon's signature was verified, so it is kept long after an
// `unredeemed` read goes stale. Cloudflare may evict it earlier; that costs one
// signature check and one read.
const ENTRY_MAX_AGE_S = 30 * 24 * 60 * 60

/**
 * Reads cached against the exact coupon text. Only a coupon whose signature verified is
 * ever stored, so an entry for a coupon means it is genuine. The Workers Cache is local
 * to each Cloudflare data centre.
 */
export class CouponStatusCache {
	constructor(
		private readonly cache: CouponCache,
		private readonly origin: string
	) {}

	async get(coupon: string): Promise<CachedRead | undefined> {
		const entry = await this.cache.match(this.key(coupon))
		return entry ? ((await entry.json()) as CachedRead) : undefined
	}

	async put(coupon: string, read: CachedRead): Promise<void> {
		const response = new Response(JSON.stringify(read), {
			headers: {
				'Content-Type': 'application/json',
				'Cache-Control': `max-age=${ENTRY_MAX_AGE_S}`
			}
		})
		await this.cache.put(this.key(coupon), response)
	}

	private key(coupon: string): string {
		return `${this.origin}/api/coupon-status/cache/${keccak256(stringToHex(coupon))}`
	}
}

/**
 * The status a cached read still gives at `now` (ms), or undefined when the chain must
 * be read again. `redeemed` never changes. `expired` was read at a safe block whose
 * timestamp had reached the expiry, and src/holo-claim.rain refuses a claim from then
 * on, so this coupon can never be claimed; a later claim of a re-issued coupon with the
 * same nonce is reported on that coupon. `unredeemed` holds for UNREDEEMED_TTL_MS, and
 * never once the coupon's own expiry (unix s) has passed.
 */
export function settledStatus(
	read: CachedRead,
	expiry: bigint,
	now: number
): ReadStatus | undefined {
	if (read.status !== 'unredeemed') return read.status
	const fresh = now - read.readAt < UNREDEEMED_TTL_MS
	const unexpired = BigInt(Math.floor(now / 1000)) < expiry
	return fresh && unexpired ? 'unredeemed' : undefined
}
