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
        })
    }
}
