<script lang="ts">
	import { createQuery } from '@tanstack/svelte-query';
	import { getOrders } from '$lib/queries/getOrders';
	import {
		PUBLIC_SUBGRAPH_URL
	} from '$env/static/public';
	import { Button, Card, Badge, Indicator } from 'flowbite-svelte';
	import { orderbookAbi } from '../generated';
	import { getAccount, switchChain, watchAccount, writeContract } from '@wagmi/core';
	import { sepolia } from 'viem/chains';
	import { createWalletClient, http, parseEther, encodePacked, keccak256, type Hex} from 'viem'
	import { privateKeyToAccount, generatePrivateKey } from 'viem/accounts'
	import { onMount } from 'svelte';
	import { setupWeb3Modal } from '$lib/web3modal';

	let config, modal;
	onMount(() => {
		({config, modal} = setupWeb3Modal());
	});


	const ORDER_HASH = "0x20d5f8aeaf824361c7d3dd2c7daf8f71ea3e1d0aef7393a8628d66ace63b509c";
	const ORDERBOOK = '0xfca89cD12Ba1346b1ac570ed988AB43b812733fe';
	const CLAIM_TOKEN = '0x72bBeF0c3d23C196D324cF7cF59C083760fFae5b';
	const OUTPUT_VAULT_ID = '0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b';

	type Order = {
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

	$: query = createQuery({
		queryKey: ['orders', getOrders, ORDER_HASH, PUBLIC_SUBGRAPH_URL],
		queryFn: () => getOrders(ORDER_HASH, PUBLIC_SUBGRAPH_URL)
	});
	$: orderJSONString = $query?.data?.order?.orderJSONString;
	$: order = orderJSONString ? (JSON.parse(orderJSONString) as Order) : undefined;
	$: order ? order = {...order, handleIO: order.handleIo} : undefined;
	

	const handleClaim = async () => {
		if (order) {
			const signedContext = await getCoupon();

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
				data: "" as Hex
			};

			finalWithdrawAmount = withdrawAmount;

			console.log({takeOrdersConfig})

			await switchChain(config, { chainId: sepolia.id })

			const result = await writeContract(
				config, 
				{
					abi: orderbookAbi,
					address: '0xfca89cD12Ba1346b1ac570ed988AB43b812733fe',
					functionName: 'takeOrders',
					args: [takeOrdersConfig],
					chainId: sepolia.id
				}
			);
		}
	};

	export const getCoupon = async (): Promise<SignedContextV1Struct> => {

		/**
		 *  Our "coupon" (the SignedContext array) will be:
		 *  [0] recipient address
		 *  [1] amount
		 *  [2] expiry timestamp in seconds
		 *  Plus some domain separators
		 *  [3] order hash
		 *  [4] order owner
		 *  [5] orderbook address
		 *  [6] token address
		 *  [7] output vault id
		 *  [8] nonce
		 */

		const coupon: [bigint, bigint, bigint, bigint, bigint, bigint, bigint, bigint, bigint] = [
			BigInt(getAccount(config).address as string),
			BigInt(parseEther(withdrawAmount.toString())),
			BigInt(2687375409),
			BigInt(ORDER_HASH),
			BigInt(order?.owner || 0),
			BigInt(ORDERBOOK),
			BigInt(CLAIM_TOKEN),
			BigInt(OUTPUT_VAULT_ID),
			BigInt(generatePrivateKey()) // getting a random 32 bytes to use as a nonce
		];

		const message = keccak256(encodePacked(
			['uint256', 'uint256', 'uint256', 'uint256', 'uint256', 'uint256', 'uint256', 'uint256', 'uint256'],
			coupon
		));

		const client = createWalletClient({
			chain: sepolia,
			transport: http()
		})
			
		const account = privateKeyToAccount('0xdcbe53cbf4cbee212fe6339821058f2787c7726ae0684335118cdea2e8adaafd')
			
		const signature = await client.signMessage({
			account,
			message: { raw: message },
		})

		const signedContext = {
			signer: "0x8E72b7568738da52ca3DCd9b24E178127A4E7d37",
			signature,
			context: coupon
		};

		console.log('result', signedContext);

		return signedContext;

	};
</script>

<Card size="xl">
	<!-- {#if $connected}
		<Badge color="green" class="w-fit gap-1"><Indicator color="green" />Connected to Sepolia</Badge>
	{/if} -->
	{#if $query.data}
	<pre>
		{JSON.stringify($query.data, null, 2)}
	</pre>
		<!-- {#each parsedOrders as order}
			<div class="my-4">
				<p>Owner: {order.owner}</p>
				<p>Handle IO: {order.handleIo}</p>
				<p>Interpreter: {order.evaluable.interpreter}</p>
				<p>Store: {order.evaluable.store}</p>
				<p>Expression: {order.evaluable.expression}</p>
				<p>Valid Inputs:</p>
				<ul>
					{#each order.validInputs as input}
						<li>Token: {input.token}</li>
						<li>Decimals: {input.decimals}</li>
						<li>Vault ID: {input.vaultId}</li>
					{/each}
				</ul>
				<p>Valid Outputs:</p>
				<ul>
					{#each order.validOutputs as output}
						<li>Token: {output.token}</li>
						<li>Decimals: {output.decimals}</li>
						<li>Vault ID: {output.vaultId}</li>
					{/each}
				</ul>
			</div>
		{/each} -->
		<Button on:click={() => modal.open()}>Connect</Button>

		<Button on:click={() => handleClaim()}>Claim</Button>
	{/if}
</Card>
