<script lang="ts">
	import { Modal, Spinner } from 'flowbite-svelte'

	export let open = false
</script>

<Modal {open} dismissable={false} placement="center">
	<div class="p-4">
		<div class="flex flex-col items-center justify-center gap-4">
			<Spinner size="10" color="blue" />
			<p class="text-lg font-semibold">Connecting Wallet</p>
			<p class="text-sm text-center text-gray-600 dark:text-gray-400">
				Please consult your MetaMask account and finalize the connection instructions.
			</p>
		</div>
	</div>
</Modal>
