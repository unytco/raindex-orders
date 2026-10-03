import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
	decodeFunctionData,
	encodeFunctionResult,
	maxUint256,
	multicall3Abi,
	parseAbi,
	type Hex
} from 'viem'

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

// Read on Sepolia: the claim order's store namespace, and two coupons redeemed
// through the orderbook (TakeOrder in 0x79342745… and 0xfe0798bd…) with the
// store keys their nonces were marked at.
const CLAIM_NAMESPACE = 0x2eff224ba29f37818f323107f4b089a0f598bb8cc44e3bf2738bbe591b2df7d1n
const REDEEMED_A =
	'0x8E72b7568738da52ca3DCd9b24E178127A4E7d37,0xff97ff0421320bfee7652a89b9d66e9eb1628a2d6da30db5788d20f49e26508252195757d74c584b9890b035fd84d87a8d1f7ac767da5e29f6299aaa32d4a98e1c,152057499784032982910041728223041816663873068381,200000000000000000000,1776472741,42941365433660945573526868378572896801276519612010928011051331832534249141753,1300945060633283894583661816534861012306758682838,1442425860134574572653119565674449928420894127102,1340384803335777240622375245841800624658726815561,108043606565222972236900316128309391016550688326814185311821020602083120460619,1775867941'
const REDEEMED_B =
	'0x8E72b7568738da52ca3DCd9b24E178127A4E7d37,0x8e0659d48eedc1f1f49d96d6de27e23528db3fdbfcb840b58fb3fd1858a3748f1b08f2a2ea500022722c2089224632e6af607971ce42ac597554b240667fca281c,152057499784032982910041728223041816663873068381,263000000000000000000,1776472743,42941365433660945573526868378572896801276519612010928011051331832534249141753,1300945060633283894583661816534861012306758682838,1442425860134574572653119565674449928420894127102,1340384803335777240622375245841800624658726815561,108043606565222972236900316128309391016550688326814185311821020602083120460619,1775867943'
const KEY_A = 0x5b791f671a06745c3b5c4c4bc3446ff7dfe601ba6e695a0d0b1f4af55fe50a3an
const KEY_B = 0x42fd2eddc5420575975b7b001ea50a43a796c99cb93410a97ab467bf37e6d433n

const BLOCK = 10_883_600n
const NOW = 1_776_500_000n

/** REDEEMED_A with some fields replaced: 2 expiry, 3 order hash, 4 owner, 5 orderbook, 8 nonce. */
function coupon(fields: Record<number, string>): string {
	const [signer, signature, ...context] = REDEEMED_A.split(',')
	for (const [i, value] of Object.entries(fields)) context[Number(i)] = value
	return [signer, signature, ...context].join(',')
}

const readAbi = parseAbi([
	'function get(uint256 namespace, uint256 key) view returns (uint256)',
	'function getChainId() view returns (uint256)',
	'function getBlockNumber() view returns (uint256)',
	'function getCurrentBlockTimestamp() view returns (uint256)'
])

type Chain = { chainId: bigint; flags: Map<bigint, bigint> }
type RpcRequest = { method: string; params: [{ to: string; data: Hex }, string] }

/** A Sepolia JSON-RPC endpoint answering Multicall3 aggregate3 calls from `chain`. */
function fakeRpc(chain: Chain) {
	const requests: RpcRequest[] = []
	const storeReads: bigint[] = []
	const fetch = vi.fn(async (_url: string, init: RequestInit) => {
		const request = JSON.parse(String(init.body))
		requests.push(request)
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
				storeReads.push(key)
				value = chain.flags.get(key) ?? 0n
			} else if (target === MULTICALL3 && functionName === 'getChainId') {
				value = chain.chainId
			} else if (target === MULTICALL3 && functionName === 'getBlockNumber') {
				value = BLOCK
			} else if (target === MULTICALL3 && functionName === 'getCurrentBlockTimestamp') {
				value = NOW
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

const redeemedChain = (): Chain => ({
	chainId: 11155111n,
	flags: new Map([
		[KEY_A, 1n],
		[KEY_B, 1n]
	])
})

async function post(body: unknown) {
	const request = new Request('http://localhost/api/coupon-status', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: typeof body === 'string' ? body : JSON.stringify(body)
	})
	const response = await POST({ request } as Parameters<typeof POST>[0])
	return { response, body: await response.json() }
}

function expectCorsHeaders(response: Response) {
	expect(response.headers.get('Access-Control-Allow-Origin')).toBe('*')
	expect(response.headers.get('Access-Control-Allow-Methods')).toBe('POST, OPTIONS')
	expect(response.headers.get('Access-Control-Allow-Headers')).toBe('Content-Type')
	expect(response.headers.get('Access-Control-Allow-Credentials')).toBeNull()
	expect(response.headers.get('Cache-Control')).toBe('no-store')
}

beforeEach(() => {
	env.SEPOLIA_RPC_URL = 'https://rpc.test/secret-key'
	vi.spyOn(console, 'error').mockImplementation(() => {})
})

afterEach(() => {
	vi.unstubAllGlobals()
	vi.restoreAllMocks()
})

describe('POST /api/coupon-status', () => {
	it('answers every coupon, in request order, from one read of the safe block', async () => {
		const rpc = fakeRpc(redeemedChain())
		const unredeemed = coupon({ 2: '1776600000', 8: '123456789' })
		const expired = coupon({ 2: '1776400000', 8: '42' })

		const { response, body } = await post({
			coupons: [unredeemed, REDEEMED_A, 'not a coupon', expired, REDEEMED_B]
		})

		expect(response.status).toBe(200)
		expectCorsHeaders(response)
		expect(body).toEqual({
			chainId: 11155111,
			block: '10883600',
			results: [
				{ status: 'unredeemed', nonce: '123456789', expiry: 1776600000 },
				// Redeemed outranks the expiry it has also passed.
				{ status: 'redeemed', nonce: '1775867941', expiry: 1776472741 },
				{ status: 'invalid', nonce: null, expiry: null },
				{ status: 'expired', nonce: '42', expiry: 1776400000 },
				{ status: 'redeemed', nonce: '1775867943', expiry: 1776472743 }
			]
		})
		expect(rpc.requests).toHaveLength(1)
		expect(rpc.requests[0].method).toBe('eth_call')
		expect(rpc.requests[0].params[1]).toBe('safe')
		expect(rpc.storeReads).toContain(KEY_A)
		expect(rpc.storeReads).toContain(KEY_B)
	})

	it('counts a coupon expiring at the block timestamp as expired, and one second later as not', async () => {
		fakeRpc(redeemedChain())

		const { body } = await post({
			coupons: [coupon({ 2: String(NOW), 8: '7' }), coupon({ 2: String(NOW + 1n), 8: '8' })]
		})

		expect(body.results.map((r: { status: string }) => r.status)).toEqual(['expired', 'unredeemed'])
	})

	it('reads a full batch of 50 in one eth_call', async () => {
		const rpc = fakeRpc(redeemedChain())
		const coupons = Array.from({ length: 50 }, (_, i) => coupon({ 2: '1776600000', 8: String(i) }))

		const { response, body } = await post({ coupons })

		expect(response.status).toBe(200)
		expect(body.results).toHaveLength(50)
		expect(rpc.requests).toHaveLength(1)
		expect(rpc.storeReads).toHaveLength(50)
	})

	const claim = REDEEMED_A.split(',')
	it.each([
		['too few fields', claim.slice(0, 10).join(',')],
		['too many fields', `${REDEEMED_A},1`],
		['a signer that is not an address', ['0x1234', ...claim.slice(1)].join(',')],
		['a signature that is not hex', [claim[0], 'signature', ...claim.slice(2)].join(',')],
		['an empty signature', [claim[0], '0x', ...claim.slice(2)].join(',')],
		['a hex context field', coupon({ 8: '0x10' })],
		['a negative context field', coupon({ 8: '-1' })],
		['an empty context field', coupon({ 8: '' })],
		['a context field above uint256', coupon({ 8: (maxUint256 + 1n).toString() })],
		['an expiry past what a JSON number holds exactly', coupon({ 2: '9007199254740992' })],
		['another order hash', coupon({ 3: '1' })],
		['another order owner', coupon({ 4: '1' })],
		['another orderbook', coupon({ 5: '1' })]
	])('answers invalid for %s, and reads nothing for it', async (_, invalid) => {
		const rpc = fakeRpc(redeemedChain())

		const { response, body } = await post({ coupons: [invalid] })

		expect(response.status).toBe(200)
		expect(body.results).toEqual([{ status: 'invalid', nonce: null, expiry: null }])
		expect(rpc.storeReads).toEqual([])
	})

	it.each([
		['a body that is not JSON', 'coupons'],
		['a body that is not an object', '["x"]'],
		['a body without coupons', {}],
		['a body with another field', { coupons: [REDEEMED_A], extra: true }],
		['coupons that are not an array', { coupons: REDEEMED_A }],
		['no coupons', { coupons: [] }],
		['51 coupons', { coupons: Array(51).fill(REDEEMED_A) }],
		['a coupon that is not a string', { coupons: [REDEEMED_A, 1] }]
	])('answers 400 for %s', async (_, body) => {
		const rpc = fakeRpc(redeemedChain())

		const { response, body: answer } = await post(body)

		expect(response.status).toBe(400)
		expectCorsHeaders(response)
		expect(answer).toEqual({ error: expect.any(String) })
		expect(rpc.fetch).not.toHaveBeenCalled()
	})

	it('answers 502 without the RPC URL when the RPC cannot be reached', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => {
				throw new TypeError('fetch failed')
			})
		)

		const { response, body } = await post({ coupons: [REDEEMED_A] })

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

		const { response, body } = await post({ coupons: [REDEEMED_A] })

		expect(response.status).toBe(502)
		expect(body).toEqual({ error: expect.any(String) })
	})

	it('answers 502 when the RPC is on another chain', async () => {
		fakeRpc({ ...redeemedChain(), chainId: 1n })

		const { response } = await post({ coupons: [REDEEMED_A] })

		expect(response.status).toBe(502)
	})

	it('answers 502 when no RPC is configured', async () => {
		const rpc = fakeRpc(redeemedChain())
		env.SEPOLIA_RPC_URL = undefined

		const { response, body } = await post({ coupons: [REDEEMED_A] })

		expect(response.status).toBe(502)
		expect(body).toEqual({ error: expect.any(String) })
		expect(rpc.fetch).not.toHaveBeenCalled()
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
