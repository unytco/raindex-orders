#!/bin/bash
# ===========================================
# Holo Bridge - Sepolia Deployment Script
# ===========================================
# This script deploys all contracts needed for the Holo Bridge on Sepolia
#
# Prerequisites:
# 1. Copy .env.example to .env. It holds addresses, never a private key.
# 2. A deployer wallet forge can sign with: a Ledger, or an encrypted keystore
#    made with `cast wallet import deployer --interactive`
# 3. Have Sepolia ETH in that wallet
#
# Usage:
#   ./deploy-sepolia.sh [step] --account deployer
#   ./deploy-sepolia.sh [step] --ledger
#
# Steps:
#   1 or token           Deploy MockHOT token
#   2 or vault           Deploy HoloLockVault
#   3 or mint            Mint test tokens to your wallet (MINT_AMOUNT, in wei)
#   5 or order-via-vault Deploy claim order via vault
#   all                  Run all steps
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

# Load environment
if [ -f .env ]; then
    source .env
else
    echo -e "${RED}Error: .env file not found${NC}"
    echo "Please copy .env.example to .env"
    exit 1
fi

if [ -n "$PRIVATE_KEY" ] && [ "$PRIVATE_KEY" != "0x..." ]; then
    echo -e "${RED}Error: remove PRIVATE_KEY from .env. Pass --account <keystore> or --ledger instead.${NC}"
    exit 1
fi

if [ -z "$SEPOLIA_RPC_URL" ]; then
    SEPOLIA_RPC_URL="https://1rpc.io/sepolia"
fi

STEP="${1:-status}"
shift || true
WALLET=("$@")
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
    if ! OUTPUT=$(forge script "$@" --rpc-url "$SEPOLIA_RPC_URL" "${WALLET[@]}" --broadcast -vvv 2>&1); then
        echo "$OUTPUT"
        die "forge script $1 failed"
    fi
    echo "$OUTPUT"
}

if [ -n "$WALLET_ADDRESS" ]; then
    BALANCE=$(cast balance "$WALLET_ADDRESS" --rpc-url "$SEPOLIA_RPC_URL" 2>/dev/null || echo "0")
    echo -e "${BLUE}Balance: $(cast from-wei "$BALANCE") ETH${NC}"
fi
echo ""

deploy_token() {
    echo -e "${YELLOW}=== Step 1: Deploying MockHOT Token ===${NC}"

    broadcast script/DeployTestHOT.s.sol:DeployTestHOT

    # Extract deployed address
    TOKEN_ADDR=$(echo "$OUTPUT" | grep -oP "MockHOT deployed at: \K0x[a-fA-F0-9]{40}" || true)

    if [ -n "$TOKEN_ADDR" ]; then
        echo -e "${GREEN}MockHOT deployed at: $TOKEN_ADDR${NC}"
        # Update .env file
        sed -i "s|^TOKEN_ADDRESS=.*|TOKEN_ADDRESS=$TOKEN_ADDR|" .env
        echo -e "${GREEN}Updated .env with TOKEN_ADDRESS${NC}"
    else
        die "Could not extract token address from output"
    fi
}

deploy_vault() {
    echo -e "${YELLOW}=== Step 2: Deploying HoloLockVault ===${NC}"

    if [ -z "$TOKEN_ADDRESS" ]; then
        echo -e "${RED}Error: TOKEN_ADDRESS not set in .env${NC}"
        echo "Run step 1 first, or set TOKEN_ADDRESS to existing token"
        exit 1
    fi

    echo "Using token: $TOKEN_ADDRESS"

    broadcast script/DeployHoloLockVault.s.sol:DeploySepoliaHoloLockVault

    # Extract deployed address
    VAULT_ADDR=$(echo "$OUTPUT" | grep -oP "HoloLockVault deployed at: \K0x[a-fA-F0-9]{40}" || true)

    if [ -n "$VAULT_ADDR" ]; then
        echo -e "${GREEN}HoloLockVault deployed at: $VAULT_ADDR${NC}"
        # Update .env file
        sed -i "s|^LOCK_VAULT_ADDRESS=.*|LOCK_VAULT_ADDRESS=$VAULT_ADDR|" .env
        sed -i "s|^ORDER_OWNER=.*|ORDER_OWNER=$WALLET_ADDRESS|" .env
        echo -e "${GREEN}Updated .env with LOCK_VAULT_ADDRESS and ORDER_OWNER${NC}"
    else
        die "Could not extract vault address from output"
    fi
}

mint_tokens() {
    echo -e "${YELLOW}=== Step 3: Minting Test Tokens ===${NC}"

    if [ -z "$TOKEN_ADDRESS" ]; then
        echo -e "${RED}Error: TOKEN_ADDRESS not set in .env${NC}"
        exit 1
    fi

    AMOUNT=${1:-"1000000000000000000000"} # Default 1000 tokens

    echo "Minting $(cast from-wei $AMOUNT) tokens to $WALLET_ADDRESS"

    # Run mint script
    TOKEN_ADDRESS="$TOKEN_ADDRESS" \
    RECIPIENT="$WALLET_ADDRESS" \
    AMOUNT="$AMOUNT" \
    forge script script/DeployTestHOT.s.sol:MintTestHOT \
        --rpc-url "$SEPOLIA_RPC_URL" \
        "${WALLET[@]}" \
        --broadcast \
        -vvv

    echo -e "${GREEN}Tokens minted successfully!${NC}"
}

deploy_order_via_vault() {
    echo -e "${YELLOW}=== Deploying Claim Order via HoloLockVault ===${NC}"
    echo -e "${GREEN}This makes the vault the order owner, so claims use the same vault as locks.${NC}"

    if [ -z "$LOCK_VAULT_ADDRESS" ]; then
        echo -e "${RED}Error: LOCK_VAULT_ADDRESS not set in .env${NC}"
        echo "Deploy the vault first with: ./deploy-sepolia.sh vault"
        exit 1
    fi

    # TestNet coupons are signed by the test signer, whose key is in src/Constants.sol.
    VALID_SIGNER="${VALID_SIGNER:-0x8E72b7568738da52ca3DCd9b24E178127A4E7d37}"
    echo "Using vault: $LOCK_VAULT_ADDRESS"
    echo "Valid signer: $VALID_SIGNER"

    BROADCAST_FILE="${FOUNDRY_BROADCAST:-broadcast}/DeployClaimOrderViaVault.s.sol/11155111/run-latest.json"
    STARTED=$(mktemp)
    NETWORK=sepolia LOCK_VAULT_ADDRESS="$LOCK_VAULT_ADDRESS" VALID_SIGNER="$VALID_SIGNER" \
        broadcast script/DeployClaimOrderViaVault.s.sol:DeployClaimOrderViaVault
    sent_since "$STARTED" "$BROADCAST_FILE" || die "forge sent nothing: is the vault admin a Safe?"
    rm -f "$STARTED"

    # The order hash is the fourth word of the orderbook's AddOrder event data.
    ORDER_HASH_VAL=$(jq -r '.receipts[0].logs[] | select(.topics[0] == "0x6fa57e1a7a1fbbf3623af2b2025fcd9a5e7e4e31a2a6ec7523445f18e9c50ebf") | .data' "$BROADCAST_FILE" | cut -c195-258 | sed 's/^/0x/')

    if [ -n "$ORDER_HASH_VAL" ] && [ "$ORDER_HASH_VAL" != "0x" ]; then
        echo -e "${GREEN}Order deployed via vault! Hash: $ORDER_HASH_VAL${NC}"
        sed -i "s|^ORDER_HASH=.*|ORDER_HASH=$ORDER_HASH_VAL|" .env
        # Set ORDER_OWNER to the vault address
        sed -i "s|^ORDER_OWNER=.*|ORDER_OWNER=$LOCK_VAULT_ADDRESS|" .env
        echo -e "${GREEN}Updated .env with ORDER_HASH and ORDER_OWNER (vault address)${NC}"
    else
        echo -e "${YELLOW}Order deployed but could not extract hash from broadcast file${NC}"
        echo "Check the transaction on Etherscan to get the order hash"
        echo "Remember: ORDER_OWNER should be set to $LOCK_VAULT_ADDRESS"
    fi
}

show_status() {
    echo -e "${BLUE}=== Deployment Status ===${NC}"
    echo ""
    echo "Wallet:          $WALLET_ADDRESS"
    echo "Balance:         $(cast from-wei $BALANCE) ETH"
    echo ""
    echo "Token Address:   ${TOKEN_ADDRESS:-Not deployed}"
    echo "Vault Address:   ${LOCK_VAULT_ADDRESS:-Not deployed}"
    echo "Order Hash:      ${ORDER_HASH:-Not deployed}"
    echo "Order Owner:     ${ORDER_OWNER:-Not set}"
    echo ""
    echo "Orderbook:       $ORDERBOOK_ADDRESS"
    echo "Vault ID:        $VAULT_ID"
    echo ""

    if [ -n "$TOKEN_ADDRESS" ]; then
        TOKEN_BAL=$(cast call "$TOKEN_ADDRESS" "balanceOf(address)(uint256)" "$WALLET_ADDRESS" --rpc-url "$SEPOLIA_RPC_URL" 2>/dev/null || echo "0")
        echo "Your token balance: $(cast from-wei $TOKEN_BAL)"
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
        mint_tokens "${MINT_AMOUNT:-1000000000000000000000}"
        ;;
    5|order-via-vault)
        deploy_order_via_vault
        ;;
    all)
        deploy_token
        echo ""
        source .env
        deploy_vault
        echo ""
        source .env
        mint_tokens
        echo ""
        source .env
        deploy_order_via_vault
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
        echo "  5 or order-via-vault - Deploy claim order via vault (recommended)"
        echo "  all                 - Run all steps (uses order-via-vault)"
        echo "  status              - Show deployment status"
        ;;
esac
