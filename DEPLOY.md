# Holo Bridge deployment guide

This guide covers deploying and testing the complete HOT <> bridged HOT bridge infrastructure on Sepolia testnet, and [deploying it on Ethereum mainnet](#mainnet-deploy).

## Prerequisites

1. MetaMask wallet with Sepolia ETH (get from [Sepolia Faucet](https://sepoliafaucet.com/))
2. Foundry installed (`forge`, `cast` commands available)
3. Rust toolchain (for bridge-orchestrator)
4. Node.js 20+ (for UI)

## Quick Start

### 1. Set Up Environment

```bash
# Copy example env file. It holds addresses, never a private key.
cp .env.example .env

# Keep the deployer key in an encrypted keystore, entered at a prompt
cast wallet import deployer --interactive
```

Every step that sends a transaction takes the deployer as `--account deployer`, or `--ledger` for a Ledger. `deploy-sepolia.sh` refuses a `PRIVATE_KEY` in `.env` and a `--private-key` option.

### 2. Deploy All Contracts

The `deploy-sepolia.sh` script handles all deployment steps:

```bash
# Check your wallet and balance
./deploy-sepolia.sh status --account deployer

# Step 1: Deploy MockHOT token
./deploy-sepolia.sh token --account deployer
# Note: Updates .env with TOKEN_ADDRESS automatically

# Step 2: Deploy HoloLockVault
./deploy-sepolia.sh vault --account deployer
# Note: Updates .env with LOCK_VAULT_ADDRESS automatically

# Step 3: Mint test tokens to your wallet
./deploy-sepolia.sh mint --account deployer

# Step 5: Deploy claim order via HoloLockVault, accepting coupons from
# VALID_SIGNER (the test signer unless set)
./deploy-sepolia.sh order-via-vault --account deployer
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

## Mainnet deploy

`deploy-mainnet.sh` deploys the bridge on Ethereum mainnet against the existing OrderBookV3. It deploys no orderbook.

```bash
npm ci   # compose-rainlang.mjs, which composes the claim expression
ETH_RPC_URL=<an Ethereum RPC> \
ADMIN_ADDRESS=<the vault's final admin> \
VALID_SIGNER=<the coupon signer's address> \
./deploy-mainnet.sh --ledger            # or --account <keystore>
```

- **Refusals.** Before it sends anything, it refuses a chain other than 1, a missing input, a raw key option such as `--private-key`, an `ADMIN_ADDRESS` equal to the deployer, and a `VALID_SIGNER` equal to the test signer.
- **What it sends.** `HoloLockVault(HOT, OrderBookV3, HOLO_VAULT_ID, deployer, MIN_LOCK_AMOUNT)`, then the claim order through the vault, then `setAdmin(ADMIN_ADDRESS)`. It sends them one at a time, each after the one before has landed.
- **The record.** It reads back the vault's token, orderbook, vault ID and admin, and the claim order, from the chain. Then it prints the deploy record under the variable names the orchestrator and the website take.
- **Failure.** A run cannot be resumed. A run that stops part way prints the transactions it sent. A new run deploys a new vault.

### Rehearse it first

`test/fork-rehearsal.sh` runs `deploy-mainnet.sh` and `rotate-claim-signer.sh`, unchanged, against an anvil fork of mainnet. Every transaction goes to an anvil it starts on 127.0.0.1.

```bash
nix develop -c test/fork-rehearsal.sh   # FORK_URL sets the mainnet RPC that anvil forks
```

It proves that the deploy sends nothing while it refuses a chain other than 1 (a plain anvil on chain 31337), a missing input, a raw key, the deployer as admin and the test signer. Then it deploys on the fork, and proves that the orchestrator passes its startup checks with the deploy record and the coupon signer's key, and refuses another key. It proves that a lock lands in the vault, that a coupon signed as the orchestrator signs claims it back, and that nonce reuse, a wrong signer and an expired coupon fail. It proves that the admin is `ADMIN_ADDRESS` and the deployer is locked out. Last, it moves the vault admin and the coupon signer to 2 of 3 Safes, as [docs/enable-multisig.md](./docs/enable-multisig.md) describes, and proves that a coupon with two owner signatures claims and one with a single signature fails. The rotation refuses the test signer and a Safe of threshold 1, and its record refuses a signer the new order does not accept, a second order of the vault's left on the orderbook, and an order from another expression deployer. The orchestrator refuses to start for the Safe as coupon signer.

The rehearsal runs the orchestrator binary, so build it first: `cargo build` in `bridge-orchestrator`, on its pinned toolchain, or set `ORCHESTRATOR` to a built binary.

### Website

One SvelteKit build per network, each its own Cloudflare Worker: `hot-bridge-ui` for Sepolia, and `hot-bridge-ui-mainnet`, the wrangler env `mainnet`. Each Workers Builds project sets `PUBLIC_NETWORK` and the `PUBLIC_*` build variables the deploy record prints, and its RPC secret: `SEPOLIA_RPC_URL` and `FAUCET_PRIVATE_KEY`, or `ETH_RPC_URL`. `PUBLIC_NETWORK` defaults to `sepolia`, and a sepolia build takes TestNet's value, in `ui/src/lib/network.ts`, for any variable it is not given, and logs which. A mainnet build takes none: it fails when a variable is missing or malformed. Either fails when the claim order the variables describe does not hash to `PUBLIC_CLAIM_ORDER_HASH`, and a mainnet build when the signer is the test signer. The MainNet Workers Builds project must set `PUBLIC_NETWORK=mainnet`. The build cannot check that `PUBLIC_CLAIM_SIGNER` is the order's signer, so take it from the deploy record. The mainnet website has no faucet.

## Testing the Complete Flow

### Lock Flow (HOT -> Bridged HOT)

1. **Start the bridge orchestrator:**
```bash
cd bridge-orchestrator
# Set SIGNER_PRIVATE_KEY: TestNet values are the defaults (bridge-orchestrator/README.md)
cargo run -- run
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
cargo run -- run
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
- `POST /api/coupon-status`: the status of up to 20 claim coupons, read from the build's network through its RPC secret

#### What bounds coupon-status RPC use

- **Signature gate.** A coupon is read only if it names the claim order's signer (`valid-signer` in `src/holo-claim.rain`, `PUBLIC_CLAIM_SIGNER` in the build) and carries a signature that signer gives, checked as the orderbook checks it. A 65-byte signature must recover to the signer, and any other coupon of that form answers `invalid` and is never read. A signature of 2 to 20 owner parts, each a key's 65-byte signature, is a contract signer's, such as a Safe's: the signer's EIP-1271 `isValidSignature` checks it in the same `eth_call` that reads the nonces. Such a coupon causes a read even when it answers `invalid`, until a check finds that the claim signer has no code; for the next 10 minutes that Worker isolate answers such coupons `invalid` without a read, and logs that it does. A coupon signer Safe needs a threshold of 2 or more, as a 65-byte signature is read as a key's.
- **Cache.** Answers are kept in the Workers Cache, keyed by the coupon's signer, signature and context values, so a coupon respelled with other hex case or leading zeros shares its entry. `redeemed` and `expired` are kept from then on. `unredeemed` is kept for 60 s, and never once the coupon's expiry has passed. The Workers Cache is local to each Cloudflare data centre, so each data centre reads a coupon once for itself. A cache read or write that fails is logged and costs only a read; it never changes an answer.
- **One read at most.** A request makes at most one `eth_call` to its network's RPC secret, `SEPOLIA_RPC_URL` or `ETH_RPC_URL`: one Multicall3 `aggregate3` at the `safe` block, reading each uncached nonce once. A request the cache answers in full, or with no valid coupon, makes none, and its `block` is `null`. An RPC that answers for another chain is refused.

A coupon whose expiry has passed, but which the `safe` block still shows unexpired, is read on every request until the `safe` block passes its expiry, about 10 to 15 minutes on Sepolia.

#### Why 20 coupons per request

`hot-bridge-ui` runs on the Workers Free plan, which allows 50 subrequests per request, Cache API `match()` and `put()` calls included, and 10 ms of CPU. A request of N coupons the cache has not seen costs N cache matches, one RPC fetch and N cache puts. On a preview build, 6 to 20 new coupons answered `200`, 25 answered `500` (the 25th put), and 44 or 50 answered `502` (the RPC fetch). So a request takes at most 20 coupons: 41 subrequests and 20 signature checks. Checking a signature costs about 1 ms of CPU, and a coupon the cache holds is not checked again. The Workers Paid plan allows 10,000 subrequests per request.

#### Optional WAF rule

Optionally, for raw floods, add a Cloudflare WAF rate limiting rule in the dashboard (Security, then WAF, then Rate limiting rules) for the zone that serves `hot-bridge.unyt.dev`:

| Setting | Value |
|---|---|
| Match | URI path equals `/api/coupon-status` |
| Count by | IP |
| Rate | 20 requests per 10 s |
| Action | Block for 10 s |

An app sends about one request every 3 minutes, plus one each time Interaction Details is opened, so this sits far above genuine use. These are the Free plan's only period and timeout, and the Free plan cannot match on method. The rule covers the custom domain only, not the Worker's `workers.dev` and version URLs.

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
- Keep private keys out of `.env` files and command lines: sign with `--account` or `--ledger`
- The test signer key in this repo is for testing only, and every mainnet input refuses it
- The coupon signer is one key at launch. A Safe multisig or a Fireblocks MPC wallet can replace it: [docs/enable-multisig.md](./docs/enable-multisig.md)
- The admin key controls emergency withdrawals. It can move to a Safe multisig, whose key holders follow [docs/key-holder.md](./docs/key-holder.md)
