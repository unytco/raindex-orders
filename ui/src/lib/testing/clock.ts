import { vi } from 'vitest'

/**
 * `promise`, with the fake clock run on a second at a time until it settles or 4 s of real time
 * pass. The cap is real time because real work, such as a module load, can outlast any number of
 * fake seconds.
 */
export async function settled<T>(promise: Promise<T>): Promise<T> {
	let done = false
	promise.then(
		() => (done = true),
		() => (done = true)
	)
	const giveUp = performance.now() + 4_000
	while (!done && performance.now() < giveUp) await vi.advanceTimersByTimeAsync(1_000)
	if (!done) throw new Error('the promise did not settle within 4 s of real time')
	return promise
}
