# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- CI runs the bridge-orchestrator Rust suite (`.github/workflows/rust.yml`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`) on the crate's pinned toolchain.
- bridge-orchestrator signs zome calls via lair when available (`CONDUCTOR_CONFIG` + `LAIR_PASSPHRASE_FILE`, defaulting to the fleet paths) — no capability grant committed per connect; falls back to client signing.

### Changed

- the Rainix/Solidity workflow (`.github/workflows/test.yml`) is now manual-only (`on: workflow_dispatch`) — it has failed for years on a dead nixpkgs pin in `lib/rain.orderbook`.
- upgrade bridge-orchestrator Holochain deps to 0.7 (rave_engine 0.9.0, holochain_client 0.9.0, zfuel 0.9.0, holo_hash / holochain_zome_types 0.7.0).
- bridge-orchestrator pins Rust 1.93.1 (`rust-toolchain.toml`) and builds on the host toolchain, not the rainix dev shell (1.89).
- bridge-orchestrator's outbound HTTPS clients (Ethereum RPC, watchtower ingest) validate against bundled webpki roots instead of the host trust store.

### Fixed

- bridge-orchestrator now has a terminal fallback for a failed cycle whose error matches none of ham's classifiers (`is_connection_error` / `is_source_chain_pressure` / `is_request_timeout`): instead of falling off the retry chain and re-running at poll cadence with no cooldown, it cools down before retrying — reusing the source-chain-pressure escalation (doubling backoff to the cap, and `warn!` → `error!` via `ham.unclassified_error` / `ham.unclassified_error_stuck`) on a separate counter, so a persistent unknown failure alerts instead of retrying quietly. The cycle-failure disposition is now a single unit-tested `classify_cycle_failure` (reconnect / cooldown / unclassified-cooldown). Fixes the consumer half of the unclassified-error gap; ham's half (e.g. `ResponderDropped` → reconnect) lands once the pinned `ham` rev is bumped.
- bridge-orchestrator builds with pure-Rust TLS (`alloy` on `reqwest-rustls-tls`), keeping the host's OpenSSL off the TLS path. 0.7's vendored `openssl-sys` (sqlcipher) means the build host needs a C toolchain, perl and make.
- `test_config` test helper now initialises the `conductor_config` / `lair_passphrase_file` fields added by the lair-signing change.
