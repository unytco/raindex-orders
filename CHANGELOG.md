# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- bridge-orchestrator reports its unclassified-error streak (`unclassified_active` / `unclassified_consecutive`) to watchtower alongside the source-chain-pressure pair, so a persistent unknown failure is visible to watchtower instead of only in log events.
- CI runs the bridge-orchestrator Rust suite (`.github/workflows/rust.yml`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`) on the crate's pinned toolchain.
- bridge-orchestrator signs zome calls via lair when available (`CONDUCTOR_CONFIG` + `LAIR_PASSPHRASE_FILE`, defaulting to the fleet paths) — no capability grant committed per connect; falls back to client signing.

### Changed

- the Rainix/Solidity workflow (`.github/workflows/test.yml`) is now manual-only (`on: workflow_dispatch`) — it has failed for years on a dead nixpkgs pin in `lib/rain.orderbook`.
- upgrade bridge-orchestrator Holochain deps to 0.7 (rave_engine 0.10.0, holochain_client 0.9.0, zfuel 0.9.1, holo_hash / holochain_zome_types 0.7.0), with rave_engine and zfuel pinned exactly.
- bridge-orchestrator pins Rust 1.93.1 (`rust-toolchain.toml`) and builds on the host toolchain, not the rainix dev shell (1.89).
- bridge-orchestrator's outbound HTTPS clients (Ethereum RPC, watchtower ingest) validate against bundled webpki roots instead of the host trust store.
- bridge-orchestrator sums a batch's amounts with rave_engine's `UnitMap::sum_vec` rather than its own copy of the same fold.
- `MAX_LINK_TAG_BYTES` is clamped to 600..=900, so no configuration can consume the last 100 bytes under Holochain's own 1000-byte link-tag limit, or set a cap too small to write anything.

### Fixed

- bridge-orchestrator decodes a network that states fees per unit, and measures a deposit batch against everything the zome writes into the parked-spend tag: the agent's whole ledger, and the lane definitions the zome resolves for a spend that names none. A batch it packs under the cap is not then refused by Holochain.
- bridge-orchestrator abandons an oversize deposit only when its own payload could not be written at any cap: a batch held back by the cap, by the agent's ledger or by the network's own definitions waits for the next cycle instead of failing every row in it permanently.
- bridge-orchestrator logs a failed cycle's whole error chain rather than its outermost line, so a wrapped conductor or socket failure still names its cause in the logs, in watchtower and on the row it reset.
- bridge-orchestrator cools down on a cycle error that matches no ham classifier, instead of hot-looping.
- bridge-orchestrator builds with pure-Rust TLS (`alloy` on `reqwest-rustls-tls`), keeping the host's OpenSSL off the TLS path. 0.7's vendored `openssl-sys` (sqlcipher) means the build host needs a C toolchain, perl and make.
- `test_config` test helper now initialises the `conductor_config` / `lair_passphrase_file` fields added by the lair-signing change.
