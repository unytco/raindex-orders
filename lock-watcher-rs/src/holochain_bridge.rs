//! Holochain bridge: build `CreateParkedLinkInput` payloads for the transactor zome.
//!
//! Payload types match the transactor zome API (ea_id, executor, tag with proof_of_deposit).
//! The actual zome call is handled by the Ham (Holochain Agent Manager) in `ham.rs`.

use crate::types::LockRecord;
use crate::{config::HolochainConfig, watcher::format_amount};
use anyhow::Result;
use rave_engine::types::{
    BridgingAgentInitiateDepositInput, CreateParkedLinkInput, ParkedLinkTag, ParkedTag, UnitMap,
};
use tracing::{debug, info, warn};

pub fn build_bridging_agent_initiate_deposit_payload(
    hc_config: &HolochainConfig,
) -> BridgingAgentInitiateDepositInput {
    BridgingAgentInitiateDepositInput {
        global_definition: hc_config.lane_definition.clone().into(), // this is just outdated and will be removed, currently not used
        lane_definition: hc_config.lane_definition.clone().into(),
    }
}

/// Build CreateParkedLinkInput from lock record and config.
/// Fails with an error if the lock's holochain agent key cannot be converted to AgentPubKey (zome call will not be attempted).
pub fn build_create_parked_link_payload(
    hc_config: &HolochainConfig,
    lock: &LockRecord,
    contract_address_hex: &str,
) -> Result<CreateParkedLinkInput, anyhow::Error> {
    info!(
        "[build_payload] Building create_parked_link payload for lock {} (amount: {}, agent: {})",
        lock.lock_id,
        format_amount(&lock.amount),
        lock.holochain_agent
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

    info!(
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
            "contract_address": format!("0x{}", contract_address_hex.to_lowercase()),
            "amount": formatted_amount,
            "depositor_wallet_address": depositor_wallet_address_as_hc_pubkey,
        }
    });
    let parked_link_tag = ParkedLinkTag {
        ct_role_id: "oracle".to_string(),
        amount: Some(UnitMap::from(vec![(
            hc_config.unit_index,
            formatted_amount.as_str(),
        )])),
        payload: proof,
    };

    info!(
        "[build_payload] Payload built successfully for lock {}: {:?}",
        lock.lock_id, parked_link_tag
    );
    let payload = CreateParkedLinkInput {
        ea_id: hc_config.credit_limit_ea_id.clone().into(),
        executor: Some(hc_config.bridging_agent_pubkey.clone().into()),
        tag: ParkedTag::ParkedLinkTag((parked_link_tag, true)),
    };
    info!(
        "[build_payload] Payload built successfully for lock {}: {:?}",
        lock.lock_id, payload
    );
    Ok(payload)
}
