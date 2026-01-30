use alloy::primitives::Address;
use anyhow::{Context, Result};
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

/// Application configuration
#[derive(Debug, Clone)]
pub struct Config {
    pub network: Network,
    pub network_config: NetworkConfig,
    pub signer_private_key: Option<String>,
    pub db_path: String,
    pub poll_interval_ms: u64,
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

        let db_path = env::var("DB_PATH")
            .unwrap_or_else(|_| "./data/locks.db".to_string());

        let poll_interval_ms: u64 = env::var("POLL_INTERVAL_MS")
            .unwrap_or_else(|_| "5000".to_string())
            .parse()
            .context("Invalid POLL_INTERVAL_MS")?;

        Ok(Config {
            network,
            network_config,
            signer_private_key,
            db_path,
            poll_interval_ms,
        })
    }
}
