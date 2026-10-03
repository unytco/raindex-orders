import { afterEach, describe, expect, it } from 'vitest'
import { transactionStore } from '$lib/stores/transactionStore'
import TransactionModal from './TransactionModal.svelte'

// Vitest runs in node, where a .svelte import compiles to Svelte's server renderer.
const render = () =>
	(TransactionModal as unknown as { render: () => { html: string } }).render().html

const HASH = '0x7d3c9b2a51e8f4c6a0b9d2e7f1c3a5b8d6e4f2a0c9b7d5e3f1a8c6b4d2e0f9a7'
const ETHERSCAN = `href="https://sepolia.etherscan.io/tx/${HASH}"`
const hasX = (html: string) => html.includes('aria-label="Close modal"')
const hasCloseButton = (html: string) => />\s*Close\s*<\/button>/.test(html)

function confirm(isLock: boolean) {
	transactionStore.awaitWalletConfirmation(isLock)
	transactionStore.awaitTxReceipt(HASH)
	transactionStore.transactionSuccess(HASH)
}

afterEach(() => transactionStore.reset())

describe('TransactionModal', () => {
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
