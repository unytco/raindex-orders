import { get } from 'svelte/store'
import {
	decodeErrorResult,
	decodeFunctionResult,
	encodeFunctionData,
	isHex,
	numberToHex,
	size,
	slice,
	type Abi,
	type Address,
	type Hex
} from 'viem'
import { orderbookAbi } from '../generated'
import { bridge } from './config'
import { parseCoupon, type CouponConfig, type SignedContextV1Struct } from './coupon'
import {
	ethereumStore,
	getEthereum,
	SWITCH_NETWORK,
	TransactionOutcomeError,
	userRejected,
	waitForTransaction,
	walletChainId,
	writeContract,
	type Eip1193Provider
} from './ethereum'
import { claimOrderStruct } from './network'
import { transactionStore } from './stores/transactionStore'

const CLAIM_REVERTED = `Your claim did not go through, so no ${bridge.tokenName} was paid and your coupon was not used. Claim again.`

const CLAIM_UNCHECKED =
	'Could not check your coupon through your wallet, so nothing was sent. Check your connection and claim again.'

const CLAIM_UNCONFIRMED =
	"Your claim was sent, but its result could not be read. Check your wallet's activity to see whether it went through before you claim again."

const CLAIM_DECLINED = 'You declined the claim in your wallet, so nothing was sent.'

const CLAIM_CANCELLED =
	'You cancelled the claim in your wallet, so nothing was paid and your coupon was not used.'

const CLAIM_REPLACED = `Your wallet replaced the claim with another transaction, so no ${bridge.tokenName} was paid and your coupon was not used. Claim again.`

const CLAIM_PENDING =
	"Your claim is still waiting to be confirmed. Check your wallet's activity before you claim again."

const CLAIM_FAILED =
	"Something went wrong with your claim. Check your wallet's activity before you claim again, and contact Unyt support if it keeps failing."

const REISSUE = 'Contact Unyt support to have the withdrawal re-issued.'
const REISSUE_IF_UNPAID = `If you have not received its ${bridge.tokenName}, contact Unyt support to have the withdrawal re-issued.`

const CLAIM_NOT_SENT =
	"Your wallet did not send the claim. If your wallet's activity shows no claim, claim again."

function refusalMessage(reason: string, coupon: CouponConfig) {
	switch (reason) {
		case 'Nonce already used':
			return `This coupon has already been claimed, so there is nothing left to claim. Its ${bridge.tokenName} went to ${coupon.recipient}.`
		case 'Order expired':
			return `This coupon expired on ${new Date(coupon.expiryTimestamp * 1000).toLocaleString()} and can no longer be claimed. ${REISSUE_IF_UNPAID}`
		case 'Wrong recipient':
			return `This coupon pays ${coupon.recipient}, not the connected wallet. Switch your wallet to that account and claim again.`
		case 'Wrong signer':
			return `This coupon was signed by a key this bridge does not accept. ${REISSUE_IF_UNPAID}`
		case 'InvalidSignature':
			return "This coupon's signature does not match its contents, so it was changed or cut short. Copy the coupon from your Unyt app again."
		case 'Wrong order hash':
		case 'Wrong order owner':
		case 'Wrong orderbook':
		case 'Wrong output token':
		case 'Wrong output vault id':
			return `This coupon does not match this bridge, so it cannot be claimed here. ${REISSUE}`
		case 'Wrong output amount':
		case 'MinimumInput':
			return 'The bridge cannot pay this claim right now, and your coupon was not used. Try again later, and contact Unyt support if it keeps failing.'
		default:
			return 'The bridge refused this claim, and your coupon was not used. Contact Unyt support.'
	}
}

type WalletError = { data?: unknown; message?: unknown }

/** `err` and the errors in it: wallets and nodes nest an eth_call's revert in different places. */
function nestedErrors(err: unknown, depth = 0): WalletError[] {
	if (depth > 5 || typeof err !== 'object' || err === null) return []
	const fields = err as Record<string, unknown>
	const inner = ['data', 'originalError', 'error', 'cause'].map(key => fields[key])
	return [fields, ...inner.flatMap(e => nestedErrors(e, depth + 1))]
}

type Revert = { reason: string; errorName?: string; args?: readonly unknown[]; data?: Hex }

function decodedRevert(data: Hex): Revert {
	try {
		const { errorName, args } = decodeErrorResult({ abi: orderbookAbi as Abi, data })
		return { reason: errorName === 'Error' ? String(args?.[0]) : errorName, errorName, args, data }
	} catch {
		return { reason: slice(data, 0, 4), data }
	}
}

function revertIn(err: unknown): Revert | undefined {
	const errors = nestedErrors(err)
	const data = errors
		.map(e => e.data)
		.find((d): d is Hex => typeof d === 'string' && isHex(d, { strict: true }) && size(d) >= 4)
	if (data) return decodedRevert(data)
	const reverted = errors
		.map(e => /execution reverted(?::\s*(.*\S))?/is.exec(String(e.message)))
		.find(match => match !== null)
	return reverted ? { reason: reverted[1] ?? '' } : undefined
}

async function refusalOf(eth: Eip1193Provider, account: Address, data: Hex, amount: bigint) {
	let result: unknown
	try {
		result = await eth.request({
			method: 'eth_call',
			params: [{ from: account, to: bridge.orderbookAddress, data }, 'latest']
		})
	} catch (err) {
		const revert = revertIn(err)
		if (revert === undefined) throw err
		return { revert, cause: err }
	}
	const [paid] = decodeFunctionResult({
		abi: orderbookAbi,
		functionName: 'takeOrders',
		data: result as Hex
	})
	if (paid !== amount) throw new Error(`The claim would pay ${paid}, not ${amount}`)
	return null
}

function takeOrdersConfig(signedContext: SignedContextV1Struct) {
	const amount = signedContext.context[1]
	return {
		minimumInput: amount,
		maximumInput: amount,
		maximumIORatio: 0n,
		orders: [
			{
				order: claimOrderStruct(bridge),
				inputIOIndex: 0n,
				outputIOIndex: 0n,
				signedContext: [signedContext]
			}
		],
		data: '0x' as Hex
	}
}

type ClaimTrace = {
	step: string
	account?: Address
	chainId?: number
	couponNonce?: Hex
	couponExpiry?: string
	sentHash?: string
	hash?: string
	outcome?: string
	revert?: Revert
	recheck?: 'would pay' | Revert | { error: unknown }
}

class ClaimError extends Error {
	readonly unconfirmed: boolean

	constructor(message: string, options: ErrorOptions & { unconfirmed?: boolean } = {}) {
		super(message, options)
		this.unconfirmed = options.unconfirmed ?? false
	}
}

async function claim(signedContext: SignedContextV1Struct, trace: ClaimTrace): Promise<string> {
	const eth = getEthereum()
	if (!eth) throw new Error('No ethereum provider')
	const account = get(ethereumStore).account as Address | null
	if (!account) throw new Error('Not connected')
	trace.account = account

	trace.step = 'checking the network'
	trace.chainId = await walletChainId(eth)
	if (trace.chainId !== bridge.chain.id) throw new ClaimError(SWITCH_NETWORK)

	const coupon = parseCoupon(signedContext)
	trace.couponNonce = numberToHex(coupon.nonce)
	trace.couponExpiry = new Date(coupon.expiryTimestamp * 1000).toISOString()
	const args = [takeOrdersConfig(signedContext)] as const
	const data = encodeFunctionData({ abi: orderbookAbi, functionName: 'takeOrders', args })
	const check = () => refusalOf(eth, account, data, coupon.withdrawAmount)

	trace.step = 'checking the coupon'
	const refusal = await check().catch(err => {
		throw new ClaimError(CLAIM_UNCHECKED, { cause: err })
	})
	if (refusal) {
		trace.revert = refusal.revert
		throw new ClaimError(refusalMessage(refusal.revert.reason, coupon), { cause: refusal.cause })
	}

	trace.step = 'asking the wallet to send'
	transactionStore.awaitWalletConfirmation()
	const sentHash = await writeContract({
		address: bridge.orderbookAddress,
		abi: orderbookAbi,
		functionName: 'takeOrders',
		args,
		from: account
	}).catch(err => {
		throw new ClaimError(userRejected(err) ? CLAIM_DECLINED : CLAIM_NOT_SENT, { cause: err })
	})
	trace.sentHash = trace.hash = sentHash

	trace.step = 'waiting for confirmation'
	transactionStore.awaitTxReceipt(sentHash)
	try {
		const receipt = await waitForTransaction(sentHash)
		return receipt.transactionHash
	} catch (err) {
		if (!(err instanceof TransactionOutcomeError)) throw err
		trace.hash = err.hash
		trace.outcome = err.outcome
		switch (err.outcome) {
			case 'pending':
				throw new ClaimError(CLAIM_PENDING, { cause: err, unconfirmed: true })
			case 'unreadable':
				throw new ClaimError(CLAIM_UNCONFIRMED, { cause: err, unconfirmed: true })
			case 'cancelled':
				throw new ClaimError(CLAIM_CANCELLED, { cause: err })
		}

		const refusalAfter = await check().catch(readErr => {
			trace.recheck = { error: readErr }
			return undefined
		})
		if (refusalAfter) {
			trace.recheck = trace.revert = refusalAfter.revert
			throw new ClaimError(refusalMessage(refusalAfter.revert.reason, coupon), { cause: err })
		}
		if (refusalAfter === null) trace.recheck = 'would pay'
		throw new ClaimError(err.outcome === 'replaced' ? CLAIM_REPLACED : CLAIM_REVERTED, {
			cause: err
		})
	}
}

function causeChain(err: unknown): unknown[] {
	const chain: unknown[] = []
	for (let link = err; link != null && chain.length < 10; link = (link as Error).cause) {
		chain.push(link)
	}
	return chain
}

export async function claimCoupon(signedContext: SignedContextV1Struct): Promise<string> {
	const trace: ClaimTrace = { step: 'starting' }
	transactionStore.awaitCheck()
	try {
		const hash = await claim(signedContext, trace)
		transactionStore.transactionSuccess(hash)
		return hash
	} catch (err) {
		const failure = err instanceof ClaimError ? err : new ClaimError(CLAIM_FAILED, { cause: err })
		console.error(`Claim failed while ${trace.step}: ${failure.message}`, {
			...trace,
			causes: causeChain(failure)
		})
		transactionStore.transactionError({
			message: failure.message,
			hash: trace.hash,
			unconfirmed: failure.unconfirmed
		})
		throw failure
	}
}
