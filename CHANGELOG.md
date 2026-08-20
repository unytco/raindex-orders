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
- upgrade bridge-orchestrator Holochain deps to 0.7 (rave_engine 0.10.0, holochain_client 0.9.0, zfuel 0.9.1, holo_hash / holochain_zome_types 0.7.0), with rave_engine and zfuel pinned exactly rather than by caret range: both define on-chain type shapes it decodes, and this family ships serialization changes in patch releases.
- bridge-orchestrator pins Rust 1.93.1 (`rust-toolchain.toml`) and builds on the host toolchain, not the rainix dev shell (1.89).
- bridge-orchestrator's outbound HTTPS clients (Ethereum RPC, watchtower ingest) validate against bundled webpki roots instead of the host trust store.
- bridge-orchestrator sums a batch's amounts with rave_engine's `UnitMap::sum_vec` rather than its own copy of the same fold.

### Fixed

- **bridge-orchestrator reads a network that states fees per unit, and stops under-measuring a batch of proofs.** A per-unit fee configuration made every bridge cycle fail at its first call. Separately, the parked-spend link tag was measured with no fees in it at all, so a batch could be packed that Holochain then refused, failing every proof in it and stranding those deposits after eight attempts. The measurement is now taken from the widest fee map the zome can write, and is checked against the code that builds the real tag.
- bridge-orchestrator cools down on a cycle error that matches no ham classifier, instead of hot-looping.
- bridge-orchestrator builds with pure-Rust TLS (`alloy` on `reqwest-rustls-tls`), keeping the host's OpenSSL off the TLS path. 0.7's vendored `openssl-sys` (sqlcipher) means the build host needs a C toolchain, perl and make.
- `test_config` test helper now initialises the `conductor_config` / `lair_passphrase_file` fields added by the lair-signing change.
