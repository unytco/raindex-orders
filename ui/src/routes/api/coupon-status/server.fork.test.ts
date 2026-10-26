import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { asBuild } from '$lib/testing/builds'
import { fakeCache } from './fakeRpc'

/**
 * coupon-status against a real Safe as the claim signer, on the anvil fork of
 * mainnet test/fork-rehearsal.sh deploys the bridge on. The rehearsal sets the
 * build variables of its deploy record, the fork's RPC, and three coupons for the
 * Safe: one its owners signed, which the Safe's isValidSignature accepts, one a
 * single owner signed and one an owner and a stranger signed, which it refuses.
 * Skipped unless the rehearsal runs it.
 */
const rehearsal = process.env.REHEARSAL_RPC_URL
const env = vi.hoisted(() => ({}) as Record<string, string | undefined>)
vi.mock('$env/dynamic/private', () => ({ env }))

const build = () =>
	Object.fromEntries(
		Object.entries(process.env).filter(([key]) => key.startsWith('PUBLIC_'))
	) as Record<string, string>

const variable = (name: string) => {
	const value = process.env[name]
	if (!value) throw new Error(`${name} is not set`)
	return value
}

describe.skipIf(!rehearsal)(
	'coupon-status on the fork rehearsal, with a Safe as the claim signer',
	() => {
		let cache: ReturnType<typeof fakeCache>

		beforeEach(() => {
			cache = fakeCache()
			env.ETH_RPC_URL = rehearsal
			vi.spyOn(console, 'error').mockImplementation(() => {})
		})

		afterEach(() => {
			vi.restoreAllMocks()
		})

		async function post(coupons: string[]) {
			const { POST } = await asBuild(build(), () => import('./+server'))
			const response = await POST({
				request: new Request('http://localhost/api/coupon-status', {
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({ coupons })
				}),
				platform: { caches: { open: cache.open } }
			} as unknown as Parameters<typeof POST>[0])
			return { response, body: await response.json() }
		}

		it(`answers ${process.env.REHEARSAL_SAFE_COUPON_STATUS} for the coupon the Safe accepts, and invalid for the two it refuses`, async () => {
			const { response, body } = await post([
				variable('SAFE_COUPON'),
				variable('ONE_OWNER_COUPON'),
				variable('NON_OWNER_COUPON')
			])

			expect(response.status).toBe(200)
			expect(body.chainId).toBe(1)
			expect(body.block).not.toBeNull()
			expect(body.results.map((r: { status: string }) => r.status)).toEqual([
				variable('REHEARSAL_SAFE_COUPON_STATUS'),
				'invalid',
				'invalid'
			])
			expect(console.error).not.toHaveBeenCalled()
		})
	}
)
