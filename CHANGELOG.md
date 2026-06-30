# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- bridge-orchestrator signs zome calls via lair when available: reads `CONDUCTOR_CONFIG` + `LAIR_PASSPHRASE_FILE` (defaulting to the fleet paths) and passes them to ham's `try_lair_signing_from_node`, so it signs as its own agent key with no capability grant committed per connect/reconnect; falls back to client signing when lair is unavailable.

### Changed

- upgrade bridge-orchestrator Holochain deps to 0.6.2-rc.0 (rave_engine 0.6.0; holo_hash / holochain_client / holochain_zome_types)

### Fixed

- `test_config` test helper now initialises the `conductor_config` / `lair_passphrase_file` fields added by the lair-signing change above. The bridge-orchestrator unit tests compile only under `cargo test` (CI runs the Solidity suite), so the stale helper had gone unnoticed.
