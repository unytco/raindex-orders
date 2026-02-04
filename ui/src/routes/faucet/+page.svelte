<script lang="ts">
	import { Button, Card, FloatingLabelInput, Alert, Spinner } from 'flowbite-svelte'
	import { isAddress } from 'viem'
	import { ethereumStore } from '$lib/ethereum'
	import { onMount } from 'svelte'

	let recipient: string = ''
	let loading = false
	let error = ''
	let success = false
	let txHash = ''
	let faucetBalance = ''
	let faucetAddress = ''
	let loadingBalance = true

	$: isConnected = $ethereumStore.isConnected
	$: account = $ethereumStore.account

	// Auto-fill recipient with connected wallet
	$: if (isConnected && account && !recipient) {
		recipient = account
	}

	$: ready = recipient && isAddress(recipient) && !loading

	const loadFaucetBalance = async () => {
		loadingBalance = true
		try {
			const response = await fetch('/api/faucet')
			const data = await response.json()

			if (response.ok) {
				faucetBalance = parseFloat(data.balance).toFixed(2)
				faucetAddress = data.address
			} else {
				console.error('Failed to load faucet balance:', data.error)
			}
		} catch (e) {
			console.error('Error loading faucet balance:', e)
		} finally {
			loadingBalance = false
		}
	}

	const handleRequest = async () => {
		error = ''
		success = false
		txHash = ''
		loading = true

		try {
			if (!isAddress(recipient)) {
				throw new Error('Invalid recipient address')
			}

			const response = await fetch('/api/faucet', {
				method: 'POST',
				headers: {
					'Content-Type': 'application/json'
				},
				body: JSON.stringify({ recipient })
			})

			const data = await response.json()

			if (!response.ok) {
				throw new Error(data.error || 'Failed to request tokens')
			}

			success = true
			txHash = data.txHash

			// Reload balance after successful request
			await loadFaucetBalance()
		} catch (e: any) {
			error = e.message || 'Failed to request tokens'
			console.error('Faucet request error:', e)
		} finally {
			loading = false
		}
	}

	onMount(() => {
		loadFaucetBalance()
		// Refresh balance every 30 seconds
		const interval = setInterval(loadFaucetBalance, 30000)
		return () => clearInterval(interval)
	})
</script>

<div class="container mx-auto max-w-2xl p-4">
	<div class="mb-4">
		<a href="/" class="text-blue-600 hover:underline">← Back to Home</a>
	</div>

	<h1 class="mb-6 text-3xl font-bold">MockHOT Faucet</h1>

	<Card size="xl" class="mb-4">
		<div class="mb-4 rounded-lg bg-blue-50 p-4">
			<h3 class="mb-2 text-lg font-semibold">Faucet Status</h3>
			{#if loadingBalance}
				<div class="flex items-center gap-2">
					<Spinner size="4" />
					<span class="text-sm">Loading...</span>
				</div>
			{:else}
				<div class="text-sm text-gray-700">
					<p><strong>Available:</strong> {faucetBalance} mockHOT</p>
					<p class="break-all text-xs text-gray-600"><strong>Address:</strong> {faucetAddress}</p>
				</div>
			{/if}
		</div>

		<div class="mb-4 rounded-lg bg-yellow-50 p-4">
			<h3 class="mb-2 font-semibold">Rate Limit</h3>
			<p class="text-sm text-gray-700">
				Each address can receive <strong>10 mockHOT</strong> every <strong>10 minutes</strong>.
			</p>
		</div>

		<div class="flex flex-col gap-4">
			<FloatingLabelInput style="outlined" bind:value={recipient} type="text">
				Recipient Address
			</FloatingLabelInput>

			<Button disabled={!ready} on:click={handleRequest}>
				{#if loading}
					<Spinner class="mr-2" size="4" />
					Sending...
				{:else}
					Request 10 mockHOT
				{/if}
			</Button>
		</div>
	</Card>

	{#if error}
		<Alert color="red" class="mb-4">
			<span class="font-medium">Error:</span> {error}
		</Alert>
	{/if}

	{#if success}
		<Alert color="green" class="mb-4">
			<div>
				<p class="font-medium">✓ Successfully sent 10 mockHOT!</p>
				<p class="mt-2 text-sm">Transaction Hash:</p>
				<a
					href="https://sepolia.etherscan.io/tx/{txHash}"
					target="_blank"
					rel="noopener noreferrer"
					class="break-all font-mono text-xs hover:underline"
				>
					{txHash}
				</a>
			</div>
		</Alert>
	{/if}

	<Card size="xl">
		<h3 class="mb-2 text-lg font-semibold">About This Faucet</h3>
		<div class="text-sm text-gray-600">
			<p class="mb-2">
				This faucet provides mockHOT tokens on Sepolia testnet for testing the HOT Bridge.
			</p>
			<ul class="list-inside list-disc space-y-1">
				<li>Get 10 mockHOT per request</li>
				<li>10 minute cooldown between requests per address</li>
				<li>Free testnet tokens for development and testing</li>
			</ul>
		</div>
	</Card>
</div>
