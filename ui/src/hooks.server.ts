import type { Handle } from '@sveltejs/kit'
import { bridge } from '$lib/config'
import { bridgePaused, stopApi, stopPage } from '$lib/server/pause'

const FAUCET_ROUTES = new Set(['/faucet', '/api/faucet'])

export const handle: Handle = ({ event, resolve }) => {
	// The route, not the raw path, is matched, as SvelteKit routes a percent-encoded path.
	const route = event.route.id ?? ''
	if (!bridge.faucet && FAUCET_ROUTES.has(route)) {
		return new Response('Not found', { status: 404 })
	}
	// Claim stops too, though a coupon's expiry runs on through a pause.
	if (bridgePaused() && route !== '/api/status') {
		return route.startsWith('/api/') ? stopApi() : stopPage()
	}
	return resolve(event)
}
