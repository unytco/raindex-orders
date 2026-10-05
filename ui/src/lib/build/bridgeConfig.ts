import { loadEnv, type Plugin } from 'vite'
import { parseBridgeConfig } from '../network'

/** Fails `vite build` when a build variable is missing or malformed. */
export function bridgeConfig(): Plugin {
	return {
		name: 'bridge-config',
		apply: 'build',
		config(config, { mode }) {
			parseBridgeConfig(loadEnv(mode, config.envDir || process.cwd(), 'PUBLIC_'))
		}
	}
}
