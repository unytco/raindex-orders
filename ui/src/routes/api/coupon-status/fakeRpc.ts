import { vi } from 'vitest'
import { decodeFunctionData, encodeFunctionResult, multicall3Abi, parseAbi, type Hex } from 'viem'

const MULTICALL3 = '0xca11bde05977b3631167028862be2a173976ca11'
const EIP1271_MAGIC_VALUE = '0x1626ba7e00000000000000000000000000000000000000000000000000000000'

const readAbi = parseAbi([
	'function get(uint256 namespace, uint256 key) view returns (uint256)',
	'function isValidSignature(bytes32 hash, bytes signature) view returns (bytes32)',
	'function getChainId() view returns (uint256)',
	'function getBlockNumber() view returns (uint256)',
	'function getCurrentBlockTimestamp() view returns (uint256)'
])

/** What the RPC answers: one chain, the claim order's store, and its signer. */
export type FakeChain = {
	chainId: bigint
	block: bigint
	timestamp: number
	store: string
	namespace: bigint
	flags: Map<bigint, bigint>
	/** A contract signer's EIP-1271 verdict. A key signer has no code and answers nothing. */
	contractSigner?: { address: string; accepts: (hash: Hex, signature: Hex) => boolean }
}

type RpcRequest = { method: string; params: [{ to: string; data: Hex }, string] }
type SignatureCheck = { signer: string; hash: Hex; signature: Hex; accepted: boolean }

/** A JSON-RPC endpoint answering Multicall3 aggregate3 calls from `chain`. */
export function fakeRpc(chain: FakeChain) {
	const requests: RpcRequest[] = []
	const storeReads: bigint[][] = []
	const signatureChecks: SignatureCheck[] = []
	const fetch = vi.fn(async (_url: string, init: RequestInit) => {
		const request = JSON.parse(String(init.body))
		requests.push(request)
		const reads: bigint[] = []
		storeReads.push(reads)
		const [{ to, data }] = request.params
		if (request.method !== 'eth_call' || to.toLowerCase() !== MULTICALL3) {
			throw new Error(`unexpected ${request.method} to ${to}`)
		}
		const { args } = decodeFunctionData({ abi: multicall3Abi, data })
		const results = (args[0] as readonly { target: string; callData: Hex }[]).map(call => {
			const decoded = decodeFunctionData({ abi: readAbi, data: call.callData })
			const target = call.target.toLowerCase()
			if (decoded.functionName === 'isValidSignature') {
				const [hash, signature] = decoded.args
				const signer = chain.contractSigner
				const isSigner = signer && target === signer.address.toLowerCase()
				const accepted = !!isSigner && signer.accepts(hash, signature)
				signatureChecks.push({ signer: target, hash, signature, accepted })
				if (!isSigner) return { success: true, returnData: '0x' as Hex }
				if (!accepted) return { success: false, returnData: '0x' as Hex }
				const returnData = encodeFunctionResult({
					abi: readAbi,
					functionName: 'isValidSignature',
					result: EIP1271_MAGIC_VALUE
				})
				return { success: true, returnData }
			}
			let value: bigint
			if (target === chain.store && decoded.functionName === 'get') {
				const [namespace, key] = decoded.args
				if (namespace !== chain.namespace) throw new Error(`read namespace ${namespace}`)
				reads.push(key)
				value = chain.flags.get(key) ?? 0n
			} else if (target === MULTICALL3 && decoded.functionName === 'getChainId') {
				value = chain.chainId
			} else if (target === MULTICALL3 && decoded.functionName === 'getBlockNumber') {
				value = chain.block
			} else if (target === MULTICALL3 && decoded.functionName === 'getCurrentBlockTimestamp') {
				value = BigInt(chain.timestamp)
			} else {
				throw new Error(`unexpected ${decoded.functionName} on ${target}`)
			}
			const returnData = encodeFunctionResult({
				abi: readAbi,
				functionName: decoded.functionName,
				result: value
			})
			return { success: true, returnData }
		})
		const result = encodeFunctionResult({
			abi: multicall3Abi,
			functionName: 'aggregate3',
			result: results
		})
		return Response.json({ jsonrpc: '2.0', id: request.id, result })
	})
	vi.stubGlobal('fetch', fetch)
	return { fetch, requests, storeReads, signatureChecks }
}

/** A Workers cache held in memory, one per test, as one data centre's would be. */
export function fakeCache() {
	const entries = new Map<string, string>()
	const store = {
		match: vi.fn(async (key: string) => {
			const body = entries.get(key)
			return body === undefined ? undefined : new Response(body)
		}),
		put: vi.fn(async (key: string, response: Response) => {
			entries.set(key, await response.text())
		})
	}
	return { entries, store, open: vi.fn(async () => store) }
}
