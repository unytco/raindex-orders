import { describe, expect, it, vi } from 'vitest'
import type { Handle } from '@sveltejs/kit'
import { asBuild, BUILDS, MAINNET_BUILD, SEPOLIA_BUILD } from '$lib/testing/builds'
import { NOT_PAUSED } from '$lib/testing/pause'

type Route = [string, string | null]

const ROUTES: Route[] = [
	['/', '/'],
	['/lock', '/lock'],
	['/claim', '/claim'],
	['/api/coupon-status', '/api/coupon-status'],
	['/api/status', '/api/status'],
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
const LOCK_PAGE: Route[] = [
	['/lock', '/lock'],
	['/lock/', '/lock'],
	['/%6cock', '/lock']
]
const FAUCET_PAGE = FAUCET_ROUTES.filter(([, route]) => route === '/faucet')
const FAUCET_API = FAUCET_ROUTES.filter(([, route]) => route === '/api/faucet')
const OPEN_WHILE_PAUSED = ROUTES.filter(([, route]) => route !== '/lock')
const STOP_TEXT =
	'The HOT bridge is paused. It will be back soon. For more information, contact info@unyt.co.'

async function request(
	build: Record<string, string>,
	[pathname, route]: Route,
	env: Record<string, string> = {}
) {
	const { handle } = await asBuild(build, () => {
		vi.doMock('$env/dynamic/private', () => ({ env }))
		return import('./hooks.server')
	})
	const resolved = new Response('the route')
	const resolve = vi.fn(async () => resolved)
	const event = { url: new URL(`https://hot-bridge.test${pathname}`), route: { id: route } }
	const response = await (handle as Handle)({ event, resolve } as unknown as Parameters<Handle>[0])
	return { response, resolved: resolve.mock.calls.length > 0 }
}

const paused = (build: Record<string, string>, route: Route) =>
	request(build, route, { BRIDGE_PAUSED: 'true' })

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

describe.each([
	['sepolia', [...LOCK_PAGE, ...FAUCET_PAGE]],
	['mainnet', LOCK_PAGE]
] as const)('while paused, a %s build', (network, stopped) => {
	const build = BUILDS[network]

	it.each(stopped)('answers %s with the stop page, without resolving it', async (path, route) => {
		const { response, resolved } = await paused(build, [path, route])
		const html = await response.text()

		expect(response.status).toBe(503)
		expect(response.headers.get('Cache-Control')).toBe('no-store')
		expect(response.headers.get('Content-Type')).toBe('text/html; charset=utf-8')
		expect(html.replace(/<[^>]+>/g, '').replace(/\s+/g, ' ')).toContain(STOP_TEXT)
		expect(html).toContain('<a href="mailto:info@unyt.co">info@unyt.co</a>')
		expect(html).not.toContain('<script')
		expect(resolved).toBe(false)
	})

	it.each(OPEN_WHILE_PAUSED)('resolves %s', async (path, route) => {
		expect((await paused(build, [path, route])).resolved).toBe(true)
	})
})

describe('while paused', () => {
	it.each(FAUCET_API)(
		'answers %s on sepolia with the stop text as JSON, without resolving it',
		async (path, route) => {
			const { response, resolved } = await paused(SEPOLIA_BUILD, [path, route])

			expect(response.status).toBe(503)
			expect(response.headers.get('Access-Control-Allow-Origin')).toBe('*')
			expect(await response.json()).toEqual({ error: STOP_TEXT })
			expect(resolved).toBe(false)
		}
	)

	it.each(FAUCET_ROUTES)('answers 404 for %s on mainnet', async (path, route) => {
		const { response, resolved } = await paused(MAINNET_BUILD, [path, route])

		expect(response.status).toBe(404)
		expect(resolved).toBe(false)
	})
})

describe.each(NOT_PAUSED)('with %o', env => {
	it.each([...ROUTES, ...FAUCET_ROUTES])('resolves %s on sepolia', async (path, route) => {
		expect((await request(SEPOLIA_BUILD, [path, route], env)).resolved).toBe(true)
	})

	it.each(ROUTES)('resolves %s on mainnet', async (path, route) => {
		expect((await request(MAINNET_BUILD, [path, route], env)).resolved).toBe(true)
	})

	it.each(FAUCET_ROUTES)('answers 404 for %s on mainnet', async (path, route) => {
		expect((await request(MAINNET_BUILD, [path, route], env)).response.status).toBe(404)
	})
})

describe('every route', () => {
	it('is answered by the Worker, as none is prerendered', () => {
		const modules = import.meta.glob('./routes/**/+*.{js,ts}', {
			query: '?raw',
			import: 'default',
			eager: true
		})

		expect(Object.keys(modules)).toEqual(
			expect.arrayContaining(['./routes/+layout.ts', './routes/api/status/+server.ts'])
		)
		for (const [path, source] of Object.entries(modules)) {
			expect(source, path).not.toMatch(/\bprerender\b/)
		}
	})
})
