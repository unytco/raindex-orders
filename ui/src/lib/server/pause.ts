import { json } from '@sveltejs/kit'
import { env } from '$env/dynamic/private'
import { PAUSED_CONTACT, PAUSED_LEAD, PAUSED_TEXT } from '$lib/pause'

/** Whether the Worker's `BRIDGE_PAUSED` secret pauses Lock and the faucet. */
export const bridgePaused = () => env.BRIDGE_PAUSED === 'true'

// Loads no script of the site, so the real page cannot render behind it.
const STOP_PAGE = `<!doctype html>
<html lang="en">
	<head>
		<meta charset="utf-8" />
		<meta name="viewport" content="width=device-width, initial-scale=1" />
		<link rel="icon" href="/favicon.png" />
		<title>HOT bridge paused</title>
		<style>
			body {
				margin: 0;
				color: #111827;
				background: #fff;
				font-family: ui-sans-serif, system-ui, sans-serif;
				line-height: 1.5;
			}
			main {
				box-sizing: border-box;
				width: min(36rem, calc(100% - 2rem));
				margin: 3rem auto;
				padding: 1.5rem;
				border: 1px solid #e5e7eb;
				border-radius: 0.5rem;
				box-shadow: 0 4px 6px -1px rgb(0 0 0 / 0.1), 0 2px 4px -2px rgb(0 0 0 / 0.1);
			}
			h1 {
				margin: 1rem 0 0.5rem;
				font-size: 1.5rem;
				line-height: 2rem;
			}
			p {
				margin: 0;
				color: #4b5563;
			}
			a {
				color: #1a56db;
				text-underline-offset: 2px;
			}
			a:focus-visible {
				outline: 2px solid #1a56db;
				outline-offset: 2px;
				border-radius: 2px;
			}
		</style>
	</head>
	<body>
		<main>
			<svg aria-hidden="true" width="40" height="40" viewBox="0 0 40 40">
				<circle cx="20" cy="20" r="20" fill="#fdf6b2" />
				<rect x="14" y="12" width="4" height="16" rx="1" fill="#723b13" />
				<rect x="22" y="12" width="4" height="16" rx="1" fill="#723b13" />
			</svg>
			<h1>Bridge paused</h1>
			<p>${PAUSED_LEAD} <a href="mailto:${PAUSED_CONTACT}">${PAUSED_CONTACT}</a>.</p>
		</main>
	</body>
</html>
`

/** The answer of the Lock and faucet pages while paused. */
export const stopPage = () =>
	new Response(STOP_PAGE, {
		status: 503,
		headers: { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' }
	})

/** The answer of the faucet's API while paused. */
export const stopApi = () =>
	json({ error: PAUSED_TEXT }, { status: 503, headers: { 'Access-Control-Allow-Origin': '*' } })
