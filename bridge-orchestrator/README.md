# bridge-orchestrator

Single-writer bridge orchestrator that unifies lock detection (ETH -> Holochain) and
withdrawal coupon generation (Holochain -> ETH) into periodic bridge cycles.

Built with clap 4. All configuration is via environment variables, optionally
loaded from a `.env` file (via dotenvy) in the working directory.

## Subcommands

### `bridge-orchestrator run`

Long-running daemon. Watches for on-chain lock events, queues work items into a
local SQLite database, and runs periodic bridge cycles that process deposits and
generate withdrawal coupons.

This is the command used by the systemd service.

```
bridge-orchestrator run
```

No additional flags.

### `bridge-orchestrator status`

Query the SQLite work-item database. Prints one JSON object per line to stdout.

```
bridge-orchestrator status [OPTIONS]
```

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--flow` | string | _(all)_ | Filter by flow name (e.g. `lock`) |
| `--state` | enum | _(all)_ | Filter by state (see values below) |
| `--item-id` | string | _(all)_ | Filter by specific item ID |
| `--limit` | integer | `50` | Maximum rows returned |

`--state` values: `detected`, `queued`, `claimed`, `in_flight`, `succeeded`, `failed`

### `bridge-orchestrator clear`

Delete work items from the SQLite database. Exactly one of the two mode flags
is required; they are mutually exclusive.

```
bridge-orchestrator clear --non-in-progress
bridge-orchestrator clear --non-in-progress --older-than-s 604800
bridge-orchestrator clear --all
```

| Flag | Description |
|------|-------------|
| `--non-in-progress` | Delete only terminal rows (`succeeded`, `failed`) |
| `--all` | Delete every row in `work_items` |
| `--older-than-s N` | Only with `--non-in-progress`: restrict deletion to terminal rows whose `updated_at` is older than N seconds. Applied to both `succeeded` and `failed`. Use the in-process retention task (below) for per-state windows. |

Outputs a JSON object. Plain `--non-in-progress` returns
`{"mode":"non_in_progress","deleted_count":N}`; with `--older-than-s` the
output also includes `succeeded_deleted` and `failed_deleted`. Steady-state ops
should rely on the in-process retention task and reserve this CLI for one-off
hygiene.

## Environment variables

Every subcommand loads the full config from the environment on startup, so
the env file must be sourced even for `status` and `clear`.

### Config (all commands)

| Variable | Required | Default |
|----------|----------|---------|
| `NETWORK` | No | `sepolia` (`mainnet` or `sepolia`) |
| `SEPOLIA_RPC_URL` | No | `https://1rpc.io/sepolia` |
| `SEPOLIA_LOCK_VAULT_ADDRESS` | **Yes** (sepolia) | -- |
| `ETH_RPC_URL` | No (mainnet) | `https://eth.llamarpc.com` |
| `MAINNET_LOCK_VAULT_ADDRESS` | **Yes** (mainnet) | -- |
| `DB_PATH` | No | `./data/bridge_orchestrator.db` |
| `POLL_INTERVAL_MS` | No | `5000` |
| `BRIDGE_CYCLE_INTERVAL_MS` | No | `180000` (falls back to `COUPON_POLL_INTERVAL_MS`) |
| `MAX_LINK_TAG_BYTES` | No | `800` (per-link tag cap, must stay under Holochain MAX_TAG_SIZE=1000) |
| `COUPONS_TARGET_KB` | No | `512` (aggregate byte budget for withdrawal coupons map, not a link tag) |
| `HOLOCHAIN_ADMIN_PORT` | No | `30000` |
| `HOLOCHAIN_APP_PORT` | No | `30001` |
| `HOLOCHAIN_APP_ID` | No | `bridging-app` |
| `HOLOCHAIN_ROLE_NAME` | No | `alliance` |
| `HOLOCHAIN_BRIDGING_AGENT_PUBKEY` | **Yes** | -- |
| `HOLOCHAIN_LANE_DEFINITION` | No | _(none)_ |
| `HOLOCHAIN_UNIT_INDEX` | No | `1` |
| `HAM_REQUEST_TIMEOUT_SECS` | No | `120` (per-request timeout applied to the Holochain app websocket; prevents a slow/hung zome call from blocking the orchestrator indefinitely) |
| `HAM_RECONNECT_BACKOFF_INITIAL_MS` | No | `1000` (initial reconnect delay after a dropped Holochain websocket) |
| `HAM_RECONNECT_BACKOFF_MAX_MS` | No | `30000` (cap on reconnect delay) |
| `HAM_RECONNECT_ESCALATE_AFTER` | No | `5` (after this many consecutive failed reconnect attempts, logs escalate from `warn` to `error` so ops alerts can fire; the loop keeps retrying forever) |
| `HAM_PRESSURE_COOLDOWN_MS` | No | `30000` (base pause after a Holochain source-chain-pressure error such as `"deadline has elapsed"`; doubles on each consecutive occurrence up to `HAM_PRESSURE_COOLDOWN_MAX_MS`) |
| `HAM_PRESSURE_COOLDOWN_MAX_MS` | No | `90000` (cap on the escalating pressure cooldown; once reached, consecutive pressure errors log at `error` level with `event="ham.source_chain_pressure_stuck"` so alerts can fire) |
| `SLOW_CALL_THRESHOLD_MS` | No | `35000` (if a write-bearing zome call inside a bridge cycle exceeds this, the orchestrator ejects the rest of the cycle instead of stacking more pressure; the reconciler advances the skipped stages next cycle; set to `0` to disable. Tune above your conductor's healthy per-call baseline so only clearly-slow calls eject the rest of the cycle; 35s sits just above the typical successful latency observed in production (~20–32s) while still protecting against pathological calls piling up) |
| `RUST_LOG` | No | `info` |

Confirmations are not configurable: 15 (mainnet) / 5 (sepolia).

### Watchtower reporter (optional)

The orchestrator can post small, DNA-scoped health and throughput
snapshots to the `unyt-watchtower` Worker. The reporter runs in a
detached tokio task with a 10s per-request HTTP timeout and
log-and-forget error handling, so it can never affect the bridge
cycle. Configuration is fully optional: if any required variable is
unset the reporter is disabled and the orchestrator runs exactly as
before.

| Variable | Required | Default |
|----------|----------|---------|
| `WATCHTOWER_INGEST_URL` | Yes (to enable) | -- (e.g. `https://watchtower.unyt.dev/ingest/bridge`) |
| `WATCHTOWER_OBSERVER_ID` | Yes (to enable) | -- (e.g. `bridge-hot-2-mhot`) |
| `WATCHTOWER_HMAC_SECRET_HEX` | Yes (to enable) | -- (64-char hex; register in the Worker's D1 `observer_secrets` table) |
| `WATCHTOWER_DNA_B64` | Yes (to enable) | -- (the alliance DNA hash this bridge is bound to) |
| `WATCHTOWER_REPORT_INTERVAL_MS` | No | `60000` |

Registration:

```bash
# From this repo's root, inside the dev shell if you have one.
./automation/scripts/register-bridge-reporter.sh \
    --observer-id bridge-hot-2-mhot \
    --dna-b64 uhCkk... \
    --ingest-url https://watchtower.unyt.dev/ingest/bridge
```

The script generates an HMAC secret, upserts it into the Worker's
`observer_secrets` table via `wrangler d1 execute`, and prints the env
lines to add to `bridge-orchestrator.env`. Reload the systemd unit after
updating the env file.

The reported panel shows up on the watchtower DNA Overview page for
the configured DNA; no new tabs or tables are added to the UI.

### Retention (automatic cleanup)

The orchestrator runs an in-process retention task that periodically
prunes old terminal rows (`succeeded`, `failed`) from `work_items`.
It runs as a detached tokio task — same failure-isolation contract
as the watchtower reporter — so any error is logged and swallowed
without touching the bridge cycle. No separate systemd timer or cron
is required.

Defaults are deliberately compact: succeeded rows are kept for 7 days
(routine history window), failed rows for 30 days (longer because
failures are operationally forensic). All values are tunable via
environment variables; set `BRIDGE_RETENTION_DISABLED=true` to skip
spawning the task entirely.

| Variable | Required | Default |
|----------|----------|---------|
| `BRIDGE_RETENTION_DISABLED` | No | `false` (set to `true` to disable the retention task) |
| `BRIDGE_RETENTION_TICK_MS` | No | `3600000` (1 hour) |
| `BRIDGE_RETENTION_SUCCEEDED_MAX_AGE_S` | No | `604800` (7 days) |
| `BRIDGE_RETENTION_FAILED_MAX_AGE_S` | No | `2592000` (30 days) |

When a tick deletes rows, a single `tracing::info!` line is emitted
with `event="bridge_orchestrator.retention.pruned"` and the per-state
counts. Idle ticks log at `trace` level so steady-state runs stay
quiet.

Application-log rotation is intentionally **not** handled by the
binary. The orchestrator writes via `tracing` to stdout/stderr and
delegates rotation to the process supervisor (systemd/journald,
docker, or your equivalent). This keeps deployment conventional and
avoids duplicating log-lifecycle logic inside the service.

### Deployment via automation

For the `hot-2-mhot` bridge server the orchestrator is fully provisioned
by the `automation/` repo. From its root, one command builds the binary,
auto-derives the DNA hash from the latest Holochain deploy result,
reuses/creates a local HMAC secret for the watchtower reporter,
registers it with the worker, writes `bridge-orchestrator.env` (including
`WATCHTOWER_*` and any `BRIDGE_RETENTION_*` overrides from
`config/hot-2-mhot-bridge/services.json`), SCPs the binary, and restarts
systemd:

```bash
cd automation && make hot-2-mhot-bridge-services
```

Manual editing of `bridge-orchestrator.env` is only needed for local/dev
setups or ad-hoc secret rotation. See `automation/scripts/setup-blockchain-bridge-services.sh`
and the `watchtower_reporter` / `retention` blocks in `services.json`
for the knobs available to operators.

### Holochain websocket resilience

The orchestrator owns one persistent app websocket to the conductor. If that
socket is dropped (idle timeout, conductor restart, network blip), the
orchestrator will automatically:

1. Detect the failure on the next pre-cycle health probe
   (`app_info` round-trip) or on the next cycle-level error classified as
   connection-like.
2. Reconnect with exponential backoff capped at `HAM_RECONNECT_BACKOFF_MAX_MS`,
   with small jitter and escalating log level per `HAM_RECONNECT_ESCALATE_AFTER`.
3. Resume normal cycles on success.

Cycle-level errors still reset affected locks from `in_flight` back to
`queued` via the existing lifecycle (see below). The reconnect layer never
retries an individual zome call; reconnects only happen between cycles so a
dropped socket cannot cause a write to be replayed mid-cycle.

**Known limitation: cycle-level idempotency.** A bridge cycle performs four
writes on the conductor before marking locks `succeeded`. If a write lands
on the conductor but the response is lost (ws drop or timeout), the affected
locks are currently re-queued and will be re-processed on the next cycle.
This risk exists independently of websocket drops and predates the
reconnect layer. A reconciliation step (scan existing EA links / action
hashes before re-committing) is a separate, larger change that is not part
of the reconnect work.

### Graceful shutdown

`bridge-orchestrator run` installs handlers for `SIGINT` and `SIGTERM`. On
signal, the currently running bridge cycle is allowed to finish
(interrupting mid-write would leave state ambiguous), and the main loop
then exits cleanly between iterations. systemd `Restart=` and rolling
deploys are safe.

### Signer (run only, when generating withdrawal coupons)

These are read lazily during the bridge cycle, not at startup.

| Variable | Required | Default |
|----------|----------|---------|
| `SIGNER_PRIVATE_KEY` | Yes | -- |
| `ORDER_HASH` | Yes | -- |
| `ORDER_OWNER` | Yes | -- |
| `ORDERBOOK_ADDRESS` | Yes | -- |
| `TOKEN_ADDRESS` | Yes | -- |
| `VAULT_ID` | Yes | -- |
| `EXPIRY_SECONDS` | No | `604800` (7 days) |

## Usage on the HOT-2-mHOT bridge server

Deployed paths:

- **Working directory:** `/home/test-hot-bridge/bridge-services`
- **Env file:** `./bridge-orchestrator.env`
- **Binary:** `/usr/local/bin/bridge-orchestrator` (symlink)
- **systemd unit:** `bridge-orchestrator.service`
- **SQLite database:** `./data/locks.db`

### Loading the environment

All commands require the env file to be sourced first:

```bash
cd /home/test-hot-bridge/bridge-services
set -a; source ./bridge-orchestrator.env; set +a
```

Or as a one-liner (useful over SSH):

```bash
bash -lc 'cd /home/test-hot-bridge/bridge-services && set -a; source ./bridge-orchestrator.env; set +a; bridge-orchestrator status --limit 10'
```

### Common recipes

```bash
# Recent 10 work items
bridge-orchestrator status --limit 10

# Only failed items
bridge-orchestrator status --state failed

# Queued items in the lock flow
bridge-orchestrator status --flow lock --state queued

# Items currently being processed
bridge-orchestrator status --state in_flight

# Look up a specific item
bridge-orchestrator status --item-id "lock:42"

# Clean up completed/failed rows
bridge-orchestrator clear --non-in-progress

# Wipe everything (use with caution)
bridge-orchestrator clear --all
```

### systemd service management

```bash
# Check service status
systemctl status bridge-orchestrator.service

# View recent logs
journalctl -u bridge-orchestrator.service -n 100 --no-pager

# Follow logs in real time
journalctl -u bridge-orchestrator.service -f

# Restart the service
systemctl restart bridge-orchestrator.service

# Stop the service
systemctl stop bridge-orchestrator.service
```

## Status output fields

Each line from `bridge-orchestrator status` is a JSON object with these fields:

| Field | Type | Description |
|-------|------|-------------|
| `id` | integer | Auto-increment row ID |
| `flow` | string | Flow name (e.g. `lock`) |
| `task_type` | string | Task within the flow (e.g. `create_parked_link`, `initiate_deposit`) |
| `item_id` | string | Identifier for the work item |
| `direction` | string or null | `transfer_in` for lock deposits, null otherwise |
| `transfer_type` | string or null | `lock` for lock deposits, null otherwise |
| `amount_raw` | string or null | Human-readable HOT amount (converted from wei if needed) |
| `beneficiary` | string or null | Holochain agent receiving the deposit |
| `counterparty` | string or null | Ethereum address that locked tokens |
| `status` | string | Current state (see lifecycle below) |
| `attempts` | integer | Number of processing attempts so far |
| `max_attempts` | integer | Maximum attempts before permanent failure (default 8) |
| `next_retry_at` | integer or null | Unix timestamp for next retry (null if not scheduled) |
| `error_class` | string or null | `transient` or `permanent` |
| `last_error` | string or null | Most recent error message |
| `created_at` | integer | Unix timestamp when the item was created |
| `updated_at` | integer | Unix timestamp of last state change |

## Work item lifecycle

```
detected ─> queued ─> claimed ─> in_flight ─┬─> succeeded
                ^                            │
                └──── (transient retry) ─────┤
                                             └─> failed (after max_attempts)
```

- **detected** -- lock event seen on-chain, waiting for confirmations
- **queued** -- ready to be processed in the next bridge cycle
- **claimed** -- picked up by the single-writer executor
- **in_flight** -- actively being processed (Holochain call or on-chain tx)
- **succeeded** -- completed successfully
- **failed** -- exhausted all retry attempts (`max_attempts` = 8)

On startup, any items left in `claimed` or `in_flight` (from a previous crash)
are automatically recovered back to `queued` if attempts remain, or marked
`failed` if `max_attempts` has been reached.
