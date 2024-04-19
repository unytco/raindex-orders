import { PUBLIC_WALLETCONNECT_ID } from '$env/static/public'
import { createWeb3Modal, defaultWagmiConfig } from '@web3modal/wagmi'

import { mainnet, sepolia } from 'viem/chains'
import { reconnect } from '@wagmi/core'

export const setupWeb3Modal = () => {
	// 1. Get a project ID at https://cloud.walletconnect.com
	const projectId = PUBLIC_WALLETCONNECT_ID

	// 2. Create wagmiConfig
	const metadata = {
		name: 'Web3Modal',
		description: 'Web3Modal Example',
		url: 'https://web3modal.com', // origin must match your domain & subdomain.
		icons: ['https://avatars.githubusercontent.com/u/37784886']
	}

	const chains = [mainnet, sepolia] as const
	const config = defaultWagmiConfig({
		chains,
		projectId,
		metadata
	})
	reconnect(config)

	// 3. Create modal
	const modal = createWeb3Modal({
		wagmiConfig: config,
		projectId,
		enableAnalytics: true, // Optional - defaults to your Cloud configuration
		enableOnramp: true // Optional - false as default
	})

	return { modal, config }
}
