import { readJson } from './utils'

export const PAUSED_CONTACT = 'info@unyt.co'
export const CONTACT_LEAD = 'For more information, contact'
export const PAUSED_LEAD = `The HOT bridge is paused. It will be back soon. ${CONTACT_LEAD}`
export const PAUSED_TEXT = `${PAUSED_LEAD} ${PAUSED_CONTACT}.`
const STATUS_FAILED = 'Could not check that the bridge is open. Please try again.'

async function readPaused(): Promise<boolean> {
	const { paused } = (await readJson('/api/status')) as { paused?: unknown }
	if (typeof paused !== 'boolean') throw new Error(`/api/status answered paused: ${paused}`)
	return paused
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
