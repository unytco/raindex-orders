import { describe, expect, it } from 'vitest'
import { pad } from 'viem'
import {
	BASE_SEPOLIA_DEFAULTS,
	BUILD_VARIABLES,
	TEST_SIGNER,
	orderHashOf,
	parseBridgeConfig,
	withTestnetDefaults
} from './network'
import { BASE_SEPOLIA_BUILD, MAINNET_BUILD, withOtherStore } from './testing/builds'

const without = (build: Record<string, string>, key: string) => {
	const rest = { ...build }
	delete rest[key]
	return rest
}

// A vault that is not TestNet's: the test vectors' order owner.
const VECTOR_ORDER_VAULT = BASE_SEPOLIA_BUILD.PUBLIC_LOCK_VAULT_ADDRESS

describe('parseBridgeConfig', () => {
	it('reads a TestNet build: Base Sepolia, mock HOT, its explorer, the faucet', () => {
		const config = parseBridgeConfig(BASE_SEPOLIA_BUILD)

		expect(config.network).toBe('base-sepolia')
		expect(config.chain.id).toBe(84532)
		expect(config.tokenName).toBe('mock HOT')
		expect(config.networkName).toBe('Base Sepolia')
		expect(config.explorer).toBe('https://sepolia.basescan.org')
		expect(config.faucet).toBe(true)
		expect(config.rpcSecret).toBe('BASE_SEPOLIA_RPC_URL')
		expect(config.addToWallet?.blockExplorerUrls).toEqual(['https://sepolia.basescan.org'])
		expect(config.tokenAddress).toBe(BASE_SEPOLIA_BUILD.PUBLIC_TOKEN_ADDRESS)
		expect(config.lockVaultAddress).toBe(BASE_SEPOLIA_BUILD.PUBLIC_LOCK_VAULT_ADDRESS)
		expect(config.orderbookAddress).toBe(BASE_SEPOLIA_BUILD.PUBLIC_ORDERBOOK_ADDRESS)
		expect(config.claimOrder).toEqual({
			orderHash: BASE_SEPOLIA_BUILD.PUBLIC_CLAIM_ORDER_HASH,
			signer: TEST_SIGNER,
			interpreter: BASE_SEPOLIA_BUILD.PUBLIC_CLAIM_INTERPRETER,
			store: BASE_SEPOLIA_BUILD.PUBLIC_CLAIM_STORE,
			bytecode: BASE_SEPOLIA_BUILD.PUBLIC_CLAIM_BYTECODE,
			inputToken: BASE_SEPOLIA_BUILD.PUBLIC_CLAIM_INPUT_TOKEN
		})
	})

	it('reads a MainNet build: Ethereum, HOT, its explorer, no faucet', () => {
		const config = parseBridgeConfig(MAINNET_BUILD)

		expect(config.network).toBe('mainnet')
		expect(config.chain.id).toBe(1)
		expect(config.tokenName).toBe('HOT')
		expect(config.networkName).toBe('Ethereum')
		expect(config.explorer).toBe('https://etherscan.io')
		expect(config.faucet).toBe(false)
		expect(config.rpcSecret).toBe('ETH_RPC_URL')
		expect(config.addToWallet).toBeUndefined()
		expect(config.claimOrder.signer).toBe(MAINNET_BUILD.PUBLIC_CLAIM_SIGNER)
		expect(config.claimOrder.inputToken).toBe(MAINNET_BUILD.PUBLIC_CLAIM_INPUT_TOKEN)
	})

	it('hashes the claim order as Raindex V6 does: the order hash test vector', () => {
		// test/TestCouponSignature.t.sol and bridge-orchestrator/src/preflight.rs hash the same order.
		expect(
			orderHashOf({
				lockVaultAddress: '0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6',
				tokenAddress: '0x6c6EE5e31d828De241282B9606C8e98Ea48526E2',
				claimOrder: {
					interpreter: '0x4c52eee7Fb6aeD0F1a30130d96d02409D9d886cf',
					store: '0x1Aa775533E28B1D843e1A589034984E3a62005DC',
					bytecode: '0x00ab',
					inputToken: '0x1234567890123456789012345678901234567890'
				}
			})
		).toBe('0xaa24ef105ecbfc39695b240de6baeb09278fd4ebf75674b29bcbd2c7ef72ff0b')
	})

	it('reads a build with nothing set as TestNet: its deploy on Base Sepolia, which the order hash check accepts, its order taking coupons from its Safe', () => {
		const config = parseBridgeConfig({})

		expect(config.network).toBe('base-sepolia')
		expect(config.tokenAddress).toBe('0x48c47491e4B424896a9FB9F23ff8119B67DADff4')
		expect(config.lockVaultAddress).toBe('0xEdDdBEF1b6dEb820888661829ecF70e9621328B4')
		expect(config.claimOrder.orderHash).toBe(
			'0x98a13365e5b43eebe38053830636bb132cddd0b14cef82c9c6c8d79cf823623e'
		)
		expect(config.claimOrder.inputToken).toBe('0xcd93874448155B54FbA986bd0081836956623278')
		expect(config.claimOrder.signer).toBe('0x46F873c3ed0Fd059e2052cA3ac5835Bf6E997EE3')
		expect(config.claimOrder.bytecode).toContain(
			pad(config.claimOrder.signer).slice(2).toLowerCase()
		)
		expect(parseBridgeConfig({ PUBLIC_NETWORK: '', PUBLIC_CLAIM_STORE: '' })).toEqual(config)
		expect(withTestnetDefaults({}).defaulted).toEqual([
			'PUBLIC_NETWORK',
			...Object.keys(BASE_SEPOLIA_DEFAULTS)
		])
	})

	it('refuses a TestNet build that names part of a new deploy: the rest is the old one, and the order hash says so, or a claim signer and order hash not given together', () => {
		const hashFault = 'PUBLIC_CLAIM_ORDER_HASH is not the hash of the order'
		const pairFault = 'PUBLIC_CLAIM_SIGNER and PUBLIC_CLAIM_ORDER_HASH name one claim order'
		const neither = without(
			without(BASE_SEPOLIA_BUILD, 'PUBLIC_CLAIM_ORDER_HASH'),
			'PUBLIC_CLAIM_SIGNER'
		)
		expect(() => parseBridgeConfig({ PUBLIC_LOCK_VAULT_ADDRESS: VECTOR_ORDER_VAULT })).toThrow(
			hashFault
		)
		expect(() => parseBridgeConfig(neither)).toThrow(hashFault)
		expect(() => parseBridgeConfig({ PUBLIC_CLAIM_SIGNER: TEST_SIGNER })).toThrow(pairFault)
		expect(() => parseBridgeConfig(without(BASE_SEPOLIA_BUILD, 'PUBLIC_CLAIM_ORDER_HASH'))).toThrow(
			pairFault
		)
		expect(() => parseBridgeConfig(without(BASE_SEPOLIA_BUILD, 'PUBLIC_CLAIM_SIGNER'))).toThrow(
			pairFault
		)
	})

	it('refuses a mainnet build missing any variable, naming it, with no TestNet value', () => {
		for (const key of BUILD_VARIABLES.filter(key => key !== 'PUBLIC_NETWORK')) {
			expect(() => parseBridgeConfig(without(MAINNET_BUILD, key))).toThrow(`${key} is required`)
			expect(() => parseBridgeConfig({ ...MAINNET_BUILD, [key]: '' })).toThrow(`${key} is required`)
		}
	})

	it.each([
		['PUBLIC_NETWORK', 'sepolia', 'PUBLIC_NETWORK must be base-sepolia or mainnet, not sepolia'],
		[
			'PUBLIC_TOKEN_ADDRESS',
			'0x1234',
			'PUBLIC_TOKEN_ADDRESS=0x1234 is not a valid nonzero address'
		],
		[
			'PUBLIC_LOCK_VAULT_ADDRESS',
			'0x47fa611Bb47f2172b99135D6c00dC5555ca230C1',
			'is not a valid nonzero address'
		],
		[
			'PUBLIC_CLAIM_INPUT_TOKEN',
			'0x0000000000000000000000000000000000000000',
			'is not a valid nonzero address'
		],
		[
			'PUBLIC_CLAIM_ORDER_HASH',
			'0x1c1fe2',
			'PUBLIC_CLAIM_ORDER_HASH=0x1c1fe2 is not a 32-byte hash'
		],
		['PUBLIC_CLAIM_BYTECODE', '0x', 'PUBLIC_CLAIM_BYTECODE=0x is not hex bytecode'],
		['PUBLIC_CLAIM_BYTECODE', '0xabc', 'PUBLIC_CLAIM_BYTECODE=0xabc is not hex bytecode'],
		['PUBLIC_CLAIM_BYTECODE', 'abcd', 'PUBLIC_CLAIM_BYTECODE=abcd is not hex bytecode']
	])('refuses a malformed %s', (key, value, fault) => {
		expect(() => parseBridgeConfig({ ...MAINNET_BUILD, [key]: value })).toThrow(fault)
	})

	it.each([
		['base-sepolia', BASE_SEPOLIA_BUILD],
		['mainnet', MAINNET_BUILD]
	])('refuses a %s build whose order does not hash to its order hash', (_, build) => {
		const stale = { ...build, PUBLIC_CLAIM_STORE: '0x1111111111111111111111111111111111111111' }

		expect(() => parseBridgeConfig(stale)).toThrow(
			'PUBLIC_CLAIM_ORDER_HASH is not the hash of the order'
		)
		expect(() =>
			parseBridgeConfig({ ...build, PUBLIC_LOCK_VAULT_ADDRESS: MAINNET_BUILD.PUBLIC_TOKEN_ADDRESS })
		).toThrow('PUBLIC_CLAIM_ORDER_HASH is not the hash of the order')
		expect(() => parseBridgeConfig({ ...build, PUBLIC_CLAIM_BYTECODE: '0x00ac' })).toThrow(
			'PUBLIC_CLAIM_ORDER_HASH is not the hash of the order'
		)
		expect(parseBridgeConfig(withOtherStore(build)).claimOrder.store).toBe(
			'0x1111111111111111111111111111111111111111'
		)
	})

	it('names every fault in one error', () => {
		expect(() =>
			parseBridgeConfig({
				...without(MAINNET_BUILD, 'PUBLIC_CLAIM_STORE'),
				PUBLIC_NETWORK: 'goerli'
			})
		).toThrow(
			'Bridge build variables: PUBLIC_NETWORK must be base-sepolia or mainnet, not goerli; PUBLIC_CLAIM_STORE is required'
		)
	})

	it('refuses the test signer on mainnet, in any case, and takes it on base-sepolia', () => {
		for (const signer of [TEST_SIGNER, TEST_SIGNER.toLowerCase()]) {
			expect(() => parseBridgeConfig({ ...MAINNET_BUILD, PUBLIC_CLAIM_SIGNER: signer })).toThrow(
				'PUBLIC_CLAIM_SIGNER is the test signer'
			)
			expect(
				parseBridgeConfig({ ...BASE_SEPOLIA_BUILD, PUBLIC_CLAIM_SIGNER: signer }).network
			).toBe('base-sepolia')
		}
	})
})
