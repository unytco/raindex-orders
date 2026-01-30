<script lang="ts">
	import { Button, Card, Spinner, Alert, Input, Label } from 'flowbite-svelte'
	import { orderbookAbi } from '../../generated'
	import { formatUnits, type Hex, isAddress } from 'viem'
	import { transactionStore } from '$lib/stores/transactionStore'
	import { getOrders } from '$lib/queries/getOrders'
	import TransactionModal from '$lib/components/TransactionModal.svelte'
	import { PUBLIC_ORDERBOOK_ADDRESS, PUBLIC_SUBGRAPH_URL } from '$env/static/public'
	import { createQuery } from '@tanstack/svelte-query'
	import { deserializeSignedContext, parseCoupon, type SignedContextV1Struct } from '$lib/coupon'
	import {
		ethereumStore,
		connectWallet,
		writeContract,
		waitForTransaction
	} from '$lib/ethereum'
	import { onMount } from 'svelte'
	import { browser } from '$app/environment'

	$: isConnected = $ethereumStore.isConnected
	$: account = $ethereumStore.account

	// Coupon input (for manual entry)
	let couponInput = ''
	let signedContext: SignedContextV1Struct | undefined

	// Get coupon from URL on mount
	onMount(() => {
		if (browser) {
			const urlParam = new URL(window.location.href).searchParams.get('c')
			if (urlParam) {
				couponInput = urlParam
				try {
					signedContext = deserializeSignedContext(urlParam)
				} catch (e) {
					console.error('Failed to parse coupon from URL:', e)
				}
			}
		}
	})

	// Parse coupon when input changes
	function parseCouponInput() {
		if (!couponInput) {
			signedContext = undefined
			return
		}
		try {
			signedContext = deserializeSignedContext(couponInput)
		} catch (e) {
			console.error('Failed to parse coupon:', e)
			signedContext = undefined
		}
	}

	// Parse coupon to display
	$: coupon = signedContext ? parseCoupon(signedContext) : undefined

	// Get the order from the subgraph based on the orderHash from the coupon
	$: query = createQuery({
		queryKey: ['orders', coupon?.orderHash || '0x0', PUBLIC_SUBGRAPH_URL],
		queryFn: () => getOrders(coupon?.orderHash || '0x0', PUBLIC_SUBGRAPH_URL),
		enabled: !!coupon?.orderHash
	})
	$: orderJSONString = $query?.data?.order?.orderJSONString

	let isLoading = false
	let error = ''
	let success = false

	const handleClaim = async () => {
		if (!signedContext) return
		if (!orderJSONString) return
		if (!isAddress(PUBLIC_ORDERBOOK_ADDRESS)) return

		error = ''
		success = false
		isLoading = true
		transactionStore.awaitWalletConfirmation()

		try {
			const order = JSON.parse(orderJSONString)
			order.handleIO = order.handleIo
			delete order.handleIo

			const takeOrderConfig = {
				order: order,
				inputIOIndex: BigInt(0),
				outputIOIndex: BigInt(0),
				signedContext: [signedContext]
			}

			const takeOrdersConfig = {
				minimumInput: signedContext.context[1],
				maximumInput: signedContext.context[1],
				maximumIORatio: BigInt(0),
				orders: [takeOrderConfig],
				data: '' as Hex
			}

			const hash = await writeContract({
				address: PUBLIC_ORDERBOOK_ADDRESS,
				abi: orderbookAbi,
				functionName: 'takeOrders',
				args: [takeOrdersConfig]
			})

			transactionStore.awaitTxReceipt(hash)
			const receipt = await waitForTransaction(hash)

			if (receipt) {
				transactionStore.transactionSuccess(hash)
				success = true
			}
		} catch (e: any) {
			error = e?.message || 'Claim failed'
			transactionStore.transactionError({ message: error })
			console.error(e)
		} finally {
			isLoading = false
		}
	}

	function truncateAddress(addr: string): string {
		return `${addr.slice(0, 10)}...${addr.slice(-8)}`
	}
</script>

<Card size="xl" class="flex flex-col gap-4">
	<div class="flex items-center gap-2">
		<a href="/" class="text-blue-600 hover:underline">← Back</a>
	</div>

	<h1 class="text-2xl font-bold">Claim HOT</h1>
	<p class="text-gray-600">
		Redeem your HoloFuel claim coupon to receive HOT tokens on Ethereum.
	</p>

	{#if !isConnected}
		<Alert color="blue"> Please connect your wallet to continue. </Alert>
		<Button on:click={connectWallet}>Connect Wallet</Button>
	{:else}
		<div class="space-y-4">
			<!-- Coupon Input -->
			<div>
				<Label for="coupon" class="mb-2">Claim Coupon</Label>
				<Input
					id="coupon"
					type="text"
					placeholder="Paste your coupon code here..."
					bind:value={couponInput}
					on:input={parseCouponInput}
					disabled={isLoading}
				/>
			</div>

			{#if $query.isFetching || $query.isLoading}
				<div class="flex items-center justify-center py-8">
					<Spinner size="16" />
				</div>
			{:else if coupon && $query.data?.order}
				<!-- Coupon Details -->
				<div class="bg-gray-50 p-4 rounded-lg space-y-2">
					<h3 class="font-semibold mb-2">Coupon Details</h3>

					<div class="grid grid-cols-2 gap-2 text-sm">
						<span class="text-gray-600">Recipient:</span>
						<a
							class="font-mono hover:underline text-blue-600"
							href={`https://sepolia.etherscan.io/address/${coupon.recipient}`}
							target="_blank"
						>
							{truncateAddress(coupon.recipient)}
						</a>

						<span class="text-gray-600">Amount:</span>
						<span class="font-semibold">
							{formatUnits(
								coupon.withdrawAmount,
								$query?.data?.order?.validOutputs[0]?.tokenVault?.token?.decimals || 18
							)}
							{$query?.data?.order?.validOutputs[0]?.tokenVault?.token?.symbol || 'HOT'}
						</span>

						<span class="text-gray-600">Expires:</span>
						<span class={new Date(coupon.expiryTimestamp * 1000) < new Date() ? 'text-red-500' : ''}>
							{new Date(coupon.expiryTimestamp * 1000).toLocaleString()}
						</span>
					</div>
				</div>

				<!-- Vault Balance -->
				<div class="text-sm text-gray-600">
					Vault Balance: {$query?.data?.order?.validOutputs[0]?.tokenVault?.balanceDisplay || '0'}
				</div>

				<!-- Error Display -->
				{#if error}
					<Alert color="red">{error}</Alert>
				{/if}

				<!-- Success Display -->
				{#if success}
					<Alert color="green">
						Claim successful! Your HOT tokens have been transferred to your wallet.
					</Alert>
				{/if}

				<!-- Action Button -->
				<Button
					class="w-fit"
					on:click={handleClaim}
					disabled={isLoading || !signedContext || !orderJSONString}
				>
					{#if isLoading}
						<Spinner size="4" class="mr-2" />
					{/if}
					Claim HOT
				</Button>
			{:else if couponInput && !coupon}
				<Alert color="red">Invalid coupon format. Please check and try again.</Alert>
			{:else if coupon && !$query.data?.order}
				<Alert color="yellow">Order not found. The coupon may be invalid or expired.</Alert>
			{:else}
				<Alert color="blue">
					Enter your claim coupon above. You should have received this after burning HoloFuel.
				</Alert>
			{/if}
		</div>
	{/if}
</Card>

<TransactionModal />
