#!/usr/bin/env node
// Composes the claim expression for one network:
//   node compose-rainlang.mjs --network mainnet --subparser 0x... --signer 0x... [src/holo-claim.rain]
//   node compose-rainlang.mjs     TestNet's network and bindings
import pkg from '@rainlanguage/dotrain'
import { readFileSync } from 'node:fs'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { parseArgs } from 'node:util'

const { RainDocument, MetaStore } = pkg

export const TEST_SIGNER = '0x8E72b7568738da52ca3DCd9b24E178127A4E7d37'
const NETWORKS = ['sepolia', 'mainnet']
const ZERO_ADDRESS = `0x${'0'.repeat(40)}`
/** TestNet's bindings: a sepolia composition takes each one it is not given. */
const SEPOLIA_DEFAULTS = {
	subparser: '0xe6A589716d5a72276C08E0e08bc941a28005e55A',
	signer: TEST_SIGNER
}

const isAddress = value => /^0x[0-9a-fA-F]{40}$/.test(value ?? '') && value !== ZERO_ADDRESS

/**
 * The expression's two network bindings, or an error naming every input at fault.
 * The network is sepolia when unset, and sepolia takes TestNet's bindings for any
 * it is not given. Mainnet has none.
 */
export function bindings({ network = 'sepolia', subparser, signer }) {
	if (network === 'sepolia') {
		subparser ||= SEPOLIA_DEFAULTS.subparser
		signer ||= SEPOLIA_DEFAULTS.signer
	}
	const faults = []
	if (!NETWORKS.includes(network)) {
		faults.push(`--network must be sepolia or mainnet, not ${network ?? 'unset'}`)
	}
	if (!isAddress(subparser)) {
		faults.push(`--subparser must be a nonzero address, not ${subparser ?? 'unset'}`)
	}
	if (!isAddress(signer)) {
		faults.push(`--signer must be a nonzero address, not ${signer ?? 'unset'}`)
	} else if (network === 'mainnet' && signer.toLowerCase() === TEST_SIGNER.toLowerCase()) {
		faults.push(`--signer ${signer} is the test signer, whose key is public: mainnet refuses it`)
	}
	if (faults.length > 0) throw new Error(faults.join('; '))
	return [
		['orderbook-subparser', subparser],
		['valid-signer', signer]
	]
}

const CLAIM_EXPRESSION = fileURLToPath(new URL('src/holo-claim.rain', import.meta.url))

export async function compose({ network, subparser, signer, file = CLAIM_EXPRESSION }) {
	const rebinds = bindings({ network, subparser, signer })
	return RainDocument.composeText(
		readFileSync(file, 'utf8'),
		['calculate-io', 'handle-io'],
		new MetaStore(),
		rebinds
	)
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
	const { values, positionals } = parseArgs({
		options: {
			network: { type: 'string' },
			subparser: { type: 'string' },
			signer: { type: 'string' }
		},
		allowPositionals: true
	})
	try {
		console.log(await compose({ ...values, file: positionals[0] }))
	} catch (error) {
		console.error(error.message)
		process.exit(1)
	}
}
