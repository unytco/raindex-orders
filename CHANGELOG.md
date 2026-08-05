# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- CI runs the bridge-orchestrator Rust suite (`.github/workflows/rust.yml`): `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`, on the crate's pinned toolchain (`rustup toolchain install` resolves `bridge-orchestrator/rust-toolchain.toml`, so the version is never restated in the workflow). It is a separate workflow from `test.yml` so the disabled Solidity job cannot mask it.
- bridge-orchestrator signs zome calls via lair when available: reads `CONDUCTOR_CONFIG` + `LAIR_PASSPHRASE_FILE` (defaulting to the fleet paths) and passes them to ham's `try_lair_signing_from_node`, so it signs as its own agent key with no capability grant committed per connect/reconnect; falls back to client signing when lair is unavailable.

### Changed

- the Rainix/Solidity workflow (`.github/workflows/test.yml`) is manual-only (`on: workflow_dispatch`). It has failed for roughly two years on a dead nixpkgs pin in the `lib/rain.orderbook` submodule, which cannot be fixed without bumping that submodule; re-enable it once that happens. The file is kept, not deleted, so the re-enable is a one-line change.
- upgrade bridge-orchestrator Holochain deps to 0.7 (rave_engine 0.9.0, holochain_client 0.9.0, zfuel 0.9.0, holo_hash / holochain_zome_types 0.7.0). `GetStrategy` is no longer public at `holochain_zome_types::entry`; it is imported from that crate's `prelude`.
- bridge-orchestrator pins Rust 1.93.1 (`bridge-orchestrator/rust-toolchain.toml`): holochain_zome_types 0.7 needs `str::floor_char_boundary`, stable since 1.91, while the repo's rainix dev shell supplies 1.89. The crate therefore builds on the host toolchain — which is how `automation` has always released it. The pin sits above the 1.91 floor; the other 0.7 crates pin their own toolchains (1.95) independently.
- bridge-orchestrator's outbound HTTPS clients (Ethereum RPC, watchtower ingest) validate against bundled webpki roots instead of the host trust store, following the switch to rustls.

### Fixed

- bridge-orchestrator builds with pure-Rust TLS (`alloy` on `reqwest-rustls-tls`), keeping the host's OpenSSL off the TLS path and unblocking the host build. Holochain 0.7 does put a *vendored* `openssl-sys` back in the tree for sqlcipher (holochain_keystore now requests lair_keystore's `rusqlite-bundled-sqlcipher-vendored-openssl`), so the build host needs a C toolchain, perl and make; it is compiled from source and is not on the TLS path.
- `test_config` test helper now initialises the `conductor_config` / `lair_passphrase_file` fields added by the lair-signing change above. The bridge-orchestrator unit tests compile only under `cargo test`, which CI did not run at the time, so the stale helper had gone unnoticed.
