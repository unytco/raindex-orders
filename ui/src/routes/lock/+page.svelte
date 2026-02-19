<script lang="ts">
	import { Button, Card, Input, Label, Helper, Spinner, Alert } from 'flowbite-svelte'
	import { erc20Abi } from '../../generated'
	import { holoLockVaultAbi } from '$lib/lockVaultAbi'
	import { formatUnits, parseUnits, type Hex } from 'viem'
	import { transactionStore } from '$lib/stores/transactionStore'
	import { onMount } from 'svelte'
	import { browser } from '$app/environment'
	import TransactionModal from '$lib/components/TransactionModal.svelte'
	import { PUBLIC_LOCK_VAULT_ADDRESS, PUBLIC_TOKEN_ADDRESS } from '$env/static/public'
	import {
		ethereumStore,
		connectWallet,
		readContract,
		writeContract,
		waitForTransaction
	} from '$lib/ethereum'
	import { isHolochainKey, holochainKeyTo32ByteHex } from '$lib/utils'

	// Form state
	let amount = ''
	let agentInput = ''
	let amountPrefilledFromUrl = false
	let agentPrefilledFromUrl = false
	let isLoading = false
	let error = ''
	let approvalJustCompleted = false

	// Contract data
	let tokenBalance: bigint = 0n
	let tokenAllowance: bigint = 0n
	let minLockAmount: bigint = 0n
	let vaultBalance: bigint = 0n
	let tokenSymbol = 'HOT'
	let tokenDecimals = 18

	// Get addresses from environment
	const lockVaultAddress = PUBLIC_LOCK_VAULT_ADDRESS
	const tokenAddress = PUBLIC_TOKEN_ADDRESS

	// Check if contracts are configured
	const isZeroAddress = (addr: string) => !addr || addr === '0x0000000000000000000000000000000000000000'
	const isConfigured = !isZeroAddress(lockVaultAddress) && !isZeroAddress(tokenAddress)

	// Reactive account
	$: isConnected = $ethereumStore.isConnected
	$: account = $ethereumStore.account

	// Fetch contract data when connected
	async function fetchContractData() {
		if (!isConnected || !account || !isConfigured) return

		try {
			// Fetch token balance
			tokenBalance = (await readContract({
				address: tokenAddress,
				abi: erc20Abi,
				functionName: 'balanceOf',
				args: [account]
			})) as bigint

			// Fetch token allowance
			tokenAllowance = (await readContract({
				address: tokenAddress,
				abi: erc20Abi,
				functionName: 'allowance',
				args: [account, lockVaultAddress]
			})) as bigint

			// Fetch token symbol
			tokenSymbol = (await readContract({
				address: tokenAddress,
				abi: erc20Abi,
				functionName: 'symbol'
			})) as string

			// Fetch token decimals
			tokenDecimals = (await readContract({
				address: tokenAddress,
				abi: erc20Abi,
				functionName: 'decimals'
			})) as number

			// Fetch min lock amount
			minLockAmount = (await readContract({
				address: lockVaultAddress,
				abi: holoLockVaultAbi,
				functionName: 'minLockAmount'
			})) as bigint

			// Fetch vault balance
			vaultBalance = (await readContract({
				address: lockVaultAddress,
				abi: holoLockVaultAbi,
				functionName: 'vaultBalance'
			})) as bigint
		} catch (e) {
			console.error('Error fetching contract data:', e)
		}
	}

	// Reactively fetch data when account changes
	$: if (isConnected && account) {
		fetchContractData()
	}

	// Derive hex for contract from Holochain key (always convert in UI)
	function getAgentHex(input: string): string {
		if (!input || !isHolochainKey(input)) return ''
		try {
			return holochainKeyTo32ByteHex(input)
		} catch {
			return ''
		}
	}
	$: agentHex = getAgentHex(agentInput)

	// Get parameters from URL on mount
	onMount(() => {
		if (browser) {
			try {
				const urlParams = new URL(window.location.href).searchParams
				const urlAmount = urlParams.get('amount')
				const urlAgent = urlParams.get('agent')

				// Validate and set amount if provided
				if (urlAmount) {
					// Validate that amount is a valid number
					const parsedAmount = parseFloat(urlAmount)
					if (!isNaN(parsedAmount) && parsedAmount > 0) {
						amount = urlAmount
						amountPrefilledFromUrl = true
					} else {
						console.warn('Invalid amount parameter in URL:', urlAmount)
					}
				}

				// Validate and set agent if provided (Holochain key only)
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

	// Handle approval
	async function handleApprove() {
		if (!amount) return

		error = ''
		isLoading = true
		transactionStore.awaitWalletConfirmation()

		try {
			const amountWei = parseUnits(amount, tokenDecimals)

			const hash = await writeContract({
				address: tokenAddress,
				abi: erc20Abi,
				functionName: 'approve',
				args: [lockVaultAddress, amountWei]
			})

			transactionStore.awaitTxReceipt(hash)
			const receipt = await waitForTransaction(hash)

			if (receipt) {
				transactionStore.reset()
				await fetchContractData()
				approvalJustCompleted = true
			}
		} catch (e: any) {
			const message = e?.message || 'Approval failed'
			error = message
			transactionStore.transactionError({ message: error })
			console.error(e)
		} finally {
			isLoading = false
		}
	}

	// Handle lock
	async function handleLock() {
		if (!amount || !agentHex) return

		// Validate we have a converted hex (from Holochain key)
		if (!agentHex) {
			error = 'Invalid Unyt agent public key. Provide a Holochain agent key (uhCA...).'
			return
		}

		error = ''
		approvalJustCompleted = false
		isLoading = true
		transactionStore.awaitWalletConfirmation(true)

		try {
			const amountWei = parseUnits(amount, tokenDecimals)

			// Check minimum amount
			if (amountWei < minLockAmount) {
				error = `Amount must be at least ${formatUnits(minLockAmount, tokenDecimals)} ${tokenSymbol}`
				isLoading = false
				transactionStore.reset()
				return
			}

			// Check allowance
			if (tokenAllowance < amountWei) {
				error = 'Insufficient allowance. Please approve first.'
				isLoading = false
				transactionStore.reset()
				return
			}

			const hash = await writeContract({
				address: lockVaultAddress,
				abi: holoLockVaultAbi,
				functionName: 'lock',
				args: [amountWei, agentHex as Hex]
			})

			transactionStore.awaitTxReceipt(hash)
			const receipt = await waitForTransaction(hash)

			if (receipt) {
				transactionStore.transactionSuccess(hash)
				await fetchContractData()
			}
		} catch (e: any) {
			const message = e?.message || 'Lock transaction failed'
			error = message
			transactionStore.transactionError({ message: error })
			console.error(e)
		} finally {
			isLoading = false
		}
	}

	// Calculate if we need approval
	$: amountWei = amount ? parseUnits(amount, tokenDecimals) : 0n
	$: needsApproval = amountWei > 0n && tokenAllowance < amountWei
	$: hasValidAgent = !!agentHex
	$: if (needsApproval) approvalJustCompleted = false
</script>

<Card size="xl" class="flex flex-col gap-4">

	<h1 class="text-2xl font-bold">Lock HOT for Mirrored-HOT</h1>
	<p class="text-gray-600">
		Lock your HOT tokens to receive Mirrored-HOT on Unyt. Your Mirrored-HOT will be credited to the
		specified agent.
	</p>

	{#if !isConfigured}
		<Alert color="yellow">
			<span class="font-semibold">Contracts not configured.</span> The Lock Vault contract address needs to be set in the environment variables.
			Please deploy the contracts and update <code>PUBLIC_LOCK_VAULT_ADDRESS</code> in your <code>.env</code> file.
		</Alert>
	{:else if !isConnected}
		<Alert color="blue"> Please connect your wallet to continue. </Alert>
		<Button on:click={connectWallet}>Connect Wallet</Button>
	{:else}
		<div class="space-y-4">
			<!-- Balance Info -->
			<div class="bg-gray-50 p-4 rounded-lg">
				<p class="text-sm text-gray-600">Your {tokenSymbol} Balance</p>
				<p class="text-xl font-semibold">
					{formatUnits(tokenBalance, tokenDecimals)}
					{tokenSymbol}
				</p>
			</div>

			<!-- Amount Input -->
			<div>
				<Label for="amount" class="mb-2">Amount to Lock</Label>
				{#if amountPrefilledFromUrl}
					<div
						class="block w-full rounded-lg border border-gray-300 bg-gray-50 p-2.5 text-sm text-gray-900 dark:border-gray-600 dark:bg-gray-700 dark:text-white select-none cursor-default"
						style="user-select: none; -webkit-user-select: none;"
						aria-readonly="true"
					>
						{amount}
					</div>
				{:else}
					<Input
						id="amount"
						type="number"
						placeholder="0.0"
						bind:value={amount}
						disabled={isLoading}
					/>
				{/if}
				<Helper class="mt-1">
					Minimum: {formatUnits(minLockAmount, tokenDecimals)}
					{tokenSymbol}
				</Helper>
			</div>

			<!-- Holochain Agent Input -->
			<div>
				<Label for="agent" class="mb-2">Unyt Agent Public Key (Holochain key)</Label>
				{#if agentPrefilledFromUrl}
					<div
						class="block w-full rounded-lg border border-gray-300 bg-gray-50 p-2.5 text-sm text-gray-900 dark:border-gray-600 dark:bg-gray-700 dark:text-white select-none cursor-default break-all"
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
						Agent key from URL (read-only). This is where your Mirrored-HOT will be sent.
					{:else}
						Paste your Holochain agent key (e.g. uhCA...). This is where your Mirrored-HOT will be sent. It is converted to hex for the contract below.
					{/if}
				</Helper>
				{#if agentInput && hasValidAgent}
					<div class="mt-2 space-y-2 rounded-lg border border-gray-200 bg-gray-50 p-3 dark:border-gray-600 dark:bg-gray-700">
						<div>
							<p class="text-xs font-medium text-gray-500 dark:text-gray-400">Holochain key</p>
							<p class="text-base font-semibold text-gray-900 dark:text-white break-all">{agentInput}</p>
						</div>
						<div>
							<p class="text-xs font-medium text-gray-500 dark:text-gray-400">Ethereum (hex, used for lock)</p>
							<p class="text-base font-semibold text-gray-900 dark:text-white break-all">{agentHex}</p>
						</div>
					</div>
				{:else if agentInput}
					<Helper color="red" class="mt-1">
						Invalid format. Provide a Holochain agent key (uhCA...).
					</Helper>
				{/if}
			</div>

			<!-- Error Display -->
			{#if error}
				<Alert color="red">{error}</Alert>
			{/if}

			{#if approvalJustCompleted && !needsApproval}
				<Alert color="green">
					Approval confirmed &mdash; now finalize by locking your {tokenSymbol}.
				</Alert>
			{/if}

			<!-- Action Buttons -->
			<div class="flex flex-row gap-2">
				{#if needsApproval}
					<Button
						class="w-fit"
						color="alternative"
						on:click={handleApprove}
						disabled={isLoading || !amount}
					>
						{#if isLoading}
							<Spinner size="4" class="mr-2" />
						{/if}
						Approve {tokenSymbol}
					</Button>
				{:else}
					<Button
						class="w-fit"
						on:click={handleLock}
						disabled={isLoading || !amount || !hasValidAgent}
					>
						{#if isLoading}
							<Spinner size="4" class="mr-2" />
						{/if}
						Lock {tokenSymbol}
					</Button>
				{/if}
			</div>

			<!-- Vault Info -->
			<!-- <div class="mt-4 pt-4 border-t">
				<p class="text-sm text-gray-500">
					Vault Balance: {formatUnits(vaultBalance, tokenDecimals)}
					{tokenSymbol}
				</p>
				<p class="text-xs text-gray-400 mt-1">
					Lock Vault: <a
						href={`https://sepolia.etherscan.io/address/${lockVaultAddress}`}
						target="_blank"
						class="hover:underline">{lockVaultAddress.slice(0, 10)}...{lockVaultAddress.slice(-8)}</a
					>
				</p>
			</div> -->
		</div>
	{/if}
</Card>

<TransactionModal />
