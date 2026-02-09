import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vitest/config';

export default defineConfig({
	plugins: [sveltekit()],
	server: {
		allowedHosts: ['.unyt.dev', '.unyt.co', 'hot-bridge.unyt.dev'],
		host: true
	},
	test: {
		include: ['src/**/*.{test,spec}.{js,ts}']
	}
});
