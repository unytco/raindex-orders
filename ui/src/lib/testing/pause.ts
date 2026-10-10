/** Worker secrets under which the bridge is not paused: only `BRIDGE_PAUSED=true` pauses it. */
export const NOT_PAUSED: Record<string, string>[] = [
	{},
	{ BRIDGE_PAUSED: '' },
	{ BRIDGE_PAUSED: 'false' },
	{ BRIDGE_PAUSED: 'TRUE' }
]
