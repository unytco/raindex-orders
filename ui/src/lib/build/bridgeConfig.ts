import { loadEnv, type Plugin } from 'vite'
import { parseBridgeConfig, withTestnetDefaults } from '../network'

/** Fails `vite build` when a build variable is missing or malformed, and names the defaults it took. */
export function bridgeConfig(): Plugin {
	return {
		name: 'bridge-config',
		apply: 'build',
		config(config, { mode }) {
			const env = loadEnv(mode, config.envDir || process.cwd(), 'PUBLIC_')
			parseBridgeConfig(env)
			const { defaulted } = withTestnetDefaults(env)
			if (defaulted.length > 0) {
				console.info(`bridge build: TestNet's values for ${defaulted.join(', ')}`)
			}
		}
	}
}
