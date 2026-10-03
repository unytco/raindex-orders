# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- the bridge UI answers `POST /api/coupon-status` with each claim coupon's status on Sepolia (`redeemed`, `unredeemed`, `expired` or `invalid`), up to 50 coupons per request, read at the `safe` block and open to any origin.
- bridge-orchestrator reports its unclassified-error streak (`unclassified_active` / `unclassified_consecutive`) to watchtower alongside the source-chain-pressure pair, so a persistent unknown failure is visible to watchtower instead of only in log events.
- CI runs the bridge-orchestrator Rust suite (`.github/workflows/rust.yml`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`) on the crate's pinned toolchain.
- bridge-orchestrator signs zome calls via lair (`CONDUCTOR_CONFIG` + `LAIR_PASSPHRASE_FILE`, defaulting to the fleet paths), committing no capability grant per connect. A node that cannot offer lair stops the orchestrator at startup with the reason instead of writing to the bridging agent's chain.

### Changed

- bridge-orchestrator bridges on the one lane that names its agent and lists the unit in `HOT_UNIT_INDEX`, and will not start while `HOLOCHAIN_LANE_DEFINITION` or `HOLOCHAIN_UNIT_INDEX` is set.
- the lair requirement is `ham`'s decision, supplied with the orchestrator's two paths, rather than restated here. A refusal names the fault before the reason the node could not offer lair.
- the Rainix/Solidity workflow (`.github/workflows/test.yml`) is now manual-only (`on: workflow_dispatch`) — it has failed for years on a dead nixpkgs pin in `lib/rain.orderbook`.
- upgrade bridge-orchestrator Holochain deps to 0.7 (rave_engine 0.12.0, holochain_client 0.9.0, zfuel 0.9.1, holo_hash / holochain_zome_types 0.7.0), with zfuel and rave_engine pinned to exact crates.io versions.
- bridge-orchestrator pins Rust 1.93.1 (`rust-toolchain.toml`) and builds on the host toolchain, not the rainix dev shell (1.89).
- bridge-orchestrator pins `ham` to an exact revision (`4e10636`) rather than its `main` branch, so a change to it reaches the orchestrator only in a commit that names the new revision.
- bridge-orchestrator's outbound HTTPS clients (Ethereum RPC, watchtower ingest) validate against bundled webpki roots instead of the host trust store.
- bridge-orchestrator sums a batch's amounts with rave_engine's `UnitMap::sum_vec` rather than its own copy of the same fold.
- `MAX_LINK_TAG_BYTES` is clamped to 600..=900, so no configuration can consume the last 100 bytes under Holochain's own 1000-byte link-tag limit, or set a cap too small to write anything.
- `MAX_LINK_TAG_BYTES` defaults to 850, the room a single deposit needs now that the parked-spend tag also states what the spend was charged.

### Fixed

- bridge-orchestrator gives each withdrawal coupon a nonce derived from its withdrawal's transaction ID, so coupons signed in the same second can all be claimed, and a withdrawal signed again cannot be claimed twice.
- bridge-orchestrator waits for the agreement a deposit was parked on to consume it, even after its lane names a new one.
- bridge-orchestrator keeps bridging after its lane's definition is extended or replaced.
- bridge-orchestrator fails a cycle with an error when no lane, or more than one, names its agent and lists the HOT unit.
- bridge-orchestrator keeps bridging when another lane sets no credit limit adjustment, and fails a cycle, naming its lane, when its own sets none.
- bridge-orchestrator sends `execute_rave` the transaction fields the alliance DNA reads, so a bridge cycle runs past stage 2 instead of failing every link on a node running the fee-charging DNA.
- bridge-orchestrator decodes a network that states fees per unit, and measures a deposit batch against everything the zome writes into the parked-spend tag: the agent's whole ledger, and the lane definitions the zome resolves for a spend that names none. A batch it packs under the cap is not then refused by Holochain.
- bridge-orchestrator abandons an oversize deposit only when its own payload could not be written at any cap: a batch held back by the cap, by the agent's ledger or by the network's own definitions waits for the next cycle instead of failing every row in it permanently.
- bridge-orchestrator logs a failed cycle's whole error chain rather than its outermost line, so a wrapped conductor or socket failure still names its cause in the logs, in watchtower and on the row it reset.
- bridge-orchestrator cools down on a cycle error that matches no ham classifier, instead of hot-looping.
- bridge-orchestrator builds with pure-Rust TLS (`alloy` on `reqwest-rustls-tls`), keeping the host's OpenSSL off the TLS path. 0.7's vendored `openssl-sys` (sqlcipher) means the build host needs a C toolchain, perl and make.
- `test_config` test helper now initialises the `conductor_config` / `lair_passphrase_file` fields added by the lair-signing change.
