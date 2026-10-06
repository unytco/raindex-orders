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
| `bridge-orchestrator/` | Service that watches Lock events, drives the Holochain bridge, and generates withdrawal coupons | Rust |
| `ui/` | Web interface for locking and claiming | SvelteKit |

## Quick Start

### Prerequisites

- [Foundry](https://book.getfoundry.sh/getting-started/installation) (forge, cast)
- [Rust](https://rustup.rs/) (for bridge-orchestrator)
- [Node.js 20+](https://nodejs.org/) (for UI)
- A Sepolia deployer wallet with Sepolia ETH (a Ledger or an encrypted keystore), and MetaMask for the UI

### 1. Deploy to Sepolia

```bash
# Set up environment: addresses only, never a private key
cp .env.example .env
# Keep the deployer key in an encrypted keystore, entered at a prompt
cast wallet import deployer --interactive

# Deploy all contracts
./deploy-sepolia.sh token --account deployer            # Deploy MockHOT token
./deploy-sepolia.sh vault --account deployer            # Deploy HoloLockVault
./deploy-sepolia.sh mint --account deployer             # Mint test tokens
./deploy-sepolia.sh order-via-vault --account deployer  # Deploy claim order
```

Mainnet deploys with `deploy-mainnet.sh`, rehearsed first on a fork: [DEPLOY.md](./DEPLOY.md#mainnet-deploy).

### 2. Run the UI

```bash
cd ui
npm install
npm run dev
# Open http://localhost:5173
```

### 3. Run the bridge orchestrator

```bash
cd bridge-orchestrator
# Set HOLOCHAIN_BRIDGING_AGENT_PUBKEY and SIGNER_PRIVATE_KEY. TestNet values are the
# defaults for the rest (bridge-orchestrator/README.md)
cargo run -- run
```

The orchestrator watches Lock events on Ethereum, drives the Holochain bridge, and generates signed withdrawal coupons for claim flows.

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
5. Bridge orchestrator detects the event
6. Holochain side credits bridged HOT to agent

### Claim Flow (Bridged HOT -> HOT)

1. User burns bridged HOT on Holochain
2. The bridge orchestrator signs a coupon with the coupon signer ([docs/enable-multisig.md](./docs/enable-multisig.md))
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

# Build the bridge orchestrator
(cd bridge-orchestrator && cargo build)

# Rehearse the mainnet deploy and the move to Safes on an anvil fork,
# after building the orchestrator above
nix develop -c test/fork-rehearsal.sh
```

## Security

- Test signer key in repo is for testing only, and every mainnet input refuses it
- Who holds the coupon signer and the vault admin, and how each moves to a multisig: [docs/enable-multisig.md](./docs/enable-multisig.md). Each key holder follows [docs/key-holder.md](./docs/key-holder.md)
- Each coupon has a unique nonce (prevents replay)
- Coupons have expiry timestamps
- Admin functions protected by access control
