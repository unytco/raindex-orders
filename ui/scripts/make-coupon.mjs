#!/usr/bin/env node
// Makes a claim coupon for a TestNet claim order on Base Sepolia, or on a local fork
// of it, and prints the claim page's link for it. From ui/:
//   npm run make-coupon -- --recipient 0x... --amount 5 [--expiry <unix seconds>] [--nonce <n>]
// It reads the claim order and the RPC from an env file: the website's,
// .env.development.local by default (test/local-ui.sh writes it), or a deploy record
// such as the repo's .env after deploy-base-sepolia.sh, with --env.
//
// The signer is the claim order's. The test signer signs with its key, which is public
// (TEST_SIGNER_KEY in src/Constants.sol). A Safe's owners sign, as the orchestrator
// has them sign (docs/signer-service.md), and the coupon carries the threshold of
// their signatures:
//   SAFE_OWNER_KEYS=<key>,<key>                  owner keys, or the file
//   --owner-keys-file <file>                     of owner keys, one a line
//   SAFE_OWNER_SERVICES=<owner>=<URL>,...        owners' signer services (mock ones)
// Owner keys never go on the command line. A coupon a service signs pays a random
// withdrawal ID, whose keccak256 is its nonce, or --withdrawal.
//
// It refuses mainnet: test coupons are for Base Sepolia, and a local fork of it, only.
import { readFileSync, statSync } from 'node:fs'
import { randomBytes } from 'node:crypto'
import { parseArgs } from 'node:util'
import { fileURLToPath } from 'node:url'
import {
	createPublicClient,
	formatUnits,
	getAddress,
	http,
	isAddress,
	isAddressEqual,
	parseAbi,
	parseUnits
} from 'viem'
import {
	couponString,
	couponWords,
	HOLO_VAULT_ID,
	signCoupon,
	TEST_SIGNER_KEY
} from './coupon-signing.mjs'
import {
	collectSafeSignature,
	noOwnersGiven,
	ownerKeysFrom,
	servicesFrom,
	testWithdrawal,
	withdrawalFrom
} from './safe-coupon.mjs'

const TEST_SIGNER = '0x8E72b7568738da52ca3DCd9b24E178127A4E7d37'
const BASE_SEPOLIA = 84532
const WEEK = 7n * 24n * 60n * 60n
const EIP1271_MAGIC_VALUE = '0x1626ba7e'
const SAFE_ABI = parseAbi([
	'function getThreshold() view returns (uint256)',
	'function getOwners() view returns (address[])',
	'function isValidSignature(bytes32 hash, bytes signature) view returns (bytes4)'
])

const fail = message => {
	console.error(`make-coupon: ${message}`)
	process.exit(1)
}

let args
try {
	;({ values: args } = parseArgs({
		options: {
			recipient: { type: 'string' },
			amount: { type: 'string' },
			expiry: { type: 'string' },
			nonce: { type: 'string' },
			withdrawal: { type: 'string' },
			env: { type: 'string' },
			rpc: { type: 'string' },
			'owner-keys-file': { type: 'string' },
			url: { type: 'string', default: 'http://localhost:5173' }
		}
	}))
} catch (err) {
	fail(
		`${err.message}. Owner keys go in SAFE_OWNER_KEYS or --owner-keys-file, never on the command line`
	)
}

if (!args.recipient || !isAddress(args.recipient)) fail('--recipient must be an address')
if (!args.amount) fail('--amount is required, in HOT, such as 5 or 0.5')
let wei
try {
	wei = parseUnits(args.amount, 18)
} catch {
	fail(`--amount ${args.amount} is not a number of HOT`)
}
if (wei <= 0n) fail('--amount must be more than 0')

const envFile = args.env ?? fileURLToPath(new URL('../.env.development.local', import.meta.url))
let text
try {
	text = readFileSync(envFile, 'utf8')
} catch {
	fail(`${envFile} is not there: run test/local-ui.sh first, or pass --env`)
}
const env = Object.fromEntries(
	text
		.split('\n')
		.map(line => /^\s*([A-Z0-9_]+)=(.*?)\s*$/.exec(line))
		.filter(Boolean)
		.map(([, key, value]) => [key, value])
)
// The website's variable, or the deploy record's (deploy-base-sepolia.sh's .env).
const value = (...keys) => keys.map(key => env[key]).find(Boolean)
const need = (...keys) => value(...keys) || fail(`${envFile} has no ${keys.join(' or ')}`)

if (env.MAINNET_LOCK_VAULT_ADDRESS)
	fail(`${envFile} is a mainnet record: make-coupon never signs for mainnet`)
const network =
	value('PUBLIC_NETWORK', 'NETWORK') ??
	(env.BASE_SEPOLIA_LOCK_VAULT_ADDRESS ? 'base-sepolia' : undefined)
if (network !== 'base-sepolia') {
	fail(`PUBLIC_NETWORK is ${network || 'not set'}: test coupons are for base-sepolia only`)
}

const rpcUrl = args.rpc ?? need('BASE_SEPOLIA_RPC_URL')
const client = createPublicClient({ transport: http(rpcUrl, { retryCount: 0 }) })
let chainId
try {
	chainId = await client.getChainId()
} catch {
	fail(`${rpcUrl} did not answer: is anvil running (test/local-ui.sh)?`)
}
if (chainId === 1) fail(`${rpcUrl} is Ethereum mainnet: make-coupon never signs for mainnet`)
if (chainId !== BASE_SEPOLIA)
	fail(`${rpcUrl} answers for chain ${chainId}, not Base Sepolia (84532)`)

const orderHash = need('PUBLIC_CLAIM_ORDER_HASH', 'ORDER_HASH')
const vault = need('PUBLIC_LOCK_VAULT_ADDRESS', 'BASE_SEPOLIA_LOCK_VAULT_ADDRESS')
const orderbook = need('PUBLIC_ORDERBOOK_ADDRESS', 'ORDERBOOK_ADDRESS')
if (env.VAULT_ID && BigInt(env.VAULT_ID) !== HOLO_VAULT_ID)
	fail(`VAULT_ID ${env.VAULT_ID} is not the bridge's vault ID`)
const order = {
	hash: BigInt(orderHash),
	owner: BigInt(vault),
	orderbook: BigInt(orderbook),
	token: BigInt(need('PUBLIC_TOKEN_ADDRESS', 'TOKEN_ADDRESS')),
	vaultId: HOLO_VAULT_ID
}
const orderExists = await client.readContract({
	address: orderbook,
	abi: parseAbi(['function orderExists(bytes32) view returns (bool)']),
	functionName: 'orderExists',
	args: [orderHash]
})
if (!orderExists) fail(`the claim order ${orderHash} is not on ${rpcUrl}`)

const signer = getAddress(need('PUBLIC_CLAIM_SIGNER', 'CLAIM_SIGNER'))
const isTestSigner = isAddressEqual(signer, TEST_SIGNER)

let keys = []
if (process.env.SAFE_OWNER_KEYS && args['owner-keys-file'])
	fail('give owner keys in SAFE_OWNER_KEYS or in --owner-keys-file, not both')
try {
	if (process.env.SAFE_OWNER_KEYS) keys = ownerKeysFrom(process.env.SAFE_OWNER_KEYS)
	if (args['owner-keys-file']) {
		const file = args['owner-keys-file']
		if (statSync(file).mode & 0o077)
			console.error(`make-coupon: ${file} can be read by others: chmod 600 it`)
		keys = ownerKeysFrom(readFileSync(file, 'utf8'))
	}
} catch (err) {
	fail(err.code === 'ENOENT' ? `${args['owner-keys-file']} is not there` : err.message)
}
let services = []
try {
	if (process.env.SAFE_OWNER_SERVICES) services = servicesFrom(process.env.SAFE_OWNER_SERVICES)
} catch (err) {
	fail(err.message)
}
const safeMode = keys.length > 0 || services.length > 0
if (isTestSigner && safeMode)
	fail("the claim order takes the test signer's coupons: it has no Safe owners to sign")
if (!isTestSigner && !safeMode) {
	fail(
		`the claim order takes coupons from ${signer}, not the test signer ${TEST_SIGNER}. ${noOwnersGiven(process.env)}`
	)
}
if (args.withdrawal && services.length === 0)
	fail('--withdrawal is for signer services, which sign a withdrawal: none is given')
if (args.nonce && services.length > 0)
	fail(
		'a signer service signs a withdrawal, whose keccak256 is the nonce: pass --withdrawal, not --nonce'
	)

// A week from the chain's time, not this machine's: a fork's clock starts at its block.
const { timestamp } = await client.getBlock()
const expiry = args.expiry ? BigInt(args.expiry) : timestamp + WEEK
if (expiry <= timestamp) fail(`--expiry ${expiry} has passed: the chain's time is ${timestamp}`)
const recipient = getAddress(args.recipient)
const randomNonce = () =>
	args.nonce ? BigInt(args.nonce) : BigInt(`0x${randomBytes(8).toString('hex')}`)

let coupon, nonce
if (!safeMode) {
	nonce = randomNonce()
	coupon = await signCoupon({
		key: TEST_SIGNER_KEY,
		order,
		recipient,
		wei,
		expiry,
		nonce
	})
} else {
	let threshold, owners
	try {
		;[threshold, owners] = await Promise.all([
			client.readContract({ address: signer, abi: SAFE_ABI, functionName: 'getThreshold' }),
			client.readContract({ address: signer, abi: SAFE_ABI, functionName: 'getOwners' })
		])
	} catch {
		fail(`the claim signer ${signer} does not answer as a Safe on ${rpcUrl}`)
	}
	let withdrawal = null
	if (services.length > 0) {
		try {
			withdrawal = args.withdrawal ? withdrawalFrom(args.withdrawal) : testWithdrawal()
		} catch (err) {
			fail(err.message)
		}
		nonce = withdrawal.nonce
	} else {
		nonce = randomNonce()
	}
	const words = couponWords({ order, recipient, wei, expiry, nonce })
	console.log(
		`The Safe ${signer} on chain ${chainId}: ${threshold} of ${owners.length} owners sign`
	)
	let safeSigned
	try {
		safeSigned = await collectSafeSignature({
			safe: signer,
			chainId,
			words,
			withdrawal: withdrawal?.id ?? null,
			owners,
			threshold: Number(threshold),
			keys,
			services,
			log: line => console.log(`  ${line}`)
		})
	} catch (err) {
		fail(err.message)
	}
	// The Safe's own answer, as the orderbook asks it at the claim.
	const answer = await client
		.readContract({
			address: signer,
			abi: SAFE_ABI,
			functionName: 'isValidSignature',
			args: [safeSigned.digest, safeSigned.signature]
		})
		.catch(err => fail(`the Safe refused the signatures: ${err.shortMessage ?? err.message}`))
	if (answer !== EIP1271_MAGIC_VALUE)
		fail(`the Safe answered ${answer}, not the EIP-1271 magic value`)
	console.log(
		`The Safe accepts the signatures of ${safeSigned.signers.map(s => s.owner).join(' and ')}`
	)
	if (withdrawal) console.log(`Withdrawal ID: ${withdrawal.id}`)
	coupon = couponString(signer, safeSigned.signature, words)
}

console.log(
	`Coupon for ${formatUnits(wei, 18)} mock HOT to ${recipient}, expiring ${new Date(Number(expiry) * 1000).toISOString()}, nonce ${nonce}`
)
console.log(`Claim link: ${args.url.replace(/\/$/, '')}/claim?c=${coupon}`)
console.log(`Coupon: ${coupon}`)
