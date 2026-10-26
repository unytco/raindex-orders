# HOT <> Bridged HOT Bridge - Architecture Documentation

## Overview

This document describes the architecture of the two-way bridge between HOT tokens on Ethereum and bridged HOT on Holochain:

- **LOCK**: User sends HOT on Ethereum -> receives bridged HOT on Holochain
- **CLAIM** (UNLOCK): User burns bridged HOT on Holochain -> receives HOT on Ethereum

## Implementation Status

| Component | Status | Location |
|-----------|--------|----------|
| HoloLockVault Contract | Complete, on Raindex V6 | `src/HoloLockVault.sol` |
| ClaimInputToken | Complete | `src/ClaimInputToken.sol` |
| MockHOT Token | Complete | `src/test/MockHOT.sol` |
| Rainlang Claim Expression | Complete, on Rainlang 0.1.9 | `src/holo-claim.rain` |
| Bridge Orchestrator (Rust) | Complete | `bridge-orchestrator/` |
| Web UI | Complete | `ui/` |
| Deployment Scripts | Complete | `deploy-base-sepolia.sh`, `deploy-mainnet.sh`, `script/` |
| TestNet on Base Sepolia | Deployed ([addresses](./README.md#testnet-on-base-sepolia)) | `deploy-base-sepolia.sh` |
| Foundry Tests | Complete | `test/` |
| Holochain Integration | Pending | - |

## System Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         SHARED LIQUIDITY POOL                                │
│           (Raindex V6 vault owned by HoloLockVault)                         │
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

In Raindex V6, vaults are identified by: `(owner_address, token_address, vault_id)`, the vault ID a bytes32, and balances are decimal floats. The vault converts each lock losslessly: the wei as the float's coefficient, -18 as its exponent.

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
    function addOrder(OrderConfigV4 calldata config) external onlyAdmin returns (bool);
    function removeOrder(OrderV4 calldata order) external onlyAdmin returns (bool);
    function adminWithdraw(uint256 amount, address to) external onlyAdmin;
}
```

**Key Features**:
- Emits `Lock` event with Holochain agent public key (bytes32), unchanged from V3
- Deposits to the Raindex V6 vault owned by this contract (`deposit4`), as a lossless decimal float, refusing an amount no float carries
- Can deploy/manage claim orders (making itself the order owner)
- Admin controls for emergency withdrawal (`withdraw4`)

**ClaimInputToken** (`src/ClaimInputToken.sol`): Raindex V6 takes an order only at a positive io-ratio, and pulls at least one unit of the input token from the taker. The claim order's ratio is 1e-30 per HOT, and its input token is this one: every transfer succeeds and nothing is held, so a claimer needs no balance or allowance of anything.

### 2. Rainlang Claim Expression (`src/holo-claim.rain`)

**Purpose**: Validates signed coupons on-chain.

```rainlang
#calculate-io
using-words-from orderbook-subparser

/* Validate coupon signature and parameters: addresses and hashes bit for bit */
:ensure(binary-equal-to(signer<0>() valid-signer) "Wrong signer"),
:ensure(binary-equal-to(signed-context<0 0>() order-counterparty()) "Wrong recipient"),
:ensure(less-than(block-timestamp() signed-context<0 2>()) "Order expired"),
:ensure(binary-equal-to(signed-context<0 3>() order-hash()) "Wrong order hash"),
:ensure(binary-equal-to(signed-context<0 4>() order-owner()) "Wrong order owner"),
:ensure(binary-equal-to(signed-context<0 5>() raindex()) "Wrong orderbook"),
:ensure(binary-equal-to(signed-context<0 6>() output-token()) "Wrong output token"),
:ensure(binary-equal-to(signed-context<0 7>() output-vault-id()) "Wrong output vault id"),

/* Prevent replay attacks via nonce */
:ensure(is-zero(get(hash(order-hash() signed-context<0 8>()))) "Nonce already used"),
:set(hash(order-hash() signed-context<0 8>()) 1),

/* Output claim amount, a decimal float. V6 takes no order at ratio 0: the claimer
   pays 1e-30 of the ClaimInputToken per HOT, which never moves */
output-amount: signed-context<0 1>(),
io-ratio: 1e-30;

#handle-io
:ensure(equal-to(output-vault-decrease() signed-context<0 1>()) "Wrong output amount");
```

The expression is composed by `compose-rainlang.mjs` with the subparser and signer bound, and parsed by the Rainlang expression deployer into the bytecode the order carries. Only words the deployed Rainlang 0.1.9 parses are used.

**Coupon Context Fields** (each a bytes32 word):
| Index | Field | Description |
|-------|-------|-------------|
| 0 | recipient | Ethereum address to receive tokens |
| 1 | amount | The decimal float Raindex V6 pays out: the wei with exponent -18, `0xffffffee` then the wei |
| 2 | expiry | Unix timestamp when coupon expires (a float of exponent 0) |
| 3 | orderHash | Hash of the claim order (keccak256 of the abi-encoded OrderV4) |
| 4 | orderOwner | HoloLockVault address |
| 5 | orderbook | RaindexV6 address |
| 6 | outputToken | Token address |
| 7 | outputVaultId | Vault ID |
| 8 | nonce | Unique nonce (prevents replay) |

### 3. Bridge Orchestrator (`bridge-orchestrator/`)

**Purpose**: Rust service that watches Lock events on Ethereum, drives the Holochain bridge, and generates signed withdrawal coupons. Replaces the legacy `lock-watcher-rs` and `coupon-signer` crates.

```bash
cd bridge-orchestrator
cargo run -- run
```

**Responsibilities**:
- Polls Ethereum RPC for Lock events and hands them off to the Holochain side
- Scans bridging entries on Holochain and emits unified bridging RAVE transactions
- Generates signed withdrawal coupons (`CouponSigner::coupon` in `src/signer.rs`) in the URL-safe `signer,signature,ctx0..ctx8` format consumed by the UI claim page
- Tracks state required to batch coupons up to a configurable size cap

**Configuration** (via `.env`):
- `SIGNER_PRIVATE_KEY`: Key that matches `valid-signer` in Rainlang
- `ORDER_HASH`, `ORDER_OWNER`, `ORDERBOOK_ADDRESS`, `TOKEN_ADDRESS`, `VAULT_ID`, `CLAIM_INTERPRETER`, `CLAIM_STORE`, `CLAIM_BYTECODE`, `CLAIM_INPUT_TOKEN`: the claim order, which startup rebuilds, hashes and probes with a simulated `takeOrders4`
- Network/RPC settings, polling interval, coupon batch size cap (see `bridge-orchestrator/src/config.rs`)

### 4. Web UI (`ui/`)

**Purpose**: SvelteKit web interface for locking and claiming.

**Routes**:
- `/` - Connect a wallet and see its network
- `/lock` - Lock HOT to receive bridged HOT
- `/claim` - Claim HOT with coupon
- `/claim?c=<coupon>` - Direct claim via URL parameter

**Key Features**:
- Direct MetaMask integration (no WalletConnect dependency)
- Reads order status directly from blockchain via RPC
- Order configuration from the build's `PUBLIC_*` variables (no subgraph dependency)
- Transaction status modal

**Configuration**: `ui/.env.example`.

---

## Rain's contracts, and the TestNet on Base Sepolia

The release Rain deployed on Ethereum mainnet and Base Sepolia, at the same addresses (rain.orderbook 0.1.15, rainlang 0.1.9; `src/Constants.sol` takes them from the release files):

| Contract | Address |
|----------|---------|
| RaindexV6 | `0x37FC0EFec37D19f8A221aa4F8F7600C9ba2AcD20` |
| RaindexV6SubParser | `0x09Bc7AF266012F44fb41D8Bd682da931666605e1` |
| Rainlang expression deployer | `0xb90c299a8321fbdd8D0F367e560f5341F840dA3F` |
| Rainlang interpreter | `0x4c52eee7Fb6aeD0F1a30130d96d02409D9d886cf` |
| Rainlang store | `0x1Aa775533E28B1D843e1A589034984E3a62005DC` |
| Test Signer | `0x8E72b7568738da52ca3DCd9b24E178127A4E7d37` |
| Vault ID | `0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b` |

The TestNet moved from Ethereum Sepolia, which has no Raindex V6, to Base Sepolia (chain 84532). Its mock HOT, HoloLockVault, ClaimInputToken and claim order are deployed there by `deploy-base-sepolia.sh` ([DEPLOY.md](./DEPLOY.md#deploy-the-testnet-on-base-sepolia)), at the [addresses in the README](./README.md#testnet-on-base-sepolia). The old Sepolia bridge, on OrderBookV3, does not carry over.

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
    pub signature: Signature,        // coupon signer's signature (optional)
}
```

### Claim Flow <- Holochain

The bridge-orchestrator also handles coupon generation for claims:

1. **Current State**: Orchestrator generates signed coupons with the configured signer key
2. **Next Step**: Move the signer off a single key ([docs/enable-multisig.md](./docs/enable-multisig.md))
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
- **Finality**: Bridge orchestrator records a lock after 5 confirmations, on mainnet (about a minute) and on Base Sepolia (about 10 seconds, enough for a TestNet whose locks carry no value)
- **Replay Protection**: Track processed lock IDs on both sides
- **Same Signer**: Use the same trusted signer for both directions ([docs/enable-multisig.md](./docs/enable-multisig.md))

### Key Management
- **Test Key**: Included in repo for testing only
- **Coupon signer**: [docs/enable-multisig.md](./docs/enable-multisig.md)
- **Vault admin**: Protects emergency withdrawal. On mainnet it is a Safe multisig from the deploy's first transaction: [docs/enable-multisig.md](./docs/enable-multisig.md)

---

## Files Reference

```
raindex-orders/
├── src/
│   ├── HoloLockVault.sol       # Lock vault contract
│   ├── ClaimInputToken.sol     # The claim order's input token, which moves nothing
│   ├── Constants.sol           # Rain's contracts from the release files, the test signer
│   ├── test/MockHOT.sol        # Test token
│   ├── test/MockRaindex.sol    # Raindex V6 mock for the vault's unit tests
│   └── holo-claim.rain         # Rainlang claim expression
├── script/
│   ├── ClaimOrderScript.sol    # Shared claim order composition and checks
│   ├── DeployTestHOT.s.sol     # Test token deployment and minting
│   ├── DeployHoloLockVault.s.sol # Base Sepolia vault deployment
│   ├── DeployClaimOrderViaVault.s.sol # Claim order deployment
│   ├── DeployMainnet.s.sol     # Mainnet deployment
│   └── RotateClaimSigner.s.sol # Coupon signer rotation
├── test/                       # Foundry tests, the mainnet and Base Sepolia fork tests, the rehearsal
├── bridge-orchestrator/        # Rust bridge orchestrator (lock watcher + coupon signer)
│   └── src/
├── ui/                         # SvelteKit web UI
│   ├── src/routes/
│   │   ├── +page.svelte        # Home
│   │   ├── lock/+page.svelte   # Lock page
│   │   └── claim/+page.svelte  # Claim page
│   └── src/lib/
│       ├── orderConfig.ts      # The claim order, from the build variables
│       ├── coupon.ts           # Coupon parsing
│       └── ethereum.ts         # MetaMask integration
├── deploy-base-sepolia.sh      # Base Sepolia (TestNet) deployment
├── deploy-mainnet.sh           # Mainnet deployment
├── rotate-claim-signer.sh      # Coupon signer rotation
├── compose-rainlang.mjs        # Composes the claim expression for a network
├── docs/                       # Multisig and key holder guides
├── DEPLOY.md                   # Deployment guide
└── README.md                   # Project overview
```
