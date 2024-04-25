<script lang="ts">
	import { Button, Modal, Spinner } from 'flowbite-svelte'
	import { transactionStore, TransactionStatus } from '$lib/stores/transactionStore'

	function unescapeString(str) {
		return str
			.replace(/\\n/g, '\n')
			.replace(/\\'/g, "'")
			.replace(/\\"/g, '"')
			.replace(/\\\\/g, '\\')
	}
</script>

<Modal
	on:close={() => transactionStore.reset()}
	open={$transactionStore.status !== TransactionStatus.IDLE}
>
	<div class="p-4">
		<div class="flex flex-col items-center justify-center gap-2">
			{#if $transactionStore.status === TransactionStatus.PENDING_WALLET}
				<Spinner size="10" color="blue" />
				{$transactionStore.status}
			{/if}
			{#if $transactionStore.status === TransactionStatus.PENDING_TX}
				<Spinner size="10" color="green" />
				{$transactionStore.status}
				<a
					class="font-blue-500 hover:underline"
					href={`https://sepolia.etherscan.io/tx/${$transactionStore.hash}`}
					target="_blank">View pending transaction on Etherscan</a
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
					{unescapeString($transactionStore.error.message)}
				</div>
				<Button on:click={() => transactionStore.reset()}>Close</Button>
			{/if}
		</div>
	</div>
</Modal>
