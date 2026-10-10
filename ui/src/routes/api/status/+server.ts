import { json } from '@sveltejs/kit'
import type { RequestHandler } from './$types'
import { bridgePaused } from '$lib/server/pause'

const HEADERS = { 'Access-Control-Allow-Origin': '*', 'Cache-Control': 'no-store' }

// No OPTIONS: a plain GET needs no preflight, so a reader adding its own headers is refused.
export const GET: RequestHandler = () => json({ paused: bridgePaused() }, { headers: HEADERS })
