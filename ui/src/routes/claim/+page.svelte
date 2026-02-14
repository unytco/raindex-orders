<script lang="ts">
	import { Button, Card, Spinner, Alert, Input, Label } from 'flowbite-svelte'
	import { orderbookAbi } from '../../generated'
	import { formatUnits, type Hex, isAddress } from 'viem'
	import { transactionStore } from '$lib/stores/transactionStore'
	import TransactionModal from '$lib/components/TransactionModal.svelte'
	import { PUBLIC_ORDERBOOK_ADDRESS } from '$env/static/public'
	import { deserializeSignedContext, parseCoupon, type SignedContextV1Struct } from '$lib/coupon'
	import { getOrderConfig, buildOrderStruct, type OrderConfig } from '$lib/orderConfig'
	import {
		ethereumStore,
		connectWallet,
		writeContract,
		readContract,
		waitForTransaction
	} from '$lib/ethereum'
	import { onMount } from 'svelte'
	import { browser } from '$app/environment'

	$: isConnected = $ethereumStore.isConnected
	$: account = $ethereumStore.account

	// Coupon input (for manual entry)
	let couponInput = ''
	let couponPrefilledFromUrl = false
	let signedContext: SignedContextV1Struct | undefined

	// Order state (from RPC instead of subgraph)
	let orderConfig: OrderConfig | undefined
	let orderExists = false
	let vaultBalance: bigint | undefined
	let isCheckingOrder = false

	// Get coupon from URL on mount
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

	// Parse coupon when input changes
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

			// Get order config from our hardcoded config
			orderConfig = getOrderConfig(couponData.orderHash)

			if (orderConfig) {
				// Check if order exists on-chain via RPC
				await checkOrderExists(orderConfig.orderHash)
				// Get vault balance via RPC
				await getVaultBalance()
			}
		} catch (e) {
			console.error('Failed to parse coupon:', e)
			signedContext = undefined
			orderConfig = undefined
		}
	}

	// Check if order exists on-chain
	async function checkOrderExists(orderHash: Hex) {
		if (!isAddress(PUBLIC_ORDERBOOK_ADDRESS)) return

		isCheckingOrder = true
		try {
			const exists = await readContract({
				address: PUBLIC_ORDERBOOK_ADDRESS,
				abi: orderbookAbi as any[],
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

	// Get vault balance via RPC
	async function getVaultBalance() {
		if (!orderConfig || !isAddress(PUBLIC_ORDERBOOK_ADDRESS)) return

		try {
			const balance = await readContract({
				address: PUBLIC_ORDERBOOK_ADDRESS,
				abi: orderbookAbi as any[],
				functionName: 'vaultBalance',
				args: [orderConfig.owner, orderConfig.outputToken, orderConfig.outputVaultId]
			})
			vaultBalance = balance as bigint
		} catch (e) {
			console.error('Failed to get vault balance:', e)
			vaultBalance = undefined
		}
	}

	// Parse coupon to display
	$: coupon = signedContext ? parseCoupon(signedContext) : undefined

	let isLoading = false
	let error = ''
	let success = false

	const handleClaim = async () => {
		if (!signedContext) return
		if (!orderConfig) return
		if (!isAddress(PUBLIC_ORDERBOOK_ADDRESS)) return

		error = ''
		success = false
		isLoading = true
		transactionStore.awaitWalletConfirmation()

		try {
			// Build order struct from config
			const order = buildOrderStruct(orderConfig)

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
				abi: orderbookAbi as any[],
				functionName: 'takeOrders',
				args: [takeOrdersConfig]
			})

			transactionStore.awaitTxReceipt(hash)
			const receipt = await waitForTransaction(hash)

			if (receipt) {
				transactionStore.transactionSuccess(hash)
				success = true
				// Refresh vault balance
				await getVaultBalance()
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


	<h1 class="text-2xl font-bold">Claim HOT</h1>
	<p class="text-gray-600">
		Redeem your Mirrored-HOT claim coupon to receive HOT tokens on Ethereum.
	</p>

	{#if !isConnected}
		<Alert color="blue"> Please connect your wallet to continue. </Alert>
		<Button on:click={connectWallet}>Connect Wallet</Button>
	{:else}
		<div class="space-y-4">
			<!-- Coupon Input -->
			<div>
				<Label for="coupon" class="mb-2">Claim Coupon</Label>
				{#if couponPrefilledFromUrl}
					<div
						class="block w-full rounded-lg border border-gray-300 bg-gray-50 p-2.5 text-sm text-gray-900 dark:border-gray-600 dark:bg-gray-700 dark:text-white select-none cursor-default break-all"
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
							{formatUnits(coupon.withdrawAmount, orderConfig.outputDecimals)} HOT
						</span>

						<span class="text-gray-600">Expires:</span>
						<span class={new Date(coupon.expiryTimestamp * 1000) < new Date() ? 'text-red-500' : ''}>
							{new Date(coupon.expiryTimestamp * 1000).toLocaleString()}
						</span>
					</div>
				</div>

				<!-- Vault Balance -->
				{#if vaultBalance !== undefined}
					<div class="text-sm text-gray-600">
						Vault Balance: {formatUnits(vaultBalance, orderConfig.outputDecimals)} HOT
					</div>
				{/if}

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
					disabled={isLoading || !signedContext || !orderConfig}
				>
					{#if isLoading}
						<Spinner size="4" class="mr-2" />
					{/if}
					Claim HOT
				</Button>
			{:else if couponInput && !coupon}
				<Alert color="red">Invalid coupon format. Please check and try again.</Alert>
			{:else if coupon && !orderConfig}
				<Alert color="yellow">Unknown order. This coupon is for an unrecognized order.</Alert>
			{:else if coupon && orderConfig && !orderExists}
				<Alert color="yellow">Order not found on-chain. The order may have been removed.</Alert>
			{:else}
				<Alert color="blue">
					Enter your claim coupon above. You should have received this after burning Mirrored-HOT.
				</Alert>
			{/if}
		</div>
	{/if}
</Card>

<TransactionModal />
