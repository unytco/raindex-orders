<script lang="ts">
	import { 
		defaultConfig, 
		WC, 
		connected,
		signerAddress,
		configuredConnectors,
	} from 'svelte-wagmi';
	import { createQuery } from '@tanstack/svelte-query';
	import { getOrders } from '$lib/queries/getOrders';
	import { onMount } from 'svelte';

	import {
		PUBLIC_WALLETCONNECT_ID,
		PUBLIC_ALCHEMY_ID,
		PUBLIC_SUBGRAPH_URL
	} from '$env/static/public';
	import { Button, Card, Badge, Indicator } from 'flowbite-svelte';
	import { orderbookAbi } from '../generated';
	import { writeContract } from '@wagmi/core';
	import { sepolia } from 'viem/chains';
	import { createWalletClient, http, parseEther, encodePacked, keccak256} from 'viem'
	import { privateKeyToAccount, generatePrivateKey } from 'viem/accounts'
	import { createConfig } from '@wagmi/core';
	import { injected } from 'wagmi/connectors'


	const ORDER_HASH = "0x20d5f8aeaf824361c7d3dd2c7daf8f71ea3e1d0aef7393a8628d66ace63b509c";
	const ORDERBOOK = '0xfca89cD12Ba1346b1ac570ed988AB43b812733fe';
	const CLAIM_TOKEN = '0x72bBeF0c3d23C196D324cF7cF59C083760fFae5b';
	const OUTPUT_VAULT_ID = '0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b';

	type Order = {
		owner: string;
		handleIo: boolean;
		evaluable: {
			interpreter: string;
			store: string;
			expression: string;
		};
		validInputs: Array<{
			token: string;
			decimals: string;
			vaultId: string;
		}>;
		validOutputs: Array<{
			token: string;
			decimals: string;
			vaultId: string;
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
	 */
	$: console.log('connectors', $configuredConnectors);

	$: orderJSONString = $query?.data?.order?.orderJSONString;
	$: order = orderJSONString ? (JSON.parse(orderJSONString) as Order) : undefined;
	
	onMount(async () => {
		const erckit = defaultConfig({
			chains: [sepolia],
			appName: 'erc.kit',
			walletConnectProjectId: PUBLIC_WALLETCONNECT_ID,
			alchemyId: PUBLIC_ALCHEMY_ID,
			connectors: [injected()]
		});
		
		erckit.init();
	});

	$: query = createQuery({
		queryKey: ['orders', getOrders, ORDER_HASH, PUBLIC_SUBGRAPH_URL],
		queryFn: () => getOrders(ORDER_HASH, PUBLIC_SUBGRAPH_URL)
	});

	const handleClaim = async () => {
		if (order) {
			const signedContext = await getCoupon();

			const takeOrderConfig: TakeOrderConfigStruct = {
				order: order,
				inputIOIndex: 0,
				outputIOIndex: 0,
				signedContext: [signedContext]
			};

			const takeOrdersConfig: TakeOrdersConfigStruct = {
				minimumInput: signedContext.context[0],
				maximumInput: signedContext.context[0],
				maximumIORatio: 0,
				orders: [takeOrderConfig],
				output: order.validInputs[0].token,
				input: order.validOutputs[0].token
			};

			finalWithdrawAmount = withdrawAmount;

			const result = await writeContract(
				createConfig({
					chains: [sepolia],
					transports: {
						[sepolia.id]: http(),
					},
					connectors: $configuredConnectors,
				}), 
				{
					abi: orderbookAbi,
					address: '0xfca89cD12Ba1346b1ac570ed988AB43b812733fe',
					functionName: 'takeOrders',
					args: [takeOrdersConfig]
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
		 */

		const coupon: [bigint, bigint, bigint, bigint, bigint, bigint, bigint, bigint, bigint] = [
			BigInt($signerAddress as string),
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
			
		const account = privateKeyToAccount('0x8d25755a3b9cea364e1e682f3892699c36a2dad1f087f772b349edca72bf842e')
			
		const signature = await client.signMessage({
			account,
			message: { raw: message },
		})

		const signedContext: SignedContextV1Struct = {
			signer: $signerAddress,
			signature,
			context: coupon
		};

		console.log('result', signedContext);

		return signedContext;

	};
</script>

<Card size="xl">
	{#if $connected}
		<Badge color="green" class="w-fit gap-1"><Indicator color="green" />Connected to Sepolia</Badge>
	{/if}
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
		<Button on:click={async () => await WC('Sign in to the app with Ethereum')()}>Connect</Button>

		<Button on:click={() => handleClaim()}>Claim</Button>
	{/if}
</Card>
