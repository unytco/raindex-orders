<script lang="ts">
	import { defaultConfig, WC } from 'svelte-wagmi';
	import {
		connected,
		chainId,
		signerAddress,
		web3Modal,
		wagmiLoaded,
		configuredConnectors
	} from 'svelte-wagmi';
	import { createQuery } from '@tanstack/svelte-query';
	import { getOrders } from '$lib/queries/getOrders';
	import { onMount } from 'svelte';

	import {
		PUBLIC_WALLETCONNECT_ID,
		PUBLIC_ALCHEMY_ID,
		PUBLIC_SUBGRAPH_URL
	} from '$env/static/public';
	import { injected } from '@wagmi/connectors';
	import { Button, Card, Badge, Indicator } from 'flowbite-svelte';
	import { orderbook, queries } from '@rainprotocol/orderbook-components';
	import * as ethers from 'ethers';
	import { useWriteOrderbookTakeOrders, orderbookAbi } from '../generated';
	import { writeContract } from '@wagmi/core';
	import { signMessage } from '@wagmi/core';
	import { configStore } from '$lib/stores';
	import { createWalletClient, http } from 'viem';
	import { mainnet, sepolia } from 'viem/chains';

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
	let withdrawAmount: number;
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

	$: orderJSONString = $query?.data?.orders[0]?.orderJSONString;
	$: order = orderJSONString ? (JSON.parse(orderJSONString) as OrderStruct) : undefined;

	let config;
	let parsedOrders: Order[] = [];
	onMount(async () => {
		const erckit = defaultConfig({
			chains: [sepolia],
			appName: 'erc.kit',
			walletConnectProjectId: PUBLIC_WALLETCONNECT_ID,
			alchemyId: PUBLIC_ALCHEMY_ID,
			connectors: [injected()]
		});
		config = erckit;
		erckit.init();
	});

	const query = createQuery({
		queryKey: ['orders'],
		queryFn: () => getOrders(PUBLIC_SUBGRAPH_URL)
	});

	$: if ($query.data) {
		console.log($query.data);
		$query.data.orders.map((order) => {
			console.log(JSON.parse(order.orderJSONString));
			parsedOrders = [...parsedOrders, JSON.parse(order.orderJSONString)];
		});
	}

	$: console.log(parsedOrders);

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
				minimumInput: signedContext.context[0],
				maximumInput: signedContext.context[0],
				maximumIORatio: 0,
				orders: [takeOrderConfig],
				output: order.validInputs[0].token,
				input: order.validOutputs[0].token
			};

			finalWithdrawAmount = withdrawAmount;

			const result = await writeContract(config, {
				abi: orderbookAbi,
				address: '0xfca89cD12Ba1346b1ac570ed988AB43b812733fe',
				functionName: 'takeOrders',
				args: [takeOrdersConfig]
			});
		}
	};

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

	$: recipientAddress = $signerAddress;
	$: claimAmount = ethers.parseEther('1');
	$: expiry = 2687375409;
	$: orderHash = parsedOrders[0]?.id;
	$: orderOwner = parsedOrders[0]?.owner;
	$: orderbookAddress = '0xfca89cD12Ba1346b1ac570ed988AB43b812733fe';
	$: tokenAddress = '0x72bBeF0c3d23C196D324cF7cF59C083760fFae5b';
	$: outputVaultId = '0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b';

	export const getCoupon = async (
		token: `0x${string}`,
		expectedOrderHash: `0x${string}`
	): Promise<SignedContextV1Struct> => {
		const coupon = {
			recipientAddress,
			claimAmount,
			expiry: 2687375409,
			orderHash,
			orderOwner,
			orderbookAddress,
			tokenAddress,
			outputVaultId
		};

		const context = [
			coupon.recipientAddress,
			BigInt(coupon.claimAmount),
			BigInt(coupon.expiry),
			coupon.orderHash,
			coupon.orderOwner,
			coupon.orderbookAddress,
			coupon.tokenAddress,
			coupon.outputVaultId
		];

		const signer = new ethers.Wallet(
			'0x8d25755a3b9cea364e1e682f3892699c36a2dad1f087f772b349edca72bf842e'
		);

		const result = await signMessage(config, {
			message: 'hello world'
		});

		// const signature = await signer.signMessage(
		// 	ethers.arrayify(
		// 		ethers.solidityKeccak256(
		// 			['uint256', 'uint256', 'uint256', 'uint256', 'uint256', 'uint256', 'uint256'],
		// 			context
		// 		)
		// 	)
		// );

		// const signedContext: SignedContextV1Struct = {
		// 	signer: signer.address,
		// 	signature,
		// 	context
		// };

		return console.log('result', result);
	};
</script>

<Card size="xl">
	{#if $connected}
		<Badge color="green" class="w-fit gap-1"><Indicator color="green" />Connected to Sepolia</Badge>
		<div>
			<!-- Add the form entries -->
		</div>
	{:else if $web3Modal}
		<Button class="w-fit" on:click={() => $web3Modal.open()}>Connect to Ethereum</Button>
	{:else}
		<p>Web3Modal not yet available</p>
	{/if}
	{#if $query.data}
		{#each parsedOrders as order}
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
		{/each}
		<Button on:click={async () => await WC('Sign in to the app with Ethereum')()}>Connect</Button>

		<Button on:click={() => handleClaim()}>Claim</Button>
	{/if}
</Card>
