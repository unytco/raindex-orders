# Holo Bridge - Sepolia Deployment Guide

This guide covers deploying and testing the complete HOT <> bridged HOT bridge infrastructure on Sepolia testnet.

## Prerequisites

1. MetaMask wallet with Sepolia ETH (get from [Sepolia Faucet](https://sepoliafaucet.com/))
2. Foundry installed (`forge`, `cast` commands available)
3. Rust toolchain (for lock-watcher and coupon-signer)
4. Node.js 20+ (for UI)

## Quick Start

### 1. Set Up Environment

```bash
# Copy example env file
cp .env.example .env

# Edit .env and add your private key
nano .env
```

Your `.env` should have:
```bash
PRIVATE_KEY=0x<your-private-key-here>
SEPOLIA_RPC_URL=https://1rpc.io/sepolia
```

### 2. Deploy All Contracts

The `deploy-sepolia.sh` script handles all deployment steps:

```bash
# Check your wallet and balance
./deploy-sepolia.sh status

# Step 1: Deploy MockHOT token
./deploy-sepolia.sh token
# Note: Updates .env with TOKEN_ADDRESS automatically

# Step 2: Deploy HoloLockVault
./deploy-sepolia.sh vault
# Note: Updates .env with LOCK_VAULT_ADDRESS automatically

# Step 3: Mint test tokens to your wallet
./deploy-sepolia.sh mint

# Step 4: Fund the vault (deposit tokens for claims)
./deploy-sepolia.sh fund

# Step 5: Deploy claim order via HoloLockVault
./deploy-sepolia.sh order-via-vault
# Note: Updates .env with ORDER_HASH and ORDER_OWNER automatically
```

### 3. Verify Deployment

```bash
# Show all deployed addresses and configuration
./deploy-sepolia.sh status
```

## Deployed Contract Addresses (Current Sepolia)

| Contract | Address |
|----------|---------|
| MockHOT Token | `0xeaC8eEEE9f84F3E3F592e9D8604100eA1b788749` |
| HoloLockVault | `0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6` |
| Orderbook | `0xfca89cD12Ba1346b1ac570ed988AB43b812733fe` |
| Claim Order Hash | `0x5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9` |
| NOOP Token (placeholder) | `0x555FA2F68dD9B7dB6c8cA1F03bFc317ce61e9028` |
| Test Signer | `0x8E72b7568738da52ca3DCd9b24E178127A4E7d37` |

## Testing the Complete Flow

### Lock Flow (HOT -> Bridged HOT)

1. **Start the lock watcher:**
```bash
cd lock-watcher-rs
cp .env.example .env
# Edit .env: set SEPOLIA_LOCK_VAULT_ADDRESS=0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6
cargo run
```

2. **Start the UI:**
```bash
cd ui
npm install
npm run dev
```

3. **Lock tokens:**
   - Open http://localhost:5173
   - Connect MetaMask to Sepolia
   - Select "Lock HOT -> bridged HOT" tab
   - Enter amount and Holochain agent public key
   - Approve and lock tokens
   - Watch the lock-watcher detect the event

### Claim Flow (Bridged HOT -> HOT)

1. **Generate a claim coupon:**
```bash
cd coupon-signer
cp .env.example .env
# .env already configured with test values

# Generate coupon for 10 tokens to a recipient
cargo run -- \
  --amount "10" \
  --recipient "0xYourRecipientAddress" \
  --format ui
```

2. **Claim via UI:**
   - Open http://localhost:5173/claim
   - Paste the coupon string into the input field
   - Or use URL: `http://localhost:5173/claim?c=<coupon>`
   - Click "Claim HOT"

3. **Claim via URL parameter:**
   - The coupon-signer outputs a URL-safe format
   - Share: `http://localhost:5173/claim?c=<signer>,<signature>,<ctx0>,<ctx1>,...`

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                    SHARED LIQUIDITY POOL                         │
│         (Raindex Orderbook Vault owned by HoloLockVault)        │
│                                                                  │
│   LOCK (HOT→bHOT)            HOT Tokens             CLAIM (bHOT→HOT)
│   ─────────────────►    ┌───────────────┐    ◄─────────────────
│   Deposits INTO         │   Balance: N   │         Withdraws FROM
│                         └───────────────┘                        │
└─────────────────────────────────────────────────────────────────┘
```

**Key Design**: The HoloLockVault contract owns both:
1. The vault where locked HOT is deposited
2. The claim order that allows withdrawals via signed coupons

This ensures LOCK deposits and CLAIM withdrawals operate on the **same pool of tokens**.

## Component Details

### HoloLockVault Contract (`src/HoloLockVault.sol`)

Functions:
- `lock(amount, holochainAgent)` - Lock tokens, emit event for bridged HOT crediting
- `addOrder(config)` - Deploy claim order (admin only)
- `removeOrder(order)` - Remove claim order (admin only)
- `adminWithdraw(amount, to)` - Emergency withdrawal (admin only)
- `vaultBalance()` - Check vault balance

### Coupon Signer (`coupon-signer/`)

Rust CLI that generates signed claim coupons:
```bash
cargo run -- \
  --amount "10" \
  --recipient "0x..." \
  --format ui    # Output format: ui (default), json, or hex
```

Output formats:
- `ui`: `signer,signature,ctx0,ctx1,...,ctx8` (decimal values, URL-safe)
- `json`: Full JSON with all fields
- `hex`: Raw hex-encoded signed context

### Lock Watcher (`lock-watcher-rs/`)

Rust service that monitors Lock events:
```bash
cargo run
```

Features:
- Polls for new Lock events
- SQLite database for tracking processed locks
- Configurable polling interval
- Ready for Holochain integration

### UI (`ui/`)

SvelteKit web interface:
- `/` - Home page with lock/claim selector
- `/lock` - Lock HOT to receive bridged HOT
- `/claim` - Claim HOT with coupon
- `/claim?c=<coupon>` - Direct claim via URL parameter

## Coupon Format

The signed coupon contains 9 context values:
| Index | Field | Description |
|-------|-------|-------------|
| 0 | recipient | Ethereum address to receive tokens |
| 1 | amount | Token amount in wei |
| 2 | expiry | Unix timestamp when coupon expires |
| 3 | orderHash | Hash of the claim order |
| 4 | orderOwner | HoloLockVault address |
| 5 | orderbook | Orderbook contract address |
| 6 | outputToken | Token address (MockHOT) |
| 7 | outputVaultId | Vault ID |
| 8 | nonce | Unique nonce (prevents replay) |

## Troubleshooting

### "Order not found" in UI
The UI reads order status directly from the blockchain via RPC. If you see this error:
1. Verify the order was deployed: `./deploy-sepolia.sh status`
2. Check ORDER_HASH in `.env` matches the deployed order
3. Ensure `ui/src/lib/orderConfig.ts` has the correct order configuration

### Transaction fails with "Wrong signer"
The coupon was signed with a different key than the one configured in the Rainlang order.
- Check `valid-signer` in `src/holo-claim.rain`
- Ensure `SIGNER_PRIVATE_KEY` in coupon-signer matches

### "Nonce already used"
Each coupon can only be used once. Generate a new coupon with a fresh nonce.

## Security Notes

- Never commit `.env` files with private keys
- The test signer key in this repo is for testing only
- In production, use Fireblocks MPC or similar secure key management
- The admin key controls emergency withdrawals - protect it carefully
