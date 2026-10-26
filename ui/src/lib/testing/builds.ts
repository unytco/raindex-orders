import { vi } from 'vitest'
import { BASE_SEPOLIA_DEFAULTS, orderHashOf, TEST_SIGNER } from '$lib/network'

/**
 * The claim order of the test vectors (test/TestCouponSignature.t.sol): a two-byte
 * stand-in bytecode, the input token 0x1234..., hashed to PUBLIC_CLAIM_ORDER_HASH as
 * Raindex V6 hashes it. Both builds carry it, so one coupon claims on either.
 */
const VECTOR_ORDER = {
	PUBLIC_TOKEN_ADDRESS: '0x6c6EE5e31d828De241282B9606C8e98Ea48526E2',
	PUBLIC_LOCK_VAULT_ADDRESS: '0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6',
	PUBLIC_ORDERBOOK_ADDRESS: '0x37FC0EFec37D19f8A221aa4F8F7600C9ba2AcD20',
	PUBLIC_CLAIM_ORDER_HASH: '0xaa24ef105ecbfc39695b240de6baeb09278fd4ebf75674b29bcbd2c7ef72ff0b',
	PUBLIC_CLAIM_INTERPRETER: '0x4c52eee7Fb6aeD0F1a30130d96d02409D9d886cf',
	PUBLIC_CLAIM_STORE: '0x1Aa775533E28B1D843e1A589034984E3a62005DC',
	PUBLIC_CLAIM_BYTECODE: '0x00ab',
	PUBLIC_CLAIM_INPUT_TOKEN: '0x1234567890123456789012345678901234567890'
}

/** A TestNet Worker's build variables: the vector's order, accepting the test signer. */
export const BASE_SEPOLIA_BUILD: Record<string, string> = {
	PUBLIC_NETWORK: 'base-sepolia',
	...(BASE_SEPOLIA_DEFAULTS as Record<string, string>),
	...VECTOR_ORDER,
	PUBLIC_CLAIM_SIGNER: TEST_SIGNER
}

/** A MainNet Worker's build variables: the vector's order, accepting anvil's third account. */
export const MAINNET_BUILD: Record<string, string> = {
	PUBLIC_NETWORK: 'mainnet',
	...VECTOR_ORDER,
	PUBLIC_CLAIM_SIGNER: '0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC'
}

/** `build` with another store, so another claim order, hashed to its own hash. */
export function withOtherStore(build: Record<string, string>): Record<string, string> {
	const other: Record<string, string> = {
		...build,
		PUBLIC_CLAIM_STORE: '0x1111111111111111111111111111111111111111'
	}
	const address = (key: string) => other[key] as `0x${string}`
	return {
		...other,
		PUBLIC_CLAIM_ORDER_HASH: orderHashOf({
			lockVaultAddress: address('PUBLIC_LOCK_VAULT_ADDRESS'),
			tokenAddress: address('PUBLIC_TOKEN_ADDRESS'),
			claimOrder: {
				interpreter: address('PUBLIC_CLAIM_INTERPRETER'),
				store: address('PUBLIC_CLAIM_STORE'),
				bytecode: address('PUBLIC_CLAIM_BYTECODE'),
				inputToken: address('PUBLIC_CLAIM_INPUT_TOKEN')
			}
		})
	}
}

export const BUILDS = { 'base-sepolia': BASE_SEPOLIA_BUILD, mainnet: MAINNET_BUILD }
export type Network = keyof typeof BUILDS

/** `load`'s modules, imported afresh as a build with these build variables. */
export async function asBuild<T>(build: Record<string, string>, load: () => Promise<T>) {
	vi.resetModules()
	vi.doMock('$env/static/public', () => build)
	return load()
}
