import { describe, expect, it, vi } from 'vitest'
import type { Handle } from '@sveltejs/kit'
import { asBuild, MAINNET_BUILD, SEPOLIA_BUILD } from '$lib/testing/builds'

const ROUTES: [string, string | null][] = [
	['/', '/'],
	['/lock', '/lock'],
	['/claim', '/claim'],
	['/api/coupon-status', '/api/coupon-status'],
	['/faucets', null]
]
const FAUCET_ROUTES: [string, string][] = [
	['/faucet', '/faucet'],
	['/faucet/', '/faucet'],
	['/faucet/__data.json', '/faucet'],
	['/api/faucet', '/api/faucet'],
	['/api/%66aucet', '/api/faucet'],
	['/%66aucet', '/faucet']
]

async function request(build: Record<string, string>, [pathname, route]: [string, string | null]) {
	const { handle } = await asBuild(build, () => import('./hooks.server'))
	const resolved = new Response('the route')
	const resolve = vi.fn(async () => resolved)
	const event = { url: new URL(`https://hot-bridge.test${pathname}`), route: { id: route } }
	const response = await (handle as Handle)({ event, resolve } as unknown as Parameters<Handle>[0])
	return { response, resolved: resolve.mock.calls.length > 0 }
}

describe('handle', () => {
	it.each(FAUCET_ROUTES)(
		'answers 404 for %s on mainnet, without resolving it',
		async (path, route) => {
			const { response, resolved } = await request(MAINNET_BUILD, [path, route])

			expect(response.status).toBe(404)
			expect(resolved).toBe(false)
		}
	)

	it.each(FAUCET_ROUTES)('resolves %s on sepolia', async (path, route) => {
		expect((await request(SEPOLIA_BUILD, [path, route])).resolved).toBe(true)
	})

	it.each(ROUTES)('resolves %s on mainnet', async (path, route) => {
		expect((await request(MAINNET_BUILD, [path, route])).resolved).toBe(true)
	})
})
