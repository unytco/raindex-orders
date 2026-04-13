use crate::config::Config;
use crate::ham::Ham;
use crate::lock_flow::{format_amount, LockFlow};
use crate::signer::{generate_coupon, signer_context_from_env};
use crate::state::{StateStore, WorkItem, WorkState};
use anyhow::{Context, Result};
use holo_hash::{ActionHash, ActionHashB64, AgentPubKey};
use holochain_zome_types::entry::GetStrategy;
use rave_engine::types::{
    CreateParkedLinkInput, CreateParkedSpendInput, GlobalDefinitionExt, LaneExt, ParkedData,
    ParkedLinkType, RAVEExecuteInputs, Transaction, TransactionDetails, UnitMap, RAVE,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;
use tracing::{error, info, warn};

pub struct BridgeOrchestrator {
    cfg: Config,
    db: StateStore,
}

impl BridgeOrchestrator {
    pub fn new(cfg: Config) -> Result<Self> {
        let db = StateStore::open(&cfg.db_path)?;
        Ok(Self { cfg, db })
    }

    pub async fn run(&self) -> Result<()> {
        info!(
            "bridge-orchestrator started network={:?} poll={}ms bridge_cycle={}ms",
            self.cfg.network, self.cfg.poll_interval_ms, self.cfg.bridge_cycle_interval_ms
        );
        let ham = Ham::connect(self.cfg.admin_port, self.cfg.app_port, &self.cfg.app_id)
            .await
            .context("Failed to connect to Holochain")?;
        let lock_flow = LockFlow::new(self.cfg.clone(), self.db.clone());

        let mut last_bridge_cycle =
            std::time::Instant::now() - Duration::from_millis(self.cfg.bridge_cycle_interval_ms);

        loop {
            if let Err(e) = lock_flow.run_cycle().await {
                error!("lock cycle failed: {}", e);
            }

            if last_bridge_cycle.elapsed()
                >= Duration::from_millis(self.cfg.bridge_cycle_interval_ms)
            {
                match self.run_bridge_cycle(&ham).await {
                    Ok(()) => {
                        last_bridge_cycle = std::time::Instant::now();
                    }
                    Err(e) => {
                        error!("bridge cycle failed: {}", e);
                    }
                }
            }

            tokio::time::sleep(Duration::from_millis(self.cfg.poll_interval_ms)).await;
        }
    }

    /// Single unified bridge cycle that handles deposits and withdrawals together.
    ///
    /// 1. ONE create_parked_link on credit limit EA (batched proof array)
    /// 2. ONE credit limit RAVE (consumes the link via aggregate_execution)
    /// 3. ONE create_parked_spend on bridging EA (aggregated proof list)
    /// 4. Scan bridging EA for pending withdrawals, generate coupons
    /// 5. ONE unified bridging RAVE with coupons map
    async fn run_bridge_cycle(&self, ham: &Ham) -> Result<()> {
        info!("bridge cycle started");
        let started = std::time::Instant::now();

        let global_definition: GlobalDefinitionExt = ham
            .call_zome(
                &self.cfg.role_name,
                "transactor",
                "get_current_global_definition",
                &Some(GetStrategy::Network),
            )
            .await?;
        let context = self.resolve_deposit_context(ham, &global_definition).await?;

        if context.bridging_agent != self.cfg.bridging_agent_pubkey {
            warn!(
                "Skipping bridge cycle: configured bridging agent does not match lane/global"
            );
            return Ok(());
        }

        let queued_locks = self.db.list_work_items("lock", WorkState::Queued, 5000)?;
        let mut deposit_proofs: Vec<Value> = Vec::new();
        let mut deposit_amounts: Vec<UnitMap> = Vec::new();
        let mut processed_lock_ids: Vec<i64> = Vec::new();

        for item in &queued_locks {
            match self.extract_lock_proof(item) {
                Ok((proof, amount)) => {
                    deposit_proofs.push(proof);
                    deposit_amounts.push(amount);
                    processed_lock_ids.push(item.id);
                }
                Err(e) => {
                    warn!(
                        "proof extraction failed id={} error={}, skipping",
                        item.item_id, e
                    );
                }
            }
        }

        if !processed_lock_ids.is_empty() {
            let total_deposit_amount = accumulate_amounts(&deposit_amounts)?;

            let parked_data = ParkedData {
                ct_role_id: "oracle".to_string(),
                amount: Some(total_deposit_amount.clone()),
                payload: json!({ "proof_of_deposit": deposit_proofs.clone() }),
            };

            info!(
                "create_parked_link start locks={} total_amount={:?}",
                processed_lock_ids.len(),
                total_deposit_amount
            );

            let link_result: (ActionHashB64, AgentPubKey) = ham
                .call_zome(
                    &self.cfg.role_name,
                    "transactor",
                    "create_parked_link",
                    &CreateParkedLinkInput {
                        ea_id: self.cfg.credit_limit_ea_id.clone().into(),
                        executor: Some(self.cfg.bridging_agent_pubkey.clone().into()),
                        parked_link_type: ParkedLinkType::ParkedData((parked_data, true)),
                    },
                )
                .await?;

            info!(
                "batched create_parked_link success locks={} action_hash={}",
                processed_lock_ids.len(),
                link_result.0
            );

            let credit_limit_ea_id: ActionHash =
                context.credit_limit_adjustment.clone().into();
            let _: (RAVE, ActionHash) = ham
                .call_zome(
                    &self.cfg.role_name,
                    "transactor",
                    "execute_rave",
                    &RAVEExecuteInputs {
                        ea_id: credit_limit_ea_id,
                        executor_inputs: Value::Null,
                        links: vec![],
                        global_definition: global_definition.id.clone().into(),
                        lane_definitions: context.lane_definitions.clone(),
                        strategy: GetStrategy::Local,
                    },
                )
                .await?;
            info!("credit limit RAVE executed");

            if !total_deposit_amount.is_zero() {
                let _: ActionHashB64 = ham
                    .call_zome(
                        &self.cfg.role_name,
                        "transactor",
                        "create_parked_spend",
                        &CreateParkedSpendInput {
                            ea_id: context.bridging_agreement.clone().into(),
                            executor: Some(self.cfg.bridging_agent_pubkey.clone().into()),
                            ct_role_id: Some("bridging_agent".to_string()),
                            amount: total_deposit_amount,
                            spender_payload: json!({
                                "proof_of_deposit": deposit_proofs,
                            }),
                            lane_definitions: context.lane_definitions.clone(),
                        },
                    )
                    .await?;
                info!("parked spend created on bridging EA");
            }

            for lock_id in &processed_lock_ids {
                self.db.mark_succeeded(*lock_id)?;
            }
            info!("marked {} locks as succeeded", processed_lock_ids.len());
        }

        let bridging_ea_id: ActionHash = context.bridging_agreement.clone().into();
        let bridging_links: Vec<Transaction> = ham
            .call_zome(
                &self.cfg.role_name,
                "transactor",
                "get_parked_links_by_ea",
                &bridging_ea_id,
            )
            .await?;

        let mut coupons_map = serde_json::Map::new();
        for tx in &bridging_links {
            if let TransactionDetails::ParkedSpend {
                attached_payload, ..
            } = &tx.details
            {
                if let Some(withdraw_to) = attached_payload
                    .get("withdraw_to_address")
                    .and_then(|v| v.as_str())
                {
                    let amount = tx
                        .amount
                        .get("1")
                        .map(|v| v.to_string())
                        .unwrap_or_default();
                    info!(
                        "generating coupon tx_id={:?} recipient={} amount={}",
                        tx.id, withdraw_to, amount
                    );
                    let signer_ctx = signer_context_from_env()?;
                    let coupon = generate_coupon(&amount, withdraw_to, &signer_ctx).await?;
                    coupons_map.insert(tx.id.to_string(), Value::String(coupon));
                }
            }
        }
        let withdrawal_count = coupons_map.len();

        if !bridging_links.is_empty() {
            let rave_result: (RAVE, ActionHash) = ham
                .call_zome(
                    &self.cfg.role_name,
                    "transactor",
                    "execute_rave",
                    &RAVEExecuteInputs {
                        ea_id: context.bridging_agreement.into(),
                        executor_inputs: json!({
                            "coupons": Value::Object(coupons_map)
                        }),
                        links: vec![],
                        global_definition: global_definition.id.clone().into(),
                        lane_definitions: context.lane_definitions,
                        strategy: GetStrategy::Local,
                    },
                )
                .await?;
            info!(
                "unified bridging RAVE executed action_hash={} deposits={} withdrawals={}",
                rave_result.1,
                processed_lock_ids.len(),
                withdrawal_count
            );
        } else if processed_lock_ids.is_empty() {
            info!("bridge cycle no-op: no pending deposits or withdrawals");
        }

        let duration_ms = started.elapsed().as_millis() as u64;
        info!(
            "bridge cycle completed duration={}ms locks={} withdrawals={}",
            duration_ms,
            processed_lock_ids.len(),
            withdrawal_count
        );

        Ok(())
    }

    fn extract_lock_proof(&self, item: &WorkItem) -> Result<(Value, UnitMap)> {
        let payload = LockPayload::deserialize(item.payload_json.clone())?;
        let contract_hex = format!("{:x}", self.cfg.lock_vault_address);
        let depositor = decode_holochain_agent_as_pubkey_string(&payload.holochain_agent)?;
        let normalized = payload.normalized_amounts()?;
        let amount = normalized.amount_hot.clone();

        let proof = json!({
            "method": "deposit",
            "contract_address": format!("0x{}", contract_hex.to_lowercase()),
            "amount": amount,
            "depositor_wallet_address": depositor
        });

        info!(
            "extracted proof id={} amount={} agent={}",
            payload.lock_id, amount, payload.holochain_agent
        );

        Ok((proof, UnitMap::from(vec![(self.cfg.unit_index, amount.as_str())])))
    }

    async fn resolve_deposit_context(
        &self,
        ham: &Ham,
        global_definition: &GlobalDefinitionExt,
    ) -> Result<DepositContext> {
        if let Some(lane_definition_hash) = self.cfg.lane_definition.clone() {
            let lanes: Vec<LaneExt> = ham
                .call_zome(
                    &self.cfg.role_name,
                    "transactor",
                    "get_all_lane",
                    &Some(GetStrategy::Network),
                )
                .await?;
            let lane_definition = lanes
                .into_iter()
                .filter_map(|lane| lane.definition)
                .find(|def| def.definition_hash == lane_definition_hash)
                .context("Configured HOLOCHAIN_LANE_DEFINITION not found in get_all_lane result")?;
            let bridging_agreement = lane_definition
                .rave_agreements
                .bridging_agreement
                .context("No bridging agreement set for configured lane definition")?;
            return Ok(DepositContext {
                lane_definitions: vec![lane_definition_hash.into()],
                bridging_agent: lane_definition.special_agents.bridging_agent.pub_key,
                credit_limit_adjustment: lane_definition.rave_agreements.credit_limit_adjustment,
                bridging_agreement,
            });
        }

        let global_lane = global_definition.lane_def.clone();
        let bridging_agreement = global_lane
            .rave_agreements
            .bridging_agreement
            .context("No bridging agreement set for global lane definition")?;
        Ok(DepositContext {
            lane_definitions: vec![],
            bridging_agent: global_lane.special_agents.bridging_agent.pub_key,
            credit_limit_adjustment: global_lane.rave_agreements.credit_limit_adjustment,
            bridging_agreement,
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct LockPayload {
    lock_id: String,
    sender: String,
    #[serde(default)]
    amount: Option<String>,
    #[serde(default)]
    amount_raw_wei: Option<String>,
    #[serde(default)]
    amount_hot: Option<String>,
    holochain_agent: String,
    tx_hash: String,
    block_number: u64,
    timestamp: u64,
    required_confirmations: u64,
}

struct NormalizedLockAmount {
    amount_raw_wei: String,
    amount_hot: String,
}

impl LockPayload {
    fn normalized_amounts(&self) -> Result<NormalizedLockAmount> {
        let amount_raw_wei = self
            .amount_raw_wei
            .clone()
            .or_else(|| self.amount.clone())
            .context("missing lock amount raw wei")?;
        let amount_hot = self
            .amount_hot
            .clone()
            .or_else(|| amount_from_legacy_field(self.amount.clone()))
            .unwrap_or_else(|| format_amount(&amount_raw_wei));
        validate_hot_amount(&amount_hot)?;
        Ok(NormalizedLockAmount {
            amount_raw_wei,
            amount_hot,
        })
    }
}

fn amount_from_legacy_field(amount: Option<String>) -> Option<String> {
    let amount = amount?;
    if amount.contains('.') {
        return Some(amount);
    }
    if !amount.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if amount.len() >= 13 {
        Some(format_amount(&amount))
    } else {
        Some(amount)
    }
}

fn validate_hot_amount(amount_hot: &str) -> Result<()> {
    if amount_hot.is_empty() {
        anyhow::bail!("amount_hot cannot be empty");
    }
    if amount_hot.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Ok(());
    }
    anyhow::bail!("amount_hot is not a valid numeric string: {}", amount_hot);
}

fn decode_holochain_agent_as_pubkey_string(agent_hex: &str) -> Result<String> {
    let bytes = hex::decode(agent_hex.trim_start_matches("0x"))
        .context("holochain agent key should be a hex string")?;
    let core_bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|v: Vec<u8>| anyhow::anyhow!("expected 32 byte agent key, got {}", v.len()))?;
    Ok(holo_hash::AgentPubKey::from_raw_32(core_bytes.to_vec()).to_string())
}

struct DepositContext {
    lane_definitions: Vec<ActionHash>,
    bridging_agent: holo_hash::AgentPubKeyB64,
    credit_limit_adjustment: ActionHashB64,
    bridging_agreement: ActionHashB64,
}

fn accumulate_amounts(amounts: &[UnitMap]) -> Result<UnitMap> {
    let mut total = UnitMap::new();
    for amount in amounts {
        total.add(amount.clone())?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_unit_maps() {
        let amounts = vec![
            UnitMap::from(vec![(1_u32, "10")]),
            UnitMap::from(vec![(1_u32, "15")]),
        ];
        let total = accumulate_amounts(&amounts).expect("amount accumulation should succeed");
        assert_eq!(total.get("1").map(|v| v.to_string()), Some("25".to_string()));
    }

    #[test]
    fn validate_hot_amount_rejects_non_numeric() {
        assert!(validate_hot_amount("1.230000").is_ok());
        assert!(validate_hot_amount("12").is_ok());
        assert!(validate_hot_amount("bad-value").is_err());
    }

    #[test]
    fn amount_from_legacy_field_converts_wei_like_values() {
        assert_eq!(
            amount_from_legacy_field(Some("1000000000000000000".to_string())),
            Some("1".to_string())
        );
        assert_eq!(
            amount_from_legacy_field(Some("2.500000".to_string())),
            Some("2.500000".to_string())
        );
    }
}
