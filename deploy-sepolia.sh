#!/bin/bash
# ===========================================
# Holo Bridge - Sepolia Deployment Script
# ===========================================
# This script deploys all contracts needed for the Holo Bridge on Sepolia
#
# Prerequisites:
# 1. Copy .env.example to .env
# 2. Add your PRIVATE_KEY to .env
# 3. Have Sepolia ETH in your wallet
#
# Usage:
#   ./deploy-sepolia.sh [step]
#
# Steps:
#   1 or token     - Deploy MockHOT token
#   2 or vault     - Deploy HoloLockVault
#   3 or mint      - Mint test tokens to your wallet
#   all            - Run all steps
#   status         - Show current deployment status

set -e

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
    echo "Please copy .env.example to .env and add your PRIVATE_KEY"
    exit 1
fi

# Check required variables
if [ -z "$PRIVATE_KEY" ] || [ "$PRIVATE_KEY" == "0x..." ]; then
    echo -e "${RED}Error: PRIVATE_KEY not set in .env${NC}"
    exit 1
fi

if [ -z "$SEPOLIA_RPC_URL" ]; then
    SEPOLIA_RPC_URL="https://1rpc.io/sepolia"
fi

# Get wallet address from private key
WALLET_ADDRESS=$(cast wallet address "$PRIVATE_KEY" 2>/dev/null)
echo -e "${BLUE}Wallet: $WALLET_ADDRESS${NC}"

# Check balance
BALANCE=$(cast balance "$WALLET_ADDRESS" --rpc-url "$SEPOLIA_RPC_URL" 2>/dev/null || echo "0")
echo -e "${BLUE}Balance: $(cast from-wei $BALANCE) ETH${NC}"
echo ""

deploy_token() {
    echo -e "${YELLOW}=== Step 1: Deploying MockHOT Token ===${NC}"

    # Run forge script
    OUTPUT=$(forge script script/DeployTestHOT.s.sol:DeployTestHOT \
        --rpc-url "$SEPOLIA_RPC_URL" \
        --private-key "$PRIVATE_KEY" \
        --broadcast \
        -vvv 2>&1)

    echo "$OUTPUT"

    # Extract deployed address
    TOKEN_ADDR=$(echo "$OUTPUT" | grep -oP "MockHOT deployed at: \K0x[a-fA-F0-9]{40}" || true)

    if [ -n "$TOKEN_ADDR" ]; then
        echo -e "${GREEN}MockHOT deployed at: $TOKEN_ADDR${NC}"
        # Update .env file
        sed -i "s|^TOKEN_ADDRESS=.*|TOKEN_ADDRESS=$TOKEN_ADDR|" .env
        echo -e "${GREEN}Updated .env with TOKEN_ADDRESS${NC}"
    else
        echo -e "${RED}Could not extract token address from output${NC}"
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

    # Run forge script
    OUTPUT=$(forge script script/DeployHoloLockVault.s.sol:DeploySepoliaHoloLockVault \
        --rpc-url "$SEPOLIA_RPC_URL" \
        --private-key "$PRIVATE_KEY" \
        --broadcast \
        -vvv 2>&1)

    echo "$OUTPUT"

    # Extract deployed address
    VAULT_ADDR=$(echo "$OUTPUT" | grep -oP "HoloLockVault deployed at: \K0x[a-fA-F0-9]{40}" || true)

    if [ -n "$VAULT_ADDR" ]; then
        echo -e "${GREEN}HoloLockVault deployed at: $VAULT_ADDR${NC}"
        # Update .env file
        sed -i "s|^LOCK_VAULT_ADDRESS=.*|LOCK_VAULT_ADDRESS=$VAULT_ADDR|" .env
        sed -i "s|^ORDER_OWNER=.*|ORDER_OWNER=$WALLET_ADDRESS|" .env
        echo -e "${GREEN}Updated .env with LOCK_VAULT_ADDRESS and ORDER_OWNER${NC}"
    else
        echo -e "${RED}Could not extract vault address from output${NC}"
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
        --private-key "$PRIVATE_KEY" \
        --broadcast \
        -vvv

    echo -e "${GREEN}Tokens minted successfully!${NC}"
}

deploy_order() {
    echo -e "${YELLOW}=== Step 4: Deploying Claim Order ===${NC}"

    if [ -z "$TOKEN_ADDRESS" ]; then
        echo -e "${RED}Error: TOKEN_ADDRESS not set in .env${NC}"
        exit 1
    fi

    # Sepolia addresses
    ORDERBOOK_SUBPARSER="0xe6A589716d5a72276C08E0e08bc941a28005e55A"
    VALID_SIGNER="0x8E72b7568738da52ca3DCd9b24E178127A4E7d37"

    echo "Using token: $TOKEN_ADDRESS"
    echo "Valid signer: $VALID_SIGNER"

    # Use dotrain via nix to compose the rainlang properly
    echo "Composing rainlang with dotrain (via nix)..."
    RAINLANG=$(nix run github:rainlanguage/dotrain -- compose --input src/holo-claim.rain --entrypoints calculate-io --entrypoints handle-io 2>&1)

    if [ $? -ne 0 ]; then
        echo -e "${RED}dotrain compose failed:${NC}"
        echo "$RAINLANG"
        echo ""
        echo "If dotrain is not available, you can deploy the order manually via:"
        echo "  https://app.rainlang.xyz"
        exit 1
    fi

    echo "Composed Rainlang:"
    echo "$RAINLANG"
    echo ""

    # Write rainlang to temp file (more reliable than env var for multi-line)
    RAINLANG_FILE=$(mktemp)
    echo "$RAINLANG" > "$RAINLANG_FILE"
    echo "Written rainlang to: $RAINLANG_FILE"

    # Run forge script with RAINLANG_FILE env var
    echo "Running forge script..."
    set +e  # Don't exit on error so we can capture output
    OUTPUT=$(RAINLANG_FILE="$RAINLANG_FILE" forge script script/DeployClaimOrder.s.sol:DeployClaimOrder \
        --rpc-url "$SEPOLIA_RPC_URL" \
        --private-key "$PRIVATE_KEY" \
        --broadcast \
        -vvv 2>&1)
    FORGE_EXIT_CODE=$?
    set -e

    # Clean up temp file
    rm -f "$RAINLANG_FILE"

    echo "$OUTPUT"

    if [ $FORGE_EXIT_CODE -ne 0 ]; then
        echo -e "${RED}Forge script failed with exit code $FORGE_EXIT_CODE${NC}"
    fi

    # Try to extract order hash from logs (look for AddOrder event)
    # The order hash appears in the transaction logs
    ORDER_HASH_VAL=$(echo "$OUTPUT" | grep -oP "orderHash: \K0x[a-fA-F0-9]{64}" || true)

    if [ -n "$ORDER_HASH_VAL" ]; then
        echo -e "${GREEN}Order deployed! Hash: $ORDER_HASH_VAL${NC}"
        sed -i "s|^ORDER_HASH=.*|ORDER_HASH=$ORDER_HASH_VAL|" .env
        echo -e "${GREEN}Updated .env with ORDER_HASH${NC}"
    else
        echo -e "${YELLOW}Order deployed but could not extract hash from output${NC}"
        echo "Check the transaction on Etherscan to get the order hash"
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
case "${1:-status}" in
    1|token)
        deploy_token
        ;;
    2|vault)
        deploy_vault
        ;;
    3|mint)
        mint_tokens "${2:-1000000000000000000000}"
        ;;
    4|order)
        deploy_order
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
        deploy_order
        ;;
    status)
        show_status
        ;;
    *)
        echo "Usage: $0 [step]"
        echo ""
        echo "Steps:"
        echo "  1 or token   - Deploy MockHOT token"
        echo "  2 or vault   - Deploy HoloLockVault"
        echo "  3 or mint    - Mint test tokens"
        echo "  4 or order   - Deploy claim order"
        echo "  all          - Run all steps"
        echo "  status       - Show deployment status"
        ;;
esac
