# Holo Bridge - Sepolia Deployment Guide

This guide covers deploying and testing the complete HOT <> bridged HOT bridge infrastructure on Sepolia testnet.

## Prerequisites

1. MetaMask wallet with Sepolia ETH (get from [Sepolia Faucet](https://sepoliafaucet.com/))
2. Foundry installed (`forge`, `cast` commands available)
3. Rust toolchain (for bridge-orchestrator)
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

1. **Start the bridge orchestrator:**
```bash
cd bridge-orchestrator
cp .env.example .env
# Edit .env with your Sepolia + Holochain settings
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
   - Watch the bridge-orchestrator detect the event and drive the Holochain bridge

### Claim Flow (Bridged HOT -> HOT)

1. **Coupon generation:**
   Withdrawal coupons are produced by the bridge orchestrator as part of its bridging cycle whenever a burn/withdraw request is observed on the Holochain side. See `bridge-orchestrator/src/signer.rs` for the signing logic.

2. **Claim via UI:**
   - Open http://localhost:5173/claim
   - Paste the coupon string into the input field
   - Or use URL: `http://localhost:5173/claim?c=<coupon>`
   - Click "Claim HOT"

3. **Claim via URL parameter:**
   - Coupons use a URL-safe format
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

### Bridge Orchestrator (`bridge-orchestrator/`)

Rust service that replaces the legacy `lock-watcher-rs` and `coupon-signer`:
```bash
cd bridge-orchestrator
cargo run
```

Responsibilities:
- Polls the orderbook for new Lock events and drives the Holochain bridge
- Processes withdrawal requests from Holochain and generates signed claim coupons (see `src/signer.rs::generate_coupon`)
- Emits batched bridging RAVE transactions with explicit links and a coupons map
- Produces the same URL-safe coupon format consumed by the UI: `signer,signature,ctx0,ctx1,...,ctx8`

### UI (`ui/`)

SvelteKit web interface:
- `/` - Home page with lock/claim selector
- `/lock` - Lock HOT to receive bridged HOT
- `/claim` - Claim HOT with coupon
- `/claim?c=<coupon>` - Direct claim via URL parameter
- `POST /api/coupon-status` - Status of up to 50 claim coupons, read from Sepolia through `SEPOLIA_RPC_URL`

Every well-formed `/api/coupon-status` request makes one `eth_call` to `SEPOLIA_RPC_URL`, even when no coupon in it is valid. The route checks no coupon signature, so anyone can build a coupon it will read, and skipping the read for an invalid batch would not reduce abuse. Add a Cloudflare rate limiting rule on `/api/*` for the Worker's route to cap the RPC traffic callers can cause.

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
| 8 | nonce | keccak256 of the 39 raw bytes of the withdrawal's Holochain transaction ID (prevents replay) |

## Troubleshooting

### "Order not found" in UI
The UI reads order status directly from the blockchain via RPC. If you see this error:
1. Verify the order was deployed: `./deploy-sepolia.sh status`
2. Check ORDER_HASH in `.env` matches the deployed order
3. Ensure `ui/src/lib/orderConfig.ts` has the correct order configuration

### Transaction fails with "Wrong signer"
The coupon was signed with a different key than the one configured in the Rainlang order.
- Check `valid-signer` in `src/holo-claim.rain`
- Ensure `SIGNER_PRIVATE_KEY` in the bridge-orchestrator environment matches

### "Nonce already used"
What it means depends on when the coupon was signed. Coupons signed before the per-withdrawal nonce carry the unix second they were signed in, a ten-digit nonce.

- **Ten-digit nonce:** coupons signed in the same second shared that nonce, so the error can mean a sibling coupon was claimed and this withdrawal is still unpaid. `POST /api/coupon-status` reports the coupon as `redeemed` in both cases. Re-issue the withdrawal (UNYT-1041).
- **Any other nonce:** the withdrawal has been claimed. Every coupon signed for one withdrawal carries the same nonce, so only one of them can be claimed.

## Security Notes

- Rotate the `SEPOLIA_RPC_URL` API key once the faucet fix (UNYT-1040) is deployed: until then `/api/faucet` answered a failed RPC call with an error that held the full RPC URL. Change the `SEPOLIA_RPC_URL` Workers Builds build variable, which `ui/scripts/cf-deploy.sh` re-applies as the runtime secret on every deploy.
- Never commit `.env` files with private keys
- The test signer key in this repo is for testing only
- In production, use Fireblocks MPC or similar secure key management
- The admin key controls emergency withdrawals - protect it carefully
