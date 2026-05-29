import adapter from '@sveltejs/adapter-cloudflare';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
	// Consult https://kit.svelte.dev/docs/integrations#preprocessors
	// for more information about preprocessors
	preprocess: vitePreprocess(),

	kit: {
		// adapter-cloudflare: deploy as a Cloudflare Pages build. CF only ever serves the
		// built bundle (no `vite dev`, so no `/@fs/` source/.env exposure — issue #14).
		// Server routes (/api/faucet) run as Pages Functions; secrets come from CF env
		// bindings via $env/dynamic/private, not baked into the build.
		adapter: adapter()
	}
};

export default config;
