import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vitest/config';

export default defineConfig({
	plugins: [sveltekit()],
	server: {
		allowedHosts: ['hot-bridge.unyt.dev', 'hot-bridge.unyt.co'],
		host: true,
		// Harden Vite's /@fs/ route. In dev mode it can serve ANY file the process can
		// read (e.g. repo-root src/Constants.sol, .env). Deny secret-/key-bearing files
		// so the dev server cannot leak them (issue #14). NOTE: this only applies to
		// `vite dev`/`vite preview`; the proper production fix is to serve a built app.
		fs: {
			strict: true,
			deny: [
				'.env',
				'.env.*',
				'**/.env',
				'**/.env.*',
				'**/.git/**',
				'**/*.sol',
				'**/*.rain',
				'**/*.key',
				'**/*.pem',
				'**/*.hex'
			]
		}
	},
	test: {
		include: ['src/**/*.{test,spec}.{js,ts}']
	}
});
