# HOT <> Bridged HOT Bridge - Architecture Documentation

## Overview

This document describes the architecture of the two-way bridge between HOT tokens on Ethereum and bridged HOT on Holochain:

- **LOCK**: User sends HOT on Ethereum -> receives bridged HOT on Holochain
- **CLAIM** (UNLOCK): User burns bridged HOT on Holochain -> receives HOT on Ethereum

## Implementation Status

| Component | Status | Location |
|-----------|--------|----------|
| HoloLockVault Contract | Complete | `src/HoloLockVault.sol` |
| MockHOT Token | Complete | `src/MockHOT.sol` |
| Rainlang Claim Expression | Complete | `src/holo-claim.rain` |
| Bridge Orchestrator (Rust) | Complete | `bridge-orchestrator/` |
| Web UI | Complete | `ui/` |
| Deployment Scripts | Complete | `deploy-sepolia.sh`, `script/` |
| Foundry Tests | Complete | `test/` |
| Holochain Integration | Pending | - |

## System Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         SHARED LIQUIDITY POOL                                │
│           (Raindex Orderbook Vault owned by HoloLockVault)                  │
│                                                                              │
│                            ┌───────────────┐                                │
│      LOCK (HOT→bHOT)       │   HOT Tokens  │        CLAIM (bHOT→HOT)        │
│    ─────────────────►      │               │      ◄─────────────────        │
│      Deposits INTO         │   Balance: N  │         Withdraws FROM         │
│                            └───────────────┘                                │
│                                                                              │
│   Order: holo-claim.rain (owned by HoloLockVault)                           │
│   - Validates signed coupons from trusted signer                            │
│   - Transfers tokens to coupon recipient                                    │
│   - Prevents replay via nonce tracking                                      │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Key Design Decision: Shared Vault Ownership

In Raindex, vaults are identified by: `(owner_address, token_address, vault_id)`

The **HoloLockVault contract** owns both:
1. The vault where locked HOT is deposited
2. The claim order that allows withdrawals via signed coupons

This ensures LOCK deposits and CLAIM withdrawals operate on the **same pool of tokens**.

---

## LOCK Flow: HOT -> Bridged HOT

```
    ETHEREUM                           │                      HOLOCHAIN
                                       │
┌─────────────┐                        │
│ User Wallet │                        │
│             │                        │
│  100 HOT    │                        │
└──────┬──────┘                        │
       │                               │
       │ 1. approve(HoloLockVault,     │
       │           100 HOT)            │
       │                               │
       │ 2. lock(100 HOT,              │
       │        holochainAgentPubKey)  │
       ▼                               │
┌──────────────────────┐               │
│   HoloLockVault      │               │
│                      │               │
│ • transferFrom(user) │               │
│ • deposit to vault   │               │
│ • emit Lock event    │───────────────┼─────────┐
└──────────────────────┘               │         │
       │                               │         │
       │ 3. Tokens deposited           │         │ 4. Lock event detected
       ▼                               │         ▼
┌──────────────────────┐               │  ┌─────────────────────┐
│   Raindex Orderbook  │               │  │ Bridge Orchestrator │
│                      │               │  │ (bridge-orchestrator)│
│ Vault balance:       │               │  │                     │
│ +100 HOT             │               │  │ • Poll for events   │
└──────────────────────┘               │  │ • Drive Holochain   │
                                       │  └──────────┬──────────┘
                                       │             │
                                       │             │ 5. Notify Holochain
                                       │             │    (implementation TBD)
                                       │             ▼
                                       │  ┌─────────────────────┐
                                       │  │   Bridged HOT DNA   │
                                       │  │                     │
                                       │  │ • Credit bridged    │
                                       │  │   HOT to agent      │
                                       │  └─────────────────────┘
```

### Lock Flow Steps

| Step | Action | Component | Status |
|------|--------|-----------|--------|
| 1 | User approves HoloLockVault | HOT ERC20 | Complete |
| 2 | User calls `lock(amount, agent)` | HoloLockVault | Complete |
| 3 | Contract deposits to orderbook | Raindex Orderbook | Complete |
| 4 | Bridge orchestrator detects event | bridge-orchestrator | Complete |
| 5 | Holochain notified of lock | TBD | **Pending** |
| 6 | Bridged HOT credited to agent | Bridged HOT DNA | **Pending** |

---

## CLAIM Flow: Bridged HOT -> HOT

```
    HOLOCHAIN                          │                      ETHEREUM
                                       │
┌─────────────────────┐                │
│  Holochain Agent    │                │
│                     │                │
│  Balance: 100 bHOT  │                │
└──────────┬──────────┘                │
           │                           │
           │ 1. [FUTURE] Request       │
           │    redemption (burn bHOT) │
           ▼                           │
┌─────────────────────┐                │
│   Bridged HOT DNA   │                │
│                     │                │
│ • [FUTURE] Burn     │                │
│   bridged HOT       │                │
│ • Notify backend    │────────────────┼─────────┐
└─────────────────────┘                │         │
                                       │         │
                                       │         │ 2. Generate signed
                                       │         │    coupon
                                       │         ▼
                                       │  ┌─────────────────────┐
                                       │  │ Bridge Orchestrator │
                                       │  │ (bridge-orchestrator)│
                                       │  │                     │
                                       │  │ • Create coupon:    │
                                       │  │   - recipient       │
                                       │  │   - amount          │
                                       │  │   - expiry          │
                                       │  │   - orderHash       │
                                       │  │   - nonce           │
                                       │  │ • Sign with key     │
                                       │  └──────────┬──────────┘
                                       │             │
                                       │             │ 3. Coupon delivered
                                       │             │    to user
                                       │             ▼
┌──────────────────────────────────────┼──────────────────────────────────────┐
│                                      │                                       │
│   ┌─────────────┐                    │                                       │
│   │ User Wallet │◄───────────────────┼───────────────────────────────────────│
│   │             │  4. User visits    │                                       │
│   │  (Ethereum) │     claim URL      │                                       │
│   └──────┬──────┘                    │                                       │
│          │                           │                                       │
│          │ 5. takeOrders(            │                                       │
│          │      order,               │                                       │
│          │      signedCoupon)        │                                       │
│          ▼                           │                                       │
│   ┌──────────────────────┐           │                                       │
│   │   Raindex Orderbook  │           │                                       │
│   │                      │           │                                       │
│   │ • Evaluate Rainlang  │           │                                       │
│   │ • Verify coupon:     │           │                                       │
│   │   - Signer matches   │           │                                       │
│   │   - Not expired      │           │                                       │
│   │   - Nonce unused     │           │                                       │
│   │ • Transfer HOT       │           │                                       │
│   └──────────┬───────────┘           │                                       │
│              │                       │                                       │
│              │ 6. HOT transferred    │                                       │
│              ▼                       │                                       │
│   ┌─────────────┐                    │                                       │
│   │ User Wallet │                    │                                       │
│   │             │                    │                                       │
│   │  +100 HOT   │                    │                                       │
│   └─────────────┘                    │                                       │
│                                      │                                       │
│              ETHEREUM                │                                       │
└──────────────────────────────────────┴───────────────────────────────────────┘
```

### Claim Flow Steps

| Step | Action | Component | Status |
|------|--------|-----------|--------|
| 1 | User burns bridged HOT | Bridged HOT DNA | **Pending** |
| 2 | Backend generates coupon | bridge-orchestrator | Complete |
| 3 | User receives coupon | Email/App/URL | Complete |
| 4 | User visits claim page | ui/ | Complete |
| 5 | User calls `takeOrders()` | Raindex Orderbook | Complete |
| 6 | HOT transferred | Orderbook | Complete |

---

## Implemented Components

### 1. HoloLockVault Contract (`src/HoloLockVault.sol`)

**Purpose**: Wrapper contract that owns the Raindex vault and claim order.

```solidity
contract HoloLockVault {
    // Events
    event Lock(address indexed sender, uint256 amount, bytes32 indexed holochainAgent, uint256 lockId);

    // Core functions
    function lock(uint256 amount, bytes32 holochainAgent) external returns (uint256 lockId);
    function vaultBalance() external view returns (uint256);

    // Admin functions (order management)
    function addOrder(OrderConfigV2 calldata config) external onlyAdmin returns (bool);
    function removeOrder(OrderV2 calldata order) external onlyAdmin returns (bool);
    function adminWithdraw(uint256 amount, address to) external onlyAdmin;
}
```

**Key Features**:
- Emits `Lock` event with Holochain agent public key (bytes32)
- Deposits to Raindex orderbook vault owned by this contract
- Can deploy/manage claim orders (making itself the order owner)
- Admin controls for emergency withdrawal

### 2. Rainlang Claim Expression (`src/holo-claim.rain`)

**Purpose**: Validates signed coupons on-chain.

```rainlang
#calculate-io
using-words-from orderbook-subparser

/* Validate coupon signature and parameters */
:ensure(equal-to(signer<0>() valid-signer) "Wrong signer"),
:ensure(equal-to(signed-context<0 0>() order-counterparty()) "Wrong recipient"),
:ensure(less-than(block-timestamp() signed-context<0 2>()) "Order expired"),
:ensure(equal-to(signed-context<0 3>() order-hash()) "Wrong order hash"),
:ensure(equal-to(signed-context<0 4>() order-owner()) "Wrong order owner"),
:ensure(equal-to(signed-context<0 5>() orderbook()) "Wrong orderbook"),
:ensure(equal-to(signed-context<0 6>() output-token()) "Wrong output token"),
:ensure(equal-to(signed-context<0 7>() output-vault-id()) "Wrong output vault id"),

/* Prevent replay attacks via nonce */
:ensure(is-zero(get(hash(order-hash() signed-context<0 8>()))) "Nonce already used"),
:set(hash(order-hash() signed-context<0 8>()) 1),

/* Output claim amount (io-ratio=0 means free claim) */
output-amount: signed-context<0 1>(),
io-ratio: 0;
```

**Coupon Context Fields**:
| Index | Field | Description |
|-------|-------|-------------|
| 0 | recipient | Ethereum address to receive tokens |
| 1 | amount | Token amount in wei |
| 2 | expiry | Unix timestamp when coupon expires |
| 3 | orderHash | Hash of the claim order |
| 4 | orderOwner | HoloLockVault address |
| 5 | orderbook | Orderbook contract address |
| 6 | outputToken | Token address |
| 7 | outputVaultId | Vault ID |
| 8 | nonce | Unique nonce (prevents replay) |

### 3. Bridge Orchestrator (`bridge-orchestrator/`)

**Purpose**: Rust service that watches Lock events on Ethereum, drives the Holochain bridge, and generates signed withdrawal coupons. Replaces the legacy `lock-watcher-rs` and `coupon-signer` crates.

```bash
cd bridge-orchestrator
cargo run
```

**Responsibilities**:
- Polls Ethereum RPC for Lock events and hands them off to the Holochain side
- Scans bridging entries on Holochain and emits unified bridging RAVE transactions
- Generates signed withdrawal coupons (`src/signer.rs::generate_coupon`) in the URL-safe `signer,signature,ctx0..ctx8` format consumed by the UI claim page
- Tracks state required to batch coupons up to a configurable size cap

**Configuration** (via `.env`):
- `SIGNER_PRIVATE_KEY`: Key that matches `valid-signer` in Rainlang
- `ORDER_HASH`, `ORDER_OWNER`, `ORDERBOOK_ADDRESS`, `TOKEN_ADDRESS`, `VAULT_ID`
- Network/RPC settings, polling interval, coupon batch size cap (see `bridge-orchestrator/src/config.rs`)

### 4. Web UI (`ui/`)

**Purpose**: SvelteKit web interface for locking and claiming.

**Routes**:
- `/` - Home page with lock/claim selector
- `/lock` - Lock HOT to receive bridged HOT
- `/claim` - Claim HOT with coupon
- `/claim?c=<coupon>` - Direct claim via URL parameter

**Key Features**:
- Direct MetaMask integration (no WalletConnect dependency)
- Reads order status directly from blockchain via RPC
- Hardcoded order configuration (no subgraph dependency)
- Transaction status modal

**Configuration** (`ui/.env`):
```env
PUBLIC_ORDERBOOK_ADDRESS=0xfca89cD12Ba1346b1ac570ed988AB43b812733fe
PUBLIC_LOCK_VAULT_ADDRESS=0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6
PUBLIC_TOKEN_ADDRESS=0xeaC8eEEE9f84F3E3F592e9D8604100eA1b788749
```

---

## Sepolia Deployment

| Contract | Address |
|----------|---------|
| MockHOT Token | `0xeaC8eEEE9f84F3E3F592e9D8604100eA1b788749` |
| HoloLockVault | `0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6` |
| Raindex Orderbook | `0xfca89cD12Ba1346b1ac570ed988AB43b812733fe` |
| Expression Deployer | `0xd19581a021f4704ad4eBfF68258e7A0a9DB1CD77` |
| Orderbook Subparser | `0xe6A589716d5a72276C08E0e08bc941a28005e55A` |
| NOOP Token (placeholder) | `0x555FA2F68dD9B7dB6c8cA1F03bFc317ce61e9028` |
| Claim Order Hash | `0x5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9` |
| Test Signer | `0x8E72b7568738da52ca3DCd9b24E178127A4E7d37` |
| Vault ID | `0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b` |

---

## Holochain Integration Points

### Lock Flow -> Holochain

The bridge-orchestrator handles Lock-to-Holochain:

1. **Current State**: Detects Lock events and forwards them into the Holochain bridge cycle
2. **Next Step**: Refine the notification/reservation mechanism once Holochain-side requirements are finalised

**Integration Options** (to be determined based on Holochain requirements):
- Direct Holochain conductor API call
- Signed certificate submission (optional, if Holochain requires cryptographic proof)
- WebSocket/webhook notification

**Optional Certificate Structure** (if cryptographic proof is needed):
```rust
pub struct ReserveCertificate {
    pub eth_tx_hash: [u8; 32],      // Ethereum tx hash
    pub amount: u128,                // HOT amount in wei
    pub lock_id: u64,                // Lock ID from contract
    pub recipient: AgentPubKey,      // Holochain agent
    pub expiry: Timestamp,           // Certificate expiry
    pub signature: Signature,        // Fireblocks signature (optional)
}
```

### Claim Flow <- Holochain

The bridge-orchestrator also handles coupon generation for claims:

1. **Current State**: Orchestrator generates signed coupons with the configured signer key
2. **Next Step**: Integrate with Fireblocks MPC for production signing
3. **Final Step**: Trigger from bridged HOT burn events

**Integration Flow**:
1. User burns bridged HOT in Bridged HOT DNA
2. Bridged HOT DNA notifies Holo backend
3. Bridge orchestrator generates a signed coupon
4. Signed coupon delivered to user (email, app notification, etc.)
5. User claims on Ethereum

---

## Security Considerations

### Smart Contract Security
- **Reentrancy**: Uses SafeERC20, deposits before external calls
- **Access Control**: Admin functions protected by `onlyAdmin` modifier
- **Integer Overflow**: Solidity 0.8+ has built-in protection

### Coupon Security
- **Signature Verification**: Rainlang verifies signer matches `valid-signer`
- **Replay Protection**: Each coupon has unique nonce, stored on-chain
- **Expiry**: Coupons have timestamp-based expiry
- **Parameter Binding**: Coupon bound to specific order, owner, token, vault

### Bridge Security
- **Finality**: Bridge orchestrator should wait for sufficient confirmations (15+)
- **Replay Protection**: Track processed lock IDs on both sides
- **Same Signer**: Use same trusted signer (Fireblocks) for both directions

### Key Management
- **Test Key**: Included in repo for testing only
- **Production**: Use Fireblocks MPC or similar secure key management
- **Admin Key**: Protects emergency withdrawal - should be multisig

---

## Files Reference

```
raindex-orders/
├── src/
│   ├── HoloLockVault.sol       # Lock vault contract
│   ├── MockHOT.sol             # Test token
│   └── holo-claim.rain         # Rainlang claim expression
├── script/
│   ├── DeployMockHOT.s.sol     # Token deployment
│   ├── DeployHoloLockVault.s.sol # Vault deployment
│   └── DeployClaimOrderViaVault.s.sol # Order deployment
├── test/
│   └── HoloLockVault.t.sol     # Foundry tests
├── bridge-orchestrator/        # Rust bridge orchestrator (lock watcher + coupon signer)
│   ├── src/
│   └── .env.example
├── ui/                         # SvelteKit web UI
│   ├── src/routes/
│   │   ├── +page.svelte        # Home
│   │   ├── lock/+page.svelte   # Lock page
│   │   └── claim/+page.svelte  # Claim page
│   └── src/lib/
│       ├── orderConfig.ts      # Hardcoded order config
│       ├── coupon.ts           # Coupon parsing
│       └── ethereum.ts         # MetaMask integration
├── deploy-sepolia.sh           # Deployment script
├── DEPLOY.md                   # Deployment guide
└── README.md                   # Project overview
```
