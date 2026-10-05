#!/usr/bin/env bash
# Sourced by deploy-mainnet.sh and rotate-claim-signer.sh.

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

# Whether forge wrote run_file after the marker file was created.
sent_since() {
	local marker=$1 run_file=$2
	[[ -f $run_file && $run_file -nt $marker ]]
}

# One line per transaction forge sent: what it called, its hash and its outcome.
landed() {
	jq -r '
		.receipts as $receipts
		| .transactions[]
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
	hex=$(jq -r '.receipts[0].blockNumber // empty' "$1")
	[[ $hex =~ ^0x[0-9a-fA-F]+$ ]] && printf '%d\n' "$hex"
}
