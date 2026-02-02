// Order configuration - hardcoded from deployment since subgraph may be behind
// Update these values after redeploying the order

import type { Hex, Address } from 'viem'

export interface OrderConfig {
	orderHash: Hex
	owner: Address
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

// Sepolia claim order deployed via HoloLockVault
export const CLAIM_ORDER: OrderConfig = {
	orderHash: '0x5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9',
	owner: '0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6', // HoloLockVault
	interpreter: '0x8853d126bc23a45b9f807739b6ea0b38ef569005',
	store: '0x23f77e7bc935503e437166498d7d72f2ea290e1f',
	expression: '0x0a1369aee76570cc7404492d55a5d1468d5a9b4b',
	// Input: NOOP token (placeholder for claims)
	inputToken: '0x555FA2F68dD9B7dB6c8cA1F03bFc317ce61e9028',
	inputDecimals: 18,
	inputVaultId: BigInt('0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b'),
	// Output: MockHOT token
	outputToken: '0xeaC8eEEE9f84F3E3F592e9D8604100eA1b788749',
	outputDecimals: 18,
	outputVaultId: BigInt('0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b'),
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
