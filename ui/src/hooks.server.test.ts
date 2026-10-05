import { describe, expect, it, vi } from 'vitest'
import type { Handle } from '@sveltejs/kit'
import { asBuild, MAINNET_BUILD, SEPOLIA_BUILD } from '$lib/testing/builds'

const ROUTES = ['/', '/lock', '/claim', '/api/coupon-status']
const FAUCET_ROUTES = ['/faucet', '/faucet/', '/faucet/__data.json', '/api/faucet']

async function request(build: Record<string, string>, pathname: string) {
	const { handle } = await asBuild(build, () => import('./hooks.server'))
	const resolved = new Response('the route')
	const resolve = vi.fn(async () => resolved)
	const event = { url: new URL(`https://hot-bridge.test${pathname}`) }
	const response = await (handle as Handle)({ event, resolve } as unknown as Parameters<Handle>[0])
	return { response, resolved: resolve.mock.calls.length > 0 }
}

describe('handle', () => {
	it.each(FAUCET_ROUTES)('answers 404 for %s on mainnet, without resolving it', async path => {
		const { response, resolved } = await request(MAINNET_BUILD, path)

		expect(response.status).toBe(404)
		expect(resolved).toBe(false)
	})

	it.each(FAUCET_ROUTES)('resolves %s on sepolia', async path => {
		expect((await request(SEPOLIA_BUILD, path)).resolved).toBe(true)
	})

	it.each([...ROUTES, '/faucets', '/api/faucetx'])('resolves %s on mainnet', async path => {
		expect((await request(MAINNET_BUILD, path)).resolved).toBe(true)
	})
})
