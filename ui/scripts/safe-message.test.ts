import { describe, expect, it } from 'vitest'
import { recoverAddress } from 'viem'
import { privateKeyToAccount } from 'viem/accounts'
import {
	couponDigest,
	safeMessageHash,
	safeMessageTypedData,
	signAsOwner
} from './safe-message.mjs'

/** The coupon test vector's nine words: docs/signer-service.md, worked example. */
const WORDS = [
	'0x0000000000000000000000001111111111111111111111111111111111111111',
	'0xffffffee000000000000000000000000000000000000000014d1120d7b160000',
	'0x0000000000000000000000000000000000000000000000000000000068581c00',
	'0x5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9',
	'0x000000000000000000000000e3e064e3c2eef66cb93da8d8114f5084e92f48d6',
	'0x00000000000000000000000037fc0efec37d19f8a221aa4f8f7600c9ba2acd20',
	'0x0000000000000000000000006c6ee5e31d828de241282b9606c8e98ea48526e2',
	'0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b',
	'0x12c911abe545a1669fcbcdaeeede5352ee5a6743f59dbacdc0251b1e4c2f7b77'
] as const
const SAFE = '0x3838Fc4E0cba211c3D6D202539bB2dc2B72C7a22'
/** Anvil's account 1, an owner of SAFE: its key is public. */
const OWNER_KEY = '0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d'
const DIGEST = '0xf32a9e86bb3d75c00acec45f208001679a5f2a7976c5ce3327b6de7cecb0781f'
const HASH = '0xd02d17b3c4226662353fa285586b495d7fcf937967ac510d2ebf535047b68d34'
const SIGNATURE =
	'0xba849cad3e89c4cfe1e5bb91fe102512c6c419a63eb7ef051875c6a378fa983e1512f61cf01f253ea5a62c247dd7b1ae2ea2a70d69e7262320f7d5fdf9e97d4a1c'

describe('the SafeMessage an owner signs for a coupon', () => {
	it('hashes the worked example as the orchestrator, forge and Safe 1.4.1 do', () => {
		expect(couponDigest([...WORDS])).toBe(DIGEST)
		expect(safeMessageHash({ chainId: 1, safe: SAFE, digest: DIGEST })).toBe(HASH)
	})

	it('depends on the chain and the Safe', () => {
		expect(safeMessageHash({ chainId: 84532, safe: SAFE, digest: DIGEST })).not.toBe(HASH)
		expect(
			safeMessageHash({
				chainId: 1,
				safe: '0x1111111111111111111111111111111111111111',
				digest: DIGEST
			})
		).not.toBe(HASH)
	})

	it('is signed alike as a raw hash and as typed data, by the owner', async () => {
		const owner = privateKeyToAccount(OWNER_KEY)
		const signature = await signAsOwner(OWNER_KEY, HASH)

		expect(signature).toBe(SIGNATURE)
		expect(
			await owner.signTypedData(safeMessageTypedData({ chainId: 1, safe: SAFE, digest: DIGEST }))
		).toBe(SIGNATURE)
		expect(await recoverAddress({ hash: HASH, signature })).toBe(owner.address)
	})
})
