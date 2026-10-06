import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { readable } from 'svelte/store'
import { asBuild, BUILDS } from '$lib/testing/builds'

vi.mock('$app/stores', () => ({ page: readable({ url: new URL('https://hot-bridge.test/') }) }))

// The lock page reads its balances through the wallet, which a server render lacks.
beforeEach(() => {
	vi.spyOn(console, 'error').mockImplementation(() => {})
})
afterEach(() => {
	vi.restoreAllMocks()
})

type Network = 'sepolia' | 'mainnet'
type Rendered = { render: (props?: Record<string, unknown>) => { html: string } }

const CHAIN = { sepolia: 11155111, mainnet: 1 }
const OTHER = { sepolia: 1, mainnet: 11155111 }
const NAMES = {
	sepolia: {
		network: 'Sepolia Testnet',
		token: 'mock HOT',
		explorer: 'https://sepolia.etherscan.io'
	},
	mainnet: { network: 'Ethereum', token: 'HOT', explorer: 'https://etherscan.io' }
}
const PAGES = {
	home: () => import('./+page.svelte'),
	lock: () => import('./lock/+page.svelte'),
	claim: () => import('./claim/+page.svelte'),
	footer: () => import('$lib/components/CommonFooter.svelte'),
	receipt: () => import('$lib/components/TransactionReceipt.svelte')
}

/** `page`'s server render on a `network` build, its wallet on `chainId` if given. */
async function render(
	network: Network,
	page: keyof typeof PAGES,
	chainId?: number,
	props?: Record<string, unknown>
) {
	return asBuild(BUILDS[network], async () => {
		const { ethereumStore } = await import('$lib/ethereum')
		if (chainId) {
			ethereumStore.set({
				isConnected: true,
				account: '0x1111111111111111111111111111111111111111',
				chainId,
				isLoading: false,
				error: null
			})
		}
		const component = (await PAGES[page]()).default as unknown as Rendered
		return component.render(props).html
	})
}

const text = (html: string) => html.replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ')

describe.each(['sepolia', 'mainnet'] as const)('a %s build', network => {
	const names = NAMES[network]

	it.each(['home', 'lock', 'claim'] as const)(
		'%s shows the switch, and no form, while the wallet is on another chain',
		async page => {
			const html = text(await render(network, page, OTHER[network]))

			expect(html).toContain(`Wrong network: please switch to ${names.network}`)
			expect(html).toContain(`Switch to ${names.network}`)
			expect(html).not.toContain('Amount to Lock')
			expect(html).not.toContain('Claim Coupon')
		}
	)

	it('lock shows its form, in its token, on its own chain', async () => {
		const html = text(await render(network, 'lock', CHAIN[network]))

		expect(html).toContain(`Lock ${names.token}`)
		expect(html).toContain('Amount to Lock')
		expect(html).not.toContain('Switch to')
	})

	it('claim shows its form, in its token, on its own chain', async () => {
		const html = text(await render(network, 'claim', CHAIN[network]))

		expect(html).toContain(`Claim ${names.token}`)
		expect(html).toContain('Claim Coupon')
		expect(html).not.toContain('Switch to')
	})

	it('home names its network on its own chain', async () => {
		const html = text(await render(network, 'home', CHAIN[network]))

		expect(html).toContain(names.network)
		expect(html).not.toContain('Switch to')
	})

	it("a receipt links the transaction on its network's explorer", async () => {
		const html = await render(network, 'receipt', undefined, {
			title: 'Done',
			message: 'Done',
			hash: '0xabc'
		})

		expect(html).toContain(`href="${names.explorer}/tx/0xabc"`)
	})
})

describe('a mainnet build', () => {
	it.each(['home', 'lock', 'claim'] as const)('%s names nothing of TestNet', async page => {
		const html = text(await render('mainnet', page, CHAIN.mainnet)).toLowerCase()

		for (const testnet of ['sepolia', 'mock', 'faucet', 'testnet']) {
			expect(html).not.toContain(testnet)
		}
	})
})

describe('the quick links', () => {
	it('link both faucets on sepolia', async () => {
		const html = await render('sepolia', 'footer')

		expect(html).toContain('href="/faucet"')
		expect(html).toContain('https://cloud.google.com/application/web3/faucet/ethereum/sepolia')
	})

	it('link no faucet on mainnet', async () => {
		const html = await render('mainnet', 'footer')

		expect(html.toLowerCase()).not.toContain('faucet')
		expect(html).toContain('Copy URL')
	})
})
