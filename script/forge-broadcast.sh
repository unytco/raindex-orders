#!/usr/bin/env bash
# Sourced by deploy-mainnet.sh, deploy-base-sepolia.sh and rotate-claim-signer.sh.

die() {
	echo "$*" >&2
	exit 1
}

# Refuses a wallet option that takes a raw private key or mnemonic, and wants
# one that signs with a hardware wallet or an encrypted keystore.
require_wallet() {
	local arg signer=
	for arg in "$@"; do
		case "$arg" in
		--private-key | --private-key=* | --private-keys | --private-keys=* | \
			--mnemonic | --mnemonic=* | --mnemonics | --mnemonics=* | \
			-i | --interactive | --interactives | --interactives=*)
			die "$arg takes a raw key: pass --ledger, --trezor or --account <keystore> instead"
			;;
		--ledger | --trezor | --account | --account=* | --keystore | --keystore=*)
			signer=1
			;;
		esac
	done
	[[ -n $signer ]] || die "pass the signer as a forge wallet option: --ledger, --trezor or --account <keystore>"
}

# Waits until rpc_url serves code at address. A load-balanced RPC such as
# sepolia.base.org can answer from a node that has not yet seen the block with
# the deploy, and the next forge script then reverts with "call to non-contract address".
# Gives up after 30 tries 2 s apart, each of which can itself wait out the RPC
# timeout, and dies with `recovery`, if given, after its own message.
wait_for_code() {
	local address=$1 rpc_url=$2 recovery=${3:-} try
	for try in $(seq 30); do
		[[ $(cast code "$address" --rpc-url "$rpc_url" 2>/dev/null) =~ ^0x[0-9a-fA-F]{2,}$ ]] && return 0
		sleep 2
	done
	die "$rpc_url serves no code at $address after 30 tries 2 s apart${recovery:+. $recovery}"
}

# Whether forge wrote run_file after the marker file was created.
sent_since() {
	local marker=$1 run_file=$2
	[[ -f $run_file && $run_file -nt $marker ]]
}

# One line per transaction forge sent: what it called, its hash and its outcome.
landed() {
	jq -r '
		(.receipts // []) as $receipts
		| (.transactions // [])[]
		| . as $tx
		| ([$receipts[] | select(.transactionHash == $tx.hash)][0]) as $receipt
		| [
			(.contractName // .contractAddress // "?"),
			(.function // "constructor"),
			(.hash // "unsent"),
			(if $receipt == null then "no receipt"
			 elif $receipt.status == "0x1" then "succeeded in block \($receipt.blockNumber)"
			 else "reverted" end),
			(if .transactionType == "CREATE" then "deployed \(.contractAddress)" else empty end)
		] | join("  ")' "$1"
}

# The block, in decimal, of the first transaction forge sent, or nothing.
first_block() {
	local hex
	hex=$(jq -r '.receipts[0].blockNumber // empty' "$1" 2>/dev/null) || true
	if [[ $hex =~ ^0x[0-9a-fA-F]+$ ]]; then printf '%d\n' "$hex"; fi
}
