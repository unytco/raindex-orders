import { spawn, type ChildProcess } from 'node:child_process'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { keccak256, pad, toHex } from 'viem'
import { couponDigest, safeMessageHash } from './safe-message.mjs'

const SAFE = '0x3838Fc4E0cba211c3D6D202539bB2dc2B72C7a22'
/** Anvil's account 1, an owner of SAFE: its key is public. */
const OWNER_KEY = '0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d'
const ORDER_OWNER = '0xe3e064e3c2eef66cb93da8d8114f5084e92f48d6'
const ORDERBOOK = '0x37fc0efec37d19f8a221aa4f8f7600c9ba2acd20'
const TOKEN = '0x6c6ee5e31d828de241282b9606c8e98ea48526e2'
const VAULT_ID = '0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b'
const ORDER_A = '0x5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9'
const ORDER_B = keccak256(toHex('another claim order'))
const DAY = 24 * 60 * 60

/** A withdrawal action hash's 39 bytes, as the API sends them: `u` then base64url. */
function withdrawal(fill: number) {
	const raw = Buffer.alloc(39, fill)
	return { id: `u${raw.toString('base64url')}`, nonce: keccak256(raw) }
}

const running: ChildProcess[] = []
afterEach(() => {
	for (const child of running.splice(0)) child.kill()
})

/** Starts the mock for the claim order `orderHash`, keeping its record in `record`. */
async function startMock(orderHash: string, record: string, port: number) {
	const child = spawn(process.execPath, ['scripts/mock-signer-service.mjs'], {
		env: {
			PATH: process.env.PATH,
			OWNER_KEY,
			SAFE,
			CHAIN_ID: '1',
			PORT: String(port),
			ORDER_HASH: orderHash,
			ORDER_OWNER,
			ORDERBOOK_ADDRESS: ORDERBOOK,
			TOKEN_ADDRESS: TOKEN,
			VAULT_ID,
			SIGNED_RECORD: record
		}
	})
	running.push(child)
	await new Promise<void>((resolve, reject) => {
		child.stdout!.on('data', chunk => {
			if (String(chunk).includes('mock signer service for')) resolve()
		})
		child.on('exit', code => reject(new Error(`the mock exited with ${code}`)))
	})
	return child
}

/** Asks the mock on `port` to sign `id` for `orderHash`, expiring `expiresIn` seconds from now. */
async function sign(
	port: number,
	id: string,
	nonce: string,
	orderHash: string,
	expiresIn = 7 * DAY
) {
	const expiry = BigInt(Math.floor(Date.now() / 1000) + expiresIn)
	const words = [
		pad('0x1111111111111111111111111111111111111111'),
		'0xffffffee000000000000000000000000000000000000000014d1120d7b160000',
		pad(toHex(expiry)),
		orderHash,
		pad(ORDER_OWNER),
		pad(ORDERBOOK),
		pad(TOKEN),
		VAULT_ID,
		nonce
	]
	const digest = couponDigest(words)
	const answer = await fetch(`http://127.0.0.1:${port}/sign`, {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({
			safe: SAFE.toLowerCase(),
			chainId: 1,
			owner: '0x70997970c51812dc3a010c7d01b50e0d17dc79c8',
			withdrawal: id,
			context: words,
			couponDigest: digest,
			safeMessageHash: safeMessageHash({ chainId: 1, safe: SAFE, digest })
		})
	})
	return { status: answer.status, body: await answer.json() }
}

describe('the mock signer service', () => {
	it('refuses a withdrawal it signed for one claim order when asked for another, across a restart', async () => {
		const record = join(mkdtempSync(join(tmpdir(), 'mock-signer-')), 'signed.json')
		const port = 18_700 + Math.floor(Math.random() * 200)
		const w = withdrawal(7)
		const other = withdrawal(9)

		let mock = await startMock(ORDER_A, record, port)
		expect((await sign(port, w.id, w.nonce, ORDER_A)).status).toBe(200)
		// The same withdrawal for the same order is signed again: its nonce lets one claim.
		expect((await sign(port, w.id, w.nonce, ORDER_A)).status).toBe(200)
		mock.kill()
		await new Promise(resolve => mock.once('exit', resolve))

		// The claim order is rotated: the service restarts with the new deploy record.
		mock = await startMock(ORDER_B, record, port)
		const again = await sign(port, w.id, w.nonce, ORDER_B)
		expect(again.status).toBe(409)
		expect(again.body.code).toBe('already_signed')
		expect(again.body.reason).toContain(ORDER_A)
		expect((await sign(port, other.id, other.nonce, ORDER_B)).status).toBe(200)
	})

	it('takes an expiry up to EXPIRY_SECONDS plus the tolerance ahead, and a little behind', async () => {
		const record = join(mkdtempSync(join(tmpdir(), 'mock-signer-')), 'signed.json')
		const port = 18_900 + Math.floor(Math.random() * 90)
		await startMock(ORDER_A, record, port)
		const at = (fill: number, expiresIn: number) => {
			const w = withdrawal(fill)
			return sign(port, w.id, w.nonce, ORDER_A, expiresIn).then(answer => answer.status)
		}

		expect(await at(1, 7 * DAY + 5 * 60)).toBe(200)
		expect(await at(2, 7 * DAY + 11 * 60)).toBe(409)
		expect(await at(3, -5 * 60)).toBe(200)
		expect(await at(4, -11 * 60)).toBe(409)
	})
})
