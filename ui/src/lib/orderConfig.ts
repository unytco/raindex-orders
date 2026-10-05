import type { Hex, Address } from 'viem'
import { bridge } from './config'
import { HOLO_VAULT_ID } from './network'

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

export function getOrderConfig(orderHash: string): OrderConfig | undefined {
	const normalizedHash = orderHash.toLowerCase()
	if (normalizedHash === CLAIM_ORDER.orderHash.toLowerCase()) {
		return CLAIM_ORDER
	}
	return undefined
}
