//! Withdrawer command: connect to Holochain and perform zome calls.
//!
//! Establishes a single session (one token, one app connection) and reuses it
//! for multiple zome calls. Zome/function names are hardcoded; add more calls
//! in `run_withdrawer` as needed.

use crate::signer::{generate_coupon_with_context, SignerContext};
use anyhow::{Context, Result};
use clap::Parser;
use holo_hash::{ActionHashB64, AgentPubKeyB64, DnaHashB64};
use holochain_client::{ExternIO, ZomeCallTarget};
use holochain_zome_types::{
    entry::GetStrategy,
    zome::{FunctionName, ZomeName},
};
use rave_engine::types::{RAVEExecuteInputs, Transaction, TransactionDetails};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::json;
use std::str::FromStr;

/// Holochain connection config for the withdrawer command.
#[derive(Debug, Clone)]
pub struct WithdrawerConfig {
    pub admin_url: String,
    pub app_url: String,
    pub app_id: String,
    pub dna_hash: DnaHashB64,
    pub agent_pubkey: AgentPubKeyB64,
}

/// CLI arguments for `coupon-signer withdrawer`.
#[derive(Parser, Debug)]
#[command(
    about = "Connect to Holochain and run zome calls (extensible for specific withdrawer flows)"
)]
pub struct WithdrawerArgs {
    /// Holochain admin websocket URL (host:port)
    #[arg(long, env = "HOLOCHAIN_ADMIN_URL", default_value = "127.0.0.1:8800")]
    pub admin_url: String,

    /// Holochain app websocket URL (host:port)
    #[arg(long, env = "HOLOCHAIN_APP_URL", default_value = "127.0.0.1:30000")]
    pub app_url: String,

    /// Installed app ID for auth token
    #[arg(long, env = "HOLOCHAIN_APP_ID", default_value = "bridging-app")]
    pub app_id: String,

    /// DNA hash (holochain base64)
    #[arg(long, env = "HOLOCHAIN_DNA_HASH")]
    pub dna_hash: String,

    /// Agent pubkey (holochain base64)
    #[arg(long, env = "HOLOCHAIN_AGENT_PUBKEY")]
    pub agent_pubkey: String,

    /// EA ID
    #[arg(long, env = "BRIDGING_AGREEMENT_ID")]
    pub bridging_agreement_id: String,
}

fn cell_id_from_config(cfg: &WithdrawerConfig) -> Result<holochain_client::CellId> {
    let dna_hash: holo_hash::DnaHash = cfg.dna_hash.clone().into();
    let agent_pubkey: holo_hash::AgentPubKey = cfg.agent_pubkey.clone().into();
    Ok(holochain_client::CellId::new(dna_hash, agent_pubkey))
}

/// Shared session: one app auth token and one app websocket, reused for multiple zome calls.
pub struct WithdrawerSession {
    app_ws: holochain_client::AppWebsocket,
    cell_id: holochain_client::CellId,
}

impl WithdrawerSession {
    /// Connect once (admin → token → app ws). Use this session for all zome calls in a run.
    pub async fn connect(cfg: &WithdrawerConfig) -> Result<Self> {
        use holochain_client::WebsocketConfig;
        use holochain_client::{AdminWebsocket, AppWebsocket, ClientAgentSigner};
        use std::net::ToSocketAddrs;
        use std::sync::Arc;
        use std::time::Duration;

        let cell_id = cell_id_from_config(cfg)?;

        let admin_addr = cfg
            .admin_url
            .to_socket_addrs()
            .context("Invalid HOLOCHAIN_ADMIN_URL")?
            .next()
            .context("HOLOCHAIN_ADMIN_URL resolved to no address")?;

        let app_addr = cfg
            .app_url
            .to_socket_addrs()
            .context("Invalid HOLOCHAIN_APP_URL")?
            .next()
            .context("HOLOCHAIN_APP_URL resolved to no address")?;

        let admin_ws = AdminWebsocket::connect(admin_addr, None)
            .await
            .context("Failed to connect to Holochain admin")?;

        let token_payload = holochain_client::IssueAppAuthenticationTokenPayload {
            installed_app_id: cfg.app_id.clone().into(),
            expiry_seconds: 3600,
            single_use: false,
        };
        let issued = admin_ws
            .issue_app_auth_token(token_payload)
            .await
            .context("Failed to issue app auth token")?;

        let signer = ClientAgentSigner::default();
        let mut client_config = WebsocketConfig::CLIENT_DEFAULT;
        client_config.default_request_timeout = Duration::from_secs(30);
        let config = Arc::new(client_config);

        let app_ws =
            AppWebsocket::connect_with_config(app_addr, config, issued.token, signer.into(), None)
                .await
                .context("Failed to connect to Holochain app")?;

        Ok(WithdrawerSession { app_ws, cell_id })
    }
    pub async fn call_zome<I, R>(&self, zome_name: &str, fn_name: &str, payload: I) -> Result<R>
    where
        I: serde::Serialize + std::fmt::Debug,
        R: DeserializeOwned,
    {
        let response = self
            .app_ws
            .call_zome(
                ZomeCallTarget::CellId(self.cell_id.clone()),
                ZomeName::from(zome_name),
                FunctionName::from(fn_name),
                ExternIO::encode(payload)?,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to call zome: {}", e))?;
        rmp_serde::from_slice(&response.0).context("Failed to deserialize response")
    }
}

/// Entry point for `coupon-signer withdrawer`. One session, then multiple zome calls (hardcoded names).
pub fn run_withdrawer(withdrawer_args: WithdrawerArgs) -> Result<()> {
    let cfg = WithdrawerConfig {
        admin_url: withdrawer_args.admin_url.clone(),
        app_url: withdrawer_args.app_url.clone(),
        app_id: withdrawer_args.app_id.clone(),
        dna_hash: DnaHashB64::from_str(&withdrawer_args.dna_hash)
            .context("Invalid HOLOCHAIN_DNA_HASH")?,
        agent_pubkey: AgentPubKeyB64::from_str(&withdrawer_args.agent_pubkey)
            .context("Invalid HOLOCHAIN_AGENT_PUBKEY")?,
    };

    let rt = tokio::runtime::Runtime::new().context("Create tokio runtime")?;
    rt.block_on(async {
        let session = WithdrawerSession::connect(&cfg).await?;
        use rave_engine::types::Transaction;
        // Example zome call (hardcoded names). Add more session.call_zome(...) here as needed.
        let bridging_agreement_id = withdrawer_args.bridging_agreement_id.clone();
        let result: Vec<Transaction> = session
            .call_zome(
                "transactor",
                "get_parked_links_by_ea",
                &bridging_agreement_id,
            )
            .await?;

        eprintln!(
            "Number of links found for ea: {} : {}",
            bridging_agreement_id,
            result.len()
        );

        // now we execute each link individually to manage the size of the inputs of the executed  transaction

        for transaction in result {
            let _ =
                process_transaction(&session, bridging_agreement_id.clone(), transaction).await?;
        }
        Ok(())
    })
}

/// For each parked spend transaction: build coupon from signer context (env), then call execute_transaction.
async fn process_transaction(
    session: &WithdrawerSession,
    bridging_agreement_id: String,
    transaction: Transaction,
) -> Result<()> {
    // Run the signer and get a coupon that will be used to execute the transaction.
    eprintln!("[process_transaction] Starting processing for transaction");
    let amount = transaction
        .clone()
        .amount
        .get("1") // hard coded value
        .map(|amount| amount.to_string())
        .unwrap_or_default();
    eprintln!("[process_transaction] Extracted amount: {}", amount);
    let (recipient, gd, ld) = if let TransactionDetails::ParkedSpend {
        attached_payload,
        global_definition,
        lane_definitions,
        ..
    } = transaction.clone().details
    {
        #[derive(Serialize, Deserialize)]
        struct SpenderPayload {
            withdraw_contract_address: String,
            withdraw_to_address: String,
        }
        let payload = serde_json::from_value::<SpenderPayload>(attached_payload.clone())
            .context("Failed to deserialize spender payload")?;
        (
            payload.withdraw_to_address,
            global_definition.clone(),
            lane_definitions.clone(),
        )
    } else {
        eprintln!("Transaction details are not a parked spend");
        return Ok(());
    };
    eprintln!(
        "[process_transaction] Recipient: {}, generating coupon...",
        recipient
    );
    let ctx = SignerContext::from_env()
        .context("Load signer context (ORDER_HASH, ORDER_OWNER, etc.) for coupon")?;
    let (coupon, _) = generate_coupon_with_context(&amount, &recipient, &ctx)?;
    eprintln!("[process_transaction] Coupon generated successfully");
    eprintln!(
        "[process_transaction] Building RAVEExecuteInputs payload for EA: {}",
        bridging_agreement_id
    );
    let payload = RAVEExecuteInputs {
        ea_id: ActionHashB64::from_str(&bridging_agreement_id)
            .context("Invalid bridging agreement id")?
            .into(),
        executor_inputs: json!(
            {
                "call_method": "withdraw",
                "coupon": coupon,
            }
        ),
        links: vec![transaction],
        global_definition: gd.clone().into(),
        lane_definitions: ld.iter().map(|ld| ld.clone().into()).collect(),
        strategy: GetStrategy::Network,
    };
    eprintln!(
        "[process_transaction] Payload built — calling zome: transactor/execute_transaction"
    );
    session
        .call_zome::<_, ()>("transactor", "execute_transaction", &payload)
        .await?;
    eprintln!("[process_transaction] Zome call committed successfully");
    Ok(())
}
