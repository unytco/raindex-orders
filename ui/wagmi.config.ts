import { defineConfig } from '@wagmi/cli';
import { etherscan, react } from '@wagmi/cli/plugins';
import { erc20Abi } from 'viem';
import { sepolia } from 'wagmi/chains';
// import { PRIVATE_ETHERSCAN_API_KEY } from '$env/static/private';

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
			apiKey: `UYY85SD38FQE7FVNKKHQPEXYJRQ85UUQQI`,
			chainId: sepolia.id,
			contracts: [
				{
					name: 'Orderbook',
					address: {
						// [mainnet.id]: '0x314159265dd8dbb310642f98f50c066173c1259b',
						[sepolia.id]: '0xfca89cD12Ba1346b1ac570ed988AB43b812733fe'
					}
				}
			]
		}),
		react()
	]
});
