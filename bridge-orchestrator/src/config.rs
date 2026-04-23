use alloy::primitives::Address;
use anyhow::{Context, Result};
use clap::ValueEnum;
use holo_hash::{ActionHashB64, AgentPubKeyB64};
use std::env;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Network {
    Mainnet,
    Sepolia,
}

impl FromStr for Network {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "mainnet" => Ok(Network::Mainnet),
            "sepolia" => Ok(Network::Sepolia),
            _ => Err(anyhow::anyhow!("Unknown network: {}", s)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub network: Network,
    pub rpc_url: String,
    pub lock_vault_address: Address,
    pub confirmations: u64,
    pub poll_interval_ms: u64,
    pub bridge_cycle_interval_ms: u64,
    /// Maximum size (in bytes) for a single Holochain link tag.
    /// Holochain's protocol MAX_TAG_SIZE is 1000; we default to 800 to leave
    /// headroom for future overhead.
    pub max_link_tag_bytes: usize,
    /// Target size (in bytes) for the aggregated withdrawal coupons map carried
    /// inside `execute_rave` executor_inputs (not a link tag, so not bound by
    /// MAX_TAG_SIZE).
    pub coupons_target_bytes: usize,
    pub db_path: String,
    pub role_name: String,
    pub app_id: String,
    pub admin_port: u16,
    pub app_port: u16,
    pub bridging_agent_pubkey: AgentPubKeyB64,
    pub lane_definition: Option<ActionHashB64>,
    pub unit_index: u32,
    /// Per-request timeout applied to the Holochain app websocket. Prevents a
    /// slow or hung zome call from blocking the orchestrator indefinitely.
    pub ham_request_timeout_secs: u64,
    /// Initial backoff delay used by the reconnect loop (milliseconds).
    pub ham_reconnect_backoff_initial_ms: u64,
    /// Cap on the reconnect backoff delay (milliseconds).
    pub ham_reconnect_backoff_max_ms: u64,
    /// Number of consecutive failed reconnect attempts before the log level
    /// escalates from `warn` to `error`. The loop keeps retrying forever.
    pub ham_reconnect_escalate_after: u32,
    /// Pause (milliseconds) applied after a cycle fails with a Holochain
    /// source-chain-pressure error (e.g. `"deadline has elapsed"`). The
    /// socket is healthy but the conductor is backpressured, so we back off
    /// before the next cycle instead of hammering it. This is the *base*
    /// value; consecutive pressure errors double the wait up to
    /// [`Config::ham_pressure_cooldown_max_ms`].
    pub ham_pressure_cooldown_ms: u64,
    /// Upper bound (milliseconds) on the escalating source-chain-pressure
    /// cooldown. Once hit, further consecutive pressure errors keep sleeping
    /// at this cap and log severity escalates from `warn!` to `error!` so
    /// operators can alert. The first fully-clean cycle resets the counter.
    pub ham_pressure_cooldown_max_ms: u64,
    /// If a write-bearing zome call inside `run_bridge_cycle` takes longer
    /// than this many milliseconds, the orchestrator ejects the rest of the
    /// cycle instead of stacking more pressure on a slow conductor. The
    /// reconciler advances the skipped stages next cycle. Set to `0` to
    /// disable stage-ejection entirely.
    pub slow_call_threshold_ms: u128,
    /// Optional watchtower reporter configuration. When `None`, the
    /// reporter task is not spawned and the orchestrator runs exactly as
    /// before. All fields must be supplied together for reporting to be
    /// enabled; a partial configuration logs a warning and disables the
    /// reporter (the bridge cycle is never affected).
    pub watchtower: Option<WatchtowerReporterConfig>,
    /// Retention policy for terminal `work_items` rows. Always present
    /// with compact defaults; set `BRIDGE_RETENTION_DISABLED=true` to
    /// skip spawning the retention task entirely.
    pub retention: RetentionConfig,
}

/// Configuration for the in-process retention task that prunes
/// long-lived terminal `work_items` rows. Enabled by default with
/// compact windows; operators tune via `BRIDGE_RETENTION_*` env vars.
///
/// The task runs a single DELETE per eligible state per tick through
/// the writer mutex — brief enough at an hourly cadence not to
/// measurably impact the bridge cycle, and the existing
/// `idx_work_items_state_created` index keeps each DELETE cheap.
#[derive(Debug, Clone)]
pub struct RetentionConfig {
    /// When `false` the retention task is never spawned. Driven by
    /// `BRIDGE_RETENTION_DISABLED=true`.
    pub enabled: bool,
    /// How often the task wakes up to prune. Driven by
    /// `BRIDGE_RETENTION_TICK_MS`.
    pub tick_interval_ms: u64,
    /// Maximum age (seconds) for `state = 'succeeded'` rows before
    /// they're eligible for deletion. Driven by
    /// `BRIDGE_RETENTION_SUCCEEDED_MAX_AGE_S`.
    pub succeeded_max_age_s: u64,
    /// Maximum age (seconds) for `state = 'failed'` rows before
    /// they're eligible for deletion. Typically larger than
    /// `succeeded_max_age_s` because failures are operationally
    /// useful for postmortems. Driven by
    /// `BRIDGE_RETENTION_FAILED_MAX_AGE_S`.
    pub failed_max_age_s: u64,
}

/// Configuration for the optional watchtower reporter task.
///
/// The reporter posts small, DNA-scoped health and throughput snapshots
/// to the watchtower Worker over HTTPS. All required fields must be set
/// together in the environment; absence of any required field fully
/// disables the reporter (logged once at startup).
#[derive(Debug, Clone)]
pub struct WatchtowerReporterConfig {
    /// Full POST URL, e.g. `https://watchtower.unyt.dev/ingest/bridge`.
    pub ingest_url: String,
    /// Per-service observer_id registered in the Worker's `observer_secrets`
    /// table. Example: `bridge-hot-2-mhot`.
    pub observer_id: String,
    /// Hex-encoded HMAC secret shared with the Worker.
    pub hmac_secret_hex: String,
    /// base64url DNA hash (39 bytes, no pad) this bridge orchestrator is
    /// bound to. The dashboard uses this to show the bridge panel on the
    /// matching DNA's Overview page.
    pub dna_b64: String,
    /// How often the reporter task wakes up to collect + post a snapshot.
    pub report_interval_ms: u64,
    /// Schema version sent in the `x-watchtower-schema` header. Kept in
    /// sync with the Worker's expected value.
    pub schema_version: u32,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let network: Network = env::var("NETWORK")
            .unwrap_or_else(|_| "sepolia".to_string())
            .parse()
            .context("Invalid NETWORK value")?;

        let (rpc_url, lock_vault_address, confirmations) = match network {
            Network::Mainnet => {
                let rpc_url =
                    env::var("ETH_RPC_URL").unwrap_or_else(|_| "https://eth.llamarpc.com".into());
                let lock_vault_address = env::var("MAINNET_LOCK_VAULT_ADDRESS")
                    .context("MAINNET_LOCK_VAULT_ADDRESS required")?
                    .parse()
                    .context("Invalid MAINNET_LOCK_VAULT_ADDRESS")?;
                (rpc_url, lock_vault_address, 15)
            }
            Network::Sepolia => {
                let rpc_url = env::var("SEPOLIA_RPC_URL")
                    .unwrap_or_else(|_| "https://1rpc.io/sepolia".into());
                let lock_vault_address = env::var("SEPOLIA_LOCK_VAULT_ADDRESS")
                    .context("SEPOLIA_LOCK_VAULT_ADDRESS required")?
                    .parse()
                    .context("Invalid SEPOLIA_LOCK_VAULT_ADDRESS")?;
                (rpc_url, lock_vault_address, 5)
            }
        };

        let poll_interval_ms = env::var("POLL_INTERVAL_MS")
            .unwrap_or_else(|_| "5000".into())
            .parse()
            .context("Invalid POLL_INTERVAL_MS")?;
        let bridge_cycle_interval_ms = env::var("BRIDGE_CYCLE_INTERVAL_MS")
            .or_else(|_| env::var("COUPON_POLL_INTERVAL_MS"))
            .unwrap_or_else(|_| "180000".into())
            .parse()
            .context("Invalid BRIDGE_CYCLE_INTERVAL_MS")?;
        if env::var("DEPOSIT_BATCH_TARGET_KB").is_ok() {
            tracing::warn!(
                "DEPOSIT_BATCH_TARGET_KB is deprecated and has no effect; use MAX_LINK_TAG_BYTES (link tag cap, default 800) and COUPONS_TARGET_KB (withdrawal coupons aggregate size, default 512 KB) instead"
            );
        }
        let max_link_tag_bytes = env::var("MAX_LINK_TAG_BYTES")
            .unwrap_or_else(|_| "800".into())
            .parse()
            .context("Invalid MAX_LINK_TAG_BYTES")?;
        let coupons_target_kb: u64 = env::var("COUPONS_TARGET_KB")
            .unwrap_or_else(|_| "512".into())
            .parse()
            .context("Invalid COUPONS_TARGET_KB")?;
        let coupons_target_bytes = (coupons_target_kb as usize).saturating_mul(1024);

        let db_path =
            env::var("DB_PATH").unwrap_or_else(|_| "./data/bridge_orchestrator.db".into());
        let admin_port = env::var("HOLOCHAIN_ADMIN_PORT")
            .unwrap_or_else(|_| "30000".into())
            .parse()
            .context("Invalid HOLOCHAIN_ADMIN_PORT")?;
        let app_port = env::var("HOLOCHAIN_APP_PORT")
            .unwrap_or_else(|_| "30001".into())
            .parse()
            .context("Invalid HOLOCHAIN_APP_PORT")?;
        let app_id = env::var("HOLOCHAIN_APP_ID").unwrap_or_else(|_| "bridging-app".into());
        let role_name = env::var("HOLOCHAIN_ROLE_NAME").unwrap_or_else(|_| "alliance".into());
        let bridging_agent_pubkey = AgentPubKeyB64::from_str(
            &env::var("HOLOCHAIN_BRIDGING_AGENT_PUBKEY")
                .context("HOLOCHAIN_BRIDGING_AGENT_PUBKEY required")?,
        )
        .context("Invalid HOLOCHAIN_BRIDGING_AGENT_PUBKEY")?;
        let lane_definition = env::var("HOLOCHAIN_LANE_DEFINITION")
            .ok()
            .and_then(|v| ActionHashB64::from_str(&v).ok());
        let unit_index = env::var("HOLOCHAIN_UNIT_INDEX")
            .unwrap_or_else(|_| "1".into())
            .parse()
            .context("Invalid HOLOCHAIN_UNIT_INDEX")?;
        let ham_request_timeout_secs = env::var("HAM_REQUEST_TIMEOUT_SECS")
            .unwrap_or_else(|_| "120".into())
            .parse()
            .context("Invalid HAM_REQUEST_TIMEOUT_SECS")?;
        let ham_reconnect_backoff_initial_ms = env::var("HAM_RECONNECT_BACKOFF_INITIAL_MS")
            .unwrap_or_else(|_| "1000".into())
            .parse()
            .context("Invalid HAM_RECONNECT_BACKOFF_INITIAL_MS")?;
        let ham_reconnect_backoff_max_ms = env::var("HAM_RECONNECT_BACKOFF_MAX_MS")
            .unwrap_or_else(|_| "30000".into())
            .parse()
            .context("Invalid HAM_RECONNECT_BACKOFF_MAX_MS")?;
        let ham_reconnect_escalate_after = env::var("HAM_RECONNECT_ESCALATE_AFTER")
            .unwrap_or_else(|_| "5".into())
            .parse()
            .context("Invalid HAM_RECONNECT_ESCALATE_AFTER")?;
        let ham_pressure_cooldown_ms = env::var("HAM_PRESSURE_COOLDOWN_MS")
            .unwrap_or_else(|_| "30000".into())
            .parse()
            .context("Invalid HAM_PRESSURE_COOLDOWN_MS")?;
        let ham_pressure_cooldown_max_ms = env::var("HAM_PRESSURE_COOLDOWN_MAX_MS")
            .unwrap_or_else(|_| "90000".into())
            .parse()
            .context("Invalid HAM_PRESSURE_COOLDOWN_MAX_MS")?;
        let slow_call_threshold_ms = env::var("SLOW_CALL_THRESHOLD_MS")
            .unwrap_or_else(|_| "35000".into())
            .parse()
            .context("Invalid SLOW_CALL_THRESHOLD_MS")?;

        let watchtower = WatchtowerReporterConfig::from_env();
        let retention = RetentionConfig::from_env()?;

        Ok(Self {
            network,
            rpc_url,
            lock_vault_address,
            confirmations,
            poll_interval_ms,
            bridge_cycle_interval_ms,
            max_link_tag_bytes,
            coupons_target_bytes,
            db_path,
            role_name,
            app_id,
            admin_port,
            app_port,
            bridging_agent_pubkey,
            lane_definition,
            unit_index,
            ham_request_timeout_secs,
            ham_reconnect_backoff_initial_ms,
            ham_reconnect_backoff_max_ms,
            ham_reconnect_escalate_after,
            ham_pressure_cooldown_ms,
            ham_pressure_cooldown_max_ms,
            slow_call_threshold_ms,
            watchtower,
            retention,
        })
    }
}

impl RetentionConfig {
    /// How often the retention task wakes up. Hourly is plenty —
    /// rows only accumulate at the pace the bridge cycle terminates
    /// items, and keeping the cadence low keeps writer-mutex hold
    /// time tiny relative to the 60s reporter and the bridge cycle.
    pub const DEFAULT_TICK_INTERVAL_MS: u64 = 3_600_000;
    /// Compact default: keep succeeded rows for 7 days. Enough to
    /// debug the most recent week, which is where operator attention
    /// lives in practice.
    pub const DEFAULT_SUCCEEDED_MAX_AGE_S: u64 = 7 * 24 * 60 * 60;
    /// Compact default: keep failed rows for 30 days. Failures are
    /// forensic — you want them around long enough to correlate with
    /// downstream incident reviews.
    pub const DEFAULT_FAILED_MAX_AGE_S: u64 = 30 * 24 * 60 * 60;

    /// Read retention config from env. Infallible modulo malformed
    /// numbers; unset variables fall back to compact defaults above.
    pub fn from_env() -> Result<Self> {
        let enabled = !env::var("BRIDGE_RETENTION_DISABLED")
            .ok()
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);

        let tick_interval_ms = env::var("BRIDGE_RETENTION_TICK_MS")
            .ok()
            .map(|v| v.parse::<u64>().context("Invalid BRIDGE_RETENTION_TICK_MS"))
            .transpose()?
            .unwrap_or(Self::DEFAULT_TICK_INTERVAL_MS);

        let succeeded_max_age_s = env::var("BRIDGE_RETENTION_SUCCEEDED_MAX_AGE_S")
            .ok()
            .map(|v| {
                v.parse::<u64>()
                    .context("Invalid BRIDGE_RETENTION_SUCCEEDED_MAX_AGE_S")
            })
            .transpose()?
            .unwrap_or(Self::DEFAULT_SUCCEEDED_MAX_AGE_S);

        let failed_max_age_s = env::var("BRIDGE_RETENTION_FAILED_MAX_AGE_S")
            .ok()
            .map(|v| {
                v.parse::<u64>()
                    .context("Invalid BRIDGE_RETENTION_FAILED_MAX_AGE_S")
            })
            .transpose()?
            .unwrap_or(Self::DEFAULT_FAILED_MAX_AGE_S);

        Ok(Self {
            enabled,
            tick_interval_ms,
            succeeded_max_age_s,
            failed_max_age_s,
        })
    }
}

impl WatchtowerReporterConfig {
    /// Default reporter cadence (1 minute). Tuned to produce ~1 hourly
    /// bucket's worth of data points while keeping load negligible.
    pub const DEFAULT_REPORT_INTERVAL_MS: u64 = 60_000;

    /// Schema version of the bridge-reporter payload. Bump in lockstep
    /// with the Worker's expected value when the payload shape changes.
    pub const SCHEMA_VERSION: u32 = 1;

    /// Read the reporter configuration from process environment. Returns
    /// `None` if the reporter is fully unconfigured (all required vars
    /// absent), or logs a warning and returns `None` if only a subset is
    /// set. The bridge cycle never depends on this, so any error here is
    /// non-fatal.
    pub fn from_env() -> Option<Self> {
        let required = [
            (
                "WATCHTOWER_INGEST_URL",
                env::var("WATCHTOWER_INGEST_URL").ok(),
            ),
            (
                "WATCHTOWER_OBSERVER_ID",
                env::var("WATCHTOWER_OBSERVER_ID").ok(),
            ),
            (
                "WATCHTOWER_HMAC_SECRET_HEX",
                env::var("WATCHTOWER_HMAC_SECRET_HEX").ok(),
            ),
            ("WATCHTOWER_DNA_B64", env::var("WATCHTOWER_DNA_B64").ok()),
        ];

        let any_set = required.iter().any(|(_, v)| v.is_some());
        let all_set = required.iter().all(|(_, v)| v.is_some());

        if !any_set {
            return None;
        }
        if !all_set {
            let missing: Vec<&str> = required
                .iter()
                .filter_map(|(k, v)| if v.is_none() { Some(*k) } else { None })
                .collect();
            tracing::warn!(
                event = "watchtower_reporter.misconfigured",
                missing = ?missing,
                "watchtower reporter disabled: partial configuration"
            );
            return None;
        }

        let report_interval_ms = env::var("WATCHTOWER_REPORT_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(Self::DEFAULT_REPORT_INTERVAL_MS);

        Some(Self {
            ingest_url: required[0].1.clone().unwrap(),
            observer_id: required[1].1.clone().unwrap(),
            hmac_secret_hex: required[2].1.clone().unwrap(),
            dna_b64: normalize_dna_b64(required[3].1.as_deref().unwrap()),
            report_interval_ms,
            schema_version: Self::SCHEMA_VERSION,
        })
    }
}

/// Strip a single leading `u` multibase prefix (base64url) so the reporter's
/// stored DNA matches the 52-char form the Holochain observer uses across
/// the rest of the Watchtower schema. Both forms encode the same hash;
/// normalizing here keeps wire payloads, D1 rows, and URLs aligned
/// regardless of what the operator pastes into `WATCHTOWER_DNA_B64`.
fn normalize_dna_b64(raw: &str) -> String {
    raw.strip_prefix('u').unwrap_or(raw).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_dna_b64_strips_leading_u() {
        assert_eq!(
            normalize_dna_b64("uhC0kYoBhEs3GyOWslej78VfMRmSSdc2TXsRQmqFn5b3v8jl58Kkj"),
            "hC0kYoBhEs3GyOWslej78VfMRmSSdc2TXsRQmqFn5b3v8jl58Kkj"
        );
    }

    #[test]
    fn normalize_dna_b64_passes_through_without_u() {
        assert_eq!(
            normalize_dna_b64("hC0kYoBhEs3GyOWslej78VfMRmSSdc2TXsRQmqFn5b3v8jl58Kkj"),
            "hC0kYoBhEs3GyOWslej78VfMRmSSdc2TXsRQmqFn5b3v8jl58Kkj"
        );
    }

    #[test]
    fn normalize_dna_b64_only_strips_one_u() {
        assert_eq!(normalize_dna_b64("uuhC0k"), "uhC0k");
    }
}
