use crate::config::Config;
use crate::lock_flow::{format_amount, LockFlow};
use crate::signer::{generate_coupon, signer_context_from_env};
use crate::state::{StateStore, WorkItem, WorkState};
use anyhow::{Context, Result};
use ham::{
    connect_with_backoff, install_shutdown_handler, is_connection_error, is_source_chain_pressure,
    BackoffConfig, Ham, HamConfig,
};
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
use tracing::{debug, error, info, warn};
use zfuel::fuel::ZFuel;

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
        let backoff = backoff_config(&self.cfg);

        let mut ham = match connect_with_backoff(
            || connect_ham(&self.cfg),
            &backoff,
            &mut shutdown,
        )
        .await
        {
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
                            if is_connection_error(&e) {
                                warn!(event = "ham.disconnected", error = %e);
                                match connect_with_backoff(
                                    || connect_ham(&self.cfg),
                                    &backoff,
                                    &mut shutdown,
                                )
                                .await
                                {
                                    Some(new_ham) => ham = new_ham,
                                    None => return Ok(()),
                                }
                            } else if is_source_chain_pressure(&e) {
                                // Server-side workflow timeout / source-chain
                                // backpressure. Socket is healthy, so we keep
                                // it open and simply pause before the next
                                // cycle instead of retrying at full tempo.
                                // The lock itself is already queued for retry
                                // by `reset_in_flight_to_queued` above, and
                                // the idempotent pre-scans in
                                // `run_bridge_cycle` prevent a duplicate
                                // parked entry if the prior write actually
                                // committed despite the elapsed deadline.
                                warn!(
                                    event = "ham.source_chain_pressure",
                                    cooldown_ms = self.cfg.ham_pressure_cooldown_ms,
                                    error = %e
                                );
                                let cooldown =
                                    Duration::from_millis(self.cfg.ham_pressure_cooldown_ms);
                                tokio::select! {
                                    _ = tokio::time::sleep(cooldown) => {}
                                    _ = shutdown.changed() => {}
                                }
                            }
                        }
                    },
                    Err(e) => {
                        if is_connection_error(&e) {
                            warn!(event = "ham.probe.failed", error = %e);
                            match connect_with_backoff(
                                || connect_ham(&self.cfg),
                                &backoff,
                                &mut shutdown,
                            )
                            .await
                            {
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
    /// The sequence is idempotent against the Holochain source chain. Before
    /// issuing any `create_parked_*` write we pre-scan both EAs and build a
    /// [`DedupIndex`] keyed by the EVM `tx_hash` embedded in every
    /// `proof_of_deposit`; any lock whose work has already been committed is
    /// either skipped at the appropriate step or marked succeeded locally.
    ///
    /// The "applied RAVE history" half of the scan is gated on
    /// `attempts > 0` so the steady-state cycle pays exactly the same zome
    /// calls it always did.
    ///
    /// 1. Resolve context + always-on live parked pre-scan (both EAs)
    /// 2. Conditional applied-RAVE scan (only if any lock is a retry)
    /// 3. Per-lock decision table → two size-capped proof batches
    /// 4. ONE create_parked_link (if any lock still needs the CL link)
    /// 5. ONE credit limit RAVE over a freshly re-fetched live link set
    /// 6. ONE create_parked_spend (if any lock still needs the bridging spend)
    /// 7. Scan bridging EA, size-capped withdrawal coupon generation
    /// 8. ONE unified bridging RAVE with explicit links and coupons map
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

        let tag_cap = self.cfg.max_link_tag_bytes;
        let coupons_budget = self.cfg.coupons_target_bytes;

        // Per-cycle safety valve: promote any queued lock that has already
        // exhausted its retry budget to `failed` before we fetch the work
        // list. Without this a permanently broken lock would keep retrying
        // forever in a long-running session (the `recover_stale_items`
        // equivalent only runs at startup).
        let promoted = self.db.fail_exhausted_queued("lock")?;
        if promoted > 0 {
            warn!(
                "[bridge] promoted {} queued lock(s) to failed (attempts >= max_attempts)",
                promoted
            );
        }

        let queued_locks = self.db.list_work_items("lock", WorkState::Queued, 5000)?;
        let credit_limit_ea_id: ActionHash = context.credit_limit_adjustment.clone().into();
        let bridging_ea_id: ActionHash = context.bridging_agreement.clone().into();
        let global_definition_hash: ActionHash = global_definition.id.clone().into();

        // --- Always-on pre-scan: live parked entries on both EAs ---
        //
        // These two calls are required anyway (we previously invoked
        // `get_parked_links_by_ea` right before each RAVE); hoisting them to
        // the top of the cycle lets us use their results for dedup AND for
        // the subsequent RAVE, so steady-state cost is unchanged.
        let cl_parked: Vec<Transaction> = ham
            .call_zome(
                &self.cfg.role_name,
                "transactor",
                "get_parked_links_by_ea",
                &credit_limit_ea_id,
            )
            .await?;
        let br_parked: Vec<Transaction> = ham
            .call_zome(
                &self.cfg.role_name,
                "transactor",
                "get_parked_links_by_ea",
                &bridging_ea_id,
            )
            .await?;

        // --- Early exit: nothing anywhere to work on ---
        if queued_locks.is_empty() && cl_parked.is_empty() && br_parked.is_empty() {
            let duration_ms = started.elapsed().as_millis() as u64;
            debug!(
                "[bridge] cycle no-op (no pending deposits or withdrawals) duration={}ms",
                duration_ms
            );
            return Ok(());
        }

        // --- Conditional deep scan: applied RAVE history ---
        //
        // Expensive (bounded by EA history, grows over time). Only run when
        // at least one queued lock has been tried before — by construction a
        // never-retried lock cannot have committed on-chain state under a
        // consumed parked link, so there is nothing for the RAVE-history
        // scan to find.
        let retry_lock_count = queued_locks.iter().filter(|l| l.attempts > 0).count();
        let needs_deep_dedup = queued_locks_need_deep_dedup(&queued_locks);
        let (cl_raves, br_raves) = if needs_deep_dedup {
            let cl_raves: Vec<Transaction> = ham
                .call_zome(
                    &self.cfg.role_name,
                    "transactor",
                    "get_raves_for_smart_agreement",
                    &credit_limit_ea_id,
                )
                .await?;
            let br_raves: Vec<Transaction> = ham
                .call_zome(
                    &self.cfg.role_name,
                    "transactor",
                    "get_raves_for_smart_agreement",
                    &bridging_ea_id,
                )
                .await?;
            info!(
                "[bridge] deep dedup scan engaged: retry_locks={} cl_raves={} br_raves={}",
                retry_lock_count,
                cl_raves.len(),
                br_raves.len()
            );
            (cl_raves, br_raves)
        } else {
            (Vec::new(), Vec::new())
        };

        let dedup = DedupIndex::from_scans(&cl_parked, &cl_raves, &br_parked, &br_raves);

        info!(
            "[bridge] \u{2500}\u{2500} cycle started \u{2500}\u{2500} locks_queued={} link_tag_cap={} cl_parked_live={} br_parked_live={} deep_dedup={}",
            queued_locks.len(),
            tag_cap,
            cl_parked.len(),
            br_parked.len(),
            needs_deep_dedup
        );

        // --- Per-lock decision + tag-size-aware batch extraction ---
        //
        // Each queued lock independently populates up to two proof batches:
        //   * `cl_proofs`    — to be packed into a single `create_parked_link`
        //   * `spend_proofs` — to be packed into a single `create_parked_spend`
        //
        // Either batch can be empty (e.g. after a partial-failure retry where
        // the CL link has already been consumed by its RAVE, so only the
        // spend still needs creating). `in_flight_lock_ids` is the union of
        // locks we'll mark in_flight; `fully_done_ids` is the disjoint set
        // we mark succeeded without any on-chain write.
        let mut cl_proofs: Vec<Value> = Vec::new();
        let mut cl_amounts: Vec<UnitMap> = Vec::new();
        let mut spend_proofs: Vec<Value> = Vec::new();
        let mut spend_amounts: Vec<UnitMap> = Vec::new();
        let mut in_flight_lock_ids: Vec<i64> = Vec::new();
        let mut fully_done_ids: Vec<i64> = Vec::new();
        let mut last_cl_tag_bytes: usize = 0;
        let mut last_spend_tag_bytes: usize = 0;
        let mut batch_capped = false;

        for item in &queued_locks {
            let (proof, amount) = match self.extract_lock_proof(item) {
                Ok(v) => v,
                Err(e) => {
                    // Extraction failures are permanent for this payload
                    // shape — retrying won't help. Promote to failed so the
                    // lock stops forever-lurking in the queue.
                    warn!(
                        "[bridge/deposits] proof extraction failed id={} error={}, marking failed",
                        item.item_id, e
                    );
                    if let Err(db_err) = self.db.mark_failed_permanent(
                        item.id,
                        &format!("proof extraction failed: {e}"),
                    ) {
                        error!(
                            "[bridge] failed to mark lock {} failed: {}",
                            item.id, db_err
                        );
                    }
                    continue;
                }
            };

            let tx_hash = proof
                .get("tx_hash")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let flags = dedup.lookup(&tx_hash);
            let decision = decide(&flags);

            if matches!(decision, LockDecision::FullyDone) {
                info!(
                    "[bridge] idempotent_complete lock={} tx_hash={} reason=rave_history_match attempts={}",
                    item.item_id, tx_hash, item.attempts
                );
                fully_done_ids.push(item.id);
                continue;
            }

            let needs_cl = matches!(decision, LockDecision::Fresh);
            let needs_spend = matches!(decision, LockDecision::Fresh | LockDecision::SkipClOnly);

            // Build tentative batches so we can tag-size the incremental add.
            let tentative_cl_proofs = if needs_cl {
                let mut p = cl_proofs.clone();
                p.push(proof.clone());
                p
            } else {
                cl_proofs.clone()
            };
            let tentative_cl_amounts = if needs_cl {
                let mut a = cl_amounts.clone();
                a.push(amount.clone());
                a
            } else {
                cl_amounts.clone()
            };
            let tentative_spend_proofs = if needs_spend {
                let mut p = spend_proofs.clone();
                p.push(proof.clone());
                p
            } else {
                spend_proofs.clone()
            };
            let tentative_spend_amounts = if needs_spend {
                let mut a = spend_amounts.clone();
                a.push(amount.clone());
                a
            } else {
                spend_amounts.clone()
            };

            let cl_tag_bytes = if tentative_cl_proofs.is_empty() {
                0
            } else {
                let total = accumulate_amounts(&tentative_cl_amounts)?;
                let payload = json!({ "proof_of_deposit": &tentative_cl_proofs });
                match estimate_parked_data_tag_bytes(&total, &payload) {
                    Ok(n) => n,
                    Err(e) => {
                        // Encoder shouldn't fail on well-formed data; if it
                        // does the payload is unprocessable on retry too.
                        warn!(
                            "[bridge/deposits] failed to estimate cl tag size id={} error={}, marking failed",
                            item.item_id, e
                        );
                        if let Err(db_err) = self.db.mark_failed_permanent(
                            item.id,
                            &format!("cl tag size estimation failed: {e}"),
                        ) {
                            error!(
                                "[bridge] failed to mark lock {} failed: {}",
                                item.id, db_err
                            );
                        }
                        continue;
                    }
                }
            };
            let spend_tag_bytes = if tentative_spend_proofs.is_empty() {
                0
            } else {
                let total = accumulate_amounts(&tentative_spend_amounts)?;
                let payload = json!({ "proof_of_deposit": &tentative_spend_proofs });
                match estimate_parked_spend_tag_bytes(
                    &total,
                    &payload,
                    &global_definition_hash,
                    &context.lane_definitions,
                ) {
                    Ok(n) => n,
                    Err(e) => {
                        warn!(
                            "[bridge/deposits] failed to estimate spend tag size id={} error={}, marking failed",
                            item.item_id, e
                        );
                        if let Err(db_err) = self.db.mark_failed_permanent(
                            item.id,
                            &format!("spend tag size estimation failed: {e}"),
                        ) {
                            error!(
                                "[bridge] failed to mark lock {} failed: {}",
                                item.id, db_err
                            );
                        }
                        continue;
                    }
                }
            };

            let tag_bytes = cl_tag_bytes.max(spend_tag_bytes);
            if tag_bytes > tag_cap {
                let batches_empty = cl_proofs.is_empty() && spend_proofs.is_empty();
                if batches_empty {
                    // A single proof that on its own exceeds the tag cap
                    // cannot ever be processed (Holochain enforces the
                    // limit at write time). Promote to failed instead of
                    // silently re-skipping every cycle forever.
                    warn!(
                        "[bridge/deposits] single proof exceeds link tag cap (size={} > cap={}), marking lock id={} failed",
                        tag_bytes, tag_cap, item.item_id
                    );
                    if let Err(db_err) = self.db.mark_failed_permanent(
                        item.id,
                        &format!(
                            "proof exceeds max_link_tag_bytes (size={tag_bytes}, cap={tag_cap})"
                        ),
                    ) {
                        error!(
                            "[bridge] failed to mark lock {} failed: {}",
                            item.id, db_err
                        );
                    }
                    continue;
                }
                info!(
                    "[bridge/deposits] batch cap reached cl={} spend={} next_tag={} cap={}",
                    cl_proofs.len(),
                    spend_proofs.len(),
                    tag_bytes,
                    tag_cap
                );
                batch_capped = true;
                break;
            }

            if flags.cl_parked_live || flags.cl_rave_applied {
                info!(
                    "[bridge/deposits] idempotent_skip step=create_parked_link lock={} tx_hash={} reason={} attempts={}",
                    item.item_id,
                    tx_hash,
                    if flags.cl_rave_applied { "rave_history" } else { "live_parked" },
                    item.attempts
                );
            }
            if flags.br_parked_live {
                info!(
                    "[bridge/deposits] idempotent_skip step=create_parked_spend lock={} tx_hash={} reason=live_parked attempts={}",
                    item.item_id, tx_hash, item.attempts
                );
            }

            if needs_cl {
                cl_proofs = tentative_cl_proofs;
                cl_amounts = tentative_cl_amounts;
                last_cl_tag_bytes = cl_tag_bytes;
            }
            if needs_spend {
                spend_proofs = tentative_spend_proofs;
                spend_amounts = tentative_spend_amounts;
                last_spend_tag_bytes = spend_tag_bytes;
            }
            in_flight_lock_ids.push(item.id);
        }

        if !fully_done_ids.is_empty() {
            info!(
                "[bridge/deposits] {} lock(s) detected as already-consumed by bridging RAVE in history; marking succeeded without any new on-chain write",
                fully_done_ids.len()
            );
            for lock_id in &fully_done_ids {
                self.db.mark_succeeded(*lock_id)?;
            }
        }

        if !batch_capped {
            info!(
                "[bridge/deposits] batch: in_flight={} cl_create={} spend_create={} already_done={} cl_tag={} spend_tag={} (fit within cap {})",
                in_flight_lock_ids.len(),
                cl_proofs.len(),
                spend_proofs.len(),
                fully_done_ids.len(),
                last_cl_tag_bytes,
                last_spend_tag_bytes,
                tag_cap
            );
        } else {
            info!(
                "[bridge/deposits] batch: in_flight={}/{} cl_create={} spend_create={} already_done={} cl_tag={} spend_tag={} (remainder deferred to next cycle)",
                in_flight_lock_ids.len(),
                queued_locks.len(),
                cl_proofs.len(),
                spend_proofs.len(),
                fully_done_ids.len(),
                last_cl_tag_bytes,
                last_spend_tag_bytes
            );
        }

        // --- Mark selected locks as in_flight ---
        for lock_id in &in_flight_lock_ids {
            self.db.mark_in_flight(*lock_id)?;
        }

        // --- create_parked_link (credit limit EA) ---
        if !cl_proofs.is_empty() {
            let total_cl = accumulate_amounts(&cl_amounts)?;
            let parked_data = ParkedData {
                ct_role_id: "oracle".to_string(),
                amount: Some(total_cl.clone()),
                payload: json!({ "proof_of_deposit": cl_proofs.clone() }),
            };
            let link_result: (ActionHashB64, AgentPubKey) = ham
                .call_zome(
                    &self.cfg.role_name,
                    "transactor",
                    "create_parked_link",
                    &CreateParkedLinkInput {
                        ea_id: credit_limit_ea_id.clone().into(),
                        executor: Some(self.cfg.bridging_agent_pubkey.clone().into()),
                        parked_link_type: ParkedLinkType::ParkedData((parked_data, true)),
                    },
                )
                .await?;
            info!(
                "[bridge/credit-limit] create_parked_link: {} proofs, action_hash={}",
                cl_proofs.len(),
                link_result.0
            );
        }

        // --- Credit-limit RAVE with a freshly re-fetched live-link set ---
        //
        // We re-fetch even if we just wrote a new link, so the RAVE call
        // sees exactly the set of links it will try to consume. Runs
        // whenever there's ANY live link on the CL EA — not just when we
        // created one this cycle — so orphaned links from a previous cycle
        // get swept up instead of sitting forever.
        let cl_links: Vec<Transaction> = ham
            .call_zome(
                &self.cfg.role_name,
                "transactor",
                "get_parked_links_by_ea",
                &credit_limit_ea_id,
            )
            .await?;
        if !cl_links.is_empty() {
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
                        ea_id: credit_limit_ea_id.clone(),
                        executor_inputs: Value::Null,
                        links: cl_links,
                        global_definition: global_definition.id.clone().into(),
                        lane_definitions: context.lane_definitions.clone(),
                        strategy: GetStrategy::Local,
                    },
                )
                .await?;
            info!("[bridge/credit-limit] RAVE executed");
        }

        // --- create_parked_spend (bridging EA) ---
        if !spend_proofs.is_empty() {
            let total_spend = accumulate_amounts(&spend_amounts)?;
            if !total_spend.is_zero() {
                let _: ActionHashB64 = ham
                    .call_zome(
                        &self.cfg.role_name,
                        "transactor",
                        "create_parked_spend",
                        &CreateParkedSpendInput {
                            ea_id: bridging_ea_id.clone().into(),
                            executor: Some(self.cfg.bridging_agent_pubkey.clone().into()),
                            ct_role_id: Some("bridging_agent".to_string()),
                            amount: total_spend.clone(),
                            spender_payload: json!({
                                "proof_of_deposit": spend_proofs,
                            }),
                            lane_definitions: context.lane_definitions.clone(),
                        },
                    )
                    .await?;
                info!(
                    "[bridge/bridging] create_parked_spend: total_amount={:?}",
                    total_spend
                );
            }
        }

        // --- Scan bridging EA (freshly re-fetched) + withdrawal selection ---
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
                        ea_id: bridging_ea_id.into(),
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
        } else if in_flight_lock_ids.is_empty() {
            debug!("[bridge] cycle no-op: no pending links on bridging EA");
        }

        // --- Mark processed locks as succeeded ---
        for lock_id in &in_flight_lock_ids {
            self.db.mark_succeeded(*lock_id)?;
        }
        if !in_flight_lock_ids.is_empty() {
            info!(
                "[bridge] marked {} locks as succeeded",
                in_flight_lock_ids.len()
            );
        }

        let duration_ms = started.elapsed().as_millis() as u64;
        info!(
            "[bridge] \u{2500}\u{2500} cycle completed \u{2500}\u{2500} duration={}ms locks={} already_done={} withdrawals={}",
            duration_ms,
            in_flight_lock_ids.len(),
            fully_done_ids.len(),
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

        // Normalize tx_hash to lowercase at the proof boundary so the
        // DedupIndex's string-keyed lookup is robust to any upstream caller
        // or migrated row that ever stored it mixed-case.
        let proof = json!({
            "method": "deposit",
            "contract_address": format!("0x{}", contract_hex.to_lowercase()),
            "amount": amount,
            "depositor_wallet_address": depositor,
            "lock_id": payload.lock_id,
            "tx_hash": payload.tx_hash.to_ascii_lowercase(),
        });

        info!(
            "[bridge/deposits] extracted proof id={} amount={} agent={} tx_hash={}",
            payload.lock_id, amount, payload.holochain_agent, payload.tx_hash
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

/// Establish a fresh `Ham` connection. Thin wrapper that centralizes the
/// connect call so both startup and reconnect flows share one path.
async fn connect_ham(cfg: &Config) -> Result<Ham> {
    Ham::connect(
        HamConfig::new(cfg.admin_port, cfg.app_port, cfg.app_id.clone())
            .with_request_timeout_secs(cfg.ham_request_timeout_secs),
    )
    .await
    .context("Failed to connect to Holochain")
}

/// Project orchestrator config into the shared [`BackoffConfig`].
fn backoff_config(cfg: &Config) -> BackoffConfig {
    BackoffConfig {
        initial_ms: cfg.ham_reconnect_backoff_initial_ms,
        max_ms: cfg.ham_reconnect_backoff_max_ms,
        escalate_after: cfg.ham_reconnect_escalate_after,
    }
}

/// Estimate the msgpack link-tag size a `ParkedData` write with the given
/// aggregate proofs payload would produce. Used to cap the
/// `create_parked_link` batch against Holochain's link tag size limit.
fn estimate_parked_data_tag_bytes(total_amount: &UnitMap, payload: &Value) -> Result<usize> {
    let parked_data = (
        ParkedData {
            ct_role_id: "oracle".to_string(),
            amount: Some(total_amount.clone()),
            payload: payload.clone(),
        },
        true,
    );
    let bytes = rmp_serde::to_vec(&parked_data)
        .context("failed to msgpack-encode ParkedData for tag-size estimation")?
        .len();
    Ok(bytes)
}

/// Estimate the msgpack link-tag size a `ParkedSpendData` write with the
/// given aggregate proofs payload would produce. Runtime-computed numeric
/// fields (new_balance / proposed_balance) are filled with worst-case-sized
/// placeholders derived from `total_amount`.
fn estimate_parked_spend_tag_bytes(
    total_amount: &UnitMap,
    payload: &Value,
    global_definition: &ActionHash,
    lane_definitions: &[ActionHash],
) -> Result<usize> {
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
    let bytes = rmp_serde::to_vec(&parked_spend)
        .context("failed to msgpack-encode ParkedSpendData for tag-size estimation")?
        .len();
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// Source-chain-truth dedup index
// ---------------------------------------------------------------------------
//
// For every queued lock we want to know four things, keyed by the lock's
// EVM tx_hash (which we now embed in every `proof_of_deposit` payload):
//
//   * cl_parked_live    — an unconsumed ParkedData exists on the credit-limit
//                         EA that already carries this tx_hash. A CL RAVE
//                         hasn't consumed it yet.
//   * cl_rave_applied   — a past credit-limit RAVE *has* consumed a parked
//                         link that carried this tx_hash. Proof survives
//                         even after the parked link itself is gone.
//   * br_parked_live    — an unconsumed ParkedSpend exists on the bridging
//                         EA that carries this tx_hash.
//   * br_rave_applied   — a past bridging RAVE has consumed this tx_hash.
//                         The lock is fully done; nothing to do.
//
// Having both the live-entry flag AND the applied-RAVE flag per side lets us
// safely no-op every single sub-step of a previously-interrupted cycle,
// including the nasty fail-after-CL-RAVE / fail-before-create_parked_spend
// window that blew up the 3499 lock.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DedupFlags {
    cl_parked_live: bool,
    cl_rave_applied: bool,
    br_parked_live: bool,
    br_rave_applied: bool,
}

/// Per-lock decision derived from [`DedupFlags`]. Encodes the five rows of
/// the decision table documented on [`BridgeOrchestrator::run_bridge_cycle`]
/// into four concrete outcomes (rows 2 and 3 both collapse to
/// [`LockDecision::SkipClOnly`] because they produce the same action).
#[derive(Debug, Clone, PartialEq, Eq)]
enum LockDecision {
    /// No on-chain evidence for this lock. Stage the proof for both the CL
    /// `create_parked_link` and the bridging `create_parked_spend` batches.
    Fresh,
    /// Credit-limit step already handled (either still live or already
    /// consumed by a past RAVE). Skip the CL create; stage the bridging
    /// spend.
    SkipClOnly,
    /// Bridging spend already exists on-chain (either live or a previous
    /// cycle already ran the CL RAVE over it). No new creates; this cycle's
    /// bridging RAVE will consume the existing parked spend.
    SkipBothCreates,
    /// The bridging RAVE has already consumed a parked spend carrying this
    /// lock's tx_hash. Nothing left to do; mark the lock succeeded locally
    /// without touching the chain.
    FullyDone,
}

/// Apply the source-chain-truth decision table to a set of [`DedupFlags`].
/// Pure / total / side-effect-free so the full table is trivially testable.
fn decide(flags: &DedupFlags) -> LockDecision {
    if flags.br_rave_applied {
        return LockDecision::FullyDone;
    }
    if flags.br_parked_live {
        return LockDecision::SkipBothCreates;
    }
    if flags.cl_parked_live || flags.cl_rave_applied {
        return LockDecision::SkipClOnly;
    }
    LockDecision::Fresh
}

/// Retry-gate predicate that decides whether this cycle needs to engage
/// the expensive RAVE-history scan. A never-retried lock cannot have
/// committed anything on-chain by construction, so a batch of pure-fresh
/// locks can safely skip the deep scan.
///
/// Locks with `attempts >= max_attempts` are also excluded: they're about
/// to be promoted to `failed` by `fail_exhausted_queued`, so paying the
/// unbounded `get_raves_for_smart_agreement` cost on their behalf would
/// be pointless — and without this guard a single permanently broken
/// lock would force a full history scan every cycle forever.
fn queued_locks_need_deep_dedup(locks: &[WorkItem]) -> bool {
    locks
        .iter()
        .any(|l| l.attempts > 0 && l.attempts < l.max_attempts)
}

#[derive(Debug, Default)]
struct DedupIndex {
    flags: std::collections::HashMap<String, DedupFlags>,
}

impl DedupIndex {
    fn from_scans(
        cl_parked: &[Transaction],
        cl_raves: &[Transaction],
        br_parked: &[Transaction],
        br_raves: &[Transaction],
    ) -> Self {
        let mut flags: std::collections::HashMap<String, DedupFlags> =
            std::collections::HashMap::new();
        for tx in cl_parked {
            for h in extract_parked_tx_hashes(tx) {
                flags.entry(h).or_default().cl_parked_live = true;
            }
        }
        for tx in br_parked {
            for h in extract_parked_tx_hashes(tx) {
                flags.entry(h).or_default().br_parked_live = true;
            }
        }
        for tx in cl_raves {
            for h in extract_rave_tx_hashes(tx) {
                flags.entry(h).or_default().cl_rave_applied = true;
            }
        }
        for tx in br_raves {
            for h in extract_rave_tx_hashes(tx) {
                flags.entry(h).or_default().br_rave_applied = true;
            }
        }
        Self { flags }
    }

    fn lookup(&self, tx_hash: &str) -> DedupFlags {
        self.flags.get(tx_hash).cloned().unwrap_or_default()
    }
}

/// Pull `proof_of_deposit[*].tx_hash` values out of a live
/// `Parked` / `ParkedSpend` transaction's `attached_payload`.
fn extract_parked_tx_hashes(tx: &Transaction) -> Vec<String> {
    let payload = match &tx.details {
        TransactionDetails::Parked {
            attached_payload, ..
        }
        | TransactionDetails::ParkedSpend {
            attached_payload, ..
        } => attached_payload,
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    collect_tx_hashes(payload, &mut out);
    out
}

/// Pull `proof_of_deposit[*].tx_hash` values out of a historical `RAVE`
/// transaction's `required_inputs` blob. Parked-link payloads the RAVE
/// consumed live inside `required_inputs.consumed_inputs` keyed by role,
/// wrapped in `RAVEInputStdPayloadInner { data, link_hash }`, so we need a
/// recursive walk rather than a fixed JSON path.
fn extract_rave_tx_hashes(tx: &Transaction) -> Vec<String> {
    let required_inputs = match &tx.details {
        TransactionDetails::RAVE { required_inputs, .. } => required_inputs,
        _ => return Vec::new(),
    };
    // RAVEInput's internal shape is `{ consumed_inputs, inputs }`; both
    // branches may carry proofs we care about (we only commit them into
    // `consumed_inputs`, but a defensive walk of both is free).
    let value = match serde_json::to_value(required_inputs) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    collect_tx_hashes(&value, &mut out);
    out
}

/// Recursively walk a `serde_json::Value` looking for any object that has a
/// `proof_of_deposit` array, and collect the `tx_hash` of each proof entry.
fn collect_tx_hashes(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            if let Some(Value::Array(arr)) = map.get("proof_of_deposit") {
                for proof in arr {
                    if let Some(h) = proof.get("tx_hash").and_then(|x| x.as_str()) {
                        // Lowercase on ingress so the DedupIndex key space
                        // is case-insensitive regardless of what a past
                        // writer stored in the parked payload.
                        out.push(h.to_ascii_lowercase());
                    }
                }
            }
            for (_, child) in map {
                collect_tx_hashes(child, out);
            }
        }
        Value::Array(arr) => {
            for child in arr {
                collect_tx_hashes(child, out);
            }
        }
        _ => {}
    }
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

    // ---------------------------------------------------------------------
    // Source-chain-truth idempotency tests
    // ---------------------------------------------------------------------

    use crate::state::{WorkItem, WorkState};
    use rave_engine::types::{
        RAVEInput, RAVEInputHandler, RAVEInputStdPayload, RAVEInputStdPayloadInner,
    };

    // ----- decide() decision table ---------------------------------------

    fn flags(
        cl_parked_live: bool,
        cl_rave_applied: bool,
        br_parked_live: bool,
        br_rave_applied: bool,
    ) -> DedupFlags {
        DedupFlags {
            cl_parked_live,
            cl_rave_applied,
            br_parked_live,
            br_rave_applied,
        }
    }

    #[test]
    fn decide_fresh_lock_is_scheduled_for_both_steps() {
        assert_eq!(
            decide(&flags(false, false, false, false)),
            LockDecision::Fresh
        );
    }

    #[test]
    fn decide_live_cl_parked_skips_cl_create_only() {
        assert_eq!(
            decide(&flags(true, false, false, false)),
            LockDecision::SkipClOnly
        );
    }

    #[test]
    fn decide_applied_cl_rave_skips_cl_create_only() {
        // This is the fail-after-CL-RAVE / fail-before-create_parked_spend
        // scenario that motivated extending dedup beyond live parked entries.
        // Without this row the orchestrator would re-issue a CL RAVE and
        // double-count the credit limit adjustment.
        assert_eq!(
            decide(&flags(false, true, false, false)),
            LockDecision::SkipClOnly
        );
    }

    #[test]
    fn decide_live_br_parked_skips_both_creates() {
        // Bridging parked_spend already exists on-chain: no new creates.
        // The CL side must have been done in a previous cycle to get here,
        // so we don't need a new CL link either.
        assert_eq!(
            decide(&flags(false, false, true, false)),
            LockDecision::SkipBothCreates
        );
        // Even if CL flags also happen to be set, SkipBothCreates still
        // dominates over SkipClOnly because the bridging spend is staged.
        assert_eq!(
            decide(&flags(true, true, true, false)),
            LockDecision::SkipBothCreates
        );
    }

    #[test]
    fn decide_applied_br_rave_marks_lock_fully_done() {
        // Once the bridging RAVE has consumed a parked spend carrying this
        // lock's tx_hash, the lock is terminally resolved on-chain. We
        // mark it succeeded locally with no further writes, regardless of
        // whatever ghost state exists on the other flags.
        assert_eq!(
            decide(&flags(false, false, false, true)),
            LockDecision::FullyDone
        );
        assert_eq!(
            decide(&flags(true, true, true, true)),
            LockDecision::FullyDone
        );
    }

    // ----- collect_tx_hashes JSON walker ---------------------------------

    #[test]
    fn collect_tx_hashes_empty_value_yields_nothing() {
        let mut out = Vec::new();
        collect_tx_hashes(&json!({}), &mut out);
        assert!(out.is_empty());
        collect_tx_hashes(&json!(null), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn collect_tx_hashes_finds_all_proofs_in_parked_payload() {
        // Shape of `ParkedData.payload` / `ParkedSpendData.payload` after
        // this change: `{"proof_of_deposit": [{"tx_hash": "0x.."}, ...]}`.
        let v = json!({
            "proof_of_deposit": [
                {"tx_hash": "0xaaa", "amount": "1", "lock_id": "1"},
                {"tx_hash": "0xbbb", "amount": "2", "lock_id": "2"},
            ]
        });
        let mut out = Vec::new();
        collect_tx_hashes(&v, &mut out);
        assert_eq!(out, vec!["0xaaa".to_string(), "0xbbb".to_string()]);
    }

    #[test]
    fn collect_tx_hashes_walks_rave_input_shape() {
        // This mirrors how a past RAVE transaction stores the proofs it
        // consumed: nested under `required_inputs.consumed_inputs.<role>.data`.
        let v = json!({
            "consumed_inputs": {
                "oracle": {
                    "data": {"proof_of_deposit": [{"tx_hash": "0xccc"}]},
                    "link_hash": "uhCkkSomeHashHere"
                },
                "some_other_role": {
                    "data": {"proof_of_deposit": [{"tx_hash": "0xddd"}]},
                    "link_hash": null
                }
            },
            "inputs": {}
        });
        let mut out = Vec::new();
        collect_tx_hashes(&v, &mut out);
        out.sort();
        assert_eq!(out, vec!["0xccc".to_string(), "0xddd".to_string()]);
    }

    #[test]
    fn collect_tx_hashes_normalizes_case_so_mixed_case_matches_lowercase_proof() {
        // Belt-and-suspenders: if a past writer (migration, manual re-enqueue,
        // a different EVM source) ever stored an uppercase `0xABC...` tx_hash
        // in a parked payload, dedup must still match a queued lock whose
        // proof carries the same hash lowercased. Otherwise we'd silently
        // re-issue the create_parked_* write and double-spend.
        let parked_payload = json!({
            "proof_of_deposit": [
                {"tx_hash": "0xABCDEF0123456789"}
            ]
        });
        let mut out = Vec::new();
        collect_tx_hashes(&parked_payload, &mut out);
        assert_eq!(out, vec!["0xabcdef0123456789".to_string()]);

        // And the symmetric check: a DedupIndex built from that payload
        // answers `Fresh` for the uppercase lookup but `SkipClOnly` /
        // whatever-is-set for the lowercase lookup — because downstream
        // callers must themselves lowercase the lookup key. That's the
        // invariant `extract_lock_proof` + the call site both enforce.
        let mut idx = DedupIndex::default();
        for h in &out {
            idx.flags.entry(h.clone()).or_default().cl_parked_live = true;
        }
        assert_eq!(
            decide(&idx.lookup("0xabcdef0123456789")),
            LockDecision::SkipClOnly,
            "lowercased lookup must match the lowercased-on-ingress key"
        );
    }

    #[test]
    fn collect_tx_hashes_ignores_proofs_without_tx_hash_field() {
        let v = json!({
            "proof_of_deposit": [
                {"amount": "1"},
                {"tx_hash": "0xaaa"}
            ]
        });
        let mut out = Vec::new();
        collect_tx_hashes(&v, &mut out);
        assert_eq!(out, vec!["0xaaa".to_string()]);
    }

    // ----- extract_rave_tx_hashes via synthetic RAVEInput ----------------

    fn rave_input_with_proofs(tx_hashes: &[&str]) -> RAVEInput {
        let mut consumed = RAVEInputHandler::new();
        let proofs: Vec<Value> = tx_hashes
            .iter()
            .map(|h| json!({"tx_hash": *h}))
            .collect();
        consumed.insert(
            "oracle".to_string(),
            RAVEInputStdPayload::Single(RAVEInputStdPayloadInner {
                data: Box::new(json!({"proof_of_deposit": proofs})),
                link_hash: None,
            }),
        );
        RAVEInput::new(consumed, RAVEInputHandler::new())
    }

    #[test]
    fn collect_tx_hashes_finds_proofs_inside_live_rave_input_struct() {
        // Exercises the same serialize-then-walk path that
        // `extract_rave_tx_hashes` uses on a `TransactionDetails::RAVE`.
        let input = rave_input_with_proofs(&["0xfeed", "0xbead"]);
        let v = serde_json::to_value(&input).expect("RAVEInput should serialize");
        let mut out = Vec::new();
        collect_tx_hashes(&v, &mut out);
        out.sort();
        assert_eq!(out, vec!["0xbead".to_string(), "0xfeed".to_string()]);
    }

    // ----- DedupIndex end-to-end sanity check ---------------------------
    //
    // Construction of a full `Transaction` fixture is intentionally avoided
    // here — the tricky paths (Parked `attached_payload` and RAVE
    // `required_inputs`) are already covered by the `collect_tx_hashes`
    // tests above and by `extract_*` walking the same walker. What we
    // additionally want to verify is that `DedupIndex::from_scans` keys by
    // tx_hash and tracks the four source flags independently. We do that
    // by poking the `flags` map directly after hand-simulating the per-side
    // for-loops the real `from_scans` runs.

    #[test]
    fn dedup_uses_tx_hash_not_amount() {
        // Negative control: two locks with identical amounts but distinct
        // tx_hashes must be tracked as independent entries in the index.
        let mut idx = DedupIndex::default();
        idx.flags
            .entry("0xlockA".to_string())
            .or_default()
            .cl_parked_live = true;
        idx.flags
            .entry("0xlockB".to_string())
            .or_default()
            .br_parked_live = true;

        assert_eq!(decide(&idx.lookup("0xlockA")), LockDecision::SkipClOnly);
        assert_eq!(decide(&idx.lookup("0xlockB")), LockDecision::SkipBothCreates);
        // Unknown tx_hash gets a zeroed DedupFlags => Fresh.
        assert_eq!(decide(&idx.lookup("0xlockC")), LockDecision::Fresh);
    }

    // ----- retry-gate predicate -----------------------------------------

    fn work_item(id: i64, attempts: i64) -> WorkItem {
        WorkItem {
            id,
            flow: "lock".to_string(),
            task_type: "create_parked_link".to_string(),
            item_id: format!("lock:{}", id),
            idempotency_key: format!("lock:{}:key", id),
            payload_json: json!({}),
            state: WorkState::Queued,
            attempts,
            max_attempts: 8,
            next_retry_at: None,
            last_attempt_at: None,
            error_class: None,
            last_error: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn fresh_batch_does_not_need_deep_dedup() {
        // When every queued lock has `attempts == 0` the cycle can safely
        // skip the RAVE-history scan — no retry means no chance of a
        // stranded commit.
        let locks = vec![work_item(1, 0), work_item(2, 0), work_item(3, 0)];
        assert!(!queued_locks_need_deep_dedup(&locks));
    }

    #[test]
    fn retry_batch_triggers_deep_dedup() {
        let locks = vec![work_item(1, 0), work_item(2, 3), work_item(3, 0)];
        assert!(queued_locks_need_deep_dedup(&locks));
    }

    #[test]
    fn empty_batch_does_not_need_deep_dedup() {
        assert!(!queued_locks_need_deep_dedup(&[]));
    }

    #[test]
    fn exhausted_retry_lock_does_not_trigger_deep_dedup() {
        // A single permanently-stuck lock whose attempts have reached
        // max_attempts would otherwise force a full get_raves_for_smart_agreement
        // scan every cycle. The gate must ignore it; it will be promoted to
        // 'failed' at the top of the next cycle by fail_exhausted_queued.
        let mut lock = work_item(1, 8);
        lock.max_attempts = 8;
        assert!(!queued_locks_need_deep_dedup(&[lock]));

        // Mix of one still-within-budget retry and one exhausted lock: the
        // within-budget one alone is enough to engage the deep scan.
        let retrying = work_item(2, 3);
        let mut exhausted = work_item(3, 8);
        exhausted.max_attempts = 8;
        assert!(queued_locks_need_deep_dedup(&[retrying, exhausted]));
    }
}
