import { BaseError } from 'viem'

/**
 * Logs a failed call without the RPC URL. viem's error message names the URL, and with
 * it any API key the URL holds, so only its short message is logged, and a route answers
 * its caller with a fixed message of its own.
 */
export function logRpcError(context: string, error: unknown) {
	console.error(`${context}:`, error instanceof BaseError ? error.shortMessage : error)
}
