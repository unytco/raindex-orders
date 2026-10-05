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
import { mainnet, sepolia } from 'viem/chains'

/** The coupon signer whose private key is committed in `src/Constants.sol`. */
export const TEST_SIGNER: Address = '0x8E72b7568738da52ca3DCd9b24E178127A4E7d37'

/** What `PUBLIC_NETWORK` selects. Every other network value is a build variable. */
export const NETWORKS = {
	sepolia: {
		chain: sepolia,
		tokenName: 'mock HOT',
		networkName: 'Sepolia Testnet',
		explorer: 'https://sepolia.etherscan.io',
		faucet: true,
		rpcSecret: 'SEPOLIA_RPC_URL',
		addToWallet: {
			chainName: 'Sepolia Testnet',
			nativeCurrency: { name: 'Sepolia ETH', symbol: 'ETH', decimals: 18 },
			rpcUrls: ['https://rpc.sepolia.org'],
			blockExplorerUrls: ['https://sepolia.etherscan.io']
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
export const HOLO_VAULT_ID = 0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334bn

const ORDER_V2 = parseAbiParameters(
	'(address owner, bool handleIO, (address interpreter, address store, address expression) evaluable, (address token, uint8 decimals, uint256 vaultId)[] validInputs, (address token, uint8 decimals, uint256 vaultId)[] validOutputs)'
)

type ClaimValues = {
	lockVaultAddress: Address
	tokenAddress: Address
	claimOrder: { interpreter: Address; store: Address; expression: Address; inputToken: Address }
}

/**
 * The OrderV2 the vault added and takeOrders names. Both IOs declare 18 decimals, as
 * script/ClaimOrderScript.sol adds them.
 */
export function claimOrderStruct({ lockVaultAddress, tokenAddress, claimOrder }: ClaimValues) {
	return {
		owner: lockVaultAddress,
		handleIO: true,
		evaluable: {
			interpreter: claimOrder.interpreter,
			store: claimOrder.store,
			expression: claimOrder.expression
		},
		validInputs: [{ token: claimOrder.inputToken, decimals: 18, vaultId: HOLO_VAULT_ID }],
		validOutputs: [{ token: tokenAddress, decimals: 18, vaultId: HOLO_VAULT_ID }]
	}
}

export const orderHashOf = (values: ClaimValues): Hex =>
	keccak256(encodeAbiParameters(ORDER_V2, [claimOrderStruct(values)]))

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
		expression: Address
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
	'PUBLIC_CLAIM_EXPRESSION',
	'PUBLIC_CLAIM_INPUT_TOKEN'
] as const

/** The build variables the website reads, inlined at build time. */
export const BUILD_VARIABLES = [
	'PUBLIC_NETWORK',
	'PUBLIC_CLAIM_ORDER_HASH',
	...ADDRESS_VARIABLES
] as const

type BuildEnv = Partial<Record<string, string>>

/**
 * The website's network values, or an error naming every build variable that is
 * missing or malformed, and the test signer on mainnet.
 */
export function parseBridgeConfig(env: BuildEnv): BridgeConfig {
	const faults: string[] = []
	const network = env.PUBLIC_NETWORK
	if (network !== 'sepolia' && network !== 'mainnet') {
		faults.push(`PUBLIC_NETWORK must be sepolia or mainnet, not ${network ?? 'unset'}`)
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

	const signer = addresses.PUBLIC_CLAIM_SIGNER
	if (network === 'mainnet' && signer && isAddressEqual(signer, TEST_SIGNER)) {
		faults.push(
			`PUBLIC_CLAIM_SIGNER is the test signer ${TEST_SIGNER}, whose key is public: mainnet refuses it`
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
			expression: a.PUBLIC_CLAIM_EXPRESSION,
			inputToken: a.PUBLIC_CLAIM_INPUT_TOKEN
		}
	}
	if (orderHashOf(config).toLowerCase() !== config.claimOrder.orderHash.toLowerCase()) {
		throw new Error(
			`Bridge build variables: PUBLIC_CLAIM_ORDER_HASH is not the hash of the order that PUBLIC_LOCK_VAULT_ADDRESS, PUBLIC_TOKEN_ADDRESS, PUBLIC_CLAIM_INTERPRETER, PUBLIC_CLAIM_STORE, PUBLIC_CLAIM_EXPRESSION and PUBLIC_CLAIM_INPUT_TOKEN describe`
		)
	}
	return config
}
