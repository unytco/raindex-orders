import type { Hex } from 'viem'

export type Order = {
	owner: Hex
	handleIO: boolean
	evaluable: {
		interpreter: Hex
		store: Hex
		expression: Hex
	}
	validInputs: Array<{
		token: Hex
		decimals: number
		vaultId: bigint
	}>
	validOutputs: Array<{
		token: Hex
		decimals: number
		vaultId: bigint
	}>
}