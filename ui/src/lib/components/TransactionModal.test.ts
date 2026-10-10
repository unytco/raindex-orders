import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import type { transactionStore as Store } from '$lib/stores/transactionStore'
import { asBuild, BUILDS } from '$lib/testing/builds'

const EXPLORERS = {
	sepolia: 'https://sepolia.etherscan.io',
	mainnet: 'https://etherscan.io'
}

const HASH = '0x7d3c9b2a51e8f4c6a0b9d2e7f1c3a5b8d6e4f2a0c9b7d5e3f1a8c6b4d2e0f9a7'
const hasX = (html: string) => html.includes('aria-label="Close modal"')
const hasCloseButton = (html: string) => />\s*Close\s*<\/button>/.test(html)

describe.each(['sepolia', 'mainnet'] as const)('TransactionModal on %s', network => {
	const ETHERSCAN = `href="${EXPLORERS[network]}/tx/${HASH}"`
	let transactionStore: typeof Store
	let render: () => string

	beforeEach(async () => {
		const loaded = await asBuild(BUILDS[network], async () => ({
			modal: (await import('./TransactionModal.svelte')).default,
			store: (await import('$lib/stores/transactionStore')).transactionStore
		}))
		transactionStore = loaded.store
		// Vitest runs in node, where a .svelte import compiles to Svelte's server renderer.
		render = () => (loaded.modal as unknown as { render: () => { html: string } }).render().html
	})

	function confirm(isLock: boolean) {
		transactionStore.awaitWalletConfirmation(isLock)
		transactionStore.awaitTxReceipt(HASH)
		transactionStore.transactionSuccess(HASH)
	}

	afterEach(() => transactionStore.reset())

	it('lock success closes with the X and tells the user to finalize in the Unyt app', () => {
		confirm(true)
		const html = render()

		expect(hasX(html)).toBe(true)
		expect(html).toContain('Lock almost complete')
		expect(html).toContain('Go back to your Unyt app now to finalize. You can close this window.')
		expect(html).toContain(ETHERSCAN)
		expect(html).toContain('View transaction on Etherscan')
		expect(hasCloseButton(html)).toBe(false)
	})

	it('claim success closes with the X alone', () => {
		confirm(false)
		const html = render()

		expect(hasX(html)).toBe(true)
		expect(html).toContain(ETHERSCAN)
		expect(hasCloseButton(html)).toBe(false)
		expect(html).not.toContain('Lock almost complete')
	})

	it('an error still offers its Close button beside the X', () => {
		transactionStore.awaitWalletConfirmation(true)
		transactionStore.transactionError({ message: 'User rejected the request.' })
		const html = render()

		expect(html).toContain('User rejected the request.')
		expect(hasX(html)).toBe(true)
		expect(hasCloseButton(html)).toBe(true)
	})

	it('a transaction that failed shows its message and links to the explorer', () => {
		transactionStore.awaitWalletConfirmation()
		transactionStore.awaitTxReceipt(HASH)
		transactionStore.transactionError({ message: 'Your claim did not go through.' })
		const html = render()

		expect(html).toContain('>Your claim did not go through.</p>')
		expect(html).toContain(ETHERSCAN)
		expect(html.split(HASH)).toHaveLength(2)
		expect(hasCloseButton(html)).toBe(true)
	})

	it('an error before any transaction links nowhere, even after an earlier one', () => {
		transactionStore.awaitWalletConfirmation()
		transactionStore.awaitTxReceipt(HASH)
		transactionStore.awaitWalletConfirmation(true)
		transactionStore.transactionError({ message: 'This coupon has already been claimed.' })
		const html = render()

		expect(html).toContain('This coupon has already been claimed.')
		expect(html).not.toContain(HASH)
		expect(html).not.toContain('View transaction on Etherscan')
	})

	it.each([
		['waiting for the wallet', () => transactionStore.awaitWalletConfirmation(true)],
		[
			'confirming on-chain',
			() => {
				transactionStore.awaitWalletConfirmation(true)
				transactionStore.awaitTxReceipt(HASH)
			}
		]
	])('cannot be dismissed while %s', (_state, enter) => {
		enter()
		const html = render()

		expect(html).toContain('role="dialog"')
		expect(hasX(html)).toBe(false)
		expect(hasCloseButton(html)).toBe(false)
	})

	it('renders nothing while idle', () => {
		expect(render()).not.toContain('role="dialog"')
	})
})
