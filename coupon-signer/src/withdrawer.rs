//! Withdrawer command: connect to Holochain via Ham and perform zome calls.
//!
//! Establishes a single Ham session and reuses it for multiple zome calls.
//! Zome/function names are hardcoded; add more ham.call_zome(...) here as needed.

use crate::ham::Ham;
use crate::signer::{generate_coupon_with_context, SignerContext};
use anyhow::{Context, Result};
use clap::Parser;
use holo_hash::{ActionHash, ActionHashB64};
use rave_engine::types::{RAVEExecuteInputs, Transaction, TransactionDetails, RAVE};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::str::FromStr;

/// CLI arguments for `coupon-signer withdrawer`.
#[derive(Parser, Debug)]
#[command(
    about = "Connect to Holochain and run zome calls (extensible for specific withdrawer flows)"
)]
pub struct WithdrawerArgs {
    /// Holochain admin websocket port
    #[arg(long, env = "HOLOCHAIN_ADMIN_PORT", default_value = "30000")]
    pub admin_port: u16,

    /// Holochain app websocket port (used when no existing app interface is found)
    #[arg(long, env = "HOLOCHAIN_APP_PORT", default_value = "30001")]
    pub app_port: u16,

    /// Installed app ID for auth token
    #[arg(long, env = "HOLOCHAIN_APP_ID", default_value = "bridging-app")]
    pub app_id: String,

    /// Holochain role name for zome calls
    #[arg(long, env = "HOLOCHAIN_ROLE_NAME", default_value = "alliance")]
    pub role_name: String,

    /// EA ID
    #[arg(long, env = "BRIDGING_AGREEMENT_ID")]
    pub bridging_agreement_id: String,
}

/// Entry point for `coupon-signer withdrawer`. One Ham session, then multiple zome calls.
pub fn run_withdrawer(withdrawer_args: WithdrawerArgs) -> Result<()> {
    let rt = tokio::runtime::Runtime::new().context("Create tokio runtime")?;
    rt.block_on(async {
        let ham = Ham::connect(
            withdrawer_args.admin_port,
            withdrawer_args.app_port,
            &withdrawer_args.app_id,
        )
        .await
        .context("Failed to connect Ham to Holochain")?;

        let role_name = &withdrawer_args.role_name;
        let bridging_agreement_id = withdrawer_args.bridging_agreement_id.clone();

        let result: Vec<Transaction> = ham
            .call_zome(
                role_name,
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

        // Execute each link individually to manage the size of the inputs.
        for transaction in result {
            let _ =
                process_transaction(&ham, role_name, bridging_agreement_id.clone(), transaction)
                    .await?;
        }
        Ok(())
    })
}

/// For each parked spend transaction: build coupon from signer context (env), then call execute_rave.
async fn process_transaction(
    ham: &Ham,
    role_name: &str,
    bridging_agreement_id: String,
    transaction: Transaction,
) -> Result<()> {
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
        strategy: holochain_zome_types::entry::GetStrategy::Network,
    };
    eprintln!("[process_transaction] Payload built — calling zome: transactor/execute_rave");
    let tx_hash = ham
        .call_zome::<_, (RAVE, ActionHash)>(role_name, "transactor", "execute_rave", &payload)
        .await?;
    eprintln!(
        "[process_transaction] Zome call committed successfully: {}",
        tx_hash.1.to_string()
    );
    Ok(())
}
