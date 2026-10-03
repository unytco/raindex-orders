import { json } from '@sveltejs/kit'
import type { RequestHandler } from './$types'
import {
	createPublicClient,
	encodeAbiParameters,
	encodePacked,
	http,
	isAddress,
	isHex,
	keccak256,
	maxUint256,
	parseAbi,
	type Address,
	type ContractFunctionParameters
} from 'viem'
import { sepolia } from 'viem/chains'
import { env } from '$env/dynamic/private'
import { PUBLIC_ORDERBOOK_ADDRESS } from '$env/static/public'
import { CLAIM_ORDER } from '$lib/orderConfig'
import { logRpcError } from '$lib/server/rpcError'

const MAX_COUPONS = 50
const MAX_BODY_BYTES = 64 * 1024
// A coupon from bridge-orchestrator/src/signer.rs is under 900 characters.
const MAX_COUPON_CHARS = 1024
const UINT256_DIGITS = maxUint256.toString().length

const HEADERS = {
	'Access-Control-Allow-Origin': '*',
	'Access-Control-Allow-Methods': 'POST, OPTIONS',
	'Access-Control-Allow-Headers': 'Content-Type',
	'Access-Control-Max-Age': '86400',
	'Cache-Control': 'no-store'
}

const orderbook = PUBLIC_ORDERBOOK_ADDRESS as Address
const orderHash = BigInt(CLAIM_ORDER.orderHash)

// OrderBook evaluates the claim order under the namespace
// LibNamespace.qualifyNamespace(order owner, orderbook) = keccak256(owner, orderbook),
// and src/holo-claim.rain marks a nonce used there at hash(order-hash() nonce).
const namespace = BigInt(
	keccak256(
		encodeAbiParameters([{ type: 'address' }, { type: 'address' }], [CLAIM_ORDER.owner, orderbook])
	)
)
const nonceKey = (nonce: bigint) =>
	BigInt(keccak256(encodePacked(['uint256', 'uint256'], [orderHash, nonce])))

// The interpreter store's `get`, and Multicall3's reads of the chain and block it runs in.
const readAbi = parseAbi([
	'function get(uint256 namespace, uint256 key) view returns (uint256)',
	'function getChainId() view returns (uint256)',
	'function getBlockNumber() view returns (uint256)',
	'function getCurrentBlockTimestamp() view returns (uint256)'
])
const multicall3 = sepolia.contracts.multicall3.address

type Coupon = { nonce: bigint; expiry: bigint }

type Result = {
	status: 'redeemed' | 'unredeemed' | 'expired' | 'invalid'
	nonce: string | null
	expiry: number | null
}

type ChainRead = { chainId: number; block: bigint; timestamp: bigint; flags: bigint[] }

const reply = (body: unknown, status: number) => json(body, { status, headers: HEADERS })

export const OPTIONS: RequestHandler = () => new Response(null, { status: 204, headers: HEADERS })

export const POST: RequestHandler = async ({ request }) => {
	const coupons = await readCoupons(request)
	if ('error' in coupons) return reply(coupons, 400)

	const parsed = coupons.map(parseCoupon)
	const valid = parsed.filter((coupon): coupon is Coupon => coupon !== null)

	const rpcUrl = env.SEPOLIA_RPC_URL
	if (!rpcUrl) {
		console.error('coupon-status: SEPOLIA_RPC_URL is not set')
		return reply({ error: 'Sepolia RPC is not configured' }, 502)
	}

	let chain: ChainRead
	try {
		chain = await readChain(rpcUrl, valid)
	} catch (e) {
		logRpcError('coupon-status: RPC read failed', e)
		return reply({ error: 'Sepolia RPC read failed' }, 502)
	}

	const flags = new Map(valid.map((coupon, i) => [coupon, chain.flags[i]]))
	const results = parsed.map((coupon): Result => {
		if (coupon === null) return { status: 'invalid', nonce: null, expiry: null }
		const status =
			flags.get(coupon) !== 0n
				? 'redeemed'
				: coupon.expiry <= chain.timestamp
					? 'expired'
					: 'unredeemed'
		return { status, nonce: coupon.nonce.toString(), expiry: Number(coupon.expiry) }
	})

	return reply({ chainId: chain.chainId, block: chain.block.toString(), results }, 200)
}

async function readCoupons(request: Request): Promise<string[] | { error: string }> {
	const text = await readText(request)
	if (text === null) return { error: `body must be at most ${MAX_BODY_BYTES} bytes` }
	let body: unknown
	try {
		body = JSON.parse(text)
	} catch {
		return { error: 'body must be JSON' }
	}
	if (
		typeof body !== 'object' ||
		body === null ||
		Array.isArray(body) ||
		Object.keys(body).length !== 1 ||
		!('coupons' in body)
	) {
		return { error: 'body must be {"coupons": [...]}' }
	}
	const { coupons } = body
	if (
		!Array.isArray(coupons) ||
		coupons.length < 1 ||
		coupons.length > MAX_COUPONS ||
		!coupons.every(coupon => typeof coupon === 'string')
	) {
		return { error: `coupons must be an array of 1 to ${MAX_COUPONS} strings` }
	}
	return coupons
}

/** The body as text, or null as soon as it is longer than MAX_BODY_BYTES. */
async function readText(request: Request): Promise<string | null> {
	if (Number(request.headers.get('Content-Length')) > MAX_BODY_BYTES) return null
	if (!request.body) return ''
	const reader = request.body.getReader()
	const decoder = new TextDecoder()
	let text = ''
	let size = 0
	for (let read = await reader.read(); !read.done; read = await reader.read()) {
		size += read.value.byteLength
		if (size > MAX_BODY_BYTES) {
			await reader.cancel()
			return null
		}
		text += decoder.decode(read.value, { stream: true })
	}
	return text + decoder.decode()
}

/**
 * A coupon is `signer,signature,c0..c8` (bridge-orchestrator/src/signer.rs). Null
 * unless it parses and names the claim order, so no other namespace is ever read.
 */
function parseCoupon(coupon: string): Coupon | null {
	if (coupon.length > MAX_COUPON_CHARS) return null
	const [signer, signature, ...fields] = coupon.split(',')
	if (fields.length !== 9) return null
	if (!isAddress(signer, { strict: false })) return null
	if (!isHex(signature) || signature.length === 2 || signature.length % 2 !== 0) return null
	if (!fields.every(field => field.length <= UINT256_DIGITS && /^\d+$/.test(field))) return null

	const context = fields.map(BigInt)
	if (context.some(value => value > maxUint256)) return null
	const [, , expiry, couponOrderHash, owner, couponOrderbook, , , nonce] = context
	if (couponOrderHash !== orderHash) return null
	if (owner !== BigInt(CLAIM_ORDER.owner)) return null
	if (couponOrderbook !== BigInt(orderbook)) return null
	// The response states expiry as a JSON number of seconds.
	if (expiry > BigInt(Number.MAX_SAFE_INTEGER)) return null

	return { nonce, expiry }
}

/**
 * One aggregate3 eth_call at the `safe` block reads every coupon's flag along with
 * the chain, number and timestamp of the block it read them in.
 */
async function readChain(rpcUrl: string, coupons: Coupon[]): Promise<ChainRead> {
	const contracts: ContractFunctionParameters<typeof readAbi, 'view'>[] = [
		{ address: multicall3, abi: readAbi, functionName: 'getChainId' },
		{ address: multicall3, abi: readAbi, functionName: 'getBlockNumber' },
		{ address: multicall3, abi: readAbi, functionName: 'getCurrentBlockTimestamp' },
		...coupons.map(({ nonce }) => ({
			address: CLAIM_ORDER.store,
			abi: readAbi,
			functionName: 'get' as const,
			args: [namespace, nonceKey(nonce)] as const
		}))
	]
	// One attempt of at most 5 s. A retry would wait as long as the RPC's Retry-After
	// asks, and the caller polls anyway.
	const transport = http(rpcUrl, { retryCount: 0, timeout: 5_000 })
	const client = createPublicClient({ chain: sepolia, transport })
	const [chainId, block, timestamp, ...flags] = await client.multicall({
		contracts,
		allowFailure: false,
		blockTag: 'safe',
		// 0 keeps viem from splitting the batch across several eth_calls.
		batchSize: 0
	})
	if (chainId !== BigInt(sepolia.id)) {
		throw new Error(`RPC answered for chain ${chainId}, expected ${sepolia.id}`)
	}
	return { chainId: sepolia.id, block, timestamp, flags }
}
