use crate::config::Config;
use crate::ham::{self, Ham};
use crate::lock_flow::{format_amount, LockFlow};
use crate::signer::{generate_coupon, signer_context_from_env};
use crate::state::{StateStore, WorkItem, WorkState};
use anyhow::{Context, Result};
use holo_hash::{ActionHash, ActionHashB64, AgentPubKey};
use holochain_zome_types::entry::GetStrategy;
use rave_engine::types::{
    CarryForwardUnits, CreateParkedLinkInput, CreateParkedSpendInput, GlobalDefinitionExt, LaneExt,
    ParkedData, ParkedLinkType, ParkedSpendData, RAVEExecuteInputs, Transaction, TransactionDetails,
    UnitMap, RAVE,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};
use zfuel::fuel::ZFuel;

/// Receiver side of the shutdown signal. Set to `true` when the process
/// receives Ctrl+C or SIGTERM.
type ShutdownRx = watch::Receiver<bool>;

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

        let mut shutdown = install_shutdown_handler();

        let mut ham = match connect_ham_with_backoff(&self.cfg, &mut shutdown).await {
            Some(h) => h,
            None => {
                info!("[bridge] shutdown received before initial connect, exiting");
                return Ok(());
            }
        };
        let lock_flow = LockFlow::new(self.cfg.clone(), self.db.clone());

        let mut last_bridge_cycle =
            std::time::Instant::now() - Duration::from_millis(self.cfg.bridge_cycle_interval_ms);

        loop {
            if *shutdown.borrow() {
                info!("[bridge] shutdown signal received, exiting cleanly");
                return Ok(());
            }

            if let Err(e) = lock_flow.run_cycle().await {
                error!("[lock-flow] cycle failed: {}", e);
            }

            if *shutdown.borrow() {
                info!("[bridge] shutdown signal received, exiting cleanly");
                return Ok(());
            }

            if last_bridge_cycle.elapsed()
                >= Duration::from_millis(self.cfg.bridge_cycle_interval_ms)
            {
                // Pre-cycle health probe: surfaces dead sockets before we
                // start a multi-step write sequence. If the probe fails for
                // connection-like reasons, reconnect and skip this iteration
                // (the cycle will retry next pass).
                match ham.ping().await {
                    Ok(()) => match self.run_bridge_cycle(&ham).await {
                        Ok(()) => {
                            last_bridge_cycle = std::time::Instant::now();
                        }
                        Err(e) => {
                            error!("[bridge] cycle failed: {}", e);
                            if let Err(reset_err) =
                                self.db.reset_in_flight_to_queued("lock", &e.to_string())
                            {
                                error!(
                                    "[bridge] failed to reset in_flight locks: {}",
                                    reset_err
                                );
                            }
                            if ham::is_connection_error(&e) {
                                warn!(event = "ham.disconnected", error = %e);
                                match connect_ham_with_backoff(&self.cfg, &mut shutdown).await
                                {
                                    Some(new_ham) => ham = new_ham,
                                    None => return Ok(()),
                                }
                            }
                        }
                    },
                    Err(e) => {
                        if ham::is_connection_error(&e) {
                            warn!(event = "ham.probe.failed", error = %e);
                            match connect_ham_with_backoff(&self.cfg, &mut shutdown).await {
                                Some(new_ham) => ham = new_ham,
                                None => return Ok(()),
                            }
                        } else {
                            warn!(
                                "[bridge] probe failed with non-connection error: {}",
                                e
                            );
                        }
                    }
                }
            }

            // Interruptible poll-interval sleep so shutdown is observed
            // promptly (never mid-write).
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(self.cfg.poll_interval_ms)) => {}
                _ = shutdown.changed() => {}
            }
        }
    }

    /// Single unified bridge cycle that handles deposits and withdrawals together.
    ///
    /// 1. Size-capped extraction of deposit proofs from queued locks
    /// 2. ONE create_parked_link on credit limit EA (batched proof array)
    /// 3. ONE credit limit RAVE with explicit links
    /// 4. ONE create_parked_spend on bridging EA (aggregated proof list)
    /// 5. Scan bridging EA, size-capped withdrawal coupon generation
    /// 6. ONE unified bridging RAVE with explicit links and coupons map
    async fn run_bridge_cycle(&self, ham: &Ham) -> Result<()> {
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
                "[bridge] skipping cycle: configured bridging agent does not match lane/global"
            );
            return Ok(());
        }

        // --- Check for work before committing to a full cycle ---
        let tag_cap = self.cfg.max_link_tag_bytes;
        let coupons_budget = self.cfg.coupons_target_bytes;
        let queued_locks = self.db.list_work_items("lock", WorkState::Queued, 5000)?;

        if queued_locks.is_empty() {
            let bridging_ea_id: ActionHash = context.bridging_agreement.clone().into();
            let bridging_links: Vec<Transaction> = ham
                .call_zome(
                    &self.cfg.role_name,
                    "transactor",
                    "get_parked_links_by_ea",
                    &bridging_ea_id,
                )
                .await?;
            if bridging_links.is_empty() {
                let duration_ms = started.elapsed().as_millis() as u64;
                debug!(
                    "[bridge] cycle no-op (no pending deposits or withdrawals) duration={}ms",
                    duration_ms
                );
                return Ok(());
            }
        }

        // --- Tag-size-aware deposit extraction ---
        //
        // A single `create_parked_link` / `create_parked_spend` call packs ALL
        // selected proofs into ONE link tag (msgpack-encoded ParkedData /
        // ParkedSpendData). Holochain rejects any link tag larger than
        // MAX_TAG_SIZE (1000 bytes). We measure the actual tentative tag size
        // as we add each proof, and stop when either the ParkedData tag or the
        // (larger) ParkedSpendData tag would exceed the configured cap.
        let mut deposit_proofs: Vec<Value> = Vec::new();
        let mut deposit_amounts: Vec<UnitMap> = Vec::new();
        let mut processed_lock_ids: Vec<i64> = Vec::new();
        let mut last_tag_bytes: usize = 0;
        let mut batch_capped = false;

        info!(
            "[bridge] \u{2500}\u{2500} cycle started \u{2500}\u{2500} locks_queued={} link_tag_cap={}",
            queued_locks.len(),
            tag_cap
        );

        let global_definition_hash: ActionHash = global_definition.id.clone().into();

        for item in &queued_locks {
            let (proof, amount) = match self.extract_lock_proof(item) {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        "[bridge/deposits] proof extraction failed id={} error={}, skipping",
                        item.item_id, e
                    );
                    continue;
                }
            };

            let mut tentative_proofs = deposit_proofs.clone();
            tentative_proofs.push(proof.clone());
            let mut tentative_amounts = deposit_amounts.clone();
            tentative_amounts.push(amount.clone());
            let tentative_total = accumulate_amounts(&tentative_amounts)?;
            let tentative_payload = json!({ "proof_of_deposit": &tentative_proofs });

            let tag_bytes = match estimate_link_tag_bytes(
                &tentative_total,
                &tentative_payload,
                &global_definition_hash,
                &context.lane_definitions,
            ) {
                Ok(n) => n,
                Err(e) => {
                    warn!(
                        "[bridge/deposits] failed to estimate tag size id={} error={}, skipping",
                        item.item_id, e
                    );
                    continue;
                }
            };

            if tag_bytes > tag_cap {
                if deposit_proofs.is_empty() {
                    // Single proof already exceeds cap: skip this lock rather than
                    // block the head of the queue forever.
                    warn!(
                        "[bridge/deposits] single proof exceeds link tag cap (size={} > cap={}), skipping lock id={}",
                        tag_bytes, tag_cap, item.item_id
                    );
                    continue;
                }
                info!(
                    "[bridge/deposits] batch cap reached at {} proofs (next tag would be {} bytes > cap {})",
                    deposit_proofs.len(),
                    tag_bytes,
                    tag_cap
                );
                batch_capped = true;
                break;
            }

            last_tag_bytes = tag_bytes;
            deposit_proofs.push(proof);
            deposit_amounts.push(amount);
            processed_lock_ids.push(item.id);
        }

        if !batch_capped {
            info!(
                "[bridge/deposits] batch: selected={}/{} tag_bytes={} (all fit within cap {})",
                deposit_proofs.len(),
                queued_locks.len(),
                last_tag_bytes,
                tag_cap
            );
        } else {
            info!(
                "[bridge/deposits] batch: selected={}/{} tag_bytes={} ({} deferred to next cycle)",
                deposit_proofs.len(),
                queued_locks.len(),
                last_tag_bytes,
                queued_locks.len() - deposit_proofs.len()
            );
        }

        // --- Mark selected locks as in_flight ---
        for lock_id in &processed_lock_ids {
            self.db.mark_in_flight(*lock_id)?;
        }

        if !processed_lock_ids.is_empty() {
            let total_deposit_amount = accumulate_amounts(&deposit_amounts)?;

            let parked_data = ParkedData {
                ct_role_id: "oracle".to_string(),
                amount: Some(total_deposit_amount.clone()),
                payload: json!({ "proof_of_deposit": deposit_proofs.clone() }),
            };

            let link_result: (ActionHashB64, AgentPubKey) = ham
                .call_zome(
                    &self.cfg.role_name,
                    "transactor",
                    "create_parked_link",
                    &CreateParkedLinkInput {
                        ea_id: context.credit_limit_adjustment.clone().into(),
                        executor: Some(self.cfg.bridging_agent_pubkey.clone().into()),
                        parked_link_type: ParkedLinkType::ParkedData((parked_data, true)),
                    },
                )
                .await?;

            info!(
                "[bridge/credit-limit] create_parked_link: {} proofs, action_hash={}",
                processed_lock_ids.len(),
                link_result.0
            );

            // --- Explicit links for credit limit RAVE ---
            let credit_limit_ea_id: ActionHash =
                context.credit_limit_adjustment.clone().into();
            let cl_links: Vec<Transaction> = ham
                .call_zome(
                    &self.cfg.role_name,
                    "transactor",
                    "get_parked_links_by_ea",
                    &credit_limit_ea_id,
                )
                .await?;

            info!(
                "[bridge/credit-limit] RAVE: consuming {} explicit links",
                cl_links.len()
            );

            let _: (RAVE, ActionHash) = ham
                .call_zome(
                    &self.cfg.role_name,
                    "transactor",
                    "execute_rave",
                    &RAVEExecuteInputs {
                        ea_id: credit_limit_ea_id,
                        executor_inputs: Value::Null,
                        links: cl_links,
                        global_definition: global_definition.id.clone().into(),
                        lane_definitions: context.lane_definitions.clone(),
                        strategy: GetStrategy::Local,
                    },
                )
                .await?;
            info!("[bridge/credit-limit] RAVE executed");

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
                            amount: total_deposit_amount.clone(),
                            spender_payload: json!({
                                "proof_of_deposit": deposit_proofs,
                            }),
                            lane_definitions: context.lane_definitions.clone(),
                        },
                    )
                    .await?;
                info!(
                    "[bridge/bridging] create_parked_spend: total_amount={:?}",
                    total_deposit_amount
                );
            }
        }

        // --- Scan bridging EA and size-capped withdrawal selection ---
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
        let mut selected_withdrawal_links: Vec<Transaction> = Vec::new();
        let mut deposit_rave_links: Vec<Transaction> = Vec::new();
        let mut coupon_cumulative_bytes: usize = 0;
        let mut total_withdrawals_found: usize = 0;
        let mut withdrawal_capped = false;

        for tx in &bridging_links {
            if let TransactionDetails::ParkedSpend {
                attached_payload, ..
            } = &tx.details
            {
                if attached_payload.get("proof_of_deposit").is_some() {
                    deposit_rave_links.push(tx.clone());
                    continue;
                }

                if let Some(withdraw_to) = attached_payload
                    .get("withdraw_to_address")
                    .and_then(|v| v.as_str())
                {
                    total_withdrawals_found += 1;

                    if withdrawal_capped {
                        continue;
                    }

                    let amount = tx
                        .amount
                        .get("1")
                        .map(|v| v.to_string())
                        .unwrap_or_default();

                    let signer_ctx = signer_context_from_env()?;
                    let coupon = generate_coupon(&amount, withdraw_to, &signer_ctx).await?;
                    let key = tx.id.to_string();

                    let entry_bytes = serde_json::to_vec(&json!({ &key: &coupon }))
                        .map(|v| v.len())
                        .unwrap_or(0);

                    if coupon_cumulative_bytes + entry_bytes > coupons_budget
                        && !selected_withdrawal_links.is_empty()
                    {
                        withdrawal_capped = true;
                        info!(
                            "[bridge/withdrawals] batch: cap reached at {} coupons, coupon_bytes={}",
                            selected_withdrawal_links.len(),
                            coupon_cumulative_bytes
                        );
                        continue;
                    }

                    coupon_cumulative_bytes += entry_bytes;
                    coupons_map.insert(key, Value::String(coupon));
                    selected_withdrawal_links.push(tx.clone());

                    info!(
                        "[bridge/withdrawals] generating coupon tx_id={:?} recipient={} amount={}",
                        tx.id, withdraw_to, amount
                    );
                }
            }
        }

        let withdrawal_count = selected_withdrawal_links.len();
        let deferred_withdrawals = total_withdrawals_found - withdrawal_count;
        info!(
            "[bridge/withdrawals] scan: found={} selected={}/{} coupon_bytes={} deferred={}",
            total_withdrawals_found,
            withdrawal_count,
            total_withdrawals_found,
            coupon_cumulative_bytes,
            deferred_withdrawals
        );

        // --- Explicit links for bridging RAVE ---
        let mut rave_links: Vec<Transaction> = Vec::new();
        rave_links.extend(deposit_rave_links.iter().cloned());
        rave_links.extend(selected_withdrawal_links);

        if !rave_links.is_empty() {
            info!(
                "[bridge/bridging] RAVE: {} deposit + {} withdrawal links",
                deposit_rave_links.len(),
                withdrawal_count
            );

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
                        links: rave_links,
                        global_definition: global_definition.id.clone().into(),
                        lane_definitions: context.lane_definitions,
                        strategy: GetStrategy::Local,
                    },
                )
                .await?;
            info!(
                "[bridge/bridging] RAVE executed action_hash={}",
                rave_result.1,
            );
        } else if processed_lock_ids.is_empty() {
            debug!("[bridge] cycle no-op: no pending links on bridging EA");
        }

        // --- Mark processed locks as succeeded ---
        for lock_id in &processed_lock_ids {
            self.db.mark_succeeded(*lock_id)?;
        }
        if !processed_lock_ids.is_empty() {
            info!("[bridge] marked {} locks as succeeded", processed_lock_ids.len());
        }

        let duration_ms = started.elapsed().as_millis() as u64;
        info!(
            "[bridge] \u{2500}\u{2500} cycle completed \u{2500}\u{2500} duration={}ms locks={} withdrawals={}",
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
            "[bridge/deposits] extracted proof id={} amount={} agent={}",
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
    amount_hot: String,
}

impl LockPayload {
    fn normalized_amounts(&self) -> Result<NormalizedLockAmount> {
        let amount_hot = self
            .amount_hot
            .clone()
            .or_else(|| amount_from_legacy_field(self.amount.clone()))
            .or_else(|| {
                let raw = self.amount_raw_wei.as_ref().or(self.amount.as_ref())?;
                Some(format_amount(raw))
            })
            .context("cannot determine amount_hot: no amount_hot, amount, or amount_raw_wei")?;
        validate_hot_amount(&amount_hot)?;
        Ok(NormalizedLockAmount { amount_hot })
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

/// Spawn a background task that flips the returned watch receiver to `true`
/// once the process receives Ctrl+C or SIGTERM. The initial value is marked
/// as seen so subsequent `.changed()` calls only complete on a real signal.
fn install_shutdown_handler() -> ShutdownRx {
    let (tx, mut rx) = watch::channel(false);
    rx.mark_unchanged();

    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            match signal(SignalKind::terminate()) {
                Ok(mut sigterm) => {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {
                            info!("received SIGINT, initiating graceful shutdown");
                        }
                        _ = sigterm.recv() => {
                            info!("received SIGTERM, initiating graceful shutdown");
                        }
                    }
                }
                Err(e) => {
                    warn!("failed to install SIGTERM handler, falling back to Ctrl+C only: {}", e);
                    let _ = tokio::signal::ctrl_c().await;
                    info!("received SIGINT, initiating graceful shutdown");
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            info!("received Ctrl+C, initiating graceful shutdown");
        }
        let _ = tx.send(true);
    });

    rx
}

/// Establish a fresh `Ham` connection. Thin wrapper that centralizes the
/// connect call so both startup and reconnect flows share one path.
async fn connect_ham(cfg: &Config) -> Result<Ham> {
    Ham::connect(
        cfg.admin_port,
        cfg.app_port,
        &cfg.app_id,
        cfg.ham_request_timeout_secs,
    )
    .await
    .context("Failed to connect to Holochain")
}

/// Loop forever (until shutdown) trying to establish a `Ham` connection.
/// Uses exponential backoff capped at `ham_reconnect_backoff_max_ms` with
/// small jitter. Logs each failed attempt at `warn!` and escalates to
/// `error!` once the consecutive attempt count passes
/// `ham_reconnect_escalate_after`, so operator alerts can fire while we
/// keep trying.
///
/// Returns `None` if a shutdown signal arrives while we are waiting or
/// retrying, allowing the caller to exit cleanly.
async fn connect_ham_with_backoff(cfg: &Config, shutdown: &mut ShutdownRx) -> Option<Ham> {
    let mut attempt: u32 = 0;
    loop {
        if *shutdown.borrow() {
            return None;
        }
        match connect_ham(cfg).await {
            Ok(ham) => {
                if attempt > 0 {
                    info!(event = "ham.reconnected", attempts = attempt);
                }
                return Some(ham);
            }
            Err(e) => {
                let delay_ms = compute_backoff_ms(attempt, cfg);
                if attempt >= cfg.ham_reconnect_escalate_after {
                    error!(
                        event = "ham.reconnect.attempt",
                        attempt,
                        delay_ms,
                        error = %e,
                        "reconnect failing persistently; operator attention needed"
                    );
                } else {
                    warn!(
                        event = "ham.reconnect.attempt",
                        attempt,
                        delay_ms,
                        error = %e,
                    );
                }
                attempt = attempt.saturating_add(1);

                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => {}
                    _ = shutdown.changed() => { return None; }
                }
            }
        }
    }
}

/// Compute the next reconnect delay: exponential (1,2,4,... *initial) capped
/// at `ham_reconnect_backoff_max_ms`, plus up to 10% jitter seeded from the
/// system clock's sub-second nanos (no extra crate dependency required).
fn compute_backoff_ms(attempt: u32, cfg: &Config) -> u64 {
    let shift = attempt.min(20);
    let factor = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    let base = cfg
        .ham_reconnect_backoff_initial_ms
        .saturating_mul(factor);
    let capped = base.min(cfg.ham_reconnect_backoff_max_ms);
    let jitter_range = (capped / 10).max(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    capped.saturating_add(nanos % jitter_range)
}

/// Estimate the worst-case msgpack link-tag size this batch will produce.
///
/// The orchestrator creates two links from the same proofs-of-deposit payload:
///   1. `ParkedData` on the credit-limit EA (via `create_parked_link`)
///   2. `ParkedSpendData` on the bridging EA (via `create_parked_spend`)
///
/// `ParkedSpendData` carries extra fields (global_definition, new_balance,
/// proposed_balance, carry_forward_units, fees_owed, lane_definitions) so its
/// tag is strictly larger. We compute both and return the max. Unknown
/// runtime-computed fields (new_balance, proposed_balance, etc.) are filled
/// with worst-case-sized placeholders derived from `total_amount`.
fn estimate_link_tag_bytes(
    total_amount: &UnitMap,
    payload: &Value,
    global_definition: &ActionHash,
    lane_definitions: &[ActionHash],
) -> Result<usize> {
    let parked_data = (
        ParkedData {
            ct_role_id: "oracle".to_string(),
            amount: Some(total_amount.clone()),
            payload: payload.clone(),
        },
        true,
    );
    let parked_data_tag = rmp_serde::to_vec(&parked_data)
        .context("failed to msgpack-encode ParkedData for tag-size estimation")?
        .len();

    let parked_spend = ParkedSpendData {
        ct_role_id: "bridging_agent".to_string(),
        amount: total_amount.clone(),
        payload: payload.clone(),
        global_definition: global_definition.clone(),
        lane_definitions: lane_definitions.to_vec(),
        new_balance: total_amount.clone(),
        carry_forward_units: CarryForwardUnits::new(),
        fees_owed: ZFuel::zero(),
        proposed_balance: total_amount.clone(),
    };
    let parked_spend_tag = rmp_serde::to_vec(&parked_spend)
        .context("failed to msgpack-encode ParkedSpendData for tag-size estimation")?
        .len();

    Ok(parked_data_tag.max(parked_spend_tag))
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
