<script lang="ts">
	import { Button, Card, FloatingLabelInput } from 'flowbite-svelte'
	import { PUBLIC_SUBGRAPH_URL } from '$env/static/public'
	import { getOrders } from '$lib/queries/getOrders'
	import type { Order } from '$lib/types'
	import { createQuery } from '@tanstack/svelte-query'
	import { generatePrivateKey } from 'viem/accounts'
	import { generateSignedContext, type CouponConfig, serializeSignedContext } from '$lib/coupon'
	import { parseUnits, isAddress, isHash, isHex, type Hex } from 'viem'
	import { PUBLIC_ORDERBOOK_ADDRESS } from '$env/static/public'

	let claimAmount = 0
	let recipient = ''
	let orderHash: Hex = ''
	let expiryHours = 24
	let couponUrl = ''

	let coupon: CouponConfig

	$: query = createQuery({
		queryKey: ['orders', getOrders, orderHash, PUBLIC_SUBGRAPH_URL],
		queryFn: () => getOrders(orderHash, PUBLIC_SUBGRAPH_URL)
	})
	$: orderJSONString = $query?.data?.order?.orderJSONString

	const handleGenerateCoupon = async () => {
		const order = JSON.parse(orderJSONString) as Order

		if (!isAddress(recipient)) throw new Error('Invalid recipient address')
		if (!isHash(orderHash)) throw new Error('Invalid order hash')
		if (!isAddress(PUBLIC_ORDERBOOK_ADDRESS)) throw new Error('Invalid orderbook address')
		if (!isHex(order.validOutputs[0].vaultId)) throw new Error('Invalid output vault ID')

		coupon = {
			recipient,
			orderHash,
			orderbookAddress: PUBLIC_ORDERBOOK_ADDRESS,
			claimTokenAddress: order.validOutputs[0].token,
			outputVaultId: order.validOutputs[0].vaultId,
			withdrawAmount: parseUnits(claimAmount.toString(), order.validOutputs[0].decimals),
			orderOwner: order.owner,
			nonce: BigInt(generatePrivateKey()),
			expiryTimestamp: Math.floor(Date.now() / 1000) + 60 * 60 * expiryHours
		}

		const signedContext = await generateSignedContext(coupon)

		// generate a url on the root of this domain with the signed context as a query parameter
		// there is probably a better encoding method for this
		const url = new URL(window.location.origin)
		url.searchParams.set('c', serializeSignedContext(signedContext))

		navigator.clipboard.writeText(url.toString())
		couponUrl = url.toString()
	}

	$: ready = orderJSONString && isAddress(recipient) && claimAmount > 0 && expiryHours > 0
</script>

<Card size="xl" class="flex flex-col gap-4">
	<FloatingLabelInput style="outlined" bind:value={orderHash} type="text">
		Order hash
	</FloatingLabelInput>
	<div>
		{#if orderJSONString}
			<pre>{JSON.stringify(JSON.parse(orderJSONString), null, 2)}</pre>
		{:else}
			<p>No order found</p>
		{/if}
	</div>
	<FloatingLabelInput style="outlined" bind:value={claimAmount} type="number">
		Claim amount
	</FloatingLabelInput>
	<div>Current vault balance: {$query?.data?.order?.validOutputs[0].tokenVault.balanceDisplay}</div>
	<FloatingLabelInput style="outlined" bind:value={recipient} type="text">
		Recipient
	</FloatingLabelInput>
	<FloatingLabelInput style="outlined" bind:value={expiryHours} type="text">
		Expiry (hours)
	</FloatingLabelInput>
	<Button disabled={!ready} on:click={handleGenerateCoupon}>Generate coupon</Button>
	{#if couponUrl}
		<span>Copied to clipboard</span>
		<span class="truncate">{couponUrl}</span>
	{/if}
	{#if coupon}
		<pre>
			{JSON.stringify(
				coupon,
				(key, value) => (typeof value === 'bigint' ? value.toString() : value), // return everything else unchanged
				2
			)}
		</pre>
	{/if}
</Card>
