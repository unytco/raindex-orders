// Fails the deploy unless the Wrangler config declares the rate limiting bindings
// POST /api/coupon-status needs. Usage: node scripts/check-rate-limits.js [config]
import { unstable_readConfig } from 'wrangler'
import { rateLimiterProblems } from '../src/lib/server/rateLimits.js'

const configPath = process.argv[2] ?? 'wrangler.jsonc'
const problems = rateLimiterProblems(
	unstable_readConfig({ config: configPath }, { hideWarnings: true })
)

if (problems.length > 0) {
	console.error(`Refusing to deploy: ${configPath} does not rate limit /api/coupon-status.`)
	for (const problem of problems) console.error(`- ${problem}`)
	process.exit(1)
}
console.log(`${configPath} declares the /api/coupon-status rate limiters.`)
