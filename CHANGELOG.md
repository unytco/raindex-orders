# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `npm run make-coupon` signs for a Safe coupon signer: the Safe's threshold of owners sign, with keys from `--owner-keys-file` or `SAFE_OWNER_KEYS` (never the command line) or at signer services in `SAFE_OWNER_SERVICES`, each signature is checked to be its owner's, they are sorted as the Safe takes them, and the Safe's `isValidSignature` accepts them before it prints the claim link. It reads the website's env file or a deploy record (`.env` after `deploy-base-sepolia.sh all-safe`), and refuses mainnet as before. `test/base-sepolia-safe-rehearsal.sh` claims its 2-of-3 coupons on a fork. DEPLOY.md, "Try M-of-N on Base Sepolia by hand", walks through a second TestNet with a 2-of-3 Safe on Base Sepolia: owner keys, the deploy, three mock signer services, the orchestrator's startup checks with services down, a lock and a claim on the website on your machine, and the Safe in the Safe app.
- `HoloBridgeDeployer` deploys the mainnet bridge in one transaction and hands the vault to `ADMIN_ADDRESS` inside it, so a Safe is the vault admin from the vault's first block and no key ever holds it. `deploy-mainnet.sh` refuses a key, a contract that is not a Safe, or a Safe of threshold 1 as the admin unless `UNSAFE_ALLOW_SINGLE_KEY_ADMIN=yes` is set, takes the Safe's expected `ADMIN_SAFE_THRESHOLD` and `ADMIN_SAFE_OWNERS` and refuses a Safe that answers otherwise, and its record refuses a vault whose admin changed other than once, from the deployer contract to `ADMIN_ADDRESS`.
- one Safe can be the vault admin and the coupon signer, `VALID_SIGNER` = `ADMIN_ADDRESS`, which is the launch shape: the deploy logs it, and refuses a Safe coupon signer whose fallback handler does not hash coupons as Safe's CompatibilityFallbackHandler does. The deploy, the rotation and `deploy-mainnet.sh safe` refuse a Safe admin or coupon signer that is not Safe's SafeProxy 1.4.1 on the Safe 1.4.1 or SafeL2 1.4.1 singleton with the CompatibilityFallbackHandler 1.4.1, checked by code hash and address, or that has a module or a guard: a module acts as the Safe with no owner's signature. A coupon's owner signatures (SafeMessage) cannot execute an admin call (SafeTx), nor the reverse (`docs/enable-multisig.md`, One Safe for both roles).
- `deploy-mainnet.sh safe` and `deploy-base-sepolia.sh safe` create the bridge's Safe from `SAFE_OWNERS` and `SAFE_THRESHOLD` through Safe's own factory 1.4.1 (SafeL2 off mainnet), at an address that follows from its inputs and `SAFE_SALT_NONCE`, or find it unchanged and send nothing. `deploy-base-sepolia.sh bridge` and `all-safe` deploy a fresh TestNet in one `HoloBridgeDeployer` transaction with that Safe as admin and coupon signer.
- bridge-orchestrator signs coupons for a Safe `CLAIM_SIGNER` with its owners: at their signer services, `SAFE_OWNER_SERVICES`, and with keys it holds, `SAFE_OWNER_KEYS`. For each coupon it reads the Safe's threshold and owners, asks every owner at once (20 seconds each; an owner that timed out is not asked again that cycle while the others are at least M; an RPC that did not answer a read of the Safe within 30 seconds is not read again that cycle), checks each signature, takes M sorted by owner, and has the Safe's `isValidSignature` accept them; with fewer than M the withdrawal stays parked, logged once with each owner's reason, until the next cycle. At startup it checks the Safe's threshold, owners and EIP-712 domain, asks each service `GET /signer`, and simulates a claim with M owner signatures, which must pass, and M-1, which must fail. Owner keys held by one orchestrator are one party, not M-of-N: on mainnet it refuses to start with M of them, with any owner key of a Safe that is also the vault admin, or with a Safe of threshold 1, unless `UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes`.
- `docs/signer-service.md`: the API a Safe owner's signer service implements (Unyt builds them): requests, answers, refusal codes, the hash an owner signs with a worked example and reference code (`ui/scripts/safe-message.mjs`), and what the service checks on Holochain before it signs. Among those checks: the withdrawal is still parked when it signs; it keeps a durable record of the order hash it signed each withdrawal for and refuses another (`already_signed`), so a claim order rotation cannot pay one withdrawal twice; the expiry window is `EXPIRY_SECONDS` plus a 10-minute clock tolerance; and the HOT amount converts to wei exactly, with test vectors. The mock service (`ui/scripts/mock-signer-service.mjs`) keeps the record and the expiry window too.
- `test/fork-rehearsal.sh` launches with one Safe, created by `deploy-mainnet.sh safe`, as admin and coupon signer, with the orchestrator collecting its owners' signatures from mock signer services (`ui/scripts/mock-signer-service.mjs`), then proves the separate shape: an admin Safe, a key signer and its rotation to a signer Safe, the website's coupon status against a real Safe on the fork before and after a claim, and the override deploy. `test/base-sepolia-safe-rehearsal.sh` proves the one-Safe TestNet deploy on a fork of Base Sepolia.

### Changed

- the claim expression (`src/holo-claim.rain`) marks a used nonce at the nonce alone, no longer at `hash(order-hash() nonce)`. Raindex keeps the store under the order's owner, the vault, so a nonce claimed on one claim order is used on every claim order the vault adds after a rotation: one burn pays once, on chain, whatever the signer becomes. The signer services' `already_signed` records and the threshold rule stand behind that. The website's coupon status reads a nonce at both keys, so a coupon redeemed on either expression of its order reads as redeemed. A TestNet deployed before this runs the old expression until its claim order is rotated (`rotate-claim-signer.sh`), which also changes its deploy record; the rotation carries none of the old order's used nonces over, as its key named the old order's hash (`docs/enable-multisig.md`).

## [0.7.1] - 2026-10-18

### Fixed

- bridge-orchestrator leaves a withdrawal parked with no coupon when it holds a unit besides HOT, names another vault, or names a recipient that fails its EIP-55 checksum, and pays the rest of the batch.
- bridge-orchestrator lowers the bridging agent's credit limit by each withdrawal it pays, and `in-transit` lists each one it is still taking off.

## [0.7.0] - 2026-10-15

### Added

- the Lock page turns Lock off for more than $1,000 worth of HOT, and while it cannot read the HOT price.

### Changed

- bridge-orchestrator credits deposits through a credit limit adjustment that requires a `call_method`.
- bridge-orchestrator runs against a Holochain 0.7.1 conductor and a unyt DNA built on rave_engine 0.14.0.

## [0.6.0] - 2026-10-15

### Added

- bridge-orchestrator holds each lock and withdrawal whose address is on `SCREENING_DENYLIST` until a person runs `release` or `block`, and `screenings --open` lists what waits.
- on mainnet, bridge-orchestrator refuses to start without `SCREENING=on` and at least one `SCREENING_DENYLIST` address.

## [0.5.0] - 2026-10-15

### Added

- the TestNet is deployed on Base Sepolia (README.md, TestNet on Base Sepolia), and a TestNet website build takes its mock HOT, vault, ClaimInputToken, claim order hash and bytecode as defaults.
- `ClaimInputToken`, the claim order's input token, deployed with the vault: Raindex V6 takes an order only at a positive io-ratio and pulls at least one unit of the input token from the taker, so the claim order's ratio is 1e-30 per HOT and this token's transfers move nothing.
- `test/TestClaimBaseSepolia.t.sol` deploys the TestNet bridge on a fork of Base Sepolia and proves a lock and a claim against the Raindex V6 there.
- a coupon test vector (`test/TestCouponSignature.t.sol`): fixed inputs, their nine words, the signature, the order hash and the claim's `takeOrders4` calldata, reproduced by the orchestrator's and the website's tests.

### Changed

- the bridge runs on Raindex V6 and Rainlang 0.1.9, the release Rain deployed on Ethereum mainnet and Base Sepolia at the same deterministic addresses (RaindexV6 `0x37FC0EFec37D19f8A221aa4F8F7600C9ba2AcD20`), in place of OrderBookV3. `src/Constants.sol` takes each address and code hash from the release's frozen file, and every script checks the code on chain before it sends anything.
- HoloLockVault deposits and withdraws through `deposit4` and `withdraw4` with a bytes32 vault ID, converting every amount losslessly to Raindex's decimal float and refusing an amount no float carries. The `Lock` event is unchanged. It is a new deployment: the mainnet vault is not deployed yet, and the TestNet's is redeployed.
- the claim expression compares addresses and hashes bit for bit (`binary-equal-to`), reads `raindex()` and `output-vault-decrease()`, and pays a decimal float. The coupon's amount word is that float, the wei with exponent -18 (`0xffffffee` then the wei); the other eight words are as before.
- a claim is `takeOrders4` for exactly the coupon's amount at the order's ratio, and the orchestrator's startup probe simulates it.
- the deploy and rotation scripts and the orchestrator refuse a coupon signer with an EIP-7702 delegation: Raindex V6 (OpenZeppelin 5's `SignatureChecker`) asks any account with code, a delegation included, through EIP-1271 and never by the key's signature, where OrderBookV3 recovered the key first. A delegated key still sends the vault admin's transactions.
- the deploy record prints `CLAIM_BYTECODE` and `CLAIM_INPUT_TOKEN` in place of `CLAIM_EXPRESSION`: a V6 order carries its parsed bytecode. The website takes `PUBLIC_CLAIM_BYTECODE` and `PUBLIC_CLAIM_INPUT_TOKEN`, and `rotate-claim-signer.sh` reads the current order from them.
- the TestNet moves from Ethereum Sepolia, which has no Raindex V6, to Base Sepolia (chain 84532): `NETWORK` and `PUBLIC_NETWORK` are `base-sepolia` or `mainnet`, `deploy-base-sepolia.sh` replaces `deploy-sepolia.sh`, the RPC secret is `BASE_SEPOLIA_RPC_URL`, and the website names Base Sepolia, Basescan and Base's faucets. The website builds the deployed Base Sepolia TestNet's mock HOT, vault and claim order in as defaults. The orchestrator defaults only what is the same on every chain, so it must be given them; the old Sepolia bridge does not carry over. A lock is still recorded after 5 confirmations, 10 seconds on Base.
- the Solidity side builds on the Soldeer packages rain.orderbook pins with solc 0.8.25, in place of the git submodules and 0.8.19. `forge soldeer install` fetches them.
- `test/fork-rehearsal.sh` re-targets V6, forks mainnet through `https://ethereum-rpc.publicnode.com` unless `FORK_URL` is set, gives the orchestrator a durable conductor config (the startup check added since 0.3.0 refused its missing one), and makes the rotation record refuse an order run by another interpreter in place of one from another expression deployer.
- a paused bridge website shows "We'll be back soon" on every page, and a Claim page left open sends nothing.

## [0.4.0] - 2026-10-12

### Added

- `bridge-orchestrator in-transit` lists what a migration's close would strand, and `--mark-failed` marks those deposit rows failed for payment by hand.

### Fixed

- bridge-orchestrator reconnects after its lair keystore restarts.
- bridge-orchestrator credits both deposits when one Ethereum transaction locks twice, for deposits it records from this release on.
- bridge-orchestrator marks a deposit bridged only once a RAVE has consumed its link: a deposit whose link the template refused waits, and one whose link it redacted is failed for a person.
- bridge-orchestrator counts a deposit proof only on a link its bridging agent signed, so a copy in another agent's spend neither marks a deposit bridged nor holds deposits back.
- bridge-orchestrator signs a withdrawal coupon only for a spend in the withdrawer role.
- bridge-orchestrator pays a deposit link only when the rows of all its deposits record it, so a write that lands late, or one whose row was deleted, pays no deposit twice.
- bridge-orchestrator stops on SIGTERM or SIGINT once the request in flight returns, and gives up on an Ethereum request after 30 s.
- bridge-orchestrator counts no attempt against a deposit when it stops or restarts, so a stop never fails one.
- bridge-orchestrator logs a failed Ethereum request of its lock read with its cause, and without the RPC URL or its key.
- `bridge-orchestrator status` and `clear` leave a running bridge's in-progress rows as they are.
- bridge-orchestrator records a deposit whose write landed late, even on its last attempt.
- bridge-orchestrator refuses to start unless its conductor config sets `db_sync_level: Full`.
- bridge-orchestrator fails a deposit for a person when its conductor no longer holds the deposit's link or spend, instead of marking it bridged.
- bridge-orchestrator fails for manual resolution a deposit an older release recorded on a link it cannot prove its own.
- bridge-orchestrator keys each lock by its vault and lock ID, and refuses a database that serves another vault.
- bridge-orchestrator records a lock once its block has 5 confirmations, on mainnet as on Sepolia.
- bridge-orchestrator logs each withdrawal or other agent's link it leaves parked once while it stays parked.
- the claim page says why a claim failed and what to do next, and sends no claim the bridge would refuse.
- a claim or lock sped up, cancelled or replaced in the wallet ends with its real result, a transaction not confirmed within five minutes says it is still pending, and the transaction window can always be closed.
- the Lock page says plainly when you decline a transaction in your wallet, or your wallet does not send it.
- the Lock page says when the amount is more than your balance, before it asks your wallet.

## [0.3.0] - 2026-10-06

### Added

- bridge-orchestrator runs with Ethereum off under `NETWORK=none`: it bridges parked deposits on Holochain, leaves withdrawals parked, and calls no Ethereum RPC.
- the MainNet bridge website has no faucet: its faucet page and API answer 404, and no faucet link shows.
- `POST /api/coupon-status` accepts a coupon signed by a Safe as the claim signer.
- `deploy-mainnet.sh` deploys the bridge on Ethereum mainnet from a Ledger or an encrypted keystore, and prints the deploy record.
- `rotate-claim-signer.sh` moves the claim order to a new coupon signer, such as a Safe.
- the bridge UI answers `POST /api/coupon-status` with each claim coupon's status on its network (`redeemed`, `unredeemed`, `expired` or `invalid`), up to 20 coupons and 64 KB per request, open to any origin. Coupons are read at the `safe` block, in at most one RPC call per request. `redeemed` and `expired` answers are kept in the Worker's cache, and `unredeemed` ones for 60 s; `block` is `null` when a request needed no read.

### Changed

- the bridge website builds for TestNet or MainNet from `PUBLIC_NETWORK`, naming its network, token and explorer to match. A TestNet build needs no variables.
- Lock and Claim show the network switch, and send nothing, while the wallet is on another chain.
- `deploy-sepolia.sh` signs with `--account` or `--ledger`, never a private key, and keeps the vault in `.env` as `SEPOLIA_LOCK_VAULT_ADDRESS`.
- bridge-orchestrator runs on the network `NETWORK` names, TestNet by default, and refuses to start when its chain, vault, claim order or signing key do not match.
- bridge-orchestrator bridges on the one lane that names its agent and lists the unit in `HOT_UNIT_INDEX`, and will not start while `HOLOCHAIN_LANE_DEFINITION` or `HOLOCHAIN_UNIT_INDEX` is set.
- upgrade bridge-orchestrator Holochain deps to 0.7 (rave_engine 0.13.0, holochain_client 0.9.0, zfuel 0.9.1, holo_hash / holochain_zome_types 0.7.0), with zfuel and rave_engine pinned to exact crates.io versions.

### Fixed

- the Lock and Claim pages show an error, not a success screen, for a transaction that reverted.
- the claim page recognises a coupon whose order hash starts with a zero, and links a recipient address that starts with one.
- the bridge UI's faucet answers a failed RPC call with a fixed error instead of the RPC error text, which held the full `SEPOLIA_RPC_URL` and any API key in it.
- bridge-orchestrator gives each withdrawal coupon a nonce derived from its withdrawal's transaction ID, so coupons signed in the same second can all be claimed, and a withdrawal signed again cannot be claimed twice.
- bridge-orchestrator waits for the agreement a deposit was parked on to consume it, even after its lane names a new one.
- bridge-orchestrator keeps bridging after its lane's definition is extended or replaced.
- bridge-orchestrator fails a cycle with an error when no lane, or more than one, names its agent and lists the HOT unit.
- `deploy-sepolia.sh all` mints `MINT_AMOUNT`.
- the deploy and rotation scripts send from a vault admin key that has an EIP-7702 delegation, such as a MetaMask smart account.
- a withdrawal coupon pays the withdrawal's amount in the unit `HOT_UNIT_INDEX` names, not always unit 1. A withdrawal no coupon can pay stays parked, and an error names it.
- bridge-orchestrator keeps bridging when another lane sets no credit limit adjustment, and fails a cycle, naming its lane, when its own sets none.

## [0.2.0] - 2026-09-30

### Added

- bridge-orchestrator signs zome calls via lair (`CONDUCTOR_CONFIG` + `LAIR_PASSPHRASE_FILE`, defaulting to the fleet paths), committing no capability grant per connect. A node that cannot offer lair stops the orchestrator at startup with the reason instead of writing to the bridging agent's chain.

### Changed

- the lair requirement is `ham`'s decision, supplied with the orchestrator's two paths, rather than restated here. A refusal names the fault before the reason the node could not offer lair.
- bridge-orchestrator pins `ham` to an exact revision (`4e10636`) rather than its `main` branch, so a change to it reaches the orchestrator only in a commit that names the new revision.
- bridge-orchestrator sums a batch's amounts with rave_engine's `UnitMap::sum_vec` rather than its own copy of the same fold.
- `MAX_LINK_TAG_BYTES` is clamped to 600..=900, so no configuration can consume the last 100 bytes under Holochain's own 1000-byte link-tag limit, or set a cap too small to write anything.
- `MAX_LINK_TAG_BYTES` defaults to 850, the room a single deposit needs now that the parked-spend tag also states what the spend was charged.

### Fixed

- bridge-orchestrator sends `execute_rave` the transaction fields the alliance DNA reads, so a bridge cycle runs past stage 2 instead of failing every link on a node running the fee-charging DNA.
- bridge-orchestrator decodes a network that states fees per unit, and measures a deposit batch against everything the zome writes into the parked-spend tag: the agent's whole ledger, and the lane definitions the zome resolves for a spend that names none. A batch it packs under the cap is not then refused by Holochain.
- bridge-orchestrator abandons an oversize deposit only when its own payload could not be written at any cap: a batch held back by the cap, by the agent's ledger or by the network's own definitions waits for the next cycle instead of failing every row in it permanently.
- bridge-orchestrator logs a failed cycle's whole error chain rather than its outermost line, so a wrapped conductor or socket failure still names its cause in the logs, in watchtower and on the row it reset.

## [0.1.0] - 2026-08-18

### Added

- bridge-orchestrator reports its unclassified-error streak (`unclassified_active` / `unclassified_consecutive`) to watchtower alongside the source-chain-pressure pair, so a persistent unknown failure is visible to watchtower instead of only in log events.
- CI runs the bridge-orchestrator Rust suite (`.github/workflows/rust.yml`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`) on the crate's pinned toolchain.

### Changed

- the Rainix/Solidity workflow (`.github/workflows/test.yml`) is now manual-only (`on: workflow_dispatch`) — it has failed for years on a dead nixpkgs pin in `lib/rain.orderbook`.
- bridge-orchestrator pins Rust 1.93.1 (`rust-toolchain.toml`) and builds on the host toolchain, not the rainix dev shell (1.89).
- bridge-orchestrator's outbound HTTPS clients (Ethereum RPC, watchtower ingest) validate against bundled webpki roots instead of the host trust store.

### Fixed

- bridge-orchestrator cools down on a cycle error that matches no ham classifier, instead of hot-looping.
- bridge-orchestrator builds with pure-Rust TLS (`alloy` on `reqwest-rustls-tls`), keeping the host's OpenSSL off the TLS path. 0.7's vendored `openssl-sys` (sqlcipher) means the build host needs a C toolchain, perl and make.
- `test_config` test helper now initialises the `conductor_config` / `lair_passphrase_file` fields added by the lair-signing change.
