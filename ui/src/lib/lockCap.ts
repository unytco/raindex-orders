import { readable } from 'svelte/store'
import { parseUnits } from 'viem'
import { readJson } from './utils'

export const LOCK_CAP_USD = 1000
// Every network uses real HOT's price: the TestNet's mock HOT has none.
export const HOT_PRICE_URL =
	'https://api.coingecko.com/api/v3/simple/price?ids=holotoken&vs_currencies=usd'
// CoinGecko's keyless API refreshes a price every 60 s and rate-limits calls per shared IP.
export const PRICE_REFRESH_MS = 60_000
const PRICE_DECIMALS = 12

async function readHotPrice(): Promise<bigint | 'unavailable'> {
	try {
		const body = (await readJson(HOT_PRICE_URL)) as { holotoken?: { usd?: unknown } } | null
		const usd = body?.holotoken?.usd
		const price =
			typeof usd === 'number' ? parseUnits(usd.toFixed(PRICE_DECIMALS), PRICE_DECIMALS) : 0n
		if (price <= 0n) throw new Error(`CoinGecko answered ${JSON.stringify(body)}`)
		return price
	} catch (e) {
		console.error('Error reading the HOT price:', e)
		return 'unavailable'
	}
}

/** HOT's price in 10⁻¹² USD per whole HOT. */
export const hotPrice = readable<bigint | 'reading' | 'unavailable'>('reading', set => {
	let stopped = false
	set('reading')
	const refresh = async () => {
		const price = await readHotPrice()
		if (!stopped) set(price)
	}
	refresh()
	const timer = setInterval(refresh, PRICE_REFRESH_MS)
	return () => {
		stopped = true
		clearInterval(timer)
	}
})

export const lockCapWei = (price: bigint, decimals: number) =>
	(BigInt(LOCK_CAP_USD) * 10n ** BigInt(decimals + PRICE_DECIMALS)) / price
