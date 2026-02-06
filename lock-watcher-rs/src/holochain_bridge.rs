//! Holochain bridge: call transactor zome `create_parked_link` with CreateParkedLinkInput.
//!
//! Payload types match the transactor zome API (ea_id, executor, tag with proof_of_deposit).

use crate::config::HolochainConfig;
use crate::types::LockRecord;
use anyhow::{Context, Result};
use rave_engine::types::{CreateParkedLinkInput, ParkedLinkTag, ParkedTag, UnitMap};
use tracing::{debug, info, warn};

/// Build CreateParkedLinkInput from lock record and config.
/// Fails with an error if the lock's holochain agent key cannot be converted to AgentPubKey (zome call will not be attempted).
pub fn build_create_parked_link_payload(
    hc_config: &HolochainConfig,
    lock: &LockRecord,
    contract_address_hex: &str,
) -> Result<CreateParkedLinkInput, anyhow::Error> {
    info!(
        "[build_payload] Building create_parked_link payload for lock {} (amount: {}, agent: {})",
        lock.lock_id, lock.amount, lock.holochain_agent
    );

    // Normalize holochain agent (strip 0x, validate as AgentPubKey). Warn and fail if conversion fails.

    // Convert 32-byte hex string (0x...) to AgentPubKey.
    // The hex represents the core 32 bytes; from_raw_32 computes the DHT location bytes.
    debug!(
        "[build_payload] Decoding holochain agent hex for lock {}",
        lock.lock_id
    );
    let depositor_wallet_address_as_hc_pubkey = match hex::decode(
        lock.holochain_agent.as_str().trim_start_matches("0x"),
    ) {
        Ok(bytes) => {
            let core_bytes: [u8; 32] = match bytes.try_into() {
                Ok(arr) => arr,
                Err(v) => {
                    let len = v.len();
                    warn!(
                        lock_id = %lock.lock_id,
                        holochain_agent = %lock.holochain_agent,
                        expected = 32,
                        actual = len,
                        "holochain agent hex has wrong byte length; zome call will not be attempted"
                    );
                    anyhow::bail!(
                        "invalid holochain agent key for lock {}: expected 32 bytes, got {} (value: {})",
                        lock.lock_id,
                        len,
                        lock.holochain_agent
                    );
                }
            };
            let agent_pubkey = holo_hash::AgentPubKey::from_raw_32(core_bytes.to_vec());
            let pubkey_str = agent_pubkey.to_string();
            debug!(
                "[build_payload] Decoded holochain agent for lock {}: {}",
                lock.lock_id, pubkey_str
            );
            pubkey_str
        }
        Err(e) => {
            warn!(
                lock_id = %lock.lock_id,
                holochain_agent = %lock.holochain_agent,
                error = %e,
                "holochain agent key could not be decoded from hex when converting to AgentPubKey; zome call will not be attempted"
            );
            anyhow::bail!(
                "invalid holochain agent key for lock {}: {} (value: {})",
                lock.lock_id,
                e,
                lock.holochain_agent
            );
        }
    };

    debug!(
        "[build_payload] Constructing proof_of_deposit JSON for lock {} (contract: {})",
        lock.lock_id,
        contract_address_hex.to_lowercase()
    );
    let formatted_amount = format_amount(&lock.amount);
    debug!(
        "[build_payload] Formatted amount for lock {}: {}",
        lock.lock_id, formatted_amount
    );
    let proof = serde_json::json!({
        "proof_of_deposit": {
            "method": "deposit",
            "contract_address": contract_address_hex.to_lowercase(),
            "amount": formatted_amount,
            "depositor_wallet_address": depositor_wallet_address_as_hc_pubkey,
        }
    });
    let parked_link_tag = ParkedLinkTag {
        ct_role_id: "oracle".to_string(),
        amount: Some(UnitMap::from(vec![(
            hc_config.unit_index,
            formatted_amount,
        )])),
        payload: proof,
    };

    info!(
        "[build_payload] Payload built successfully for lock {} (ea_id: {}, executor: {})",
        lock.lock_id, hc_config.credit_limit_ea_id, hc_config.bridging_agent_pubkey
    );

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

    info!(
        "[call_zome] Initiating zome call: transactor/create_parked_link (admin: {}, app: {})",
        hc_config.admin_url, hc_config.app_url
    );

    let cell_id = cell_id_from_config(hc_config)?;
    debug!("[call_zome] CellId constructed from config");

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

    debug!(
        "[call_zome] Connecting to admin websocket at {}",
        admin_addr
    );
    let admin_ws = AdminWebsocket::connect(admin_addr, None)
        .await
        .context("Failed to connect to Holochain admin")?;
    debug!("[call_zome] Admin websocket connected");

    let token_payload = holochain_client::IssueAppAuthenticationTokenPayload {
        installed_app_id: hc_config.app_id.clone().into(),
        expiry_seconds: 3600,
        single_use: false,
    };
    debug!(
        "[call_zome] Issuing app auth token for app_id: {}",
        hc_config.app_id
    );
    let issued = admin_ws
        .issue_app_auth_token(token_payload)
        .await
        .context("Failed to issue app auth token")?;
    debug!("[call_zome] App auth token issued successfully");

    let signer = ClientAgentSigner::default();
    let mut client_config = WebsocketConfig::CLIENT_DEFAULT;
    client_config.default_request_timeout = Duration::from_secs(30);
    let config = Arc::new(client_config);

    debug!("[call_zome] Connecting to app websocket at {}", app_addr);
    let app_ws =
        AppWebsocket::connect_with_config(app_addr, config, issued.token, signer.into(), None)
            .await
            .context("Failed to connect to Holochain app")?;
    debug!("[call_zome] App websocket connected");

    debug!("[call_zome] Encoding create_parked_link payload");
    let payload_io = ExternIO::encode(payload).context("Encode create_parked_link payload")?;

    info!("[call_zome] Sending zome call: transactor/create_parked_link");
    let response = app_ws
        .call_zome(
            ZomeCallTarget::CellId(cell_id),
            ZomeName::from("transactor"),
            FunctionName::from("create_parked_link"),
            payload_io,
        )
        .await
        .map_err(|e| anyhow::anyhow!("Zome call create_parked_link failed: {}", e))?;

    info!("[call_zome] Zome call create_parked_link committed successfully");
    debug!(?response, "[call_zome] create_parked_link response");
    Ok(())
}
