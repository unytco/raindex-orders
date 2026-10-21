# HOT <> Bridged HOT Bridge

A two-way bridge between HOT tokens on Ethereum and bridged HOT on Holochain, on Raindex V6.

## Overview

This repository contains the Ethereum-side infrastructure for the HOT <> bridged HOT swap:

- **LOCK**: Users send HOT on Ethereum and receive bridged HOT on Holochain
- **CLAIM**: Users burn bridged HOT on Holochain and receive HOT on Ethereum via signed coupons

```
┌─────────────────────────────────────────────────────────────────┐
│                    SHARED LIQUIDITY POOL                         │
│         (Raindex V6 vault owned by HoloLockVault)               │
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
| `src/ClaimInputToken.sol` | The claim order's input token, which moves nothing | Solidity |
| `src/holo-claim.rain` | Rainlang expression for validating claim coupons | Rainlang |
| `src/Constants.sol` | Raindex V6 and Rainlang at the addresses Rain deployed, from the release files | Solidity |
| `bridge-orchestrator/` | Service that watches Lock events, drives the Holochain bridge, and generates withdrawal coupons | Rust |
| `ui/` | Web interface for locking and claiming | SvelteKit |

## Quick Start

### Prerequisites

- [Foundry](https://book.getfoundry.sh/getting-started/installation) (forge, cast)
- [Rust](https://rustup.rs/) (for bridge-orchestrator)
- [Node.js 20+](https://nodejs.org/) (for UI)
- A Base Sepolia deployer wallet with Base Sepolia ETH (a Ledger or an encrypted keystore), and MetaMask for the UI

The Solidity side builds on Soldeer packages: `forge soldeer install` once, then `forge build`. It takes Raindex V6 and Rainlang at the addresses Rain deployed on Ethereum mainnet and Base Sepolia, the same on both ([DEPLOY.md](./DEPLOY.md#raindex-v6-and-rainlang)).

### 1. Deploy the TestNet on Base Sepolia

```bash
# Set up environment: addresses only, never a private key
cp .env.example .env
# Keep the deployer key in an encrypted keystore, entered at a prompt
cast wallet import deployer --interactive

# Deploy all contracts
./deploy-base-sepolia.sh token --account deployer            # Deploy MockHOT token
./deploy-base-sepolia.sh vault --account deployer            # Deploy HoloLockVault and its ClaimInputToken
./deploy-base-sepolia.sh mint --account deployer             # Mint test tokens
./deploy-base-sepolia.sh order-via-vault --account deployer  # Deploy claim order
```

The TestNet moved from Ethereum Sepolia, which has no Raindex V6, to Base Sepolia (chain 84532): [DEPLOY.md](./DEPLOY.md#deploy-the-testnet-on-base-sepolia). Mainnet deploys with `deploy-mainnet.sh`, rehearsed first on a fork: [DEPLOY.md](./DEPLOY.md#mainnet-deploy).

### 2. Run the UI

```bash
cd ui
npm install
npm run dev
# Open http://localhost:5173
```

To try Lock and Claim with MetaMask before deploying, `test/local-ui.sh <your address>` deploys the TestNet on a local anvil fork of Base Sepolia and points the dev server at it, and `npm run make-coupon` in `ui/` makes a coupon to claim: [DEPLOY.md](./DEPLOY.md#test-the-website-locally).

### 3. Run the bridge orchestrator

```bash
cd bridge-orchestrator
# Set HOLOCHAIN_BRIDGING_AGENT_PUBKEY, SIGNER_PRIVATE_KEY and the deploy record's
# values. What is the same on every chain defaults to TestNet's
# (bridge-orchestrator/README.md)
cargo run -- run
```

The orchestrator watches Lock events on Ethereum, drives the Holochain bridge, and generates signed withdrawal coupons for claim flows.

## Deployments

| Contract | Ethereum mainnet and Base Sepolia |
|----------|---------|
| RaindexV6 (rain.orderbook 0.1.15) | `0x37FC0EFec37D19f8A221aa4F8F7600C9ba2AcD20` |
| RaindexV6SubParser | `0x09Bc7AF266012F44fb41D8Bd682da931666605e1` |
| Rainlang 0.1.9 expression deployer, interpreter, store | `0xb90c299a8321fbdd8D0F367e560f5341F840dA3F`, `0x4c52eee7Fb6aeD0F1a30130d96d02409D9d886cf`, `0x1Aa775533E28B1D843e1A589034984E3a62005DC` |
| Test Signer (TestNet only) | `0x8E72b7568738da52ca3DCd9b24E178127A4E7d37` |

### TestNet on Base Sepolia

| Contract | Address on Base Sepolia |
|----------|---------|
| Mock HOT | `0x916A830d1603252152fE17258bb10A393F173a72` |
| HoloLockVault | `0xF9ca23d23E92C3D7BD08AfCED901Df2Ddb173937` |
| ClaimInputToken | `0xfd7F9439C8f57Bf68619BcFFe0790b2628fa5928` |
| Claim order hash (owner: the vault) | `0x788f1663131e3010290f4f6995a5013a1ac557428b181f598c38d5e1be965ccb` |
| Claim order signer | the test signer |

`deploy-base-sepolia.sh` wrote these to `.env`, and `broadcast/*/84532/run-latest.json` records each transaction. A TestNet website build takes them from `ui/src/lib/network.ts` when it is not given them. For a `.env` for this deploy, copy `.env.base-sepolia.example`, which has them filled in. For the website, copy `ui/.env.base-sepolia.example` to `ui/.env.local` and set `FAUCET_PRIVATE_KEY`, the one value it needs that is not committed. The old Sepolia bridge ran on OrderBookV3 and does not carry over.

## Documentation

- [DEPLOY.md](./DEPLOY.md) - Detailed deployment guide
- [LOCK_INFRASTRUCTURE_PLAN.md](./LOCK_INFRASTRUCTURE_PLAN.md) - Architecture and design documentation

## How It Works

### Lock Flow (HOT -> Bridged HOT)

1. User approves HoloLockVault to spend their HOT
2. User calls `lock(amount, holochainAgentPubKey)`
3. HoloLockVault deposits tokens to its Raindex V6 vault, as a decimal float of the wei
4. `Lock` event emitted with amount and Holochain agent
5. Bridge orchestrator detects the event
6. Holochain side credits bridged HOT to agent

### Claim Flow (Bridged HOT -> HOT)

1. User burns bridged HOT on Holochain
2. The bridge orchestrator signs a coupon with the coupon signer ([docs/enable-multisig.md](./docs/enable-multisig.md))
3. User receives coupon (URL or direct)
4. User visits claim page and submits coupon
5. Rainlang expression validates coupon (signer, expiry, nonce)
6. HOT transferred from vault to user's wallet. The claimer pays the order's input token at the order's ratio of 1e-30 per HOT, which the ClaimInputToken never moves

## Development

```bash
# Fetch the Soldeer packages, then build contracts
forge soldeer install
forge build

# Run tests: the unit tests, and the claim order on forks of Ethereum mainnet
# (RPC_URL_ETH) and Base Sepolia (RPC_URL_BASE_SEPOLIA), public RPCs by default,
# at the latest block unless FORK_BLOCK_ETH / FORK_BLOCK_BASE_SEPOLIA pins one
forge test

# Build the bridge orchestrator
(cd bridge-orchestrator && cargo build)

# Rehearse the mainnet deploy, with a Safe as the vault admin, and the coupon
# signer's move to a Safe on an anvil fork, after building the orchestrator above
nix develop -c test/fork-rehearsal.sh
```

## Security

- Test signer key in repo is for testing only, and every mainnet input refuses it
- The coupon signer is a key with no EIP-7702 delegation, or a Safe: Raindex V6 asks a delegated key's delegate for a coupon's signature, so the scripts and the orchestrator refuse one ([docs/enable-multisig.md](./docs/enable-multisig.md))
- On mainnet the vault admin is a Safe multisig from the deploy's first transaction, and the coupon signer is a key or a Safe: [docs/enable-multisig.md](./docs/enable-multisig.md). Each key holder follows [docs/key-holder.md](./docs/key-holder.md). A Safe coupon signer whose owner keys one orchestrator holds is still one party; M-of-N needs the signer service of [docs/signer-service.md](./docs/signer-service.md), which is not built
- Each coupon has a unique nonce (prevents replay)
- Coupons have expiry timestamps
- Admin functions protected by access control
