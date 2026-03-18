import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vitest/config';

export default defineConfig({
	plugins: [sveltekit()],
	server: {
		allowedHosts: ['hot-bridge.unyt.dev', 'hot-bridge.unyt.co'],
		host: true
	},
	test: {
		include: ['src/**/*.{test,spec}.{js,ts}']
	}
});
