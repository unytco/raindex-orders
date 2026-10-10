import { vi } from 'vitest'
import { SEPOLIA_DEFAULTS } from '$lib/network'

/** The TestNet Worker's build variables. */
export const SEPOLIA_BUILD = {
	PUBLIC_NETWORK: 'sepolia',
	...SEPOLIA_DEFAULTS
}

/** A MainNet Worker's build variables: the deploy record of a fork rehearsal. */
export const MAINNET_BUILD = {
	PUBLIC_NETWORK: 'mainnet',
	PUBLIC_TOKEN_ADDRESS: '0x6c6EE5e31d828De241282B9606C8e98Ea48526E2',
	PUBLIC_LOCK_VAULT_ADDRESS: '0x47FA611Bb47f2172b99135D6c00dC5555ca230C1',
	PUBLIC_ORDERBOOK_ADDRESS: '0xf1224A483ad7F1E9aA46A8CE41229F32d7549A74',
	PUBLIC_CLAIM_ORDER_HASH: '0x1c1fe2c6a3060731a0c6be0ebeaf7b65a232d8d5774eeabbd326bfb0b7f7dfd2',
	PUBLIC_CLAIM_SIGNER: '0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC',
	PUBLIC_CLAIM_INTERPRETER: '0x4C7436641da0505A8012218c1524Db0060Fd7253',
	PUBLIC_CLAIM_STORE: '0x32a868432101C516647E7Ee217CA641B288953C6',
	PUBLIC_CLAIM_EXPRESSION: '0x1e814F560938B7Ed82Ba00Cc075a822E4789309E',
	PUBLIC_CLAIM_INPUT_TOKEN: '0xdAC17F958D2ee523a2206206994597C13D831ec7'
}

export const BUILDS = { sepolia: SEPOLIA_BUILD, mainnet: MAINNET_BUILD }
export type Network = keyof typeof BUILDS

/** `load`'s modules, imported afresh as a build with these build variables. */
export async function asBuild<T>(build: Record<string, string>, load: () => Promise<T>) {
	vi.resetModules()
	vi.doMock('$env/static/public', () => build)
	return load()
}
