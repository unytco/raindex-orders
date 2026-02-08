# HOT <> Bridged HOT Bridge

A two-way bridge between HOT tokens on Ethereum and bridged HOT on Holochain.

## Overview

This repository contains the Ethereum-side infrastructure for the HOT <> bridged HOT swap:

- **LOCK**: Users send HOT on Ethereum and receive bridged HOT on Holochain
- **CLAIM**: Users burn bridged HOT on Holochain and receive HOT on Ethereum via signed coupons

```
┌─────────────────────────────────────────────────────────────────┐
│                    SHARED LIQUIDITY POOL                         │
│         (Raindex Orderbook Vault owned by HoloLockVault)        │
│                                                                  │
│   LOCK (HOT→bHOT)            HOT Tokens             CLAIM (bHOT→HOT)
│   ─────────────────►    ┌───────────────┐    ◄─────────────────
│   Deposits INTO         │               │         Withdraws FROM
│                         └───────────────┘                        │
└─────────────────────────────────────────────────────────────────┘
```

## Components

| Component | Description | Language |
|-----------|-------------|----------|
| `src/HoloLockVault.sol` | Smart contract for locking HOT and managing claim orders | Solidity |
| `src/holo-claim.rain` | Rainlang expression for validating claim coupons | Rainlang |
| `coupon-signer/` | CLI tool for generating signed claim coupons | Rust |
| `lock-watcher-rs/` | Service that monitors Lock events on Ethereum | Rust |
| `ui/` | Web interface for locking and claiming | SvelteKit |

## Quick Start

### Prerequisites

- [Foundry](https://book.getfoundry.sh/getting-started/installation) (forge, cast)
- [Rust](https://rustup.rs/) (for coupon-signer and lock-watcher)
- [Node.js 20+](https://nodejs.org/) (for UI)
- MetaMask with Sepolia ETH

### 1. Deploy to Sepolia

```bash
# Set up environment
cp .env.example .env
# Edit .env with your private key

# Deploy all contracts
./deploy-sepolia.sh token       # Deploy MockHOT token
./deploy-sepolia.sh vault       # Deploy HoloLockVault
./deploy-sepolia.sh mint        # Mint test tokens
./deploy-sepolia.sh fund        # Fund the vault
./deploy-sepolia.sh order-via-vault  # Deploy claim order
```

### 2. Run the UI

```bash
cd ui
npm install
npm run dev
# Open http://localhost:5173
```

### 3. Test Locking

```bash
cd lock-watcher-rs
cp .env.example .env
# Edit with SEPOLIA_LOCK_VAULT_ADDRESS
cargo run
```

Then use the UI to lock tokens. The watcher will detect the Lock event.

### 4. Test Claiming

```bash
cd coupon-signer
cargo run -- --amount "10" --recipient "0xYourAddress" --format ui
```

Paste the output into the claim page at http://localhost:5173/claim

## Sepolia Deployment

| Contract | Address |
|----------|---------|
| MockHOT Token | `0xeaC8eEEE9f84F3E3F592e9D8604100eA1b788749` |
| HoloLockVault | `0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6` |
| Orderbook (Raindex) | `0xfca89cD12Ba1346b1ac570ed988AB43b812733fe` |
| Claim Order Hash | `0x5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9` |

## Documentation

- [DEPLOY.md](./DEPLOY.md) - Detailed deployment guide
- [LOCK_INFRASTRUCTURE_PLAN.md](./LOCK_INFRASTRUCTURE_PLAN.md) - Architecture and design documentation

## How It Works

### Lock Flow (HOT -> Bridged HOT)

1. User approves HoloLockVault to spend their HOT
2. User calls `lock(amount, holochainAgentPubKey)`
3. HoloLockVault deposits tokens to its Raindex vault
4. `Lock` event emitted with amount and Holochain agent
5. Lock watcher detects event
6. Holochain side credits bridged HOT to agent

### Claim Flow (Bridged HOT -> HOT)

1. User burns bridged HOT on Holochain
2. Holo backend generates signed coupon via Fireblocks
3. User receives coupon (URL or direct)
4. User visits claim page and submits coupon
5. Rainlang expression validates coupon (signer, expiry, nonce)
6. HOT transferred from vault to user's wallet

## Development

```bash
# Build contracts
forge build

# Run tests
forge test

# Build coupon-signer
cd coupon-signer && cargo build

# Build lock-watcher
cd lock-watcher-rs && cargo build
```

## Security

- Test signer key in repo is for testing only
- Production uses Fireblocks MPC for signing
- Each coupon has a unique nonce (prevents replay)
- Coupons have expiry timestamps
- Admin functions protected by access control
