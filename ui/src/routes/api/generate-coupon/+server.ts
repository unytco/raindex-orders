import { json } from '@sveltejs/kit'
import type { RequestHandler } from './$types'
import { exec } from 'child_process'
import { promisify } from 'util'
import path from 'path'
import { ADMIN_PASSWORD } from '$env/static/private'

const execAsync = promisify(exec)

export const POST: RequestHandler = async ({ request }) => {
	try {
		const { recipient, amount, expirySeconds, password } = await request.json()

		// Validate password first
		if (!password || password !== ADMIN_PASSWORD) {
			return json({ error: 'Invalid password' }, { status: 401 })
		}

		// Validate inputs
		if (!recipient || !amount) {
			return json({ error: 'Missing required fields: recipient and amount' }, { status: 400 })
		}

		// Path to the coupon-signer binary
		// Assumes it's built in the coupon-signer directory
		const couponSignerPath = path.join(
			process.cwd(),
			'../coupon-signer/target/release/coupon-signer'
		)

		// Build the command
		// The coupon-signer will read env vars from its .env file or from environment
		const command = `${couponSignerPath} --amount "${amount}" --recipient "${recipient}" --expiry-seconds ${expirySeconds || 604800} --output ui`

		console.log('Executing:', command)

		// Execute the coupon-signer
		const { stdout, stderr } = await execAsync(command, {
			cwd: path.join(process.cwd(), '../coupon-signer'),
			env: {
				...process.env,
				// The .env file in coupon-signer directory will be loaded by the binary
			}
		})

		// The "ui" format outputs the coupon code on the first line of stdout
		// stderr contains the human-readable info
		const lines = stdout.trim().split('\n')
		const couponCode = lines[0] // First line is the coupon code

		if (!couponCode) {
			console.error('stderr:', stderr)
			return json({ error: 'Failed to generate coupon: no output from binary' }, { status: 500 })
		}

		return json({
			success: true,
			couponCode,
			info: stderr.trim() // Contains human-readable details
		})
	} catch (error: any) {
		console.error('Error generating coupon:', error)
		return json(
			{
				error: error.message || 'Failed to generate coupon',
				details: error.stderr || error.stdout
			},
			{ status: 500 }
		)
	}
}
