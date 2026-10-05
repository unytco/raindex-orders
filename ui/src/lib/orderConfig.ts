import type { Hex, Address } from 'viem'
import { bridge } from './config'

export interface OrderConfig {
	orderHash: Hex
	owner: Address
	// The only signer whose coupons the order accepts.
	signer: Address
	interpreter: Address
	store: Address
	expression: Address
	inputToken: Address
	inputDecimals: number
	inputVaultId: bigint
	outputToken: Address
	outputDecimals: number
	outputVaultId: bigint
	handleIO: boolean
}

/** `HOLO_VAULT_ID` in src/Constants.sol, on both networks. */
const HOLO_VAULT_ID = BigInt('0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b')

/** The claim order HoloLockVault added, from this build's variables. */
export const CLAIM_ORDER: OrderConfig = {
	...bridge.claimOrder,
	owner: bridge.lockVaultAddress,
	inputDecimals: 18,
	inputVaultId: HOLO_VAULT_ID,
	outputToken: bridge.tokenAddress,
	outputDecimals: 18,
	outputVaultId: HOLO_VAULT_ID,
	handleIO: true
}

// Build the order struct for takeOrders call
export function buildOrderStruct(config: OrderConfig) {
	return {
		owner: config.owner,
		handleIO: config.handleIO,
		evaluable: {
			interpreter: config.interpreter,
			store: config.store,
			expression: config.expression
		},
		validInputs: [
			{
				token: config.inputToken,
				decimals: config.inputDecimals,
				vaultId: config.inputVaultId
			}
		],
		validOutputs: [
			{
				token: config.outputToken,
				decimals: config.outputDecimals,
				vaultId: config.outputVaultId
			}
		]
	}
}

// Get order config by hash (for future multi-order support)
export function getOrderConfig(orderHash: string): OrderConfig | undefined {
	const normalizedHash = orderHash.toLowerCase()
	if (normalizedHash === CLAIM_ORDER.orderHash.toLowerCase()) {
		return CLAIM_ORDER
	}
	return undefined
}
