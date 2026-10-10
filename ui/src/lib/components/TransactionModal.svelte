<script context="module" lang="ts">
	export const LOCK_FINALIZE_LINE =
		'Go back to your Unyt app now to finalize. You can close this window.'
</script>

<script lang="ts">
	import { Button, Modal, Spinner } from 'flowbite-svelte'
	import { transactionStore, TransactionStatus } from '$lib/stores/transactionStore'
	import { explorerTx } from '$lib/config'

	$: isLockSuccess =
		$transactionStore.status === TransactionStatus.SUCCESS && $transactionStore.isLockTransaction

	$: isPending =
		$transactionStore.status === TransactionStatus.PENDING_WALLET ||
		$transactionStore.status === TransactionStatus.PENDING_TX
</script>

<Modal
	on:close={() => {
		if (!isPending) transactionStore.reset()
	}}
	open={$transactionStore.status !== TransactionStatus.IDLE}
	dismissable={!isPending}
	placement="center"
>
	<div class="p-4">
		<div class="flex flex-col items-center justify-center gap-2">
			{#if $transactionStore.status === TransactionStatus.PENDING_WALLET}
				<Spinner size="10" color="blue" />
				<div class="text-center">
					<p>{$transactionStore.status}</p>
					<p class="mt-2 text-sm text-gray-600 dark:text-gray-400">
						Check your wallet (e.g. MetaMask) and approve or sign the transaction if prompted.
					</p>
				</div>
			{/if}
			{#if $transactionStore.status === TransactionStatus.PENDING_TX}
				<Spinner size="10" color="green" />
				<div class="text-center">
					<p>{$transactionStore.status}</p>
					<p class="mt-2 text-sm text-gray-600 dark:text-gray-400">
						Check your wallet if needed. The transaction is being confirmed on-chain.
					</p>
				</div>
				<a
					class="text-blue-500 hover:underline"
					href={explorerTx($transactionStore.hash)}
					target="_blank">View pending transaction on Etherscan</a
				>
			{/if}

			{#if $transactionStore.status === TransactionStatus.SUCCESS}
				<div
					class="mb-4 flex h-16 w-16 items-center justify-center rounded-full bg-green-100 dark:bg-green-900"
				>
					<h1 class="text-2xl">✅</h1>
				</div>
				{#if isLockSuccess}
					<p class="text-lg font-semibold">Lock almost complete</p>
					<p class="text-sm text-gray-600 dark:text-gray-400">{LOCK_FINALIZE_LINE}</p>
				{:else}
					{$transactionStore.status}
				{/if}
				<a
					class="text-blue-500 hover:underline"
					href={explorerTx($transactionStore.hash)}
					target="_blank">View transaction on Etherscan</a
				>
			{/if}

			{#if $transactionStore.status === TransactionStatus.ERROR}
				<div
					class="mb-2 flex h-16 w-16 items-center justify-center rounded-full bg-green-100 dark:bg-green-900"
				>
					<h1 class="text-2xl">❌</h1>
				</div>
				<p class="w-full break-words text-center">{$transactionStore.error.message}</p>
				{#if $transactionStore.hash}
					<a
						class="text-blue-500 hover:underline"
						href={explorerTx($transactionStore.hash)}
						target="_blank">View transaction on Etherscan</a
					>
				{/if}
				<Button on:click={() => transactionStore.reset()}>Close</Button>
			{/if}
		</div>
	</div>
</Modal>
