<script lang="ts">
	import '../app.pcss'
	import { onMount } from 'svelte'
	import { QueryClient, QueryClientProvider } from '@tanstack/svelte-query'
	import { initEthereum } from '$lib/ethereum'
	import CommonFooter from '$lib/components/CommonFooter.svelte'

	const queryClient = new QueryClient({
		defaultOptions: {
			queries: {
				refetchOnWindowFocus: false,
				retry: false
			}
		}
	})

	onMount(() => {
		initEthereum()
	})
</script>

<QueryClientProvider client={queryClient}>
	<main class="m-12 flex flex-col items-center justify-center">
		<slot />
		<CommonFooter />
	</main>
</QueryClientProvider>
