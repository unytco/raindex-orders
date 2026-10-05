use alloy::primitives::Address;
use anyhow::{Context, Result};
use clap::ValueEnum;
use holo_hash::AgentPubKeyB64;
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

impl Network {
    pub fn name(self) -> &'static str {
        match self {
            Network::Mainnet => "mainnet",
            Network::Sepolia => "sepolia",
        }
    }

    pub fn chain_id(self) -> u64 {
        match self {
            Network::Mainnet => 1,
            Network::Sepolia => 11_155_111,
        }
    }

    pub fn rpc_url_var(self) -> &'static str {
        match self {
            Network::Mainnet => "ETH_RPC_URL",
            Network::Sepolia => "SEPOLIA_RPC_URL",
        }
    }

    pub fn lock_vault_var(self) -> &'static str {
        match self {
            Network::Mainnet => "MAINNET_LOCK_VAULT_ADDRESS",
            Network::Sepolia => "SEPOLIA_LOCK_VAULT_ADDRESS",
        }
    }

    fn confirmations(self) -> u64 {
        match self {
            Network::Mainnet => 15,
            Network::Sepolia => 5,
        }
    }

    fn other(self) -> Network {
        match self {
            Network::Mainnet => Network::Sepolia,
            Network::Sepolia => Network::Mainnet,
        }
    }
}

/// TestNet's values: a sepolia run takes each one it is not given. Mainnet has none,
/// so a mainnet run names every value it lacks.
const SEPOLIA_DEFAULTS: [(&str, &str); 12] = [
    ("SEPOLIA_RPC_URL", "https://1rpc.io/sepolia"),
    (
        "SEPOLIA_LOCK_VAULT_ADDRESS",
        "0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6",
    ),
    (
        "TOKEN_ADDRESS",
        "0xeaC8eEEE9f84F3E3F592e9D8604100eA1b788749",
    ),
    (
        "ORDERBOOK_ADDRESS",
        "0xfca89cD12Ba1346b1ac570ed988AB43b812733fe",
    ),
    (
        "VAULT_ID",
        "0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b",
    ),
    (
        "ORDER_HASH",
        "0x5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9",
    ),
    ("ORDER_OWNER", "0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6"),
    ("CLAIM_SIGNER", "0x8E72b7568738da52ca3DCd9b24E178127A4E7d37"),
    (
        "CLAIM_INTERPRETER",
        "0x8853d126bc23a45b9f807739b6ea0b38ef569005",
    ),
    ("CLAIM_STORE", "0x23f77e7bc935503e437166498d7d72f2ea290e1f"),
    (
        "CLAIM_EXPRESSION",
        "0x0a1369aee76570cc7404492d55a5d1468d5a9b4b",
    ),
    (
        "CLAIM_INPUT_TOKEN",
        "0x555FA2F68dD9B7dB6c8cA1F03bFc317ce61e9028",
    ),
];

/// `setting`, with TestNet's value for each network value a sepolia run is not
/// given. An empty value counts as not given.
pub(crate) fn with_network_defaults(
    network: Network,
    setting: impl Fn(&str) -> Option<String>,
) -> impl Fn(&str) -> Option<String> {
    move |key| {
        setting(key)
            .filter(|value| !value.is_empty())
            .or_else(|| testnet_default(network, key).map(str::to_string))
    }
}

fn testnet_default(network: Network, key: &str) -> Option<&'static str> {
    match network {
        Network::Sepolia => SEPOLIA_DEFAULTS
            .iter()
            .find(|(default_key, _)| *default_key == key)
            .map(|(_, value)| *value),
        Network::Mainnet => None,
    }
}

/// Logs which of `keys` a run takes from TestNet's defaults.
pub(crate) fn log_testnet_defaults(
    network: Network,
    setting: impl Fn(&str) -> Option<String>,
    keys: &[&str],
) {
    let defaulted: Vec<&str> = keys
        .iter()
        .copied()
        .filter(|key| {
            testnet_default(network, key).is_some()
                && setting(key).filter(|value| !value.is_empty()).is_none()
        })
        .collect();
    if !defaulted.is_empty() {
        tracing::info!(
            event = "config.testnet_defaults",
            defaults = ?defaulted,
            "using TestNet's values for {}",
            defaulted.join(", ")
        );
    }
}

/// The chain the bridge watches and signs for. A run has none under NETWORK=none,
/// and then makes no Ethereum call at all.
#[derive(Debug, Clone, PartialEq)]
pub struct Ethereum {
    pub network: Network,
    pub rpc_url: String,
    pub lock_vault_address: Address,
    pub confirmations: u64,
}

/// The Ethereum variables `setting` holds a value for, which NETWORK=none ignores.
pub fn ignored_without_ethereum(setting: impl Fn(&str) -> Option<String>) -> Vec<&'static str> {
    [Network::Sepolia, Network::Mainnet]
        .into_iter()
        .flat_map(|network| [network.rpc_url_var(), network.lock_vault_var()])
        .chain(crate::signer::variables())
        .filter(|key| setting(key).is_some_and(|value| !value.is_empty()))
        .collect()
}

/// The network `NETWORK` names, sepolia when it is unset or empty.
fn network_from(setting: &impl Fn(&str) -> Option<String>) -> Result<Network> {
    match setting("NETWORK").filter(|raw| !raw.is_empty()) {
        None => {
            tracing::info!(
                event = "config.testnet_defaults",
                "NETWORK is unset: sepolia"
            );
            Ok(Network::Sepolia)
        }
        Some(raw) => raw
            .parse::<Network>()
            .map_err(|_| anyhow::anyhow!("NETWORK={raw} is not sepolia, mainnet or none")),
    }
}

fn ethereum_settings(setting: impl Fn(&str) -> Option<String>) -> Result<Option<Ethereum>> {
    if setting("NETWORK").is_some_and(|raw| raw.eq_ignore_ascii_case("none")) {
        return Ok(None);
    }
    let network = network_from(&setting)?;

    let mut faults = Vec::new();
    let other = network.other();
    for key in [other.rpc_url_var(), other.lock_vault_var()] {
        if setting(key).is_some_and(|value| !value.is_empty()) {
            faults.push(format!(
                "{key} is a {} variable and NETWORK is {}: unset it",
                other.name(),
                network.name()
            ));
        }
    }

    let own = [network.rpc_url_var(), network.lock_vault_var()];
    log_testnet_defaults(network, &setting, &own);
    let setting = with_network_defaults(network, setting);

    let rpc_url = setting(network.rpc_url_var());
    if rpc_url.is_none() {
        faults.push(format!(
            "{} is required for NETWORK={}",
            network.rpc_url_var(),
            network.name()
        ));
    }

    let vault_key = network.lock_vault_var();
    let lock_vault_address = match setting(vault_key) {
        None => {
            faults.push(format!(
                "{vault_key} is required for NETWORK={}",
                network.name()
            ));
            None
        }
        Some(raw) => match raw.parse::<Address>() {
            Ok(address) => Some(address),
            Err(_) => {
                faults.push(format!("{vault_key}={raw} is not an address"));
                None
            }
        },
    };

    match (rpc_url, lock_vault_address) {
        (Some(rpc_url), Some(lock_vault_address)) if faults.is_empty() => Ok(Some(Ethereum {
            network,
            rpc_url,
            lock_vault_address,
            confirmations: network.confirmations(),
        })),
        _ => anyhow::bail!("{}", faults.join("; ")),
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub ethereum: Option<Ethereum>,
    pub poll_interval_ms: u64,
    pub bridge_cycle_interval_ms: u64,
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
    /// Conductor config path — its `keystore.connection_url` is read to sign
    /// zome calls via lair (no cap grant). Defaults to the fleet path.
    pub conductor_config: String,
    /// Lair passphrase file, read to unlock the keystore. Defaults to the
    /// fleet path.
    pub lair_passphrase_file: String,
    pub bridging_agent_pubkey: AgentPubKeyB64,
    pub hot_unit_index: u32,
    /// Per-request timeout applied to the Holochain app websocket. Prevents a
    /// slow or hung zome call from blocking the orchestrator indefinitely.
    pub ham_request_timeout_secs: u64,
    pub ham_reconnect_backoff_initial_ms: u64,
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
    /// If a measured zome call inside `run_bridge_cycle` takes longer
    /// than this many milliseconds, the orchestrator ejects the rest of the
    /// cycle instead of stacking more pressure on a slow conductor. The
    /// measured calls are each stage's write and the ledger read that sizes
    /// the spend tag, so a slow read ends a cycle before anything is
    /// written. Set to `0` to disable stage-ejection entirely.
    pub slow_call_threshold_ms: u128,
    /// Optional per-cycle cap on the number of parked links fed into an
    /// `execute_rave` call. Applied independently to the S2 credit-limit
    /// RAVE (`cl_links`) and the S4 bridging RAVE (pooled
    /// deposits + selected withdrawals). `None` means no cap (current
    /// behavior). `Some(n)` truncates each RAVE's input Vec to at most
    /// `n` entries; deferred links stay live server-side and are picked
    /// up by the next cycle. Intended as a mitigation when `execute_rave`
    /// hangs correlate with large batch sizes; the existing withdrawal
    /// `coupons_target_bytes` cap remains in force on top.
    pub rave_max_links: Option<usize>,
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
        let ethereum = ethereum_settings(|key| env::var(key).ok())?;

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
                "DEPOSIT_BATCH_TARGET_KB is deprecated and has no effect; use MAX_LINK_TAG_BYTES (link tag cap, default 850) and COUPONS_TARGET_KB (withdrawal coupons aggregate size, default 512 KB) instead"
            );
        }
        let max_link_tag_bytes = capped_link_tag_bytes(
            env::var("MAX_LINK_TAG_BYTES")
                .unwrap_or_else(|_| LINK_TAG_BYTES_DEFAULT.to_string())
                .parse()
                .context("Invalid MAX_LINK_TAG_BYTES")?,
        );
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
        let conductor_config = env::var("CONDUCTOR_CONFIG")
            .unwrap_or_else(|_| "/etc/holochain/conductor-config.yaml".into());
        let lair_passphrase_file = env::var("LAIR_PASSPHRASE_FILE")
            .unwrap_or_else(|_| "/var/lib/holochain/lair-passphrase".into());
        let bridging_agent_pubkey = AgentPubKeyB64::from_str(
            &env::var("HOLOCHAIN_BRIDGING_AGENT_PUBKEY")
                .context("HOLOCHAIN_BRIDGING_AGENT_PUBKEY required")?,
        )
        .context("Invalid HOLOCHAIN_BRIDGING_AGENT_PUBKEY")?;
        let hot_unit_index = hot_unit_index(|key| env::var(key).ok())?;
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

        // `RAVE_MAX_LINKS`: unset → no cap; explicit `0` is normalized to
        // `None` with a one-time startup warn so operators don't have to
        // remove the env entirely to disable the feature. Any positive
        // integer becomes `Some(n)` and truncates each `execute_rave`
        // input Vec. Bad input fails fast at startup.
        let rave_max_links = match env::var("RAVE_MAX_LINKS") {
            Ok(raw) => {
                let n: usize = raw
                    .trim()
                    .parse()
                    .context("Invalid RAVE_MAX_LINKS (expected a non-negative integer)")?;
                if n == 0 {
                    tracing::warn!(
                        event = "config.rave_max_links_disabled",
                        "RAVE_MAX_LINKS=0 treated as disabled (no cap)"
                    );
                    None
                } else {
                    Some(n)
                }
            }
            Err(_) => None,
        };

        let watchtower = WatchtowerReporterConfig::from_env();
        let retention = RetentionConfig::from_env()?;

        Ok(Self {
            ethereum,
            poll_interval_ms,
            bridge_cycle_interval_ms,
            max_link_tag_bytes,
            coupons_target_bytes,
            db_path,
            role_name,
            app_id,
            admin_port,
            app_port,
            conductor_config,
            lair_passphrase_file,
            bridging_agent_pubkey,
            hot_unit_index,
            ham_request_timeout_secs,
            ham_reconnect_backoff_initial_ms,
            ham_reconnect_backoff_max_ms,
            ham_reconnect_escalate_after,
            ham_pressure_cooldown_ms,
            ham_pressure_cooldown_max_ms,
            slow_call_threshold_ms,
            rave_max_links,
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

/// One real deposit proof, plus room for the agent's ledger to grow under the
/// cap, has to fit on the widest network we run: a cycle whose single deposit
/// does not fit makes no progress at all.
pub(crate) const LINK_TAG_BYTES_DEFAULT: usize = 850;

/// Holochain refuses a link tag over its own MAX_TAG_SIZE of 1000. The last 100
/// bytes are left to no configuration at all: they cover the agent's ledger
/// moving between the cycle reading it and the zome writing it, which is the
/// one part of a tag's size the estimate cannot see.
pub(crate) const LINK_TAG_BYTES_CEILING: usize = 900;
const _: () = assert!(LINK_TAG_BYTES_CEILING < 1000);

/// A tag holds a deposit proof, the agent's ledger and the network's lanes, and
/// the smallest of those measured on its own is over 400 bytes. A cap under
/// this is a typo, not a policy, and it would hold back every deposit.
const LINK_TAG_BYTES_FLOOR: usize = 600;

fn capped_link_tag_bytes(configured: usize) -> usize {
    if !(LINK_TAG_BYTES_FLOOR..=LINK_TAG_BYTES_CEILING).contains(&configured) {
        tracing::warn!(
            "MAX_LINK_TAG_BYTES={configured} is outside {LINK_TAG_BYTES_FLOOR}..={LINK_TAG_BYTES_CEILING}, using the nearer bound"
        );
    }
    configured.clamp(LINK_TAG_BYTES_FLOOR, LINK_TAG_BYTES_CEILING)
}

/// Refused rather than ignored, so a deploy that still sets one stops instead of
/// running without it.
const RETIRED_SETTINGS: [(&str, &str); 2] = [
    (
        "HOLOCHAIN_LANE_DEFINITION",
        "remove it. The bridge finds its lane from HOLOCHAIN_BRIDGING_AGENT_PUBKEY and HOT_UNIT_INDEX",
    ),
    ("HOLOCHAIN_UNIT_INDEX", "rename it to HOT_UNIT_INDEX"),
];

fn hot_unit_index(setting: impl Fn(&str) -> Option<String>) -> Result<u32> {
    let retired: Vec<String> = RETIRED_SETTINGS
        .iter()
        .filter(|(key, _)| setting(key).is_some())
        .map(|(key, replacement)| format!("{key} is retired: {replacement}"))
        .collect();
    if !retired.is_empty() {
        anyhow::bail!("{}", retired.join("; "));
    }
    setting("HOT_UNIT_INDEX")
        .as_deref()
        .unwrap_or("1")
        .parse()
        .context("Invalid HOT_UNIT_INDEX")
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
pub(crate) fn test_settings(set: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let set: Vec<(String, String)> = set
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    move |key| {
        set.iter()
            .find(|(set_key, _)| set_key == key)
            .map(|(_, value)| value.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_tag_bytes_cannot_be_raised_above_the_ceiling() {
        assert_eq!(capped_link_tag_bytes(1000), LINK_TAG_BYTES_CEILING);
        assert_eq!(
            capped_link_tag_bytes(LINK_TAG_BYTES_CEILING + 1),
            LINK_TAG_BYTES_CEILING
        );
        assert_eq!(capped_link_tag_bytes(LINK_TAG_BYTES_DEFAULT), 850);
        assert_eq!(capped_link_tag_bytes(700), 700);
    }

    #[test]
    fn link_tag_bytes_cannot_be_lowered_below_a_single_proof() {
        assert_eq!(capped_link_tag_bytes(0), LINK_TAG_BYTES_FLOOR);
        assert_eq!(capped_link_tag_bytes(80), LINK_TAG_BYTES_FLOOR);
        assert_eq!(
            capped_link_tag_bytes(LINK_TAG_BYTES_FLOOR - 1),
            LINK_TAG_BYTES_FLOOR
        );
    }

    #[test]
    fn each_retired_setting_stops_the_orchestrator_naming_its_replacement() {
        for (key, replacement) in [
            (
                "HOLOCHAIN_LANE_DEFINITION",
                "HOLOCHAIN_BRIDGING_AGENT_PUBKEY",
            ),
            ("HOLOCHAIN_UNIT_INDEX", "HOT_UNIT_INDEX"),
        ] {
            for value in ["1", ""] {
                let err = hot_unit_index(test_settings(&[("HOT_UNIT_INDEX", "1"), (key, value)]))
                    .expect_err("a retired setting would otherwise be dropped in silence");
                let message = format!("{err:#}");
                assert!(
                    message.starts_with(&format!("{key} is retired")),
                    "{message}"
                );
                assert!(message.contains(replacement), "{message}");
            }
        }

        let err = hot_unit_index(test_settings(&[
            ("HOLOCHAIN_LANE_DEFINITION", ""),
            ("HOLOCHAIN_UNIT_INDEX", "1"),
        ]))
        .unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains("HOLOCHAIN_LANE_DEFINITION is retired")
                && message.contains("HOLOCHAIN_UNIT_INDEX is retired"),
            "one restart names every retired setting: {message}"
        );
    }

    #[test]
    fn hot_unit_index_parses_and_defaults_to_one() {
        assert_eq!(hot_unit_index(test_settings(&[])).unwrap(), 1);
        assert_eq!(
            hot_unit_index(test_settings(&[("HOT_UNIT_INDEX", "3")])).unwrap(),
            3
        );
        let err = hot_unit_index(test_settings(&[("HOT_UNIT_INDEX", "hot")])).unwrap_err();
        assert!(
            format!("{err:#}").contains("Invalid HOT_UNIT_INDEX"),
            "{err:#}"
        );
    }

    const VAULT: &str = "0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6";

    fn network_refusal(set: &[(&str, &str)]) -> String {
        format!("{:#}", ethereum_settings(test_settings(set)).unwrap_err())
    }

    fn chain(set: &[(&str, &str)]) -> Ethereum {
        ethereum_settings(test_settings(set))
            .unwrap()
            .expect("the settings name no chain")
    }

    #[test]
    fn each_network_reads_its_own_rpc_url_and_vault() {
        let sepolia = chain(&[
            ("NETWORK", "sepolia"),
            ("SEPOLIA_RPC_URL", "https://sepolia.rpc.test"),
            ("SEPOLIA_LOCK_VAULT_ADDRESS", VAULT),
        ]);
        assert_eq!(
            sepolia,
            Ethereum {
                network: Network::Sepolia,
                rpc_url: "https://sepolia.rpc.test".to_string(),
                lock_vault_address: VAULT.parse().unwrap(),
                confirmations: 5,
            }
        );

        let mainnet = chain(&[
            ("NETWORK", "mainnet"),
            ("ETH_RPC_URL", "https://eth.rpc.test"),
            ("MAINNET_LOCK_VAULT_ADDRESS", VAULT),
        ]);
        assert_eq!(mainnet.network, Network::Mainnet);
        assert_eq!(mainnet.rpc_url, "https://eth.rpc.test");
        assert_eq!(mainnet.confirmations, 15);
    }

    #[test]
    fn an_unset_network_is_sepolia_with_testnet_values() {
        for unset in [vec![], vec![("NETWORK", "")]] {
            assert_eq!(
                chain(&unset),
                Ethereum {
                    network: Network::Sepolia,
                    rpc_url: "https://1rpc.io/sepolia".to_string(),
                    lock_vault_address: VAULT.parse().unwrap(),
                    confirmations: 5,
                }
            );
        }
        assert_eq!(
            network_refusal(&[("NETWORK", "goerli")]),
            "NETWORK=goerli is not sepolia, mainnet or none"
        );
    }

    #[test]
    fn a_sepolia_run_still_refuses_mainnet_variables() {
        assert_eq!(
            network_refusal(&[("ETH_RPC_URL", "https://eth.rpc.test")]),
            "ETH_RPC_URL is a mainnet variable and NETWORK is sepolia: unset it"
        );
    }

    #[test]
    fn mainnet_without_its_rpc_url_or_vault_is_refused_with_no_fallback() {
        assert_eq!(
            network_refusal(&[("NETWORK", "mainnet")]),
            "ETH_RPC_URL is required for NETWORK=mainnet; MAINNET_LOCK_VAULT_ADDRESS is required for NETWORK=mainnet"
        );
        assert_eq!(
            network_refusal(&[("NETWORK", "mainnet"), ("ETH_RPC_URL", "")]),
            "ETH_RPC_URL is required for NETWORK=mainnet; MAINNET_LOCK_VAULT_ADDRESS is required for NETWORK=mainnet"
        );
        assert_eq!(
            network_refusal(&[
                ("NETWORK", "sepolia"),
                ("SEPOLIA_LOCK_VAULT_ADDRESS", "0xE3E0"),
            ]),
            "SEPOLIA_LOCK_VAULT_ADDRESS=0xE3E0 is not an address"
        );
    }

    #[test]
    fn each_variable_of_the_other_network_is_refused() {
        for (network, own, other) in [
            (
                "mainnet",
                [
                    ("ETH_RPC_URL", "https://eth.rpc.test"),
                    ("MAINNET_LOCK_VAULT_ADDRESS", VAULT),
                ],
                ["SEPOLIA_RPC_URL", "SEPOLIA_LOCK_VAULT_ADDRESS"],
            ),
            (
                "sepolia",
                [
                    ("SEPOLIA_RPC_URL", "https://sepolia.rpc.test"),
                    ("SEPOLIA_LOCK_VAULT_ADDRESS", VAULT),
                ],
                ["ETH_RPC_URL", "MAINNET_LOCK_VAULT_ADDRESS"],
            ),
        ] {
            for key in other {
                let mut set = vec![("NETWORK", network)];
                set.extend(own);
                set.push((key, "0x1"));
                let message = network_refusal(&set);
                assert_eq!(
                    message,
                    format!(
                        "{key} is a {} variable and NETWORK is {network}: unset it",
                        if network == "mainnet" {
                            "sepolia"
                        } else {
                            "mainnet"
                        }
                    )
                );
            }
        }
    }

    #[test]
    fn an_empty_variable_of_the_other_network_counts_as_unset() {
        let mainnet = chain(&[
            ("NETWORK", "mainnet"),
            ("ETH_RPC_URL", "https://eth.rpc.test"),
            ("MAINNET_LOCK_VAULT_ADDRESS", VAULT),
            ("SEPOLIA_RPC_URL", ""),
            ("SEPOLIA_LOCK_VAULT_ADDRESS", ""),
        ]);
        assert_eq!(mainnet.network, Network::Mainnet);
        assert_eq!(mainnet.rpc_url, "https://eth.rpc.test");

        let sepolia = chain(&[("ETH_RPC_URL", ""), ("MAINNET_LOCK_VAULT_ADDRESS", "")]);
        assert_eq!(sepolia.network, Network::Sepolia);
        assert_eq!(sepolia.rpc_url, "https://1rpc.io/sepolia");
    }

    #[test]
    fn network_none_names_no_chain_and_ignores_every_chain_variable() {
        let set = [
            ("NETWORK", "none"),
            ("SEPOLIA_RPC_URL", "http://127.0.0.1:8545"),
            (
                "SEPOLIA_LOCK_VAULT_ADDRESS",
                "0x0000000000000000000000000000000000000000",
            ),
            ("ETH_RPC_URL", "https://eth.rpc.test"),
            ("ORDER_HASH", ""),
            ("SIGNER_PRIVATE_KEY", "0xabc"),
        ];

        for network in ["none", "NONE"] {
            let mut with = set.to_vec();
            with[0] = ("NETWORK", network);
            assert_eq!(ethereum_settings(test_settings(&with)).unwrap(), None);
        }
        assert_eq!(
            ignored_without_ethereum(test_settings(&set)),
            [
                "SEPOLIA_RPC_URL",
                "SEPOLIA_LOCK_VAULT_ADDRESS",
                "ETH_RPC_URL",
                "SIGNER_PRIVATE_KEY"
            ]
        );
    }

    #[test]
    fn only_an_explicit_none_turns_ethereum_off() {
        assert_eq!(chain(&[]).network, Network::Sepolia);
        assert_eq!(chain(&[("NETWORK", "")]).network, Network::Sepolia);
        assert_eq!(
            network_refusal(&[("NETWORK", "off")]),
            "NETWORK=off is not sepolia, mainnet or none"
        );
    }

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
