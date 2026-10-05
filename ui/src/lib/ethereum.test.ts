import { afterEach, describe, expect, it, vi } from 'vitest'
import { get } from 'svelte/store'
import { erc20Abi, numberToHex } from 'viem'
import { asBuild, BUILDS } from './testing/builds'

const CHAIN = { sepolia: 11155111, mainnet: 1 }
const OTHER = { sepolia: 1, mainnet: 11155111 }
const ACCOUNT = '0x1111111111111111111111111111111111111111'

/** A wallet on `chainId` that records what it is asked. */
function wallet(chainId: number, refuseSwitch?: number) {
	const asked: { method: string; params?: unknown[] }[] = []
	const request = vi.fn(async (args: { method: string; params?: unknown[] }) => {
		asked.push(args)
		if (args.method === 'eth_chainId') return numberToHex(chainId)
		if (args.method === 'eth_accounts') return []
		if (args.method === 'eth_sendTransaction') return '0xabc'
		if (args.method === 'wallet_switchEthereumChain' && refuseSwitch) {
			throw Object.assign(new Error('refused'), { code: refuseSwitch })
		}
		return null
	})
	vi.stubGlobal('window', { ethereum: { request, on: vi.fn() } })
	return asked
}

async function connectedOn(network: 'sepolia' | 'mainnet', walletChain: number) {
	const asked = wallet(walletChain)
	const ethereum = await asBuild(BUILDS[network], () => import('./ethereum'))
	ethereum.ethereumStore.set({
		isConnected: true,
		account: ACCOUNT,
		chainId: walletChain,
		isLoading: false,
		error: null
	})
	return { ethereum, asked }
}

const approve = {
	address: '0x2222222222222222222222222222222222222222',
	abi: erc20Abi,
	functionName: 'approve',
	args: [ACCOUNT, 1n]
}

afterEach(() => {
	vi.unstubAllGlobals()
	vi.restoreAllMocks()
})

describe.each(['sepolia', 'mainnet'] as const)('on a %s build', network => {
	it('sends nothing while the wallet is on another chain', async () => {
		const { ethereum, asked } = await connectedOn(network, OTHER[network])

		expect(get(ethereum.onWrongNetwork)).toBe(true)
		await expect(ethereum.writeContract(approve)).rejects.toThrow('Switch your wallet to')
		expect(asked.map(a => a.method)).not.toContain('eth_sendTransaction')
	})

	it("checks the wallet's own chain, not the last one it reported", async () => {
		const { ethereum, asked } = await connectedOn(network, OTHER[network])
		ethereum.ethereumStore.update(s => ({ ...s, chainId: CHAIN[network] }))

		expect(get(ethereum.onWrongNetwork)).toBe(false)
		await expect(ethereum.writeContract(approve)).rejects.toThrow('Switch your wallet to')
		expect(asked.map(a => a.method)).not.toContain('eth_sendTransaction')
	})

	it("sends on the build's own chain, naming it to the wallet", async () => {
		const { ethereum, asked } = await connectedOn(network, CHAIN[network])

		expect(get(ethereum.onWrongNetwork)).toBe(false)
		await expect(ethereum.writeContract(approve)).resolves.toBe('0xabc')
		const sends = asked.filter(a => a.method === 'eth_sendTransaction')
		expect(sends).toHaveLength(1)
		expect(sends[0].params).toEqual([
			expect.objectContaining({ chainId: numberToHex(CHAIN[network]) })
		])
	})

	it('reads the chain from the wallet when an account connects', async () => {
		wallet(CHAIN[network])
		const listeners: Record<string, (value: never) => void> = {}
		;(window as unknown as { ethereum: { on: unknown } }).ethereum.on = (
			event: string,
			listener: (value: never) => void
		) => (listeners[event] = listener)
		const ethereum = await asBuild(BUILDS[network], () => import('./ethereum'))
		await ethereum.initEthereum()

		await (listeners.accountsChanged as unknown as (a: string[]) => Promise<void>)([ACCOUNT])

		expect(get(ethereum.ethereumStore).chainId).toBe(CHAIN[network])
		expect(get(ethereum.onWrongNetwork)).toBe(false)
	})

	it("asks the wallet to switch to the build's chain, then reads the chain it is on", async () => {
		const { ethereum, asked } = await connectedOn(network, CHAIN[network])
		ethereum.ethereumStore.update(s => ({ ...s, chainId: OTHER[network] }))

		expect(await ethereum.switchNetwork()).toBe(true)
		expect(asked).toEqual([
			{ method: 'wallet_switchEthereumChain', params: [{ chainId: numberToHex(CHAIN[network]) }] },
			{ method: 'eth_chainId' }
		])
		expect(get(ethereum.onWrongNetwork)).toBe(false)
	})
})

describe('a wallet without the chain', () => {
	it('is asked to add Sepolia', async () => {
		const asked = wallet(1, 4902)
		const ethereum = await asBuild(BUILDS.sepolia, () => import('./ethereum'))

		expect(await ethereum.switchNetwork()).toBe(true)
		expect(asked[1]).toEqual({
			method: 'wallet_addEthereumChain',
			params: [
				{
					chainId: '0xaa36a7',
					chainName: 'Sepolia Testnet',
					nativeCurrency: { name: 'Sepolia ETH', symbol: 'ETH', decimals: 18 },
					rpcUrls: ['https://rpc.sepolia.org'],
					blockExplorerUrls: ['https://sepolia.etherscan.io']
				}
			]
		})
	})

	it('is not asked to add Ethereum', async () => {
		vi.spyOn(console, 'error').mockImplementation(() => {})
		const asked = wallet(11155111, 4902)
		const ethereum = await asBuild(BUILDS.mainnet, () => import('./ethereum'))

		expect(await ethereum.switchNetwork()).toBe(false)
		expect(asked.map(a => a.method)).toEqual(['wallet_switchEthereumChain'])
	})
})
