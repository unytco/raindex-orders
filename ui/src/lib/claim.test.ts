import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { get } from 'svelte/store'
import { encodeErrorResult, encodeFunctionResult, parseAbi, type Hex } from 'viem'
import { orderbookAbi } from '../generated'
import { deserializeSignedContext, parseCoupon } from './coupon'
import { TransactionStatus } from './stores/transactionStore'
import { asBuild, BUILDS } from './testing/builds'

// The coupon of HASH, a claim that ran out of gas on Sepolia.
const COUPON = deserializeSignedContext(
	'0x8E72b7568738da52ca3DCd9b24E178127A4E7d37,0xd6cde27f587d1d4b5dd3bd77c80ab5e8c1984c4b449a5f5947530e2be16026ae6b9d81ef48533195f4fac523d52598729f693900e1ec40901f1e7de1d281c5581c,726553263277270008480206851738792059068710201360,35000000000000000000,1792079901,42941365433660945573526868378572896801276519612010928011051331832534249141753,1300945060633283894583661816534861012306758682838,1442425860134574572653119565674449928420894127102,1340384803335777240622375245841800624658726815561,108043606565222972236900316128309391016550688326814185311821020602083120460619,35545326640015910568712478866971482885679877279279017985578456828248036901145'
)
const RECIPIENT = parseCoupon(COUPON).recipient
const OTHER_WALLET = '0x1111111111111111111111111111111111111111'
const EXPIRES = new Date(1792079901 * 1000).toLocaleString()
const HASH = '0xe76a7f6dff5bd4384292b7ebf8bba2496021e3eb90a8cf5ee2cbf730d6021898'
// The takeOrders call that transaction made to the orderbook.
const TAKE_ORDERS =
	'0x8a44689c0000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000001e5b8fa8fe2ac0000000000000000000000000000000000000000000000000001e5b8fa8fe2ac0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000005a0000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000080000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000260000000000000000000000000e3e064e3c2eef66cb93da8d8114f5084e92f48d600000000000000000000000000000000000000000000000000000000000000010000000000000000000000008853d126bc23a45b9f807739b6ea0b38ef56900500000000000000000000000023f77e7bc935503e437166498d7d72f2ea290e1f0000000000000000000000000a1369aee76570cc7404492d55a5d1468d5a9b4b00000000000000000000000000000000000000000000000000000000000000e000000000000000000000000000000000000000000000000000000000000001600000000000000000000000000000000000000000000000000000000000000001000000000000000000000000555fa2f68dd9b7db6c8ca1f03bfc317ce61e90280000000000000000000000000000000000000000000000000000000000000012eede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b0000000000000000000000000000000000000000000000000000000000000001000000000000000000000000eac8eeee9f84f3e3f592e9d8604100ea1b7887490000000000000000000000000000000000000000000000000000000000000012eede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000200000000000000000000000008e72b7568738da52ca3dcd9b24e178127a4e7d37000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000000000000000000001a000000000000000000000000000000000000000000000000000000000000000090000000000000000000000007f43c66d6283bc54384dc5b178f65b4af1c12810000000000000000000000000000000000000000000000001e5b8fa8fe2ac0000000000000000000000000000000000000000000000000000000000006ad0f81d5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9000000000000000000000000e3e064e3c2eef66cb93da8d8114f5084e92f48d6000000000000000000000000fca89cd12ba1346b1ac570ed988ab43b812733fe000000000000000000000000eac8eeee9f84f3e3f592e9d8604100ea1b788749eede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b4e95f122036e7d6cf3baca9b6e95820c751d1ee3db4eb2125409b87ad60281190000000000000000000000000000000000000000000000000000000000000041d6cde27f587d1d4b5dd3bd77c80ab5e8c1984c4b449a5f5947530e2be16026ae6b9d81ef48533195f4fac523d52598729f693900e1ec40901f1e7de1d281c5581c000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'

// What Sepolia's eth_call answered for that call before the coupon expired: as it stands,
// from another wallet, with the block time past its expiry, and with its expiry altered.
const SEPOLIA = {
	nonceUsed:
		'0x08c379a0000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000124e6f6e636520616c726561647920757365640000000000000000000000000000',
	wrongRecipient:
		'0x08c379a00000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000000f57726f6e6720726563697069656e740000000000000000000000000000000000',
	expired:
		'0x08c379a00000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000000d4f72646572206578706972656400000000000000000000000000000000000000',
	invalidSignature: '0x52bf98480000000000000000000000000000000000000000000000000000000000000000'
} as const

const ensure = (reason: string) =>
	encodeErrorResult({
		abi: parseAbi(['error Error(string)']),
		errorName: 'Error',
		args: [reason]
	})

const ALREADY_CLAIMED = (token: string) =>
	`This coupon has already been claimed, so there is nothing left to claim. Its ${token} went to ${RECIPIENT}.`
const REISSUE_IF_UNPAID =
	'If you have not received its mock HOT, contact Unyt support to have the withdrawal re-issued.'
const OTHER_BRIDGE =
	'This coupon does not match this bridge, so it cannot be claimed here. Contact Unyt support to have the withdrawal re-issued.'
const VAULT_SHORT =
	'The bridge cannot pay this claim right now, and your coupon was not used. Try again later, and contact Unyt support if it keeps failing.'
const NO_REASON =
	'The bridge refused this claim, and your coupon was not used. Contact Unyt support.'
const REVERTED =
	'Your claim did not go through, so no mock HOT was paid and your coupon was not used. Claim again.'
const UNCHECKED =
	'Could not check your coupon through your wallet, so nothing was sent. Check your connection and claim again.'
const UNCONFIRMED =
	"Your claim was sent, but its result could not be read. Check your wallet's activity to see whether it went through before you claim again."

const PAYS = encodeFunctionResult({
	abi: orderbookAbi,
	functionName: 'takeOrders',
	result: [35000000000000000000n, 0n]
})

const nodeRevert = (data: Hex) => ({ code: 3, message: 'execution reverted', data })
const metaMaskRevert = (data: Hex) => ({
	code: -32603,
	message: 'Internal JSON-RPC error.',
	data: nodeRevert(data)
})
const REVERT_SHAPES = {
	node: nodeRevert,
	'MetaMask nesting': metaMaskRevert,
	'original error nesting': (data: Hex) => ({
		code: -32603,
		data: { originalError: nodeRevert(data) }
	}),
	'-32000 with data': (data: Hex) => ({ code: -32000, message: 'Execution reverted', data })
}

type Answer = Hex | object
type Request = { method: string; params?: unknown[] }
type Wallet = {
	ethCallAnswers: Answer[]
	receipt?: Answer
	send?: Answer
	network?: 'sepolia' | 'mainnet'
	walletChain?: Hex
	account?: string
	accountDuringCheck?: string
}

async function wallet({
	ethCallAnswers,
	receipt = { status: '0x1' },
	send = HASH,
	network = 'sepolia',
	walletChain = network === 'sepolia' ? '0xaa36a7' : '0x1',
	account = RECIPIENT,
	accountDuringCheck
}: Wallet) {
	const asked: string[] = []
	const params: Record<string, unknown> = {}
	const modalWhen: Record<string, unknown> = {}
	let modal = (): { status?: unknown } => ({})
	let switchAccount: (to: string) => void = () => {}
	const answer = (value: Answer | undefined) => {
		if (value === undefined) throw new Error('unexpected request')
		if (typeof value === 'string' || 'status' in value) return value
		throw value
	}
	const request = vi.fn(async ({ method, params: [first] = [] }: Request) => {
		asked.push(method)
		params[method] = first
		modalWhen[method] = modal().status
		if (method === 'eth_chainId') return walletChain
		if (method === 'eth_call') {
			if (accountDuringCheck) switchAccount(accountDuringCheck)
			return answer(ethCallAnswers.shift())
		}
		if (method === 'eth_sendTransaction') return answer(send)
		if (method === 'eth_getTransactionReceipt') return answer(receipt)
		throw new Error(`unexpected ${method}`)
	})
	vi.stubGlobal('window', { ethereum: { request, on: vi.fn() } })
	const { claim, ethereum, store } = await asBuild(BUILDS[network], async () => ({
		claim: await import('./claim'),
		ethereum: await import('./ethereum'),
		store: await import('./stores/transactionStore')
	}))
	ethereum.ethereumStore.set({
		isConnected: true,
		account,
		chainId: network === 'sepolia' ? 11155111 : 1,
		isLoading: false,
		error: null
	})
	modal = () => get(store.transactionStore)
	switchAccount = to => ethereum.ethereumStore.update(s => ({ ...s, account: to }))
	return { asked, params, modalWhen, modal, claim: () => claim.claimCoupon(COUPON) }
}

beforeEach(() => {
	vi.spyOn(console, 'error').mockImplementation(() => {})
})
afterEach(() => {
	vi.unstubAllGlobals()
	vi.restoreAllMocks()
})

describe.each(Object.entries(REVERT_SHAPES))(
	'a claim the order refuses, told as %s,',
	(_shape, revert) => {
		it.each([
			['already claimed', SEPOLIA.nonceUsed, ALREADY_CLAIMED('mock HOT')],
			[
				'expired',
				SEPOLIA.expired,
				`This coupon expired on ${EXPIRES} and can no longer be claimed. ${REISSUE_IF_UNPAID}`
			],
			[
				'altered',
				SEPOLIA.invalidSignature,
				"This coupon's signature does not match its contents, so it was changed or cut short. Copy the coupon from your Unyt app again."
			],
			[
				'signed by a key the order does not accept',
				ensure('Wrong signer'),
				`This coupon was signed by a key this bridge does not accept. ${REISSUE_IF_UNPAID}`
			],
			['for another order', ensure('Wrong order hash'), OTHER_BRIDGE],
			['for another order owner', ensure('Wrong order owner'), OTHER_BRIDGE],
			['for another orderbook', ensure('Wrong orderbook'), OTHER_BRIDGE],
			['for another token', ensure('Wrong output token'), OTHER_BRIDGE],
			['for another vault', ensure('Wrong output vault id'), OTHER_BRIDGE],
			['more than the vault holds', ensure('Wrong output amount'), VAULT_SHORT],
			[
				'from an empty vault',
				encodeErrorResult({
					abi: orderbookAbi,
					errorName: 'MinimumInput',
					args: [35000000000000000000n, 0n]
				}),
				VAULT_SHORT
			],
			[
				'for a reason the site does not know',
				ensure('Something new'),
				'The bridge refused this claim (Something new), and your coupon was not used. Contact Unyt support.'
			],
			['with an empty reason', ensure(''), NO_REASON],
			[
				'with an error the site cannot decode',
				'0xdeadbeef',
				'The bridge refused this claim (0xdeadbeef), and your coupon was not used. Contact Unyt support.'
			]
		] as const)('%s is not sent, and says why', async (_case, data, message) => {
			const refusal = revert(data)
			const { asked, claim, modal } = await wallet({ ethCallAnswers: [refusal] })

			await expect(claim()).rejects.toMatchObject({ message, cause: refusal })
			expect(asked).not.toContain('eth_sendTransaction')
			expect(modal()).toMatchObject({
				status: TransactionStatus.ERROR,
				error: { message },
				hash: ''
			})
		})
	}
)

describe('a claim the order refuses', () => {
	it('for another wallet names the wallet the coupon pays', async () => {
		const { params, claim } = await wallet({
			ethCallAnswers: [nodeRevert(SEPOLIA.wrongRecipient)],
			account: OTHER_WALLET
		})

		await expect(claim()).rejects.toMatchObject({
			message: `This coupon pays ${RECIPIENT}, not the connected wallet. Switch your wallet to that account and claim again.`
		})
		expect(params.eth_call).toMatchObject({ from: OTHER_WALLET })
	})

	it('names its own token on a MainNet build', async () => {
		const { claim } = await wallet({
			ethCallAnswers: [nodeRevert(SEPOLIA.nonceUsed)],
			network: 'mainnet'
		})

		await expect(claim()).rejects.toMatchObject({ message: ALREADY_CLAIMED('HOT') })
	})

	it.each([
		['execution reverted: Nonce already used', ALREADY_CLAIMED('mock HOT')],
		['execution reverted: Nonce already used\n', ALREADY_CLAIMED('mock HOT')],
		['Execution reverted', NO_REASON]
	])('told only as "%s" is not sent, and says why', async (reverted, message) => {
		const { asked, claim } = await wallet({
			ethCallAnswers: [{ code: 3, message: reverted, data: '0x' }]
		})

		await expect(claim()).rejects.toMatchObject({ message })
		expect(asked).not.toContain('eth_sendTransaction')
	})
})

describe('a claim the order would pay', () => {
	it('is sent as checked, and shows success with its hash once mined', async () => {
		const { asked, params, modalWhen, claim, modal } = await wallet({ ethCallAnswers: [PAYS] })

		await expect(claim()).resolves.toBe(HASH)
		const call = { from: RECIPIENT, to: BUILDS.sepolia.PUBLIC_ORDERBOOK_ADDRESS, data: TAKE_ORDERS }
		expect(params.eth_call).toMatchObject(call)
		expect(params.eth_sendTransaction).toMatchObject(call)
		expect(asked.filter(m => m === 'eth_sendTransaction')).toHaveLength(1)
		expect(modalWhen).toMatchObject({
			eth_call: TransactionStatus.PENDING_WALLET,
			eth_sendTransaction: TransactionStatus.PENDING_WALLET,
			eth_getTransactionReceipt: TransactionStatus.PENDING_TX
		})
		expect(modal()).toMatchObject({ status: TransactionStatus.SUCCESS, hash: HASH })
	})

	it('that reverts on chain, and would still pay, says it did not go through', async () => {
		const { claim, modal } = await wallet({
			ethCallAnswers: [PAYS, PAYS],
			receipt: { status: '0x0' }
		})

		await expect(claim()).rejects.toMatchObject({ message: REVERTED })
		expect(modal()).toMatchObject({ error: { message: REVERTED }, hash: HASH })
	})

	it('that reverts because the coupon was claimed meanwhile says so', async () => {
		const { claim, modal } = await wallet({
			ethCallAnswers: [PAYS, nodeRevert(SEPOLIA.nonceUsed)],
			receipt: { status: '0x0' }
		})

		await expect(claim()).rejects.toMatchObject({ message: ALREADY_CLAIMED('mock HOT') })
		expect(modal()).toMatchObject({ hash: HASH })
	})

	it('that reverts, when the reason cannot be read, still says it did not go through', async () => {
		const { claim } = await wallet({
			ethCallAnswers: [PAYS, new Error('Failed to fetch')],
			receipt: { status: '0x0' }
		})

		await expect(claim()).rejects.toMatchObject({ message: REVERTED })
	})

	it('whose receipt cannot be read says to look before claiming again', async () => {
		const { asked, claim, modal } = await wallet({
			ethCallAnswers: [PAYS],
			receipt: { code: -32603, message: 'Internal JSON-RPC error.' }
		})

		await expect(claim()).rejects.toMatchObject({ message: UNCONFIRMED })
		expect(asked.filter(m => m === 'eth_call')).toHaveLength(1)
		expect(modal()).toMatchObject({ error: { message: UNCONFIRMED }, hash: HASH })
	})

	it('that the user declines in the wallet says nothing was sent', async () => {
		const { claim, modal } = await wallet({
			ethCallAnswers: [PAYS],
			send: { code: 4001, message: 'MetaMask Tx Signature: User denied transaction signature.' }
		})

		const message = 'You declined the claim in your wallet, so nothing was sent.'
		await expect(claim()).rejects.toMatchObject({ message })
		expect(modal()).toMatchObject({ error: { message }, hash: '' })
	})

	it('that the wallet fails to send says to look before claiming again', async () => {
		const { claim } = await wallet({
			ethCallAnswers: [PAYS],
			send: { code: -32603, message: 'Internal JSON-RPC error.' }
		})

		await expect(claim()).rejects.toMatchObject({
			message:
				"Your wallet did not send the claim (Internal JSON-RPC error). If your wallet's activity shows no claim, claim again."
		})
	})

	it('is sent from the account it was checked for, even if the wallet switches', async () => {
		const { params, claim } = await wallet({
			ethCallAnswers: [PAYS],
			accountDuringCheck: OTHER_WALLET
		})

		await claim()
		expect(params.eth_sendTransaction).toMatchObject({ from: RECIPIENT })
	})
})

describe('a claim the wallet cannot check', () => {
	it.each([
		['the read fails', new Error('Failed to fetch')],
		['the wallet reports an internal error', { code: -32603, message: 'Internal JSON-RPC error.' }],
		['the wallet reports text as data', { code: -32603, data: 'header not found' }],
		['the orderbook answers nothing', '0x'],
		['the orderbook answers too little', '0x12345678'],
		['the orderbook answers a revert as a result', ensure('Nonce already used')]
	] as const)('is not sent when %s', async (_case, failure) => {
		const { asked, claim } = await wallet({ ethCallAnswers: [failure] })

		await expect(claim()).rejects.toMatchObject({ message: UNCHECKED })
		expect(asked).not.toContain('eth_sendTransaction')
	})

	it('is not checked or sent while the wallet is on another network', async () => {
		const { asked, claim } = await wallet({ ethCallAnswers: [PAYS], walletChain: '0x1' })

		await expect(claim()).rejects.toMatchObject({
			message: 'Switch your wallet to Sepolia Testnet first'
		})
		expect(asked).not.toContain('eth_call')
		expect(asked).not.toContain('eth_sendTransaction')
	})
})
