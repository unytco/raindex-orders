import type { Handle } from '@sveltejs/kit'
import { bridge } from '$lib/config'

const FAUCET_ROUTES = new Set(['/faucet', '/api/faucet'])

/**
 * The faucet exists on TestNet only: elsewhere its page and API answer 404. The
 * route, not the raw path, is matched, as SvelteKit routes a percent-encoded path.
 */
export const handle: Handle = ({ event, resolve }) => {
	if (!bridge.faucet && FAUCET_ROUTES.has(event.route.id ?? '')) {
		return new Response('Not found', { status: 404 })
	}
	return resolve(event)
}
