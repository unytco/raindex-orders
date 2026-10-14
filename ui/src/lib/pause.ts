export const PAUSED_CONTACT = 'info@unyt.co'
export const CONTACT_LEAD = 'For more information, contact'
export const PAUSED_LEAD = `The HOT bridge is paused. It will be back soon. ${CONTACT_LEAD}`
export const PAUSED_TEXT = `${PAUSED_LEAD} ${PAUSED_CONTACT}.`
export const STATUS_TIMEOUT_MS = 15_000
const STATUS_FAILED = 'Could not check that the bridge is open. Please try again.'

async function readPaused(): Promise<boolean> {
	// Not AbortSignal.timeout: it needs Safari 16, and the build targets Safari 14.
	const timeout = new AbortController()
	const timer = setTimeout(() => timeout.abort(), STATUS_TIMEOUT_MS)
	try {
		const response = await fetch('/api/status', { signal: timeout.signal })
		if (!response.ok) throw new Error(`/api/status answered ${response.status}`)
		const { paused } = (await response.json()) as { paused?: unknown }
		if (typeof paused !== 'boolean') throw new Error(`/api/status answered paused: ${paused}`)
		return paused
	} finally {
		clearTimeout(timer)
	}
}

export async function bridgeStatus(): Promise<{ open: boolean; paused: boolean; error: string }> {
	try {
		const paused = await readPaused()
		return { open: !paused, paused, error: '' }
	} catch (e) {
		console.error('Error reading the bridge status:', e)
		return { open: false, paused: false, error: STATUS_FAILED }
	}
}
