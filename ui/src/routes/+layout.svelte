<script>
	import { setContext, getContext } from 'svelte';
	import '../app.pcss';
	import { QueryClient, QueryClientProvider } from '@tanstack/svelte-query';
	import { onMount } from 'svelte';
	import { setupWeb3Modal } from '$lib/web3modal';
	const queryClient = new QueryClient();
	let config, modal;


	const web3ContextKey = 'web3Context';

		({ config, modal } =  setupWeb3Modal()); 
		setContext(web3ContextKey, { config, modal });

</script>


<QueryClientProvider client={queryClient}>
	<main class="m-12 flex flex-col items-center justify-center">
		<slot></slot>
	</main>
</QueryClientProvider>