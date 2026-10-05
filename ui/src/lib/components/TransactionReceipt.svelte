<script lang="ts">
	export let title: string
	export let message: string
	export let hash: string
</script>

<div class="flex flex-col items-center justify-center gap-4 py-8">
	<div
		class="flex h-20 w-20 items-center justify-center rounded-full bg-green-100 dark:bg-green-900"
	>
		<span class="text-4xl">✅</span>
	</div>
	<h1 class="text-2xl font-bold">{title}</h1>
	<p class="text-center text-gray-600">{message}</p>

	<div class="w-full max-w-md space-y-2 rounded-lg bg-gray-50 p-4">
		<div class="grid grid-cols-2 gap-2 text-sm">
			<slot />
		</div>
	</div>

	<a
		class="text-blue-500 hover:underline"
		href={`https://sepolia.etherscan.io/tx/${hash}`}
		target="_blank"
	>
		View transaction on Etherscan
	</a>
</div>
