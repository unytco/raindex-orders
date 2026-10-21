# The bridge's Safe

On mainnet one Safe is the vault admin from the bridge's first transaction and the claim order's coupon signer. Its owners are separate parties, each running a [signer service](./signer-service.md). The deploy and the orchestrator also take the two roles apart: a key or another Safe as the coupon signer. The coupon signer can also move to a [Fireblocks MPC wallet](#fireblocks-as-the-coupon-signer).

- **The vault admin** is `ADMIN_ADDRESS`, which `deploy-mainnet.sh` hands the vault to inside the one transaction that deploys it: a `HoloBridgeDeployer` deploys the claim input token and the vault with itself as admin, adds the claim order through the vault, and calls `setAdmin(ADMIN_ADDRESS)`, all in that transaction. No key is the admin at any point, and the deployer contract holds no right once the transaction ends. The deploy refuses a key, a contract that is not a Safe, or a Safe of threshold 1 as `ADMIN_ADDRESS`, so that no one key controls the vault, unless `UNSAFE_ALLOW_SINGLE_KEY_ADMIN=yes` is set, which it logs. From then on every admin call is a Safe transaction its threshold of owners approve: `adminWithdraw`, `adminRecoverTokens`, `setMinLockAmount`, `addOrder`, `removeOrder` and `setAdmin`.
- **The coupon signer** can be a contract. Raindex V6 checks a coupon's signature with OpenZeppelin's `SignatureChecker`: for a contract signer it asks the contract's EIP-1271 `isValidSignature`, so `valid-signer` in `src/holo-claim.rain` can be a Safe with no Rainlang change. A Safe 1.4.1 answers through its `CompatibilityFallbackHandler`, which accepts the signatures of its threshold of owners over the SafeMessage hash of the coupon's digest, sorted by owner address and joined. A key with an EIP-7702 delegation counts as a contract to Raindex V6, which asks its delegate and never checks the key's own signature, so such a key cannot be the coupon signer: the deploy and rotation scripts and the orchestrator refuse one. A delegated key can still be a Safe owner, as the Safe recovers each owner's signature itself.

`test/fork-rehearsal.sh` proves all of it on a fork of mainnet: the Safe's creation, the deploy's refusals, one 2 of 3 Safe as the vault admin from the deploy's block and as the coupon signer, the orchestrator collecting its owners' signatures from (mock) signer services, and the separate shape: a key signer, its move to a signer Safe of its own, the website reading its coupons, and the override. `test/base-sepolia-safe-rehearsal.sh` proves the one-Safe deploy on a fork of Base Sepolia.

## One Safe for both roles

Whoever can sign coupons can take the vault's HOT through claims, and whoever administers the vault can take it with `adminWithdraw`. The two roles carry the same risk, so one set of independent signers holds both. With several parties and no one able to act alone, none of them has custody of the HOT.

The two roles use the same owner keys, so a signature given for one must not work for the other. Safe 1.4.1 makes sure of that:

- An admin call is a Safe transaction. The owners sign its SafeTx hash: EIP-712, under the Safe's domain, of the type `SafeTx(address to,uint256 value,bytes data,uint8 operation,uint256 safeTxGas,uint256 baseGas,uint256 gasPrice,address gasToken,address refundReceiver,uint256 nonce)` ([`SAFE_TX_TYPEHASH`](https://github.com/safe-global/safe-smart-account/blob/v1.4.1/contracts/Safe.sol#L54-L57), [`encodeTransactionData`](https://github.com/safe-global/safe-smart-account/blob/v1.4.1/contracts/Safe.sol#L383-L411)). `execTransaction` checks the owner signatures over that hash and nothing else ([`execTransaction`](https://github.com/safe-global/safe-smart-account/blob/v1.4.1/contracts/Safe.sol#L139-L173), [`checkNSignatures`](https://github.com/safe-global/safe-smart-account/blob/v1.4.1/contracts/Safe.sol#L274-L334)).
- A coupon is a message. Raindex V6 asks the Safe's `isValidSignature(digest, signatures)`, which the CompatibilityFallbackHandler answers by checking the owner signatures over the SafeMessage hash: EIP-712, under the same domain, of the type `SafeMessage(bytes message)` ([`SAFE_MSG_TYPEHASH`](https://github.com/safe-global/safe-smart-account/blob/v1.4.1/contracts/handler/CompatibilityFallbackHandler.sol#L13-L14), [`isValidSignature(bytes32,bytes)`](https://github.com/safe-global/safe-smart-account/blob/v1.4.1/contracts/handler/CompatibilityFallbackHandler.sol#L77-L81), which passes the digest on to [`isValidSignature(bytes,bytes)`](https://github.com/safe-global/safe-smart-account/blob/v1.4.1/contracts/handler/CompatibilityFallbackHandler.sol#L28-L39), [`encodeMessageDataForSafe`](https://github.com/safe-global/safe-smart-account/blob/v1.4.1/contracts/handler/CompatibilityFallbackHandler.sol#L56-L59)).

The two hashes have different type hashes (`0xbb8310d4…` and `0x60b3cbf8…`), so their struct hashes differ, and an ECDSA signature over one recovers a different address over the other. A coupon's owner signatures fed to `execTransaction` fail with `GS026`, and an admin call's owner signatures fed to a claim fail with `InvalidSignature`: `test/TestOneSafeAdminAndSigner.t.sol` and the rehearsal's `OneSafeRehearsal` prove both on a fork of mainnet. Making one pass as the other would mean finding a keccak256 collision.

This holds as long as each owner signs only hashes it computed itself: a coupon's SafeMessage hash from words it checked, or an admin transaction it reviewed in the Safe app. An owner key that signs any hash it is sent would sign an admin transaction too. The signer service must never do that ([signer-service.md](./signer-service.md)).

Two more ways the Safe vouches for a coupon or an admin call, both needing the owners themselves: an owner's `approveHash` of one exact hash, and a Safe transaction through `SignMessageLib` that marks a message as signed. Each takes a deliberate on-chain act of the owners, so neither lets one role stand in for the other.

Each key holder follows [key-holder.md](./key-holder.md).

## Create the Safe

```sh
ETH_RPC_URL=... SAFE_OWNERS=0x...,0x...,0x... SAFE_THRESHOLD=2 ./deploy-mainnet.sh safe --ledger
```

This creates the Safe through Safe's own SafeProxyFactory 1.4.1, with the Safe 1.4.1 singleton (SafeL2 on Base Sepolia, through `deploy-base-sepolia.sh safe`), the CompatibilityFallbackHandler, and no module or guard. The address follows from the owners, the threshold and `SAFE_SALT_NONCE`: it is printed before anything is sent, and a second run finds the Safe and sends nothing. The address commits to those inputs, so no one can create a different Safe there first. It prints the `ADMIN_ADDRESS`, `ADMIN_SAFE_THRESHOLD`, `ADMIN_SAFE_OWNERS` and `VALID_SIGNER` the deploy takes.

The Safe needs a threshold of 2 to 20 and more than half its owners, and owners that sign with keys: each owner is one signer service's key. More than half, so that every two sets of signers share an owner, which refuses a withdrawal it signed for another claim order: 2 of 3 or 3 of 5, not 2 of 4 ([signer-service.md](./signer-service.md#one-order-per-withdrawal)). The website's coupon status reads a 65-byte signature as one key's, so with a threshold of 1 every coupon reads as invalid, and it reads a longer signature only when each owner's part is a key's signature. A Safe made in the Safe web app works too, if it is the same code as one this creates: Safe's SafeProxy 1.4.1 on the Safe 1.4.1 or SafeL2 1.4.1 singleton, with the CompatibilityFallbackHandler 1.4.1, and no module or guard. The deploy, the rotation and `deploy-mainnet.sh safe` check each of these on chain, by code hash and address, and refuse a Safe that fails one, for both roles. A module executes the Safe's transactions with no owner's signature, so as the vault admin it could take the vault's HOT and as the coupon signer mark any coupon signed. A guard can refuse what the owners sign, and another handler answers the coupons' EIP-1271 question as it likes. A Safe of another release, such as 1.3.0 or 1.5.0, is refused too: create the Safe with `deploy-mainnet.sh safe`.

## Launch with the Safe

```sh
ETH_RPC_URL=... \
ADMIN_ADDRESS=<the Safe> ADMIN_SAFE_THRESHOLD=2 ADMIN_SAFE_OWNERS=0x...,0x...,0x... \
VALID_SIGNER=<the Safe> \
./deploy-mainnet.sh --ledger
```

The deploy prints the Safe's threshold and owners as it read them, sends its one transaction, then prints the deploy record, which refuses a vault whose admin changed other than once, from the `HoloBridgeDeployer` to `ADMIN_ADDRESS`. [DEPLOY.md](../DEPLOY.md#mainnet-deploy) has the whole flow.

To hand the vault to one key regardless, set `UNSAFE_ALLOW_SINGLE_KEY_ADMIN=yes`. The deploy then takes a key, or a Safe of threshold 1, as `ADMIN_ADDRESS`, and logs that one key controls the vault. `setAdmin` from that key moves the vault to a Safe later:

```sh
cast send "$MAINNET_LOCK_VAULT_ADDRESS" 'setAdmin(address)' "$SAFE" --ledger --rpc-url "$ETH_RPC_URL"
cast call "$MAINNET_LOCK_VAULT_ADDRESS" 'admin()(address)' --rpc-url "$ETH_RPC_URL"
```

## The coupon signer: the Safe, or a key

`VALID_SIGNER` at the deploy, or at a rotation, is either:

- **A Safe**, the admin Safe at launch. The orchestrator runs with `SAFE_OWNER_SERVICES`, each owner's signer service as `<owner>=<base URL>`, separated by commas, and may hold some owner keys itself in `SAFE_OWNER_KEYS`; not `SIGNER_PRIVATE_KEY`. For each coupon it reads the Safe's threshold and owners, asks every owner at once, checks each answer, takes M owner signatures, and has the Safe's own `isValidSignature` accept them before the coupon goes out; with fewer than M the withdrawal stays parked until the next cycle ([signer-service.md](./signer-service.md#what-the-orchestrator-does)). At startup it reads the Safe's threshold, owners and EIP-712 domain, asks each service who it signs for, refuses a threshold of half the owners or fewer on every network, a key that is not an owner's, more keys than the threshold, or a key twice, and simulates a claim with M owner signatures, which must pass, and M-1, which must fail. **Owner keys in the orchestrator are one party, however many**: holding M of them adds no protection. When the Safe is also the vault admin, each of those keys approves admin transactions too, so a compromise of the orchestrator's host would reach `adminWithdraw`. On mainnet the orchestrator therefore refuses to start with M owner keys, with any owner key of a Safe that is also the vault admin, or with a Safe of threshold 1, unless `UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes` is set, which it logs. Off mainnet it logs them and starts. A threshold of half the owners or fewer it refuses on every network, at startup and for each coupon, and that override does not change this. The protection comes from owners at independent signer services, each checking the withdrawal on its own Holochain node.
- **A key.** The orchestrator runs with `SIGNER_PRIVATE_KEY`, that key. At startup it refuses unless the claim order on chain accepts coupons signed with it.

The owners of the Safe change through an ordinary Safe owner change (add, remove or swap an owner, change the threshold), which the current owners sign in the Safe app. The change must leave the threshold more than half the owners, and before the owners sign it, the services of the owners after it take in the records of the services of the owners before it ([signer-service.md](./signer-service.md#one-order-per-withdrawal)). The claim order, the website and the deploy record do not change. Each coupon is checked at claim time, so a coupon whose signatures came from a removed owner must be signed again, and the orchestrator reads the owners for each coupon it signs.

## Move the coupon signer to another signer

For a bridge launched with a key, or a Safe that is not the admin, as the coupon signer. A bridge whose one Safe holds both roles needs none of this: its signers change through the Safe.

1. Create the signer Safe, as above.
2. Stop the orchestrator. Until step 3, any coupon it signs is for an order that is about to be removed.
3. Replace the claim order with one whose `valid-signer` is the Safe. Export the current deploy record's lines, then run:

   ```sh
   ETH_RPC_URL=... VALID_SIGNER=<the Safe> ./rotate-claim-signer.sh
   ```

   The script adds the new order and then removes the old one, through the vault admin. It reads the old order from the record's `NETWORK`, `MAINNET_LOCK_VAULT_ADDRESS`, `ORDER_HASH`, `PUBLIC_CLAIM_BYTECODE` and `PUBLIC_CLAIM_INPUT_TOKEN`. The new record it prints is read from the chain, and the script refuses to print it unless the new order accepts coupons from `VALID_SIGNER`.

   - If the admin is a Safe, pass no wallet option. The script sends nothing and prints the two calls. Propose each from the admin Safe in that order, in the Safe web app's Transaction Builder: the vault as the address, value 0, and the printed data. When both have run, print the new deploy record with `./rotate-claim-signer.sh record <block of the first call>`. It checks only the orders the vault added from that block on, so pass the first call's own block.
   - If the admin is a key, pass its wallet option, `--ledger` or `--account <keystore>`. The script sends both calls and prints the new deploy record.
4. Give the orchestrator the new record's `ORDER_HASH`, `CLAIM_SIGNER`, `CLAIM_INTERPRETER`, `CLAIM_STORE`, `CLAIM_BYTECODE` and `CLAIM_INPUT_TOKEN`, and `SAFE_OWNER_SERVICES` (or `SAFE_OWNER_KEYS`) in place of `SIGNER_PRIVATE_KEY`, then start it.
5. Give the website the new `PUBLIC_CLAIM_ORDER_HASH`, `PUBLIC_CLAIM_SIGNER`, `PUBLIC_CLAIM_INTERPRETER`, `PUBLIC_CLAIM_STORE`, `PUBLIC_CLAIM_BYTECODE` and `PUBLIC_CLAIM_INPUT_TOKEN` from the new deploy record, then build and deploy it again. The build refuses values that do not hash to the order hash, but it cannot check the signer. Its coupon status asks the Safe's `isValidSignature` for each coupon that names it, in the same call that reads the coupon's nonce.
6. Reissue the coupons that were not claimed. A coupon names its order's hash, so a coupon for the old order fails on the new one.

The claim order marks a nonce used at the nonce alone, under the store namespace Raindex keeps for the order's owner, the vault. The new order reads the same namespace and the same store, so a withdrawal claimed on the old order is used on the new one: a second coupon for it fails with `Nonce already used`, whatever order hash it names. Reissue a coupon only once its order is removed; a reissued coupon for a withdrawal that was claimed is refused on chain. With a Safe coupon signer, each signer service also records the order hash it signed each withdrawal for, and refuses that withdrawal for any other order hash with `already_signed` until its operator has made both checks below and cleared the record ([signer-service.md](./signer-service.md#one-order-per-withdrawal)): a second line behind the chain's. To read whether a coupon is redeemed, ask the store, where a nonzero answer means redeemed:

```sh
namespace=$(cast keccak "$(cast abi-encode 'f(address,address)' "$VAULT" "$ORDERBOOK")")
cast call "$CLAIM_STORE" 'get(uint256,bytes32)(bytes32)' "$namespace" "$NONCE" --rpc-url "$ETH_RPC_URL"
```

`NONCE` is the coupon's last field, and `CLAIM_STORE` the deploy record's `PUBLIC_CLAIM_STORE`, which a rotation keeps. A claim order run by another store would start its nonces afresh: a rotation must keep `CLAIM_STORE`. A claim order deployed before the nonce key changed (CHANGELOG, Unreleased) marked a nonce at `hash(order-hash() nonce)` instead, a key the new order does not read, so a rotation of such an order carries none of its used nonces over: only a TestNet deployed before that runs one, and the website's coupon status reads both keys for it. Mainnet launches on the new order. Before rotating such a TestNet order, reissue only its unclaimed coupons (step 6), never a withdrawal it paid; or deploy the TestNet afresh with `deploy-base-sepolia.sh all-safe` ([DEPLOY.md](../DEPLOY.md#deploy-the-testnet-on-base-sepolia)). To read whether the old order is removed, `cast call "$ORDERBOOK" 'orderExists(bytes32)(bool)' "$COUPON_ORDER_HASH" --rpc-url "$ETH_RPC_URL"` must answer `false`, `COUPON_ORDER_HASH` being the coupon's sixth field.

## Fireblocks as the coupon signer

A Fireblocks MPC wallet is a paid alternative to a Safe. It signs as one Ethereum address, so the claim order needs only that address as `valid-signer`, set at the deploy or with `rotate-claim-signer.sh` as above. The website needs no code change, only the new build variables, as it checks a 65-byte signature as one key's. The orchestrator needs a `CouponKey` in `bridge-orchestrator/src/signer.rs` that has Fireblocks sign the coupon's digest and returns the 65-byte signature, and its startup check compares `CLAIM_SIGNER` with that key's address.

## What is not built yet

**The signer services**, one per owner, which Unyt builds. Each receives a signing request from the orchestrator and reads the withdrawal from its own Holochain node. It checks that the burn is still parked, its amount and recipient, the coupon's expiry and nonce, the claim order, and that it has not signed the withdrawal for another order. It signs the coupon's SafeMessage hash with its own owner key only when all of it holds. [signer-service.md](./signer-service.md) is the API: requests, answers, refusal codes, the hash with a worked example, and the open questions.

The orchestrator's side is built: it collects the owners' signatures over that API, checks them, and has the Safe accept them. Until the services exist, an orchestrator holding M owner keys is one party, as a single key is, and on mainnet it starts so only with `UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes`.
