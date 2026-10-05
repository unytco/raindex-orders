#!/usr/bin/env bash
# Deploys the HOT bridge on Ethereum mainnet. DEPLOY.md, "Mainnet deploy":
#   ETH_RPC_URL=... ADMIN_ADDRESS=0x... VALID_SIGNER=0x... ./deploy-mainnet.sh --ledger
set -euo pipefail
cd "$(dirname "$0")"
source script/forge-broadcast.sh

: "${ETH_RPC_URL:?ETH_RPC_URL is required}"
: "${ADMIN_ADDRESS:?ADMIN_ADDRESS is required}"
: "${VALID_SIGNER:?VALID_SIGNER is required}"
require_wallet "$@"

script=script/DeployMainnet.s.sol:DeployMainnet
run_file="${FOUNDRY_BROADCAST:-broadcast}/DeployMainnet.s.sol/1/run-latest.json"
started=$(mktemp)
trap 'rm -f "$started"' EXIT

if ! forge script "$script" --rpc-url "$ETH_RPC_URL" --broadcast --slow "$@"; then
	if sent_since "$started" "$run_file"; then
		echo "The deploy stopped part way. It cannot be resumed: a new run deploys a new vault. It sent:" >&2
		landed "$run_file" >&2
	else
		echo "Nothing was sent." >&2
	fi
	exit 1
fi
sent_since "$started" "$run_file" || die "forge sent nothing."

echo "Sent:"
landed "$run_file"
vault_tx=$(jq -r '.transactions[] | select(.transactionType == "CREATE" and .contractName == "HoloLockVault")' "$run_file")
vault=$(jq -r '.contractAddress' <<<"$vault_tx")
forge script "$script" --rpc-url "$ETH_RPC_URL" --sig 'record(address,uint256)' "$vault" "$(first_block "$run_file")"
