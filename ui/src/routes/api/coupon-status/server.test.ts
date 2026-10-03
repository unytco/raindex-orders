import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
	decodeFunctionData,
	encodeFunctionResult,
	encodePacked,
	keccak256,
	maxUint256,
	multicall3Abi,
	parseAbi,
	type Hex
} from 'viem'
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts'
import { SEPOLIA_REDEEMED } from './fixtures'

const env = vi.hoisted(() => ({
	SEPOLIA_RPC_URL: 'https://rpc.test/secret-key' as string | undefined
}))
vi.mock('$env/dynamic/private', () => ({ env }))
vi.mock('$env/static/public', () => ({
	PUBLIC_ORDERBOOK_ADDRESS: '0xfca89cD12Ba1346b1ac570ed988AB43b812733fe'
}))

import { OPTIONS, POST } from './+server'

const MULTICALL3 = '0xca11bde05977b3631167028862be2a173976ca11'
const STORE = '0x23f77e7bc935503e437166498d7d72f2ea290e1f'
const SECP256K1_N = 0xfffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141n

// Read on Sepolia: the claim order's store namespace, and the store keys the nonces of
// the first two fixtures were marked at when they were redeemed.
const CLAIM_NAMESPACE = 0x2eff224ba29f37818f323107f4b089a0f598bb8cc44e3bf2738bbe591b2df7d1n
const [A, B, C] = SEPOLIA_REDEEMED.map(({ coupon }) => coupon)
const KEY_A = 0x5b791f671a06745c3b5c4c4bc3446ff7dfe601ba6e695a0d0b1f4af55fe50a3an
const KEY_B = 0x42fd2eddc5420575975b7b001ea50a43a796c99cb93410a97ab467bf37e6d433n
const EXPIRY_A = 1_776_472_741
const EXPIRY_B = 1_776_472_743
const EXPIRY_C = 1_776_472_922

const BLOCK = 10_883_600n
const BEFORE_ALL = 1_776_400_000

const readAbi = parseAbi([
	'function get(uint256 namespace, uint256 key) view returns (uint256)',
	'function getChainId() view returns (uint256)',
	'function getBlockNumber() view returns (uint256)',
	'function getCurrentBlockTimestamp() view returns (uint256)'
])

/** The fields of a coupon: signer, signature, then context 0 to 8. */
const fieldsOf = (coupon: string) => coupon.split(',')
const withFields = (coupon: string, changes: Record<number, string>) => {
	const fields = fieldsOf(coupon)
	for (const [i, value] of Object.entries(changes)) fields[Number(i)] = value
	return fields.join(',')
}
/** `coupon` with context field `i` (0 to 8) set to `value`. */
const withContext = (coupon: string, i: number, value: string) =>
	withFields(coupon, { [i + 2]: value })

/** `coupon` lengthened to `chars` with leading zeros on its context fields, still signed. */
function padded(coupon: string, chars: number): string {
	const fields = fieldsOf(coupon)
	let extra = chars - coupon.length
	for (let i = 2; i < fields.length && extra > 0; i++) {
		const zeros = Math.min(extra, 78 - fields[i].length)
		fields[i] = '0'.repeat(zeros) + fields[i]
		extra -= zeros
	}
	return fields.join(',')
}

/** `coupon` signed instead by `key`, naming `signer` (default: the claim signer it names). */
async function resigned(coupon: string, key: Hex, signer = fieldsOf(coupon)[0]) {
	const context = fieldsOf(coupon).slice(2).map(BigInt)
	const hash = keccak256(encodePacked(Array<'uint256'>(9).fill('uint256'), context))
	const signature = await privateKeyToAccount(key).signMessage({ message: { raw: hash } })
	return withFields(coupon, { 0: signer, 1: signature })
}

type Chain = { chainId: bigint; timestamp: number; flags: Map<bigint, bigint> }
type RpcRequest = { method: string; params: [{ to: string; data: Hex }, string] }

/** A Sepolia JSON-RPC endpoint answering Multicall3 aggregate3 calls from `chain`. */
function fakeRpc(chain: Chain) {
	const requests: RpcRequest[] = []
	const storeReads: bigint[][] = []
	const fetch = vi.fn(async (_url: string, init: RequestInit) => {
		const request = JSON.parse(String(init.body))
		requests.push(request)
		const reads: bigint[] = []
		storeReads.push(reads)
		const [{ to, data }] = request.params
		if (request.method !== 'eth_call' || to.toLowerCase() !== MULTICALL3) {
			throw new Error(`unexpected ${request.method} to ${to}`)
		}
		const { args } = decodeFunctionData({ abi: multicall3Abi, data })
		const results = (args[0] as readonly { target: string; callData: Hex }[]).map(call => {
			const { functionName, args } = decodeFunctionData({ abi: readAbi, data: call.callData })
			const target = call.target.toLowerCase()
			let value: bigint
			if (target === STORE && functionName === 'get') {
				const [namespace, key] = args
				if (namespace !== CLAIM_NAMESPACE) throw new Error(`read namespace ${namespace}`)
				reads.push(key)
				value = chain.flags.get(key) ?? 0n
			} else if (target === MULTICALL3 && functionName === 'getChainId') {
				value = chain.chainId
			} else if (target === MULTICALL3 && functionName === 'getBlockNumber') {
				value = BLOCK
			} else if (target === MULTICALL3 && functionName === 'getCurrentBlockTimestamp') {
				value = BigInt(chain.timestamp)
			} else {
				throw new Error(`unexpected ${functionName} on ${target}`)
			}
			const returnData = encodeFunctionResult({ abi: readAbi, functionName, result: value })
			return { success: true, returnData }
		})
		const result = encodeFunctionResult({
			abi: multicall3Abi,
			functionName: 'aggregate3',
			result: results
		})
		return Response.json({ jsonrpc: '2.0', id: request.id, result })
	})
	vi.stubGlobal('fetch', fetch)
	return { fetch, requests, storeReads }
}

/** Sepolia with A and B redeemed, at a safe block timestamped `timestamp`. */
const sepolia = (timestamp = BEFORE_ALL): Chain => ({
	chainId: 11155111n,
	timestamp,
	flags: new Map([
		[KEY_A, 1n],
		[KEY_B, 1n]
	])
})
/** Sepolia with nothing redeemed. */
const unclaimed = (timestamp = BEFORE_ALL): Chain => ({ ...sepolia(timestamp), flags: new Map() })

/** A Workers cache held in memory, one per test, as one data centre's would be. */
function fakeCache() {
	const entries = new Map<string, string>()
	const store = {
		match: vi.fn(async (key: string) => {
			const body = entries.get(key)
			return body === undefined ? undefined : new Response(body)
		}),
		put: vi.fn(async (key: string, response: Response) => {
			entries.set(key, await response.text())
		})
	}
	return { entries, store, open: vi.fn(async () => store) }
}
let cache: ReturnType<typeof fakeCache>

async function post(body: unknown) {
	return send(
		new Request('http://localhost/api/coupon-status', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: typeof body === 'string' ? body : JSON.stringify(body)
		})
	)
}

async function send(request: Request) {
	const response = await POST({
		request,
		platform: { caches: { open: cache.open } }
	} as unknown as Parameters<typeof POST>[0])
	return { response, body: await response.json() }
}

/** `text` as a body that states no Content-Length, as a chunked upload does. */
function streamed(text: string): Request {
	const bytes = new TextEncoder().encode(text)
	const body = new ReadableStream({
		start(controller) {
			for (let i = 0; i < bytes.length; i += 4096) controller.enqueue(bytes.slice(i, i + 4096))
			controller.close()
		}
	})
	return new Request('http://localhost/api/coupon-status', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body,
		duplex: 'half'
	} as RequestInit)
}

/** A JSON body of one coupon, padded with trailing whitespace to `bytes`. */
function bodyOf(bytes: number): string {
	const json = JSON.stringify({ coupons: [A] })
	return json + ' '.repeat(bytes - json.length)
}

/** Sets the wall clock to `seconds` since the epoch. */
const setClock = (seconds: number) => vi.setSystemTime(seconds * 1000)

function expectCorsHeaders(response: Response) {
	expect(response.headers.get('Access-Control-Allow-Origin')).toBe('*')
	expect(response.headers.get('Access-Control-Allow-Methods')).toBe('POST, OPTIONS')
	expect(response.headers.get('Access-Control-Allow-Headers')).toBe('Content-Type')
	expect(response.headers.get('Access-Control-Max-Age')).toBe('86400')
	expect(response.headers.get('Access-Control-Allow-Credentials')).toBeNull()
	expect(response.headers.get('Cache-Control')).toBe('no-store')
}

const invalid = { status: 'invalid', nonce: null, expiry: null }

beforeEach(() => {
	env.SEPOLIA_RPC_URL = 'https://rpc.test/secret-key'
	cache = fakeCache()
	vi.useFakeTimers({ toFake: ['Date'] })
	setClock(BEFORE_ALL)
	vi.spyOn(console, 'error').mockImplementation(() => {})
})

afterEach(() => {
	vi.useRealTimers()
	vi.unstubAllGlobals()
	vi.restoreAllMocks()
})

describe('POST /api/coupon-status', () => {
	it('answers every coupon, in request order, from one read of the safe block', async () => {
		// A redeemed, B not; both past expiry at this block, C not.
		const chain = sepolia(EXPIRY_B + 1)
		chain.flags.delete(KEY_B)
		const rpc = fakeRpc(chain)

		const { response, body } = await post({ coupons: [C, A, 'not a coupon', B] })

		expect(response.status).toBe(200)
		expectCorsHeaders(response)
		expect(body).toEqual({
			chainId: 11155111,
			block: '10883600',
			results: [
				{ status: 'unredeemed', nonce: '1775868122', expiry: EXPIRY_C },
				// Redeemed outranks the expiry it has also passed.
				{ status: 'redeemed', nonce: '1775867941', expiry: EXPIRY_A },
				invalid,
				{ status: 'expired', nonce: '1775867943', expiry: EXPIRY_B }
			]
		})
		expect(rpc.requests).toHaveLength(1)
		expect(rpc.requests[0].method).toBe('eth_call')
		expect(rpc.requests[0].params[1]).toBe('safe')
		expect(rpc.storeReads[0]).toEqual(expect.arrayContaining([KEY_A, KEY_B]))
	})

	it('counts a coupon expiring at the block timestamp as expired, and one expiring later as not', async () => {
		fakeRpc(unclaimed(EXPIRY_A))

		const { body } = await post({ coupons: [A, B] })

		expect(body.results.map((r: { status: string }) => r.status)).toEqual(['expired', 'unredeemed'])
	})

	it('reads 20 Sepolia coupons, the most a request takes, in one eth_call', async () => {
		const rpc = fakeRpc(unclaimed())
		const coupons = SEPOLIA_REDEEMED.map(({ coupon }) => coupon)
		expect(coupons).toHaveLength(20)

		const { response, body } = await post({ coupons })

		expect(response.status).toBe(200)
		expect(body.results).toHaveLength(20)
		expect(body.results.every((r: { status: string }) => r.status === 'unredeemed')).toBe(true)
		expect(rpc.requests).toHaveLength(1)
		expect(new Set(rpc.storeReads[0]).size).toBe(20)
	})

	it('reads a nonce once for coupons that share it', async () => {
		const rpc = fakeRpc(sepolia())

		const { body } = await post({ coupons: [A, padded(A, A.length + 1)] })

		expect(body.results.map((r: { status: string }) => r.status)).toEqual(['redeemed', 'redeemed'])
		expect(rpc.storeReads).toEqual([[KEY_A]])
	})

	it('reads a coupon as long as one can be, every context field 78 digits', async () => {
		const rpc = fakeRpc(sepolia())
		const longest = padded(A, 886)
		expect(
			fieldsOf(longest)
				.slice(2)
				.every(field => field.length === 78)
		).toBe(true)

		const { body } = await post({ coupons: [longest] })

		expect(body.results).toEqual([{ status: 'redeemed', nonce: '1775867941', expiry: EXPIRY_A }])
		expect(rpc.storeReads).toEqual([[KEY_A]])
	})

	it('makes no RPC call, and answers block null, when no coupon is valid', async () => {
		const rpc = fakeRpc(sepolia())

		const { response, body } = await post({ coupons: ['not a coupon', withContext(A, 3, '1')] })

		expect(response.status).toBe(200)
		expect(body).toEqual({ chainId: 11155111, block: null, results: [invalid, invalid] })
		expect(rpc.fetch).not.toHaveBeenCalled()
	})

	const signature = fieldsOf(A)[1]
	const r = signature.slice(2, 66)
	const s = BigInt(`0x${signature.slice(66, 130)}`)
	const highS = (SECP256K1_N - s).toString(16).padStart(64, '0')
	const flippedV = signature.endsWith('1c') ? '1b' : '1c'
	it.each([
		['a forged signature', withFields(A, { 1: `0x${'11'.repeat(32)}${signature.slice(66)}` })],
		[
			'a signature changed in one byte',
			withFields(A, { 1: `0x${r.slice(0, 62)}00${signature.slice(66)}` })
		],
		[
			'the high-s twin of the real signature, which the orderbook refuses',
			withFields(A, { 1: `0x${r}${highS}${flippedV}` })
		],
		['the real signature with v as 0 or 1', withFields(A, { 1: `${signature.slice(0, -2)}01` })],
		[
			'another signer named in the coupon',
			withFields(A, { 0: '0x1111111111111111111111111111111111111111' })
		],
		['a signature that is not hex', withFields(A, { 1: 'signature' })],
		['an empty signature', withFields(A, { 1: '0x' })],
		['a signature of the wrong length', withFields(A, { 1: signature.slice(0, -2) })],
		['a signer that is not an address', withFields(A, { 0: '0x1234' })]
	])('answers invalid for %s, and makes no RPC call', async (_, forged) => {
		const rpc = fakeRpc(sepolia())

		const { response, body } = await post({ coupons: [forged] })

		expect(response.status).toBe(200)
		expect(body).toEqual({ chainId: 11155111, block: null, results: [invalid] })
		expect(rpc.fetch).not.toHaveBeenCalled()
	})

	it('answers invalid for a coupon signed by another key, whichever signer it names', async () => {
		const rpc = fakeRpc(sepolia())
		const key = generatePrivateKey()
		const other = privateKeyToAccount(key).address

		const { body } = await post({
			coupons: [await resigned(A, key), await resigned(A, key, other)]
		})

		expect(body.results).toEqual([invalid, invalid])
		expect(rpc.fetch).not.toHaveBeenCalled()
		// The coupon they were made from still reads, so the claim signer is the one checked.
		expect((await post({ coupons: [A] })).body.results[0].status).toBe('redeemed')
	})

	it.each([
		['too few fields', fieldsOf(A).slice(0, 10).join(',')],
		['too many fields', `${A},1`],
		['a hex context field', withContext(A, 8, '0x10')],
		['a negative context field', withContext(A, 8, '-1')],
		['an empty context field', withContext(A, 8, '')],
		['a context field above uint256', withContext(A, 8, (maxUint256 + 1n).toString())],
		['a context field of 79 digits', withContext(A, 8, '1775867941'.padStart(79, '0'))],
		['a coupon longer than any genuine one', `0${padded(A, 886)}`],
		['a coupon with a 60 KB context field', withContext(A, 1, '1'.repeat(60 * 1024))],
		['an expiry past what a JSON number holds exactly', withContext(A, 2, '9007199254740992')],
		['another order hash', withContext(A, 3, '1')],
		['another order owner', withContext(A, 4, '1')],
		['another orderbook', withContext(A, 5, '1')]
	])('answers invalid for %s, and makes no RPC call', async (_, malformed) => {
		const rpc = fakeRpc(sepolia())

		const { response, body } = await post({ coupons: [malformed] })

		expect(response.status).toBe(200)
		expect(body.results).toEqual([invalid])
		expect(rpc.fetch).not.toHaveBeenCalled()
	})

	it.each([
		['a body that is not JSON', 'coupons'],
		['a body that is not an object', '["x"]'],
		['a body without coupons', {}],
		['a body with another field', { coupons: [A], extra: true }],
		['coupons that are not an array', { coupons: A }],
		['no coupons', { coupons: [] }],
		['21 coupons', { coupons: Array(21).fill(A) }],
		['a coupon that is not a string', { coupons: [A, 1] }]
	])('answers 400 for %s', async (_, body) => {
		const rpc = fakeRpc(sepolia())

		const { response, body: answer } = await post(body)

		expect(response.status).toBe(400)
		expectCorsHeaders(response)
		expect(answer).toEqual({ error: expect.any(String) })
		expect(rpc.fetch).not.toHaveBeenCalled()
	})

	it('answers 400 to a body over 64 KB by its Content-Length, unread', async () => {
		const rpc = fakeRpc(sepolia())
		const request = new Request('http://localhost/api/coupon-status', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json', 'Content-Length': '65537' },
			body: bodyOf(64 * 1024)
		})

		const { response, body } = await send(request)

		expect(response.status).toBe(400)
		expectCorsHeaders(response)
		expect(body).toEqual({ error: expect.any(String) })
		expect(request.bodyUsed).toBe(false)
		expect(rpc.fetch).not.toHaveBeenCalled()
	})

	it('answers 400 to a body that states no length once it passes 64 KB', async () => {
		const rpc = fakeRpc(sepolia())

		const { response, body } = await send(streamed(bodyOf(64 * 1024 + 1)))

		expect(response.status).toBe(400)
		expect(body).toEqual({ error: expect.any(String) })
		expect(rpc.fetch).not.toHaveBeenCalled()
	})

	it('reads a body of exactly 64 KB, with or without a stated length', async () => {
		fakeRpc(sepolia())

		const stated = await post(bodyOf(64 * 1024))
		const unstated = await send(streamed(bodyOf(64 * 1024)))

		expect(stated.response.status).toBe(200)
		expect(unstated.response.status).toBe(200)
		expect(unstated.body.results).toEqual([
			{ status: 'redeemed', nonce: '1775867941', expiry: EXPIRY_A }
		])
	})

	it('asks a rate-limited RPC once and answers 502', async () => {
		const fetch = vi.fn(
			async () => new Response('rate limited', { status: 429, headers: { 'Retry-After': '1' } })
		)
		vi.stubGlobal('fetch', fetch)

		const { response } = await post({ coupons: [A] })

		expect(response.status).toBe(502)
		expect(fetch).toHaveBeenCalledTimes(1)
	})

	it('answers 502 when the RPC has not answered in 5 s', async () => {
		vi.useRealTimers()
		vi.useFakeTimers()
		const fetch = vi.fn(
			(_url: string, init: RequestInit) =>
				new Promise<Response>((_, reject) =>
					init.signal?.addEventListener('abort', () =>
						reject(new DOMException('aborted', 'AbortError'))
					)
				)
		)
		vi.stubGlobal('fetch', fetch)
		let answered = false
		const answer = post({ coupons: [A] }).finally(() => (answered = true))

		await vi.advanceTimersByTimeAsync(4_999)
		expect(answered).toBe(false)
		await vi.advanceTimersByTimeAsync(1)
		const { response } = await answer

		expect(response.status).toBe(502)
		expect(fetch).toHaveBeenCalledTimes(1)
	})

	it('answers 502 without the RPC URL when the RPC cannot be reached', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => {
				throw new TypeError('fetch failed')
			})
		)

		const { response, body } = await post({ coupons: [A] })

		expect(response.status).toBe(502)
		expectCorsHeaders(response)
		expect(body).toEqual({ error: expect.any(String) })
		expect(JSON.stringify(body)).not.toContain('secret-key')
	})

	it('answers 502 when the RPC returns an error', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async (_url: string, init: RequestInit) =>
				Response.json({
					jsonrpc: '2.0',
					id: JSON.parse(String(init.body)).id,
					error: { code: -32602, message: 'invalid block tag' }
				})
			)
		)

		const { response, body } = await post({ coupons: [A] })

		expect(response.status).toBe(502)
		expect(body).toEqual({ error: expect.any(String) })
	})

	it('answers 502 when the RPC is on another chain', async () => {
		fakeRpc({ ...sepolia(), chainId: 1n })

		const { response } = await post({ coupons: [A] })

		expect(response.status).toBe(502)
	})

	it('answers 502 when a coupon needs a read and no RPC is configured', async () => {
		const rpc = fakeRpc(sepolia())
		env.SEPOLIA_RPC_URL = undefined

		const { response, body } = await post({ coupons: [A] })

		expect(response.status).toBe(502)
		expect(body).toEqual({ error: expect.any(String) })
		expect(rpc.fetch).not.toHaveBeenCalled()
	})
})

describe('POST /api/coupon-status cache', () => {
	it('answers a redeemed coupon from the cache from then on, with no RPC call', async () => {
		const rpc = fakeRpc(sepolia())
		await post({ coupons: [A] })

		setClock(BEFORE_ALL + 30 * 24 * 60 * 60)
		const { body } = await post({ coupons: [A] })

		expect(body).toEqual({
			chainId: 11155111,
			block: null,
			results: [{ status: 'redeemed', nonce: '1775867941', expiry: EXPIRY_A }]
		})
		expect(rpc.requests).toHaveLength(1)
	})

	it('answers an expired coupon from the cache from then on, with no RPC call', async () => {
		const rpc = fakeRpc(unclaimed(EXPIRY_B))
		setClock(EXPIRY_B + 600)
		expect((await post({ coupons: [B] })).body.results[0].status).toBe('expired')

		setClock(EXPIRY_B + 24 * 60 * 60)
		const { body } = await post({ coupons: [B] })

		expect(body.results[0].status).toBe('expired')
		expect(rpc.requests).toHaveLength(1)
	})

	it('reads an unredeemed coupon again once its answer is 60 s old', async () => {
		const rpc = fakeRpc(unclaimed())
		await post({ coupons: [C] })

		setClock(BEFORE_ALL + 59.999)
		expect((await post({ coupons: [C] })).body.block).toBeNull()
		expect(rpc.requests).toHaveLength(1)

		setClock(BEFORE_ALL + 60)
		const { body } = await post({ coupons: [C] })
		expect(body.block).toBe('10883600')
		expect(body.results[0].status).toBe('unredeemed')
		expect(rpc.requests).toHaveLength(2)
	})

	it('reads an unredeemed coupon again once its expiry passes, within 60 s', async () => {
		// The safe block lags the wall clock, so it can still find a coupon unexpired.
		const rpc = fakeRpc(unclaimed(EXPIRY_A - 100))
		setClock(EXPIRY_A - 10)
		expect((await post({ coupons: [A] })).body.results[0].status).toBe('unredeemed')

		setClock(EXPIRY_A - 1)
		await post({ coupons: [A] })
		expect(rpc.requests).toHaveLength(1)

		setClock(EXPIRY_A)
		await post({ coupons: [A] })
		expect(rpc.requests).toHaveLength(2)
	})

	it('reads, in one call, only the coupons the cache cannot answer', async () => {
		const rpc = fakeRpc(sepolia())
		await post({ coupons: [A] })
		rpc.storeReads.length = 0

		const { body } = await post({ coupons: [A, 'not a coupon', C, A] })

		expect(body.block).toBe('10883600')
		expect(body.results.map((r: { status: string }) => r.status)).toEqual([
			'redeemed',
			'invalid',
			'unredeemed',
			'redeemed'
		])
		expect(rpc.requests).toHaveLength(2)
		expect(rpc.storeReads).toHaveLength(1)
		expect(rpc.storeReads[0]).toHaveLength(1)
		expect(rpc.storeReads[0]).not.toContain(KEY_A)
	})

	it('stores nothing for an invalid coupon, and nothing when the read fails', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => {
				throw new TypeError('fetch failed')
			})
		)

		await post({ coupons: [withFields(A, { 1: `0x${'11'.repeat(65)}` }), A] })

		expect(cache.store.put).not.toHaveBeenCalled()
	})
})

describe('OPTIONS /api/coupon-status', () => {
	it('answers the CORS preflight', async () => {
		const request = new Request('http://localhost/api/coupon-status', {
			method: 'OPTIONS',
			headers: {
				Origin: 'tauri://localhost',
				'Access-Control-Request-Method': 'POST',
				'Access-Control-Request-Headers': 'content-type'
			}
		})

		const response = await OPTIONS({ request } as Parameters<typeof OPTIONS>[0])

		expect(response.status).toBe(204)
		expectCorsHeaders(response)
	})
})
