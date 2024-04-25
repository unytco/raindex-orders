<script lang="ts">
	import { Button, Card, Spinner } from 'flowbite-svelte'
	import { orderbookAbi } from '../generated'
	import { writeContract, waitForTransactionReceipt } from '@wagmi/core'
	import { sepolia } from 'viem/chains'
	import { formatUnits, type Hex, isAddress, ContractFunctionExecutionError } from 'viem'
	import { transactionStore } from '$lib/stores/transactionStore'
	import { getOrders } from '$lib/queries/getOrders'
	import { getContext } from 'svelte'
	import TransactionModal from '$lib/components/TransactionModal.svelte'
	import { PUBLIC_ORDERBOOK_ADDRESS, PUBLIC_SUBGRAPH_URL } from '$env/static/public'
	import { createQuery } from '@tanstack/svelte-query'
	import { deserializeSignedContext, parseCoupon } from '$lib/coupon'
	import type { Web3Context } from '$lib/web3modal'

	const web3ContextKey = 'web3Context'
	const { config, modal } = getContext(web3ContextKey) as Web3Context

	// get the context from the url query param "c" and parse it
	const urlParam = new URL(window.location.href).searchParams.get('c')
	$: signedContext = urlParam ? deserializeSignedContext(urlParam) : undefined

	// parse it back to the original coupon so we can render it
	$: coupon = signedContext ? parseCoupon(signedContext) : undefined

	// get the order from the subgraph based on the orderHash from the coupon
	$: query = createQuery({
		queryKey: ['orders', getOrders, PUBLIC_SUBGRAPH_URL],
		queryFn: () => getOrders(coupon?.orderHash || '0x0', PUBLIC_SUBGRAPH_URL)
	})
	$: orderJSONString = $query?.data?.order?.orderJSONString

	const handleClaim = async () => {
		if (!signedContext) return
		if (!orderJSONString) return
		if (!isAddress(PUBLIC_ORDERBOOK_ADDRESS)) return

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

		let hash

		transactionStore.awaitWalletConfirmation()

		try {
			hash = await writeContract(config, {
				abi: orderbookAbi,
				address: PUBLIC_ORDERBOOK_ADDRESS,
				functionName: 'takeOrders',
				args: [takeOrdersConfig],
				chainId: sepolia.id
			})
		} catch (e: unknown) {
			transactionStore.transactionError({
				message:
					e instanceof ContractFunctionExecutionError ? e?.cause.shortMessage : 'Unknown error'
			})
			console.log(e.cause.shortMessage)
			console.error(e)
			return
		}

		transactionStore.awaitTxReceipt(hash)
		const transactionReceipt = await waitForTransactionReceipt(config, { hash })
		if (transactionReceipt) {
			console.log('TX-Receipt', transactionReceipt)
			transactionStore.transactionSuccess(transactionReceipt.transactionHash)
		}
	}
</script>

<Card size="xl" class="flex flex-col gap-4">
	{#if $query.isFetching || $query.isLoading || $query.isRefetching}
		<div class="items-center justify-center self-center">
			<Spinner size="16" />
		</div>
	{:else if $query.data && coupon}
		<h1 class="text-2xl">Claim</h1>
		<div>
			{#each Object.entries(coupon) as [key, value]}
				{#if key === 'recipient'}
					<p>
						{key}:
						<a
							class="font-semibold hover:underline"
							href={`https://sepolia.etherscan.io/address/${value}`}>{value}</a
						>
					</p>
				{:else if key === 'withdrawAmount' && typeof value === 'bigint'}
					<p>
						{key}:
						<span class="font-semibold"
							>{formatUnits(
								value,
								$query?.data?.order?.validOutputs[0].tokenVault.token.decimals
							)}</span
						>
					</p>
				{:else if key === 'expiryTimestamp' && typeof value === 'number'}
					<p>
						{key}:
						<span class="font-semibold">{new Date(value * 1000).toLocaleString()}</span>
					</p>
				{:else}
					<p>{key}: <span class="font-semibold">{value}</span></p>
				{/if}
			{/each}
		</div>

		<div>Vault balance: {$query?.data?.order?.validOutputs[0].tokenVault.balanceDisplay}</div>

		<div class="flex flex-row gap-2">
			<Button class="w-fit" on:click={() => modal.open()}>Connect</Button>
			<Button class="w-fit" on:click={() => handleClaim()}>Claim</Button>
		</div>
	{/if}
</Card>

<TransactionModal />
