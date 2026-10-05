<script lang="ts">
	import { Button, Card, Alert } from 'flowbite-svelte'
	import { ethereumStore, onWrongNetwork, connectWallet, switchNetwork } from '$lib/ethereum'
	import { bridge } from '$lib/config'
	import ConnectWalletModal from '$lib/components/ConnectWalletModal.svelte'

	$: isConnected = $ethereumStore.isConnected
	$: account = $ethereumStore.account

	function truncateAddress(addr: string): string {
		return `${addr.slice(0, 6)}...${addr.slice(-4)}`
	}

	let showConnectModal = false
	async function handleConnect() {
		showConnectModal = true
		await connectWallet()
		showConnectModal = false
	}
</script>

<Card size="xl" class="flex flex-col gap-6">
	<div class="text-center">
		<h1 class="mb-2 text-3xl font-bold">Bridge Home</h1>
		<p class="text-gray-600">Bridge between Blockchain and Mirrored-Units on Unyt</p>
	</div>

	{#if !isConnected}
		<Alert color="blue" class="text-center">Connect your wallet to get started</Alert>
		<Button class="w-full" on:click={handleConnect}>Connect Wallet</Button>
	{:else}
		<div class="rounded-lg bg-gray-50 p-4 text-center">
			<p class="text-sm text-gray-600">Connected</p>
			<p class="font-mono font-semibold">{truncateAddress(account || '')}</p>
			{#if $onWrongNetwork}
				<p class="mt-1 text-sm text-red-500">
					Wrong network: please switch to {bridge.networkName}
				</p>
			{:else}
				<p class="mt-1 text-sm text-green-600">{bridge.networkName}</p>
			{/if}
		</div>

		{#if $onWrongNetwork}
			<Button class="w-full" color="red" on:click={switchNetwork}
				>Switch to {bridge.networkName}</Button
			>
		{/if}
	{/if}

	{#if $ethereumStore.error}
		<Alert color="red">{$ethereumStore.error}</Alert>
	{/if}
</Card>

<ConnectWalletModal open={showConnectModal} />
