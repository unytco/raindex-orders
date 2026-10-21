// A Safe owner's signer service for test/fork-rehearsal.sh, speaking the API in
// docs/signer-service.md. It is a MOCK: it holds a test key in memory and does
// none of the Holochain checks a real service must do before it signs. It does
// the checks that need no Holochain node: the Safe, chain and owner; the claim
// order's words against the deploy record; the nonce against the withdrawal ID;
// the startup probe's fixed words; the expiry, within EXPIRY_SECONDS and
// EXPIRY_TOLERANCE_SECONDS; the hashes, which it computes itself from the words;
// and the record of the order hash it signed each withdrawal for, which it keeps
// in the file SIGNED_RECORD when that is set, and in memory otherwise.
//
//   OWNER_KEY=0x.. SAFE=0x.. CHAIN_ID=1 PORT=18601 ORDER_HASH=0x.. ORDER_OWNER=0x.. \
//   ORDERBOOK_ADDRESS=0x.. TOKEN_ADDRESS=0x.. VAULT_ID=0x.. node scripts/mock-signer-service.mjs
//
// MODE=refuse answers every /sign with 409; MODE=wrong-key signs with another key.
import { existsSync, readFileSync, renameSync, writeFileSync } from 'node:fs'
import { createServer } from 'node:http'
import { isAddressEqual, keccak256, pad, stringToHex, toHex } from 'viem'
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts'
import { couponDigest, safeMessageHash, signAsOwner } from './safe-message.mjs'

const env = name => {
	const value = process.env[name]
	if (!value) throw new Error(`${name} is required`)
	return value
}
const ownerKey = env('OWNER_KEY')
const owner = privateKeyToAccount(ownerKey).address
const safe = env('SAFE')
const chainId = Number(env('CHAIN_ID'))
const mode = process.env.MODE ?? 'sign'
const signingKey = mode === 'wrong-key' ? generatePrivateKey() : ownerKey
// docs/signer-service.md, "The expiry": the orchestrator's EXPIRY_SECONDS plus a
// tolerance for the two clocks.
const expirySeconds = BigInt(process.env.EXPIRY_SECONDS ?? 7 * 24 * 60 * 60)
const toleranceSeconds = BigInt(process.env.EXPIRY_TOLERANCE_SECONDS ?? 10 * 60)
// docs/signer-service.md, "One order per withdrawal": withdrawal -> the order
// hash this service signed it for, written before a signature leaves.
const recordFile = process.env.SIGNED_RECORD
const signed = new Map(
	Object.entries(
		recordFile && existsSync(recordFile) ? JSON.parse(readFileSync(recordFile, 'utf8')) : {}
	)
)
function remember(withdrawal, orderHash) {
	signed.set(withdrawal, orderHash)
	if (!recordFile) return
	writeFileSync(`${recordFile}.tmp`, JSON.stringify(Object.fromEntries(signed)))
	renameSync(`${recordFile}.tmp`, recordFile)
}
const word = hex => pad(hex.toLowerCase(), { size: 32 })
const record = [
	word(env('ORDER_HASH')),
	word(env('ORDER_OWNER')),
	word(env('ORDERBOOK_ADDRESS')),
	word(env('TOKEN_ADDRESS')),
	word(env('VAULT_ID'))
]
const PROBE_NONCE = keccak256(stringToHex('bridge-orchestrator startup probe'))
const ONE_WEI = word('0xffffffee00000000000000000000000000000000000000000000000000000001')
const MAX_EXPIRY = word(toHex((1n << 223n) - 1n))

class Refusal extends Error {
	constructor(status, code, reason) {
		super(reason)
		this.status = status
		this.code = code
	}
}
const refuse = (code, reason) => {
	throw new Refusal(409, code, reason)
}

/** The checks that need no Holochain node; a real service adds those first. */
function check(body) {
	if (mode === 'refuse') refuse('refused_by_operator', 'MODE=refuse')
	if (!isAddressEqual(body.safe, safe) || body.chainId !== chainId)
		refuse('wrong_safe_or_chain', `this service signs for ${safe} on chain ${chainId}`)
	if (!isAddressEqual(body.owner, owner)) refuse('wrong_owner', `this service signs as ${owner}`)
	const words = body.context
	if (
		!Array.isArray(words) ||
		words.length !== 9 ||
		!words.every(w => /^0x[0-9a-fA-F]{64}$/.test(w))
	)
		throw new Refusal(400, 'bad_request', 'context is nine 32-byte hex words')
	const lower = words.map(w => w.toLowerCase())
	for (let i = 0; i < 5; i++) {
		if (lower[3 + i] !== record[i])
			refuse('order_mismatch', `word ${3 + i} is not the deploy record's`)
	}
	if (body.withdrawal === null) {
		if (
			lower[0] !== word(safe) ||
			lower[1] !== ONE_WEI ||
			lower[2] !== MAX_EXPIRY ||
			lower[8] !== PROBE_NONCE
		)
			refuse('probe_mismatch', 'a request with no withdrawal must be the startup probe')
	} else {
		if (typeof body.withdrawal !== 'string' || !body.withdrawal.startsWith('u'))
			throw new Refusal(400, 'bad_request', 'withdrawal is a base64 action hash or null')
		const raw = Buffer.from(body.withdrawal.slice(1), 'base64url')
		if (raw.length !== 39 || lower[8] !== keccak256(raw))
			refuse('nonce_mismatch', 'word 8 is not keccak256 of the withdrawal')
		const expiry = BigInt(lower[2])
		const now = BigInt(Math.floor(Date.now() / 1000))
		if (expiry <= now - toleranceSeconds || expiry > now + expirySeconds + toleranceSeconds)
			refuse('expiry_out_of_range', `expiry ${expiry}`)
		console.log(`MOCK: no Holochain check of ${body.withdrawal}`)
	}
	const digest = couponDigest(lower)
	const hash = safeMessageHash({ chainId, safe, digest })
	if (body.couponDigest?.toLowerCase() !== digest || body.safeMessageHash?.toLowerCase() !== hash)
		refuse('hash_mismatch', `the words hash to ${digest} and ${hash}`)
	if (body.withdrawal !== null) {
		const before = signed.get(body.withdrawal)
		if (before !== undefined && before !== lower[3])
			refuse(
				'already_signed',
				`${body.withdrawal} was signed for the order ${before}; an operator clears it once that order is removed and its coupon unredeemed`
			)
		remember(body.withdrawal, lower[3])
	}
	return hash
}

async function answer(request, body) {
	if (request.method === 'GET' && request.url.endsWith('/signer'))
		return [200, { owner, safe, chainId }]
	if (request.method === 'POST' && request.url.endsWith('/sign')) {
		const hash = check(JSON.parse(body))
		return [200, { owner, signature: await signAsOwner(signingKey, hash) }]
	}
	return [404, { code: 'not_found', reason: request.url }]
}

createServer((request, response) => {
	let body = ''
	request.on('data', chunk => (body += chunk))
	request.on('end', async () => {
		let status, reply
		try {
			;[status, reply] = await answer(request, body)
		} catch (err) {
			;[status, reply] =
				err instanceof Refusal
					? [err.status, { code: err.code, reason: err.message }]
					: [400, { code: 'bad_request', reason: String(err) }]
		}
		console.log(`${request.method} ${request.url} -> ${status} ${reply.code ?? ''}`)
		response.writeHead(status, { 'Content-Type': 'application/json' })
		response.end(JSON.stringify(reply))
	})
}).listen(Number(env('PORT')), '127.0.0.1', () =>
	console.log(`mock signer service for ${owner} on ${env('PORT')}`)
)
