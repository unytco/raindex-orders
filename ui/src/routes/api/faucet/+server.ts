import { json } from '@sveltejs/kit'
import type { RequestHandler } from './$types'
import { createWalletClient, createPublicClient, http, parseEther, formatEther } from 'viem'
import { privateKeyToAccount } from 'viem/accounts'
import { sepolia } from 'viem/chains'
// $env/dynamic/private reads from the runtime env (Cloudflare Pages secret bindings),
// so the key is NOT inlined into the built bundle — unlike $env/static/private (issue #14).
import { env } from '$env/dynamic/private'
import { PUBLIC_TOKEN_ADDRESS } from '$env/static/public'

// ERC20 ABI for transfer function
const ERC20_ABI = [
	{
		inputs: [
			{ name: 'to', type: 'address' },
			{ name: 'amount', type: 'uint256' }
		],
		name: 'transfer',
		outputs: [{ name: '', type: 'bool' }],
		stateMutability: 'nonpayable',
		type: 'function'
	},
	{
		inputs: [{ name: 'account', type: 'address' }],
		name: 'balanceOf',
		outputs: [{ name: '', type: 'uint256' }],
		stateMutability: 'view',
		type: 'function'
	}
] as const

// In-memory rate limiting store (address -> last request timestamp)
// In production, consider using Redis or a database
const rateLimitStore = new Map<string, number>()
const RATE_LIMIT_MS = 10 * 60 * 1000 // 10 minutes
const FAUCET_AMOUNT = parseEther('1000') // 1000 HOT

// Fail fast with a clear, named error if a required runtime secret binding is
// missing. Without this, a missing FAUCET_PRIVATE_KEY surfaces as an opaque viem
// error and a missing SEPOLIA_RPC_URL silently falls back to the public RPC.
function requireFaucetEnv(): { privateKey: `0x${string}`; rpcUrl: string } {
	const privateKey = env.FAUCET_PRIVATE_KEY
	const rpcUrl = env.SEPOLIA_RPC_URL
	const missing = [
		!privateKey && 'FAUCET_PRIVATE_KEY',
		!rpcUrl && 'SEPOLIA_RPC_URL'
	].filter(Boolean)
	if (missing.length > 0) {
		throw new Error(`Missing ${missing.join(', ')}`)
	}
	return { privateKey: privateKey as `0x${string}`, rpcUrl: rpcUrl as string }
}

// Get faucet balance
export const GET: RequestHandler = async () => {
	try {
		const { privateKey, rpcUrl } = requireFaucetEnv()
		const account = privateKeyToAccount(privateKey)

		const publicClient = createPublicClient({
			chain: sepolia,
			transport: http(rpcUrl)
		})

		const balance = await publicClient.readContract({
			address: PUBLIC_TOKEN_ADDRESS as `0x${string}`,
			abi: ERC20_ABI,
			functionName: 'balanceOf',
			args: [account.address]
		})

		return json({
			balance: formatEther(balance),
			address: account.address
		})
	} catch (error: any) {
		console.error('Error getting faucet balance:', error)
		return json({ error: error.message || 'Failed to get balance' }, { status: 500 })
	}
}

// Request tokens from faucet
export const POST: RequestHandler = async ({ request }) => {
	try {
		const { recipient } = await request.json()

		if (!recipient) {
			return json({ error: 'Recipient address is required' }, { status: 400 })
		}

		// Normalize address to lowercase for rate limiting
		const normalizedRecipient = recipient.toLowerCase()

		// Check rate limit
		const lastRequest = rateLimitStore.get(normalizedRecipient)
		const now = Date.now()

		if (lastRequest && now - lastRequest < RATE_LIMIT_MS) {
			const remainingMs = RATE_LIMIT_MS - (now - lastRequest)
			const remainingMinutes = Math.ceil(remainingMs / 60000)
			return json(
				{
					error: `Rate limit exceeded. Please wait ${remainingMinutes} more minute(s) before requesting again.`,
					remainingMs
				},
				{ status: 429 }
			)
		}

		// Create wallet client
		const { privateKey, rpcUrl } = requireFaucetEnv()
		const account = privateKeyToAccount(privateKey)
		const walletClient = createWalletClient({
			account,
			chain: sepolia,
			transport: http(rpcUrl)
		})

		const publicClient = createPublicClient({
			chain: sepolia,
			transport: http(rpcUrl)
		})

		// Check faucet balance
		const faucetBalance = await publicClient.readContract({
			address: PUBLIC_TOKEN_ADDRESS as `0x${string}`,
			abi: ERC20_ABI,
			functionName: 'balanceOf',
			args: [account.address]
		})

		if (faucetBalance < FAUCET_AMOUNT) {
			return json(
				{
					error: 'Faucet is empty. Please contact the administrator to refill it.',
					balance: formatEther(faucetBalance)
				},
				{ status: 503 }
			)
		}

		// Send tokens
		const hash = await walletClient.writeContract({
			address: PUBLIC_TOKEN_ADDRESS as `0x${string}`,
			abi: ERC20_ABI,
			functionName: 'transfer',
			args: [recipient as `0x${string}`, FAUCET_AMOUNT]
		})

		// Wait for transaction confirmation
		const receipt = await publicClient.waitForTransactionReceipt({ hash })

		if (receipt.status === 'success') {
			// Update rate limit store
			rateLimitStore.set(normalizedRecipient, now)

			return json({
				success: true,
				txHash: hash,
				amount: formatEther(FAUCET_AMOUNT),
				recipient
			})
		} else {
			return json({ error: 'Transaction failed' }, { status: 500 })
		}
	} catch (error: any) {
		console.error('Error sending faucet tokens:', error)
		return json(
			{
				error: error.message || 'Failed to send tokens',
				details: error.shortMessage || error.details
			},
			{ status: 500 }
		)
	}
}
