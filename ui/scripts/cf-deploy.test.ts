import { spawnSync } from 'node:child_process'
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

let bin: string

/** Runs cf-deploy.sh with `env`, against an npx that records each wrangler call. */
function deploy(env: Record<string, string>) {
	const run = spawnSync('bash', [join(__dirname, 'cf-deploy.sh')], {
		encoding: 'utf8',
		env: { PATH: `${bin}:${process.env.PATH}`, ...env }
	})
	const log = join(bin, 'calls')
	let calls: string[] = []
	try {
		calls = readFileSync(log, 'utf8').trim().split('\n')
	} catch {
		calls = []
	}
	return { status: run.status, stderr: run.stderr, calls }
}

beforeEach(() => {
	bin = mkdtempSync(join(tmpdir(), 'cf-deploy-'))
	const npx = join(bin, 'npx')
	writeFileSync(
		npx,
		`#!/usr/bin/env bash
{ printf '%s ' "$@"; if [[ $2 == secret ]]; then cat "$4"; fi; echo; } >> "${bin}/calls"
`
	)
	chmodSync(npx, 0o755)
})

afterEach(() => rmSync(bin, { recursive: true }))

describe('cf-deploy.sh', () => {
	it('deploys hot-bridge-ui with its Sepolia RPC and faucet key', () => {
		const { status, calls } = deploy({
			PUBLIC_NETWORK: 'sepolia',
			SEPOLIA_RPC_URL: 'https://sepolia.rpc.test',
			FAUCET_PRIVATE_KEY: '0xfaucet'
		})

		expect(status).toBe(0)
		expect(calls[0]).toBe('wrangler deploy --keep-vars ')
		expect(calls[1]).toMatch(/^wrangler secret bulk \S+ \{/)
		expect(JSON.parse(calls[1].slice(calls[1].indexOf('{')))).toEqual({
			SEPOLIA_RPC_URL: 'https://sepolia.rpc.test',
			FAUCET_PRIVATE_KEY: '0xfaucet'
		})
	})

	it('deploys hot-bridge-ui when PUBLIC_NETWORK is unset', () => {
		const { status, calls } = deploy({
			SEPOLIA_RPC_URL: 'https://sepolia.rpc.test',
			FAUCET_PRIVATE_KEY: '0xfaucet'
		})

		expect(status).toBe(0)
		expect(calls[0]).toBe('wrangler deploy --keep-vars ')
	})

	it('deploys the mainnet env with ETH_RPC_URL, and neither needs nor sets a faucet key', () => {
		const { status, calls } = deploy({
			PUBLIC_NETWORK: 'mainnet',
			ETH_RPC_URL: 'https://eth.rpc.test',
			FAUCET_PRIVATE_KEY: '0xfaucet'
		})

		expect(status).toBe(0)
		expect(calls[0]).toBe('wrangler deploy --keep-vars --env mainnet ')
		expect(calls[1]).toMatch(/^wrangler secret bulk \S+ --env mainnet /)
		expect(JSON.parse(calls[1].slice(calls[1].indexOf('{')))).toEqual({
			ETH_RPC_URL: 'https://eth.rpc.test'
		})
	})

	it.each([
		[
			{ PUBLIC_NETWORK: 'sepolia', SEPOLIA_RPC_URL: 'https://sepolia.rpc.test' },
			'FAUCET_PRIVATE_KEY'
		],
		[{ PUBLIC_NETWORK: 'sepolia', FAUCET_PRIVATE_KEY: '0xfaucet' }, 'SEPOLIA_RPC_URL'],
		[{ PUBLIC_NETWORK: 'mainnet', SEPOLIA_RPC_URL: 'https://sepolia.rpc.test' }, 'ETH_RPC_URL'],
		[{ PUBLIC_NETWORK: 'goerli', ETH_RPC_URL: 'https://eth.rpc.test' }, 'PUBLIC_NETWORK'],
		[{ ETH_RPC_URL: 'https://eth.rpc.test' }, 'SEPOLIA_RPC_URL']
	])('fails, deploying nothing, without %o', (env, missing) => {
		const { status, stderr, calls } = deploy(env)

		expect(status).not.toBe(0)
		expect(stderr).toContain(missing)
		expect(calls).toEqual([])
	})
})
