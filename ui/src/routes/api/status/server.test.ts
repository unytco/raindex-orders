import { describe, expect, it, vi } from 'vitest'
import { NOT_PAUSED } from '$lib/testing/pause'

async function status(env: Record<string, string>) {
	vi.resetModules()
	vi.doMock('$env/dynamic/private', () => ({ env }))
	const { GET } = await import('./+server')
	return GET({} as Parameters<typeof GET>[0])
}

async function read(env: Record<string, string>) {
	const response = await status(env)
	return {
		status: response.status,
		body: await response.json(),
		origin: response.headers.get('Access-Control-Allow-Origin'),
		cache: response.headers.get('Cache-Control')
	}
}

describe('GET /api/status', () => {
	it('answers paused, to any origin, while BRIDGE_PAUSED is true', async () => {
		expect(await read({ BRIDGE_PAUSED: 'true' })).toEqual({
			status: 200,
			body: { paused: true },
			origin: '*',
			cache: 'no-store'
		})
	})

	it.each(NOT_PAUSED)('answers not paused, to any origin, with %o', async env => {
		expect(await read(env)).toEqual({
			status: 200,
			body: { paused: false },
			origin: '*',
			cache: 'no-store'
		})
	})
})
