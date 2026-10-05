import type { Handle } from '@sveltejs/kit'
import { bridge } from '$lib/config'

const FAUCET_ROUTE = /^\/(api\/)?faucet(\/|$)/

/** The faucet exists on TestNet only: elsewhere its page and API answer 404. */
export const handle: Handle = ({ event, resolve }) => {
	if (!bridge.faucet && FAUCET_ROUTE.test(event.url.pathname)) {
		return new Response('Not found', { status: 404 })
	}
	return resolve(event)
}
