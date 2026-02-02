# Holo Bridge - Sepolia Deployment Guide

## Prerequisites

1. MetaMask wallet with Sepolia ETH
2. Foundry installed (`forge`, `cast` commands available)
3. Nix installed (for running tests/rain CLI)

## Quick Start

### 1. Set Up Environment

```bash
# Copy example env file
cp .env.example .env

# Edit .env and add your private key
# Export from MetaMask: Account Details -> Export Private Key
nano .env  # or your preferred editor
```

Your `.env` should have:
```
PRIVATE_KEY=0x<your-private-key-here>
SEPOLIA_RPC_URL=https://1rpc.io/sepolia
```

### 2. Deploy Contracts

```bash
# Check your wallet and balance
./deploy-sepolia.sh status

# Step 1: Deploy MockHOT token (or skip if using existing TROT)
./deploy-sepolia.sh token

# Update .env with the TOKEN_ADDRESS from output
# Then run step 2:

# Step 2: Deploy HoloLockVault
./deploy-sepolia.sh vault

# Update .env with LOCK_VAULT_ADDRESS from output

# Step 3: Mint test tokens to your wallet
./deploy-sepolia.sh mint
```

### 3. Deploy Claim Order (via Web Interface)

Since the Raindex order requires the rain CLI which has complex setup, use the web interface:

1. Go to https://app.rainlang.xyz
2. Connect your wallet to Sepolia
3. Click "New Order"
4. Paste the Rainlang code (everything after `---` in `src/holo-claim.rain`):

```rainlang
#orderbook-subparser 0xe6A589716d5a72276C08E0e08bc941a28005e55A
#valid-signer 0x8E72b7568738da52ca3DCd9b24E178127A4E7d37

#calculate-io
using-words-from orderbook-subparser

/* do the checks */
:ensure(equal-to(signer<0>() valid-signer) "Wrong signer"),
:ensure(equal-to(signed-context<0 0>() order-counterparty()) "Wrong recipient"),
:ensure(less-than(block-timestamp() signed-context<0 2>()) "Order expired"),
:ensure(equal-to(signed-context<0 3>() order-hash()) "Wrong order hash"),
:ensure(equal-to(signed-context<0 4>() order-owner()) "Wrong order owner"),
:ensure(equal-to(signed-context<0 5>() orderbook()) "Wrong orderbook"),
:ensure(equal-to(signed-context<0 6>() output-token()) "Wrong output token"),
:ensure(equal-to(signed-context<0 7>() output-vault-id()) "Wrong output vault id"),

/* check the nonce has not been used before */
:ensure(is-zero(get(hash(order-hash() signed-context<0 8>()))) "Nonce already used"),
:set(hash(order-hash() signed-context<0 8>()) 1),

output-amount: signed-context<0 1>(),
io-ratio: 0;

#handle-io
:ensure(equal-to(output-vault-balance-decrease() signed-context<0 1>()) "Wrong output amount");
```

5. Configure the order:
   - **Input token**: NOOP (0x555FA2F68dD9B7dB6c8cA1F03bFc317ce61e9028) or any placeholder
   - **Input vault ID**: `0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b`
   - **Output token**: Your TOKEN_ADDRESS (or TROT: 0x72bBeF0c3d23C196D324cF7cF59C083760fFae5b)
   - **Output vault ID**: `0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b`

6. Deploy the order and note the **Order Hash** from the transaction

7. **Fund the order vault**: Deposit tokens into the orderbook vault
   - Go to the Orderbook contract on Etherscan
   - Call `deposit(token, vaultId, amount)` with your tokens

8. Update `.env`:
```
ORDER_HASH=0x<order-hash-from-deployment>
ORDER_OWNER=<your-wallet-address>
```

## Using Existing Infrastructure

If you want to skip deploying new contracts, you can use existing Sepolia contracts:

```
TOKEN_ADDRESS=0x72bBeF0c3d23C196D324cF7cF59C083760fFae5b  # TROT
ORDERBOOK_ADDRESS=0xfca89cD12Ba1346b1ac570ed988AB43b812733fe
```

## Testing the Flow

### Lock Flow (HOT → HoloFuel)

1. Start the lock watcher:
```bash
cd lock-watcher-rs
cp .env.example .env
# Edit .env with your SEPOLIA_LOCK_VAULT_ADDRESS
cargo run
```

2. Run the UI:
```bash
cd ui
cp .env.example .env
# Edit .env with PUBLIC_LOCK_VAULT_ADDRESS and PUBLIC_TOKEN_ADDRESS
npm install
npm run dev
```

3. Open http://localhost:5173, connect wallet, and lock tokens

### Claim Flow (HoloFuel → HOT)

1. Generate a coupon:
```bash
cd coupon-signer
cp .env.example .env  # Uses test signer key

cargo run -- \
  --amount "10" \
  --recipient "0x<claimer-address>" \
  --order-hash "$ORDER_HASH" \
  --order-owner "$ORDER_OWNER" \
  --orderbook "0xfca89cD12Ba1346b1ac570ed988AB43b812733fe" \
  --token "$TOKEN_ADDRESS" \
  --vault-id "0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b"
```

2. Use the coupon in the claim UI or directly call `takeOrders` on the orderbook

## Contract Addresses Summary

| Contract | Sepolia Address |
|----------|-----------------|
| Orderbook | 0xfca89cD12Ba1346b1ac570ed988AB43b812733fe |
| Expression Deployer | 0xd19581a021f4704ad4eBfF68258e7A0a9DB1CD77 |
| Orderbook Subparser | 0xe6A589716d5a72276C08E0e08bc941a28005e55A |
| TROT (test token) | 0x72bBeF0c3d23C196D324cF7cF59C083760fFae5b |
| NOOP (placeholder) | 0x555FA2F68dD9B7dB6c8cA1F03bFc317ce61e9028 |

## Security Notes

- Never commit `.env` files with private keys
- The test signer key in this repo is for testing only
- In production, use a secure key management solution (e.g., Fireblocks)
