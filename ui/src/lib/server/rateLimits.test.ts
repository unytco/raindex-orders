import { describe, expect, it } from 'vitest'
import { unstable_readConfig } from 'wrangler'
import { rateLimiterProblems } from './rateLimits.js'

// What scripts/check-rate-limits.js decides before cf-deploy.sh runs `wrangler deploy`.

const limiter = (name: string, extra: object = {}) => ({
	name,
	type: 'ratelimit',
	namespace_id: '1036001',
	simple: { limit: 10, period: 60 },
	...extra
})
const perIp = limiter('COUPON_STATUS_PER_IP_LIMITER')

describe('rateLimiterProblems', () => {
	it('passes the committed wrangler.jsonc, as Wrangler reads it', () => {
		// npm run test runs in ui/, where cf-deploy.sh runs the check too.
		const config = unstable_readConfig({ config: 'wrangler.jsonc' }, { hideWarnings: true })

		expect(rateLimiterProblems(config)).toEqual([])
	})

	it('fails a config without the rate limiters, naming both', () => {
		const problems = rateLimiterProblems({ unsafe: {} })

		expect(problems).toHaveLength(2)
		expect(problems.join()).toContain('COUPON_STATUS_PER_IP_LIMITER')
		expect(problems.join()).toContain('COUPON_STATUS_TOTAL_LIMITER')
	})

	it.each([
		['one limiter missing', [perIp]],
		['a limiter of another type', [perIp, limiter('COUPON_STATUS_TOTAL_LIMITER', { type: 'kv' })]],
		[
			'a limiter with no namespace_id',
			[perIp, limiter('COUPON_STATUS_TOTAL_LIMITER', { namespace_id: undefined })]
		],
		[
			'a limiter with no limit',
			[perIp, limiter('COUPON_STATUS_TOTAL_LIMITER', { simple: { period: 60 } })]
		],
		[
			'a limiter with a period Cloudflare refuses',
			[perIp, limiter('COUPON_STATUS_TOTAL_LIMITER', { simple: { limit: 60, period: 30 } })]
		]
	])('fails a config with %s', (_, bindings) => {
		const problems = rateLimiterProblems({ unsafe: { bindings } })

		expect(problems).toHaveLength(1)
		expect(problems[0]).toContain('COUPON_STATUS_TOTAL_LIMITER')
	})
})
