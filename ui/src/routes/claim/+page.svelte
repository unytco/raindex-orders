<script lang="ts">
	import {
		orderbook,
		queries,
		type OrderStruct,
		type TakeOrdersConfigStruct,
		type TakeOrderConfigStruct
	} from '@rainprotocol/orderbook-components';
	import { Button, Heading, Spinner, Card, Input } from 'flowbite-svelte';
	import { getCoupon } from '$lib/getCoupon';
	import ConnectButton from '$lib/connect/ConnectButton.svelte';
	import { signerAddress } from 'svelte-ethers-store';
	import OxitsSymbol from '$lib/svgs/OxitsSymbol.svelte';
	import { fade } from 'svelte/transition';
	import SuccessTick from '$lib/svgs/SuccessTick.svelte';
	import type { ContractReceipt } from 'ethers';
	import { config } from 
	enum ClaimStep {
		None,
		WaitingOnConfirmation,
		Claiming,
		Claimed,
		Error
	}

	let claimStep: ClaimStep = ClaimStep.None;
	let withdrawAmount: number;
	let finalWithdrawAmount: number;
	let receipt: ContractReceipt;

	const orderHash = '0xda5dc19d90667b3ae0a8da4838e66b155901c0bfcab8c062ac3c9b71493eaea7';
	const { result } = queries.queryOrders();
	$: orderJSONString = $result?.data?.find(({ id }) => id === orderHash)?.orderJSONString;
	$: order = orderJSONString ? (JSON.parse(orderJSONString) as OrderStruct) : undefined;

	const handleClaim = async () => {
		if ($orderbook && order) {
			const signedContext = await getCoupon(
				order.validOutputs[0].token as `0x${string}`,
				orderHash
			);
			const takeOrderConfig: TakeOrderConfigStruct = {
				order: order,
				inputIOIndex: 0,
				outputIOIndex: 0,
				signedContext: [signedContext]
			};

			const takeOrdersConfig: TakeOrdersConfigStruct = {
				output: order.validInputs[0].token,
				input: order.validOutputs[0].token,
				minimumInput: signedContext.context[0],
				maximumInput: signedContext.context[0],
				maximumIORatio: 0,
				orders: [takeOrderConfig]
			};

			finalWithdrawAmount = withdrawAmount;

			claimStep = ClaimStep.WaitingOnConfirmation;
			try {
				const tx = await $orderbook.takeOrders(takeOrdersConfig);
				claimStep = ClaimStep.Claiming;
				receipt = await tx.wait();
				claimStep = ClaimStep.Claimed;
			} catch {
				claimStep = ClaimStep.Error;
				return;
			}
		}
	};

	const getWalletDetails = async () => {
		try {
			const response = await fetch('/wallet/get-balance', {
				method: 'GET',
				headers: {
					'Content-Type': 'application/json'
				}
			});
			if (response.ok) {
				const fromEndpoint = await response.json();
				return { data: fromEndpoint };
			} else {
				throw new Error('Error fetching wallet details');
			}
		} catch (error) {
			return { error };
		}
	};
</script>

<div class="mx-auto flex flex-col items-start gap-y-6">
	{#await getWalletDetails()}
		<div class="flex flex-row items-center gap-x-2">
			<Spinner /><span class="text-lg">Getting claimable balance</span>
		</div>
	{:then { data, error }}
		{#if data}
			<div in:fade class="flex max-w-lg flex-col gap-y-4">
				<Heading tag="h4">Withdraw your OXITs to your wallet</Heading>
				{#if claimStep == ClaimStep.None}
					<Card class="flex flex-row items-center">
						<div class="flex shrink-0 flex-col items-start space-y-2">
							<span class="text-xl text-white"
								><OxitsSymbol classes="inline h-[1.5rem]" /> {data.total}</span
							>
							<span class="whitespace-nowrap">Available OXITs</span>
						</div>
						<input
							class="w-0 grow border-0 bg-transparent p-0 text-right text-4xl text-white [appearance:textfield] focus:ring-0 focus:ring-offset-0 [&::-webkit-inner-spin-button]:appearance-none [&::-webkit-outer-spin-button]:appearance-none"
							type="number"
							placeholder="0"
							bind:value={withdrawAmount}
						/>
					</Card>
					{#if !$signerAddress}
						<ConnectButton />
					{:else}
						<Button on:click={handleClaim}>Claim</Button>
					{/if}
				{:else if claimStep == ClaimStep.WaitingOnConfirmation}
					<div class="mt-10 flex flex-col items-center gap-y-3">
						<Spinner size="10" /><span class="text-lg">Waiting on confirmation</span>
					</div>
				{:else if claimStep == ClaimStep.Claiming}
					<div class="mt-10 flex flex-col items-center gap-y-3">
						<Spinner size="10" /><span class="text-lg">Claiming</span>
					</div>
				{:else if claimStep == ClaimStep.Claimed}
					<div class="mt-10 flex flex-col items-center gap-y-3">
						<SuccessTick classes="w-24" />
						<span class="text-2xl"
							><OxitsSymbol classes="inline h-[1.5rem]" />
							{finalWithdrawAmount} successfully withdrawn.</span
						>
						<a
							href={'https://mumbai.polygonscan.com/tx/' + receipt?.transactionHash}
							target="_blank"
							class="underline">View transaction</a
						>
					</div>
				{:else if claimStep == ClaimStep.Error}
					<div class="mt-10 flex flex-col items-center gap-y-3">
						<span class="text-lg">Error</span>
					</div>
				{/if}
			</div>
		{/if}
	{/await}
</div>
