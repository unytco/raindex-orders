import { get } from 'svelte/store'
import {
	decodeErrorResult,
	decodeFunctionResult,
	encodeFunctionData,
	isHex,
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
	requireBridgeChain,
	TransactionRevertedError,
	userRejected,
	waitForTransaction,
	writeContract,
	type Eip1193Provider
} from './ethereum'
import { claimOrderStruct } from './network'
import { transactionStore } from './stores/transactionStore'
import { errorMessage } from './utils'

const CLAIM_REVERTED = `Your claim did not go through, so no ${bridge.tokenName} was paid and your coupon was not used. Claim again.`

const CLAIM_UNCHECKED =
	'Could not check your coupon through your wallet, so nothing was sent. Check your connection and claim again.'

const CLAIM_UNCONFIRMED =
	"Your claim was sent, but its result could not be read. Check your wallet's activity to see whether it went through before you claim again."

const CLAIM_DECLINED = 'You declined the claim in your wallet, so nothing was sent.'

const REISSUE = 'Contact Unyt support to have the withdrawal re-issued.'
const REISSUE_IF_UNPAID = `If you have not received its ${bridge.tokenName}, contact Unyt support to have the withdrawal re-issued.`

const notSent = (reason: string) =>
	`Your wallet did not send the claim (${reason.replace(/\.$/, '')}). If your wallet's activity shows no claim, claim again.`

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
			return `The bridge refused this claim${reason ? ` (${reason})` : ''}, and your coupon was not used. Contact Unyt support.`
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

function decodedReason(data: Hex) {
	try {
		const { errorName, args } = decodeErrorResult({ abi: orderbookAbi as Abi, data })
		return errorName === 'Error' ? String(args?.[0]) : errorName
	} catch {
		return slice(data, 0, 4)
	}
}

function revertReason(err: unknown): string | undefined {
	const errors = nestedErrors(err)
	const data = errors
		.map(e => e.data)
		.find((d): d is Hex => typeof d === 'string' && isHex(d, { strict: true }) && size(d) >= 4)
	if (data) return decodedReason(data)
	const reverted = errors
		.map(e => /execution reverted(?::\s*(.*\S))?/is.exec(String(e.message)))
		.find(match => match !== null)
	return reverted ? (reverted[1] ?? '') : undefined
}

async function refusalOf(eth: Eip1193Provider, account: Address, data: Hex, amount: bigint) {
	let result: unknown
	try {
		result = await eth.request({
			method: 'eth_call',
			params: [{ from: account, to: bridge.orderbookAddress, data }, 'latest']
		})
	} catch (err) {
		const reason = revertReason(err)
		if (reason === undefined) throw err
		return { reason, cause: err }
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

async function claim(signedContext: SignedContextV1Struct): Promise<string> {
	const eth = getEthereum()
	if (!eth) throw new Error('No ethereum provider')
	const account = get(ethereumStore).account as Address | null
	if (!account) throw new Error('Not connected')
	await requireBridgeChain(eth)

	const coupon = parseCoupon(signedContext)
	const args = [takeOrdersConfig(signedContext)] as const
	const data = encodeFunctionData({ abi: orderbookAbi, functionName: 'takeOrders', args })
	const check = () => refusalOf(eth, account, data, coupon.withdrawAmount)

	const refusal = await check().catch(err => {
		throw new Error(CLAIM_UNCHECKED, { cause: err })
	})
	if (refusal) throw new Error(refusalMessage(refusal.reason, coupon), { cause: refusal.cause })

	const hash = await writeContract({
		address: bridge.orderbookAddress,
		abi: orderbookAbi,
		functionName: 'takeOrders',
		args,
		from: account
	}).catch(err => {
		const message = userRejected(err)
			? CLAIM_DECLINED
			: notSent(errorMessage(err, 'no reason given'))
		throw new Error(message, { cause: err })
	})
	transactionStore.awaitTxReceipt(hash)

	try {
		await waitForTransaction(hash)
	} catch (err) {
		if (!(err instanceof TransactionRevertedError)) {
			throw new Error(CLAIM_UNCONFIRMED, { cause: err })
		}
		const refusalAfter = await check().catch(readErr => {
			console.error('Could not read why the claim reverted:', readErr)
			return null
		})
		throw new Error(refusalAfter ? refusalMessage(refusalAfter.reason, coupon) : CLAIM_REVERTED, {
			cause: err
		})
	}
	return hash
}

export async function claimCoupon(signedContext: SignedContextV1Struct): Promise<string> {
	transactionStore.awaitWalletConfirmation()
	try {
		const hash = await claim(signedContext)
		transactionStore.transactionSuccess(hash)
		return hash
	} catch (err) {
		transactionStore.transactionError({ message: errorMessage(err, 'Claim failed') })
		throw err
	}
}
