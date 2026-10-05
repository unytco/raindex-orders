import assert from 'node:assert/strict'
import { execFileSync, spawnSync } from 'node:child_process'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'
import { TEST_SIGNER, bindings, compose } from './compose-rainlang.mjs'

const SUBPARSER = {
	sepolia: '0xe6A589716d5a72276C08E0e08bc941a28005e55A',
	mainnet: '0xFCe5E9F48049f3D8850C2C5fd7AD792F10B36326'
}
const SIGNER = '0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC'
const COMPOSER = fileURLToPath(new URL('compose-rainlang.mjs', import.meta.url))

for (const network of ['sepolia', 'mainnet']) {
	test(`${network}: binds the subparser and signer it is given`, async () => {
		const rainlang = await compose({ network, subparser: SUBPARSER[network], signer: SIGNER })

		assert.match(rainlang, new RegExp(`using-words-from ${SUBPARSER[network]}`))
		assert.match(rainlang, new RegExp(`equal-to\\(signer<0>\\(\\) ${SIGNER}\\)`))
		assert.doesNotMatch(rainlang, new RegExp(TEST_SIGNER, 'i'))
	})
}

test('sepolia takes the test signer', async () => {
	const rainlang = await compose({
		network: 'sepolia',
		subparser: SUBPARSER.sepolia,
		signer: TEST_SIGNER
	})

	assert.match(rainlang, new RegExp(TEST_SIGNER))
})

test('composes for TestNet when given nothing', async () => {
	const rainlang = await compose({})

	assert.match(rainlang, new RegExp(`using-words-from ${SUBPARSER.sepolia}`))
	assert.match(rainlang, new RegExp(TEST_SIGNER))
	assert.equal(execFileSync('node', [COMPOSER], { encoding: 'utf8' }).trim(), rainlang.trim())
})

test('mainnet has no default binding', () => {
	assert.throws(() => bindings({ network: 'mainnet' }), {
		message:
			'--subparser must be a nonzero address, not unset; --signer must be a nonzero address, not unset'
	})
})

test('mainnet refuses the test signer, in any case', () => {
	for (const signer of [TEST_SIGNER, TEST_SIGNER.toLowerCase()]) {
		assert.throws(
			() => bindings({ network: 'mainnet', subparser: SUBPARSER.mainnet, signer }),
			/is the test signer, whose key is public: mainnet refuses it/
		)
	}
})

test('names every missing or malformed input', () => {
	assert.throws(
		() => bindings({ network: 'goerli', subparser: '0x1234', signer: undefined }),
		{
			message:
				'--network must be sepolia or mainnet, not goerli; --subparser must be a nonzero address, not 0x1234; --signer must be a nonzero address, not unset'
		}
	)
	assert.throws(
		() =>
			bindings({ network: 'sepolia', subparser: SUBPARSER.sepolia, signer: `0x${'0'.repeat(40)}` }),
		/--signer must be a nonzero address/
	)
})

test('the command prints the composed expression, and exits 1 naming a refusal', () => {
	const args = ['--network', 'mainnet', '--subparser', SUBPARSER.mainnet, '--signer']
	const out = execFileSync('node', [COMPOSER, ...args, SIGNER], { encoding: 'utf8' })
	assert.match(out, /using-words-from/)

	const refused = spawnSync('node', [COMPOSER, ...args, TEST_SIGNER], {
		encoding: 'utf8'
	})
	assert.equal(refused.status, 1)
	assert.equal(refused.stdout, '')
	assert.match(refused.stderr, /mainnet refuses it/)
})
