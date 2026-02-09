<script lang="ts">
	import { page } from '$app/stores'
	import { Button } from 'flowbite-svelte'

	$: currentUrl = $page?.url?.href ?? ''
	let copied = false
	let copyTimeout: ReturnType<typeof setTimeout>

	async function copyUrl() {
		try {
			await navigator.clipboard.writeText(currentUrl)
			copied = true
			clearTimeout(copyTimeout)
			copyTimeout = setTimeout(() => (copied = false), 2000)
		} catch {
			// fallback for older browsers
			copied = false
		}
	}
</script>

<div class="flex flex-wrap items-center gap-2">
	<code
		class="max-w-[min(100%,20rem)] truncate rounded bg-gray-100 px-2 py-1 text-sm text-gray-700 dark:bg-gray-700 dark:text-gray-300"
		title={currentUrl}
	>{currentUrl}</code>
	<Button size="xs" color="light" on:click={copyUrl}>
		{copied ? 'Copied!' : 'Copy URL'}
	</Button>
</div>
