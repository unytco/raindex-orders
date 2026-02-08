<script lang="ts">
	import { Button, Card, FloatingLabelInput, Alert } from 'flowbite-svelte'
	import { isAddress } from 'viem'

	// Admin inputs
	let password: string = ''
	let recipient: string = ''
	let hotAmount: string = ''
	let expiryHours = 24

	// Output
	let couponCode = ''
	let couponInfo = ''
	let error = ''
	let loading = false

	$: ready = password && recipient && hotAmount && isAddress(recipient) && parseFloat(hotAmount) > 0

	const handleGenerateCoupon = async () => {
		error = ''
		couponCode = ''
		couponInfo = ''
		loading = true

		try {
			// Validate inputs
			if (!isAddress(recipient)) {
				throw new Error('Invalid recipient address') 
			}

			const amount = parseFloat(hotAmount)
			if (isNaN(amount) || amount <= 0) {
				throw new Error('Invalid HOT amount')
			}

			// Call the server API endpoint
			const response = await fetch('/api/generate-coupon', {
				method: 'POST',
				headers: {
					'Content-Type': 'application/json'
				},
				body: JSON.stringify({
					password,
					recipient,
					amount: hotAmount,
					expirySeconds: expiryHours * 3600
				})
			})

			const data = await response.json()

			if (!response.ok) {
				throw new Error(data.error || 'Failed to generate coupon')
			}

			couponCode = data.couponCode
			couponInfo = data.info

			// Copy to clipboard (only in browser context)
			if (typeof navigator !== 'undefined' && navigator.clipboard) {
				try {
					await navigator.clipboard.writeText(couponCode)
				} catch (e) {
					console.warn('Failed to copy to clipboard:', e)
				}
			}
		} catch (e: any) {
			error = e.message || 'Failed to generate coupon'
			console.error('Coupon generation error:', e)
		} finally {
			loading = false
		}
	}

	const copyToClipboard = async () => {
		if (typeof navigator !== 'undefined' && navigator.clipboard) {
			try {
				await navigator.clipboard.writeText(couponCode)
				alert('Copied to clipboard!')
			} catch (e) {
				console.error('Failed to copy to clipboard:', e)
				alert('Failed to copy to clipboard')
			}
		}
	}
</script>

<div class="container mx-auto max-w-4xl p-4">
	<h1 class="mb-6 text-3xl font-bold">Admin - Generate Coupons</h1>

	<Card size="xl" class="mb-4">
		<h2 class="mb-4 text-xl font-semibold">Create HOT Claim Coupon</h2>

		<div class="flex flex-col gap-4">
			<FloatingLabelInput style="outlined" bind:value={password} type="password">
				Admin Password
			</FloatingLabelInput>

			<FloatingLabelInput style="outlined" bind:value={recipient} type="text">
				Recipient ETH Address
			</FloatingLabelInput>

			<FloatingLabelInput style="outlined" bind:value={hotAmount} type="number" step="0.01">
				HOT Amount
			</FloatingLabelInput>

			<FloatingLabelInput style="outlined" bind:value={expiryHours} type="number" min="1">
				Expiry (hours)
			</FloatingLabelInput>

			<Button disabled={!ready || loading} on:click={handleGenerateCoupon}>
				{loading ? 'Generating...' : 'Generate Coupon'}
			</Button>
		</div>
	</Card>

	{#if error}
		<Alert color="red" class="mb-4">
			<span class="font-medium">Error:</span> {error}
		</Alert>
	{/if}

	{#if couponCode}
		<Card size="xl" class="mb-4">
			<h3 class="mb-2 text-lg font-semibold text-green-600">✓ Coupon Generated Successfully</h3>

			<div class="mb-4">
				<label class="mb-2 block text-sm font-medium">Coupon Code:</label>
				<div class="relative">
					<textarea
						readonly
						rows="4"
						class="w-full rounded-lg border border-gray-300 bg-gray-50 p-2.5 font-mono text-sm"
						value={couponCode}
					/>
					<Button size="xs" class="absolute right-2 top-2" on:click={copyToClipboard}>
						Copy
					</Button>
				</div>
			</div>

			{#if couponInfo}
				<div class="mb-4">
					<label class="mb-2 block text-sm font-medium">Details:</label>
					<pre class="whitespace-pre-wrap rounded-lg bg-gray-50 p-2.5 text-xs text-gray-700">{couponInfo}</pre>
				</div>
			{/if}
		</Card>
	{/if}

	<Card size="xl">
		<h3 class="mb-2 text-lg font-semibold">Notes</h3>
		<div class="text-sm text-gray-600">
			<p class="mb-2">
				This page generates coupons using the <code class="rounded bg-gray-100 px-1">coupon-signer</code> binary.
			</p>
			<p>
				Configuration is loaded from <code class="rounded bg-gray-100 px-1">coupon-signer/.env</code>
			</p>
		</div>
	</Card>
</div>
 