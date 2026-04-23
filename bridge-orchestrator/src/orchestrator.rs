use crate::config::Config;
use crate::lock_flow::{format_amount, LockFlow};
use crate::signer::{generate_coupon, signer_context_from_env};
use crate::state::{StateStore, WorkItem, WorkStep};
use crate::watchtower_reporter::{self, ReporterState};
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
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tracing::{debug, error, info, warn};
use zfuel::fuel::ZFuel;

pub struct BridgeOrchestrator {
    cfg: Config,
    db: StateStore,
    reporter: ReporterState,
}

/// Severity bucket for a source-chain-pressure event. Mapped to a
/// concrete `tracing` level by the cycle loop: `Warn` → `warn!`,
/// `Stuck` → `error!` with a distinct `event` tag so alerting can fire.
#[derive(Debug, PartialEq, Eq)]
enum PressureSeverity {
    Warn,
    Stuck,
}

impl BridgeOrchestrator {
    pub fn new(cfg: Config) -> Result<Self> {
        let db = StateStore::open(&cfg.db_path)?;
        let reporter = ReporterState::new();
        Ok(Self { cfg, db, reporter })
    }

    /// Timestamp in milliseconds. Wrapped so we can keep every
    /// orchestrator hook that updates reporter state a single line.
    fn now_ms() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    /// Derive the "stuck" threshold the reporter should use: a bridge
    /// is considered stuck when it hasn't completed a cycle within
    /// three configured cycle intervals.
    fn stuck_threshold_ms(&self) -> u64 {
        self.cfg.bridge_cycle_interval_ms.saturating_mul(3)
    }

    /// Returns `true` when the last write-bearing zome call was slow
    /// enough that we should not stack any more source-chain pressure in
    /// this cycle. Stage-ejection is disabled when the threshold is `0`.
    fn should_eject(&self, elapsed_ms: u128) -> bool {
        let threshold = self.cfg.slow_call_threshold_ms;
        threshold > 0 && elapsed_ms > threshold
    }

    /// Emit the structured ejection warning so operators can see which
    /// stage tripped the threshold. The reconciler will drive the
    /// skipped stages forward on the next cycle.
    fn log_stage_ejected(&self, stage: &str, elapsed_ms: u128) {
        warn!(
            event = "bridge.stage_ejected",
            stage,
            elapsed_ms = elapsed_ms as u64,
            threshold_ms = self.cfg.slow_call_threshold_ms as u64,
            "[bridge/cycle] stage ejected — skipping remaining stages; reconciler will advance next cycle"
        );
        self.reporter.update(|h| {
            h.stage_ejections_total = h.stage_ejections_total.saturating_add(1);
        });
    }

    /// Compute the next source-chain-pressure cooldown given the
    /// consecutive-failure count (1-indexed: first failure is attempt=1).
    /// Doubles from `base` until hitting `cap`, then stays at the cap.
    fn pressure_cooldown_ms(base: u64, cap: u64, attempt: u32) -> u64 {
        if attempt == 0 {
            return base.min(cap);
        }
        // Use `checked_shl` to avoid overflow when attempt is very large;
        // saturate at u64::MAX (which will itself be clamped by `cap`).
        let shift = (attempt - 1).min(63);
        let scaled = base.saturating_mul(1u64 << shift);
        scaled.min(cap)
    }

    /// Decide whether a source-chain-pressure event should log at
    /// `warn!` (early attempts, still-escalating cooldown) or `error!`
    /// (we're sitting at the cap and the conductor has failed several
    /// cycles in a row — ops should see a stuck indicator). The
    /// `attempt > 3` threshold is the point where the default
    /// progression (30s → 60s → 90s → 90s …) has been at the cap for
    /// two consecutive cycles, i.e. the cap is no longer buying us any
    /// new headroom.
    fn pressure_severity(attempt: u32, cooldown_ms: u64, cap_ms: u64) -> PressureSeverity {
        let at_cap = cooldown_ms >= cap_ms;
        if at_cap && attempt > 3 {
            PressureSeverity::Stuck
        } else {
            PressureSeverity::Warn
        }
    }

    pub async fn run(&self) -> Result<()> {
        info!(
            "bridge-orchestrator started network={:?} poll={}ms bridge_cycle={}ms",
            self.cfg.network, self.cfg.poll_interval_ms, self.cfg.bridge_cycle_interval_ms
        );

        // Spawn the watchtower reporter, if configured. The handle is
        // intentionally dropped: the task runs detached, and any
        // failure inside it is logged and swallowed.
        if let Some(wt_cfg) = self.cfg.watchtower.clone() {
            // The returned JoinHandle is intentionally dropped: the
            // reporter runs detached, and any failure inside it is
            // logged and swallowed by the task itself.
            drop(watchtower_reporter::spawn(
                wt_cfg,
                self.reporter.clone(),
                self.db.clone(),
                self.stuck_threshold_ms(),
            ));
        } else {
            tracing::info!(
                event = "watchtower_reporter.disabled",
                "watchtower reporter not configured; skipping"
            );
        }

        // Spawn the retention task. Like the reporter, it runs
        // detached and swallows its own errors — the bridge cycle is
        // never affected. Disabled via `BRIDGE_RETENTION_DISABLED=true`
        // in which case the task exits immediately.
        drop(crate::retention::spawn(
            self.cfg.retention.clone(),
            self.db.clone(),
        ));

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

        // Counter of consecutive source-chain-pressure failures. Doubles
        // the cooldown on each successive error (up to the configured
        // cap) and drives log severity escalation so operators get a
        // clear alertable signal if the conductor stays stuck. Reset to
        // zero on the first fully-clean cycle.
        let mut pressure_consecutive: u32 = 0;

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
                    Ok(()) => {
                        let cycle_started_at_ms = Self::now_ms();
                        let cycle_started_instant = std::time::Instant::now();
                        self.reporter.update(|h| {
                            h.last_cycle_started_at_ms = Some(cycle_started_at_ms);
                        });
                        match self.run_bridge_cycle(&ham).await {
                        Ok(()) => {
                            last_bridge_cycle = std::time::Instant::now();
                            let duration_ms =
                                cycle_started_instant.elapsed().as_millis() as u64;
                            self.reporter.update(|h| {
                                h.last_cycle_finished_at_ms = Some(Self::now_ms());
                                h.last_cycle_duration_ms = Some(duration_ms);
                                h.consecutive_failed_cycles = 0;
                                h.pressure_active = false;
                                h.pressure_consecutive = 0;
                            });
                            // A fully-clean cycle (no error) resets the
                            // escalating-cooldown counter. Any non-zero
                            // previous state means we just recovered
                            // from pressure — log that transition so
                            // operators see the bounce-back.
                            if pressure_consecutive > 0 {
                                info!(
                                    event = "ham.source_chain_pressure_recovered",
                                    previous_attempts = pressure_consecutive,
                                    "[bridge] source-chain pressure cleared; cooldown counter reset"
                                );
                                pressure_consecutive = 0;
                            }
                        }
                        Err(e) => {
                            error!("[bridge] cycle failed: {}", e);
                            let err_str = e.to_string();
                            self.reporter.update(|h| {
                                h.last_cycle_finished_at_ms = Some(Self::now_ms());
                                h.consecutive_failed_cycles =
                                    h.consecutive_failed_cycles.saturating_add(1);
                                h.last_error = Some(err_str.clone());
                                h.last_error_at_ms = Some(Self::now_ms());
                            });
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
                                self.reporter.update(|h| {
                                    h.reconnect_failures_total =
                                        h.reconnect_failures_total.saturating_add(1);
                                });
                                match connect_with_backoff(
                                    || connect_ham(&self.cfg),
                                    &backoff,
                                    &mut shutdown,
                                )
                                .await
                                {
                                    Some(new_ham) => {
                                        ham = new_ham;
                                        self.reporter.update(|h| {
                                            h.reconnects_ok_total =
                                                h.reconnects_ok_total.saturating_add(1);
                                        });
                                    }
                                    None => return Ok(()),
                                }
                            } else if is_source_chain_pressure(&e) {
                                // Server-side workflow timeout / source-chain
                                // backpressure. Socket is healthy, so we keep
                                // it open and simply pause before the next
                                // cycle instead of retrying at full tempo.
                                // The lock itself is already queued for retry
                                // by `reset_in_flight_to_queued` above; on
                                // the next cycle the reconcile prelude will
                                // observe whether the write landed silently
                                // and advance the row's `step` accordingly.
                                pressure_consecutive =
                                    pressure_consecutive.saturating_add(1);
                                let cooldown_ms = Self::pressure_cooldown_ms(
                                    self.cfg.ham_pressure_cooldown_ms,
                                    self.cfg.ham_pressure_cooldown_max_ms,
                                    pressure_consecutive,
                                );
                                match Self::pressure_severity(
                                    pressure_consecutive,
                                    cooldown_ms,
                                    self.cfg.ham_pressure_cooldown_max_ms,
                                ) {
                                    PressureSeverity::Stuck => error!(
                                        event = "ham.source_chain_pressure_stuck",
                                        attempt = pressure_consecutive,
                                        cooldown_ms,
                                        error = %e,
                                        "[bridge] conductor source-chain pressure persists; check Holochain conductor health"
                                    ),
                                    PressureSeverity::Warn => warn!(
                                        event = "ham.source_chain_pressure",
                                        attempt = pressure_consecutive,
                                        cooldown_ms,
                                        error = %e,
                                    ),
                                }
                                let cooldown = Duration::from_millis(cooldown_ms);
                                self.reporter.update(|h| {
                                    h.pressure_active = true;
                                    h.pressure_consecutive = pressure_consecutive;
                                });
                                tokio::select! {
                                    _ = tokio::time::sleep(cooldown) => {}
                                    _ = shutdown.changed() => {}
                                }
                            }
                        }
                        }
                    },
                    Err(e) => {
                        if is_connection_error(&e) {
                            warn!(event = "ham.probe.failed", error = %e);
                            self.reporter.update(|h| {
                                h.reconnect_failures_total =
                                    h.reconnect_failures_total.saturating_add(1);
                            });
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

    /// Single unified bridge cycle built as a four-stage pipeline.
    ///
    /// Each lock row carries a `step` column (see [`WorkStep`]) that
    /// explicitly tracks which zome calls have been proven to have landed
    /// on-chain. Every stage advances rows by comparing their recorded
    /// ActionHash against a freshly-fetched live-link set — there is no
    /// reliance on walking past RAVE history. The advancement is emergent
    /// from the fact that `execute_rave` is invoked with the just-read
    /// live link set, so a silently-committed RAVE simply drops the
    /// consumed links out of the next fetch, and rows whose hashes are no
    /// longer in the live set advance automatically.
    ///
    /// 1. Resolve context + fetch live parked links on both EAs.
    /// 2. Reconcile — promote rows through the pipeline based on what chain
    ///    truth says is currently live, before any new write.
    /// 3. S1: `create_parked_link` (CL EA) packing proofs from rows at
    ///    `step='new'` up to the link-tag cap.
    /// 4. S2: `execute_rave` (CL EA) over the refetched live set; advances
    ///    rows whose `cl_link_hash` was in the consumed set.
    /// 5. S3: `create_parked_spend` (bridging EA) packing proofs from rows
    ///    at `step='cl_rave_executed'` up to the tag cap.
    /// 6. S4: `execute_rave` (bridging EA) over the refetched live set
    ///    plus withdrawal coupons; advances rows whose `br_spend_hash` was
    ///    in the consumed set to `state='succeeded'`.
    async fn run_bridge_cycle(&self, ham: &Ham) -> Result<()> {
        let started = std::time::Instant::now();

        let global_definition: GlobalDefinitionExt = ham
            .call_zome(
                &self.cfg.role_name,
                "transactor",
                "get_current_global_definition",
                &Some(GetStrategy::Local),
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

        let credit_limit_ea_id: ActionHash = context.credit_limit_adjustment.clone().into();
        let bridging_ea_id: ActionHash = context.bridging_agreement.clone().into();
        let global_definition_hash: ActionHash = global_definition.id.clone().into();

        // --- Initial live-link probe on both EAs ---
        //
        // Required for both the reconcile prelude AND as the input set for
        // stage S2 (CL RAVE, re-fetched again in between to pick up any
        // link we wrote in S1). Gives us chain truth before we issue any
        // new write.
        let cl_parked_initial: Vec<Transaction> = ham
            .call_zome(
                &self.cfg.role_name,
                "transactor",
                "get_parked_links_by_ea",
                &credit_limit_ea_id,
            )
            .await?;
        let br_parked_initial: Vec<Transaction> = ham
            .call_zome(
                &self.cfg.role_name,
                "transactor",
                "get_parked_links_by_ea",
                &bridging_ea_id,
            )
            .await?;

        // --- Reconcile: advance rows through the pipeline whenever chain
        // truth already reflects the next step. Covers every
        // silently-committed-write recovery scenario by inspecting the
        // live parked-link set against stored per-row hashes.
        //
        // Capture the returned counts so the cycle-completion line can
        // show reconcile activity alongside the stage write totals —
        // giving operators a single-line picture of the whole cycle.
        let reconcile = self.reconcile_pipeline(&cl_parked_initial, &br_parked_initial)?;

        let s1_rows = self
            .db
            .list_pending_by_step("lock", WorkStep::New, 5000)?;
        let s3_rows_initial = self
            .db
            .list_pending_by_step("lock", WorkStep::ClRaveExecuted, 5000)?;
        let br_spend_pending_initial = self
            .db
            .list_pending_by_step("lock", WorkStep::BrSpendCreated, 5000)?;
        if s1_rows.is_empty()
            && s3_rows_initial.is_empty()
            && br_spend_pending_initial.is_empty()
            && cl_parked_initial.is_empty()
            && br_parked_initial.is_empty()
        {
            let duration_ms = started.elapsed().as_millis() as u64;
            debug!(
                "[bridge] cycle no-op (no pending deposits or withdrawals) duration={}ms",
                duration_ms
            );
            return Ok(());
        }

        info!(
            "[bridge/cycle] started s1_pending={} s3_pending={} br_spend_pending={} cl_parked_live={} br_parked_live={} tag_cap={}",
            s1_rows.len(),
            s3_rows_initial.len(),
            br_spend_pending_initial.len(),
            cl_parked_initial.len(),
            br_parked_initial.len(),
            tag_cap,
        );

        // ---------------------------------------------------------------
        // S1: create_parked_link on credit-limit EA
        // ---------------------------------------------------------------
        let s1_batch = self.build_cl_batch(&s1_rows, tag_cap)?;
        let s1_attempted = !s1_batch.ids.is_empty();
        if s1_attempted {
            for id in &s1_batch.ids {
                self.db.mark_in_flight(*id)?;
            }
            let total_cl = accumulate_amounts(&s1_batch.amounts)?;
            let parked_data = ParkedData {
                ct_role_id: "oracle".to_string(),
                amount: Some(total_cl.clone()),
                payload: json!({ "proof_of_deposit": s1_batch.proofs.clone() }),
            };
            let (link_result, s1_elapsed_ms): ((ActionHashB64, AgentPubKey), u128) = timed_call(
                "s1",
                "create_parked_link",
                ham.call_zome(
                    &self.cfg.role_name,
                    "transactor",
                    "create_parked_link",
                    &CreateParkedLinkInput {
                        ea_id: credit_limit_ea_id.clone(),
                        executor: Some(self.cfg.bridging_agent_pubkey.clone().into()),
                        parked_link_type: ParkedLinkType::ParkedData((parked_data, true)),
                    },
                ),
            )
            .await?;
            let cl_link_hash = link_result.0.to_string();
            info!(
                "[bridge/s1] create_parked_link: {} proofs, action_hash={}",
                s1_batch.proofs.len(),
                link_result.0
            );
            // One ActionHash for the whole batch; every contributing row
            // stores it for future reconcile comparisons.
            for id in &s1_batch.ids {
                self.db.advance_to_cl_link_created(*id, &cl_link_hash)?;
            }
            if self.should_eject(s1_elapsed_ms) {
                self.log_stage_ejected("s1", s1_elapsed_ms);
                return Ok(());
            }
        }

        // ---------------------------------------------------------------
        // S2: execute_rave on credit-limit EA
        // ---------------------------------------------------------------
        //
        // Re-fetch the live CL link set so the RAVE sees exactly the links
        // it will consume — including any link we wrote in S1 plus
        // orphaned links from earlier cycles. Advancement for rows at
        // `step='cl_link_created'` is then driven by hash membership in
        // this consumed set: if `cl_link_hash ∈ cl_links`, the RAVE
        // consumed it.
        let cl_links: Vec<Transaction> = ham
            .call_zome(
                &self.cfg.role_name,
                "transactor",
                "get_parked_links_by_ea",
                &credit_limit_ea_id,
            )
            .await?;
        let consumed_cl_ids: HashSet<String> =
            cl_links.iter().map(|t| t.id.to_string()).collect();
        let mut cl_rave_advanced = 0usize;
        if !cl_links.is_empty() {
            info!(
                "[bridge/s2] RAVE: consuming {} explicit links",
                cl_links.len()
            );
            let (rave_result, s2_elapsed_ms): ((RAVE, ActionHash), u128) = timed_call(
                "s2",
                "execute_rave",
                ham.call_zome(
                    &self.cfg.role_name,
                    "transactor",
                    "execute_rave",
                    &RAVEExecuteInputs {
                        ea_id: credit_limit_ea_id.clone(),
                        executor_inputs: Value::Null,
                        links: cl_links.clone(),
                        global_definition: global_definition.id.clone().into(),
                        lane_definitions: context.lane_definitions.clone(),
                        strategy: GetStrategy::Local,
                    },
                ),
            )
            .await?;
            let cl_rave_hash = rave_result.1.to_string();
            info!(
                "[bridge/s2] RAVE executed action_hash={}",
                rave_result.1
            );
            let cl_created_rows = self
                .db
                .list_pending_by_step("lock", WorkStep::ClLinkCreated, 5000)?;
            for row in cl_created_rows {
                if let Some(hash) = &row.cl_link_hash {
                    if consumed_cl_ids.contains(hash) {
                        self.db
                            .advance_to_cl_rave_executed(row.id, Some(&cl_rave_hash))?;
                        cl_rave_advanced += 1;
                    }
                }
            }
            // Always log after the RAVE ran, even when 0 rows advanced.
            // A zero here is the diagnostic signal for "RAVE fired but
            // no stored cl_link_hash matched the consumed set" — i.e.
            // orphaned links or a silent write failure on a prior
            // cycle's S1.
            info!(
                "[bridge/s2] RAVE executed: {} lock(s) advanced cl_link_created → cl_rave_executed",
                cl_rave_advanced
            );
            if self.should_eject(s2_elapsed_ms) {
                self.log_stage_ejected("s2", s2_elapsed_ms);
                return Ok(());
            }
        }

        // ---------------------------------------------------------------
        // S3: create_parked_spend on bridging EA
        // ---------------------------------------------------------------
        //
        // Re-list `cl_rave_executed` rows here (not reusing the initial
        // snapshot) because S2 may have just promoted additional rows
        // into this step.
        let s3_rows = self
            .db
            .list_pending_by_step("lock", WorkStep::ClRaveExecuted, 5000)?;
        let s3_batch = self.build_spend_batch(
            &s3_rows,
            tag_cap,
            &global_definition_hash,
            &context.lane_definitions,
        )?;
        let s3_attempted = !s3_batch.ids.is_empty();
        let mut s3_written = 0usize;
        if s3_attempted {
            let total_spend = accumulate_amounts(&s3_batch.amounts)?;
            if !total_spend.is_zero() {
                for id in &s3_batch.ids {
                    self.db.mark_in_flight(*id)?;
                }
                let (spend_hash, s3_elapsed_ms): (ActionHashB64, u128) = timed_call(
                    "s3",
                    "create_parked_spend",
                    ham.call_zome(
                        &self.cfg.role_name,
                        "transactor",
                        "create_parked_spend",
                        &CreateParkedSpendInput {
                            ea_id: bridging_ea_id.clone(),
                            executor: Some(self.cfg.bridging_agent_pubkey.clone().into()),
                            ct_role_id: Some("bridging_agent".to_string()),
                            amount: total_spend.clone(),
                            spender_payload: json!({
                                "proof_of_deposit": s3_batch.proofs.clone(),
                            }),
                            lane_definitions: context.lane_definitions.clone(),
                        },
                    ),
                )
                .await?;
                let spend_hash_str = spend_hash.to_string();
                info!(
                    "[bridge/s3] create_parked_spend: total_amount={:?} action_hash={}",
                    total_spend, spend_hash
                );
                for id in &s3_batch.ids {
                    self.db
                        .advance_to_br_spend_created(*id, &spend_hash_str)?;
                }
                s3_written = s3_batch.ids.len();
                if self.should_eject(s3_elapsed_ms) {
                    self.log_stage_ejected("s3", s3_elapsed_ms);
                    return Ok(());
                }
            } else {
                debug!(
                    "[bridge/s3] batch skipped: total_spend is zero (ids={})",
                    s3_batch.ids.len()
                );
            }
        }

        // ---------------------------------------------------------------
        // S4: execute_rave on bridging EA (deposits + withdrawals)
        // ---------------------------------------------------------------
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

        let consumed_deposit_spend_ids: HashSet<String> = deposit_rave_links
            .iter()
            .map(|t| t.id.to_string())
            .collect();

        let mut rave_links: Vec<Transaction> = Vec::new();
        rave_links.extend(deposit_rave_links.iter().cloned());
        rave_links.extend(selected_withdrawal_links);

        let mut succeeded_locks = 0usize;
        if !rave_links.is_empty() {
            info!(
                "[bridge/s4] RAVE: {} deposit + {} withdrawal links",
                deposit_rave_links.len(),
                withdrawal_count
            );

            let (rave_result, _s4_elapsed_ms): ((RAVE, ActionHash), u128) = timed_call(
                "s4",
                "execute_rave",
                ham.call_zome(
                    &self.cfg.role_name,
                    "transactor",
                    "execute_rave",
                    &RAVEExecuteInputs {
                        ea_id: bridging_ea_id,
                        executor_inputs: json!({
                            "coupons": Value::Object(coupons_map)
                        }),
                        links: rave_links,
                        global_definition: global_definition.id.clone().into(),
                        lane_definitions: context.lane_definitions,
                        strategy: GetStrategy::Local,
                    },
                ),
            )
            .await?;
            let br_rave_hash = rave_result.1.to_string();
            info!(
                "[bridge/s4] RAVE executed action_hash={}",
                rave_result.1,
            );
            let br_spend_rows = self
                .db
                .list_pending_by_step("lock", WorkStep::BrSpendCreated, 5000)?;
            for row in br_spend_rows {
                if let Some(hash) = &row.br_spend_hash {
                    if consumed_deposit_spend_ids.contains(hash) {
                        self.db
                            .advance_to_br_rave_executed(row.id, Some(&br_rave_hash))?;
                        succeeded_locks += 1;
                    }
                }
            }
            // Same rationale as S2: always log after the bridging
            // RAVE ran. A zero here means the RAVE consumed nothing
            // stored by us, which is an orphaned-spend warning sign.
            info!(
                "[bridge/s4] RAVE executed: {} lock(s) advanced br_spend_created → br_rave_executed (succeeded)",
                succeeded_locks
            );
        } else if !s1_attempted && !s3_attempted {
            debug!("[bridge] cycle no-op: no pending links on bridging EA");
        }

        let duration_ms = started.elapsed().as_millis() as u64;
        info!(
            "[bridge/cycle] completed duration={}ms reconcile=(s1={} s2={} s3={} s4={}) s1_written={} s2_advanced={} s3_written={} s4_succeeded={} withdrawals={} capped_cl={} capped_spend={}",
            duration_ms,
            reconcile.s1_advanced,
            reconcile.s2_advanced,
            reconcile.s3_advanced,
            reconcile.s4_advanced,
            s1_batch.ids.len(),
            cl_rave_advanced,
            s3_written,
            succeeded_locks,
            withdrawal_count,
            s1_batch.capped,
            s3_batch.capped,
        );

        Ok(())
    }

    /// Reconcile each lock against live chain truth before running the
    /// pipeline's write stages. Every advancement here is driven by
    /// observing the expected side-effect on `get_parked_links_by_ea` — we
    /// never walk past RAVE history.
    ///
    /// Rules, applied in step-order (a single row that already has chain
    /// evidence at multiple steps gets cascaded forward each time we
    /// re-query its step after advancing):
    ///
    /// * `step='new'` and the lock's `tx_hash` is present in a live CL
    ///   parked-link payload → advance to `cl_link_created` with that
    ///   link's ActionHash.
    /// * `step='cl_link_created'` and `cl_link_hash` is NOT in the live CL
    ///   link set → the CL RAVE consumed the link. Advance to
    ///   `cl_rave_executed` with `cl_rave_hash=NULL` (we can't recover
    ///   the actual RAVE hash after the fact).
    /// * `step='cl_rave_executed'` and the lock's `tx_hash` is present in
    ///   a live bridging parked-spend payload → advance to
    ///   `br_spend_created` with that spend's ActionHash.
    /// * `step='br_spend_created'` and `br_spend_hash` is NOT in the live
    ///   bridging link set → the bridging RAVE consumed the spend. Advance
    ///   to `br_rave_executed` (simultaneously `state='succeeded'`).
    fn reconcile_pipeline(
        &self,
        cl_parked: &[Transaction],
        br_parked: &[Transaction],
    ) -> Result<ReconcileCounts> {
        let cl_by_tx_hash = build_tx_hash_to_link_id(cl_parked);
        let br_by_tx_hash = build_tx_hash_to_link_id(br_parked);
        let cl_live_ids: HashSet<String> =
            cl_parked.iter().map(|t| t.id.to_string()).collect();
        let br_live_ids: HashSet<String> =
            br_parked.iter().map(|t| t.id.to_string()).collect();
        let mut counts = ReconcileCounts::default();

        for row in self.db.list_pending_by_step("lock", WorkStep::New, 5000)? {
            let Some(tx_hash) = self.lock_tx_hash(&row) else {
                continue;
            };
            if let Some(link_id) = cl_by_tx_hash.get(&tx_hash) {
                debug!(
                    "[bridge/reconcile] lock={} new → cl_link_created (tx_hash matched live CL link {})",
                    row.item_id, link_id
                );
                self.db.advance_to_cl_link_created(row.id, link_id)?;
                counts.s1_advanced += 1;
            }
        }

        for row in self
            .db
            .list_pending_by_step("lock", WorkStep::ClLinkCreated, 5000)?
        {
            let Some(hash) = row.cl_link_hash.clone() else {
                continue;
            };
            if !cl_live_ids.contains(&hash) {
                debug!(
                    "[bridge/reconcile] lock={} cl_link_created → cl_rave_executed (cl_link_hash {} no longer live)",
                    row.item_id, hash
                );
                self.db.advance_to_cl_rave_executed(row.id, None)?;
                counts.s2_advanced += 1;
            }
        }

        for row in self
            .db
            .list_pending_by_step("lock", WorkStep::ClRaveExecuted, 5000)?
        {
            let Some(tx_hash) = self.lock_tx_hash(&row) else {
                continue;
            };
            if let Some(spend_id) = br_by_tx_hash.get(&tx_hash) {
                debug!(
                    "[bridge/reconcile] lock={} cl_rave_executed → br_spend_created (tx_hash matched live bridging spend {})",
                    row.item_id, spend_id
                );
                self.db.advance_to_br_spend_created(row.id, spend_id)?;
                counts.s3_advanced += 1;
            }
        }

        for row in self
            .db
            .list_pending_by_step("lock", WorkStep::BrSpendCreated, 5000)?
        {
            let Some(hash) = row.br_spend_hash.clone() else {
                continue;
            };
            if !br_live_ids.contains(&hash) {
                debug!(
                    "[bridge/reconcile] lock={} br_spend_created → br_rave_executed (br_spend_hash {} no longer live, lock succeeded)",
                    row.item_id, hash
                );
                self.db.advance_to_br_rave_executed(row.id, None)?;
                counts.s4_advanced += 1;
            }
        }

        // One structured summary line per cycle. In production the
        // per-row `info!`s above can be noisy; this single record is
        // the canonical signal for "the reconciler did work this
        // cycle" (i.e. a previous cycle crashed mid-call).
        debug!(
            event = "bridge.reconcile.summary",
            s1 = counts.s1_advanced,
            s2 = counts.s2_advanced,
            s3 = counts.s3_advanced,
            s4 = counts.s4_advanced,
            "[bridge/reconcile] cycle summary"
        );

        Ok(counts)
    }

    /// Extract a single lock row's proof `tx_hash` for reconcile lookups,
    /// tolerating extraction failures (they're handled by the batch
    /// builders, which mark the row permanently failed at the next
    /// processing pass). Routes through [`normalize_tx_hash`] so the
    /// comparison side of the reconciler matches the emission side in
    /// [`BridgeOrchestrator::extract_lock_proof`].
    ///
    /// On extraction failure we warn! with the `item_id` — otherwise a
    /// malformed-payload row would be silently skipped by the
    /// reconciler every cycle while staying stuck at `step='new'`
    /// until the next write path sees it. The write path already
    /// permanently-fails such a row, but that can take arbitrary time;
    /// the warn log gives operators an immediate signal.
    fn lock_tx_hash(&self, item: &WorkItem) -> Option<String> {
        match self.extract_lock_proof(item) {
            Ok((proof, _)) => proof
                .get("tx_hash")
                .and_then(|v| v.as_str())
                .map(normalize_tx_hash),
            Err(e) => {
                warn!(
                    "[bridge/reconcile] lock={} unable to extract tx_hash for reconcile: {}",
                    item.item_id, e
                );
                None
            }
        }
    }

    /// Build the S1 `create_parked_link` batch from rows at `step='new'`,
    /// respecting the link-tag cap. Rows that cannot be processed (bad
    /// payload, encoder failure, single-proof oversize) are marked
    /// permanently failed and excluded.
    fn build_cl_batch(&self, rows: &[WorkItem], tag_cap: usize) -> Result<ProofBatch> {
        let mut out = ProofBatch::default();
        for item in rows {
            let (proof, amount) = match self.extract_lock_proof(item) {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        "[bridge/s1] proof extraction failed id={} error={}, marking failed",
                        item.item_id, e
                    );
                    if let Err(db_err) = self
                        .db
                        .mark_failed_permanent(item.id, &format!("proof extraction failed: {e}"))
                    {
                        error!(
                            "[bridge] failed to mark lock {} failed: {}",
                            item.id, db_err
                        );
                    }
                    continue;
                }
            };

            let mut tentative_proofs = out.proofs.clone();
            tentative_proofs.push(proof.clone());
            let mut tentative_amounts = out.amounts.clone();
            tentative_amounts.push(amount.clone());

            let total = accumulate_amounts(&tentative_amounts)?;
            let payload = json!({ "proof_of_deposit": &tentative_proofs });
            let tag_bytes = match estimate_parked_data_tag_bytes(&total, &payload) {
                Ok(n) => n,
                Err(e) => {
                    warn!(
                        "[bridge/s1] failed to estimate cl tag size id={} error={}, marking failed",
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
            };

            if tag_bytes > tag_cap {
                if out.ids.is_empty() {
                    warn!(
                        "[bridge/s1] single proof exceeds link tag cap (size={} > cap={}), marking lock id={} failed",
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
                    "[bridge/s1] batch cap reached size={} next_tag={} cap={}",
                    out.ids.len(),
                    tag_bytes,
                    tag_cap
                );
                out.capped = true;
                break;
            }

            out.ids.push(item.id);
            out.proofs = tentative_proofs;
            out.amounts = tentative_amounts;
            out.tag_bytes = tag_bytes;
        }
        Ok(out)
    }

    /// Build the S3 `create_parked_spend` batch from rows at
    /// `step='cl_rave_executed'`, respecting the link-tag cap. Same
    /// failure handling as [`build_cl_batch`].
    fn build_spend_batch(
        &self,
        rows: &[WorkItem],
        tag_cap: usize,
        global_definition_hash: &ActionHash,
        lane_definitions: &[ActionHash],
    ) -> Result<ProofBatch> {
        let mut out = ProofBatch::default();
        for item in rows {
            let (proof, amount) = match self.extract_lock_proof(item) {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        "[bridge/s3] proof extraction failed id={} error={}, marking failed",
                        item.item_id, e
                    );
                    if let Err(db_err) = self
                        .db
                        .mark_failed_permanent(item.id, &format!("proof extraction failed: {e}"))
                    {
                        error!(
                            "[bridge] failed to mark lock {} failed: {}",
                            item.id, db_err
                        );
                    }
                    continue;
                }
            };

            let mut tentative_proofs = out.proofs.clone();
            tentative_proofs.push(proof.clone());
            let mut tentative_amounts = out.amounts.clone();
            tentative_amounts.push(amount.clone());

            let total = accumulate_amounts(&tentative_amounts)?;
            let payload = json!({ "proof_of_deposit": &tentative_proofs });
            let tag_bytes = match estimate_parked_spend_tag_bytes(
                &total,
                &payload,
                global_definition_hash,
                lane_definitions,
            ) {
                Ok(n) => n,
                Err(e) => {
                    warn!(
                        "[bridge/s3] failed to estimate spend tag size id={} error={}, marking failed",
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
            };

            if tag_bytes > tag_cap {
                if out.ids.is_empty() {
                    warn!(
                        "[bridge/s3] single proof exceeds link tag cap (size={} > cap={}), marking lock id={} failed",
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
                    "[bridge/s3] batch cap reached size={} next_tag={} cap={}",
                    out.ids.len(),
                    tag_bytes,
                    tag_cap
                );
                out.capped = true;
                break;
            }

            out.ids.push(item.id);
            out.proofs = tentative_proofs;
            out.amounts = tentative_amounts;
            out.tag_bytes = tag_bytes;
        }
        Ok(out)
    }

    fn extract_lock_proof(&self, item: &WorkItem) -> Result<(Value, UnitMap)> {
        let payload = LockPayload::deserialize(item.payload_json.clone())?;
        let contract_hex = format!("{:x}", self.cfg.lock_vault_address);
        let depositor = decode_holochain_agent_as_pubkey_string(&payload.holochain_agent)?;
        let normalized = payload.normalized_amounts()?;
        let amount = normalized.amount_hot.clone();

        // Normalize tx_hash to lowercase at the proof boundary so the
        // reconciler's string-keyed lookup is robust to any upstream caller
        // or migrated row that ever stored it mixed-case.
        let proof = json!({
            "method": "deposit",
            "contract_address": format!("0x{}", contract_hex.to_lowercase()),
            "amount": amount,
            "depositor_wallet_address": depositor,
            "lock_id": payload.lock_id,
            "tx_hash": normalize_tx_hash(&payload.tx_hash),
        });

        debug!(
            "[bridge/s1] extracted proof id={} amount={} agent={} tx_hash={}",
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
                    &Some(GetStrategy::Local),
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

/// Size-capped proof batch produced by S1/S3 batch builders. `capped=true`
/// means at least one more eligible row was deferred to a future cycle
/// because adding it would have exceeded the link-tag cap.
#[derive(Default)]
struct ProofBatch {
    ids: Vec<i64>,
    proofs: Vec<Value>,
    amounts: Vec<UnitMap>,
    tag_bytes: usize,
    capped: bool,
}

/// Per-cycle summary of reconciler advancements. One counter per
/// pipeline step transition (S1 → S4), incremented once per row
/// advanced by the reconciler in this cycle.
///
/// Returned from [`BridgeOrchestrator::reconcile_pipeline`] so tests
/// can assert advancement exactly, and emitted as a single structured
/// `info!` line so operators can see recovery activity at a glance.
/// A non-zero count is the canonical signal that a prior cycle
/// crashed mid-call.
#[derive(Debug, Default, PartialEq, Eq)]
struct ReconcileCounts {
    s1_advanced: usize,
    s2_advanced: usize,
    s3_advanced: usize,
    s4_advanced: usize,
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

/// Normalise a tx_hash to the canonical form used for all reconciler
/// lookups: trimmed of surrounding whitespace and lowercased. Every
/// call site that reads or writes a `tx_hash` for cross-boundary
/// comparison (proof emission, reconciler probe, live-parked index)
/// MUST route through this helper so the invariant is enforced
/// symmetrically on both sides of the equality check.
fn normalize_tx_hash(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

/// Build a `tx_hash -> parked-link ActionHash` index from a live
/// `get_parked_links_by_ea` result. Used by the reconciler to advance a
/// `step='new'` / `step='cl_rave_executed'` row the moment it sees its
/// proof's `tx_hash` inside a live parked payload, without having to
/// inspect RAVE history.
///
/// Keys are normalised via [`normalize_tx_hash`] so mixed-case or
/// whitespace-padded writers on either side of the comparison still
/// match.
fn build_tx_hash_to_link_id(parked: &[Transaction]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for tx in parked {
        let payload = match &tx.details {
            TransactionDetails::Parked {
                attached_payload, ..
            }
            | TransactionDetails::ParkedSpend {
                attached_payload, ..
            } => attached_payload,
            _ => continue,
        };
        let Some(Value::Array(proofs)) = payload.get("proof_of_deposit") else {
            continue;
        };
        for proof in proofs {
            if let Some(h) = proof.get("tx_hash").and_then(|v| v.as_str()) {
                out.insert(normalize_tx_hash(h), tx.id.to_string());
            }
        }
    }
    out
}

/// Wrap a zome-call future with elapsed-ms measurement and structured
/// logging. `stage` identifies the pipeline stage ("s1".."s4") and
/// `fn_name` the zome function. Returns the call result paired with the
/// measured elapsed time in milliseconds so callers can drive
/// stage-ejection policy. On error the elapsed is still logged (at
/// `warn!`) before the error is propagated to the caller.
async fn timed_call<F, T>(stage: &str, fn_name: &str, fut: F) -> Result<(T, u128)>
where
    F: std::future::Future<Output = Result<T>>,
{
    let start = std::time::Instant::now();
    let result = fut.await;
    let elapsed_ms = start.elapsed().as_millis();
    match &result {
        Ok(_) => info!(
            event = "bridge.zome_call",
            stage,
            fn_name,
            elapsed_ms = elapsed_ms as u64,
            "zome call completed"
        ),
        Err(e) => warn!(
            event = "bridge.zome_call_failed",
            stage,
            fn_name,
            elapsed_ms = elapsed_ms as u64,
            error = %e,
            "zome call failed"
        ),
    }
    result.map(|v| (v, elapsed_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // Reconciler tests
    //
    // These exercise `BridgeOrchestrator::reconcile_pipeline` end-to-end
    // against a real SQLite StateStore and synthetic `Transaction`
    // fixtures standing in for `get_parked_links_by_ea` results. The
    // reconciler is the recovery gate for every crashed/half-retried
    // cycle, so each step transition gets its own positive and (where
    // the transition is step-gated) negative test.
    // -----------------------------------------------------------------

    use crate::config::{Network, RetentionConfig};
    use alloy::primitives::Address;
    use holo_hash::{ActionHash, AgentPubKey, AgentPubKeyB64};
    use holochain_zome_types::timestamp::Timestamp;
    use rave_engine::types::TransactionType;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_db_path(name: &str) -> String {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("/tmp/bridge-orchestrator-orch-{}-{}.db", name, ts)
    }

    fn test_config(db_path: String) -> Config {
        let agent_pubkey: AgentPubKeyB64 =
            AgentPubKey::from_raw_32(vec![1u8; 32]).into();
        Config {
            network: Network::Sepolia,
            rpc_url: "http://localhost:0".to_string(),
            lock_vault_address: Address::ZERO,
            confirmations: 5,
            poll_interval_ms: 1000,
            bridge_cycle_interval_ms: 1000,
            max_link_tag_bytes: 800,
            coupons_target_bytes: 512 * 1024,
            db_path,
            role_name: "alliance".to_string(),
            app_id: "bridging-app".to_string(),
            admin_port: 0,
            app_port: 0,
            bridging_agent_pubkey: agent_pubkey,
            lane_definition: None,
            unit_index: 1,
            ham_request_timeout_secs: 120,
            ham_reconnect_backoff_initial_ms: 1000,
            ham_reconnect_backoff_max_ms: 30000,
            ham_reconnect_escalate_after: 5,
            ham_pressure_cooldown_ms: 30000,
            ham_pressure_cooldown_max_ms: 90000,
            slow_call_threshold_ms: 35000,
            watchtower: None,
            retention: RetentionConfig {
                enabled: false,
                tick_interval_ms: 3_600_000,
                succeeded_max_age_s: 7 * 24 * 60 * 60,
                failed_max_age_s: 30 * 24 * 60 * 60,
            },
        }
    }

    fn test_orchestrator(name: &str) -> BridgeOrchestrator {
        let path = test_db_path(name);
        let db = StateStore::open(&path).unwrap();
        BridgeOrchestrator {
            cfg: test_config(path),
            db,
            reporter: ReporterState::new(),
        }
    }

    fn action_hash(seed: u8) -> ActionHash {
        ActionHash::from_raw_32(vec![seed; 32])
    }

    /// Build a synthetic `TransactionDetails::Parked` fixture with the
    /// given `tx_hash` embedded in the attached proof payload, keyed
    /// under a parked-link ActionHash derived from `seed`.
    fn parked_tx(seed: u8, tx_hash: &str) -> Transaction {
        let id = action_hash(seed).into();
        let executor: AgentPubKeyB64 = AgentPubKey::from_raw_32(vec![2u8; 32]).into();
        let ea_id = action_hash(0xEA).into();
        Transaction {
            id,
            tx_type: TransactionType::Parked,
            amount: UnitMap::new(),
            counterparty: vec![],
            history: vec![],
            timestamp: Timestamp(0),
            creator: executor.clone(),
            details: TransactionDetails::Parked {
                ea_id,
                smart_agreement_title: "test".to_string(),
                executor,
                ct_role_id: "role".to_string(),
                role_display_name: "Role".to_string(),
                attached_payload: json!({
                    "proof_of_deposit": [{ "tx_hash": tx_hash }]
                }),
                consumed_link: false,
            },
        }
    }

    /// Build a synthetic `TransactionDetails::ParkedSpend` fixture the
    /// same way. The reconciler uses the same proof-walker for both,
    /// so S3 recovery is symmetric with S1 recovery.
    fn parked_spend_tx(seed: u8, tx_hash: &str) -> Transaction {
        let id = action_hash(seed).into();
        let spender: AgentPubKeyB64 = AgentPubKey::from_raw_32(vec![3u8; 32]).into();
        let executor: AgentPubKeyB64 = AgentPubKey::from_raw_32(vec![4u8; 32]).into();
        let ea_id = action_hash(0xEB).into();
        Transaction {
            id,
            tx_type: TransactionType::ParkedSpend,
            amount: UnitMap::new(),
            counterparty: vec![],
            history: vec![],
            timestamp: Timestamp(0),
            creator: spender.clone(),
            details: TransactionDetails::ParkedSpend {
                is_parked_spend_credit: false,
                ea_id,
                smart_agreement_title: "test".to_string(),
                spender,
                executor,
                ct_role_id: "role".to_string(),
                role_display_name: "Role".to_string(),
                global_definition: action_hash(0xAA).into(),
                lane_definitions: vec![],
                new_balance: UnitMap::new(),
                fees_owed: ZFuel::zero(),
                proposed_balance: UnitMap::new(),
                attached_payload: json!({
                    "proof_of_deposit": [{ "tx_hash": tx_hash }]
                }),
            },
        }
    }

    /// Enqueue a minimal lock row with a well-formed payload that
    /// `extract_lock_proof` can parse. The `tx_hash` field is what the
    /// reconciler matches against the live-parked map.
    fn enqueue_lock(orch: &BridgeOrchestrator, item_id: &str, tx_hash: &str) -> i64 {
        // A valid 32-byte hex agent (64 hex chars) so
        // `decode_holochain_agent_as_pubkey_string` succeeds.
        let agent_hex = "00".repeat(32);
        let payload = serde_json::json!({
            "lock_id": item_id,
            "sender": "0x0000000000000000000000000000000000000000",
            "amount_hot": "1.0",
            "holochain_agent": agent_hex,
            "tx_hash": tx_hash,
            "block_number": 1,
            "timestamp": 0,
            "required_confirmations": 1,
        });
        orch.db
            .enqueue_queued(
                "lock",
                "create_parked_link",
                item_id,
                &format!("{}:key", item_id),
                &payload,
            )
            .unwrap();
        orch.db
            .list_work_items("lock", crate::state::WorkState::Queued, 1000)
            .unwrap()
            .into_iter()
            .find(|r| r.item_id == item_id)
            .map(|r| r.id)
            .expect("row just enqueued must be listable")
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

    #[test]
    fn normalize_tx_hash_trims_and_lowercases_idempotently() {
        // Single chokepoint for the lowercase+trim invariant. Every
        // reconciler comparison depends on both sides of the equality
        // going through this — a regression here (e.g. dropping trim)
        // silently breaks tx_hash matching for any upstream writer
        // that pads whitespace.
        assert_eq!(normalize_tx_hash("  0xABC\n"), "0xabc");
        assert_eq!(normalize_tx_hash("0xabc"), "0xabc");
        assert_eq!(normalize_tx_hash("\t0xDeAdBeEf "), "0xdeadbeef");
        let once = normalize_tx_hash("  0xFeedFace  ");
        let twice = normalize_tx_hash(&once);
        assert_eq!(once, twice, "normalize_tx_hash must be idempotent");
    }

    #[test]
    fn build_tx_hash_to_link_id_lowercases_mixed_case_hashes() {
        // Guards the invariant that a past writer storing an uppercase
        // `0xABC...` tx_hash in a parked payload still matches a queued
        // lock whose proof carries the same hash lowercased. This is
        // the only tx_hash-based lookup that survives the chain-history
        // walker removal — make sure it stays robust against upstream
        // case inconsistency.
        let tx = parked_tx(0x11, "0xABCDEF0123456789");
        let expected_id = tx.id.to_string();
        let map = build_tx_hash_to_link_id(&[tx]);
        assert_eq!(
            map.get("0xabcdef0123456789").map(String::as_str),
            Some(expected_id.as_str()),
            "uppercase tx_hash in the live parked payload must be normalised to lowercase"
        );
    }

    #[test]
    fn build_tx_hash_to_link_id_indexes_parked_spend_payloads_too() {
        // S3 recovery uses the same index against bridging-EA links,
        // which show up as `TransactionDetails::ParkedSpend`. Confirm
        // the walker covers both variants.
        let tx_spend = parked_spend_tx(0x22, "0xdeadbeef");
        let expected = tx_spend.id.to_string();
        let map = build_tx_hash_to_link_id(&[tx_spend]);
        assert_eq!(map.get("0xdeadbeef").map(String::as_str), Some(expected.as_str()));
    }

    #[test]
    fn build_tx_hash_to_link_id_skips_non_parked_and_missing_hashes() {
        // A RAVE or any payload without a `proof_of_deposit` array must
        // not pollute the reconciler's index — otherwise a row could be
        // advanced against a live link that has nothing to do with it.
        let mut rave_tx = parked_tx(0x33, "0xshouldbeignored");
        rave_tx.tx_type = TransactionType::RAVE;
        rave_tx.details = TransactionDetails::Parked {
            ea_id: action_hash(0xEA).into(),
            smart_agreement_title: "test".to_string(),
            executor: AgentPubKey::from_raw_32(vec![2u8; 32]).into(),
            ct_role_id: "role".to_string(),
            role_display_name: "Role".to_string(),
            attached_payload: json!({ "something_else": [] }),
            consumed_link: false,
        };
        let map = build_tx_hash_to_link_id(&[rave_tx]);
        assert!(map.is_empty(), "payloads missing proof_of_deposit must not be indexed");
    }

    #[test]
    fn reconcile_advances_new_row_when_tx_hash_matches_live_cl_link() {
        // S1 recovery: a row at step='new' whose tx_hash appears in a
        // live CL parked link means `create_parked_link` silently
        // succeeded on a previous cycle. Advance to cl_link_created
        // with the observed link hash so S2 picks it up.
        let orch = test_orchestrator("reconcile-s1-advance");
        let row_id = enqueue_lock(&orch, "lock:r:1", "0xabc123");
        let live_link = parked_tx(0x10, "0xabc123");
        let expected_hash = live_link.id.to_string();

        orch.reconcile_pipeline(&[live_link], &[]).unwrap();

        let row = orch
            .db
            .list_pending_by_step("lock", WorkStep::ClLinkCreated, 10)
            .unwrap()
            .into_iter()
            .find(|r| r.id == row_id)
            .expect("row must have advanced to cl_link_created");
        assert_eq!(row.cl_link_hash.as_deref(), Some(expected_hash.as_str()));
    }

    #[test]
    fn reconcile_leaves_new_row_untouched_when_tx_hash_absent_from_live_cl() {
        // Negative case: if the live CL set doesn't include a matching
        // tx_hash, the row stays at step='new' and S1 will re-issue
        // the batch on this cycle. This is the branch that prevents
        // silent advancement on unrelated links.
        let orch = test_orchestrator("reconcile-s1-noop");
        let row_id = enqueue_lock(&orch, "lock:r:2", "0xabc999");
        let unrelated = parked_tx(0x20, "0xdeadbeef");

        orch.reconcile_pipeline(&[unrelated], &[]).unwrap();

        let rows = orch
            .db
            .list_pending_by_step("lock", WorkStep::New, 10)
            .unwrap();
        assert!(
            rows.iter().any(|r| r.id == row_id),
            "row must remain at step='new' when no live link matches"
        );
    }

    #[test]
    fn reconcile_advances_cl_link_created_when_hash_no_longer_live() {
        // S2 recovery: a stored `cl_link_hash` that is no longer in the
        // live CL set means the CL RAVE consumed it. Advance to
        // cl_rave_executed without needing to see the RAVE's own
        // ActionHash (that's the "emergent idempotency" property).
        let orch = test_orchestrator("reconcile-s2-advance");
        let row_id = enqueue_lock(&orch, "lock:r:3", "0xfeedface");
        let stored_hash = action_hash(0x30).to_string();
        orch.db
            .advance_to_cl_link_created(row_id, &stored_hash)
            .unwrap();

        orch.reconcile_pipeline(&[], &[]).unwrap();

        let row = orch
            .db
            .list_pending_by_step("lock", WorkStep::ClRaveExecuted, 10)
            .unwrap()
            .into_iter()
            .find(|r| r.id == row_id)
            .expect("row must have advanced to cl_rave_executed");
        assert!(
            row.cl_rave_hash.is_none(),
            "inferred advancement must leave cl_rave_hash NULL"
        );
    }

    #[test]
    fn reconcile_leaves_cl_link_created_untouched_when_hash_still_live() {
        // Positive-stability: when a CL link is still live, the row
        // must stay at cl_link_created so the cycle's S2 step has
        // something to consume. Otherwise we'd double-consume the link
        // on the next RAVE.
        let orch = test_orchestrator("reconcile-s2-noop");
        let row_id = enqueue_lock(&orch, "lock:r:4", "0xfeedface");
        let stored_hash = action_hash(0x40).to_string();
        orch.db
            .advance_to_cl_link_created(row_id, &stored_hash)
            .unwrap();

        // Build a live CL set that contains our stored hash.
        let live = parked_tx(0x40, "0xfeedface");
        assert_eq!(live.id.to_string(), stored_hash);

        orch.reconcile_pipeline(&[live], &[]).unwrap();

        let rows = orch
            .db
            .list_pending_by_step("lock", WorkStep::ClLinkCreated, 10)
            .unwrap();
        assert!(
            rows.iter().any(|r| r.id == row_id),
            "row must stay at cl_link_created while its link is still live"
        );
    }

    #[test]
    fn reconcile_advances_cl_rave_executed_when_tx_hash_matches_live_bridging_spend() {
        // S3 recovery: `create_parked_spend` silently succeeded on a
        // previous cycle; the bridging-EA live set now carries our
        // tx_hash as a parked spend. Advance to br_spend_created.
        let orch = test_orchestrator("reconcile-s3-advance");
        let row_id = enqueue_lock(&orch, "lock:r:5", "0xcafef00d");
        orch.db
            .advance_to_cl_link_created(row_id, &action_hash(0x50).to_string())
            .unwrap();
        orch.db.advance_to_cl_rave_executed(row_id, None).unwrap();

        let live_spend = parked_spend_tx(0x51, "0xcafef00d");
        let expected = live_spend.id.to_string();

        orch.reconcile_pipeline(&[], &[live_spend]).unwrap();

        let row = orch
            .db
            .list_pending_by_step("lock", WorkStep::BrSpendCreated, 10)
            .unwrap()
            .into_iter()
            .find(|r| r.id == row_id)
            .expect("row must have advanced to br_spend_created");
        assert_eq!(row.br_spend_hash.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn reconcile_advances_br_spend_created_to_succeeded_when_hash_no_longer_live() {
        // S4 recovery / terminal: a stored `br_spend_hash` that has
        // dropped out of the bridging live set means the bridging RAVE
        // consumed the spend. Advance to br_rave_executed + succeeded
        // so the cycle doesn't re-process it.
        let orch = test_orchestrator("reconcile-s4-advance");
        let row_id = enqueue_lock(&orch, "lock:r:6", "0xfacefeed");
        let spend_hash = action_hash(0x60).to_string();
        orch.db
            .advance_to_cl_link_created(row_id, &action_hash(0x61).to_string())
            .unwrap();
        orch.db.advance_to_cl_rave_executed(row_id, None).unwrap();
        orch.db
            .advance_to_br_spend_created(row_id, &spend_hash)
            .unwrap();

        orch.reconcile_pipeline(&[], &[]).unwrap();

        let succeeded = orch
            .db
            .list_work_items("lock", crate::state::WorkState::Succeeded, 10)
            .unwrap();
        let row = succeeded
            .into_iter()
            .find(|r| r.id == row_id)
            .expect("row must be succeeded");
        assert_eq!(row.step, WorkStep::BrRaveExecuted);
        assert_eq!(row.br_spend_hash.as_deref(), Some(spend_hash.as_str()));
        assert!(
            row.br_rave_hash.is_none(),
            "inferred S4 advancement must leave br_rave_hash NULL"
        );
    }

    #[test]
    fn batched_cl_advance_attributes_the_same_action_hash_to_every_row() {
        // Open risk from the per-step tracking plan: when multiple
        // rows are batched into a single `create_parked_link` zome
        // call, the returned ActionHash covers the entire batch.
        // The orchestrator must therefore attribute that same hash
        // to every row in the batch — otherwise one of the rows
        // would have NULL cl_link_hash and the reconciler's S2 probe
        // (cl_link_hash no longer live → advance) could never fire
        // for it.
        //
        // This test locks in the invariant against future refactors
        // by simulating exactly what the cycle does after a
        // successful batched call.
        let orch = test_orchestrator("batched-cl-attribution");
        let id_a = enqueue_lock(&orch, "lock:batched:a", "0xb1");
        let id_b = enqueue_lock(&orch, "lock:batched:b", "0xb2");
        let id_c = enqueue_lock(&orch, "lock:batched:c", "0xb3");

        let rows = orch
            .db
            .list_pending_by_step("lock", WorkStep::New, 100)
            .unwrap();
        assert_eq!(rows.len(), 3, "all three rows must be pending at 'new'");

        let batch = orch
            .build_cl_batch(&rows, 16 * 1024)
            .expect("batch construction must succeed for well-formed rows");
        assert_eq!(
            batch.ids.len(),
            3,
            "build_cl_batch must include all three rows in a single batch"
        );
        assert!(batch.ids.contains(&id_a));
        assert!(batch.ids.contains(&id_b));
        assert!(batch.ids.contains(&id_c));

        // Simulate the "successful batched call" side of the cycle:
        // every id in `batch.ids` is advanced with the shared audit
        // hash returned by `create_parked_link`.
        let shared_hash = "uhCkkSHARED";
        for id in &batch.ids {
            orch.db
                .advance_to_cl_link_created(*id, shared_hash)
                .unwrap();
        }

        let advanced = orch
            .db
            .list_pending_by_step("lock", WorkStep::ClLinkCreated, 100)
            .unwrap();
        let rows_with_ids: Vec<_> = advanced
            .iter()
            .filter(|r| r.id == id_a || r.id == id_b || r.id == id_c)
            .collect();
        assert_eq!(
            rows_with_ids.len(),
            3,
            "all batched rows must be at cl_link_created after advance"
        );
        for row in rows_with_ids {
            assert_eq!(
                row.cl_link_hash.as_deref(),
                Some(shared_hash),
                "every row in the batch must share the same cl_link_hash; row {} did not",
                row.item_id
            );
        }
    }

    #[test]
    fn reconcile_pipeline_returns_counts_with_one_advance_per_step_transition() {
        // Operability contract: reconcile_pipeline must return exactly
        // one increment per advance_to_* call it issues, so operators
        // can look at a single summary line per cycle and know which
        // stages were recovering rows.
        //
        // Seed one row at each of the four step transitions, then
        // construct the live CL / bridging sets so every row has
        // exactly one advancement available.
        let orch = test_orchestrator("reconcile-counts-each-step");

        // S1: step='new', tx_hash matches a live CL link.
        let id_s1 = enqueue_lock(&orch, "lock:counts:s1", "0xa1");
        let s1_live = parked_tx(0x81, "0xa1");

        // S2: step='cl_link_created' with hash that is NOT in live CL
        // set (RAVE consumed it).
        let id_s2 = enqueue_lock(&orch, "lock:counts:s2", "0xa2");
        let s2_stored = action_hash(0x82).to_string();
        orch.db
            .advance_to_cl_link_created(id_s2, &s2_stored)
            .unwrap();

        // S3: step='cl_rave_executed', tx_hash matches a live bridging
        // parked-spend.
        let id_s3 = enqueue_lock(&orch, "lock:counts:s3", "0xa3");
        orch.db
            .advance_to_cl_link_created(id_s3, &action_hash(0x83).to_string())
            .unwrap();
        orch.db.advance_to_cl_rave_executed(id_s3, None).unwrap();
        let s3_live_spend = parked_spend_tx(0x84, "0xa3");

        // S4: step='br_spend_created' with hash that is NOT in live
        // bridging set (bridging RAVE consumed it).
        let id_s4 = enqueue_lock(&orch, "lock:counts:s4", "0xa4");
        orch.db
            .advance_to_cl_link_created(id_s4, &action_hash(0x85).to_string())
            .unwrap();
        orch.db.advance_to_cl_rave_executed(id_s4, None).unwrap();
        orch.db
            .advance_to_br_spend_created(id_s4, &action_hash(0x86).to_string())
            .unwrap();

        let counts = orch
            .reconcile_pipeline(&[s1_live], &[s3_live_spend])
            .unwrap();

        assert_eq!(
            counts,
            ReconcileCounts {
                s1_advanced: 1,
                s2_advanced: 1,
                s3_advanced: 1,
                s4_advanced: 1,
            }
        );

        // Sanity: every seeded row did actually move forward, so the
        // counter increments aren't lying about the DB state.
        assert!(orch
            .db
            .list_pending_by_step("lock", WorkStep::ClLinkCreated, 10)
            .unwrap()
            .iter()
            .any(|r| r.id == id_s1));
        assert!(orch
            .db
            .list_pending_by_step("lock", WorkStep::ClRaveExecuted, 10)
            .unwrap()
            .iter()
            .any(|r| r.id == id_s2));
        assert!(orch
            .db
            .list_pending_by_step("lock", WorkStep::BrSpendCreated, 10)
            .unwrap()
            .iter()
            .any(|r| r.id == id_s3));
        assert!(orch
            .db
            .list_work_items("lock", crate::state::WorkState::Succeeded, 10)
            .unwrap()
            .iter()
            .any(|r| r.id == id_s4));
    }

    #[test]
    fn reconcile_pipeline_returns_zeroed_counts_when_no_rows_advance() {
        // Negative case: an empty DB plus empty live sets must yield
        // `ReconcileCounts::default()`. This pins down the "quiet
        // cycle" baseline so a future refactor can't silently count
        // phantom advances.
        let orch = test_orchestrator("reconcile-counts-quiet");
        let counts = orch.reconcile_pipeline(&[], &[]).unwrap();
        assert_eq!(counts, ReconcileCounts::default());
    }

    #[test]
    fn reconcile_is_idempotent_when_run_twice_against_same_live_sets() {
        // The reconciler runs as the first phase of every cycle. A
        // double-run (e.g. a cycle that retries its own prelude)
        // MUST NOT produce a different outcome than a single run.
        let orch = test_orchestrator("reconcile-idempotent");
        let a = enqueue_lock(&orch, "lock:r:a", "0xaaaa");
        let b = enqueue_lock(&orch, "lock:r:b", "0xbbbb");
        let live_a = parked_tx(0x70, "0xaaaa");

        orch.reconcile_pipeline(std::slice::from_ref(&live_a), &[])
            .unwrap();
        orch.reconcile_pipeline(std::slice::from_ref(&live_a), &[])
            .unwrap();

        let cl = orch
            .db
            .list_pending_by_step("lock", WorkStep::ClLinkCreated, 10)
            .unwrap();
        assert!(cl.iter().any(|r| r.id == a));
        let new = orch
            .db
            .list_pending_by_step("lock", WorkStep::New, 10)
            .unwrap();
        assert!(new.iter().any(|r| r.id == b));
    }

    // -----------------------------------------------------------------
    // Deadline-elapsed mitigation tests
    //
    // These pin down the three primitives used by the cycle loop's
    // source-chain-pressure handling:
    //   * `timed_call` — always reports elapsed_ms and preserves the
    //     result type through the wrapper;
    //   * `pressure_cooldown_ms` — doubles from `base` up to `cap` and
    //     never exceeds the cap (the progression ops will see with
    //     defaults 30s → 60s → 90s → 90s …);
    //   * `pressure_severity` — attempts 1..=3 stay at `Warn`, and the
    //     severity flips to `Stuck` only once the cooldown is at the
    //     cap AND we've been stuck there for more than one cycle;
    //   * `should_eject` — honours the `slow_call_threshold_ms=0`
    //     disable switch and the strict `>` comparison.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn timed_call_propagates_ok_result_with_elapsed_ms() {
        // Sanity: on a successful future the wrapper returns the inner
        // value unchanged and a non-zero elapsed measurement. We use
        // a short sleep to guarantee elapsed_ms > 0 without making the
        // test flaky.
        let (value, elapsed_ms) = timed_call("test", "noop", async {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            Ok::<_, anyhow::Error>(42u32)
        })
        .await
        .expect("timed_call should propagate Ok");
        assert_eq!(value, 42);
        assert!(
            elapsed_ms >= 5,
            "elapsed_ms should be at least the sleep duration, got {}",
            elapsed_ms
        );
    }

    #[tokio::test]
    async fn timed_call_propagates_err_without_panicking() {
        // On error the wrapper still must not panic and must return
        // the original error unchanged. We don't assert on elapsed
        // here because the error path doesn't return it.
        let result: Result<(u32, u128)> = timed_call("test", "boom", async {
            Err::<u32, _>(anyhow::anyhow!("kaboom"))
        })
        .await;
        assert!(result.is_err());
        assert!(
            format!("{}", result.unwrap_err()).contains("kaboom"),
            "error should be propagated unchanged"
        );
    }

    #[test]
    fn pressure_cooldown_ms_doubles_up_to_cap_with_defaults() {
        // Defaults used in production (HAM_PRESSURE_COOLDOWN_MS=30000,
        // HAM_PRESSURE_COOLDOWN_MAX_MS=90000). The progression pins
        // down the operator-facing behaviour: two clean doublings,
        // then saturation at the cap.
        let base = 30_000u64;
        let cap = 90_000u64;
        assert_eq!(BridgeOrchestrator::pressure_cooldown_ms(base, cap, 1), 30_000);
        assert_eq!(BridgeOrchestrator::pressure_cooldown_ms(base, cap, 2), 60_000);
        assert_eq!(BridgeOrchestrator::pressure_cooldown_ms(base, cap, 3), 90_000);
        assert_eq!(BridgeOrchestrator::pressure_cooldown_ms(base, cap, 4), 90_000);
        assert_eq!(BridgeOrchestrator::pressure_cooldown_ms(base, cap, 50), 90_000);
    }

    #[test]
    fn pressure_cooldown_ms_zero_attempt_returns_base() {
        // Attempt=0 is the "reset" state (we just entered the pressure
        // branch without a prior failure). The function should return
        // the base value, not some degenerate shift.
        assert_eq!(
            BridgeOrchestrator::pressure_cooldown_ms(30_000, 90_000, 0),
            30_000
        );
    }

    #[test]
    fn pressure_cooldown_ms_clamps_on_large_attempt() {
        // A large attempt count must not overflow the u64 shift; we
        // saturate to the cap instead of panicking.
        assert_eq!(
            BridgeOrchestrator::pressure_cooldown_ms(30_000, 90_000, 10_000),
            90_000
        );
    }

    #[test]
    fn pressure_severity_emits_warn_for_early_attempts() {
        // attempts 1..=3 should log at `warn!` — the cooldown is
        // still growing or has just hit the cap for the first time;
        // there isn't yet evidence of a chronically stuck conductor.
        let cap = 90_000u64;
        assert_eq!(
            BridgeOrchestrator::pressure_severity(1, 30_000, cap),
            PressureSeverity::Warn
        );
        assert_eq!(
            BridgeOrchestrator::pressure_severity(2, 60_000, cap),
            PressureSeverity::Warn
        );
        assert_eq!(
            BridgeOrchestrator::pressure_severity(3, 90_000, cap),
            PressureSeverity::Warn
        );
    }

    #[test]
    fn pressure_severity_escalates_to_stuck_once_cap_persists() {
        // attempt=4 is the first time we're at the cap for TWO
        // consecutive cycles — this is the chronic-stuck signal and
        // should fire at `error!` for alerting.
        let cap = 90_000u64;
        assert_eq!(
            BridgeOrchestrator::pressure_severity(4, 90_000, cap),
            PressureSeverity::Stuck
        );
        assert_eq!(
            BridgeOrchestrator::pressure_severity(10, 90_000, cap),
            PressureSeverity::Stuck
        );
    }

    #[test]
    fn pressure_severity_stays_warn_if_cap_not_reached() {
        // If an operator bumps the cap far higher than the base, we
        // might stay in the doubling regime for many attempts without
        // ever hitting the cap. Those cycles should keep logging at
        // `warn!` — the cap itself is the chronic-stuck trigger.
        let high_cap = 10_000_000u64;
        assert_eq!(
            BridgeOrchestrator::pressure_severity(7, 30_000 * 64, high_cap),
            PressureSeverity::Warn
        );
    }

    #[test]
    fn should_eject_respects_threshold_and_disable_switch() {
        // A strictly greater elapsed than the threshold ejects; equal
        // does not (we give the call that hit the threshold the
        // benefit of the doubt, since the write has already landed).
        // `slow_call_threshold_ms=0` disables ejection entirely so
        // operators can opt out without changing code.
        let mut orch = test_orchestrator("should-eject-threshold");
        orch.cfg.slow_call_threshold_ms = 15_000;
        assert!(!orch.should_eject(14_999));
        assert!(!orch.should_eject(15_000));
        assert!(orch.should_eject(15_001));
        assert!(orch.should_eject(60_000));

        orch.cfg.slow_call_threshold_ms = 0;
        assert!(!orch.should_eject(60_000));
        assert!(!orch.should_eject(u128::MAX));
    }
}
