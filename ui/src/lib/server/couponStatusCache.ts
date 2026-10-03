import type { Hex } from 'viem'

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
 * Reads cached against a coupon's canonical identity (see couponId in the route), so
 * every spelling of one coupon shares an entry. Only a coupon whose signature verified
 * is ever stored, and the identity covers its signature and context, so an entry means
 * that coupon is genuine. The Workers Cache is local to each Cloudflare data centre.
 *
 * The cache only saves reads: a failed match counts as a miss and a failed put is
 * dropped, each logged, so neither changes an answer.
 */
export class CouponStatusCache {
	constructor(
		private readonly cache: CouponCache,
		private readonly origin: string
	) {}

	async get(id: Hex): Promise<CachedRead | undefined> {
		try {
			const entry = await this.cache.match(this.key(id))
			return entry ? ((await entry.json()) as CachedRead) : undefined
		} catch (e) {
			console.error('coupon-status: cache match failed; reading the chain instead:', e)
			return undefined
		}
	}

	async put(id: Hex, read: CachedRead): Promise<void> {
		const response = new Response(JSON.stringify(read), {
			headers: {
				'Content-Type': 'application/json',
				'Cache-Control': `max-age=${ENTRY_MAX_AGE_S}`
			}
		})
		try {
			await this.cache.put(this.key(id), response)
		} catch (e) {
			console.error('coupon-status: cache put failed; answering without it:', e)
		}
	}

	private key(id: Hex): string {
		return `${this.origin}/api/coupon-status/cache/${id}`
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
