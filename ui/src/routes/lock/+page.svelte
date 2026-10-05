<script lang="ts">
	import { Button, Card, Input, Label, Helper, Spinner, Alert } from 'flowbite-svelte'
	import { erc20Abi } from '../../generated'
	import { holoLockVaultAbi } from '$lib/lockVaultAbi'
	import { formatUnits, parseUnits, maxUint256, type Hex } from 'viem'
	import { transactionStore } from '$lib/stores/transactionStore'
	import { onMount } from 'svelte'
	import { browser } from '$app/environment'
	import TransactionModal, { LOCK_FINALIZE_LINE } from '$lib/components/TransactionModal.svelte'
	import TransactionReceipt from '$lib/components/TransactionReceipt.svelte'
	import ConnectWalletModal from '$lib/components/ConnectWalletModal.svelte'
	import WrongNetwork from '$lib/components/WrongNetwork.svelte'
	import { bridge } from '$lib/config'
	import {
		ethereumStore,
		onWrongNetwork,
		connectWallet,
		readContract,
		writeContract,
		waitForTransaction
	} from '$lib/ethereum'
	import { isHolochainKey, holochainKeyTo32ByteHex, errorMessage } from '$lib/utils'

	let amount = ''
	let agentInput = ''
	let amountPrefilledFromUrl = false
	let agentPrefilledFromUrl = false
	let isLoading = false
	let error = ''
	// Replaces the form once a lock confirms, until the page is reloaded.
	let lockReceipt: { amount: string; hash: string } | undefined

	let tokenBalance: bigint = 0n
	let tokenAllowance: bigint = 0n
	let minLockAmount: bigint = 0n
	let tokenSymbol = 'HOT'
	let tokenDecimals = 18

	const lockVaultAddress = bridge.lockVaultAddress
	const tokenAddress = bridge.tokenAddress

	$: isConnected = $ethereumStore.isConnected
	$: account = $ethereumStore.account

	async function fetchContractData() {
		if (!isConnected || !account || $onWrongNetwork) return

		try {
			tokenBalance = (await readContract({
				address: tokenAddress,
				abi: erc20Abi,
				functionName: 'balanceOf',
				args: [account]
			})) as bigint

			tokenAllowance = (await readContract({
				address: tokenAddress,
				abi: erc20Abi,
				functionName: 'allowance',
				args: [account, lockVaultAddress]
			})) as bigint

			tokenSymbol = (await readContract({
				address: tokenAddress,
				abi: erc20Abi,
				functionName: 'symbol'
			})) as string

			tokenDecimals = (await readContract({
				address: tokenAddress,
				abi: erc20Abi,
				functionName: 'decimals'
			})) as number

			minLockAmount = (await readContract({
				address: lockVaultAddress,
				abi: holoLockVaultAbi,
				functionName: 'minLockAmount'
			})) as bigint
		} catch (e) {
			console.error('Error fetching contract data:', e)
		}
	}

	$: if (isConnected && account && !$onWrongNetwork) {
		fetchContractData()
	}

	function getAgentHex(input: string): string {
		if (!input || !isHolochainKey(input)) return ''
		try {
			return holochainKeyTo32ByteHex(input)
		} catch {
			return ''
		}
	}
	$: agentHex = getAgentHex(agentInput)

	function formatToken(value: bigint, decimals: number): string {
		const full = formatUnits(value, decimals)
		const [whole, frac] = full.split('.')
		if (!frac) return whole
		const trimmed = frac.slice(0, MAX_DECIMAL_PLACES).replace(/0+$/, '')
		return trimmed ? `${whole}.${trimmed}` : whole
	}

	onMount(() => {
		if (browser) {
			try {
				const urlParams = new URL(window.location.href).searchParams
				const urlAmount = urlParams.get('amount')
				const urlAgent = urlParams.get('agent')

				if (urlAmount) {
					const parsedAmount = parseFloat(urlAmount)
					if (!isNaN(parsedAmount) && parsedAmount > 0) {
						amount = urlAmount
						amountPrefilledFromUrl = true
					} else {
						console.warn('Invalid amount parameter in URL:', urlAmount)
					}
				}

				if (urlAgent && isHolochainKey(urlAgent)) {
					agentInput = urlAgent
					agentPrefilledFromUrl = true
				} else if (urlAgent) {
					console.warn('Invalid agent parameter in URL: expected Holochain key (uhCA...)', urlAgent)
				}
			} catch (e) {
				console.error('Error reading URL parameters:', e)
				// Page continues to work normally even if URL parsing fails
			}
		}
	})

	const MAX_DECIMAL_PLACES = 6

	// Handle lock (runs approve first if needed, then lock — single action for the user)
	async function handleLock() {
		if (!amount || !agentHex) return

		if (!agentHex) {
			error = 'Invalid Unyt agent public key. Provide a Holochain agent key (uhCA...).'
			return
		}

		const parts = amount.split('.')
		if (parts.length === 2 && parts[1].length > MAX_DECIMAL_PLACES) {
			error = `Amount can have at most ${MAX_DECIMAL_PLACES} decimal places.`
			return
		}

		error = ''
		isLoading = true

		try {
			const amountWei = parseUnits(amount, tokenDecimals)

			if (amountWei < minLockAmount) {
				error = `Amount must be at least ${formatToken(minLockAmount, tokenDecimals)} ${tokenSymbol}`
				isLoading = false
				transactionStore.reset()
				return
			}

			// If allowance is insufficient, approve unlimited once (one confirmation), then lock (one confirmation).
			// After first time, only lock is needed (single confirmation).
			if (tokenAllowance < amountWei) {
				transactionStore.awaitWalletConfirmation()
				const approveHash = await writeContract({
					address: tokenAddress,
					abi: erc20Abi,
					functionName: 'approve',
					args: [lockVaultAddress, maxUint256]
				})
				transactionStore.awaitTxReceipt(approveHash)
				await waitForTransaction(approveHash)
				transactionStore.reset()
				await fetchContractData()
			}

			transactionStore.awaitWalletConfirmation(true)
			const hash = await writeContract({
				address: lockVaultAddress,
				abi: holoLockVaultAbi,
				functionName: 'lock',
				args: [amountWei, agentHex as Hex]
			})

			transactionStore.awaitTxReceipt(hash)
			await waitForTransaction(hash)
			transactionStore.transactionSuccess(hash)
			lockReceipt = { amount: `${formatToken(amountWei, tokenDecimals)} ${tokenSymbol}`, hash }
			await fetchContractData()
		} catch (e) {
			error = errorMessage(e, 'Transaction failed')
			transactionStore.transactionError({ message: error })
			console.error(e)
		} finally {
			isLoading = false
		}
	}

	$: hasValidAgent = !!agentHex
	let showConnectModal = false
	async function handleConnect() {
		showConnectModal = true
		await connectWallet()
		showConnectModal = false
	}
</script>

<Card size="xl" class="flex flex-col gap-4">
	{#if lockReceipt}
		<TransactionReceipt
			title="Lock almost complete"
			message={LOCK_FINALIZE_LINE}
			hash={lockReceipt.hash}
		>
			<span class="text-gray-600">Amount:</span>
			<span class="font-semibold">{lockReceipt.amount}</span>
		</TransactionReceipt>
	{:else}
		<h1 class="text-2xl font-bold">Lock {bridge.tokenName}</h1>
		<p class="text-gray-600">
			Lock your {bridge.tokenName} tokens to receive {bridge.tokenName} on Unyt. Your {bridge.tokenName}
			will be credited to the specified agent.
		</p>

		{#if !isConnected}
			<Alert color="blue">Please connect your wallet to continue.</Alert>
			<Button on:click={handleConnect}>Connect Wallet</Button>
		{:else if $onWrongNetwork}
			<WrongNetwork />
		{:else}
			<div class="space-y-4">
				<div class="rounded-lg bg-gray-50 p-4">
					<p class="text-sm text-gray-600">Your {tokenSymbol} Balance</p>
					<p class="text-xl font-semibold">
						{formatToken(tokenBalance, tokenDecimals)}
						{tokenSymbol}
					</p>
				</div>

				<div>
					<Label for="amount" class="mb-2">Amount to Lock</Label>
					{#if amountPrefilledFromUrl}
						<div
							class="block w-full cursor-default select-none rounded-lg border border-gray-300 bg-gray-50 p-2.5 text-sm text-gray-900 dark:border-gray-600 dark:bg-gray-700 dark:text-white"
							style="user-select: none; -webkit-user-select: none;"
							aria-readonly="true"
						>
							{amount}
						</div>
					{:else}
						<Input
							id="amount"
							type="number"
							step="0.000001"
							placeholder="0.0"
							bind:value={amount}
							disabled={isLoading}
						/>
					{/if}
					<Helper class="mt-1">
						Minimum: {formatToken(minLockAmount, tokenDecimals)}
						{tokenSymbol}
					</Helper>
				</div>

				<div>
					<Label for="agent" class="mb-2">Unyt Agent Public Key (Holochain key)</Label>
					{#if agentPrefilledFromUrl}
						<div
							class="block w-full cursor-default select-none break-all rounded-lg border border-gray-300 bg-gray-50 p-2.5 text-sm text-gray-900 dark:border-gray-600 dark:bg-gray-700 dark:text-white"
							style="user-select: none; -webkit-user-select: none;"
							aria-readonly="true"
						>
							{agentInput}
						</div>
					{:else}
						<Input
							id="agent"
							type="text"
							placeholder="uhCA..."
							bind:value={agentInput}
							disabled={isLoading}
						/>
					{/if}
					<Helper class="mt-1">
						{#if agentPrefilledFromUrl}
							Agent key from URL (read-only). This is where your {bridge.tokenName} will be sent.
						{:else}
							Paste your Holochain agent key (e.g. uhCA...). This is where your {bridge.tokenName} will
							be sent. It is converted to hex for the contract below.
						{/if}
					</Helper>
					{#if agentInput && hasValidAgent}
						<div
							class="mt-2 space-y-2 rounded-lg border border-gray-200 bg-gray-50 p-3 dark:border-gray-600 dark:bg-gray-700"
						>
							<div>
								<p class="text-xs font-medium text-gray-500 dark:text-gray-400">Holochain key</p>
								<p class="break-all text-base font-semibold text-gray-900 dark:text-white">
									{agentInput}
								</p>
							</div>
							<div>
								<p class="text-xs font-medium text-gray-500 dark:text-gray-400">
									Ethereum (hex, used for lock)
								</p>
								<p class="break-all text-base font-semibold text-gray-900 dark:text-white">
									{agentHex}
								</p>
							</div>
						</div>
					{:else if agentInput}
						<Helper color="red" class="mt-1">
							Invalid format. Provide a Holochain agent key (uhCA...).
						</Helper>
					{/if}
				</div>

				{#if error}
					<Alert color="red">{error}</Alert>
				{/if}

				<!-- Single action: Lock (approve is done automatically first if needed) -->
				<div class="flex flex-row gap-2">
					<Button
						class="w-fit disabled:!bg-primary-300 disabled:!opacity-100"
						on:click={handleLock}
						disabled={isLoading || !amount || !hasValidAgent}
					>
						{#if isLoading}
							<Spinner size="4" class="mr-2" />
						{/if}
						Lock {tokenSymbol}
					</Button>
				</div>
			</div>
		{/if}
	{/if}
</Card>

<TransactionModal />
<ConnectWalletModal open={showConnectModal} />
