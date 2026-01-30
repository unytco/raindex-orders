<script lang="ts">
	import { Button, Card, Alert } from 'flowbite-svelte'
	import { ethereumStore, connectWallet, switchToSepolia } from '$lib/ethereum'

	const SEPOLIA_CHAIN_ID = 11155111

	$: isConnected = $ethereumStore.isConnected
	$: account = $ethereumStore.account
	$: chainId = $ethereumStore.chainId
	$: isWrongNetwork = isConnected && chainId !== SEPOLIA_CHAIN_ID

	function truncateAddress(addr: string): string {
		return `${addr.slice(0, 6)}...${addr.slice(-4)}`
	}
</script>

<Card size="xl" class="flex flex-col gap-6">
	<div class="text-center">
		<h1 class="text-3xl font-bold mb-2">HOT Bridge</h1>
		<p class="text-gray-600">Bridge between HOT and HoloFuel</p>
	</div>

	{#if !isConnected}
		<Alert color="blue" class="text-center">
			Connect your wallet to get started
		</Alert>
		<Button class="w-full" on:click={connectWallet}>
			Connect Wallet
		</Button>
	{:else}
		<div class="bg-gray-50 p-4 rounded-lg text-center">
			<p class="text-sm text-gray-600">Connected</p>
			<p class="font-mono font-semibold">{truncateAddress(account || '')}</p>
			{#if isWrongNetwork}
				<p class="text-sm text-red-500 mt-1">Wrong network - please switch to Sepolia</p>
			{:else}
				<p class="text-sm text-green-600 mt-1">Sepolia Testnet</p>
			{/if}
		</div>

		{#if isWrongNetwork}
			<Button class="w-full" color="red" on:click={switchToSepolia}>
				Switch to Sepolia
			</Button>
		{/if}

		<div class="border-t pt-6">
			<h2 class="text-lg font-semibold mb-4 text-center">Select Bridge Direction</h2>

			<div class="flex flex-col gap-4">
				<a href="/lock" class="block">
					<Card class="hover:bg-gray-50 cursor-pointer transition-colors">
						<div class="flex items-center justify-between">
							<div>
								<h3 class="text-lg font-semibold">HOT → HoloFuel</h3>
								<p class="text-sm text-gray-600">Lock HOT tokens to receive HoloFuel on Holochain</p>
							</div>
							<div class="text-2xl">→</div>
						</div>
					</Card>
				</a>

				<a href="/claim" class="block">
					<Card class="hover:bg-gray-50 cursor-pointer transition-colors">
						<div class="flex items-center justify-between">
							<div>
								<h3 class="text-lg font-semibold">HoloFuel → HOT</h3>
								<p class="text-sm text-gray-600">Redeem HoloFuel to receive HOT tokens on Ethereum</p>
							</div>
							<div class="text-2xl">→</div>
						</div>
					</Card>
				</a>
			</div>
		</div>
	{/if}

	{#if $ethereumStore.error}
		<Alert color="red">{$ethereumStore.error}</Alert>
	{/if}
</Card>
