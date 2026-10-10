export const PAUSED_CONTACT = 'info@unyt.co'
/** The stop text up to its contact address, which each page renders as a mail link. */
export const PAUSED_LEAD =
	'The HOT bridge is paused. It will be back soon. For more information, contact'
export const PAUSED_TEXT = `${PAUSED_LEAD} ${PAUSED_CONTACT}.`

/** Whether `/api/status` reports the bridge paused. Throws on any answer but `{ paused: boolean }`. */
export async function readPaused(): Promise<boolean> {
	const response = await fetch('/api/status')
	if (!response.ok) throw new Error(`/api/status answered ${response.status}`)
	const { paused } = (await response.json()) as { paused?: unknown }
	if (typeof paused !== 'boolean') throw new Error(`/api/status answered paused: ${paused}`)
	return paused
}
