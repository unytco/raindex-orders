import { json } from '@sveltejs/kit'
import type { RequestHandler } from './$types'
import {
	createPublicClient,
	encodeAbiParameters,
	encodePacked,
	hashMessage,
	hexToBigInt,
	http,
	isAddress,
	isAddressEqual,
	isHex,
	keccak256,
	maxUint256,
	parseAbi,
	recoverMessageAddress,
	stringToHex,
	type ContractFunctionParameters,
	type Hex
} from 'viem'
import { env } from '$env/dynamic/private'
import { bridge } from '$lib/config'
import { CLAIM_ORDER } from '$lib/orderConfig'
import {
	CouponStatusCache,
	settledStatus,
	type CouponCache,
	type ReadStatus
} from '$lib/server/couponStatusCache'
import { logRpcError } from '$lib/server/rpcError'

// hot-bridge-ui runs on the Workers Free plan: 50 subrequests per request, Cache API
// calls included, and 10 ms of CPU. N uncached coupons cost N cache matches, one RPC
// fetch and N cache puts, so 20 costs at most 41 subrequests and 20 signature checks.
const MAX_COUPONS = 20
const KEY_SIGNATURE_BYTES = 65
// A contract signer's signature is checked on chain by its EIP-1271 isValidSignature.
// A Safe's signature holds 65 bytes per owner, and the claim signer's Safe needs two or more.
const MIN_CONTRACT_SIGNATURE_BYTES = 2 * KEY_SIGNATURE_BYTES
const MAX_CONTRACT_SIGNATURE_BYTES = 20 * KEY_SIGNATURE_BYTES
const EIP1271_MAGIC_VALUE = '0x1626ba7e00000000000000000000000000000000000000000000000000000000'
const MAX_BODY_BYTES = 64 * 1024
const UINT256_DIGITS = maxUint256.toString().length
// The highest `s` OpenZeppelin's ECDSA.tryRecover accepts, which the orderbook uses to
// check a coupon's signature. viem would also accept the high-`s` twin.
const MAX_SIGNATURE_S = 0x7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0n

const HEADERS = {
	'Access-Control-Allow-Origin': '*',
	'Access-Control-Allow-Methods': 'POST, OPTIONS',
	'Access-Control-Allow-Headers': 'Content-Type',
	'Access-Control-Max-Age': '86400',
	'Cache-Control': 'no-store'
}

const orderbook = bridge.orderbookAddress
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

// The interpreter store's `get`, a contract signer's EIP-1271 check, and Multicall3's
// reads of the chain and block it runs in.
const readAbi = parseAbi([
	'function get(uint256 namespace, uint256 key) view returns (uint256)',
	'function isValidSignature(bytes32 hash, bytes signature) view returns (bytes32)',
	'function getChainId() view returns (uint256)',
	'function getBlockNumber() view returns (uint256)',
	'function getCurrentBlockTimestamp() view returns (uint256)'
])
const multicall3 = bridge.chain.contracts.multicall3.address

type Coupon = {
	id: Hex
	signature: Hex
	hash: Hex
	nonce: bigint
	expiry: bigint
	signedBy: 'key' | 'contract'
}

type Result = {
	status: ReadStatus | 'invalid'
	nonce: string | null
	expiry: number | null
}

type ChainRead = {
	block: bigint
	timestamp: bigint
	flags: Map<bigint, bigint>
	contractSigned: Set<Coupon>
}

const reply = (body: unknown, status: number) => json(body, { status, headers: HEADERS })

export const OPTIONS: RequestHandler = () => new Response(null, { status: 204, headers: HEADERS })

export const POST: RequestHandler = async ({ request, platform }) => {
	const texts = await readCoupons(request)
	if ('error' in texts) return reply(texts, 400)
	if (!platform) throw new Error('coupon-status needs the Cloudflare Workers platform')
	// The adapter types `caches` with @cloudflare/workers-types, whose Response is not
	// the DOM Response this app builds; at runtime both are the Workers Response.
	const store = (await platform.caches.open('coupon-status')) as unknown as CouponCache
	const cache = new CouponStatusCache(store, new URL(request.url).origin)

	// A coupon with no status at the end is invalid.
	const coupons = texts.map(parseCoupon)
	const statuses = new Map<Coupon, ReadStatus>()
	const unread: Coupon[] = []
	const unverified: Coupon[] = []
	const now = Date.now()
	await Promise.all(
		coupons.map(async coupon => {
			if (!coupon) return
			const cached = await cache.get(coupon.id)
			const status = cached && settledStatus(cached, coupon.expiry, now)
			if (status) statuses.set(coupon, status)
			// An entry exists only for a coupon whose signature was verified.
			else if (cached) unread.push(coupon)
			else if (coupon.signedBy === 'contract') unverified.push(coupon)
			else if (await signedByClaimSigner(coupon)) unread.push(coupon)
		})
	)

	let chain: ChainRead | undefined
	if (unread.length + unverified.length > 0) {
		const rpcUrl = env[bridge.rpcSecret]
		if (!rpcUrl) {
			console.error(`coupon-status: ${bridge.rpcSecret} is not set`)
			return reply({ error: 'RPC is not configured' }, 502)
		}
		try {
			chain = await readChain(rpcUrl, unread, unverified)
		} catch (e) {
			logRpcError('coupon-status: RPC read failed', e)
			return reply({ error: 'RPC read failed' }, 502)
		}
		const readAt = Date.now()
		const { flags, timestamp, contractSigned } = chain
		const verified = [...unread, ...unverified.filter(coupon => contractSigned.has(coupon))]
		await Promise.all(
			verified.map(coupon => {
				const status =
					flags.get(coupon.nonce) !== 0n
						? 'redeemed'
						: coupon.expiry <= timestamp
							? 'expired'
							: 'unredeemed'
				statuses.set(coupon, status)
				return cache.put(coupon.id, { status, readAt })
			})
		)
	}

	const results = coupons.map((coupon): Result => {
		const status = coupon && statuses.get(coupon)
		if (!status) return { status: 'invalid', nonce: null, expiry: null }
		return { status, nonce: coupon.nonce.toString(), expiry: Number(coupon.expiry) }
	})
	return reply({ chainId: bridge.chain.id, block: chain?.block.toString() ?? null, results }, 200)
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
 * unless it parses, names the claim order and its signer, and carries a signature the
 * orderbook would accept the form of. A key's signature is checked by
 * signedByClaimSigner, a contract signer's on chain by readChain.
 */
function parseCoupon(text: string): Coupon | null {
	const [signer, signature, ...fields] = text.split(',')
	if (fields.length !== 9) return null
	if (!isAddress(signer, { strict: false }) || !isAddressEqual(signer, CLAIM_ORDER.signer)) {
		return null
	}
	const signedBy = signatureForm(signature)
	if (!signedBy) return null
	if (!fields.every(field => field.length <= UINT256_DIGITS && /^\d+$/.test(field))) return null

	const context = fields.map(BigInt)
	if (context.some(value => value > maxUint256)) return null
	const [, , expiry, couponOrderHash, owner, couponOrderbook, , , nonce] = context
	if (couponOrderHash !== orderHash) return null
	if (owner !== BigInt(CLAIM_ORDER.owner)) return null
	if (couponOrderbook !== BigInt(orderbook)) return null
	// The response states expiry as a JSON number of seconds.
	if (expiry > BigInt(Number.MAX_SAFE_INTEGER)) return null

	const hash = keccak256(encodePacked(Array<'uint256'>(9).fill('uint256'), context))
	return {
		id: couponId(signer, signature as Hex, context),
		signature: signature as Hex,
		hash,
		nonce,
		expiry,
		signedBy
	}
}

/**
 * Who could have made `signature`: a key, as 65 bytes r, s, v with v 27 or 28 and a
 * low s, as ECDSA.tryRecover requires; or a contract signer, as anything of a length
 * a Safe of 2 to 20 owners signs with.
 */
function signatureForm(signature: string): Coupon['signedBy'] | null {
	if (!isHex(signature) || signature.length % 2 !== 0) return null
	const bytes = (signature.length - 2) / 2
	if (bytes === KEY_SIGNATURE_BYTES) {
		if (!['1b', '1c'].includes(signature.slice(130).toLowerCase())) return null
		if (hexToBigInt(`0x${signature.slice(66, 130)}`) > MAX_SIGNATURE_S) return null
		return 'key'
	}
	if (bytes >= MIN_CONTRACT_SIGNATURE_BYTES && bytes <= MAX_CONTRACT_SIGNATURE_BYTES) {
		return 'contract'
	}
	return null
}

/**
 * One coupon's identity, whatever its spelling: hex case and leading zeros change the
 * text but not what the orderbook checks. Its context includes the expiry, so a coupon
 * re-issued for the same withdrawal has an identity of its own.
 */
function couponId(signer: string, signature: Hex, context: bigint[]): Hex {
	const canonical = [signer.toLowerCase(), signature.toLowerCase(), ...context.map(String)]
	return keccak256(stringToHex(canonical.join(',')))
}

/**
 * Whether the coupon's signature recovers to the claim order's signer, over the
 * EIP-191 message the orderbook checks: keccak256 of the nine context words.
 */
async function signedByClaimSigner({ hash, signature }: Coupon): Promise<boolean> {
	try {
		const recovered = await recoverMessageAddress({ message: { raw: hash }, signature })
		return isAddressEqual(recovered, CLAIM_ORDER.signer)
	} catch {
		return false
	}
}

/**
 * One aggregate3 eth_call at the `safe` block reads each nonce's flag once, along with
 * the chain, number and timestamp of the block it read them in. In the same call the
 * claim signer's EIP-1271 isValidSignature checks each `unverified` coupon, over the
 * digest the orderbook passes it, as OpenZeppelin's SignatureChecker reads the answer.
 */
async function readChain(
	rpcUrl: string,
	unread: Coupon[],
	unverified: Coupon[]
): Promise<ChainRead> {
	const nonces = [...new Set([...unread, ...unverified].map(coupon => coupon.nonce))]
	const contracts: ContractFunctionParameters<typeof readAbi, 'view'>[] = [
		{ address: multicall3, abi: readAbi, functionName: 'getChainId' },
		{ address: multicall3, abi: readAbi, functionName: 'getBlockNumber' },
		{ address: multicall3, abi: readAbi, functionName: 'getCurrentBlockTimestamp' },
		...nonces.map(nonce => ({
			address: CLAIM_ORDER.store,
			abi: readAbi,
			functionName: 'get' as const,
			args: [namespace, nonceKey(nonce)] as const
		})),
		...unverified.map(({ hash, signature }) => ({
			address: CLAIM_ORDER.signer,
			abi: readAbi,
			functionName: 'isValidSignature' as const,
			args: [hashMessage({ raw: hash }), signature] as const
		}))
	]
	// One attempt of at most 5 s. A retry would wait as long as the RPC's Retry-After
	// asks, and the caller polls anyway.
	const transport = http(rpcUrl, { retryCount: 0, timeout: 5_000 })
	const client = createPublicClient({ chain: bridge.chain, transport })
	const results = await client.multicall({
		contracts,
		// A contract signer reverts on a signature it refuses.
		allowFailure: true,
		blockTag: 'safe',
		// 0 keeps viem from splitting the batch across several eth_calls.
		batchSize: 0
	})
	const read = (i: number) => {
		const answer = results[i]
		if (answer.status === 'failure') throw answer.error
		return answer.result as bigint
	}
	const chainId = read(0)
	if (chainId !== BigInt(bridge.chain.id)) {
		throw new Error(`RPC answered for chain ${chainId}, expected ${bridge.chain.id}`)
	}
	const checks = results.slice(3 + nonces.length)
	return {
		block: read(1),
		timestamp: read(2),
		flags: new Map(nonces.map((nonce, i) => [nonce, read(3 + i)])),
		contractSigned: new Set(
			unverified.filter((_, i) => {
				const answer = checks[i]
				return answer.status === 'success' && String(answer.result) === EIP1271_MAGIC_VALUE
			})
		)
	}
}
