<script>
	import '../app.pcss';
	import { QueryClient, QueryClientProvider } from '@tanstack/svelte-query';

	const queryClient = new QueryClient();
</script>

<QueryClientProvider client={queryClient}>
	<main class="m-12 flex flex-col items-center justify-center">
		<slot></slot>
	</main>
</QueryClientProvider>
