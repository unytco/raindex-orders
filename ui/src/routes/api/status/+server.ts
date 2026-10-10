import { json } from '@sveltejs/kit'
import type { RequestHandler } from './$types'
import { bridgePaused } from '$lib/server/pause'

const HEADERS = { 'Access-Control-Allow-Origin': '*', 'Cache-Control': 'no-store' }

export const GET: RequestHandler = () => json({ paused: bridgePaused() }, { headers: HEADERS })
