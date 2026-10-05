import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { generatePrivateKey } from 'viem/accounts'

const { RPC_HOST, RPC_KEY, RPC_URL } = vi.hoisted(() => {
	const RPC_HOST = 'sepolia.rpc-provider.test'
	const RPC_KEY = 'secret-api-key-1234'
	return { RPC_HOST, RPC_KEY, RPC_URL: `https://${RPC_HOST}/v3/${RPC_KEY}` }
})

vi.mock('$env/dynamic/private', () => ({
	env: { FAUCET_PRIVATE_KEY: generatePrivateKey(), SEPOLIA_RPC_URL: RPC_URL }
}))
vi.mock('$env/static/public', async () => (await import('$lib/testing/builds')).SEPOLIA_BUILD)

import { GET, POST } from './+server'

type Event = Parameters<typeof POST>[0]

const failures: [string, () => Promise<Response>][] = [
	[
		'cannot be reached',
		async () => {
			throw new TypeError('fetch failed')
		}
	],
	[
		'refuses the key and names it',
		async () =>
			Response.json({ error: `invalid API key ${RPC_KEY} for ${RPC_URL}` }, { status: 401 })
	],
	[
		'answers a JSON-RPC error naming the URL',
		async () => Response.json({ jsonrpc: '2.0', id: 1, error: { code: -32000, message: RPC_URL } })
	]
]

const calls: Record<string, () => ReturnType<typeof GET>> = {
	'GET /api/faucet': () => GET({} as Event),
	'POST /api/faucet': () =>
		POST({
			request: new Request('http://localhost/api/faucet', {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ recipient: '0x1111111111111111111111111111111111111111' })
			})
		} as Event)
}

let logged: unknown[][]

beforeEach(() => {
	logged = []
	vi.spyOn(console, 'error').mockImplementation((...args) => void logged.push(args))
})

afterEach(() => {
	vi.unstubAllGlobals()
	vi.restoreAllMocks()
})

describe.each(Object.entries(calls))('%s', (_, call) => {
	it.each(failures)(
		'answers 500 without the RPC URL, host or key when the RPC %s',
		async (_, fetch) => {
			vi.stubGlobal('fetch', vi.fn(fetch))

			const response = await call()
			const text = await response.text()

			expect(response.status).toBe(500)
			expect(JSON.parse(text)).toEqual({ error: expect.any(String) })
			for (const secret of [RPC_URL, RPC_HOST, RPC_KEY]) {
				expect(text).not.toContain(secret)
				expect(JSON.stringify(logged)).not.toContain(secret)
			}
		}
	)
})
