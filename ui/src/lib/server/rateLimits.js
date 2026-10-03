// Plain JavaScript so that scripts/check-rate-limits.js can import it under Node at
// deploy time, with no build step.

/**
 * The Workers Rate Limiting bindings POST /api/coupon-status takes a token from before
 * it calls the RPC: one keyed by the caller's IP, and one every caller shares.
 */
export const RATE_LIMITERS = /** @type {const} */ ({
	perIp: 'COUPON_STATUS_PER_IP_LIMITER',
	total: 'COUPON_STATUS_TOTAL_LIMITER'
})

/**
 * What is wrong with the RATE_LIMITERS in a Wrangler config, as read by Wrangler's
 * `unstable_readConfig`. Empty when both are declared as rate limiting bindings.
 *
 * @param {{ unsafe?: { bindings?: Record<string, unknown>[] } }} config
 * @returns {string[]}
 */
export function rateLimiterProblems(config) {
	const bindings = config.unsafe?.bindings ?? []
	return Object.values(RATE_LIMITERS).flatMap(name => {
		const binding = bindings.find(b => b.name === name)
		if (!binding) return [`${name} is not declared in unsafe.bindings`]
		const simple = /** @type {{ limit?: unknown, period?: unknown } | undefined} */ (binding.simple)
		const problems = []
		if (binding.type !== 'ratelimit') {
			problems.push(`${name} has type ${binding.type}, not ratelimit`)
		}
		if (typeof binding.namespace_id !== 'string' || !/^[1-9]\d*$/.test(binding.namespace_id)) {
			problems.push(`${name} needs a namespace_id holding a positive integer`)
		}
		if (!Number.isInteger(simple?.limit) || Number(simple?.limit) < 1) {
			problems.push(`${name} needs a positive integer simple.limit`)
		}
		if (simple?.period !== 10 && simple?.period !== 60) {
			problems.push(`${name} needs a simple.period of 10 or 60`)
		}
		return problems
	})
}
