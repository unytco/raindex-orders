import { describe, expect, it } from 'vitest'
import { BUILD_VARIABLES, TEST_SIGNER, parseBridgeConfig } from './network'
import { MAINNET_BUILD, SEPOLIA_BUILD } from './testing/builds'

const without = (build: Record<string, string>, key: string) => {
	const rest = { ...build }
	delete rest[key]
	return rest
}

describe('parseBridgeConfig', () => {
	it('reads a TestNet build as today: Sepolia, mock HOT, its explorer, the faucet', () => {
		const config = parseBridgeConfig(SEPOLIA_BUILD)

		expect(config.network).toBe('sepolia')
		expect(config.chain.id).toBe(11155111)
		expect(config.tokenName).toBe('mock HOT')
		expect(config.networkName).toBe('Sepolia Testnet')
		expect(config.explorer).toBe('https://sepolia.etherscan.io')
		expect(config.faucet).toBe(true)
		expect(config.rpcSecret).toBe('SEPOLIA_RPC_URL')
		expect(config.addToWallet?.blockExplorerUrls).toEqual(['https://sepolia.etherscan.io'])
		expect(config.tokenAddress).toBe(SEPOLIA_BUILD.PUBLIC_TOKEN_ADDRESS)
		expect(config.lockVaultAddress).toBe(SEPOLIA_BUILD.PUBLIC_LOCK_VAULT_ADDRESS)
		expect(config.orderbookAddress).toBe(SEPOLIA_BUILD.PUBLIC_ORDERBOOK_ADDRESS)
		expect(config.claimOrder).toEqual({
			orderHash: SEPOLIA_BUILD.PUBLIC_CLAIM_ORDER_HASH,
			signer: TEST_SIGNER,
			interpreter: SEPOLIA_BUILD.PUBLIC_CLAIM_INTERPRETER,
			store: SEPOLIA_BUILD.PUBLIC_CLAIM_STORE,
			expression: SEPOLIA_BUILD.PUBLIC_CLAIM_EXPRESSION,
			inputToken: SEPOLIA_BUILD.PUBLIC_CLAIM_INPUT_TOKEN
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

	it.each(['sepolia', 'mainnet'])('refuses a %s build missing any variable, naming it', network => {
		const build = network === 'sepolia' ? SEPOLIA_BUILD : MAINNET_BUILD
		for (const key of BUILD_VARIABLES) {
			expect(() => parseBridgeConfig(without(build, key))).toThrow(
				key === 'PUBLIC_NETWORK'
					? 'PUBLIC_NETWORK must be sepolia or mainnet'
					: `${key} is required`
			)
		}
	})

	it.each([
		['PUBLIC_NETWORK', 'goerli', 'PUBLIC_NETWORK must be sepolia or mainnet, not goerli'],
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
		]
	])('refuses a malformed %s', (key, value, fault) => {
		expect(() => parseBridgeConfig({ ...MAINNET_BUILD, [key]: value })).toThrow(fault)
	})

	it.each([
		['sepolia', SEPOLIA_BUILD],
		['mainnet', MAINNET_BUILD]
	])('refuses a %s build whose order does not hash to its order hash', (_, build) => {
		const stale = { ...build, PUBLIC_CLAIM_STORE: '0x1111111111111111111111111111111111111111' }

		expect(() => parseBridgeConfig(stale)).toThrow(
			'PUBLIC_CLAIM_ORDER_HASH is not the hash of the order'
		)
		expect(() =>
			parseBridgeConfig({ ...build, PUBLIC_LOCK_VAULT_ADDRESS: MAINNET_BUILD.PUBLIC_TOKEN_ADDRESS })
		).toThrow('PUBLIC_CLAIM_ORDER_HASH is not the hash of the order')
	})

	it('names every fault in one error', () => {
		expect(() =>
			parseBridgeConfig({ ...without(SEPOLIA_BUILD, 'PUBLIC_CLAIM_STORE'), PUBLIC_NETWORK: '' })
		).toThrow(
			'Bridge build variables: PUBLIC_NETWORK must be sepolia or mainnet, not ; PUBLIC_CLAIM_STORE is required'
		)
	})

	it('refuses the test signer on mainnet, in any case, and takes it on sepolia', () => {
		for (const signer of [TEST_SIGNER, TEST_SIGNER.toLowerCase()]) {
			expect(() => parseBridgeConfig({ ...MAINNET_BUILD, PUBLIC_CLAIM_SIGNER: signer })).toThrow(
				'PUBLIC_CLAIM_SIGNER is the test signer'
			)
			expect(parseBridgeConfig({ ...SEPOLIA_BUILD, PUBLIC_CLAIM_SIGNER: signer }).network).toBe(
				'sepolia'
			)
		}
	})
})
