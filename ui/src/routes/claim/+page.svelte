<script lang="ts">
	import { Button, Card, Spinner, Alert, Input, Label } from 'flowbite-svelte'
	import { orderbookAbi } from '../../generated'
	import { formatUnits, type Hex } from 'viem'
	import TransactionModal from '$lib/components/TransactionModal.svelte'
	import TransactionReceipt from '$lib/components/TransactionReceipt.svelte'
	import ConnectWalletModal from '$lib/components/ConnectWalletModal.svelte'
	import WrongNetwork from '$lib/components/WrongNetwork.svelte'
	import { bridge, explorerAddress } from '$lib/config'
	import { deserializeSignedContext, parseCoupon, type SignedContextV1Struct } from '$lib/coupon'
	import { getOrderConfig, type OrderConfig } from '$lib/orderConfig'
	import { claimCoupon } from '$lib/claim'
	import { ethereumStore, onWrongNetwork, connectWallet, readContract } from '$lib/ethereum'
	import { errorMessage } from '$lib/utils'
	import { onMount } from 'svelte'
	import { browser } from '$app/environment'

	$: isConnected = $ethereumStore.isConnected

	let couponInput = ''
	let couponPrefilledFromUrl = false
	let signedContext: SignedContextV1Struct | undefined

	let orderConfig: OrderConfig | undefined
	let orderExists = false
	let vaultBalance: bigint | undefined
	let isCheckingOrder = false

	onMount(() => {
		if (browser) {
			const urlParam = new URL(window.location.href).searchParams.get('c')
			if (urlParam) {
				couponInput = urlParam
				couponPrefilledFromUrl = true
				parseCouponInput()
			}
		}
	})

	async function parseCouponInput() {
		if (!couponInput) {
			signedContext = undefined
			orderConfig = undefined
			orderExists = false
			return
		}
		try {
			signedContext = deserializeSignedContext(couponInput)
			const couponData = parseCoupon(signedContext)

			orderConfig = getOrderConfig(couponData.orderHash)
		} catch (e) {
			console.error('Failed to parse coupon:', e)
			signedContext = undefined
			orderConfig = undefined
		}
	}

	// The reads go through the wallet, so they run again once it is on this network.
	$: if (orderConfig && !$onWrongNetwork) loadOrderState(orderConfig)

	async function loadOrderState(config: OrderConfig) {
		await checkOrderExists(config.orderHash)
		await getVaultBalance()
	}

	async function checkOrderExists(orderHash: Hex) {
		isCheckingOrder = true
		try {
			const exists = await readContract({
				address: bridge.orderbookAddress,
				abi: orderbookAbi,
				functionName: 'orderExists',
				args: [orderHash]
			})
			orderExists = exists as boolean
		} catch (e) {
			console.error('Failed to check order exists:', e)
			orderExists = false
		} finally {
			isCheckingOrder = false
		}
	}

	async function getVaultBalance() {
		if (!orderConfig) return

		try {
			const balance = await readContract({
				address: bridge.orderbookAddress,
				abi: orderbookAbi,
				functionName: 'vaultBalance',
				args: [orderConfig.owner, orderConfig.outputToken, orderConfig.outputVaultId]
			})
			vaultBalance = balance as bigint
		} catch (e) {
			console.error('Failed to get vault balance:', e)
			vaultBalance = undefined
		}
	}

	$: coupon = signedContext ? parseCoupon(signedContext) : undefined

	let isLoading = false
	let error = ''
	let errorUnconfirmed = false
	let success = false
	let successTxHash = ''

	const handleClaim = async () => {
		if (!signedContext) return
		if (!orderConfig) return

		error = ''
		success = false
		isLoading = true

		try {
			const hash = await claimCoupon(signedContext)
			success = true
			successTxHash = hash
			await getVaultBalance()
		} catch (e) {
			error = errorMessage(e, 'Claim failed')
			errorUnconfirmed = (e as { unconfirmed?: boolean }).unconfirmed === true
		} finally {
			isLoading = false
		}
	}

	function truncateAddress(addr: string): string {
		return `${addr.slice(0, 10)}...${addr.slice(-8)}`
	}

	let showConnectModal = false
	async function handleConnect() {
		showConnectModal = true
		await connectWallet()
		showConnectModal = false
	}
</script>

<Card size="xl" class="flex flex-col gap-4">
	{#if success}
		<TransactionReceipt
			title="Claim Successful!"
			message="Your {bridge.tokenName} tokens have been transferred to your wallet."
			hash={successTxHash}
		>
			{#if coupon && orderConfig}
				<span class="text-gray-600">Amount:</span>
				<span class="font-semibold">
					{formatUnits(coupon.withdrawAmount, orderConfig.outputDecimals)}
					{bridge.tokenName}
				</span>

				<span class="text-gray-600">Recipient:</span>
				<a
					class="font-mono text-blue-600 hover:underline"
					href={explorerAddress(coupon.recipient)}
					target="_blank"
				>
					{truncateAddress(coupon.recipient)}
				</a>
			{/if}
		</TransactionReceipt>
	{:else}
		<h1 class="text-2xl font-bold">Claim {bridge.tokenName}</h1>
		<p class="text-gray-600">
			Redeem your {bridge.tokenName} claim coupon to receive {bridge.tokenName} tokens on Ethereum.
		</p>

		{#if !isConnected}
			<Alert color="blue">Please connect your wallet to continue.</Alert>
			<Button on:click={handleConnect}>Connect Wallet</Button>
		{:else if $onWrongNetwork}
			<WrongNetwork />
		{:else}
			<div class="space-y-4">
				<div>
					<Label for="coupon" class="mb-2">Claim Coupon</Label>
					{#if couponPrefilledFromUrl}
						<div
							class="block w-full cursor-default select-none break-all rounded-lg border border-gray-300 bg-gray-50 p-2.5 text-sm text-gray-900 dark:border-gray-600 dark:bg-gray-700 dark:text-white"
							style="user-select: none; -webkit-user-select: none;"
							aria-readonly="true"
						>
							{couponInput}
						</div>
					{:else}
						<Input
							id="coupon"
							type="text"
							placeholder="Paste your coupon code here..."
							bind:value={couponInput}
							on:input={parseCouponInput}
							disabled={isLoading}
						/>
					{/if}
				</div>

				{#if isCheckingOrder}
					<div class="flex items-center justify-center py-8">
						<Spinner size="16" />
					</div>
				{:else if coupon && orderConfig && orderExists}
					<div class="space-y-2 rounded-lg bg-gray-50 p-4">
						<h3 class="mb-2 font-semibold">Coupon Details</h3>

						<div class="grid grid-cols-2 gap-2 text-sm">
							<span class="text-gray-600">Recipient:</span>
							<a
								class="font-mono text-blue-600 hover:underline"
								href={explorerAddress(coupon.recipient)}
								target="_blank"
							>
								{truncateAddress(coupon.recipient)}
							</a>

							<span class="text-gray-600">Amount:</span>
							<span class="font-semibold">
								{formatUnits(coupon.withdrawAmount, orderConfig.outputDecimals)}
								{bridge.tokenName}
							</span>

							<span class="text-gray-600">Expires:</span>
							<span
								class={new Date(coupon.expiryTimestamp * 1000) < new Date() ? 'text-red-500' : ''}
							>
								{new Date(coupon.expiryTimestamp * 1000).toLocaleString()}
							</span>
						</div>
					</div>

					{#if vaultBalance !== undefined}
						<div class="text-sm text-gray-600">
							Vault Balance: {formatUnits(vaultBalance, orderConfig.outputDecimals)}
							{bridge.tokenName}
						</div>
					{/if}

					{#if error}
						<Alert color={errorUnconfirmed ? 'yellow' : 'red'} class="break-words">{error}</Alert>
					{/if}

					<Button
						class="w-fit"
						on:click={handleClaim}
						disabled={isLoading || success || !signedContext || !orderConfig}
					>
						{#if isLoading}
							<Spinner size="4" class="mr-2" />
						{/if}
						Claim {bridge.tokenName}
					</Button>
				{:else if couponInput && !coupon}
					<Alert color="red">Invalid coupon format. Please check and try again.</Alert>
				{:else if coupon && !orderConfig}
					<Alert color="yellow">Unknown order. This coupon is for an unrecognized order.</Alert>
				{:else if coupon && orderConfig && !orderExists}
					<Alert color="yellow">Order not found on-chain. The order may have been removed.</Alert>
				{:else}
					<Alert color="blue">
						Enter your claim coupon above. You should have received this after burning {bridge.tokenName}.
					</Alert>
				{/if}
			</div>
		{/if}
	{/if}
</Card>

<TransactionModal />
<ConnectWalletModal open={showConnectModal} />
