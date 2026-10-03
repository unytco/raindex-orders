// See https://kit.svelte.dev/docs/types#app
// for information about these interfaces
// Brings in App.Platform's `context`, `caches` and `cf`, as the Cloudflare adapter types them.
/// <reference types="@sveltejs/adapter-cloudflare" />
declare global {
	namespace App {
		// interface Error {}
		// interface Locals {}
		// interface PageData {}
		// interface PageState {}
		// interface Platform {}
	}
}

export {}
