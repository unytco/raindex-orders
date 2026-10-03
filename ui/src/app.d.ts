// See https://kit.svelte.dev/docs/types#app
// for information about these interfaces
declare global {
	namespace App {
		// interface Error {}
		// interface Locals {}
		// interface PageData {}
		// interface PageState {}
		interface Platform {
			// Bindings declared in wrangler.jsonc. Optional, since a route must not trust a
			// deploy to have carried them.
			env?: {
				COUPON_STATUS_PER_IP_LIMITER?: RateLimiter
				COUPON_STATUS_TOTAL_LIMITER?: RateLimiter
			}
		}
	}

	/** A Workers Rate Limiting binding. */
	interface RateLimiter {
		limit(options: { key: string }): Promise<{ success: boolean }>
	}
}

export {}
