# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- bridge-orchestrator credits both deposits when one Ethereum transaction locks twice, for deposits it records from this release on.
- bridge-orchestrator marks a deposit bridged only once a RAVE has consumed its link: a deposit whose link the template refused waits, and one whose link it redacted is failed for a person.
- bridge-orchestrator counts a deposit proof only on a link its bridging agent signed, so a copy in another agent's spend cannot mark a deposit bridged.
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
