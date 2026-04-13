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

Delete work items from the SQLite database. Exactly one of the two flags is
required; they are mutually exclusive.

```
bridge-orchestrator clear --non-in-progress
bridge-orchestrator clear --all
```

| Flag | Description |
|------|-------------|
| `--non-in-progress` | Delete only terminal rows (`succeeded`, `failed`) |
| `--all` | Delete every row in `work_items` |

Outputs a JSON object: `{"mode":"non_in_progress","deleted_count":N}`

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
| `DEPOSIT_BATCH_TARGET_KB` | No | `512` |
| `HOLOCHAIN_ADMIN_PORT` | No | `30000` |
| `HOLOCHAIN_APP_PORT` | No | `30001` |
| `HOLOCHAIN_APP_ID` | No | `bridging-app` |
| `HOLOCHAIN_ROLE_NAME` | No | `alliance` |
| `HOLOCHAIN_BRIDGING_AGENT_PUBKEY` | **Yes** | -- |
| `HOLOCHAIN_LANE_DEFINITION` | No | _(none)_ |
| `HOLOCHAIN_UNIT_INDEX` | No | `1` |
| `RUST_LOG` | No | `info` |

Confirmations are not configurable: 15 (mainnet) / 5 (sepolia).

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
