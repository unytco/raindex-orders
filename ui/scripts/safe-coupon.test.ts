import { spawn, type ChildProcess } from 'node:child_process'
import { afterEach, describe, expect, it } from 'vitest'
import { hashMessage, keccak256, numberToHex, type Hex } from 'viem'
import { privateKeyToAccount, sign } from 'viem/accounts'
import { couponWords, HOLO_VAULT_ID } from './coupon-signing.mjs'
import { couponDigest, safeMessageHash } from './safe-message.mjs'
import {
	collectSafeSignature,
	noOwnersGiven,
	ownerKeysFrom,
	ownerSignatureSigner,
	servicesFrom,
	testWithdrawal,
	withdrawalFrom
} from './safe-coupon.mjs'

/** Anvil's accounts 1, 2 and 3: their keys are public. */
const KEYS: Hex[] = [
	'0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d',
	'0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a',
	'0x7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6'
]
const OWNERS = KEYS.map(key => privateKeyToAccount(key).address)
const STRANGER: Hex = '0x47e179ec197488593b187f80a00eb0da91f1b9d0b13f8733639f19c30a34926a'
const SAFE = '0x3838Fc4E0cba211c3D6D202539bB2dc2B72C7a22'
const ORDER = {
	hash: 0x5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9n,
	owner: 0xe3e064e3c2eef66cb93da8d8114f5084e92f48d6n,
	orderbook: 0x37fc0efec37d19f8a221aa4f8f7600c9ba2acd20n,
	token: 0x6c6ee5e31d828de241282b9606c8e98ea48526e2n,
	vaultId: HOLO_VAULT_ID
}
const words = (nonce: bigint) =>
	couponWords({
		order: ORDER,
		recipient: '0x1111111111111111111111111111111111111111',
		wei: 15n * 10n ** 17n,
		expiry: BigInt(Math.floor(Date.now() / 1000) + 7 * 24 * 60 * 60),
		nonce
	})
const hashOf = (context: bigint[]) =>
	safeMessageHash({
		chainId: 84532,
		safe: SAFE,
		digest: couponDigest(context.map(word => numberToHex(word, { size: 32 })))
	})

describe('owner keys and services, as make-coupon reads them', () => {
	it('takes keys separated by commas or lines, with comments, and names a bad one by number only', () => {
		expect(
			ownerKeysFrom(`# the Safe's owners\n${KEYS[0]}\n\n${KEYS[1]}, ${KEYS[2]} # three\n`)
		).toEqual(KEYS)
		const almost = KEYS[1].slice(0, -1)
		expect(() => ownerKeysFrom(`${KEYS[0]}\n${almost}`)).toThrow(
			'owner key 2 is not 0x and 64 hex digits'
		)
		expect(() => ownerKeysFrom(almost)).not.toThrow(almost)
		for (const outOfRange of [
			`0x${'0'.repeat(64)}`,
			'0xfffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141',
			`0x${'f'.repeat(64)}`
		]) {
			expect(() => ownerKeysFrom(`${KEYS[0]}\n${outOfRange}`)).toThrow(
				'owner key 2 is not a secp256k1 private key'
			)
			expect(() => ownerKeysFrom(outOfRange)).not.toThrow(BigInt(outOfRange).toString())
		}
		expect(
			ownerKeysFrom('0xfffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364140')
		).toHaveLength(1)
	})

	it('takes SAFE_OWNER_SERVICES as the orchestrator does', () => {
		expect(
			servicesFrom(`${OWNERS[0].toLowerCase()}=http://127.0.0.1:1/, ${OWNERS[1]}=https://b.test/x`)
		).toEqual([
			{ owner: OWNERS[0], url: 'http://127.0.0.1:1' },
			{ owner: OWNERS[1], url: 'https://b.test/x' }
		])
		expect(() => servicesFrom('http://127.0.0.1:1')).toThrow('not <owner address>=<URL>')
		expect(() => servicesFrom(`${OWNERS[0]}=127.0.0.1:1`)).toThrow('not an http(s) URL')
	})

	it('names an owner variable that is set but empty, as SAFE_OWNER_SERVICES=$SERVICES leaves it in a new terminal', () => {
		const give =
			'For a Safe, give its owners: keys in SAFE_OWNER_KEYS or --owner-keys-file, signer services in SAFE_OWNER_SERVICES'
		expect(noOwnersGiven({})).toBe(give)
		expect(noOwnersGiven({ SAFE_OWNER_SERVICES: '' })).toBe(
			`SAFE_OWNER_SERVICES is set but empty, so no owner was asked: in a new terminal, a variable such as $SERVICES is empty until it is set again. ${give}`
		)
		expect(noOwnersGiven({ SAFE_OWNER_SERVICES: ' , ', SAFE_OWNER_KEYS: '# none\n' })).toBe(
			`SAFE_OWNER_SERVICES and SAFE_OWNER_KEYS are set but empty, so no owner was asked: in a new terminal, a variable such as $SERVICES is empty until it is set again. ${give}`
		)
	})

	it('makes a withdrawal ID of 39 bytes whose keccak256 is the nonce, and reads one back', () => {
		const w = testWithdrawal()
		const raw = Buffer.from(w.id.slice(1), 'base64url')

		expect(raw.length).toBe(39)
		expect(raw.subarray(0, 3).toString('hex')).toBe('842924')
		expect(w.nonce).toBe(BigInt(keccak256(raw)))
		expect(withdrawalFrom(w.id)).toEqual(w)
		expect(() => withdrawalFrom('uAAAA')).toThrow('39 bytes')
	})
})

describe('an owner signature', () => {
	it("is read as the hash's, v 27 or 28, or as Safe's eth_sign form, v 31 or 32", async () => {
		const hash = keccak256('0x01')
		const raw = await sign({ hash, privateKey: KEYS[0], to: 'hex' })
		const ethSign = await sign({ hash: hashMessage({ raw: hash }), privateKey: KEYS[0], to: 'hex' })
		const v = parseInt(ethSign.slice(130), 16)
		const asSafe = `${ethSign.slice(0, 130)}${(v + 4).toString(16)}`

		expect(await ownerSignatureSigner(hash, raw)).toBe(OWNERS[0])
		expect(await ownerSignatureSigner(hash, asSafe)).toBe(OWNERS[0])
		expect(await ownerSignatureSigner(hash, ethSign)).not.toBe(OWNERS[0])
		expect(await ownerSignatureSigner(hash, `${raw.slice(0, 130)}00`)).toBeNull()
		expect(await ownerSignatureSigner(hash, raw.slice(0, 20))).toBeNull()
	})
})

const running: ChildProcess[] = []
afterEach(() => {
	for (const child of running.splice(0)) child.kill()
})

/** The mock signer service of owner `i` on `port`, for ORDER on `chainId`. */
async function mockService(i: number, port: number, chainId = 84532) {
	const child = spawn(process.execPath, ['scripts/mock-signer-service.mjs'], {
		env: {
			PATH: process.env.PATH,
			OWNER_KEY: KEYS[i],
			SAFE,
			CHAIN_ID: String(chainId),
			PORT: String(port),
			ORDER_HASH: numberToHex(ORDER.hash, { size: 32 }),
			ORDER_OWNER: numberToHex(ORDER.owner, { size: 20 }),
			ORDERBOOK_ADDRESS: numberToHex(ORDER.orderbook, { size: 20 }),
			TOKEN_ADDRESS: numberToHex(ORDER.token, { size: 20 }),
			VAULT_ID: numberToHex(ORDER.vaultId, { size: 32 })
		}
	})
	running.push(child)
	await new Promise<void>((resolve, reject) => {
		child.stdout!.on('data', chunk => {
			if (String(chunk).includes('mock signer service for')) resolve()
		})
		child.on('exit', code => reject(new Error(`the mock exited with ${code}`)))
	})
	return { owner: OWNERS[i], url: `http://127.0.0.1:${port}` }
}

describe("collecting a Safe's owner signatures", () => {
	const safe = { safe: SAFE, chainId: 84532, owners: OWNERS, threshold: 2 }

	it('takes the threshold of the lowest owners that signed, in ascending order', async () => {
		const context = words(1n)
		const { signature, signers } = await collectSafeSignature({
			...safe,
			words: context,
			withdrawal: null,
			keys: [KEYS[2], KEYS[0], KEYS[1]]
		})

		const lowest = [...OWNERS].sort((a, b) => (BigInt(a) < BigInt(b) ? -1 : 1)).slice(0, 2)
		expect(signers.map(s => s.owner)).toEqual(lowest)
		expect(signature.length).toBe(2 + 2 * 130)
		const parts = [signature.slice(2, 132), signature.slice(132)].map(p => `0x${p}`)
		for (const [i, part] of parts.entries())
			expect(await ownerSignatureSigner(hashOf(context), part)).toBe(lowest[i])
	})

	it('refuses fewer signers than the threshold, a stranger, and an owner given twice', async () => {
		const base = { ...safe, words: words(2n), withdrawal: null }

		await expect(collectSafeSignature({ ...base, keys: [KEYS[0]] })).rejects.toThrow(
			"1 of the Safe's owners signed, and its threshold is 2"
		)
		const stranger = privateKeyToAccount(STRANGER).address
		await expect(collectSafeSignature({ ...base, keys: [KEYS[0], STRANGER] })).rejects.toThrow(
			`${stranger} is not an owner of the Safe`
		)
		await expect(collectSafeSignature({ ...base, keys: [KEYS[0], KEYS[0]] })).rejects.toThrow(
			'is given twice'
		)
	})

	it('asks signer services with the withdrawal, and names a refusal', async () => {
		const port = 19_100 + Math.floor(Math.random() * 400)
		const [a, b] = await Promise.all([mockService(0, port), mockService(1, port + 1)])
		const w = testWithdrawal()
		const lines: string[] = []

		const { signers } = await collectSafeSignature({
			...safe,
			words: words(w.nonce),
			withdrawal: w.id,
			services: [a, b],
			log: (line: string) => lines.push(line)
		})
		expect(signers).toHaveLength(2)
		expect(lines.every(line => line.endsWith(': signed'))).toBe(true)

		// A request for Ethereum mainnet: each service signs for Base Sepolia only.
		const other = testWithdrawal()
		await expect(
			collectSafeSignature({
				...safe,
				chainId: 1,
				words: words(other.nonce),
				withdrawal: other.id,
				services: [a],
				keys: [KEYS[2]]
			})
		).rejects.toThrow('refused: wrong_safe_or_chain')
	})
})
