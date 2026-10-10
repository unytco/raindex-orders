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

No additional flags. Before it reads from Holochain or writes anything, `run`
refuses to start, naming each variable at fault, when:

- a signer variable is unset or malformed
- the RPC answers for a chain other than `NETWORK`'s (1 for `mainnet`,
  11155111 for `sepolia`)
- the vault has no contract, or its `token()`, `orderbook()` or `vaultId()`
  differs from `TOKEN_ADDRESS`, `ORDERBOOK_ADDRESS` or `VAULT_ID`
- `ORDER_OWNER` is not the vault
- the claim order that `ORDER_OWNER`, `TOKEN_ADDRESS`, `VAULT_ID` and the
  `CLAIM_*` values describe does not hash to `ORDER_HASH`
- `ORDERBOOK_ADDRESS` holds no order `ORDER_HASH`
- `CLAIM_SIGNER` is a contract, such as a Safe: the orchestrator signs only as
  a key
- `SIGNER_PRIVATE_KEY` is not the key of `CLAIM_SIGNER`
- the claim order does not accept a coupon signed with that key. `run`
  simulates `takeOrders` with a one-wei coupon to itself, and starts only if
  the order pays it, or answers `MinimumInput` because its vault is empty.
- on `mainnet`, `SIGNER_PRIVATE_KEY` is the test signer
  `0x8E72b7568738da52ca3DCd9b24E178127A4E7d37`, whose key is public

An RPC it cannot reach, or that does not answer within 30 s, also stops it, and
its supervisor restarts it. So does a conductor config, at `CONDUCTOR_CONFIG`,
that does not set `db_sync_level: Full`, which `run` checks first, with
Ethereum on or off: without it a power loss can roll back the conductor's
latest writes behind what the bridge has recorded. And so does a database that
serves another vault: a `DB_PATH` serves the vault it first ran with, and names
each lock by that vault and its lock ID. Once the checks pass it logs
`startup checks passed`.

`NETWORK=none` turns Ethereum off, for a node with no chain such as a local
emulation. `run` then makes no Ethereum call: it skips the checks above,
watches no lock and signs no coupon. It still does its Holochain work. Deposits
parked on the bridging agreement go through, and every withdrawal stays parked.
A lock row whose next step writes a deposit proof waits for a run on a chain.
It logs once that Ethereum is off, and names each chain variable it ignores.
Only an explicit `none` turns Ethereum off.

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

`--state` values: `queued`, `claimed`, `in_flight`, `succeeded`, `failed`

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

### `bridge-orchestrator in-transit`

Lists each transfer the bridging agent's network still holds, so a migration
closes the old network only once nothing is in transit, or the operator accepts
what is. Run it with the orchestrator stopped. It takes the configuration of
`run`, reads `DB_PATH` and the bridging agent's conductor, and sends no write
to Holochain or Ethereum.

```
bridge-orchestrator in-transit
bridge-orchestrator in-transit --mark-failed
```

It prints one JSON object per line:

| `kind` | What is in transit | Fields |
|--------|--------------------|--------|
| `row` | A row that is neither `succeeded` nor `failed`, at `cl_link_created` or `br_spend_created` | `item_id`, `lock_id`, `step`, `link` |
| `deposit_link` | A live link carrying deposit proofs that the bridging agent parked on the credit-limit adjustment agreement or the bridging agreement of its lane in force | `agreement`, `link`, `lock_ids` |
| `withdrawal` | A live spend in the `withdrawer` role on that bridging agreement | `agreement`, `spend`, `spender`, `amount`, `withdraw_to_address` |

`lock_id` is `null` for a row whose payload cannot be read, `link` for one that
records none, and `withdraw_to_address` for a withdrawal that names none, which
no coupon can pay. `amount` is the spend's unit map, its HOT under
`HOT_UNIT_INDEX`. A `failed` row is not listed, and nor is another agent's spend
in the `bridging_agent` role or a spend in any other role: no cycle takes them.
Log lines go to stdout too, and never start with `{`.

It exits 0 only when it read the database and the conductor and found nothing.
It fails on a `DB_PATH` that does not exist, on one that serves another vault
than the one configured when Ethereum is on, and on a row whose state or step it
cannot read.

`--mark-failed` lists the same, then marks `failed` each listed row and each
row, neither `succeeded` nor `failed`, whose lock is in a listed link. Each
gets `last_error` `in transit at the old network's close; it is paid by hand on
the new network`, so the new network's orchestrator pays none of them. It marks
all of them in one transaction, or none, and changes no other row. It exits 0
only when it read the database and the conductor and marked them. A lock in a
listed link whose row is already `succeeded` or `failed`, a lock with no row,
and a withdrawal are recorded only in what it prints.

What it lists is paid by hand on the new network, once the person who pays has
checked that the old network did not pay it. Pay each lock ID once: a deposit
waiting on its link shows both as its `row` and in the `lock_ids` of the link
carrying it, and an earlier `failed` row may name it too. A lock whose row is
`succeeded` was paid. Pay each withdrawal once, by its `spend`.

## Environment variables

Every subcommand loads the config below on startup, so the env file must be
sourced even for `status` and `clear`. Only `run` reads the signer variables
and the chain, and only `run` and `in-transit` the conductor. A network
variable set to an empty value counts as unset.

### Config (all commands)

| Variable | Required | Default |
|----------|----------|---------|
| `NETWORK` | No | `sepolia` (`mainnet`, `sepolia` or `none`) |
| `SEPOLIA_RPC_URL` | No, refused on mainnet | `https://1rpc.io/sepolia` |
| `SEPOLIA_LOCK_VAULT_ADDRESS` | No, refused on mainnet | `0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6` |
| `ETH_RPC_URL` | **Yes** (mainnet), refused on sepolia | -- |
| `MAINNET_LOCK_VAULT_ADDRESS` | **Yes** (mainnet), refused on sepolia | -- |
| `DB_PATH` | No | `./data/bridge_orchestrator.db` |
| `POLL_INTERVAL_MS` | No | `5000` |
| `BRIDGE_CYCLE_INTERVAL_MS` | No | `180000` (falls back to `COUPON_POLL_INTERVAL_MS`) |
| `MAX_LINK_TAG_BYTES` | No | `850` (per-link tag cap, clamped to 600..=900: the last 100 bytes under Holochain MAX_TAG_SIZE=1000 stay out of reach of any configuration, and a value below 600 is clamped up to it with a warning at startup, not refused) |
| `COUPONS_TARGET_KB` | No | `512` (aggregate byte budget for withdrawal coupons map, not a link tag) |
| `HOLOCHAIN_ADMIN_PORT` | No | `30000` |
| `HOLOCHAIN_APP_PORT` | No | `30001` |
| `HOLOCHAIN_APP_ID` | No | `bridging-app` |
| `HOLOCHAIN_ROLE_NAME` | No | `alliance` |
| `CONDUCTOR_CONFIG` | No | `/etc/holochain/conductor-config.yaml` (the conductor config naming the external `lair_server` that signs zome calls; `run` also requires it to set `db_sync_level: Full`) |
| `LAIR_PASSPHRASE_FILE` | No | `/var/lib/holochain/lair-passphrase` |
| `HOLOCHAIN_BRIDGING_AGENT_PUBKEY` | **Yes** | -- (each cycle bridges on the one lane that names this key its bridging agent and lists `HOT_UNIT_INDEX` in its service units, counting the global definition's lane and each lane's definition in force. No such lane, or more than one, fails the cycle, and so does that lane setting no credit limit adjustment or no bridging agreement) |
| `HOT_UNIT_INDEX` | No | `1` (`HOLOCHAIN_UNIT_INDEX` or `HOLOCHAIN_LANE_DEFINITION` set at all, even empty, stops the orchestrator at startup) |
| `HAM_REQUEST_TIMEOUT_SECS` | No | `120` (per-request timeout applied to the Holochain app websocket; prevents a slow/hung zome call from blocking the orchestrator indefinitely) |
| `HAM_RECONNECT_BACKOFF_INITIAL_MS` | No | `1000` (initial reconnect delay after a dropped Holochain websocket) |
| `HAM_RECONNECT_BACKOFF_MAX_MS` | No | `30000` (cap on reconnect delay) |
| `HAM_RECONNECT_ESCALATE_AFTER` | No | `5` (after this many consecutive failed reconnect attempts, logs escalate from `warn` to `error` so ops alerts can fire; the loop keeps retrying forever) |
| `HAM_PRESSURE_COOLDOWN_MS` | No | `30000` (base pause after a Holochain source-chain-pressure error such as `"deadline has elapsed"`; doubles on each consecutive occurrence up to `HAM_PRESSURE_COOLDOWN_MAX_MS`) |
| `HAM_PRESSURE_COOLDOWN_MAX_MS` | No | `90000` (cap on the escalating pressure cooldown; once reached, consecutive pressure errors log at `error` level with `event="ham.source_chain_pressure_stuck"` so alerts can fire) |
| `SLOW_CALL_THRESHOLD_MS` | No | `35000` (if a measured zome call inside a bridge cycle exceeds this, the orchestrator ejects the rest of the cycle instead of stacking more pressure; the reconciler advances whatever was already written; set to `0` to disable. The measured calls are each stage's write and the ledger read that sizes the spend tag, so a slow read ends a cycle before anything is written. Tune above your conductor's healthy per-call baseline so only clearly-slow calls eject the rest of the cycle; 35s sits just above the typical successful latency observed in production (~20–32s) while still protecting against pathological calls piling up) |
| `RAVE_MAX_LINKS` | No | _(unset = no cap)_ — if set to a positive integer, each `execute_rave` call in a cycle consumes at most this many parked links; the rest stay live server-side and are picked up by the next cycle. Applied independently to the S2 credit-limit RAVE (`cl_links`) and the S4 bridging RAVE (pooled deposits + selected withdrawals; deposits are kept preferentially). `0` is treated as disabled (warn at startup). The existing `COUPONS_TARGET_KB` withdrawal-coupon cap still applies on top. Intended as a mitigation when `execute_rave` hangs correlate with large batch sizes; leave unset unless you've observed that pattern. |
| `RUST_LOG` | No | `info` |

Confirmations are not configurable: the lock read reads only blocks with 5
confirmations, on both networks. A new `DB_PATH` starts at the newest of them.
A database an earlier release left `detected` rows in loses them on its first
start, and the lock read goes back to read their blocks again.

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

The `automation/` repo provisions the orchestrator on the blockchain bridging
node. From its root, one command builds the binary, derives the DNA hash from
the latest Holochain deploy result, reuses or creates the watchtower reporter's
HMAC secret and registers it with the worker, writes `bridge-orchestrator.env`
from `config/blockchain-bridging/services.json` (including `WATCHTOWER_*` and
any `BRIDGE_RETENTION_*` overrides), copies the binary over, and restarts
systemd:

```bash
cd automation && make blockchain-bridging-services
```

Edit `bridge-orchestrator.env` by hand only for local setups or a secret
rotation. `automation/scripts/setup-blockchain-bridge-services.sh` and the
`watchtower_reporter` / `retention` blocks in `services.json` hold the knobs
available to operators.

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

### Graceful shutdown

`bridge-orchestrator run` installs handlers for `SIGINT` and `SIGTERM`. On a
signal:

- the request in flight to Holochain or Ethereum finishes, and the link or
  spend a write returns is recorded on its rows;
- no other request starts: no next stage, no next window of the lock read, no
  next cycle;
- the process exits 0.

The cycle ends before its next call, each row at the step it reached. The next
start returns a row left `in_flight` to `queued`, and its reconcile records the
rows a RAVE took when the stop came. A stop waits for at most one request: a
Holochain call, bounded by `HAM_REQUEST_TIMEOUT_SECS`, or an Ethereum request,
which the lock read gives up on after 30 s.

### Signer (run only)

`run` parses these at startup. The deploy record prints every one but the key,
under these names. On `sepolia` each one but the key defaults to TestNet's
value, the Sepolia claim order, and startup logs which ones it took. On
`mainnet` every one is required.

| Variable | Required | Sepolia default |
|----------|----------|-----------------|
| `SIGNER_PRIVATE_KEY` | Yes | -- (TestNet's claim order takes the test signer's key, `TEST_SIGNER_KEY` in `src/Constants.sol`) |
| `ORDER_HASH` | mainnet | `0x5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9` |
| `ORDER_OWNER` | mainnet | `0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6` |
| `ORDERBOOK_ADDRESS` | mainnet | `0xfca89cD12Ba1346b1ac570ed988AB43b812733fe` |
| `TOKEN_ADDRESS` | mainnet | `0xeaC8eEEE9f84F3E3F592e9D8604100eA1b788749` |
| `VAULT_ID` | mainnet | `0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b` |
| `CLAIM_SIGNER` | mainnet | `0x8E72b7568738da52ca3DCd9b24E178127A4E7d37` (the claim order's `valid-signer`: the key's address) |
| `CLAIM_INTERPRETER` | mainnet | `0x8853d126bc23a45b9f807739b6ea0b38ef569005` |
| `CLAIM_STORE` | mainnet | `0x23f77e7bc935503e437166498d7d72f2ea290e1f` |
| `CLAIM_EXPRESSION` | mainnet | `0x0a1369aee76570cc7404492d55a5d1468d5a9b4b` |
| `CLAIM_INPUT_TOKEN` | mainnet | `0x555FA2F68dD9B7dB6c8cA1F03bFc317ce61e9028` |
| `EXPIRY_SECONDS` | No | `604800` (7 days), from 1 to `31536000` (a year) |

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

### Re-queue failed rows

No subcommand does this; go at the database. Stop the service first, and leave
`step` alone — it records real on-chain progress, so resetting it re-creates
links that already exist.

```bash
systemctl stop bridge-orchestrator
cp "$DB_PATH" "$DB_PATH.bak"

python3 -c "
import sqlite3, os
c = sqlite3.connect(os.environ['DB_PATH'])
n = c.execute('''UPDATE work_items
                    SET state='queued', attempts=0, next_retry_at=NULL,
                        error_class=NULL, last_error=NULL,
                        updated_at=strftime('%s','now')
                  WHERE state='failed' ''').rowcount
c.commit(); print('requeued', n)
"

systemctl start bridge-orchestrator
```

Add `AND id IN (...)` to target specific rows. `attempts` must be under
`max_attempts` or the next cycle fails it again. Before you re-queue a row,
check the link it records (`cl_link_hash`, or `br_spend_hash` once it has one):
while a failed row records a live link, that link is never paid, and re-queuing
the row can let the next RAVE pay it. Leave a row whose `last_error` ends
`resolve by hand` failed: check by hand whether its depositor was credited, and
credit it by hand only if not and its link is not live. Also leave failed a row
whose `last_error` is `in transit at the old network's close; it is paid by hand
on the new network`.
Rows left `claimed` or `in_flight` need no action: startup re-queues them.

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
queued ─> claimed ─> in_flight ─┬─> succeeded
    ^                            │
    └──── (transient retry) ─────┤
                                 └─> failed (after max_attempts)
```

- **queued**: a lock in a block with 5 confirmations, ready for the next bridge cycle
- **claimed**: picked up by the single-writer executor
- **in_flight**: actively being processed (Holochain call or on-chain tx)
- **succeeded**: completed successfully
- **failed**: used up its attempts (`max_attempts` = 8), or cannot be processed
  and needs a person (`last_error` says why)

On startup, every item a stop or a crash left `claimed` or `in_flight` goes
back to `queued` with its attempts unchanged. Only a failed cycle counts an
attempt, against the items it had in flight.

S2 and S4 give the RAVE a deposit link only when the row of every lock it
carries records exactly that link and is not failed. A link whose only gap is rows that record
no link yet waits one cycle for reconcile to record them
(`bridge.rave.link_deferred`). Any other deposit link, or one still short a
cycle later, stays parked, `bridge.rave.link_withheld` logs it with the reason,
and each row that records it is failed for a person.
