import type { Handle } from '@sveltejs/kit'
import { bridge } from '$lib/config'
import { bridgePaused, stopApi, stopPage } from '$lib/server/pause'

const FAUCET_ROUTES = new Set(['/faucet', '/api/faucet'])
// Claim stays open, as a coupon's expiry runs on through a pause and none is reissued
// (documentation/specs/bridge-stop/README.md, "Operating assumptions and limits").
const PAUSED_PAGES = new Set(['/lock', '/faucet'])
const PAUSED_APIS = new Set(['/api/faucet'])

export const handle: Handle = ({ event, resolve }) => {
	// The route, not the raw path, is matched, as SvelteKit routes a percent-encoded path.
	const route = event.route.id ?? ''
	if (!bridge.faucet && FAUCET_ROUTES.has(route)) {
		return new Response('Not found', { status: 404 })
	}
	// Stops the website only: a lock sent to the vault without it still lands, and the
	// orchestrator bridges it (documentation/specs/bridge-stop/README.md, "Operating
	// assumptions and limits").
	if (bridgePaused()) {
		if (PAUSED_PAGES.has(route)) return stopPage()
		if (PAUSED_APIS.has(route)) return stopApi()
	}
	return resolve(event)
}
