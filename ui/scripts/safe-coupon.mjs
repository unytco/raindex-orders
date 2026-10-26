// Collects a Safe coupon signer's owner signatures for make-coupon.mjs, as the
// orchestrator collects them (bridge-orchestrator/src/signer/safe.rs): from owner
// keys it holds and from owners' signer services (docs/signer-service.md). For test
// coupons on Base Sepolia only.
import { randomBytes } from 'node:crypto'
import {
	concat,
	getAddress,
	hashMessage,
	isAddress,
	isAddressEqual,
	keccak256,
	numberToHex,
	recoverAddress
} from 'viem'
import { privateKeyToAccount } from 'viem/accounts'
import { couponDigest, safeMessageHash, signAsOwner } from './safe-message.mjs'

const KEY = /^0x[0-9a-fA-F]{64}$/
// secp256k1's group order: a private key is above 0 and below it.
const CURVE_ORDER = 0xfffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141n
const SIGN_TIMEOUT_MS = 20_000
const MAX_ANSWER_BYTES = 64 * 1024

/**
 * Owner keys from the text of a file or of SAFE_OWNER_KEYS: separated by commas or
 * new lines, `#` starting a comment. An error names the entry, never the key.
 */
export function ownerKeysFrom(text) {
	const keys = text
		.split('\n')
		.map(line => line.replace(/#.*/, ''))
		.flatMap(line => line.split(','))
		.map(key => key.trim())
		.filter(Boolean)
	keys.forEach((key, i) => {
		if (!KEY.test(key)) throw new Error(`owner key ${i + 1} is not 0x and 64 hex digits`)
		const value = BigInt(key)
		if (value === 0n || value >= CURVE_ORDER)
			throw new Error(`owner key ${i + 1} is not a secp256k1 private key`)
	})
	return keys
}

/** SAFE_OWNER_SERVICES: `<owner address>=<base URL>`, separated by commas, as the orchestrator takes it. */
export function servicesFrom(raw) {
	return raw
		.split(',')
		.map(entry => entry.trim())
		.filter(Boolean)
		.map(entry => {
			const at = entry.indexOf('=')
			const owner = entry.slice(0, at).trim()
			const url = entry.slice(at + 1).trim()
			if (at < 0 || !isAddress(owner, { strict: false }))
				throw new Error(
					`SAFE_OWNER_SERVICES holds ${JSON.stringify(entry)}, not <owner address>=<URL>`
				)
			if (!/^https?:\/\//.test(url))
				throw new Error(`the service of ${owner} is not an http(s) URL`)
			return { owner: getAddress(owner), url: url.replace(/\/$/, '') }
		})
}

/**
 * Why make-coupon has no Safe owner to ask, for a claim order that does not take the
 * test signer. `SAFE_OWNER_SERVICES=$SERVICES` in a terminal where SERVICES was never
 * set passes an empty value, so a variable that is set but holds no owner is named.
 */
export function noOwnersGiven(env) {
	const blank = ['SAFE_OWNER_SERVICES', 'SAFE_OWNER_KEYS'].filter(
		name => name in env && !env[name].replace(/#.*/g, '').replace(/[\s,]/g, '')
	)
	const why = blank.length
		? `${blank.join(' and ')} ${blank.length > 1 ? 'are' : 'is'} set but empty, so no owner was asked: in a new terminal, a variable such as $SERVICES is empty until it is set again. `
		: ''
	return `${why}For a Safe, give its owners: keys in SAFE_OWNER_KEYS or --owner-keys-file, signer services in SAFE_OWNER_SERVICES`
}

/**
 * A withdrawal ID as the signer service API carries it, `u` then base64url of a
 * Holochain action hash's 39 bytes, and the coupon nonce it gives: keccak256 of those
 * bytes. Random, as no Holochain spend stands behind a test coupon.
 */
export function testWithdrawal(
	raw = Buffer.concat([Buffer.from('842924', 'hex'), randomBytes(36)])
) {
	if (raw.length !== 39) throw new Error('a withdrawal ID is 39 bytes')
	return { id: `u${raw.toString('base64url')}`, nonce: BigInt(keccak256(raw)) }
}

/** The withdrawal `id` (`u` then base64url of 39 bytes) and its nonce. */
export function withdrawalFrom(id) {
	const raw =
		typeof id === 'string' && id.startsWith('u') ? Buffer.from(id.slice(1), 'base64url') : null
	if (!raw || raw.length !== 39 || `u${raw.toString('base64url')}` !== id)
		throw new Error('--withdrawal must be u then the base64url of 39 bytes')
	return testWithdrawal(raw)
}

/**
 * Who signed `hash` with `signature`: 65 bytes r, s, v, with v 27 or 28 over the
 * hash, or Safe's eth_sign form, v 31 or 32 over its EIP-191 message. Null otherwise.
 */
export async function ownerSignatureSigner(hash, signature) {
	if (!/^0x[0-9a-fA-F]{130}$/.test(signature ?? '')) return null
	const v = parseInt(signature.slice(130), 16)
	try {
		if (v === 27 || v === 28) return await recoverAddress({ hash, signature })
		if (v === 31 || v === 32) {
			const plain = `${signature.slice(0, 130)}${(v - 4).toString(16)}`
			return await recoverAddress({ hash: hashMessage({ raw: hash }), signature: plain })
		}
	} catch {
		return null
	}
	return null
}

/** One owner's signature from its service, or the reason it gave none. */
async function askService({ owner, url }, request) {
	let answer
	try {
		answer = await fetch(`${url}/sign`, {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ ...request, owner: owner.toLowerCase() }),
			redirect: 'manual',
			signal: AbortSignal.timeout(SIGN_TIMEOUT_MS)
		})
	} catch (err) {
		return { reason: `did not answer: ${err.name === 'TimeoutError' ? 'timed out' : err.message}` }
	}
	const text = await answer.text()
	if (text.length > MAX_ANSWER_BYTES) return { reason: 'answered more than 64 KiB' }
	let body
	try {
		body = JSON.parse(text)
	} catch {
		return { reason: `answered HTTP ${answer.status} with no JSON` }
	}
	if (answer.status === 409)
		return { reason: `refused: ${body.code} (${String(body.reason).slice(0, 200)})` }
	if (answer.status !== 200) return { reason: `answered HTTP ${answer.status}` }
	return { signature: body.signature }
}

/**
 * The coupon's signature for `safe`: the owners in `keys` and at `services` sign the
 * SafeMessage hash of the nine `words`, each signature is checked to be its owner's
 * over that hash and the owner one of `owners`, and the `threshold` lowest owners
 * that signed are joined in ascending order, as Safe's checkNSignatures takes them.
 * `log` gets one line for each owner. Throws, naming each owner's reason, when fewer
 * than `threshold` signed.
 */
export async function collectSafeSignature({
	safe,
	chainId,
	words,
	withdrawal,
	owners,
	threshold,
	keys = [],
	services = [],
	log = () => {}
}) {
	const context = words.map(word => numberToHex(word, { size: 32 }))
	const digest = couponDigest(context)
	const hash = safeMessageHash({ chainId, safe, digest })
	const request = {
		safe: safe.toLowerCase(),
		chainId,
		withdrawal,
		context,
		couponDigest: digest,
		safeMessageHash: hash
	}
	const isOwner = address => owners.some(owner => isAddressEqual(owner, address))
	const asked = [
		...keys.map(key => {
			const owner = privateKeyToAccount(key).address
			return { owner, from: 'key', sign: async () => ({ signature: await signAsOwner(key, hash) }) }
		}),
		...services.map(service => ({
			owner: service.owner,
			from: `service ${new URL(service.url).host}`,
			sign: () => askService(service, request)
		}))
	]
	const seen = new Set()
	for (const { owner } of asked) {
		if (!isOwner(owner)) throw new Error(`${owner} is not an owner of the Safe ${safe}`)
		if (seen.has(owner.toLowerCase())) throw new Error(`${owner} is given twice`)
		seen.add(owner.toLowerCase())
	}
	const answers = await Promise.all(
		asked.map(async ({ owner, from, sign }) => {
			const { signature, reason } = await sign()
			if (reason) return { owner, from, reason }
			const signer = await ownerSignatureSigner(hash, signature)
			if (!signer || !isAddressEqual(signer, owner))
				return { owner, from, reason: 'answered a signature that is not its own over the hash' }
			return { owner, from, signature }
		})
	)
	for (const { owner, from, reason } of answers) log(`${owner} (${from}): ${reason ?? 'signed'}`)
	const signed = answers
		.filter(answer => answer.signature)
		.sort((a, b) => (BigInt(a.owner) < BigInt(b.owner) ? -1 : 1))
		.slice(0, threshold)
	if (signed.length < threshold) {
		throw new Error(
			`${signed.length} of the Safe's owners signed, and its threshold is ${threshold}: ${answers
				.filter(answer => answer.reason)
				.map(({ owner, reason }) => `${owner} ${reason}`)
				.join('; ')}`
		)
	}
	return {
		signature: concat(signed.map(answer => answer.signature)),
		digest,
		hash,
		signers: signed
	}
}
