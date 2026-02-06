use alloy::primitives::Address;
use anyhow::{Context, Result};
use holo_hash::{ActionHashB64, AgentPubKeyB64, DnaHashB64};
use std::env;
use std::str::FromStr;

/// Network type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Network-specific configuration
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub chain_id: u64,
    pub rpc_url: String,
    pub ws_url: Option<String>,
    pub lock_vault_address: Address,
    pub confirmations: u64,
}

/// Holochain conductor and transactor config (optional; used to call create_parked_link)
#[derive(Debug, Clone)]
pub struct HolochainConfig {
    pub admin_port: u16,
    pub app_port: u16,
    pub app_id: String,
    /// DNA hash (holochain base64, e.g. u...).
    pub dna_hash: DnaHashB64,
    /// Agent pubkey (holochain base64, e.g. u...).
    pub agent_pubkey: AgentPubKeyB64,
    pub bridging_agent_pubkey: AgentPubKeyB64,
    pub credit_limit_ea_id: ActionHashB64,
    pub unit_index: u32,
}

/// Application configuration
#[derive(Debug, Clone)]
pub struct Config {
    pub network: Network,
    pub network_config: NetworkConfig,
    pub signer_private_key: Option<String>,
    pub db_path: String,
    pub poll_interval_ms: u64,
    pub holochain: Option<HolochainConfig>,
}

impl Config {
    /// Load configuration from environment variables
    pub fn from_env() -> Result<Self> {
        // Load .env file if present
        dotenvy::dotenv().ok();

        let network: Network = env::var("NETWORK")
            .unwrap_or_else(|_| "sepolia".to_string())
            .parse()
            .context("Invalid NETWORK value")?;

        let network_config = match network {
            Network::Mainnet => {
                let rpc_url = env::var("ETH_RPC_URL")
                    .unwrap_or_else(|_| "https://eth.llamarpc.com".to_string());
                let ws_url = env::var("ETH_WS_URL").ok();
                let lock_vault_address = env::var("MAINNET_LOCK_VAULT_ADDRESS")
                    .context("MAINNET_LOCK_VAULT_ADDRESS required for mainnet")?
                    .parse()
                    .context("Invalid MAINNET_LOCK_VAULT_ADDRESS")?;

                NetworkConfig {
                    chain_id: 1,
                    rpc_url,
                    ws_url,
                    lock_vault_address,
                    confirmations: 15,
                }
            }
            Network::Sepolia => {
                let rpc_url = env::var("SEPOLIA_RPC_URL")
                    .unwrap_or_else(|_| "https://1rpc.io/sepolia".to_string());
                let ws_url = env::var("SEPOLIA_WS_URL").ok();
                let lock_vault_address = env::var("SEPOLIA_LOCK_VAULT_ADDRESS")
                    .context("SEPOLIA_LOCK_VAULT_ADDRESS required for sepolia")?
                    .parse()
                    .context("Invalid SEPOLIA_LOCK_VAULT_ADDRESS")?;

                NetworkConfig {
                    chain_id: 11155111,
                    rpc_url,
                    ws_url,
                    lock_vault_address,
                    confirmations: 5, // Faster for testing
                }
            }
        };

        let signer_private_key = env::var("SIGNER_PRIVATE_KEY").ok();

        let db_path = env::var("DB_PATH").unwrap_or_else(|_| "./data/locks.db".to_string());

        let poll_interval_ms: u64 = env::var("POLL_INTERVAL_MS")
            .unwrap_or_else(|_| "5000".to_string())
            .parse()
            .context("Invalid POLL_INTERVAL_MS")?;

        let holochain = {
            let admin_port = env::var("HOLOCHAIN_ADMIN_PORT")
                .ok()
                .unwrap_or("30000".to_string());
            let app_port = env::var("HOLOCHAIN_APP_PORT")
                .ok()
                .unwrap_or("30001".to_string());
            let app_id = env::var("HOLOCHAIN_APP_ID")
                .ok()
                .unwrap_or("bridging-app".to_string());
            let dna_hash = env::var("HOLOCHAIN_DNA_HASH").ok();
            let agent_pubkey = env::var("HOLOCHAIN_AGENT_PUBKEY").ok();
            let bridging_agent_pubkey = env::var("HOLOCHAIN_BRIDGING_AGENT_PUBKEY").ok();
            let credit_limit_ea_id = env::var("HOLOCHAIN_CREDIT_LIMIT_EA_ID").ok();
            let unit_index = env::var("HOLOCHAIN_UNIT_INDEX").ok();

            let all_present = [
                ("HOLOCHAIN_ADMIN_PORT", !admin_port.is_empty()),
                ("HOLOCHAIN_APP_PORT", !app_port.is_empty()),
                ("HOLOCHAIN_APP_ID", !app_id.is_empty()),
                ("HOLOCHAIN_DNA_HASH", dna_hash.is_some()),
                ("HOLOCHAIN_AGENT_PUBKEY", agent_pubkey.is_some()),
                (
                    "HOLOCHAIN_BRIDGING_AGENT_PUBKEY",
                    bridging_agent_pubkey.is_some(),
                ),
                ("HOLOCHAIN_CREDIT_LIMIT_EA_ID", credit_limit_ea_id.is_some()),
                ("HOLOCHAIN_UNIT_INDEX", unit_index.is_some()),
            ];
            let missing: Vec<_> = all_present
                .iter()
                .filter(|(_, present)| !*present)
                .map(|(name, _)| *name)
                .collect();
            let any_present = all_present.iter().any(|(_, p)| *p);

            if !missing.is_empty() {
                if any_present {
                    anyhow::bail!(
                        "Holochain config incomplete: missing {} (set all or none)",
                        missing.join(", ")
                    );
                }
                None
            } else {
                let dna_hash_s = dna_hash.expect("checked");
                let agent_pubkey_s = agent_pubkey.expect("checked");
                let bridging_agent_pubkey_s = bridging_agent_pubkey.expect("checked");
                let credit_limit_ea_id_s = credit_limit_ea_id.expect("checked");
                let unit_index_s = unit_index.expect("checked");

                // Convert to expected types before building config; bail if any value is not the right type
                let dna_hash =
                    DnaHashB64::from_str(&dna_hash_s).context("Invalid HOLOCHAIN_DNA_HASH")?;
                let agent_pubkey = AgentPubKeyB64::from_str(&agent_pubkey_s)
                    .context("Invalid HOLOCHAIN_AGENT_PUBKEY")?;
                let bridging_agent_pubkey = AgentPubKeyB64::from_str(&bridging_agent_pubkey_s)
                    .context("Invalid HOLOCHAIN_BRIDGING_AGENT_PUBKEY")?;
                let credit_limit_ea_id = ActionHashB64::from_str(&credit_limit_ea_id_s)
                    .context("Invalid HOLOCHAIN_CREDIT_LIMIT_EA_ID")?;
                let unit_index = unit_index_s
                    .parse()
                    .context("Invalid HOLOCHAIN_UNIT_INDEX (must be u32)")?;

                let admin_port: u16 = admin_port
                    .parse()
                    .context("Invalid HOLOCHAIN_ADMIN_PORT (must be u16)")?;
                let app_port: u16 = app_port
                    .parse()
                    .context("Invalid HOLOCHAIN_APP_PORT (must be u16)")?;

                Some(HolochainConfig {
                    admin_port,
                    app_port,
                    app_id,
                    dna_hash,
                    agent_pubkey,
                    bridging_agent_pubkey,
                    credit_limit_ea_id,
                    unit_index,
                })
            }
        };

        Ok(Config {
            network,
            network_config,
            signer_private_key,
            db_path,
            poll_interval_ms,
            holochain,
        })
    }
}
