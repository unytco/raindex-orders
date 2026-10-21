#!/bin/bash
# ===========================================
# Holo Bridge - Base Sepolia (TestNet) Deployment Script
# ===========================================
# This script deploys all contracts needed for the Holo Bridge on Base Sepolia (chain 84532)
#
# Prerequisites:
# 1. Copy .env.example to .env. It holds addresses, never a private key.
# 2. A deployer wallet forge can sign with: a Ledger, or an encrypted keystore
#    made with `cast wallet import deployer --interactive`
# 3. Have Base Sepolia ETH in that wallet (DEPLOY.md names faucets)
#
# Usage:
#   ./deploy-base-sepolia.sh [step] --account deployer
#   ./deploy-base-sepolia.sh [step] --ledger
#
# Steps:
#   1 or token           Deploy MockHOT token
#   2 or vault           Deploy HoloLockVault
#   3 or mint            Mint test tokens to your wallet (MINT_AMOUNT, in wei)
#   4 or order-via-vault Deploy claim order via vault
#   all                  Run all steps
#   safe                 Create the bridge's Safe (SAFE_OWNERS, SAFE_THRESHOLD), or find it
#   bridge               Deploy the vault and claim order in one transaction, with
#                        the Safe ADMIN_ADDRESS as the vault admin and VALID_SIGNER
#   all-safe             token, mint, safe, bridge: a fresh TestNet whose one Safe
#                        is the vault admin and the coupon signer
#   status               Show current deployment status

set -e
cd "$(dirname "$0")"
source script/forge-broadcast.sh

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Load environment: ENV_FILE, .env by default, holds the addresses and gets
# each one this script deploys. They are exported, as forge reads only .env itself.
ENV_FILE="${ENV_FILE:-.env}"
load_env() {
    set -a
    # shellcheck source=/dev/null
    source "$ENV_FILE"
    set +a
}
if [ -f "$ENV_FILE" ]; then
    load_env
else
    echo -e "${RED}Error: $ENV_FILE file not found${NC}"
    echo "Please copy .env.example to $ENV_FILE"
    exit 1
fi

if [ -n "$PRIVATE_KEY" ]; then
    echo -e "${RED}Error: remove PRIVATE_KEY from $ENV_FILE. Pass --account <keystore> or --ledger instead.${NC}"
    exit 1
fi

if [ -z "$BASE_SEPOLIA_RPC_URL" ]; then
    BASE_SEPOLIA_RPC_URL="https://sepolia.base.org"
fi

STEP="${1:-status}"
shift || true
WALLET=("$@")
if [ "$STEP" != "status" ]; then
    CHAIN_ID=$(cast chain-id --rpc-url "$BASE_SEPOLIA_RPC_URL") || die "BASE_SEPOLIA_RPC_URL did not answer"
    [ "$CHAIN_ID" = 84532 ] || die "BASE_SEPOLIA_RPC_URL answers for chain $CHAIN_ID, not Base Sepolia"
fi

if [ ${#WALLET[@]} -gt 0 ] || [ "$STEP" != "status" ]; then
    require_wallet "${WALLET[@]}"
    WALLET_ADDRESS=$(cast wallet address "${WALLET[@]}")
    # Scripts that default to msg.sender see the wallet only through --sender.
    WALLET+=(--sender "$WALLET_ADDRESS")
    echo -e "${BLUE}Wallet: $WALLET_ADDRESS${NC}"
fi

# Runs a forge script that broadcasts, leaving its output in OUTPUT. A failed
# run stops the deploy, so no later step reads an earlier run's results.
broadcast() {
    if ! OUTPUT=$(forge script "$@" --rpc-url "$BASE_SEPOLIA_RPC_URL" "${WALLET[@]}" --broadcast -vvv 2>&1); then
        echo "$OUTPUT"
        die "forge script $1 failed"
    fi
    echo "$OUTPUT"
}

BALANCE=0
if [ -n "$WALLET_ADDRESS" ]; then
    BALANCE=$(cast balance "$WALLET_ADDRESS" --rpc-url "$BASE_SEPOLIA_RPC_URL" 2>/dev/null || echo "0")
    echo -e "${BLUE}Balance: $(cast from-wei "$BALANCE") ETH${NC}"
fi
echo ""

set_env() {
    if grep -q "^$1=" "$ENV_FILE"; then
        sed -i "s|^$1=.*|$1=$2|" "$ENV_FILE"
    else
        [ -z "$(tail -c1 "$ENV_FILE")" ] || echo >> "$ENV_FILE"
        echo "$1=$2" >> "$ENV_FILE"
    fi
}

deploy_token() {
    echo -e "${YELLOW}=== Step 1: Deploying MockHOT Token ===${NC}"

    broadcast script/DeployTestHOT.s.sol:DeployTestHOT

    # Extract deployed address
    TOKEN_ADDR=$(echo "$OUTPUT" | grep -oP "MockHOT deployed at: \K0x[a-fA-F0-9]{40}" || true)

    if [ -n "$TOKEN_ADDR" ]; then
        echo -e "${GREEN}MockHOT deployed at: $TOKEN_ADDR${NC}"
        set_env TOKEN_ADDRESS "$TOKEN_ADDR"
        echo -e "${GREEN}Updated $ENV_FILE with TOKEN_ADDRESS${NC}"
        wait_for_code "$TOKEN_ADDR" "$BASE_SEPOLIA_RPC_URL" \
            "The token is in $ENV_FILE: once the RPC serves it, run the next step, not this one again"
    else
        die "Could not extract token address from output"
    fi
}

deploy_vault() {
    echo -e "${YELLOW}=== Step 2: Deploying HoloLockVault ===${NC}"

    if [ -z "$TOKEN_ADDRESS" ]; then
        echo -e "${RED}Error: TOKEN_ADDRESS not set in $ENV_FILE${NC}"
        echo "Run step 1 first, or set TOKEN_ADDRESS to an existing token on Base Sepolia"
        exit 1
    fi

    echo "Using token: $TOKEN_ADDRESS"

    broadcast script/DeployHoloLockVault.s.sol:DeployBaseSepoliaHoloLockVault

    # Extract deployed addresses
    VAULT_ADDR=$(echo "$OUTPUT" | grep -oP "HoloLockVault deployed at: \K0x[a-fA-F0-9]{40}" || true)
    INPUT_TOKEN_ADDR=$(echo "$OUTPUT" | grep -oP "ClaimInputToken deployed at: \K0x[a-fA-F0-9]{40}" || true)

    if [ -n "$VAULT_ADDR" ] && [ -n "$INPUT_TOKEN_ADDR" ]; then
        echo -e "${GREEN}HoloLockVault deployed at: $VAULT_ADDR${NC}"
        echo -e "${GREEN}ClaimInputToken deployed at: $INPUT_TOKEN_ADDR${NC}"
        set_env BASE_SEPOLIA_LOCK_VAULT_ADDRESS "$VAULT_ADDR"
        set_env CLAIM_INPUT_TOKEN "$INPUT_TOKEN_ADDR"
        echo -e "${GREEN}Updated $ENV_FILE with BASE_SEPOLIA_LOCK_VAULT_ADDRESS and CLAIM_INPUT_TOKEN${NC}"
    else
        die "Could not extract the vault and claim input token addresses from output"
    fi
}

mint_tokens() {
    echo -e "${YELLOW}=== Step 3: Minting Test Tokens ===${NC}"

    if [ -z "$TOKEN_ADDRESS" ]; then
        echo -e "${RED}Error: TOKEN_ADDRESS not set in $ENV_FILE${NC}"
        exit 1
    fi

    AMOUNT=${MINT_AMOUNT:-1000000000000000000000}

    echo "Minting $(cast from-wei "$AMOUNT") tokens to $WALLET_ADDRESS"

    # Run mint script
    TOKEN_ADDRESS="$TOKEN_ADDRESS" \
    RECIPIENT="$WALLET_ADDRESS" \
    AMOUNT="$AMOUNT" \
    forge script script/DeployTestHOT.s.sol:MintTestHOT \
        --rpc-url "$BASE_SEPOLIA_RPC_URL" \
        "${WALLET[@]}" \
        --broadcast \
        -vvv

    echo -e "${GREEN}Tokens minted successfully!${NC}"
}

deploy_order_via_vault() {
    echo -e "${YELLOW}=== Deploying Claim Order via HoloLockVault ===${NC}"
    echo -e "${GREEN}This makes the vault the order owner, so claims use the same vault as locks.${NC}"

    if [ -z "$BASE_SEPOLIA_LOCK_VAULT_ADDRESS" ] || [ -z "$CLAIM_INPUT_TOKEN" ]; then
        echo -e "${RED}Error: BASE_SEPOLIA_LOCK_VAULT_ADDRESS or CLAIM_INPUT_TOKEN not set in $ENV_FILE${NC}"
        echo "Deploy the vault first with: ./deploy-base-sepolia.sh vault"
        exit 1
    fi

    # TestNet coupons are signed by the test signer, whose key is in src/Constants.sol.
    VALID_SIGNER="${VALID_SIGNER:-0x8E72b7568738da52ca3DCd9b24E178127A4E7d37}"
    echo "Using vault: $BASE_SEPOLIA_LOCK_VAULT_ADDRESS"
    echo "Claim input token: $CLAIM_INPUT_TOKEN"
    echo "Valid signer: $VALID_SIGNER"

    BROADCAST_FILE="${FOUNDRY_BROADCAST:-broadcast}/DeployClaimOrderViaVault.s.sol/84532/run-latest.json"
    STARTED=$(mktemp)
    trap 'rm -f "$STARTED"' EXIT
    NETWORK=base-sepolia BASE_SEPOLIA_LOCK_VAULT_ADDRESS="$BASE_SEPOLIA_LOCK_VAULT_ADDRESS" CLAIM_INPUT_TOKEN="$CLAIM_INPUT_TOKEN" \
        VALID_SIGNER="$VALID_SIGNER" broadcast script/DeployClaimOrderViaVault.s.sol:DeployClaimOrderViaVault
    sent_since "$STARTED" "$BROADCAST_FILE" || die "forge sent nothing: is the vault admin a Safe?"

    # The order hash is the second word of Raindex's AddOrderV3 event data.
    ORDER_HASH_VAL=$(jq -r '.receipts[0].logs[] | select(.topics[0] == "0x87491344dfbcf91f6cbbc610cbbeedc85313d37a02df0c93527f7ea5f8db717f") | .data' "$BROADCAST_FILE" | cut -c67-130 | sed 's/^/0x/')

    if [ -n "$ORDER_HASH_VAL" ] && [ "$ORDER_HASH_VAL" != "0x" ]; then
        echo -e "${GREEN}Order deployed via vault! Hash: $ORDER_HASH_VAL${NC}"
        set_env ORDER_HASH "$ORDER_HASH_VAL"
        set_env ORDER_OWNER "$BASE_SEPOLIA_LOCK_VAULT_ADDRESS"
        echo -e "${GREEN}Updated $ENV_FILE with ORDER_HASH and ORDER_OWNER (vault address)${NC}"
    else
        die "The order was added, but its hash is not in $BROADCAST_FILE. Take ORDER_HASH from the transaction on Basescan (https://sepolia.basescan.org), and set ORDER_OWNER to $BASE_SEPOLIA_LOCK_VAULT_ADDRESS."
    fi
}

create_safe() {
    echo -e "${YELLOW}=== Creating the bridge's Safe, or finding it ===${NC}"
    [ -n "$SAFE_OWNERS" ] && [ -n "$SAFE_THRESHOLD" ] ||
        die "SAFE_OWNERS (the owners, separated by commas) and SAFE_THRESHOLD are required"
    NETWORK=base-sepolia broadcast script/CreateSafe.s.sol:CreateSafe
    SAFE_ADDR=$(echo "$OUTPUT" | grep -oP "^\s*SAFE=\K0x[a-fA-F0-9]{40}" | tail -1 || true)
    [ -n "$SAFE_ADDR" ] || die "Could not extract the Safe's address from output"
    echo -e "${GREEN}The bridge's Safe: $SAFE_ADDR${NC}"
    set_env ADMIN_ADDRESS "$SAFE_ADDR"
    set_env ADMIN_SAFE_THRESHOLD "$SAFE_THRESHOLD"
    set_env ADMIN_SAFE_OWNERS "$SAFE_OWNERS"
    set_env VALID_SIGNER "$SAFE_ADDR"
    echo -e "${GREEN}Updated $ENV_FILE: the Safe is ADMIN_ADDRESS and VALID_SIGNER${NC}"
    wait_for_code "$SAFE_ADDR" "$BASE_SEPOLIA_RPC_URL" \
        "The Safe is in $ENV_FILE: once the RPC serves it, run the next step"
}

# Writes the deploy record's `name` into ENV_FILE.
record_to_env() {
    local name value
    for name in "$@"; do
        value=$(echo "$RECORD" | grep -oP "^\s*$name=\K0x[a-fA-F0-9]+" | head -1 || true)
        [ -n "$value" ] || die "The deploy record has no $name"
        set_env "$name" "$value"
    done
}

deploy_bridge() {
    echo -e "${YELLOW}=== Deploying the vault and claim order in one transaction ===${NC}"
    [ -n "$TOKEN_ADDRESS" ] || die "TOKEN_ADDRESS is not set in $ENV_FILE: run step 1 first"
    [ -n "$ADMIN_ADDRESS" ] && [ -n "$VALID_SIGNER" ] ||
        die "ADMIN_ADDRESS and VALID_SIGNER are not set in $ENV_FILE: run the safe step first"
    echo "Token: $TOKEN_ADDRESS"
    echo "Vault admin: $ADMIN_ADDRESS"
    echo "Coupon signer: $VALID_SIGNER"
    local script=script/DeployBridge.s.sol:DeployBaseSepoliaBridge
    local run_file="${FOUNDRY_BROADCAST:-broadcast}/DeployBridge.s.sol/84532/run-latest.json"
    local started deployer block forge_failed= sent=
    started=$(mktemp)
    # Not through broadcast: forge can fail after it sent, as when the receipt
    # wait times out on the load-balanced sepolia.base.org. A second run would
    # then deploy a second bridge, so what landed is read before anything stops.
    OUTPUT=$(forge script "$script" --rpc-url "$BASE_SEPOLIA_RPC_URL" "${WALLET[@]}" --broadcast -vvv 2>&1) || forge_failed=1
    echo "$OUTPUT"
    sent_since "$started" "$run_file" && sent=1
    rm -f "$started"
    [ -n "$sent" ] || die "forge sent nothing${forge_failed:+: forge script $script failed}"
    if [ -n "$forge_failed" ]; then
        echo "forge script $script failed after it sent. Do not run this again: a new run deploys a second bridge. It sent:" >&2
        landed "$run_file" >&2
        die "If HoloBridgeDeployer succeeded, read it and its block from $run_file, then run: forge script $script --rpc-url \$BASE_SEPOLIA_RPC_URL --sig 'record(address,uint256)' <HoloBridgeDeployer> <block>"
    fi
    deployer=$(jq -r '[.transactions[] | select(.transactionType == "CREATE" and .contractName == "HoloBridgeDeployer")][0].contractAddress // empty' "$run_file")
    block=$(first_block "$run_file")
    [[ $deployer =~ ^0x[0-9a-fA-F]{40}$ && $block =~ ^[1-9][0-9]*$ ]] ||
        die "The deploy landed. Read the HoloBridgeDeployer and its block from $run_file, then run: forge script $script --rpc-url \$BASE_SEPOLIA_RPC_URL --sig 'record(address,uint256)' <HoloBridgeDeployer> <block>"
    wait_for_code "$deployer" "$BASE_SEPOLIA_RPC_URL" \
        "The deploy landed: HoloBridgeDeployer $deployer in block $block. Once the RPC serves it, run: forge script $script --rpc-url \$BASE_SEPOLIA_RPC_URL --sig 'record(address,uint256)' $deployer $block"
    RECORD=$(forge script "$script" --rpc-url "$BASE_SEPOLIA_RPC_URL" --sig 'record(address,uint256)' "$deployer" "$block" 2>&1) || {
        echo "$RECORD"
        die "The deploy landed: HoloBridgeDeployer $deployer in block $block. Its record failed; fix the above and run the record again"
    }
    echo "$RECORD" | sed -n '/Deploy record/,$p'
    record_to_env BASE_SEPOLIA_LOCK_VAULT_ADDRESS CLAIM_INPUT_TOKEN ORDER_HASH ORDER_OWNER CLAIM_SIGNER CLAIM_BYTECODE
    echo -e "${GREEN}Updated $ENV_FILE with the deploy record${NC}"
}

show_status() {
    echo -e "${BLUE}=== Deployment Status ===${NC}"
    echo ""
    if [ -n "$WALLET_ADDRESS" ]; then
        echo "Wallet:          $WALLET_ADDRESS"
        echo "Balance:         $(cast from-wei "$BALANCE") ETH"
        echo ""
    fi
    echo "Token Address:   ${TOKEN_ADDRESS:-Not deployed}"
    echo "Vault Address:   ${BASE_SEPOLIA_LOCK_VAULT_ADDRESS:-Not deployed}"
    echo "Order Hash:      ${ORDER_HASH:-Not deployed}"
    echo "Order Owner:     ${ORDER_OWNER:-Not set}"
    echo "Vault Admin:     ${ADMIN_ADDRESS:-the deployer}"
    echo "Coupon Signer:   ${VALID_SIGNER:-the test signer}"
    echo ""
    echo "Orderbook:       $ORDERBOOK_ADDRESS"
    echo "Vault ID:        $VAULT_ID"
    echo ""

    if [ -n "$TOKEN_ADDRESS" ] && [ -n "$WALLET_ADDRESS" ]; then
        TOKEN_BAL=$(cast call "$TOKEN_ADDRESS" "balanceOf(address)(uint256)" "$WALLET_ADDRESS" --rpc-url "$BASE_SEPOLIA_RPC_URL" 2>/dev/null || echo "0")
        echo "Your token balance: $(cast from-wei "$TOKEN_BAL")"
    fi
}

# Main
case "$STEP" in
    1|token)
        deploy_token
        ;;
    2|vault)
        deploy_vault
        ;;
    3|mint)
        mint_tokens
        ;;
    4|order-via-vault)
        deploy_order_via_vault
        ;;
    all)
        deploy_token
        echo ""
        load_env
        deploy_vault
        echo ""
        load_env
        mint_tokens
        echo ""
        load_env
        deploy_order_via_vault
        ;;
    safe)
        create_safe
        ;;
    bridge)
        deploy_bridge
        ;;
    all-safe)
        deploy_token
        echo ""
        load_env
        mint_tokens
        echo ""
        load_env
        create_safe
        echo ""
        load_env
        deploy_bridge
        ;;
    status)
        show_status
        ;;
    *)
        echo "Usage: $0 [step] --account <keystore> | --ledger"
        echo ""
        echo "Steps:"
        echo "  1 or token          - Deploy MockHOT token"
        echo "  2 or vault          - Deploy HoloLockVault"
        echo "  3 or mint           - Mint test tokens"
        echo "  4 or order-via-vault - Deploy claim order via vault"
        echo "  all                 - Run all steps"
        echo "  safe                - Create the bridge's Safe, or find it"
        echo "  bridge              - Deploy vault and claim order, the Safe as admin and signer"
        echo "  all-safe            - token, mint, safe, bridge"
        echo "  status              - Show deployment status"
        exit 1
        ;;
esac
