import type { Handle } from '@sveltejs/kit'
import { bridge } from '$lib/config'
import { bridgePaused, stopApi, stopPage } from '$lib/server/pause'

const FAUCET_ROUTES = new Set(['/faucet', '/api/faucet'])
const PAUSED_PAGES = new Set(['/lock', '/faucet'])
const PAUSED_APIS = new Set(['/api/faucet'])

/**
 * The faucet exists on TestNet only: elsewhere its page and API answer 404. While the
 * bridge is paused, Lock and the faucet answer 503 without running. The route, not the
 * raw path, is matched, as SvelteKit routes a percent-encoded path.
 */
export const handle: Handle = ({ event, resolve }) => {
	const route = event.route.id ?? ''
	if (!bridge.faucet && FAUCET_ROUTES.has(route)) {
		return new Response('Not found', { status: 404 })
	}
	if (bridgePaused()) {
		if (PAUSED_PAGES.has(route)) return stopPage()
		if (PAUSED_APIS.has(route)) return stopApi()
	}
	return resolve(event)
}
