# raindex-orders — Agent Instructions

## Purpose

The HOT ↔ bridged-HOT (bHOT) bridge: a Solidity vault contract
(`HoloLockVault`) that locks/unlocks token pairs against a Holochain
ledger, plus a Rainlang claim coupon, a **bridge orchestrator**
daemon (Rust) that watches both sides and reconciles, and a UI
(SvelteKit / Vite) for end users to initiate swaps.

## Classification

`service` — the bridge orchestrator and UI deploy via `automation/`;
the contracts deploy via the included Foundry scripts.

## Stack

- **Solidity (Foundry)** at root: `foundry.toml`, `src/`, `test/`,
  `lib/`, `script/`, plus `compose-rainlang.mjs` for Rainlang
  composition and `deploy-sepolia.sh` for testnet deploy.
- **Rust** at [`bridge-orchestrator/`](bridge-orchestrator/) — the
  reconciliation daemon.
- **TypeScript / SvelteKit** at [`ui/`](ui/) — Vite, with
  `format` / `lint` / `test` npm scripts wired.
- Root `package.json` is a tools shell (no scripts) — `npx`
  invocations use it for transitive deps.
- **Requires `nix develop -c …`** — see
  [`flake.nix`](flake.nix). The workshop's
  [Nix discipline section](../AGENTS.md#nix-discipline) lists this
  repo.

## Build

```bash
nix develop -c forge build                          # contracts
nix develop -c bash -c '( cd bridge-orchestrator && cargo build --release )'
nix develop -c bash -c '( cd ui && npm install && npm run build )'
node compose-rainlang.mjs                           # rebuild composed Rainlang
```

## Format

Apply, then verify, per stack:

```bash
# Solidity
nix develop -c forge fmt
nix develop -c forge fmt --check

# Rust (orchestrator)
( cd bridge-orchestrator && nix develop -c cargo fmt )
( cd bridge-orchestrator && nix develop -c cargo fmt --check )

# UI (scripts already wired)
( cd ui && npm run format )
( cd ui && npm run format -- --check )   # if `format` accepts --check; else use lint
( cd ui && npm run lint )
```

## Test

```bash
nix develop -c forge test                                   # contracts
( cd bridge-orchestrator && nix develop -c cargo test )     # orchestrator
( cd ui && npm run test )                                   # UI
```

`forge test` is the load-bearing suite — bridge correctness is
proven there.

## Deploy

- **Contracts**: `bash deploy-sepolia.sh` for testnet; mainnet
  deploys are `forge script` invocations with explicit args (see
  [`script/`](script/) and [`DEPLOY.md`](DEPLOY.md)).
- **Bridge orchestrator + UI**: deploy via
  [`automation/`](../automation/) (`make hot-2-mhot-bridge-services`
  or similar — see workshop
  [Deployment hub](../AGENTS.md#deployment-hub-automation)).

## Related repos in workshop

- Bridge orchestrator uses [`ham`](../ham/) for its Holochain
  `AppWebsocket` connection.
- Deployed by [`automation/`](../automation/).
- Coordinates with the Unyt DNA in
  [`unyt-sandbox/unyt`](../unyt-sandbox/unyt/) (the Holochain side
  the bridge reconciles against).

## Changelog

File: [`./CHANGELOG.md`](./CHANGELOG.md). Format: [Keep a Changelog
1.1.0](https://keepachangelog.com/en/1.1.0/) with `## [Unreleased]`
at the top and standard subsections. One bullet per agent change,
≤120 chars, present-tense imperative. Branch-type → section mapping
per workshop
[`branch-and-pr-workflow.mdc`](../.cursor/rules/branch-and-pr-workflow.mdc).

Contract changes are user-money-impacting — always note ABI changes,
storage-layout changes, and constructor-arg changes under
`### Changed` (or `### Removed` / `### Added`). Bridge-protocol
changes MUST appear; UI-only tweaks are usually `### Changed`.

## Repo-specific rules

- **Contract storage layout is sacred.** Adding a field is fine;
  reordering or removing existing storage breaks deployed state.
  Use append-only, document under `### Changed`.
- **Rainlang composition is generated.** Edit the source `.rain`
  files and run `node compose-rainlang.mjs` to refresh — don't edit
  composed output by hand.
- **Bridge orchestrator must idempotently retry.** Network
  partitions are routine; the daemon must not double-claim.

## Lessons learned

_Append entries here whenever an agent (or human) loses time to
something a guardrail would have prevented. Keep each entry: date,
short symptom, concrete fix._
