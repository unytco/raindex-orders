use crate::config::{
    bridging_agent, hot_unit_index, log_testnet_defaults, network_from, screening_settings,
    with_network_defaults, Holochain, Network,
};
use crate::screening::Screener;
use crate::signer::{
    expiry_seconds, is_bearer_token, order_words, parsed, NOT_A_BEARER_TOKEN, TEST_SIGNER,
};
use alloy::primitives::{Address, B256};
use alloy::signers::local::PrivateKeySigner;
use anyhow::Result;
use holo_hash::AgentPubKeyB64;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU64;

const TESTNET_DEFAULTED: [&str; 2] = ["ORDERBOOK_ADDRESS", "VAULT_ID"];

const DEFAULT_EXPIRY_TOLERANCE_SECONDS: u64 = 600;

/// No `Debug`: it holds the bearer token.
pub struct ServiceConfig {
    pub network: Network,
    pub safe: Address,
    pub order_owner: Address,
    pub order: [B256; 5],
    pub key: PrivateKeySigner,
    pub authorization: String,
    pub bind: SocketAddr,
    pub db_path: String,
    pub expiry_seconds: NonZeroU64,
    pub expiry_tolerance_seconds: u64,
    /// `None` with `SCREENING=off`.
    pub screener: Option<Box<dyn Screener>>,
    pub bridging_agent: AgentPubKeyB64,
    pub hot_unit_index: u32,
    pub holochain: Holochain,
}

impl ServiceConfig {
    pub fn from_env() -> Result<Self> {
        Self::from_settings(|key| std::env::var(key).ok())
    }

    /// Names every fault in one refusal, and never quotes the key or the token.
    pub(super) fn from_settings(setting: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let given = |key: &str| setting(key).filter(|value| !value.is_empty());
        let mut faults = Vec::new();
        let network = match given("NETWORK") {
            Some(raw) if raw.parse::<Network>().is_err() => {
                faults.push(format!("NETWORK={raw} is not base-sepolia or mainnet"));
                None
            }
            _ => Some(network_from(&setting)?),
        };
        // With a bad NETWORK, the rest is checked by base-sepolia's rules, which
        // mainnet's only add to.
        let rules = network.unwrap_or(Network::BaseSepolia);
        log_testnet_defaults(rules, &setting, &TESTNET_DEFAULTED);
        let defaulted = with_network_defaults(rules, &setting);

        let safe = parsed(&given, "CLAIM_SIGNER", "an address", &mut faults);
        let order_hash = parsed(&given, "ORDER_HASH", "a 32-byte hash", &mut faults);
        let order_owner = parsed(&given, "ORDER_OWNER", "an address", &mut faults);
        let orderbook = parsed(&defaulted, "ORDERBOOK_ADDRESS", "an address", &mut faults);
        let token = parsed(&given, "TOKEN_ADDRESS", "an address", &mut faults);
        let vault_id = parsed(&defaulted, "VAULT_ID", "a 32-byte vault ID", &mut faults);
        let key = match given("SAFE_OWNER_KEY").map(|raw| raw.parse::<PrivateKeySigner>()) {
            None => {
                faults.push("SAFE_OWNER_KEY is required".to_string());
                None
            }
            Some(Err(_)) => {
                faults.push("SAFE_OWNER_KEY is not a secp256k1 private key".to_string());
                None
            }
            Some(Ok(key)) => {
                if rules == Network::Mainnet && key.address() == TEST_SIGNER {
                    faults.push(format!(
                        "SAFE_OWNER_KEY is the test signer {TEST_SIGNER}, whose key is public: mainnet refuses it"
                    ));
                }
                Some(key)
            }
        };
        let bearer = given("SIGNER_BEARER_TOKEN");
        match &bearer {
            None => faults.push("SIGNER_BEARER_TOKEN is required".to_string()),
            Some(token) if !is_bearer_token(token) => {
                faults.push(format!("SIGNER_BEARER_TOKEN holds {NOT_A_BEARER_TOKEN}"))
            }
            Some(_) => {}
        }
        let bind_addr = parsed_or(
            &given,
            "SIGNER_BIND_ADDR",
            "an IP address",
            IpAddr::from([127, 0, 0, 1]),
            &mut faults,
        );
        let bind_port: Option<u16> = parsed(&given, "SIGNER_BIND_PORT", "a port", &mut faults);
        let db_path = given("SIGNER_DB_PATH");
        if db_path.is_none() {
            faults.push("SIGNER_DB_PATH is required".to_string());
        }
        let expiry_seconds = expiry_seconds(&given, &mut faults);
        let expiry_tolerance_seconds = parsed_or(
            &given,
            "EXPIRY_TOLERANCE_SECONDS",
            "a number of seconds",
            DEFAULT_EXPIRY_TOLERANCE_SECONDS,
            &mut faults,
        );
        let screener = named(screening_settings(rules, &setting), &mut faults);
        let bridging_agent = named(bridging_agent(&setting), &mut faults);
        let hot_unit_index = named(hot_unit_index(&setting), &mut faults);
        let holochain = named(Holochain::from_settings(&setting), &mut faults);

        let config = (|| {
            Some(Self {
                network: network?,
                safe: safe?,
                order_owner: order_owner?,
                order: order_words(order_hash?, order_owner?, orderbook?, token?, vault_id?),
                key: key?,
                authorization: format!("Bearer {}", bearer?),
                bind: SocketAddr::new(bind_addr?, bind_port?),
                db_path: db_path?,
                expiry_seconds: expiry_seconds?,
                expiry_tolerance_seconds: expiry_tolerance_seconds?,
                screener: screener?,
                bridging_agent: bridging_agent?,
                hot_unit_index: hot_unit_index?,
                holochain: holochain?,
            })
        })();
        match config {
            Some(config) if faults.is_empty() => Ok(config),
            _ => anyhow::bail!("{}", faults.join("; ")),
        }
    }
}

fn parsed_or<T: std::str::FromStr>(
    setting: &impl Fn(&str) -> Option<String>,
    key: &str,
    what: &str,
    default: T,
    faults: &mut Vec<String>,
) -> Option<T> {
    match setting(key) {
        None => Some(default),
        Some(_) => parsed(setting, key, what, faults),
    }
}

fn named<T>(read: Result<T>, faults: &mut Vec<String>) -> Option<T> {
    read.map_err(|err| faults.push(format!("{err:#}"))).ok()
}
