import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
	encodeAbiParameters,
	encodePacked,
	hashMessage,
	keccak256,
	type Address,
	type Hex
} from 'viem'
import { privateKeyToAccount } from 'viem/accounts'
import { asBuild, MAINNET_BUILD, SEPOLIA_BUILD } from '$lib/testing/builds'
import { SEPOLIA_REDEEMED } from './fixtures'
import { fakeCache, fakeRpc } from './fakeRpc'

const env = vi.hoisted(() => ({}) as Record<string, string | undefined>)
vi.mock('$env/dynamic/private', () => ({ env }))

const VAULT_ID = 0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334bn
// Anvil's third account: MAINNET_BUILD's claim signer.
const KEY_SIGNER = '0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a'
const OWNERS = [
	'0x47e179ec197488593b187f80a00eb0da91f1b9d0b13f8733639f19c30a34926a',
	'0x8b3a350cf5c34c9194ca85829a2df0ec3153be0318b5e2d3348e872092edffba'
] as const
const SAFE = '0x5afe5afe5afe5afe5afe5afe5afe5afe5afe5afe'
const NOW = 1_776_400_000
const SAFE_BUILD = { ...MAINNET_BUILD, PUBLIC_CLAIM_SIGNER: SAFE }

type Build = typeof MAINNET_BUILD

let cache: ReturnType<typeof fakeCache>

function context(build: Build, nonce: bigint): bigint[] {
	return [
		0x1111111111111111111111111111111111111111n,
		10n ** 18n,
		BigInt(NOW + 7 * 24 * 60 * 60),
		BigInt(build.PUBLIC_CLAIM_ORDER_HASH),
		BigInt(build.PUBLIC_LOCK_VAULT_ADDRESS),
		BigInt(build.PUBLIC_ORDERBOOK_ADDRESS),
		BigInt(build.PUBLIC_TOKEN_ADDRESS),
		VAULT_ID,
		nonce
	]
}

const contextHash = (words: bigint[]) =>
	keccak256(encodePacked(Array<'uint256'>(9).fill('uint256'), words))

const coupon = (signer: string, signature: Hex, words: bigint[]) =>
	[signer, signature, ...words.map(String)].join(',')

/** A coupon signed by `key`, as bridge-orchestrator signs one. */
async function keySigned(build: Build, key: Hex, nonce: bigint) {
	const words = context(build, nonce)
	const account = privateKeyToAccount(key)
	const signature = await account.signMessage({ message: { raw: contextHash(words) } })
	return coupon(account.address, signature, words)
}

/** A coupon naming the Safe, carrying `owners` 65-byte signatures. */
async function safeSigned(nonce: bigint, owners: readonly Hex[] = OWNERS) {
	const words = context(SAFE_BUILD, nonce)
	const signatures = await Promise.all(
		owners.map(key =>
			privateKeyToAccount(key).signMessage({ message: { raw: contextHash(words) } })
		)
	)
	return coupon(SAFE, `0x${signatures.map(s => s.slice(2)).join('')}`, words)
}

/** The chain `build` names, where the Safe accepts any signature two owners long. */
function chain(build: Build, chainId: bigint, flags = new Map<bigint, bigint>()) {
	const namespace = BigInt(
		keccak256(
			encodeAbiParameters(
				[{ type: 'address' }, { type: 'address' }],
				[build.PUBLIC_LOCK_VAULT_ADDRESS as Address, build.PUBLIC_ORDERBOOK_ADDRESS as Address]
			)
		)
	)
	return fakeRpc({
		chainId,
		block: 24_000_000n,
		timestamp: NOW,
		store: build.PUBLIC_CLAIM_STORE.toLowerCase(),
		namespace,
		flags,
		contractSigner: {
			address: SAFE,
			accepts: (_hash, signature) => signature.length === 2 + 2 * 130
		}
	})
}

const nonceKey = (build: Build, nonce: bigint) =>
	BigInt(
		keccak256(encodePacked(['uint256', 'uint256'], [BigInt(build.PUBLIC_CLAIM_ORDER_HASH), nonce]))
	)

type Post = (typeof import('./+server'))['POST']

const load = async (build: Build) => (await asBuild(build, () => import('./+server'))).POST

async function post(build: Build, coupons: string[]) {
	return postTo(await load(build), coupons)
}

async function postTo(POST: Post, coupons: string[]) {
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

const statuses = (body: { results: { status: string }[] }) => body.results.map(r => r.status)

beforeEach(() => {
	for (const key of Object.keys(env)) delete env[key]
	cache = fakeCache()
	vi.useFakeTimers({ toFake: ['Date'] })
	vi.setSystemTime(NOW * 1000)
	vi.spyOn(console, 'error').mockImplementation(() => {})
})

afterEach(() => {
	vi.useRealTimers()
	vi.unstubAllGlobals()
	vi.restoreAllMocks()
})

describe('a mainnet build', () => {
	it('reads Ethereum through ETH_RPC_URL and reports chain 1', async () => {
		env.ETH_RPC_URL = 'https://eth.rpc.test/key'
		const rpc = chain(MAINNET_BUILD, 1n, new Map([[nonceKey(MAINNET_BUILD, 1n), 1n]]))

		const { response, body } = await post(MAINNET_BUILD, [
			await keySigned(MAINNET_BUILD, KEY_SIGNER, 1n),
			await keySigned(MAINNET_BUILD, KEY_SIGNER, 2n)
		])

		expect(response.status).toBe(200)
		expect(body.chainId).toBe(1)
		expect(statuses(body)).toEqual(['redeemed', 'unredeemed'])
		expect(rpc.fetch).toHaveBeenCalledTimes(1)
		expect(rpc.fetch.mock.calls[0][0]).toBe('https://eth.rpc.test/key')
	})

	it('refuses the read, and caches nothing, when the RPC answers for Sepolia', async () => {
		env.ETH_RPC_URL = 'https://eth.rpc.test/key'
		chain(MAINNET_BUILD, 11155111n, new Map([[nonceKey(MAINNET_BUILD, 1n), 1n]]))

		const { response } = await post(MAINNET_BUILD, [await keySigned(MAINNET_BUILD, KEY_SIGNER, 1n)])

		expect(response.status).toBe(502)
		expect(cache.store.put).not.toHaveBeenCalled()
	})

	it('never reads through SEPOLIA_RPC_URL', async () => {
		env.SEPOLIA_RPC_URL = 'https://sepolia.rpc.test/key'
		const rpc = chain(MAINNET_BUILD, 1n)

		const { response } = await post(MAINNET_BUILD, [await keySigned(MAINNET_BUILD, KEY_SIGNER, 1n)])

		expect(response.status).toBe(502)
		expect(rpc.fetch).not.toHaveBeenCalled()
	})

	it('answers invalid for a Sepolia coupon, with no RPC call', async () => {
		env.ETH_RPC_URL = 'https://eth.rpc.test/key'
		const rpc = chain(MAINNET_BUILD, 1n)

		const { body } = await post(MAINNET_BUILD, [SEPOLIA_REDEEMED[0].coupon])

		expect(statuses(body)).toEqual(['invalid'])
		expect(rpc.fetch).not.toHaveBeenCalled()
	})
})

describe('a sepolia build', () => {
	it('never reads through ETH_RPC_URL', async () => {
		env.ETH_RPC_URL = 'https://eth.rpc.test/key'
		const rpc = chain(SEPOLIA_BUILD, 11155111n)

		const { response } = await post(SEPOLIA_BUILD, [SEPOLIA_REDEEMED[0].coupon])

		expect(response.status).toBe(502)
		expect(rpc.fetch).not.toHaveBeenCalled()
	})
})

describe('a Safe as the claim signer', () => {
	beforeEach(() => {
		env.ETH_RPC_URL = 'https://eth.rpc.test/key'
	})

	it('is asked, in the one eth_call, whether it signed the digest the orderbook checks', async () => {
		const rpc = chain(SAFE_BUILD, 1n, new Map([[nonceKey(SAFE_BUILD, 1n), 1n]]))
		const signed = await safeSigned(1n)

		const { body } = await post(SAFE_BUILD, [signed, await safeSigned(2n)])

		expect(statuses(body)).toEqual(['redeemed', 'unredeemed'])
		expect(rpc.requests).toHaveLength(1)
		expect(rpc.signatureChecks).toHaveLength(2)
		const [check] = rpc.signatureChecks
		expect(check.signer).toBe(SAFE)
		expect(check.hash).toBe(hashMessage({ raw: contextHash(context(SAFE_BUILD, 1n)) }))
		expect(check.signature).toBe(signed.split(',')[1])
		expect(check.accepted).toBe(true)
	})

	it('answers invalid for a signature it refuses, and checks it again next time', async () => {
		const rpc = chain(SAFE_BUILD, 1n)
		const refused = await safeSigned(1n, [...OWNERS, OWNERS[0]])

		expect(statuses((await post(SAFE_BUILD, [refused])).body)).toEqual(['invalid'])
		expect(statuses((await post(SAFE_BUILD, [refused])).body)).toEqual(['invalid'])

		expect(rpc.signatureChecks.map(c => c.accepted)).toEqual([false, false])
		expect(cache.store.put).not.toHaveBeenCalled()
	})

	it('answers a redeemed coupon it accepted from the cache, with no RPC call', async () => {
		const rpc = chain(SAFE_BUILD, 1n, new Map([[nonceKey(SAFE_BUILD, 1n), 1n]]))
		const signed = await safeSigned(1n)
		await post(SAFE_BUILD, [signed])

		const { body } = await post(SAFE_BUILD, [signed])

		expect(statuses(body)).toEqual(['redeemed'])
		expect(body.block).toBeNull()
		expect(rpc.requests).toHaveLength(1)
	})

	it('answers invalid, with no RPC call, for one owner signature or more than 20', async () => {
		const rpc = chain(SAFE_BUILD, 1n)
		const words = context(SAFE_BUILD, 1n)
		const one = await safeSigned(1n, [OWNERS[0]])
		const tooMany = coupon(SAFE, `0x${'11'.repeat(21 * 65)}`, words)

		const { body } = await post(SAFE_BUILD, [one, tooMany])

		expect(statuses(body)).toEqual(['invalid', 'invalid'])
		expect(rpc.fetch).not.toHaveBeenCalled()
	})

	it('checks a signature of 20 owners on chain', async () => {
		const rpc = chain(SAFE_BUILD, 1n)

		await post(SAFE_BUILD, [await safeSigned(1n, Array(20).fill(OWNERS[0]))])

		expect(rpc.signatureChecks).toHaveLength(1)
	})

	it('answers invalid, with no RPC call, for an owner part that would call another contract', async () => {
		const rpc = chain(SAFE_BUILD, 1n)
		const signed = await safeSigned(1n)
		const contractPart = `${signed.split(',')[1].slice(0, -2)}00`

		const { body } = await post(SAFE_BUILD, [
			coupon(SAFE, contractPart as Hex, context(SAFE_BUILD, 1n))
		])

		expect(statuses(body)).toEqual(['invalid'])
		expect(rpc.fetch).not.toHaveBeenCalled()
	})

	it('stops asking a claim signer with no code once it has found none', async () => {
		const rpc = chain(MAINNET_BUILD, 1n)
		const POST = await load(MAINNET_BUILD)
		const words = (nonce: bigint) => context(MAINNET_BUILD, nonce)
		const signer = privateKeyToAccount(KEY_SIGNER)
		const twice = async (nonce: bigint) => {
			const signature = await signer.signMessage({ message: { raw: contextHash(words(nonce)) } })
			return coupon(signer.address, `${signature}${signature.slice(2)}`, words(nonce))
		}

		const first = await postTo(POST, [await twice(1n)])
		const second = await postTo(POST, [await twice(2n)])

		expect(statuses(first.body)).toEqual(['invalid'])
		expect(statuses(second.body)).toEqual(['invalid'])
		expect(rpc.signatureChecks).toEqual([expect.objectContaining({ accepted: false })])
		expect(rpc.fetch).toHaveBeenCalledTimes(1)
		expect(
			statuses((await postTo(POST, [await keySigned(MAINNET_BUILD, KEY_SIGNER, 3n)])).body)
		).toEqual(['unredeemed'])
	})
})
