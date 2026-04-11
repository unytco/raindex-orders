use crate::config::Config;
use crate::coupon_flow::CouponFlow;
use crate::ham::Ham;
use crate::lock_flow::{format_amount, LockFlow};
use crate::signer::{generate_coupon, signer_context_from_env};
use crate::state::{StateStore, WorkItem};
use anyhow::{Context, Result};
use holo_hash::{ActionHash, ActionHashB64, AgentPubKey};
use holochain_zome_types::entry::GetStrategy;
use rave_engine::types::{
    CreateParkedLinkInput, CreateParkedSpendInput, GlobalDefinitionExt, LaneExt, ParkedData,
    ParkedLinkType, RAVEExecuteInputs, Transaction, TransactionDetails, UnitMap, RAVE,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::str::FromStr;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
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
            "bridge-orchestrator started network={:?} poll={}ms coupon_poll={}ms",
            self.cfg.network, self.cfg.poll_interval_ms, self.cfg.coupon_poll_interval_ms
        );
        let ham = Ham::connect(self.cfg.admin_port, self.cfg.app_port, &self.cfg.app_id)
            .await
            .context("Failed to connect to Holochain")?;
        let lock_flow = LockFlow::new(self.cfg.clone(), self.db.clone());
        let coupon_flow = CouponFlow::new(self.cfg.clone(), self.db.clone());

        let mut last_flow = "coupon".to_string();
        let mut last_coupon_scan = std::time::Instant::now() - Duration::from_millis(self.cfg.coupon_poll_interval_ms);
        loop {
            if let Err(e) = lock_flow.run_cycle().await {
                error!("lock cycle failed: {}", e);
            }

            if last_coupon_scan.elapsed() >= Duration::from_millis(self.cfg.coupon_poll_interval_ms) {
                if let Err(e) = coupon_flow.run_cycle(&ham).await {
                    error!("coupon cycle failed: {}", e);
                }
                last_coupon_scan = std::time::Instant::now();
            }

            if let Some(item) = self.claim_round_robin(&last_flow)? {
                last_flow = item.flow.clone();
                self.db.mark_in_flight(item.id)?;
                let started = std::time::Instant::now();
                let started_event = if item.flow == "lock" {
                    "lock"
                } else {
                    "coupon"
                };
                info!(
                    "{} write started id={} task={} attempt={}/{}",
                    started_event, item.item_id, item.task_type, item.attempts, item.max_attempts
                );
                let result = self.process_item(&ham, &item).await;
                let duration_ms = started.elapsed().as_millis() as u64;
                match result {
                    Ok(()) => {
                        let success_event = if item.flow == "lock" {
                            "lock"
                        } else {
                            "coupon"
                        };
                        self.db.mark_succeeded(item.id)?;
                        info!(
                            "{} write succeeded id={} task={} duration={}ms",
                            success_event, item.item_id, item.task_type, duration_ms
                        );
                    }
                    Err(e) => {
                        self.handle_failure(&item, e.to_string(), duration_ms)?;
                    }
                }
            } else {
                let queue_depth = self.db.queue_depth_by_flow()?;
                let lock_depth = queue_depth
                    .iter()
                    .find(|(flow, _)| flow == "lock")
                    .map(|(_, n)| *n)
                    .unwrap_or(0);
                let coupon_depth = queue_depth
                    .iter()
                    .find(|(flow, _)| flow == "coupon")
                    .map(|(_, n)| *n)
                    .unwrap_or(0);
                info!("queue depth lock={} coupon={}", lock_depth, coupon_depth);
            }

            tokio::time::sleep(Duration::from_millis(self.cfg.poll_interval_ms)).await;
        }
    }

    fn claim_round_robin(&self, last_flow: &str) -> Result<Option<WorkItem>> {
        let preferred = if last_flow == "lock" { "coupon" } else { "lock" };
        if let Some(item) = self.db.claim_next(Some(preferred))? {
            return Ok(Some(item));
        }
        self.db.claim_next(None)
    }

    async fn process_item(&self, ham: &Ham, item: &WorkItem) -> Result<()> {
        match item.task_type.as_str() {
            "create_parked_link" => self.process_lock_create_parked_link(ham, item).await,
            "execute_rave" => self.process_coupon_execute_rave(ham, item).await,
            other => anyhow::bail!("Unknown task type: {}", other),
        }
    }

    async fn process_lock_create_parked_link(&self, ham: &Ham, item: &WorkItem) -> Result<()> {
        let payload = LockPayload::deserialize(item.payload_json.clone())?;
        let contract_hex = format!("{:x}", self.cfg.lock_vault_address);

        let depositor_wallet_address_as_hc_pubkey =
            decode_holochain_agent_as_pubkey_string(&payload.holochain_agent)?;
        let normalized = payload.normalized_amounts()?;
        let amount = normalized.amount_hot.clone();

        let proof = json!({
            "proof_of_deposit": {
                "method": "deposit",
                "contract_address": format!("0x{}", contract_hex.to_lowercase()),
                "amount": amount,
                "depositor_wallet_address": depositor_wallet_address_as_hc_pubkey
            }
        });

        let parked_link_tag = ParkedData {
            ct_role_id: "oracle".to_string(),
            amount: Some(UnitMap::from(vec![(self.cfg.unit_index, amount.as_str())])),
            payload: proof,
        };

        let zome_payload = CreateParkedLinkInput {
            ea_id: self.cfg.credit_limit_ea_id.clone().into(),
            executor: Some(self.cfg.bridging_agent_pubkey.clone().into()),
            parked_link_type: ParkedLinkType::ParkedData((parked_link_tag, true)),
        };
        info!(
            "zome call start id={} zome=transactor.create_parked_link amount_hot={} amount_raw_wei={} agent={}",
            payload.lock_id, amount, normalized.amount_raw_wei, payload.holochain_agent
        );
        let zome_result: (ActionHashB64, AgentPubKey) = ham
            .call_zome(
                &self.cfg.role_name,
                "transactor",
                "create_parked_link",
                &zome_payload,
            )
            .await?;
        info!(
            "zome call success id={} zome=transactor.create_parked_link action_hash={} executor={}",
            payload.lock_id,
            zome_result.0,
            zome_result.1
        );

        info!(
            "create_parked_link succeeded for lock={}, continuing inline to initiate_deposit",
            payload.lock_id
        );
        self.process_lock_initiate_deposit(ham).await
    }

    async fn process_lock_initiate_deposit(&self, ham: &Ham) -> Result<()> {
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
                "Skipping initiate_deposit: configured bridging agent does not match lane/global bridging agent"
            );
            return Ok(());
        }

        let credit_limit_ea_id: ActionHash = context.credit_limit_adjustment.clone().into();
        let parked_links: Vec<Transaction> = ham
            .call_zome(
                &self.cfg.role_name,
                "transactor",
                "get_parked_links_by_ea",
                &credit_limit_ea_id,
            )
            .await?;
        if parked_links.is_empty() {
            info!("initiate_deposit no-op: no parked links on credit limit agreement");
            return Ok(());
        }

        let _: (RAVE, ActionHash) = ham
            .call_zome(
                &self.cfg.role_name,
                "transactor",
                "execute_rave",
                &RAVEExecuteInputs {
                    ea_id: context.credit_limit_adjustment.clone().into(),
                    executor_inputs: Value::Null,
                    links: vec![],
                    global_definition: global_definition.id.clone().into(),
                    lane_definitions: context.lane_definitions.clone(),
                    strategy: GetStrategy::Local,
                },
            )
            .await?;

        let mut prepared_links = Vec::new();
        for link in parked_links {
            if let TransactionDetails::Parked {
                attached_payload, ..
            } = link.details.clone()
            {
                if let Some(proof_of_deposit) = attached_payload.get("proof_of_deposit") {
                    let proof = proof_of_deposit.clone();
                    let proof_size_bytes = serde_json::to_vec(&proof)
                        .context("Failed to serialize proof_of_deposit for size estimate")?
                        .len();
                    prepared_links.push(PreparedDepositLink {
                        transaction: link,
                        proof_of_deposit: proof,
                        proof_size_bytes,
                    });
                }
            }
        }
        if prepared_links.is_empty() {
            info!("initiate_deposit no-op: parked links missing proof_of_deposit");
            return Ok(());
        }

        let target_bytes = kb_to_bytes(self.cfg.deposit_batch_target_kb);
        let selected_index = select_single_link_index(prepared_links.len());
        let Some(selected_index) = selected_index else {
            info!("initiate_deposit no-op: no selectable links in single-link mode");
            return Ok(());
        };
        let selected = prepared_links
            .get(selected_index)
            .context("invalid single-link index while selecting link")?;
        let selected_links_count = 1usize;
        let pod_inputs = vec![selected.proof_of_deposit.clone()];
        let amounts = vec![selected.transaction.amount.clone()];
        let estimated_bytes = selected.proof_size_bytes;
        let invoiced_amount = accumulate_amounts(&amounts)?;
        if invoiced_amount.is_zero() {
            info!("initiate_deposit no-op: selected links aggregated to zero amount");
            return Ok(());
        }

        info!(
            "initiate_deposit single-link selected total_links={} selected_links={} estimated_bytes={} batch_target_bytes={} selection_mode=single_link",
            prepared_links.len(),
            selected_links_count,
            estimated_bytes,
            target_bytes
        );

        let create_payload = CreateParkedSpendInput {
            ea_id: context.bridging_agreement.clone().into(),
            executor: Some(self.cfg.bridging_agent_pubkey.clone().into()),
            ct_role_id: Some("bridging_agent".to_string()),
            amount: invoiced_amount.clone(),
            spender_payload: json!({
                "proof_of_deposit": pod_inputs,
            }),
            lane_definitions: context.lane_definitions.clone(),
        };
        let parked_spend_link_id: ActionHashB64 = ham
            .call_zome(
                &self.cfg.role_name,
                "transactor",
                "create_parked_spend",
                &create_payload,
            )
            .await?;

        let bridging_ea_id: ActionHash = context.bridging_agreement.clone().into();
        let bridging_links: Vec<Transaction> = ham
            .call_zome(
                &self.cfg.role_name,
                "transactor",
                "get_parked_links_by_ea",
                &bridging_ea_id,
            )
            .await?;
        let parked_spend_tx = bridging_links
            .into_iter()
            .find(|tx| tx.id == parked_spend_link_id)
            .context("created parked spend link not found in bridging agreement links")?;

        let rave_result: (RAVE, ActionHash) = ham
            .call_zome(
                &self.cfg.role_name,
                "transactor",
                "execute_rave",
                &RAVEExecuteInputs {
                    ea_id: context.bridging_agreement.into(),
                    executor_inputs: json!({
                        "call_method": "deposit",
                        "coupon": "0",
                    }),
                    links: vec![parked_spend_tx],
                    global_definition: global_definition.id.clone().into(),
                    lane_definitions: context.lane_definitions,
                    strategy: GetStrategy::Local,
                },
            )
            .await?;
        info!(
            "initiate_deposit success action_hash={} selected_links={} estimated_bytes={} selection_mode=single_link",
            rave_result.1,
            selected_links_count,
            estimated_bytes
        );
        Ok(())
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

    async fn process_coupon_execute_rave(&self, ham: &Ham, item: &WorkItem) -> Result<()> {
        let tx: Transaction = serde_json::from_value(item.payload_json.clone())
            .context("deserialize parked transaction")?;

        let (recipient, global_definition, lane_definitions, amount) = if let TransactionDetails::ParkedSpend {
            attached_payload,
            global_definition,
            lane_definitions,
            ..
        } = tx.clone().details
        {
            #[derive(Serialize, Deserialize)]
            struct SpenderPayload {
                withdraw_to_address: String,
            }
            let payload = serde_json::from_value::<SpenderPayload>(attached_payload)
                .context("deserialize parked spender payload")?;
            let amount = tx
                .clone()
                .amount
                .get("1")
                .map(|v| v.to_string())
                .unwrap_or_default();
            (
                payload.withdraw_to_address,
                global_definition,
                lane_definitions,
                amount,
            )
        } else {
            return Ok(());
        };

        let signer_ctx = signer_context_from_env()?;
        info!(
            "coupon generation start tx_id={:?} recipient={} amount_raw={}",
            item.item_id, recipient, amount
        );
        let coupon = generate_coupon(&amount, &recipient, &signer_ctx).await?;
        info!(
            "coupon generated tx_id={:?} recipient={} amount_raw={}",
            item.item_id, recipient, amount
        );

        let payload = RAVEExecuteInputs {
            ea_id: ActionHashB64::from_str(&self.cfg.bridging_agreement_id)
                .context("Invalid BRIDGING_AGREEMENT_ID")?
                .into(),
            executor_inputs: json!({
                "call_method": "withdraw",
                "coupon": coupon
            }),
            links: vec![tx],
            global_definition: global_definition.into(),
            lane_definitions: lane_definitions.iter().map(|ld| ld.clone().into()).collect(),
            strategy: holochain_zome_types::entry::GetStrategy::Network,
        };
        info!(
            "zome call start tx_id={:?} zome=transactor.execute_rave recipient={} amount_raw={}",
            item.item_id, recipient, amount
        );
        let zome_result: (RAVE, ActionHash) = ham
            .call_zome(&self.cfg.role_name, "transactor", "execute_rave", &payload)
            .await?;
        info!(
            "zome call success tx_id={:?} zome=transactor.execute_rave action_hash={}",
            item.item_id, zome_result.1
        );
        Ok(())
    }

    fn handle_failure(&self, item: &WorkItem, err: String, duration_ms: u64) -> Result<()> {
        let error_class = classify_error(&err);
        if error_class == "transient" {
            let now = now_unix_secs();
            let delay_secs = compute_retry_delay_secs(item.attempts);
            let next_retry_at = now + delay_secs;
            if self.db.schedule_retry(item.id, &err, next_retry_at)? {
                warn!(
                    "retry scheduled flow={} id={} task={} attempt={}/{} class={} next_retry_at={} delay={}ms duration={}ms error={}",
                    item.flow,
                    item.item_id,
                    item.task_type,
                    item.attempts,
                    item.max_attempts,
                    error_class,
                    next_retry_at,
                    delay_secs * 1000,
                    duration_ms,
                    err
                );
                return Ok(());
            }
        }

        self.db.mark_failed_terminal(item.id, &err, error_class)?;
        warn!(
            "{} write failed id={} task={} attempt={}/{} class={} duration={}ms error={}",
            item.flow, item.item_id, item.task_type, item.attempts, item.max_attempts, error_class, duration_ms, err
        );
        if item.attempts >= item.max_attempts {
            warn!(
                "retry exhausted flow={} id={} task={} attempts={}",
                item.flow, item.item_id, item.task_type, item.max_attempts
            );
        }
        Ok(())
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

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

fn classify_error(err: &str) -> &'static str {
    let e = err.to_lowercase();
    let permanent_signals = [
        "invalid",
        "deserialize",
        "expected 32 byte agent key",
        "unknown task type",
        "invalid bridging_agreement_id",
    ];
    if permanent_signals.iter().any(|s| e.contains(s)) {
        "permanent"
    } else {
        "transient"
    }
}

fn compute_retry_delay_secs(attempts: i64) -> i64 {
    let base = 5_i64;
    let max_delay = 900_i64;
    let pow = (attempts.saturating_sub(1)).clamp(0, 16) as u32;
    let delay = base.saturating_mul(2_i64.saturating_pow(pow));
    delay.clamp(base, max_delay)
}

#[derive(Clone)]
struct PreparedDepositLink {
    transaction: Transaction,
    proof_of_deposit: Value,
    proof_size_bytes: usize,
}

struct DepositContext {
    lane_definitions: Vec<ActionHash>,
    bridging_agent: holo_hash::AgentPubKeyB64,
    credit_limit_adjustment: ActionHashB64,
    bridging_agreement: ActionHashB64,
}

fn kb_to_bytes(kb: u64) -> usize {
    kb.saturating_mul(1024).clamp(1, usize::MAX as u64) as usize
}

fn select_single_link_index(link_count: usize) -> Option<usize> {
    if link_count > 0 {
        Some(0)
    } else {
        None
    }
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
    fn classifies_permanent_errors() {
        assert_eq!(classify_error("Invalid BRIDGING_AGREEMENT_ID"), "permanent");
        assert_eq!(classify_error("Failed to deserialize response"), "permanent");
    }

    #[test]
    fn classifies_transient_errors() {
        assert_eq!(classify_error("websocket disconnected"), "transient");
        assert_eq!(classify_error("timeout while calling zome"), "transient");
    }

    #[test]
    fn backoff_is_bounded() {
        assert_eq!(compute_retry_delay_secs(1), 5);
        assert_eq!(compute_retry_delay_secs(2), 10);
        assert!(compute_retry_delay_secs(12) <= 900);
    }

    #[test]
    fn selects_first_link_when_available() {
        let index = select_single_link_index(3);
        assert_eq!(index, Some(0));
    }

    #[test]
    fn selects_none_when_no_links() {
        let index = select_single_link_index(0);
        assert_eq!(index, None);
    }

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
