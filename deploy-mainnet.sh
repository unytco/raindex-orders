#!/usr/bin/env bash
# Deploys the HOT bridge on Ethereum mainnet, in one transaction, with a Safe as
# the vault admin from the start. DEPLOY.md, "Mainnet deploy":
#   ETH_RPC_URL=... SAFE_OWNERS=0x..,0x..,0x.. SAFE_THRESHOLD=2 ./deploy-mainnet.sh safe --ledger
#   ETH_RPC_URL=... ADMIN_ADDRESS=<the Safe> ADMIN_SAFE_THRESHOLD=2 ADMIN_SAFE_OWNERS=0x..,0x..,0x.. \
#   VALID_SIGNER=<the Safe> ./deploy-mainnet.sh --ledger
# The first creates the bridge's Safe through Safe's own factory, or finds it,
# and prints the inputs the second takes. The Safe is the vault admin and the
# coupon signer. One key as the admin needs UNSAFE_ALLOW_SINGLE_KEY_ADMIN=yes.
set -euo pipefail
cd "$(dirname "$0")"
source script/forge-broadcast.sh

: "${ETH_RPC_URL:?ETH_RPC_URL is required}"

# The Safe's address follows from SAFE_OWNERS, SAFE_THRESHOLD and SAFE_SALT_NONCE:
# it is printed before anything is sent, and running this again finds it.
if [[ ${1:-} == safe ]]; then
	shift
	: "${SAFE_OWNERS:?SAFE_OWNERS is required: the owners of the Safe, separated by commas}"
	: "${SAFE_THRESHOLD:?SAFE_THRESHOLD is required: how many owners must sign}"
	require_wallet "$@"
	NETWORK=mainnet forge script script/CreateSafe.s.sol:CreateSafe --rpc-url "$ETH_RPC_URL" --broadcast --slow "$@"
	exit
fi

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
deployer=$(jq -r '[.transactions[] | select(.transactionType == "CREATE" and .contractName == "HoloBridgeDeployer")][0].contractAddress // empty' "$run_file" 2>/dev/null) || true
block=$(first_block "$run_file")
record="forge script $script --rpc-url \$ETH_RPC_URL --sig 'record(address,uint256)'"
[[ $deployer =~ ^0x[0-9a-fA-F]{40}$ && $block =~ ^[1-9][0-9]*$ ]] ||
	die "The deploy landed. Do not run this again. Read the HoloBridgeDeployer and its block from $run_file, then run: $record <HoloBridgeDeployer> <block>"
forge script "$script" --rpc-url "$ETH_RPC_URL" --sig 'record(address,uint256)' "$deployer" "$block" ||
	die "The deploy landed: HoloBridgeDeployer $deployer in block $block. Do not run this again. Fix the above, then run: $record $deployer $block"
