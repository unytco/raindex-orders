<script lang="ts">
	import { Card } from 'flowbite-svelte'
	import CopyableUrl from './CopyableUrl.svelte'
	import SepoliaFaucetLink from './SepoliaFaucetLink.svelte'
	import HotFaucetLink from './HotFaucetLink.svelte'
</script>

<aside class="w-full lg:w-72">
	<h2 class="mb-4 text-sm font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">
		Quick links
	</h2>
	<div class="flex flex-col gap-4">
		<Card class="flex flex-col">
			<h3 class="text-base font-semibold text-gray-900 dark:text-white">Copy this page URL</h3>
			<p class="mb-3 text-sm text-gray-600 dark:text-gray-400">
				Click copy to copy the URL to a different browser.
			</p>
			<div class="mt-auto">
				<CopyableUrl />
			</div>
		</Card>

		<a href="https://cloud.google.com/application/web3/faucet/ethereum/sepolia" target="_blank" rel="noopener noreferrer" class="block">
			<Card class="h-full transition-colors hover:bg-gray-50 dark:hover:bg-gray-800">
				<h3 class="text-base font-semibold text-gray-900 dark:text-white">Get Test ETH (Sepolia)</h3>
				<p class="text-sm text-gray-600 dark:text-gray-400">
					Use the Sepolia faucet to obtain test ETH for gas on the Sepolia network.
				</p>
				<p class="mt-2 text-sm font-medium text-blue-600 dark:text-blue-400">Go to Sepolia Faucet →</p>
			</Card>
		</a>

		<a href="/faucet" target="_blank" rel="noopener noreferrer" class="block">
			<Card class="h-full transition-colors hover:bg-gray-50 dark:hover:bg-gray-800">
				<h3 class="text-base font-semibold text-gray-900 dark:text-white">Get mock HOT</h3>
				<p class="text-sm text-gray-600 dark:text-gray-400">
					Use the HOT faucet to obtain mock HOT tokens for testing the bridge.
				</p>
				<p class="mt-2 text-sm font-medium text-blue-600 dark:text-blue-400">Go to HOT Faucet →</p>
			</Card>
		</a>
	</div>
</aside>
