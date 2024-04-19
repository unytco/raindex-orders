<script lang="ts">
	import { getCoupon } from '$lib/getCoupon';
	import { TransactionStatus } from '$lib/types.ts';
	import { createQuery } from '@tanstack/svelte-query';
	import { getOrders } from '$lib/queries/getOrders';
	import { PUBLIC_SUBGRAPH_URL } from '$env/static/public';
	import { Button, Card, Badge, Indicator, Input, Modal, Spinner, Label } from 'flowbite-svelte';
	import { orderbookAbi } from '../generated';
	import {
		getAccount,
		switchChain,
		watchAccount,
		writeContract,
		waitForTransaction
	} from '@wagmi/core';
	import { sepolia } from 'viem/chains';
	import { createWalletClient, http, parseEther, encodePacked, keccak256, type Hex } from 'viem';
	import { privateKeyToAccount, generatePrivateKey } from 'viem/accounts';
	import { onMount } from 'svelte';
	import transactionStore from '$lib/stores/transactionStore';
	import { formatDate, truncateEthAddress } from '$lib/utils';

	import { getContext } from 'svelte';
	const web3ContextKey = 'web3Context';
	const { config, modal } = getContext(web3ContextKey);

	const ORDER_HASH = '0x20d5f8aeaf824361c7d3dd2c7daf8f71ea3e1d0aef7393a8628d66ace63b509c';
	const ORDERBOOK = '0xfca89cD12Ba1346b1ac570ed988AB43b812733fe';
	const CLAIM_TOKEN = '0x72bBeF0c3d23C196D324cF7cF59C083760fFae5b';
	const OUTPUT_VAULT_ID = '0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b';

	export type Order = {
		owner: Hex;
		handleIO: boolean;
		evaluable: {
			interpreter: Hex;
			store: Hex;
			expression: Hex;
		};
		validInputs: Array<{
			token: Hex;
			decimals: number;
			vaultId: bigint;
		}>;
		validOutputs: Array<{
			token: Hex;
			decimals: number;
			vaultId: bigint;
		}>;
	};

	enum ClaimStep {
		None,
		WaitingOnConfirmation,
		Claiming,
		Claimed,
		Error
	}

	let claimStep: ClaimStep = ClaimStep.None;
	let withdrawAmount: number = 10;
	let finalWithdrawAmount: number;
	let receipt;
	$: parsedData = JSON.parse(orderJSONString || '{}');

	$: query = createQuery({
		queryKey: ['orders', getOrders, ORDER_HASH, PUBLIC_SUBGRAPH_URL],
		queryFn: () => getOrders(ORDER_HASH, PUBLIC_SUBGRAPH_URL)
	});

	$: orderJSONString = $query?.data?.order?.orderJSONString;
	$: order = orderJSONString ? (JSON.parse(orderJSONString) as Order) : undefined;
	$: order ? (order = { ...order, handleIO: order.handleIo }) : undefined;

	let fullContext;
	let signedContext;

	$: if ($query.data) {
		getCouponInfo();
	}

	const getCouponInfo = async () => {
		if (order) {
			fullContext = await getCoupon({
				withdrawAmount,
				ORDER_HASH,
				ORDERBOOK,
				CLAIM_TOKEN,
				OUTPUT_VAULT_ID,
				order
			});
		}
	};

	const handleClaim = async () => {
		let hash;
		if (order) {
			const signedContext = fullContext.signedContext;

			const takeOrderConfig = {
				order: order,
				inputIOIndex: BigInt(0),
				outputIOIndex: BigInt(0),
				signedContext: [signedContext]
			};

			const takeOrdersConfig = {
				minimumInput: signedContext.context[1],
				maximumInput: signedContext.context[1],
				maximumIORatio: BigInt(0),
				orders: [takeOrderConfig],
				data: '' as Hex
			};

			finalWithdrawAmount = withdrawAmount;

			transactionStore.awaitWalletConfirmation();
			try {
				hash = await writeContract(config, {
					abi: orderbookAbi,
					address: '0xfca89cD12Ba1346b1ac570ed988AB43b812733fe',
					functionName: 'takeOrders',
					args: [takeOrdersConfig],
					chainId: sepolia.id
				});
			} catch {
				transactionStore.transactionError('User denied transaction.');
			}

			if (hash) {
				transactionStore.awaitTxReceipt(hash);
				const transactionReceipt = await waitForTransaction(config, { hash });
				if (transactionReceipt) {
					transactionStore.transactionSuccess(transactionReceipt);
				}
			} else {
				transactionStore.transactionError('User denied transaction.');
			}

		}
	};


</script>

<Card size="xl" class="flex flex-col  gap-4">
	{#if $query.isFetching || $query.isLoading || $query.isRefetching}
		<div class="items-center justify-center self-center">
			<Spinner size="16" />
		</div>
	{:else if $query.data && fullContext}
		<h1 class="text-2xl">Order</h1>
		<div>
			{#each Object.entries(fullContext.renderedValues) as [key, value]}
				{#if key === 'Recipient Address'}
					<p>
						{key}:
						<a
							class="font-semibold hover:underline"
							href={`https://sepolia.etherscan.io/address/${value}`}>{truncateEthAddress(value)}</a
						>
					</p>
				{:else}
					<p>{key}: <span class="font-semibold">{value}</span></p>
				{/if}
			{/each}
		</div>
		<!-- <p class="truncate">{JSON.stringify($query.data, null, 2)}</p> -->

		<div class="flex flex-row gap-2">
			<Button disabl class="w-fit" on:click={() => modal.open()}>Connect</Button>
			<Button class="w-fit" on:click={() => handleClaim()}>Claim</Button>
		</div>
	{/if}
</Card>

<Modal
	on:close={() => transactionStore.reset()}
	open={$transactionStore.status !== TransactionStatus.IDLE}
>
	<div class="p-4">
		<div class="flex flex-col items-center justify-center gap-2">
			{#if $transactionStore.status === TransactionStatus.PENDING_WALLET}
				<Spinner size="10" color="orange" />
				{$transactionStore.status}
			{/if}
			{#if $transactionStore.status === TransactionStatus.PENDING_TX}
				<Spinner size="10" color="green" />
				{$transactionStore.status}
				<a
					class="font-blue-500 hover:underline"
					href={`https://sepolia.etherscan.io/tx/${$transactionStore.hash}`}
					target="_blank">View transaction on Etherscan</a
				>
			{/if}

			{#if $transactionStore.status === TransactionStatus.SUCCESS}
				<div
					class="mb-4 flex h-16 w-16 items-center justify-center rounded-full bg-green-100 dark:bg-green-900"
				>
					<h1 class="text-2xl">✅</h1>
				</div>
				{$transactionStore.status}
				<a
					class="font-blue-500 hover:underline"
					href={`https://sepolia.etherscan.io/tx/${$transactionStore.hash}`}
					target="_blank">View transaction on Etherscan</a
				>
				<Button on:click={() => transactionStore.reset()}>Close</Button>
			{/if}

			{#if $transactionStore.status === TransactionStatus.ERROR}
				<div
					class="mb-2 flex h-16 w-16 items-center justify-center rounded-full bg-green-100 dark:bg-green-900"
				>
					<h1 class="text-2xl">❌</h1>
				</div>
				<div class="flex flex-col">
					{$transactionStore.error}
				</div>
				<Button on:click={() => transactionStore.reset()}>Close</Button>
			{/if}
		</div>
	</div>
</Modal>
