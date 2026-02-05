//! Holochain bridge: call transactor zome `create_parked_link` with CreateParkedLinkInput.
//!
//! Payload types match the transactor zome API (ea_id, executor, tag with proof_of_deposit).

use crate::config::HolochainConfig;
use crate::types::LockRecord;
use anyhow::{Context, Result};
use rave_engine::types::{CreateParkedLinkInput, ParkedLinkTag, ParkedTag, UnitMap};
use tracing::warn;

/// Build CreateParkedLinkInput from lock record and config.
/// Fails with an error if the lock's holochain agent key cannot be converted to AgentPubKey (zome call will not be attempted).
pub fn build_create_parked_link_payload(
    hc_config: &HolochainConfig,
    lock: &LockRecord,
    contract_address_hex: &str,
) -> Result<CreateParkedLinkInput, anyhow::Error> {
    // 32 byte hex string that start with `0x......``
    // todo
    // Normalize holochain agent (strip 0x, validate as AgentPubKey). Warn and fail if conversion fails.
    let depositor_wallet_address = match holo_hash::AgentPubKey::try_from(
        lock.holochain_agent.as_str().trim_start_matches("0x"),
    ) {
        Ok(agent) => agent.to_string(),
        Err(e) => {
            warn!(
                lock_id = %lock.lock_id,
                holochain_agent = %lock.holochain_agent,
                error = %e,
                "holochain agent key could not be converted to AgentPubKey; zome call will not be attempted"
            );
            anyhow::bail!(
                "invalid holochain agent key for lock {}: {} (value: {})",
                lock.lock_id,
                e,
                lock.holochain_agent
            );
        }
    };

    let proof = serde_json::json!({
        "proof_of_deposit": {
            "method": "deposit",
            "contract_address": contract_address_hex.to_lowercase(),
            "amount": lock.amount,
            "depositor_wallet_address": depositor_wallet_address,
        }
    });
    let parked_link_tag = ParkedLinkTag {
        ct_role_id: "oracle".to_string(),
        amount: Some(UnitMap::from(vec![(
            hc_config.unit_index,
            lock.amount.as_str(),
        )])),
        payload: proof,
    };

    Ok(CreateParkedLinkInput {
        ea_id: hc_config.credit_limit_ea_id.clone().into(),
        executor: Some(hc_config.bridging_agent_pubkey.clone().into()),
        tag: ParkedTag::ParkedLinkTag((parked_link_tag, true)),
    })
}

/// Build CellId from HOLOCHAIN_DNA_HASH and HOLOCHAIN_AGENT_PUBKEY (holochain base64).
fn cell_id_from_config(hc_config: &HolochainConfig) -> Result<holochain_client::CellId> {
    let dna_hash: holo_hash::DnaHash = hc_config.dna_hash.clone().into();
    let agent_pubkey: holo_hash::AgentPubKey = hc_config.agent_pubkey.clone().into();
    Ok(holochain_client::CellId::new(dna_hash, agent_pubkey))
}

/// Call transactor zome `create_parked_link` on the local conductor.
/// Connects via AdminWebsocket (token) then AppWebsocket, then sends the zome call.
/// Uses HOLOCHAIN_DNA_HASH and HOLOCHAIN_AGENT_PUBKEY from config to target the correct cell.
pub async fn call_create_parked_link(
    hc_config: &HolochainConfig,
    payload: &CreateParkedLinkInput,
) -> Result<()> {
    use holochain_client::WebsocketConfig;
    use holochain_client::{
        AdminWebsocket, AppWebsocket, ClientAgentSigner, ExternIO, ZomeCallTarget,
    };
    use holochain_zome_types::prelude::{FunctionName, ZomeName};
    use std::net::ToSocketAddrs;
    use std::sync::Arc;
    use std::time::Duration;

    let cell_id = cell_id_from_config(hc_config)?;

    let admin_addr = hc_config
        .admin_url
        .to_socket_addrs()
        .context("Invalid HOLOCHAIN_ADMIN_URL")?
        .next()
        .context("HOLOCHAIN_ADMIN_URL resolved to no address")?;

    let app_addr = hc_config
        .app_url
        .to_socket_addrs()
        .context("Invalid HOLOCHAIN_APP_URL")?
        .next()
        .context("HOLOCHAIN_APP_URL resolved to no address")?;

    let admin_ws = AdminWebsocket::connect(admin_addr, None)
        .await
        .context("Failed to connect to Holochain admin")?;

    let token_payload = holochain_client::IssueAppAuthenticationTokenPayload {
        installed_app_id: hc_config.app_id.clone().into(),
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

    let payload_io = ExternIO::encode(payload).context("Encode create_parked_link payload")?;

    app_ws
        .call_zome(
            ZomeCallTarget::CellId(cell_id),
            ZomeName::from("transactor"),
            FunctionName::from("create_parked_link"),
            payload_io,
        )
        .await
        .map_err(|e| anyhow::anyhow!("Zome call create_parked_link failed: {}", e))?;

    Ok(())
}
