import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { ConfigEnv, UserConfig } from 'vite'
import { BUILD_VARIABLES } from '../network'
import { BUILDS, MAINNET_BUILD } from '../testing/builds'
import { bridgeConfig } from './bridgeConfig'

// No .env file is read: the build variables are the process's alone.
const envDir = '/nonexistent/bridge-build'
let outside: Record<string, string | undefined>

/** Runs the plugin as `vite build` does. */
function build() {
	const plugin = bridgeConfig()
	expect(plugin.apply).toBe('build')
	const hook = plugin.config as (config: UserConfig, env: ConfigEnv) => void
	hook({ envDir }, { command: 'build', mode: 'production' })
}

beforeEach(() => {
	outside = Object.fromEntries(BUILD_VARIABLES.map(key => [key, process.env[key]]))
	for (const key of BUILD_VARIABLES) delete process.env[key]
})

afterEach(() => {
	vi.unstubAllEnvs()
	for (const [key, value] of Object.entries(outside)) {
		if (value === undefined) delete process.env[key]
		else process.env[key] = value
	}
})

describe('the build', () => {
	it.each(Object.entries(BUILDS))('passes with the %s build variables', (_, variables) => {
		for (const [key, value] of Object.entries(variables)) vi.stubEnv(key, value)

		expect(build).not.toThrow()
	})

	it('fails with a variable missing, naming it', () => {
		for (const [key, value] of Object.entries(MAINNET_BUILD)) vi.stubEnv(key, value)
		vi.stubEnv('PUBLIC_CLAIM_EXPRESSION', '')

		expect(build).toThrow('PUBLIC_CLAIM_EXPRESSION is required')
	})

	it('fails on mainnet with the test signer', () => {
		for (const [key, value] of Object.entries(MAINNET_BUILD)) vi.stubEnv(key, value)
		vi.stubEnv('PUBLIC_CLAIM_SIGNER', '0x8E72b7568738da52ca3DCd9b24E178127A4E7d37')

		expect(build).toThrow('PUBLIC_CLAIM_SIGNER is the test signer')
	})
})

describe('vite.config.ts', () => {
	it('runs the build check', async () => {
		const config = (await import('../../../vite.config')).default as { plugins: unknown[] }

		const names = config.plugins.flat(Infinity).map(p => (p as { name?: string } | null)?.name)

		expect(names).toContain('bridge-config')
	})
})
