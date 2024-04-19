<script>
	import '../app.pcss';
	import { setContext } from 'svelte';
	import { QueryClient, QueryClientProvider } from '@tanstack/svelte-query';
	import { setupWeb3Modal } from '$lib/web3modal';
	import {type Web3Modal } from '@web3modal/wagmi';
	import {type Config} from "@wagmi/core";
	import {Web3Context} '$lib/types'

	const queryClient = new QueryClient();
	let config: Config
	let modal: Web3Modal


	const web3ContextKey = 'web3Context';

	({ config, modal } = setupWeb3Modal());
	setContext(web3ContextKey, { config, modal });
</script>

<QueryClientProvider client={queryClient}>
	<main class="m-12 flex flex-col items-center justify-center">
		<slot />
	</main>
</QueryClientProvider>
