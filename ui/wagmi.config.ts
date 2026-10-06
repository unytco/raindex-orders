import { defineConfig } from '@wagmi/cli'
import { etherscan, react } from '@wagmi/cli/plugins'
import { erc20Abi } from 'viem'
import { sepolia } from 'wagmi/chains'

const apiKey = process.env.ETHERSCAN_API_KEY
if (!apiKey) {
	throw new Error('Set ETHERSCAN_API_KEY to fetch the Orderbook ABI from Etherscan')
}

export default defineConfig({
	out: 'src/generated.ts',
	contracts: [
		{
			name: 'erc20',
			abi: erc20Abi
		}
	],
	plugins: [
		etherscan({
			apiKey,
			chainId: sepolia.id,
			contracts: [
				{
					name: 'Orderbook',
					address: {
						[sepolia.id]: '0xfca89cD12Ba1346b1ac570ed988AB43b812733fe'
					}
				}
			]
		}),
		react()
	]
})
