import {
	encodeAbiParameters,
	isAddress,
	isAddressEqual,
	isHex,
	keccak256,
	parseAbiParameters,
	zeroAddress,
	type Address,
	type Hex
} from 'viem'
import { baseSepolia, mainnet } from 'viem/chains'

/** The coupon signer whose private key is committed in `src/Constants.sol`. */
export const TEST_SIGNER: Address = '0x8E72b7568738da52ca3DCd9b24E178127A4E7d37'

/** What `PUBLIC_NETWORK` selects. Every other network value is a build variable. */
export const NETWORKS = {
	'base-sepolia': {
		chain: baseSepolia,
		tokenName: 'mock HOT',
		networkName: 'Base Sepolia',
		explorer: 'https://sepolia.basescan.org',
		faucet: true,
		rpcSecret: 'BASE_SEPOLIA_RPC_URL',
		addToWallet: {
			chainName: 'Base Sepolia',
			nativeCurrency: { name: 'Sepolia ETH', symbol: 'ETH', decimals: 18 },
			rpcUrls: ['https://sepolia.base.org'],
			blockExplorerUrls: ['https://sepolia.basescan.org']
		}
	},
	mainnet: {
		chain: mainnet,
		tokenName: 'HOT',
		networkName: 'Ethereum',
		explorer: 'https://etherscan.io',
		faucet: false,
		rpcSecret: 'ETH_RPC_URL',
		addToWallet: undefined
	}
} as const

export type NetworkName = keyof typeof NETWORKS

/** `HOLO_VAULT_ID` in src/Constants.sol, on both networks. */
export const HOLO_VAULT_ID: Hex =
	'0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b'

/** `CLAIM_ORDER_NONCE` in src/Constants.sol: one claim order per signer. */
export const CLAIM_ORDER_NONCE: Hex = `0x${'0'.repeat(64)}`

/**
 * `CLAIM_IO_RATIO` in src/Constants.sol: the claim order's io-ratio, `1e-30` in
 * src/holo-claim.rain, as the decimal float the parser makes of it (coefficient 1,
 * exponent -30). A claim passes it as its maximum ratio.
 */
export const CLAIM_IO_RATIO: Hex =
	'0xffffffe200000000000000000000000000000000000000000000000000000001'

const ORDER_V4 = parseAbiParameters(
	'(address owner, (address interpreter, address store, bytes bytecode) evaluable, (address token, bytes32 vaultId)[] validInputs, (address token, bytes32 vaultId)[] validOutputs, bytes32 nonce)'
)

type ClaimValues = {
	lockVaultAddress: Address
	tokenAddress: Address
	claimOrder: { interpreter: Address; store: Address; bytecode: Hex; inputToken: Address }
}

/**
 * The OrderV4 the vault added and takeOrders4 names, as script/ClaimOrderScript.sol
 * adds it: its bytecode is the parsed claim expression, and both IOs pay out of
 * HOLO_VAULT_ID.
 */
export function claimOrderStruct({ lockVaultAddress, tokenAddress, claimOrder }: ClaimValues) {
	return {
		owner: lockVaultAddress,
		evaluable: {
			interpreter: claimOrder.interpreter,
			store: claimOrder.store,
			bytecode: claimOrder.bytecode
		},
		validInputs: [{ token: claimOrder.inputToken, vaultId: HOLO_VAULT_ID }],
		validOutputs: [{ token: tokenAddress, vaultId: HOLO_VAULT_ID }],
		nonce: CLAIM_ORDER_NONCE
	}
}

/** The order's hash as Raindex V6 hashes it: keccak256 of abi.encode(OrderV4). */
export const orderHashOf = (values: ClaimValues): Hex =>
	keccak256(encodeAbiParameters(ORDER_V4, [claimOrderStruct(values)]))

export type BridgeConfig = (typeof NETWORKS)[NetworkName] & {
	network: NetworkName
	tokenAddress: Address
	lockVaultAddress: Address
	orderbookAddress: Address
	claimOrder: {
		orderHash: Hex
		signer: Address
		interpreter: Address
		store: Address
		bytecode: Hex
		inputToken: Address
	}
}

const ADDRESS_VARIABLES = [
	'PUBLIC_TOKEN_ADDRESS',
	'PUBLIC_LOCK_VAULT_ADDRESS',
	'PUBLIC_ORDERBOOK_ADDRESS',
	'PUBLIC_CLAIM_SIGNER',
	'PUBLIC_CLAIM_INTERPRETER',
	'PUBLIC_CLAIM_STORE',
	'PUBLIC_CLAIM_INPUT_TOKEN'
] as const

/** The build variables the website reads, inlined at build time. */
export const BUILD_VARIABLES = [
	'PUBLIC_NETWORK',
	'PUBLIC_CLAIM_ORDER_HASH',
	'PUBLIC_CLAIM_BYTECODE',
	...ADDRESS_VARIABLES
] as const

type BuildEnv = Partial<Record<string, string>>

/**
 * The bytecode of TestNet's claim order on Base Sepolia, as its AddOrderV3 event
 * carries it, with TestNet's PUBLIC_CLAIM_SIGNER as its valid-signer.
 */
const BASE_SEPOLIA_CLAIM_BYTECODE: Hex =
	'0x000000000000000000000000000000000000000000000000000000000000000d00000000000000000000000046f873c3ed0fd059e2052ca3ac5835bf6e997ee30000000000000000000000000000000000000057726f6e67207369676e6572ec0000000000000000000000000000000057726f6e6720726563697069656e74ef0000000000000000000000000000000000004f726465722065787069726564ed00000000000000000000000000000057726f6e67206f726465722068617368f0000000000000000000000000000057726f6e67206f72646572206f776e6572f10000000000000000000000000000000057726f6e67206f72646572626f6f6bef0000000000000000000000000057726f6e67206f757470757420746f6b656ef20000000000000000000057726f6e67206f7574707574207661756c74206964f5000000000000000000000000004e6f6e636520616c72656164792075736564f20000000000000000000000000000000000000000000000000000000000000001ffffffe20000000000000000000000000000000000000000000000000000000100000000000000000000000057726f6e67206f757470757420616d6f756e74f300000000000000000000000000000000000000000000000000000000000000e902000000cc320300020110000101100000031000051c1200001e0200000110000203100201031000061c1200001e020000011000030310020618100000251200001e0200000110000403100001031003061c1200001e0200000110000503100101031004061c1200001e0200000110000603100100031005061c1200001e0200000110000703100004031006061c1200001e0200000110000803100204031007061c1200001e020000011000090310080646110000241100001e0200000110000a0310080647020000031001060110000b050300000110000c03100106031004041f1200001e020000'

/**
 * TestNet's values: a base-sepolia build takes each one it is not given. Some are
 * the same on every chain: Raindex V6 and the Rainlang interpreter and store. The
 * rest are TestNet's deploy on Base Sepolia (README.md, TestNet on Base Sepolia):
 * mock HOT, the lock vault, its ClaimInputToken, and the claim order and its signer.
 * A build that names a new deploy names all of it.
 * Mainnet has no defaults.
 */
export const BASE_SEPOLIA_DEFAULTS: Partial<
	Record<Exclude<(typeof BUILD_VARIABLES)[number], 'PUBLIC_NETWORK'>, string>
> = {
	PUBLIC_ORDERBOOK_ADDRESS: '0x37FC0EFec37D19f8A221aa4F8F7600C9ba2AcD20',
	PUBLIC_CLAIM_INTERPRETER: '0x4c52eee7Fb6aeD0F1a30130d96d02409D9d886cf',
	PUBLIC_CLAIM_STORE: '0x1Aa775533E28B1D843e1A589034984E3a62005DC',
	PUBLIC_TOKEN_ADDRESS: '0x48c47491e4B424896a9FB9F23ff8119B67DADff4',
	PUBLIC_LOCK_VAULT_ADDRESS: '0xEdDdBEF1b6dEb820888661829ecF70e9621328B4',
	PUBLIC_CLAIM_INPUT_TOKEN: '0xcd93874448155B54FbA986bd0081836956623278',
	PUBLIC_CLAIM_SIGNER: '0x46F873c3ed0Fd059e2052cA3ac5835Bf6E997EE3',
	PUBLIC_CLAIM_ORDER_HASH: '0x98a13365e5b43eebe38053830636bb132cddd0b14cef82c9c6c8d79cf823623e',
	PUBLIC_CLAIM_BYTECODE: BASE_SEPOLIA_CLAIM_BYTECODE
}

/**
 * `env` with TestNet's values for the network values a base-sepolia build is not
 * given, PUBLIC_NETWORK itself base-sepolia when unset. An empty value counts as
 * not given. Returns the names it filled.
 */
export function withTestnetDefaults(env: BuildEnv): { env: BuildEnv; defaulted: string[] } {
	const given = (key: string) => (env[key] ? env[key] : undefined)
	const network = given('PUBLIC_NETWORK') ?? 'base-sepolia'
	const filled: BuildEnv = { ...env, PUBLIC_NETWORK: network }
	const defaulted = given('PUBLIC_NETWORK') ? [] : ['PUBLIC_NETWORK']
	if (network === 'base-sepolia') {
		for (const [key, value] of Object.entries(BASE_SEPOLIA_DEFAULTS)) {
			if (!given(key)) {
				filled[key] = value
				defaulted.push(key)
			}
		}
	}
	return { env: filled, defaulted }
}

/** The website's network values, or an error naming every fault in its build variables. */
export function parseBridgeConfig(given: BuildEnv): BridgeConfig {
	const { env, defaulted } = withTestnetDefaults(given)
	const faults: string[] = []
	const network = env.PUBLIC_NETWORK
	if (network !== 'base-sepolia' && network !== 'mainnet') {
		faults.push(`PUBLIC_NETWORK must be base-sepolia or mainnet, not ${network}`)
	}

	const addresses: Partial<Record<(typeof ADDRESS_VARIABLES)[number], Address>> = {}
	for (const key of ADDRESS_VARIABLES) {
		const value = env[key]
		if (!value) faults.push(`${key} is required`)
		else if (!isAddress(value) || isAddressEqual(value, zeroAddress)) {
			faults.push(`${key}=${value} is not a valid nonzero address`)
		} else addresses[key] = value
	}

	const orderHash = env.PUBLIC_CLAIM_ORDER_HASH
	if (!orderHash) faults.push('PUBLIC_CLAIM_ORDER_HASH is required')
	else if (!isHex(orderHash, { strict: true }) || orderHash.length !== 66) {
		faults.push(`PUBLIC_CLAIM_ORDER_HASH=${orderHash} is not a 32-byte hash`)
	}

	const bytecode = env.PUBLIC_CLAIM_BYTECODE
	if (!bytecode) faults.push('PUBLIC_CLAIM_BYTECODE is required')
	else if (!isHex(bytecode, { strict: true }) || bytecode.length % 2 !== 0 || bytecode.length < 4) {
		faults.push(`PUBLIC_CLAIM_BYTECODE=${bytecode} is not hex bytecode`)
	}

	const signer = addresses.PUBLIC_CLAIM_SIGNER
	if (network === 'mainnet' && signer && isAddressEqual(signer, TEST_SIGNER)) {
		faults.push(
			`PUBLIC_CLAIM_SIGNER is the test signer ${TEST_SIGNER}, whose key is public: mainnet refuses it`
		)
	}
	if (defaulted.includes('PUBLIC_CLAIM_SIGNER') !== defaulted.includes('PUBLIC_CLAIM_ORDER_HASH')) {
		faults.push(
			"PUBLIC_CLAIM_SIGNER and PUBLIC_CLAIM_ORDER_HASH name one claim order: give both from one deploy record, or neither for TestNet's"
		)
	}

	if (faults.length > 0) throw new Error(`Bridge build variables: ${faults.join('; ')}`)
	const a = addresses as Record<(typeof ADDRESS_VARIABLES)[number], Address>
	const config: BridgeConfig = {
		...NETWORKS[network as NetworkName],
		network: network as NetworkName,
		tokenAddress: a.PUBLIC_TOKEN_ADDRESS,
		lockVaultAddress: a.PUBLIC_LOCK_VAULT_ADDRESS,
		orderbookAddress: a.PUBLIC_ORDERBOOK_ADDRESS,
		claimOrder: {
			orderHash: orderHash as Hex,
			signer: a.PUBLIC_CLAIM_SIGNER,
			interpreter: a.PUBLIC_CLAIM_INTERPRETER,
			store: a.PUBLIC_CLAIM_STORE,
			bytecode: bytecode as Hex,
			inputToken: a.PUBLIC_CLAIM_INPUT_TOKEN
		}
	}
	if (orderHashOf(config).toLowerCase() !== config.claimOrder.orderHash.toLowerCase()) {
		throw new Error(
			`Bridge build variables: PUBLIC_CLAIM_ORDER_HASH is not the hash of the order that PUBLIC_LOCK_VAULT_ADDRESS, PUBLIC_TOKEN_ADDRESS, PUBLIC_CLAIM_INTERPRETER, PUBLIC_CLAIM_STORE, PUBLIC_CLAIM_BYTECODE and PUBLIC_CLAIM_INPUT_TOKEN describe`
		)
	}
	return config
}
