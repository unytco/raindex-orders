# Moving the bridge to a multisig

The bridge launches with one coupon signer key and the vault admin that `deploy-mainnet.sh` sets. Each of the two can move to a Safe multisig later. Neither move changes a contract or redeploys the vault.

- **The vault admin** can be any address. `setAdmin` hands it to a Safe.
- **The coupon signer** can be a contract. The orderbook checks a coupon's signature with OpenZeppelin's `SignatureChecker`. For a contract signer, it asks the contract's EIP-1271 `isValidSignature`, so `valid-signer` in `src/holo-claim.rain` can be a Safe with no Rainlang change. A Safe 1.4.1 answers through its `CompatibilityFallbackHandler`. It accepts the signatures of its owners over the SafeMessage hash of the coupon's digest, sorted by owner address and joined.

`test/fork-rehearsal.sh` proves both moves on a fork of mainnet: a 2 of 3 Safe acts as the vault admin, and a coupon with two owner signatures claims while a coupon with one fails.

Each key holder follows [key-holder.md](./key-holder.md).

## Move the vault admin to a Safe

1. In the Safe web app, create a Safe on Ethereum with three owners and a threshold of 2.
2. From the current admin, hand the vault over:

   ```sh
   cast send "$MAINNET_LOCK_VAULT_ADDRESS" 'setAdmin(address)' "$SAFE" --ledger --rpc-url "$ETH_RPC_URL"
   ```

3. Make sure the vault names the Safe:

   ```sh
   cast call "$MAINNET_LOCK_VAULT_ADDRESS" 'admin()(address)' --rpc-url "$ETH_RPC_URL"
   ```

From then on, every admin call is a Safe transaction that two owners approve: `adminWithdraw`, `adminRecoverTokens`, `setMinLockAmount`, `addOrder`, `removeOrder` and `setAdmin`. The orchestrator and the website keep their values.

## Move the coupon signer to a Safe

A coupon signer Safe needs a threshold of 2 to 20, which `rotate-claim-signer.sh` checks, and owners that sign with keys. The website's coupon status reads a 65-byte signature as one key's, so with a threshold of 1 every coupon reads as invalid. It reads a longer signature only when each owner's part is a key's signature.

1. Create the Safe. Its owners are the keys of the signer services, which are not built yet: see the last section.
2. Stop the orchestrator. Until step 3, any coupon it signs is for an order that is about to be removed.
3. Replace the claim order with one whose `valid-signer` is the Safe. Export the current deploy record's lines, then run:

   ```sh
   ETH_RPC_URL=... VALID_SIGNER=<the Safe> ./rotate-claim-signer.sh --ledger
   ```

   The script adds the new order and then removes the old one, through the vault admin. It reads the old order from the record's `NETWORK`, `MAINNET_LOCK_VAULT_ADDRESS`, `ORDER_HASH`, `PUBLIC_CLAIM_INTERPRETER`, `PUBLIC_CLAIM_STORE` and `PUBLIC_CLAIM_EXPRESSION`. The new record it prints is read from the chain, and the script refuses to print it unless the new order accepts coupons from `VALID_SIGNER`.

   - If the admin is a key, pass its wallet option. The script sends both calls and prints the new deploy record.
   - If the admin is a Safe, pass no wallet option. The script sends nothing and prints the two calls. Propose each from the admin Safe in that order, in the Safe web app's Transaction Builder: the vault as the address, value 0, and the printed data. When both have run, print the new deploy record with `./rotate-claim-signer.sh record <block of the first call>`. It checks only the orders the vault added from that block on, so pass the first call's own block.
4. Give the orchestrator the new record's `ORDER_HASH`, `CLAIM_SIGNER`, `CLAIM_INTERPRETER`, `CLAIM_STORE`, `CLAIM_EXPRESSION` and `CLAIM_INPUT_TOKEN`, and the new signer's key, then start it. At startup it refuses unless the order on chain accepts coupons signed with that key. Until the parts in the last section exist, it refuses a Safe as `CLAIM_SIGNER`, so a rotation to a Safe leaves the bridge paying no withdrawals.
5. Give the website the new `PUBLIC_CLAIM_ORDER_HASH`, `PUBLIC_CLAIM_SIGNER`, `PUBLIC_CLAIM_INTERPRETER`, `PUBLIC_CLAIM_STORE` and `PUBLIC_CLAIM_EXPRESSION` from the new deploy record, then build and deploy it again. The build refuses values that do not hash to the order hash, but it cannot check the signer.
6. Reissue the coupons that were not claimed. A coupon names its order's hash, so a coupon for the old order fails on the new one.

The claim order marks a nonce used under its own order hash. A withdrawal claimed on an old order is not marked on the new one, so a new coupon for it pays it a second time. Reissue a coupon only once its order is removed, and only for a withdrawal none of whose coupons is redeemed. To read whether a coupon is redeemed, ask the store of the order it names, where a nonzero answer means redeemed:

```sh
namespace=$(cast keccak "$(cast abi-encode 'f(address,address)' "$VAULT" "$ORDERBOOK")")
key=$(cast keccak "$(cast abi-encode --packed 'f(uint256,uint256)' "$COUPON_ORDER_HASH" "$NONCE")")
cast call "$COUPON_ORDER_STORE" 'get(uint256,uint256)(uint256)' "$namespace" "$key" --rpc-url "$ETH_RPC_URL"
```

`COUPON_ORDER_HASH` is the coupon's sixth field, `NONCE` its last, and `COUPON_ORDER_STORE` the `PUBLIC_CLAIM_STORE` of the deploy record that named that order. After more than one rotation, check every order a withdrawal has had a coupon for.

## Fireblocks as the coupon signer

A Fireblocks MPC wallet is a paid alternative to a Safe. It signs as one Ethereum address, so the claim order needs only that address as `valid-signer`, set with `rotate-claim-signer.sh` as above. The website needs no code change, only the new build variables, as it checks a 65-byte signature as one key's. The orchestrator needs a `CouponKey` in `bridge-orchestrator/src/signer.rs` that has Fireblocks sign the coupon's digest and returns the 65-byte signature, and its startup check compares `CLAIM_SIGNER` with that key's address.

## What is not built yet

A Safe as the coupon signer needs two parts that do not exist yet. Until both exist, the coupon signer stays one key.

- **A second signer service.** It watches the same withdrawals on the Unyt network and checks each one itself: that the withdrawal is real, its amount and recipient, and that it has no coupon yet. Then it signs the coupon's SafeMessage hash with its own owner key. Without its own check, a second signature adds no protection.
- **The orchestrator collecting two signatures.** A `CouponKey` for the Safe in `bridge-orchestrator/src/signer.rs` signs with the orchestrator's owner key and gets the second service's signature. It returns both, sorted by owner address and joined, and names the Safe as the coupon's signer. Coupon building does not change. The startup check, which now refuses a contract `CLAIM_SIGNER`, then accepts the Safe whose signer this is.
