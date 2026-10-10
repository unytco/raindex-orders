use crate::config::{Config, Ethereum, LINK_TAG_BYTES_CEILING};
use crate::lock_flow::{format_amount, LockFlow};
use crate::signer::{CouponSigner, Payout};
use crate::state::{StateStore, WorkItem, WorkStep};
use crate::watchtower_reporter::{self, CycleClass, ReporterState};
use alloy::primitives::Address;
use anyhow::{Context, Result};
use ham::{
    connect_with_backoff, install_shutdown_handler, is_connection_error, is_request_timeout,
    is_source_chain_pressure, BackoffConfig, CapGrantOptIn, Ham, HamConfig, LairCredentials,
    ShutdownRx,
};
use holo_hash::{ActionHash, ActionHashB64, AgentPubKey, AgentPubKeyB64};
use holochain_zome_types::prelude::{ActionData, GetStrategy, Record};
use rave_engine::types::{
    CreateParkedLinkInput, CreateParkedSpendInput, GlobalDefinitionExt, LaneDefinition, LaneExt,
    Ledger, ParkedData, ParkedLinkType, ParkedSpendData, RAVEExecuteInputs, Transaction,
    TransactionDetails, UnitFee, UnitMap, RAVE,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tracing::{debug, error, info, warn};
use zfuel::fuel::ZFuel;

const BRIDGING_AGENT_ROLE: &str = "bridging_agent";
const ORACLE_ROLE: &str = "oracle";

pub struct BridgeOrchestrator {
    cfg: Config,
    db: StateStore,
    reporter: ReporterState,
    ethereum: Option<EthereumSide>,
}

pub struct EthereumSide {
    pub chain: Ethereum,
    pub signer: CouponSigner,
}

/// Severity bucket for a source-chain-pressure event. Mapped to a
/// concrete `tracing` level by the cycle loop: `Warn` → `warn!`,
/// `Stuck` → `error!` with a distinct `event` tag so alerting can fire.
#[derive(Debug, PartialEq, Eq)]
enum PressureSeverity {
    Warn,
    Stuck,
}

/// How the cycle loop should react to a failed bridge cycle, decided purely
/// from the error's `ham` classification. Extracted from the loop so the
/// three-way disposition — including the terminal catch-all that keeps an
/// unclassified error off a hot retry loop — is unit-testable without a live
/// conductor.
#[derive(Debug, PartialEq, Eq)]
enum CycleFailureAction {
    /// A transport/connection failure (`is_connection_error`): drop and
    /// rebuild the socket.
    Reconnect,
    /// Server-side source-chain pressure or a client-side per-request timeout
    /// (`is_source_chain_pressure` / `is_request_timeout`): the socket is
    /// healthy, so keep it and cool down before retrying.
    Cooldown,
    /// Matches none of `ham`'s classifiers. Safety net: cool down (never
    /// reconnect — the socket may be perfectly fine, e.g. write backpressure or
    /// an oversize payload) so an unknown or newly-introduced failure mode
    /// degrades to a slow retry instead of a hot loop.
    UnclassifiedCooldown,
}

/// Classify a failed-cycle error into the action the loop takes, in the same
/// order the loop applies the checks: connection first (reconnect), then the
/// two slow-call classes (cooldown), then the terminal fallback. Keeping the
/// dispatch in one pure function is what lets the "nothing falls through to a
/// full-tempo retry" contract be tested directly.
fn classify_cycle_failure(e: &anyhow::Error) -> CycleFailureAction {
    if is_connection_error(e) {
        CycleFailureAction::Reconnect
    } else if is_source_chain_pressure(e) || is_request_timeout(e) {
        CycleFailureAction::Cooldown
    } else {
        CycleFailureAction::UnclassifiedCooldown
    }
}

/// Wait `duration_ms`, returning early if shutdown is signalled, so the loop
/// never sits out a full cooldown or poll interval after a Ctrl-C. Only wakes
/// the wait — the loop's own `shutdown.borrow()` checks are what exit it.
async fn sleep_or_shutdown(duration_ms: u64, shutdown: &mut ShutdownRx) {
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(duration_ms)) => {}
        _ = shutdown.changed() => {}
    }
}

struct BridgingSelection {
    deposits: Vec<Transaction>,
    withdrawals: Vec<Transaction>,
    coupons: serde_json::Map<String, Value>,
    coupon_bytes: usize,
    withdrawals_found: usize,
}

async fn select_bridging_links(
    signer: Option<&CouponSigner>,
    bridging_agent: &AgentPubKeyB64,
    bridging_links: &[Transaction],
    coupons_budget: usize,
    hot_unit_index: u32,
) -> Result<BridgingSelection> {
    let mut selection = BridgingSelection {
        deposits: Vec::new(),
        withdrawals: Vec::new(),
        coupons: serde_json::Map::new(),
        coupon_bytes: 0,
        withdrawals_found: 0,
    };
    let mut withdrawal_capped = false;

    for tx in bridging_links {
        let TransactionDetails::ParkedSpend {
            attached_payload, ..
        } = &tx.details
        else {
            continue;
        };
        if attached_payload.get("proof_of_deposit").is_some() && tx.creator == *bridging_agent {
            selection.deposits.push(tx.clone());
            continue;
        }
        let Some(withdraw_to) = attached_payload
            .get("withdraw_to_address")
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        selection.withdrawals_found += 1;
        let Some(signer) = signer else {
            continue;
        };
        if withdrawal_capped {
            continue;
        }

        let amount = tx
            .amount
            .get(&hot_unit_index.to_string())
            .map(|v| v.to_string())
            .unwrap_or_default();
        let terms = match Payout::new(withdraw_to, &amount) {
            Ok(terms) => terms,
            Err(e) => {
                error!(
                    event = "bridge.withdrawal_unpayable",
                    "[bridge/withdrawals] withdrawal {:?} stays parked, no coupon can pay it in HOT unit {hot_unit_index}: {e:#} (its amount: {:?})",
                    tx.id, tx.amount
                );
                continue;
            }
        };
        let coupon = signer.coupon(terms, tx.id.as_ref()).await?;
        let key = tx.id.to_string();

        let entry_bytes = serde_json::to_vec(&json!({ &key: &coupon }))
            .map(|v| v.len())
            .unwrap_or(0);

        if selection.coupon_bytes + entry_bytes > coupons_budget
            && !selection.withdrawals.is_empty()
        {
            withdrawal_capped = true;
            info!(
                "[bridge/withdrawals] batch: cap reached at {} coupons, coupon_bytes={}",
                selection.withdrawals.len(),
                selection.coupon_bytes
            );
            continue;
        }

        selection.coupon_bytes += entry_bytes;
        selection.coupons.insert(key, Value::String(coupon));
        selection.withdrawals.push(tx.clone());

        info!(
            "[bridge/withdrawals] generating coupon tx_id={:?} recipient={} amount={}",
            tx.id, withdraw_to, amount
        );
    }

    Ok(selection)
}

impl BridgeOrchestrator {
    pub fn new(cfg: Config, ethereum: Option<EthereumSide>) -> Result<Self> {
        let db = StateStore::open(&cfg.db_path)?;
        let reporter = ReporterState::new();
        Ok(Self {
            cfg,
            db,
            reporter,
            ethereum,
        })
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

    /// Returns `true` when the last measured zome call was slow enough
    /// that we should not stack any more source-chain pressure in this
    /// cycle. Stage-ejection is disabled when the threshold is `0`.
    fn should_eject(&self, elapsed_ms: u128) -> bool {
        let threshold = self.cfg.slow_call_threshold_ms;
        threshold > 0 && elapsed_ms > threshold
    }

    /// Emit the structured ejection warning so operators can see which call
    /// tripped the threshold. Whatever the ejected cycle had already written
    /// is picked up by the reconciler next cycle; the rest is simply retried.
    fn log_stage_ejected(&self, stage: &str, fn_name: &str, elapsed_ms: u128) {
        warn!(
            event = "bridge.stage_ejected",
            stage,
            fn_name,
            elapsed_ms = elapsed_ms as u64,
            threshold_ms = self.cfg.slow_call_threshold_ms as u64,
            "[bridge/cycle] stage ejected, skipping remaining stages"
        );
        self.reporter.update(|h| {
            h.stage_ejections_total = h.stage_ejections_total.saturating_add(1);
        });
    }

    /// `None` means the read was slow enough to eject the stage before the
    /// spend, the same verdict the calls around it get.
    async fn spend_tag_ledger(
        &self,
        read: impl std::future::Future<Output = Result<Ledger>>,
    ) -> Result<Option<Ledger>> {
        let (ledger, elapsed_ms) = timed_call("s3", "get_ledger", read)
            .await
            .context("failed to read the bridging agent's ledger")?;
        if self.should_eject(elapsed_ms) {
            self.log_stage_ejected("s3", "get_ledger", elapsed_ms);
            return Ok(None);
        }
        Ok(Some(ledger))
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

    /// Pair the escalating cooldown with the severity it implies for a given
    /// consecutive-failure count. Both cooldown classes (source-chain pressure
    /// and the unclassified fallback) deliberately share this one backoff
    /// curve, so operators get a single consistent progression; they differ
    /// only in which counter they feed it and which events they emit.
    fn pressure_backoff(&self, attempt: u32) -> (u64, PressureSeverity) {
        let cooldown_ms = Self::pressure_cooldown_ms(
            self.cfg.ham_pressure_cooldown_ms,
            self.cfg.ham_pressure_cooldown_max_ms,
            attempt,
        );
        let severity =
            Self::pressure_severity(attempt, cooldown_ms, self.cfg.ham_pressure_cooldown_max_ms);
        (cooldown_ms, severity)
    }

    pub async fn run(&self) -> Result<()> {
        info!(
            "bridge-orchestrator started network={} poll={}ms bridge_cycle={}ms",
            self.ethereum
                .as_ref()
                .map_or("none", |side| side.chain.network.name()),
            self.cfg.poll_interval_ms,
            self.cfg.bridge_cycle_interval_ms
        );

        // Checked before anything is spawned: a node that cannot offer lair is
        // a misconfiguration, and left to the connect closures below it would
        // be a failure the reconnect loop retries forever instead. The closures
        // still rebuild it per attempt, so a lair reset that rewrites the
        // conductor's connection_url is picked up without a restart.
        ham_config(&self.cfg)?;

        // The reporter runs detached: any failure inside it is logged and
        // swallowed by the task itself.
        if let Some(wt_cfg) = self.cfg.watchtower.clone() {
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

        let mut ham =
            match connect_with_backoff(|| connect_ham(&self.cfg), &backoff, &mut shutdown).await {
                Some(h) => h,
                None => {
                    info!("[bridge] shutdown received before initial connect, exiting");
                    return Ok(());
                }
            };
        let lock_flow = self
            .ethereum
            .as_ref()
            .map(|side| LockFlow::new(side.chain.clone(), self.db.clone()));

        let mut last_bridge_cycle =
            std::time::Instant::now() - Duration::from_millis(self.cfg.bridge_cycle_interval_ms);

        // Counter of consecutive source-chain-pressure failures. Doubles
        // the cooldown on each successive error (up to the configured
        // cap) and drives log severity escalation so operators get a
        // clear alertable signal if the conductor stays stuck. Reset to
        // zero on the first fully-clean cycle.
        let mut pressure_consecutive: u32 = 0;

        // Sibling counter for consecutive cycles ending in an error that
        // matches none of ham's classifiers (the terminal-fallback class).
        // Same escalation shape as `pressure_consecutive` — drives the doubling
        // cooldown and the `warn!` → `error!` bump so a persistent *unknown*
        // failure alerts instead of retrying quietly forever. Reset on the
        // first fully-clean cycle.
        let mut unclassified_consecutive: u32 = 0;

        loop {
            if *shutdown.borrow() {
                info!("[bridge] shutdown signal received, exiting cleanly");
                return Ok(());
            }

            if let Some(lock_flow) = &lock_flow {
                if let Err(e) = lock_flow.run_cycle().await {
                    error!("[lock-flow] cycle failed: {}", e);
                }
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
                        let conductor = Conductor {
                            ham: &ham,
                            role_name: &self.cfg.role_name,
                        };
                        match self.run_bridge_cycle(&conductor).await {
                            Ok(()) => {
                                last_bridge_cycle = std::time::Instant::now();
                                let duration_ms =
                                    cycle_started_instant.elapsed().as_millis() as u64;
                                self.reporter.update(|h| {
                                    h.last_cycle_finished_at_ms = Some(Self::now_ms());
                                    h.last_cycle_duration_ms = Some(duration_ms);
                                    h.consecutive_failed_cycles = 0;
                                    h.set_cycle_class(CycleClass::None);
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
                                if unclassified_consecutive > 0 {
                                    info!(
                                        event = "ham.unclassified_error_recovered",
                                        previous_attempts = unclassified_consecutive,
                                        "[bridge] unclassified-error streak cleared; cooldown counter reset"
                                    );
                                    unclassified_consecutive = 0;
                                }
                            }
                            Err(e) => {
                                // `{:#}` and not `{}`: an `anyhow` chain prints
                                // only its outermost context otherwise, which
                                // here is our wording, not the conductor's.
                                let err_str = format!("{e:#}");
                                error!("[bridge] cycle failed: {}", err_str);
                                self.reporter.update(|h| {
                                    h.last_cycle_finished_at_ms = Some(Self::now_ms());
                                    h.consecutive_failed_cycles =
                                        h.consecutive_failed_cycles.saturating_add(1);
                                    h.last_error = Some(err_str.clone());
                                    h.last_error_at_ms = Some(Self::now_ms());
                                });
                                if let Err(reset_err) =
                                    self.db.reset_in_flight_to_queued("lock", &err_str)
                                {
                                    error!(
                                        "[bridge] failed to reset in_flight locks: {}",
                                        reset_err
                                    );
                                }
                                match classify_cycle_failure(&e) {
                                    CycleFailureAction::Reconnect => {
                                        warn!(event = "ham.disconnected", error = %err_str);
                                        // A lost socket is neither cooldown class, so drop
                                        // whichever one an earlier cycle published: it would
                                        // otherwise outlive its condition and — since the
                                        // dashboard ranks a cooldown class above "failing
                                        // cycles" — hide a reconnect loop behind a stale
                                        // pressure or unclassified badge until the next
                                        // fully-clean cycle.
                                        self.reporter.update(|h| {
                                            h.reconnect_failures_total =
                                                h.reconnect_failures_total.saturating_add(1);
                                            h.set_cycle_class(CycleClass::None);
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
                                    }
                                    CycleFailureAction::Cooldown => {
                                        // Two distinct but similarly-shaped slow-call
                                        // failures that share one cooldown policy:
                                        //   * `is_source_chain_pressure` — server-side
                                        //     workflow timeout / source-chain
                                        //     backpressure (socket is healthy).
                                        //   * `is_request_timeout` — client-side
                                        //     per-request timeout fired while the
                                        //     zome call was still running (socket is
                                        //     also healthy).
                                        // In both cases reconnecting is counter-
                                        // productive, so we keep the socket open and
                                        // pause before the next cycle instead of
                                        // retrying at full tempo. The lock is already
                                        // queued for retry by the
                                        // `reset_in_flight_to_queued` call above; the
                                        // next cycle's reconcile prelude will
                                        // observe whether the write landed silently
                                        // and advance the row's `step` accordingly.
                                        //
                                        // The two classes share cooldown shape and
                                        // severity thresholds so operators get a
                                        // single consistent backoff curve, but they
                                        // emit distinct event keys
                                        // (`ham.source_chain_pressure*` vs
                                        // `ham.request_timeout*`) so dashboards and
                                        // alerts can tell them apart.
                                        let request_timeout = is_request_timeout(&e);
                                        pressure_consecutive =
                                            pressure_consecutive.saturating_add(1);
                                        let (cooldown_ms, severity) =
                                            self.pressure_backoff(pressure_consecutive);
                                        match (request_timeout, severity) {
                                            (true, PressureSeverity::Stuck) => error!(
                                                event = "ham.request_timeout_stuck",
                                                attempt = pressure_consecutive,
                                                cooldown_ms,
                                                error = %err_str,
                                                "[bridge] ham per-request timeout persists; root cause is upstream of the orchestrator"
                                            ),
                                            (true, PressureSeverity::Warn) => warn!(
                                                event = "ham.request_timeout",
                                                attempt = pressure_consecutive,
                                                cooldown_ms,
                                                error = %err_str,
                                            ),
                                            (false, PressureSeverity::Stuck) => error!(
                                                event = "ham.source_chain_pressure_stuck",
                                                attempt = pressure_consecutive,
                                                cooldown_ms,
                                                error = %err_str,
                                                "[bridge] conductor source-chain pressure persists; check Holochain conductor health"
                                            ),
                                            (false, PressureSeverity::Warn) => warn!(
                                                event = "ham.source_chain_pressure",
                                                attempt = pressure_consecutive,
                                                cooldown_ms,
                                                error = %err_str,
                                            ),
                                        }
                                        self.reporter.update(|h| {
                                            h.set_cycle_class(CycleClass::Pressure(
                                                pressure_consecutive,
                                            ));
                                        });
                                        sleep_or_shutdown(cooldown_ms, &mut shutdown).await;
                                    }
                                    CycleFailureAction::UnclassifiedCooldown => {
                                        // Terminal safety net (B111): an error matching none of
                                        // ham's classifiers must not fall off the chain and retry
                                        // at poll cadence with no cooldown. We deliberately do NOT
                                        // reconnect (the socket may be healthy — e.g. write
                                        // backpressure or an oversize payload); instead we reuse the
                                        // source-chain-pressure escalation on a *separate* counter,
                                        // so a persistent unknown failure backs off (doubling to the
                                        // cap) and escalates `warn!` → `error!` for alerting rather
                                        // than retrying quietly forever. `consecutive_failed_cycles`
                                        // and `last_error` were already recorded above.
                                        unclassified_consecutive =
                                            unclassified_consecutive.saturating_add(1);
                                        let (cooldown_ms, severity) =
                                            self.pressure_backoff(unclassified_consecutive);
                                        match severity {
                                            PressureSeverity::Stuck => error!(
                                                event = "ham.unclassified_error_stuck",
                                                attempt = unclassified_consecutive,
                                                cooldown_ms,
                                                error = %err_str,
                                                "[bridge] cycle keeps failing with errors matching no ham classifier; investigate — not a known transport or backpressure failure"
                                            ),
                                            PressureSeverity::Warn => warn!(
                                                event = "ham.unclassified_error",
                                                attempt = unclassified_consecutive,
                                                cooldown_ms,
                                                error = %err_str,
                                                "[bridge] cycle failed with an error matching no ham classifier; cooling down before retry"
                                            ),
                                        }
                                        // Publish the unclassified streak so watchtower can surface it
                                        // (parity with pressure). Setting the class also clears the pressure pair
                                        // an earlier pressure cycle left behind: this failure is
                                        // *not* source-chain pressure, and leaving that pair set
                                        // would have watchtower saying "check conductor health"
                                        // while the actual failure class is unknown. The *local*
                                        // escalation counters are deliberately untouched — each
                                        // counts its class's failures since the last clean cycle,
                                        // so the backoff curve survives a class switch.
                                        self.reporter.update(|h| {
                                            h.set_cycle_class(CycleClass::Unclassified(
                                                unclassified_consecutive,
                                            ));
                                        });
                                        sleep_or_shutdown(cooldown_ms, &mut shutdown).await;
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        if is_connection_error(&e) {
                            warn!(event = "ham.probe.failed", error = %e);
                            // Same reasoning as the disconnect arm above: the probe
                            // failing on a dead socket ends any cooldown class an
                            // earlier cycle published.
                            self.reporter.update(|h| {
                                h.reconnect_failures_total =
                                    h.reconnect_failures_total.saturating_add(1);
                                h.set_cycle_class(CycleClass::None);
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
                            warn!("[bridge] probe failed with non-connection error: {}", e);
                        }
                    }
                }
            }

            // Interruptible so shutdown is never observed mid-write.
            sleep_or_shutdown(self.cfg.poll_interval_ms, &mut shutdown).await;
        }
    }

    /// Single unified bridge cycle built as a four-stage pipeline.
    ///
    /// Each lock row's `step` column (see [`WorkStep`]) tracks which zome calls
    /// have been proven to land on-chain. A stage advances a row only on
    /// evidence in a freshly-fetched live-link set, never by walking past RAVE
    /// history: `execute_rave` is invoked with the just-read live set, so a
    /// silently-committed RAVE drops the consumed links out of the next fetch
    /// and the rows behind them advance on their own.
    ///
    /// 1. Resolve context.
    /// 2. Reconcile: promote rows through the pipeline based on what is
    ///    still parked on the agreement each row was parked on, before any
    ///    new write.
    /// 3. S1: `create_parked_link` (CL EA) packing proofs from rows at
    ///    `step='new'` up to the link-tag cap.
    /// 4. S2: `execute_rave` (CL EA) over the refetched live set; advances
    ///    each row at `cl_link_created` whose own proof was in a link the
    ///    RAVE took off the agreement.
    /// 5. S3: `create_parked_spend` (bridging EA) packing proofs from rows
    ///    at `step='cl_rave_executed'` up to the tag cap.
    /// 6. S4: `execute_rave` (bridging EA) over the refetched live set
    ///    plus withdrawal coupons; advances to `state='succeeded'` each row
    ///    at `br_spend_created` whose own proof was in a link the RAVE took
    ///    off the agreement.
    async fn run_bridge_cycle<C: ConductorReads + ConductorWrites>(
        &self,
        conductor: &C,
    ) -> Result<()> {
        let started = std::time::Instant::now();

        let global_definition = conductor.global_definition().await?;
        let context = Self::resolve_deposit_context(
            conductor,
            &self.cfg.bridging_agent_pubkey,
            self.cfg.hot_unit_index,
            &global_definition,
        )
        .await?;

        let tag_cap = self.cfg.max_link_tag_bytes;
        let coupons_budget = self.cfg.coupons_target_bytes;

        // Per-cycle safety valve: `recover_stale_items` only runs at startup, so
        // without this a permanently broken lock retries forever in a long
        // session.
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

        let mut live = LiveLinks::default();
        let reconcile = self
            .reconcile_pipeline(conductor, &mut live, &context)
            .await?;
        let cl_parked_live = live.on(conductor, &credit_limit_ea_id).await?.len();
        let br_parked_live = live.on(conductor, &bridging_ea_id).await?.len();

        let vault = self
            .ethereum
            .as_ref()
            .map(|side| side.chain.lock_vault_address);
        let s1_rows = self.db.list_pending_by_step("lock", WorkStep::New, 5000)?;
        let s3_rows_initial =
            self.db
                .list_pending_by_step("lock", WorkStep::ClRaveExecuted, 5000)?;
        let br_spend_pending_initial =
            self.db
                .list_pending_by_step("lock", WorkStep::BrSpendCreated, 5000)?;
        if s1_rows.is_empty()
            && s3_rows_initial.is_empty()
            && br_spend_pending_initial.is_empty()
            && cl_parked_live == 0
            && br_parked_live == 0
        {
            let duration_ms = started.elapsed().as_millis() as u64;
            debug!(
                "[bridge] cycle no-op (no pending deposits or withdrawals) duration={}ms",
                duration_ms
            );
            return Ok(());
        }

        info!(
            "[bridge/cycle] started on {} s1_pending={} s3_pending={} br_spend_pending={} cl_parked_live={} br_parked_live={} tag_cap={}",
            context.lane,
            s1_rows.len(),
            s3_rows_initial.len(),
            br_spend_pending_initial.len(),
            cl_parked_live,
            br_parked_live,
            tag_cap,
        );

        // ---------------------------------------------------------------
        // S1: create_parked_link on credit-limit EA
        // ---------------------------------------------------------------
        let s1_batch = match vault {
            Some(vault) => self.build_cl_batch(vault, &s1_rows, tag_cap)?,
            None => ProofBatch::default(),
        };
        let s1_attempted = !s1_batch.ids.is_empty();
        if s1_attempted {
            for id in &s1_batch.ids {
                self.db.mark_in_flight(*id)?;
            }
            let total_cl = UnitMap::sum_vec(s1_batch.amounts.clone())?;
            let parked_data = parked_data(
                &total_cl,
                &json!({ "proof_of_deposit": s1_batch.proofs.clone() }),
            );
            let (cl_link_hash, s1_elapsed_ms) = timed_call(
                "s1",
                "create_parked_link",
                conductor.create_parked_link(&CreateParkedLinkInput {
                    ea_id: credit_limit_ea_id.clone(),
                    executor: Some(self.cfg.bridging_agent_pubkey.clone().into()),
                    parked_link_type: ParkedLinkType::ParkedData((parked_data, true)),
                }),
            )
            .await?;
            let cl_link_hash = cl_link_hash.to_string();
            info!(
                "[bridge/s1] create_parked_link: {} proofs, action_hash={}",
                s1_batch.proofs.len(),
                cl_link_hash
            );
            // One ActionHash for the whole batch; every contributing row
            // stores it for future reconcile comparisons.
            for id in &s1_batch.ids {
                self.record_cl_link(*id, &cl_link_hash, &context)?;
            }
            if self.should_eject(s1_elapsed_ms) {
                self.log_stage_ejected("s1", "create_parked_link", s1_elapsed_ms);
                return Ok(());
            }
        }

        // ---------------------------------------------------------------
        // S2: execute_rave on credit-limit EA
        // ---------------------------------------------------------------
        //
        // Re-fetch the live CL link set so the RAVE sees exactly the links it
        // will consume, including any link S1 just wrote and orphaned links
        // from earlier cycles.
        let cl_links_fetched = conductor.parked_links(&credit_limit_ea_id).await?;
        let cl_fetched_count = cl_links_fetched.len();
        let (cl_links, deferred_cl_links) =
            apply_rave_link_cap(cl_links_fetched, self.cfg.rave_max_links);
        let mut cl_rave_advanced = 0usize;
        if !cl_links.is_empty() {
            if deferred_cl_links > 0 {
                info!(
                    event = "bridge.s2.rave_capped",
                    cap = self.cfg.rave_max_links.unwrap_or(0),
                    fetched = cl_fetched_count,
                    consuming = cl_links.len(),
                    deferred = deferred_cl_links,
                    "[bridge/s2] RAVE: capping CL link batch to {} (deferring {} to next cycle)",
                    cl_links.len(),
                    deferred_cl_links
                );
            }
            info!(
                "[bridge/s2] RAVE: consuming {} explicit links",
                cl_links.len()
            );
            let (cl_rave_hash, s2_elapsed_ms) = timed_call(
                "s2",
                "execute_rave",
                conductor.execute_rave(&RAVEExecuteInputs {
                    ea_id: credit_limit_ea_id.clone(),
                    executor_inputs: Value::Null,
                    links: cl_links.clone(),
                    global_definition: global_definition.id.clone().into(),
                    lane_definitions: context.lane_definitions.clone(),
                    strategy: GetStrategy::Local,
                }),
            )
            .await?;
            let cl_rave_hash = cl_rave_hash.to_string();
            info!("[bridge/s2] RAVE executed action_hash={}", cl_rave_hash);
            if self.should_eject(s2_elapsed_ms) {
                self.log_stage_ejected("s2", "execute_rave", s2_elapsed_ms);
                return Ok(());
            }
            let outcome = self
                .rave_outcome(
                    conductor,
                    "s2",
                    &credit_limit_ea_id,
                    &cl_rave_hash,
                    &cl_links,
                )
                .await?;
            cl_rave_advanced = self.advance_consumed(
                WorkStep::ClLinkCreated,
                &self.links_by_lock(&outcome.taken),
                |id| self.db.advance_to_cl_rave_executed(id, Some(&cl_rave_hash)),
            )?;
            info!(
                "[bridge/s2] RAVE executed: {} lock(s) advanced cl_link_created → cl_rave_executed",
                cl_rave_advanced
            );
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
        let s3_batch = match vault {
            Some(vault) if !s3_rows.is_empty() => {
                // Read on the same connection as the write, and immediately before
                // it: the tag carries this agent's ledger as it stands when the
                // zome reads it, and every zome call in between would move it.
                let Some(ledger) = self.spend_tag_ledger(conductor.ledger()).await? else {
                    return Ok(());
                };
                let tag_context = SpendTagContext {
                    ledger,
                    global_definition: global_definition_hash.clone(),
                    lane_definitions: lane_definitions_written(&context),
                    unit_fees: global_definition
                        .system_rave_agreements
                        .compute_transaction_fee
                        .unit_fees
                        .clone(),
                };
                self.build_spend_batch(vault, &s3_rows, tag_cap, &tag_context)?
            }
            _ => ProofBatch::default(),
        };
        let s3_attempted = !s3_batch.ids.is_empty();
        let mut s3_written = 0usize;
        if s3_attempted {
            let total_spend = UnitMap::sum_vec(s3_batch.amounts.clone())?;
            if !total_spend.is_zero() {
                for id in &s3_batch.ids {
                    self.db.mark_in_flight(*id)?;
                }
                let (spend_hash, s3_elapsed_ms) = timed_call(
                    "s3",
                    "create_parked_spend",
                    conductor.create_parked_spend(&CreateParkedSpendInput {
                        ea_id: bridging_ea_id.clone(),
                        executor: Some(self.cfg.bridging_agent_pubkey.clone().into()),
                        ct_role_id: Some(BRIDGING_AGENT_ROLE.to_string()),
                        amount: total_spend.clone(),
                        spender_payload: json!({
                            "proof_of_deposit": s3_batch.proofs.clone(),
                        }),
                        lane_definitions: context.lane_definitions.clone(),
                    }),
                )
                .await?;
                let spend_hash_str = spend_hash.to_string();
                info!(
                    "[bridge/s3] create_parked_spend: total_amount={:?} action_hash={}",
                    total_spend, spend_hash
                );
                for id in &s3_batch.ids {
                    self.record_br_spend(*id, &spend_hash_str, &context)?;
                }
                s3_written = s3_batch.ids.len();
                if self.should_eject(s3_elapsed_ms) {
                    self.log_stage_ejected("s3", "create_parked_spend", s3_elapsed_ms);
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
        let bridging_links = conductor.parked_links(&bridging_ea_id).await?;

        let BridgingSelection {
            deposits: deposit_rave_links,
            withdrawals: selected_withdrawal_links,
            coupons: mut coupons_map,
            coupon_bytes: coupon_cumulative_bytes,
            withdrawals_found: total_withdrawals_found,
        } = select_bridging_links(
            self.ethereum.as_ref().map(|side| &side.signer),
            &self.cfg.bridging_agent_pubkey,
            &bridging_links,
            coupons_budget,
            self.cfg.hot_unit_index,
        )
        .await?;

        // Build the pooled RAVE link Vec (deposits first, then selected
        // withdrawals) before applying the optional per-cycle cap. The
        // deposits-first ordering makes `apply_rave_link_cap` preferentially
        // defer withdrawals, which is operationally preferable: deposits
        // already have their HOT-side settled in S3, so finishing their
        // CL→BR flow unlocks user-facing progress faster than re-batching
        // withdrawals.
        let pre_cap_deposit_count = deposit_rave_links.len();
        let pre_cap_withdrawal_count = selected_withdrawal_links.len();
        let deposit_rave_ids: HashSet<String> = deposit_rave_links
            .iter()
            .map(|t| t.id.to_string())
            .collect();

        let mut rave_links: Vec<Transaction> = Vec::new();
        rave_links.extend(deposit_rave_links.iter().cloned());
        rave_links.extend(selected_withdrawal_links);

        let (rave_links, deferred_br_rave) =
            apply_rave_link_cap(rave_links, self.cfg.rave_max_links);

        let retained_deposit_ids: HashSet<String> = rave_links
            .iter()
            .filter(|t| deposit_rave_ids.contains(&t.id.to_string()))
            .map(|t| t.id.to_string())
            .collect();
        let retained_withdrawal_ids: HashSet<String> = rave_links
            .iter()
            .filter(|t| !deposit_rave_ids.contains(&t.id.to_string()))
            .map(|t| t.id.to_string())
            .collect();

        // Strip coupons whose withdrawal link was deferred by the cap so
        // `execute_rave` only receives coupons for links it's actually
        // going to process.
        coupons_map.retain(|k, _| retained_withdrawal_ids.contains(k));

        let retained_deposit_count = retained_deposit_ids.len();
        let withdrawal_count = retained_withdrawal_ids.len();
        let deferred_withdrawals = total_withdrawals_found - withdrawal_count;
        let deferred_deposits_by_rave_cap = pre_cap_deposit_count - retained_deposit_count;
        let deferred_withdrawals_by_rave_cap = pre_cap_withdrawal_count - withdrawal_count;

        if deferred_br_rave > 0 {
            info!(
                event = "bridge.s4.rave_capped",
                cap = self.cfg.rave_max_links.unwrap_or(0),
                deferred = deferred_br_rave,
                deferred_deposits = deferred_deposits_by_rave_cap,
                deferred_withdrawals_by_cap = deferred_withdrawals_by_rave_cap,
                "[bridge/s4] RAVE: capping combined link batch to {} (deferring {} to next cycle)",
                rave_links.len(),
                deferred_br_rave
            );
        }

        info!(
            "[bridge/withdrawals] scan: found={} selected={}/{} coupon_bytes={} deferred={}",
            total_withdrawals_found,
            withdrawal_count,
            total_withdrawals_found,
            coupon_cumulative_bytes,
            deferred_withdrawals
        );

        let mut succeeded_locks = 0usize;
        if !rave_links.is_empty() {
            info!(
                "[bridge/s4] RAVE: {} deposit + {} withdrawal links",
                retained_deposit_count, withdrawal_count
            );

            let (br_rave_hash, s4_elapsed_ms) = timed_call(
                "s4",
                "execute_rave",
                conductor.execute_rave(&RAVEExecuteInputs {
                    ea_id: bridging_ea_id.clone(),
                    executor_inputs: json!({
                        "coupons": Value::Object(coupons_map)
                    }),
                    links: rave_links.clone(),
                    global_definition: global_definition.id.clone().into(),
                    lane_definitions: context.lane_definitions,
                    strategy: GetStrategy::Local,
                }),
            )
            .await?;
            let br_rave_hash = br_rave_hash.to_string();
            info!("[bridge/s4] RAVE executed action_hash={}", br_rave_hash);
            if self.should_eject(s4_elapsed_ms) {
                self.log_stage_ejected("s4", "execute_rave", s4_elapsed_ms);
                return Ok(());
            }
            let outcome = self
                .rave_outcome(conductor, "s4", &bridging_ea_id, &br_rave_hash, &rave_links)
                .await?;
            succeeded_locks = self.advance_consumed(
                WorkStep::BrSpendCreated,
                &self.links_by_lock(&outcome.taken),
                |id| self.db.advance_to_br_rave_executed(id, Some(&br_rave_hash)),
            )?;
            info!(
                "[bridge/s4] RAVE executed: {} lock(s) advanced br_spend_created → br_rave_executed (succeeded)",
                succeeded_locks
            );
        } else if !s1_attempted && !s3_attempted {
            debug!("[bridge] cycle no-op: no pending links on bridging EA");
        }

        let duration_ms = started.elapsed().as_millis() as u64;
        info!(
            "[bridge/cycle] completed duration={}ms reconcile=(s1={} s2={} s3={} s4={}) s1_written={} s2_advanced={} s3_written={} s4_succeeded={} withdrawals={} capped_cl={} capped_spend={} deferred_cl={} deferred_br_rave={}",
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
            deferred_cl_links,
            deferred_br_rave,
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
    /// * `step='new'` and the lock's own proof is in a live CL parked link →
    ///   advance to `cl_link_created` with that link's ActionHash.
    /// * `step='cl_link_created'` and `cl_link_hash` has left the agreement
    ///   it was parked on → that agreement's RAVE consumed the link. Advance
    ///   to `cl_rave_executed` with `cl_rave_hash=NULL` (we can't recover the
    ///   actual RAVE hash after the fact).
    /// * `step='cl_rave_executed'` and the lock's own proof is in a live
    ///   bridging parked spend → advance to `br_spend_created` with that
    ///   spend's ActionHash.
    /// * `step='br_spend_created'` and `br_spend_hash` has left the
    ///   agreement it was parked on → that agreement's RAVE consumed the
    ///   spend. Advance to `br_rave_executed` (simultaneously
    ///   `state='succeeded'`).
    async fn reconcile_pipeline(
        &self,
        conductor: &impl ConductorReads,
        live: &mut LiveLinks,
        context: &DepositContext,
    ) -> Result<ReconcileCounts> {
        let credit_limit: ActionHash = context.credit_limit_adjustment.clone().into();
        let bridging: ActionHash = context.bridging_agreement.clone().into();
        let cl_by_lock = self.links_by_lock(live.on(conductor, &credit_limit).await?);
        let br_by_lock = self.links_by_lock(live.on(conductor, &bridging).await?);
        let mut counts = ReconcileCounts::default();

        for row in self.db.list_pending_by_step("lock", WorkStep::New, 5000)? {
            let Some(link_id) = self.lock_key(&row).and_then(|lock| cl_by_lock.get(&lock)) else {
                continue;
            };
            debug!(
                "[bridge/reconcile] lock={} new → cl_link_created (its proof is in live CL link {})",
                row.item_id, link_id
            );
            self.record_cl_link(row.id, link_id, context)?;
            counts.s1_advanced += 1;
        }

        for row in self
            .db
            .list_pending_by_step("lock", WorkStep::ClLinkCreated, 5000)?
        {
            if self
                .link_consumed(conductor, live, &row, &credit_limit)
                .await?
            {
                self.db.advance_to_cl_rave_executed(row.id, None)?;
                counts.s2_advanced += 1;
            }
        }

        for row in self
            .db
            .list_pending_by_step("lock", WorkStep::ClRaveExecuted, 5000)?
        {
            let Some(spend_id) = self.lock_key(&row).and_then(|lock| br_by_lock.get(&lock)) else {
                continue;
            };
            debug!(
                "[bridge/reconcile] lock={} cl_rave_executed → br_spend_created (its proof is in live bridging spend {})",
                row.item_id, spend_id
            );
            self.record_br_spend(row.id, spend_id, context)?;
            counts.s3_advanced += 1;
        }

        for row in self
            .db
            .list_pending_by_step("lock", WorkStep::BrSpendCreated, 5000)?
        {
            if self.link_consumed(conductor, live, &row, &bridging).await? {
                self.db.advance_to_br_rave_executed(row.id, None)?;
                counts.s4_advanced += 1;
            }
        }

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

    fn record_cl_link(&self, id: i64, link: &str, context: &DepositContext) -> Result<()> {
        let agreement = context.credit_limit_adjustment.to_string();
        self.db.advance_to_cl_link_created(id, link, &agreement)
    }

    fn record_br_spend(&self, id: i64, spend: &str, context: &DepositContext) -> Result<()> {
        let agreement = context.bridging_agreement.to_string();
        self.db.advance_to_br_spend_created(id, spend, &agreement)
    }

    /// Whether the link a row waits on has left the agreement it was parked
    /// on. A read that fails for this row alone leaves it where it is, holding
    /// up no other row. A failing conductor ends the cycle.
    async fn link_consumed(
        &self,
        conductor: &impl ConductorReads,
        live: &mut LiveLinks,
        row: &WorkItem,
        in_force: &ActionHash,
    ) -> Result<bool> {
        let Some((link, recorded)) = row.parked_link() else {
            return Ok(false);
        };
        let (agreement, parked) = match parked_on(conductor, live, link, recorded).await {
            Ok(checked) => checked,
            Err(e) if classify_cycle_failure(&e) != CycleFailureAction::UnclassifiedCooldown => {
                return Err(e.context(format!(
                    "lock {} at {}: its link {} could not be checked",
                    row.item_id, row.step, link
                )));
            }
            Err(e) => {
                error!(
                    event = "bridge.reconcile.unresolved",
                    "[bridge/reconcile] lock={} at {} stays pending, its link {} could not be checked: {:#}",
                    row.item_id,
                    row.step,
                    link,
                    e
                );
                return Ok(false);
            }
        };
        if recorded.is_none() {
            self.db
                .record_parked_agreement(row.id, &agreement.to_string())?;
            info!(
                event = "bridge.reconcile.agreement_recorded",
                "[bridge/reconcile] lock={} link {} was parked on agreement {}",
                row.item_id,
                link,
                agreement
            );
        }
        if !parked {
            debug!(
                "[bridge/reconcile] lock={} at {}: link {} left agreement {}",
                row.item_id, row.step, link, agreement
            );
            return Ok(true);
        }
        if agreement != *in_force {
            error!(
                event = "bridge.reconcile.superseded_agreement",
                "[bridge/reconcile] lock={} waits on agreement {}, no longer in force: only a RAVE on it can consume link {}",
                row.item_id,
                agreement,
                link
            );
        }
        Ok(false)
    }

    /// Advances, through `advance`, each row at `step` whose own proof is in
    /// `taken`, the links the RAVE took off its agreement, and returns how many
    /// it advanced.
    fn advance_consumed(
        &self,
        step: WorkStep,
        taken: &HashMap<LockKey, String>,
        advance: impl Fn(i64) -> Result<()>,
    ) -> Result<usize> {
        let taken_links: HashSet<&str> = taken.values().map(String::as_str).collect();
        let mut advanced = 0;
        for row in self.db.list_pending_by_step("lock", step, 5000)? {
            let (Some((link, _)), Some(lock)) = (row.parked_link(), self.lock_key(&row)) else {
                continue;
            };
            if taken.contains_key(&lock) {
                advance(row.id)?;
                advanced += 1;
            } else if taken_links.contains(link) {
                error!(
                    event = "bridge.rave.proof_missing",
                    lock_id = lock.lock_id,
                    tx_hash = lock.tx_hash,
                    link,
                    "[bridge/rave] lock={} at {}: its recorded link {} went to the RAVE without its proof",
                    row.item_id,
                    row.step,
                    link
                );
            }
        }
        Ok(advanced)
    }

    fn links_by_lock(&self, parked: &[Transaction]) -> HashMap<LockKey, String> {
        links_by_lock(parked, &self.cfg.bridging_agent_pubkey)
    }

    /// The links among `sent` that the RAVE `rave` took off `agreement`, read
    /// from the agreement once it has run: a template leaves a link it rejects
    /// parked. A link gone counts as used because the HOT templates redact
    /// none; were one to, the RAVE's own consumed inputs are the record to read
    /// (workshop `documentation/specs/bridge-stop/README.md` § Operating
    /// assumptions and limits).
    async fn rave_outcome(
        &self,
        conductor: &impl ConductorReads,
        stage: &str,
        agreement: &ActionHash,
        rave: &str,
        sent: &[Transaction],
    ) -> Result<RaveOutcome> {
        let (live, _) = timed_call(stage, "get_parked_links_by_ea", conductor.parked_links(agreement))
            .await
            .with_context(|| {
                format!(
                    "the {stage} RAVE {rave} on agreement {agreement} ran, but what it took could not be read: the next reconcile advances its rows"
                )
            })?;
        let live: HashSet<String> = live.iter().map(|link| link.id.to_string()).collect();
        let (left, taken): (Vec<_>, Vec<_>) = sent
            .iter()
            .cloned()
            .partition(|link| live.contains(&link.id.to_string()));
        let outcome = RaveOutcome { taken, left };
        for link in &outcome.left {
            let locks: Vec<String> = self
                .links_by_lock(std::slice::from_ref(link))
                .into_keys()
                .map(|lock| lock.lock_id)
                .collect();
            if locks.is_empty() {
                warn!(
                    event = "bridge.rave.link_refused",
                    "[bridge/{stage}] the RAVE {rave} on agreement {agreement} left link {} parked",
                    link.id
                );
            } else {
                error!(
                    event = "bridge.rave.link_refused",
                    "[bridge/{stage}] the RAVE {rave} on agreement {agreement} left link {} parked, and with it lock(s) {}",
                    link.id,
                    locks.join(", ")
                );
            }
        }
        Ok(outcome)
    }

    /// The lock [`BridgeOrchestrator::extract_lock_proof`] writes into the row's proof.
    fn lock_key(&self, item: &WorkItem) -> Option<LockKey> {
        match LockPayload::deserialize(item.payload_json.clone()) {
            Ok(payload) => Some(LockKey::new(&payload.lock_id, &payload.tx_hash)),
            Err(e) => {
                error!(
                    event = "bridge.payload_unreadable",
                    "[bridge] lock={} has an unreadable lock payload: {}", item.item_id, e
                );
                None
            }
        }
    }

    /// Build the S1 `create_parked_link` batch from rows at `step='new'`,
    /// respecting the link-tag cap. A row whose payload cannot be read,
    /// encoded, or written at any cap is marked permanently failed and
    /// excluded; a row the cap alone cannot take waits for the next cycle.
    fn build_cl_batch(
        &self,
        vault: Address,
        rows: &[WorkItem],
        tag_cap: usize,
    ) -> Result<ProofBatch> {
        let mut out = ProofBatch::default();
        for item in rows {
            let (proof, amount) = match self.extract_lock_proof(vault, item) {
                Ok(v) => v,
                Err(e) => {
                    error!(
                        "[bridge/s1] proof extraction failed id={} error={}, abandoning the deposit",
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

            let total = UnitMap::sum_vec(tentative_amounts.clone())?;
            let payload = json!({ "proof_of_deposit": &tentative_proofs });
            let tag_bytes = match estimate_parked_data_tag_bytes(&total, &payload) {
                Ok(n) => n,
                Err(e) => {
                    error!(
                        "[bridge/s1] failed to estimate cl tag size id={} error={}, abandoning the deposit",
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
                if !out.ids.is_empty() {
                    info!(
                        "[bridge/s1] batch cap reached size={} next_tag={} cap={}",
                        out.ids.len(),
                        tag_bytes,
                        tag_cap
                    );
                    out.capped = true;
                    break;
                }
                // Nothing but this row is in the tag yet, and a CL tag carries
                // nothing of the network's, so this is the payload on its own.
                match self.resolve_unfittable_head("s1", item, tag_cap, tag_bytes) {
                    UnfittableHead::Deferred => {
                        if !out.capped {
                            error!(
                                "[bridge/s1] nothing fits under the cap: proof id={} measures {} against cap={}, every row waits for the next cycle",
                                item.item_id, tag_bytes, tag_cap
                            );
                        }
                        out.capped = true;
                        continue;
                    }
                    UnfittableHead::Abandoned => continue,
                }
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
    /// failure handling as [`build_cl_batch`], with the agent's ledger in the
    /// measurement.
    fn build_spend_batch(
        &self,
        vault: Address,
        rows: &[WorkItem],
        tag_cap: usize,
        tag_context: &SpendTagContext,
    ) -> Result<ProofBatch> {
        let mut out = ProofBatch::default();
        for item in rows {
            let (proof, amount) = match self.extract_lock_proof(vault, item) {
                Ok(v) => v,
                Err(e) => {
                    error!(
                        "[bridge/s3] proof extraction failed id={} error={}, abandoning the deposit",
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

            let total = UnitMap::sum_vec(tentative_amounts.clone())?;
            let payload = json!({ "proof_of_deposit": &tentative_proofs });
            let tag_bytes = match tag_context.estimate_tag_bytes(&total, &payload) {
                Ok(n) => n,
                Err(e) => {
                    error!(
                        "[bridge/s3] failed to estimate spend tag size id={} error={}, abandoning the deposit",
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
                if !out.ids.is_empty() {
                    info!(
                        "[bridge/s3] batch cap reached size={} next_tag={} cap={}",
                        out.ids.len(),
                        tag_bytes,
                        tag_cap
                    );
                    out.capped = true;
                    break;
                }
                // A payload that will not encode can never be written, so it
                // takes the same verdict as one that is too big.
                let alone = tag_context
                    .payload_only()
                    .estimate_tag_bytes(&amount, &json!({ "proof_of_deposit": [&proof] }))
                    .unwrap_or(usize::MAX);
                match self.resolve_unfittable_head("s3", item, tag_cap, alone) {
                    UnfittableHead::Deferred => {
                        if !out.capped {
                            error!(
                                "[bridge/s3] nothing fits under the cap: proof id={} measures {} against cap={}, of which {} is the ledger, the lanes and the charged units. Every row waits for the next cycle; the cap can be raised to at most {}",
                                item.item_id,
                                tag_bytes,
                                tag_cap,
                                tag_bytes.saturating_sub(alone),
                                LINK_TAG_BYTES_CEILING
                            );
                        }
                        out.capped = true;
                        continue;
                    }
                    UnfittableHead::Abandoned => continue,
                }
            }

            out.ids.push(item.id);
            out.proofs = tentative_proofs;
            out.amounts = tentative_amounts;
            out.tag_bytes = tag_bytes;
        }
        Ok(out)
    }

    /// What becomes of a proof that will not fit while the batch is still
    /// empty. Only its own payload measuring over the ceiling is the row's
    /// fault: the cap, the agent's ledger, the network's lanes and its charged
    /// units all move without it, and a deposit abandoned here is never
    /// retried.
    fn resolve_unfittable_head(
        &self,
        stage: &str,
        item: &WorkItem,
        tag_cap: usize,
        alone: usize,
    ) -> UnfittableHead {
        if alone <= LINK_TAG_BYTES_CEILING {
            return UnfittableHead::Deferred;
        }
        error!(
            "[bridge/{}] proof id={} cannot be written at any cap (size={} > ceiling={}), abandoning the deposit",
            stage, item.item_id, alone, LINK_TAG_BYTES_CEILING
        );
        if let Err(db_err) = self.db.mark_failed_permanent(
            item.id,
            &format!(
                "proof exceeds the link tag ceiling (size={alone}, ceiling={LINK_TAG_BYTES_CEILING}, cap={tag_cap})"
            ),
        ) {
            error!(
                "[bridge] failed to mark lock {} failed: {}",
                item.id, db_err
            );
        }
        UnfittableHead::Abandoned
    }

    fn extract_lock_proof(&self, vault: Address, item: &WorkItem) -> Result<(Value, UnitMap)> {
        let payload = LockPayload::deserialize(item.payload_json.clone())?;
        let contract_hex = format!("{vault:x}");
        let depositor = decode_holochain_agent_as_pubkey_string(&payload.holochain_agent)?;
        let normalized = payload.normalized_amounts()?;
        let amount = normalized.amount_hot.clone();
        let lock = LockKey::new(&payload.lock_id, &payload.tx_hash);

        let proof = json!({
            "method": "deposit",
            "contract_address": format!("0x{}", contract_hex.to_lowercase()),
            "amount": amount,
            "depositor_wallet_address": depositor,
            "lock_id": lock.lock_id,
            "tx_hash": lock.tx_hash,
        });

        debug!(
            "[bridge/s1] extracted proof id={} amount={} agent={} tx_hash={}",
            payload.lock_id, amount, payload.holochain_agent, payload.tx_hash
        );

        Ok((
            proof,
            UnitMap::from(vec![(self.cfg.hot_unit_index, amount.as_str())]),
        ))
    }

    async fn resolve_deposit_context(
        conductor: &impl ConductorReads,
        bridging_agent: &AgentPubKeyB64,
        hot_unit_index: u32,
        global_definition: &GlobalDefinitionExt,
    ) -> Result<DepositContext> {
        let lanes = conductor.all_lanes().await?;
        let lane_definition_count = lanes
            .iter()
            .filter(|lane| lane.definition.is_some())
            .count();

        let mut candidates = vec![CandidateLane {
            name: "the global definition's lane".to_string(),
            version: None,
            definition: global_definition.lane_def.clone(),
        }];
        for lane in lanes {
            let Some(newest) = lane.definition else {
                continue;
            };
            let Some(in_force) = conductor
                .version_in_force(newest.definition_hash.into())
                .await?
            else {
                continue;
            };
            candidates.push(CandidateLane {
                name: format!(
                    "lane {:?} ({})",
                    lane.basic_properties.name, newest.origin_id
                ),
                definition: conductor.lane_definition(in_force.clone()).await?,
                version: Some(in_force),
            });
        }

        let credit_limit = UnitMap::from(vec![(hot_unit_index, "0")]);
        let mut matching = candidates
            .into_iter()
            .filter(|lane| lane.accepts_adjustment(bridging_agent, &credit_limit));
        let lane = match (matching.next(), matching.next()) {
            (Some(lane), None) => lane,
            (None, _) => anyhow::bail!(
                "no lane in force has bridging agent {bridging_agent} and service unit {hot_unit_index}"
            ),
            (Some(first), Some(second)) => {
                let names: Vec<String> = [first, second]
                    .into_iter()
                    .chain(matching)
                    .map(|lane| lane.name)
                    .collect();
                anyhow::bail!(
                    "more than one lane in force has bridging agent {bridging_agent} and service unit {hot_unit_index}: {}",
                    names.join(", ")
                )
            }
        };
        let agreements = lane.definition.rave_agreements;
        let credit_limit_adjustment = agreements
            .credit_limit_adjustment
            .with_context(|| format!("{} sets no credit limit adjustment", lane.name))?;
        let bridging_agreement = agreements
            .bridging_agreement
            .with_context(|| format!("{} sets no bridging agreement", lane.name))?;
        Ok(DepositContext {
            lane: lane.name,
            lane_definitions: lane.version.into_iter().collect(),
            lane_definition_count,
            credit_limit_adjustment,
            bridging_agreement,
        })
    }
}

struct CandidateLane {
    name: String,
    version: Option<ActionHash>,
    definition: LaneDefinition,
}

impl CandidateLane {
    /// Mirrors `adjustment_refusal` in the alliance DNA's RAVE validation.
    fn accepts_adjustment(&self, author: &AgentPubKeyB64, credit_limit: &UnitMap) -> bool {
        let is_global_lane = self.version.is_none();
        self.definition.special_agents.bridging_agent.pub_key == *author
            && (is_global_lane || !credit_limit.contains_base_unit())
            && credit_limit
                .get_service_units()
                .iter()
                .all(|unit| self.definition.service_units.0.contains_key(unit))
    }
}

/// A trait so a test can stand in for the conductor.
trait ConductorReads {
    async fn global_definition(&self) -> Result<GlobalDefinitionExt>;
    async fn all_lanes(&self) -> Result<Vec<LaneExt>>;
    async fn version_in_force(&self, newest: ActionHash) -> Result<Option<ActionHash>>;
    async fn lane_definition(&self, version: ActionHash) -> Result<LaneDefinition>;
    async fn parked_links(&self, agreement: &ActionHash) -> Result<Vec<Transaction>>;
    async fn agreement_of(&self, link: ActionHash) -> Result<ActionHash>;
    async fn ledger(&self) -> Result<Ledger>;
}

/// The writes a bridge cycle makes, each answering the ActionHash it wrote.
trait ConductorWrites {
    async fn create_parked_link(&self, input: &CreateParkedLinkInput) -> Result<ActionHashB64>;
    async fn execute_rave(&self, input: &RAVEExecuteInputs) -> Result<ActionHash>;
    async fn create_parked_spend(&self, input: &CreateParkedSpendInput) -> Result<ActionHashB64>;
}

/// Each agreement's parked links, and each link's agreement, as reconcile
/// first read them, a failed read included, so rows sharing one cost one read.
#[derive(Default)]
struct LiveLinks {
    parked: HashMap<ActionHash, Result<Vec<Transaction>, String>>,
    agreements: HashMap<ActionHash, Result<ActionHash, String>>,
}

impl LiveLinks {
    async fn on(
        &mut self,
        conductor: &impl ConductorReads,
        agreement: &ActionHash,
    ) -> Result<&[Transaction]> {
        let read = conductor.parked_links(agreement);
        read_once(&mut self.parked, agreement, read)
            .await
            .map(Vec::as_slice)
    }

    async fn agreement_of(
        &mut self,
        conductor: &impl ConductorReads,
        link: ActionHash,
    ) -> Result<ActionHash> {
        let read = conductor.agreement_of(link.clone());
        read_once(&mut self.agreements, &link, read).await.cloned()
    }
}

async fn read_once<'a, T>(
    reads: &'a mut HashMap<ActionHash, Result<T, String>>,
    hash: &ActionHash,
    read: impl std::future::Future<Output = Result<T>>,
) -> Result<&'a T> {
    if !reads.contains_key(hash) {
        let read = read.await.map_err(|e| format!("{e:#}"));
        reads.insert(hash.clone(), read);
    }
    reads[hash].as_ref().map_err(|e| anyhow::anyhow!("{e}"))
}

struct Conductor<'a> {
    ham: &'a Ham,
    role_name: &'a str,
}

impl Conductor<'_> {
    async fn record(&self, hash: &ActionHash) -> Result<Record> {
        self.ham
            .call_zome(
                self.role_name,
                "transactor",
                "hdk_must_get_valid_record",
                hash,
            )
            .await
    }
}

impl ConductorReads for Conductor<'_> {
    async fn global_definition(&self) -> Result<GlobalDefinitionExt> {
        self.ham
            .call_zome(
                self.role_name,
                "transactor",
                "get_current_global_definition",
                &Some(GetStrategy::Local),
            )
            .await
            .context("failed to read the current global definition")
    }

    async fn all_lanes(&self) -> Result<Vec<LaneExt>> {
        self.ham
            .call_zome(
                self.role_name,
                "transactor",
                "get_all_lane",
                &Some(GetStrategy::Local),
            )
            .await
            .context("failed to read the network's lanes")
    }

    async fn version_in_force(&self, newest: ActionHash) -> Result<Option<ActionHash>> {
        let in_force: Vec<ActionHash> = self
            .ham
            .call_zome(
                self.role_name,
                "transactor",
                "get_lane_definitions_in_force",
                &vec![newest],
            )
            .await
            .context("failed to read which lane definition is in force")?;
        Ok(in_force.into_iter().next())
    }

    async fn lane_definition(&self, version: ActionHash) -> Result<LaneDefinition> {
        let record = self
            .record(&version)
            .await
            .with_context(|| format!("failed to read lane definition {version}"))?;
        lane_definition_of(&record)
    }

    async fn parked_links(&self, agreement: &ActionHash) -> Result<Vec<Transaction>> {
        self.ham
            .call_zome(
                self.role_name,
                "transactor",
                "get_parked_links_by_ea",
                agreement,
            )
            .await
            .with_context(|| format!("failed to read the links parked on agreement {agreement}"))
    }

    async fn agreement_of(&self, link: ActionHash) -> Result<ActionHash> {
        let record = self
            .record(&link)
            .await
            .with_context(|| format!("failed to read parked link {link}"))?;
        agreement_parked_on(&record)
    }

    async fn ledger(&self) -> Result<Ledger> {
        self.ham
            .call_zome(self.role_name, "transactor", "get_ledger", &())
            .await
    }
}

impl ConductorWrites for Conductor<'_> {
    async fn create_parked_link(&self, input: &CreateParkedLinkInput) -> Result<ActionHashB64> {
        let (link, _executor): (ActionHashB64, AgentPubKey) = self
            .ham
            .call_zome(self.role_name, "transactor", "create_parked_link", input)
            .await
            .context("create_parked_link failed")?;
        Ok(link)
    }

    async fn execute_rave(&self, input: &RAVEExecuteInputs) -> Result<ActionHash> {
        let (_rave, hash): (RAVE, ActionHash) = self
            .ham
            .call_zome(self.role_name, "transactor", "execute_rave", input)
            .await
            .context("execute_rave failed")?;
        Ok(hash)
    }

    async fn create_parked_spend(&self, input: &CreateParkedSpendInput) -> Result<ActionHashB64> {
        self.ham
            .call_zome(self.role_name, "transactor", "create_parked_spend", input)
            .await
            .context("create_parked_spend failed")
    }
}

/// A parked link points from the agreement it was parked on.
fn agreement_parked_on(record: &Record) -> Result<ActionHash> {
    match &record.action().data {
        ActionData::CreateLink(link) => link.base_address.clone().into_action_hash(),
        _ => None,
    }
    .with_context(|| {
        format!(
            "{} is not a link parked on an agreement",
            record.action_address()
        )
    })
}

/// The agreement a link was parked on, learned from the link when `recorded`
/// names none, and whether the link is still parked there.
async fn parked_on(
    conductor: &impl ConductorReads,
    live: &mut LiveLinks,
    link: &str,
    recorded: Option<&str>,
) -> Result<(ActionHash, bool)> {
    let agreement = match recorded {
        Some(agreement) => action_hash_from(agreement)?,
        None => {
            live.agreement_of(conductor, action_hash_from(link)?)
                .await?
        }
    };
    let parked = live
        .on(conductor, &agreement)
        .await?
        .iter()
        .any(|t| t.id.to_string() == link);
    Ok((agreement, parked))
}

fn action_hash_from(b64: &str) -> Result<ActionHash> {
    ActionHashB64::from_b64_str(b64)
        .map(Into::into)
        .with_context(|| format!("{b64} is not an action hash"))
}

fn lane_definition_of(record: &Record) -> Result<LaneDefinition> {
    LaneDefinition::try_from(record).map_err(|e| {
        anyhow::anyhow!(
            "{} is not a lane definition: {e:?}",
            record.action_address()
        )
    })
}

enum UnfittableHead {
    Deferred,
    Abandoned,
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

#[derive(Debug)]
struct DepositContext {
    lane: String,
    /// Empty means the spend names no lane and the zome resolves its own list,
    /// bounded by `lane_definition_count`.
    lane_definitions: Vec<ActionHash>,
    lane_definition_count: usize,
    credit_limit_adjustment: ActionHashB64,
    bridging_agreement: ActionHashB64,
}

/// The links a RAVE was sent, split by whether it took them off their
/// agreement.
struct RaveOutcome {
    taken: Vec<Transaction>,
    left: Vec<Transaction>,
}

/// Truncate `links` to at most `cap` entries. Returns the (possibly
/// truncated) Vec and the count of deferred entries.
///
/// `cap == None` or `cap == Some(0)` means no cap. Order-preserving, so S4 can
/// keep deposits ahead of withdrawals and have the cap defer withdrawals first.
fn apply_rave_link_cap(
    mut links: Vec<Transaction>,
    cap: Option<usize>,
) -> (Vec<Transaction>, usize) {
    match cap {
        Some(n) if n > 0 && links.len() > n => {
            let deferred = links.len() - n;
            links.truncate(n);
            (links, deferred)
        }
        _ => (links, 0),
    }
}

/// The `HamConfig` every connection this orchestrator makes is built from.
/// Lair signing is required, never best-effort: the bridging agent carries its
/// key across migrations, and the signing path `ham` would otherwise use
/// commits a capability grant to that chain on every connect. Against a chain
/// that has already closed, that grant is invalid and costs the agent its
/// migration for good.
fn ham_config(cfg: &Config) -> Result<HamConfig> {
    HamConfig::new(cfg.admin_port, cfg.app_port, cfg.app_id.clone())
        .with_request_timeout_secs(cfg.ham_request_timeout_secs)
        .with_signing(
            LairCredentials::Node {
                conductor_config: cfg.conductor_config.clone().into(),
                passphrase_file: cfg.lair_passphrase_file.clone().into(),
            },
            CapGrantOptIn::Withheld,
        )
        .context(
            "CONDUCTOR_CONFIG / LAIR_PASSPHRASE_FILE must name a node whose conductor runs an \
             external lair_server",
        )
}

/// One connect path, shared by startup and reconnect. Rebuilds the signing
/// config on every attempt: `reset-lair.sh` rewrites the conductor's
/// `connection_url` under a running orchestrator, and a config read once at
/// startup would dial the old keystore until someone restarted the service.
async fn connect_ham(cfg: &Config) -> Result<Ham> {
    Ham::connect(ham_config(cfg)?)
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

/// The `ParkedData` a `create_parked_link` writes, which the zome puts in the
/// link tag exactly as given.
fn parked_data(total_amount: &UnitMap, payload: &Value) -> ParkedData {
    ParkedData {
        ct_role_id: ORACLE_ROLE.to_string(),
        amount: Some(total_amount.clone()),
        payload: payload.clone(),
    }
}

/// Estimate the msgpack link-tag size a `ParkedData` write with the given
/// aggregate proofs payload would produce. Used to cap the
/// `create_parked_link` batch against Holochain's link tag size limit.
fn estimate_parked_data_tag_bytes(total_amount: &UnitMap, payload: &Value) -> Result<usize> {
    let bytes = rmp_serde::to_vec(&(parked_data(total_amount, payload), true))
        .context("failed to msgpack-encode ParkedData for tag-size estimation")?
        .len();
    Ok(bytes)
}

/// What a `create_parked_spend` writes into its link tag beyond the batch
/// itself: the agent's running ledger, which the zome copies back into the tag,
/// and the definitions and per-unit fees the write is measured against.
struct SpendTagContext {
    ledger: Ledger,
    global_definition: ActionHash,
    lane_definitions: Vec<ActionHash>,
    unit_fees: Vec<UnitFee>,
}

impl SpendTagContext {
    fn estimate_tag_bytes(&self, total_amount: &UnitMap, payload: &Value) -> Result<usize> {
        let bytes = rmp_serde::to_vec(&self.widest_spend_data(total_amount, payload))
            .context("failed to msgpack-encode ParkedSpendData for tag-size estimation")?
            .len();
        Ok(bytes)
    }

    /// The widest `ParkedSpendData` the zome can write for this batch. A parked
    /// spend is a `DirectCommitment`, which takes the balance the ledger holds
    /// and applies this spend and its fee, adds that fee to what is owed and
    /// states it, and leaves the proposed balance and the carry-forward units as
    /// they stand.
    fn widest_spend_data(&self, total_amount: &UnitMap, payload: &Value) -> ParkedSpendData {
        let charged: Vec<String> = self.unit_fees.iter().map(UnitFee::index_key).collect();
        let spent = total_amount.get_unit_indexes();
        let widest_over = |held: Vec<String>, added: &[String]| {
            widest_amounts(held.into_iter().chain(added.iter().cloned()))
        };
        ParkedSpendData {
            ct_role_id: BRIDGING_AGENT_ROLE.to_string(),
            amount: total_amount.clone(),
            fee: widest_amounts(charged.clone()),
            payload: payload.clone(),
            global_definition: self.global_definition.clone(),
            lane_definitions: self.lane_definitions.clone(),
            new_balance: widest_over(
                self.ledger.balance.get_unit_indexes(),
                &[spent, charged.clone()].concat(),
            ),
            carry_forward_units: self.ledger.carry_forward_units.clone(),
            fees_owed: widest_over(self.ledger.fees_owed.get_unit_indexes(), &charged),
            proposed_balance: widest_over(self.ledger.proposed_balance.get_unit_indexes(), &[]),
        }
    }

    /// The same tag with everything the network puts in it stripped out: the
    /// ledger, the lanes and the charged units all move on their own, so only
    /// what is left says a proof can never be written rather than not now.
    fn payload_only(&self) -> Self {
        Self {
            ledger: Ledger::empty(),
            global_definition: self.global_definition.clone(),
            lane_definitions: vec![],
            unit_fees: vec![],
        }
    }
}

/// The lane definitions the tag will carry. A spend that names none leaves the
/// zome to resolve its own list, which it writes into the tag, so the estimate
/// stands in for every lane definition it could resolve.
fn lane_definitions_written(context: &DepositContext) -> Vec<ActionHash> {
    if context.lane_definitions.is_empty() {
        vec![ActionHash::from_raw_32(vec![0; 32]); context.lane_definition_count]
    } else {
        context.lane_definitions.clone()
    }
}

/// The given units at the longest amount `ZFuel` prints. Only the key set can
/// be known before the write: a balance moves with every transaction, and a fee
/// lands on top of an amount already at its trigger, which `fee_cap` and the
/// exemptions only ever narrow.
fn widest_amounts(units: impl IntoIterator<Item = String>) -> UnitMap {
    UnitMap::load(
        units
            .into_iter()
            .map(|unit| (unit, ZFuel::new_with_default_precision(i64::MIN)))
            .collect(),
    )
}

fn normalize_tx_hash(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

/// The lock a deposit proof stands for. Neither field names a lock alone: the
/// ID repeats across vaults, and one transaction can lock twice.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LockKey {
    lock_id: String,
    tx_hash: String,
}

impl LockKey {
    fn new(lock_id: &str, tx_hash: &str) -> Self {
        Self {
            lock_id: lock_id.to_string(),
            tx_hash: normalize_tx_hash(tx_hash),
        }
    }

    fn of_proof(proof: &Value) -> Option<Self> {
        Some(Self::new(
            proof.get("lock_id")?.as_str()?,
            proof.get("tx_hash")?.as_str()?,
        ))
    }
}

/// Each deposit proof the bridging agent's own parked links carry, by its
/// lock, with the ActionHash of the link carrying it. Anyone can park a spend
/// carrying a copy of a proof, so a link the agent did not sign counts for
/// nothing.
fn links_by_lock(
    parked: &[Transaction],
    bridging_agent: &AgentPubKeyB64,
) -> HashMap<LockKey, String> {
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
        let Some(proofs) = payload.get("proof_of_deposit") else {
            continue;
        };
        let link = tx.id.to_string();
        if tx.creator != *bridging_agent {
            warn!(
                event = "bridge.proof_foreign",
                link,
                creator = %tx.creator,
                "[bridge] link {link} by {} carries a proof_of_deposit, which counts only on the bridging agent's own links",
                tx.creator
            );
            continue;
        }
        let Some(proofs) = proofs.as_array() else {
            error!(
                event = "bridge.proof_unreadable",
                link, "[bridge] link {link} carries a proof_of_deposit that is not a list"
            );
            continue;
        };
        for (index, proof) in proofs.iter().enumerate() {
            let Some(lock) = LockKey::of_proof(proof) else {
                error!(
                    event = "bridge.proof_unreadable",
                    link, "[bridge] proof {index} in link {link} names no lock: {proof}"
                );
                continue;
            };
            if let Some(other) = out.insert(lock.clone(), link.clone()) {
                if other != link {
                    error!(
                        event = "bridge.proof_duplicated",
                        lock_id = lock.lock_id,
                        tx_hash = lock.tx_hash,
                        "[bridge] lock {} in {} is carried by two live links, {other} and {link}: a RAVE taking both acts on it twice",
                        lock.lock_id,
                        lock.tx_hash
                    );
                }
            }
        }
    }
    out
}

/// Wrap a zome-call future with elapsed-ms measurement and structured logging.
/// The elapsed time comes back with the result so callers can drive
/// stage-ejection policy, and is logged on the error path too.
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
            error = %format!("{e:#}"),
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
    use alloy::signers::local::PrivateKeySigner;
    use holo_hash::{ActionHash, AgentPubKey, AgentPubKeyB64};
    use holochain_client::ExternIO;
    use holochain_zome_types::prelude::{
        Action, ActionHashed, ActionHeader, CreateData, CreateLinkData, Entry, EntryHash,
        EntryType, LinkTag, RecordEntry, Signature, SignedActionHashed,
    };
    use holochain_zome_types::timestamp::Timestamp;
    use rave_engine::types::{
        AddressBook, CommonRAVEAgreements, CommonSpecialAgents, LaneBasicPropertiesExt,
        LaneDefinitionExt, TransactionType, UnitIndexMap,
    };
    use serde::de::IgnoredAny;
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::{SystemTime, UNIX_EPOCH};
    use zfuel::fraction::Fraction;
    use zfuel::fuel::Precision;

    fn test_db_path(name: &str) -> String {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("/tmp/bridge-orchestrator-orch-{}-{}.db", name, ts)
    }

    fn test_config(db_path: String) -> Config {
        let agent_pubkey: AgentPubKeyB64 = AgentPubKey::from_raw_32(vec![1u8; 32]).into();
        Config {
            poll_interval_ms: 1000,
            bridge_cycle_interval_ms: 1000,
            max_link_tag_bytes: 800,
            coupons_target_bytes: 512 * 1024,
            db_path,
            role_name: "alliance".to_string(),
            app_id: "bridging-app".to_string(),
            admin_port: 0,
            app_port: 0,
            conductor_config: "/etc/holochain/conductor-config.yaml".to_string(),
            lair_passphrase_file: "/var/lib/holochain/lair-passphrase".to_string(),
            bridging_agent_pubkey: agent_pubkey,
            hot_unit_index: 1,
            ham_request_timeout_secs: 120,
            ham_reconnect_backoff_initial_ms: 1000,
            ham_reconnect_backoff_max_ms: 30000,
            ham_reconnect_escalate_after: 5,
            ham_pressure_cooldown_ms: 30000,
            ham_pressure_cooldown_max_ms: 90000,
            slow_call_threshold_ms: 35000,
            rave_max_links: None,
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
            ethereum: Some(EthereumSide {
                chain: Ethereum {
                    network: Network::Sepolia,
                    rpc_url: "http://localhost:0".to_string(),
                    lock_vault_address: VAULT,
                    confirmations: 5,
                },
                signer: CouponSigner::with_key(PrivateKeySigner::random()),
            }),
        }
    }

    const VAULT: Address = Address::ZERO;

    const LAIR_URL: &str = "unix:///var/lib/holochain/lair/socket?k=abc123";

    /// A node laid out as the fleet lays one out: a conductor config naming an
    /// external `lair_server`, and the passphrase that unlocks it.
    fn node_with_lair() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("conductor-config.yaml"),
            format!("keystore:\n  type: lair_server\n  connection_url: {LAIR_URL}\n"),
        )
        .expect("write conductor config");
        std::fs::write(dir.path().join("lair-passphrase"), b"deadbeef\n")
            .expect("write lair passphrase");
        dir
    }

    /// `test_config` pointed at `node`'s files instead of the fleet's, so the
    /// verdict comes from the fixture rather than from whatever this machine
    /// happens to have at /etc/holochain.
    fn config_for_node(name: &str, conductor_config: &str, node: &tempfile::TempDir) -> Config {
        let mut cfg = test_config(test_db_path(name));
        cfg.conductor_config = node.path().join(conductor_config).display().to_string();
        cfg.lair_passphrase_file = node.path().join("lair-passphrase").display().to_string();
        cfg
    }

    #[test]
    fn the_orchestrator_connects_through_lair() {
        let node = node_with_lair();
        let cfg = ham_config(&config_for_node("lair", "conductor-config.yaml", &node))
            .expect("a node with an external lair_server configures lair signing");
        assert_eq!(
            cfg.lair.expect("lair signing").connection_url.as_str(),
            LAIR_URL
        );
        assert!(
            !cfg.allow_cap_grant_signing,
            "the orchestrator never asks ham for the path that writes to the bridging agent's chain"
        );
    }

    #[test]
    fn a_node_without_lair_stops_the_orchestrator() {
        let node = node_with_lair();
        let err = ham_config(&config_for_node(
            "no-lair",
            "absent-conductor-config.yaml",
            &node,
        ))
        .expect_err("without lair there is no signing path that does not write to the chain");
        let err = format!("{err:#}");
        // ham states the fault; the orchestrator names the knobs to turn.
        assert!(err.contains("lair signing is required"), "{err}");
        assert!(err.contains("CONDUCTOR_CONFIG"), "{err}");
    }

    fn action_hash(seed: u8) -> ActionHash {
        ActionHash::from_raw_32(vec![seed; 32])
    }

    const CL_EA: u8 = 0xEA;
    const BR_EA: u8 = 0xEB;

    fn ea(seed: u8) -> String {
        action_hash(seed).to_string()
    }

    /// The fields of a deposit proof that name its lock.
    fn proof(lock_id: &str, tx_hash: &str) -> Value {
        json!({ "lock_id": lock_id, "tx_hash": tx_hash })
    }

    /// The agent `test_config` bridges for, which signs every link the
    /// orchestrator writes.
    fn bridging_agent() -> AgentPubKeyB64 {
        AgentPubKey::from_raw_32(vec![1u8; 32]).into()
    }

    fn signed_by_another(mut link: Transaction) -> Transaction {
        link.creator = AgentPubKey::from_raw_32(vec![0xF0; 32]).into();
        link
    }

    fn parked_tx(seed: u8, proofs: &[Value]) -> Transaction {
        let id = action_hash(seed).into();
        let executor = bridging_agent();
        let ea_id = action_hash(CL_EA).into();
        Transaction {
            id,
            tx_type: TransactionType::Parked,
            amount: UnitMap::new(),
            fee: UnitMap::new(),
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
                attached_payload: json!({ "proof_of_deposit": proofs }),
                consumed_link: false,
            },
        }
    }

    fn parked_spend_tx(seed: u8, proofs: &[Value]) -> Transaction {
        let id = action_hash(seed).into();
        let spender = bridging_agent();
        let executor = bridging_agent();
        let ea_id = action_hash(BR_EA).into();
        Transaction {
            id,
            tx_type: TransactionType::ParkedSpend,
            amount: UnitMap::new(),
            fee: UnitMap::new(),
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
                fees_owed: UnitMap::new(),
                proposed_balance: UnitMap::new(),
                attached_payload: json!({ "proof_of_deposit": proofs }),
            },
        }
    }

    /// Enqueue a lock row `extract_lock_proof` can read, with `item_id` as
    /// its lock ID.
    fn enqueue_lock(orch: &BridgeOrchestrator, item_id: &str, tx_hash: &str) -> i64 {
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
        let total = UnitMap::sum_vec(amounts).expect("amount accumulation should succeed");
        assert_eq!(
            total.get("1").map(|v| v.to_string()),
            Some("25".to_string())
        );
    }

    fn unit_fee(unit_index: u8, trigger: &str) -> UnitFee {
        UnitFee {
            // Seeded apart from `unit_index`: nothing binds the two.
            unit_definition: action_hash(0xD0 ^ unit_index).into(),
            unit_index,
            spender_pay_percent: Fraction::new(1, 100).unwrap(),
            fee_cap: None,
            fee_trigger: trigger.parse().unwrap(),
            exempt_agents: vec![],
        }
    }

    fn widest_amount() -> ZFuel {
        ZFuel::new_with_default_precision(i64::MIN)
    }

    fn pending_rows(orch: &BridgeOrchestrator) -> Vec<WorkItem> {
        orch.db
            .list_pending_by_step("lock", WorkStep::New, 100)
            .unwrap()
    }

    fn tag_context(ledger: Ledger, unit_fees: &[UnitFee]) -> SpendTagContext {
        SpendTagContext {
            ledger,
            global_definition: action_hash(0xAA),
            lane_definitions: vec![action_hash(0xB1), action_hash(0xB2)],
            unit_fees: unit_fees.to_vec(),
        }
    }

    fn cl_estimate(total: &UnitMap, proofs: &[Value]) -> usize {
        estimate_parked_data_tag_bytes(total, &json!({ "proof_of_deposit": proofs }))
            .expect("cl tag estimation must succeed")
    }

    fn spend_estimate(ctx: &SpendTagContext, total: &UnitMap, proofs: &[Value]) -> usize {
        ctx.estimate_tag_bytes(total, &json!({ "proof_of_deposit": proofs }))
            .expect("spend tag estimation must succeed")
    }

    fn spend_tag(ledger: Ledger, total: &UnitMap, unit_fees: &[UnitFee]) -> ParkedSpendData {
        tag_context(ledger, unit_fees).widest_spend_data(total, &json!({ "proof_of_deposit": [] }))
    }

    fn tag_len(data: ParkedSpendData) -> usize {
        ParkedLinkType::ParkedSpendBalance(data)
            .link_tag()
            .expect("the zome encoder must accept the tag")
            .0
            .len()
    }

    #[test]
    fn fees_owed_covers_every_charged_unit_at_the_longest_amount_zfuel_prints() {
        let owed = spend_tag(
            Ledger::empty(),
            &UnitMap::new(),
            &[unit_fee(0, "100"), unit_fee(7, "250.5")],
        )
        .fees_owed;
        let expected = UnitMap::load(
            [
                ("0".to_string(), widest_amount()),
                ("7".to_string(), widest_amount()),
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(owed, expected);
        assert_eq!(widest_amount().to_string(), "-9223372036854.775808");
    }

    #[test]
    fn fees_owed_keeps_what_the_ledger_owes_in_a_unit_no_longer_charged() {
        let ledger = Ledger::new(vec![], vec![], vec![(4, "2")], vec![]);
        assert_eq!(
            spend_tag(ledger, &UnitMap::new(), &[unit_fee(0, "100")])
                .fees_owed
                .get_unit_indexes(),
            vec!["0".to_string(), "4".to_string()]
        );
    }

    #[test]
    fn fees_owed_ignores_everything_that_only_narrows_a_charge() {
        let plain = unit_fee(0, "100");
        let mut narrowed = unit_fee(0, "999999");
        narrowed.fee_cap = Some("1".parse().unwrap());
        narrowed.exempt_agents = vec![AgentPubKey::from_raw_32(vec![1u8; 32]).into()];

        assert_eq!(
            spend_tag(Ledger::empty(), &UnitMap::new(), &[plain]).fees_owed,
            spend_tag(Ledger::empty(), &UnitMap::new(), &[narrowed]).fees_owed
        );
    }

    #[test]
    fn a_spend_link_tag_is_the_bare_msgpack_of_parked_spend_data() {
        // The estimate counts the struct; the zome writes the tag. A variant
        // header or any framing on either side would put them apart, and
        // nothing else here would notice.
        let total = UnitMap::from(vec![(1_u32, "10")]);
        let payload = json!({ "proof_of_deposit": [{ "tx_hash": "0x01" }] });
        let ctx = tag_context(
            Ledger::new(
                vec![(2, "-9223372036854.775807"), (3, "4"), (11, "0.5")],
                vec![(3, vec!["2", "3"])],
                vec![(0, "0.5")],
                vec![(2, "7"), (11, "1")],
            ),
            &[unit_fee(0, "100"), unit_fee(1, "250.5")],
        );
        let data = ctx.widest_spend_data(&total, &payload);

        let tag = ParkedLinkType::ParkedSpendBalance(data.clone())
            .link_tag()
            .expect("the zome encoder must accept the tag");

        assert_eq!(rmp_serde::to_vec(&data).unwrap(), tag.0);
        assert_eq!(
            ctx.estimate_tag_bytes(&total, &payload).unwrap(),
            tag.0.len()
        );
    }

    #[test]
    fn a_cl_link_tag_is_the_bare_msgpack_of_parked_data_and_its_flag() {
        let total = UnitMap::from(vec![(1_u32, "10")]);
        let payload = json!({ "proof_of_deposit": [{ "tx_hash": "0x01" }] });
        let data = parked_data(&total, &payload);

        let tag = ParkedLinkType::ParkedData((data.clone(), true))
            .link_tag()
            .expect("the zome encoder must accept the tag");

        assert_eq!(rmp_serde::to_vec(&(data, true)).unwrap(), tag.0);
        assert_eq!(
            estimate_parked_data_tag_bytes(&total, &payload).unwrap(),
            tag.0.len()
        );
    }

    #[test]
    fn no_legal_zfuel_prints_wider_than_the_widest_amount() {
        let widest = widest_amount().to_string().len();
        for value in Precision::MIN..=Precision::MAX {
            let precision = Precision::new(value).unwrap();
            // The default precision's negative bound is i64::MIN, which negates
            // to itself.
            let extremes = [
                ZFuel::max_units_at(precision) as i64,
                (ZFuel::min_units_abs_at(precision) as i64).wrapping_neg(),
            ];
            for units in extremes {
                let amount = ZFuel::new(units, precision).expect("a bound is in range");
                assert!(
                    amount.to_string().len() <= widest,
                    "{amount} at precision {value} is wider than the {widest} bytes charged for it"
                );
            }
        }
    }

    #[test]
    fn spend_tag_estimate_upper_bounds_the_widest_the_zome_could_write() {
        let held = || {
            Ledger::new(
                vec![
                    (0, "12.5"),
                    (2, "-9223372036854.775807"),
                    (3, "1"),
                    (5, "0.000001"),
                ],
                vec![(3, vec!["2", "3"])],
                vec![(0, "0.5")],
                vec![(2, "7")],
            )
        };
        let one = || UnitMap::from(vec![(1_u32, "10")]);
        let cases: Vec<(&str, Ledger, UnitMap, Vec<UnitFee>)> = vec![
            (
                "an empty ledger",
                Ledger::empty(),
                one(),
                vec![unit_fee(1, "100")],
            ),
            ("nothing charged", held(), one(), vec![]),
            (
                "units the batch never names",
                held(),
                one(),
                vec![unit_fee(0, "100"), unit_fee(1, "250.5")],
            ),
            (
                "a fee on a unit in neither the ledger nor the batch",
                held(),
                one(),
                vec![unit_fee(9, "100")],
            ),
            (
                "a multi-unit batch",
                held(),
                UnitMap::from(vec![(1_u32, "10"), (9, "2.5")]),
                vec![unit_fee(0, "100")],
            ),
            (
                "three-character unit keys",
                Ledger::new(vec![(255, "1")], vec![], vec![], vec![(200, "2")]),
                UnitMap::from(vec![(200_u32, "10")]),
                vec![unit_fee(255, "100")],
            ),
        ];

        let payload = json!({ "proof_of_deposit": [{ "tx_hash": "0x01" }] });
        for (name, ledger, amount, fees) in cases {
            let ctx = tag_context(ledger.clone(), &fees);
            let charged: Vec<String> = fees.iter().map(UnitFee::index_key).collect();
            let spent = amount.get_unit_indexes();
            // What a `DirectCommitment` leaves behind, every entry as wide as
            // `ZFuel` can print it: the balance takes this spend and its fee,
            // what is owed takes the fee, the proposed balance is untouched.
            let written = tag_len(ParkedSpendData {
                new_balance: widest_amounts(
                    ledger
                        .balance
                        .get_unit_indexes()
                        .into_iter()
                        .chain(spent)
                        .chain(charged.clone()),
                ),
                fees_owed: widest_amounts(
                    ledger
                        .fees_owed
                        .get_unit_indexes()
                        .into_iter()
                        .chain(charged),
                ),
                proposed_balance: widest_amounts(ledger.proposed_balance.get_unit_indexes()),
                ..ctx.widest_spend_data(&amount, &payload)
            });

            let estimate = ctx
                .estimate_tag_bytes(&amount, &payload)
                .expect("spend tag estimation must succeed");
            assert!(
                estimate >= written,
                "{name}: estimate {estimate} is under the {written} bytes the zome could write"
            );
        }
    }

    #[test]
    fn a_spend_that_names_no_lane_is_measured_for_the_lanes_the_zome_resolves() {
        let context = |lane_definitions: Vec<ActionHash>| DepositContext {
            lane: String::new(),
            lane_definitions,
            lane_definition_count: 3,
            credit_limit_adjustment: action_hash(0xC1).into(),
            bridging_agreement: action_hash(0xC2).into(),
        };

        assert_eq!(lane_definitions_written(&context(vec![])).len(), 3);
        assert_eq!(
            lane_definitions_written(&context(vec![action_hash(0xB1)])),
            vec![action_hash(0xB1)]
        );

        // The stand-ins are only a size, so they have to cost what the hashes
        // the zome resolves would cost.
        let total = UnitMap::from(vec![(1_u32, "10")]);
        let proofs = vec![json!({ "tx_hash": "0x01" })];
        let mut resolved = tag_context(Ledger::empty(), &[]);
        resolved.lane_definitions = vec![action_hash(0xB7), action_hash(0xB8), action_hash(0xB9)];
        let mut stood_in = tag_context(Ledger::empty(), &[]);
        stood_in.lane_definitions = lane_definitions_written(&context(vec![]));

        assert_eq!(
            spend_estimate(&stood_in, &total, &proofs),
            spend_estimate(&resolved, &total, &proofs)
        );
    }

    #[test]
    fn a_real_deposit_proof_still_fits_alone_under_the_default_cap() {
        // A cycle where nothing fits makes no progress at all, so one real
        // deposit has to stay well under the cap on the widest network we run:
        // six units held and two charged, as the live global definition stands.
        let ctx = tag_context(
            Ledger::new(
                vec![(0, "1"), (1, "2"), (2, "3"), (3, "4"), (4, "5"), (5, "6")],
                vec![],
                vec![(0, "1")],
                vec![],
            ),
            &[unit_fee(0, "100"), unit_fee(1, "100")],
        );
        let proof = json!({
            "method": "deposit",
            "contract_address": "0x1234567890abcdef1234567890abcdef12345678",
            "amount": "1.5",
            "depositor_wallet_address": "uhCAkYoBhEs3GyOWslej78VfMRmSSdc2TXsRQmqFn5b3v8jl58Kkj",
            "lock_id": format!("0x{:064x}:0", 12345),
            "tx_hash": format!("0x{:064x}", 1),
        });

        let bytes = spend_estimate(&ctx, &UnitMap::from(vec![(1_u32, "1.5")]), &[proof]);
        let room_for_growth = 4 * PER_UNIT_TAG_BYTES;
        assert!(
            bytes + room_for_growth <= crate::config::LINK_TAG_BYTES_DEFAULT,
            "a single deposit proof measures {bytes} bytes, leaving under {room_for_growth} bytes of the default cap for the ledger to grow into"
        );
    }

    #[test]
    fn spend_tag_carries_the_carry_forward_units_the_ledger_holds() {
        let ledger = Ledger::new(vec![], vec![(3, vec!["2", "3"])], vec![], vec![]);
        let total = UnitMap::from(vec![(1_u32, "10")]);
        let proofs = vec![json!({ "tx_hash": "0x01" })];

        assert_eq!(
            spend_tag(ledger.clone(), &total, &[]).carry_forward_units,
            ledger.carry_forward_units
        );
        assert!(
            spend_estimate(&tag_context(ledger, &[]), &total, &proofs)
                > spend_estimate(&tag_context(Ledger::empty(), &[]), &total, &proofs),
            "a carried unit costs tag bytes the estimate has to charge for"
        );
    }

    // One msgpack map entry: a one-character fixstr key and the 21-character
    // fixstr `widest_amount` renders to, each with its one-byte header.
    const PER_UNIT_TAG_BYTES: usize = 2 + 22;

    #[test]
    fn spend_tag_estimate_costs_a_fixed_width_per_charged_unit() {
        let total = UnitMap::from(vec![(1_u32, "10")]);
        let proofs = vec![json!({ "tx_hash": "0x01" })];

        // A charged unit reaches the balance, as the fee is deducted, what is
        // owed, and the fee the tag states. Not the proposed balance, which a
        // spend leaves alone.
        let per_charged_unit = 3 * PER_UNIT_TAG_BYTES;
        let uncharged = spend_estimate(&tag_context(Ledger::empty(), &[]), &total, &proofs);
        let one = spend_estimate(
            &tag_context(Ledger::empty(), &[unit_fee(0, "100")]),
            &total,
            &proofs,
        );
        // Charged apart from the spend's own unit, which every map already
        // names whether it is charged or not.
        let three = spend_estimate(
            &tag_context(
                Ledger::empty(),
                &[
                    unit_fee(0, "100"),
                    unit_fee(2, "250.5"),
                    unit_fee(3, "1000"),
                ],
            ),
            &total,
            &proofs,
        );

        assert_eq!(one - uncharged, per_charged_unit);
        assert_eq!(three - uncharged, 3 * per_charged_unit);
    }

    #[test]
    fn spend_tag_estimate_costs_a_fixed_width_per_unit_the_ledger_holds() {
        let total = UnitMap::from(vec![(1_u32, "10")]);
        let proofs = vec![json!({ "tx_hash": "0x01" })];

        let empty = spend_estimate(&tag_context(Ledger::empty(), &[]), &total, &proofs);
        let held = spend_estimate(
            &tag_context(
                Ledger::new(
                    vec![(4, "1"), (9, "2")],
                    vec![],
                    vec![(6, "1")],
                    vec![(7, "3")],
                ),
                &[],
            ),
            &total,
            &proofs,
        );

        // Two units in the balance, one owed, one proposed: four entries the
        // batch itself never names.
        assert_eq!(held - empty, 4 * PER_UNIT_TAG_BYTES);
    }

    #[test]
    fn spend_batch_packs_to_the_cap_and_defers_the_proof_that_would_cross_it() {
        let orch = test_orchestrator("spend-batch-cap");
        let id_a = enqueue_lock(&orch, "lock:cap:a", "0xc1");
        enqueue_lock(&orch, "lock:cap:b", "0xc2");
        let rows = pending_rows(&orch);
        assert_eq!(rows.len(), 2, "both rows must be pending");

        let fees = [unit_fee(0, "100"), unit_fee(1, "250.5")];
        let ctx = tag_context(Ledger::empty(), &fees);

        let (proof_a, amount_a) = orch.extract_lock_proof(VAULT, &rows[0]).unwrap();
        let (proof_b, amount_b) = orch.extract_lock_proof(VAULT, &rows[1]).unwrap();
        let both = UnitMap::sum_vec(vec![amount_a, amount_b]).unwrap();
        let both_bytes = spend_estimate(&ctx, &both, &[proof_a, proof_b]);

        let exact = orch
            .build_spend_batch(VAULT, &rows, both_bytes, &ctx)
            .unwrap();
        assert_eq!(exact.ids.len(), 2, "a tag of exactly the cap still fits");
        assert!(!exact.capped);
        assert_eq!(exact.tag_bytes, both_bytes);

        let short = orch
            .build_spend_batch(VAULT, &rows, both_bytes - 1, &ctx)
            .unwrap();
        assert_eq!(
            short.ids,
            vec![id_a],
            "one byte over the cap defers the crossing proof, head order preserved"
        );
        assert!(short.capped);

        let wider = orch
            .build_spend_batch(
                VAULT,
                &rows,
                both_bytes,
                &tag_context(
                    Ledger::empty(),
                    &[fees[0].clone(), fees[1].clone(), unit_fee(2, "9999.999999")],
                ),
            )
            .unwrap();
        assert_eq!(
            wider.ids,
            vec![id_a],
            "a network charging one more unit defers a proof at the same cap"
        );
        assert!(wider.capped);

        assert_eq!(
            pending_rows(&orch).len(),
            2,
            "a deferred proof stays pending for the next cycle, it is never failed"
        );
    }

    #[test]
    fn spend_batch_defers_a_writable_proof_the_cap_cannot_take() {
        // The cap crosses the line here, and on a live network the ledger is as
        // likely to. Neither is the deposit's fault, and one failed is never
        // retried.
        let orch = test_orchestrator("spend-batch-no-room");
        let id_a = enqueue_lock(&orch, "lock:room:a", "0xd1");
        let id_b = enqueue_lock(&orch, "lock:room:b", "0xd2");
        let rows = pending_rows(&orch);

        let ctx = tag_context(Ledger::empty(), &[unit_fee(0, "100")]);
        let (proof_a, amount_a) = orch.extract_lock_proof(VAULT, &rows[0]).unwrap();
        let single_bytes = spend_estimate(&ctx, &amount_a, &[proof_a]);

        let batch = orch
            .build_spend_batch(VAULT, &rows, single_bytes - 1, &ctx)
            .unwrap();
        assert!(batch.ids.is_empty(), "no proof fits under the cap");
        assert!(batch.capped, "every row waits for the next cycle");
        assert_eq!(
            pending_rows(&orch).iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![id_a, id_b],
            "a writable deposit is never abandoned over the cap"
        );
        assert!(orch
            .db
            .list_work_items("lock", crate::state::WorkState::Failed, 100)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_network_that_fills_the_tag_defers_instead_of_abandoning() {
        // The lanes and the charged units are the network's, and both grow
        // without any deposit's help. Neither can be what ends one.
        let orch = test_orchestrator("spend-batch-network-full");
        let id_a = enqueue_lock(&orch, "lock:net:a", "0xf1");
        let rows = pending_rows(&orch);

        let fees: Vec<UnitFee> = (0..12).map(|index| unit_fee(index, "100")).collect();
        let mut ctx = tag_context(Ledger::empty(), &fees);
        ctx.lane_definitions = (0..12).map(|seed| action_hash(0xE0 + seed)).collect();

        let (proof_a, amount_a) = orch.extract_lock_proof(VAULT, &rows[0]).unwrap();
        let bytes = spend_estimate(&ctx, &amount_a, &[proof_a]);
        assert!(
            bytes > crate::config::LINK_TAG_BYTES_CEILING,
            "the network has to fill the tag on its own for this to mean anything: {bytes}"
        );

        let batch = orch
            .build_spend_batch(VAULT, &rows, crate::config::LINK_TAG_BYTES_DEFAULT, &ctx)
            .unwrap();
        assert!(batch.ids.is_empty());
        assert!(batch.capped);
        assert!(
            orch.db
                .list_work_items("lock", crate::state::WorkState::Failed, 100)
                .unwrap()
                .is_empty(),
            "a deposit is never abandoned for what the network puts in the tag"
        );
        assert_eq!(
            pending_rows(&orch).iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![id_a]
        );
    }

    #[test]
    fn a_deferred_proof_does_not_hold_back_a_smaller_one_behind_it() {
        let orch = test_orchestrator("spend-batch-defer-tail");
        enqueue_lock(&orch, &format!("lock:{}", "x".repeat(120)), "0xf2");
        let id_small = enqueue_lock(&orch, "lock:s", "0xf3");
        let rows = pending_rows(&orch);

        let ctx = tag_context(Ledger::empty(), &[unit_fee(0, "100")]);
        let (proof_small, amount_small) = orch.extract_lock_proof(VAULT, &rows[1]).unwrap();
        let small_bytes = spend_estimate(&ctx, &amount_small, &[proof_small]);

        let batch = orch
            .build_spend_batch(VAULT, &rows, small_bytes, &ctx)
            .unwrap();
        assert_eq!(
            batch.ids,
            vec![id_small],
            "the smaller proof behind a deferred one is still packed"
        );
        assert!(batch.capped);
        assert!(orch
            .db
            .list_work_items("lock", crate::state::WorkState::Failed, 100)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn spend_batch_abandons_only_a_proof_no_cap_could_write() {
        let orch = test_orchestrator("spend-batch-unwritable");
        let id_oversized = enqueue_lock(&orch, &format!("lock:{}", "x".repeat(900)), "0xd3");
        let id_tail = enqueue_lock(&orch, "lock:tail:b", "0xd4");
        let rows = pending_rows(&orch);

        let ctx = tag_context(Ledger::empty(), &[unit_fee(0, "100")]);
        let (proof_tail, amount_tail) = orch.extract_lock_proof(VAULT, &rows[1]).unwrap();
        let tail_bytes = spend_estimate(&ctx, &amount_tail, &[proof_tail]);

        let batch = orch
            .build_spend_batch(VAULT, &rows, tail_bytes, &ctx)
            .unwrap();
        assert_eq!(
            batch.ids,
            vec![id_tail],
            "the proof behind an unwritable one is still packed"
        );

        let failed = orch
            .db
            .list_work_items("lock", crate::state::WorkState::Failed, 100)
            .unwrap();
        assert_eq!(
            failed.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![id_oversized],
            "only the proof that cannot be written at all is abandoned"
        );
        assert_eq!(failed[0].error_class.as_deref(), Some("permanent"));
        let error = failed[0].last_error.clone().unwrap();
        assert!(
            error.contains(&LINK_TAG_BYTES_CEILING.to_string()),
            "the operator needs the line it crossed: {error}"
        );
    }

    #[test]
    fn cl_batch_defers_at_the_cap_and_when_nothing_fits() {
        let orch = test_orchestrator("cl-batch-cap");
        let id_a = enqueue_lock(&orch, "lock:cl:a", "0xe1");
        enqueue_lock(&orch, "lock:cl:b", "0xe2");
        let rows = pending_rows(&orch);

        let (proof_a, amount_a) = orch.extract_lock_proof(VAULT, &rows[0]).unwrap();
        let (proof_b, amount_b) = orch.extract_lock_proof(VAULT, &rows[1]).unwrap();
        let single_bytes = cl_estimate(&amount_a, std::slice::from_ref(&proof_a));
        let both_bytes = cl_estimate(
            &UnitMap::sum_vec(vec![amount_a, amount_b]).unwrap(),
            &[proof_a, proof_b],
        );

        let exact = orch.build_cl_batch(VAULT, &rows, both_bytes).unwrap();
        assert_eq!(exact.ids.len(), 2, "a tag of exactly the cap still fits");
        assert!(!exact.capped);
        assert_eq!(exact.tag_bytes, both_bytes);

        let short = orch.build_cl_batch(VAULT, &rows, both_bytes - 1).unwrap();
        assert_eq!(
            short.ids,
            vec![id_a],
            "one byte over the cap defers the crossing proof"
        );
        assert!(short.capped);
        assert_eq!(
            pending_rows(&orch).len(),
            2,
            "a deferred proof stays pending for the next cycle, it is never failed"
        );

        let none = orch.build_cl_batch(VAULT, &rows, single_bytes - 1).unwrap();
        assert!(none.ids.is_empty(), "no proof fits under the cap");
        assert!(none.capped, "a writable proof waits for the next cycle");
        assert!(
            orch.db
                .list_work_items("lock", crate::state::WorkState::Failed, 100)
                .unwrap()
                .is_empty(),
            "a writable deposit is never abandoned over the cap"
        );
    }

    #[test]
    fn cl_batch_abandons_only_a_proof_no_cap_could_write() {
        let orch = test_orchestrator("cl-batch-unwritable");
        let id_oversized = enqueue_lock(&orch, &format!("lock:{}", "x".repeat(900)), "0xe3");
        let id_tail = enqueue_lock(&orch, "lock:cl:tail", "0xe4");
        let rows = pending_rows(&orch);

        let (proof_tail, amount_tail) = orch.extract_lock_proof(VAULT, &rows[1]).unwrap();
        let tail_bytes = cl_estimate(&amount_tail, std::slice::from_ref(&proof_tail));

        let batch = orch.build_cl_batch(VAULT, &rows, tail_bytes).unwrap();
        assert_eq!(
            batch.ids,
            vec![id_tail],
            "the proof behind an unwritable one is still packed"
        );
        assert_eq!(batch.tag_bytes, tail_bytes);

        let failed = orch
            .db
            .list_work_items("lock", crate::state::WorkState::Failed, 100)
            .unwrap();
        assert_eq!(
            failed.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![id_oversized]
        );
        assert_eq!(failed[0].error_class.as_deref(), Some("permanent"));
    }

    /// The shape `get_current_global_definition` returns.
    fn global_definition() -> GlobalDefinitionExt {
        let hash = ActionHashB64::from(action_hash(1)).to_string();
        let agent = AgentPubKeyB64::from(AgentPubKey::from_raw_32(vec![2u8; 32])).to_string();
        serde_json::from_value(json!({
            "id": hash,
            "lane_def": {
                "effective_start_date": 0,
                "expiration_date": 9_223_372_036_854_775_807i64,
                "special_agents": {
                    "bridging_agent": { "pub_key": agent, "address_book_data": null }
                },
                "rave_agreements": {
                    "credit_limit_adjustment": hash,
                    "bridging_agreement": hash,
                    "proof_of_service": hash
                }
            },
            "oracles": { "pricing_oracle": null },
            "system_rave_agreements": {
                "compute_credit_limit": hash,
                "compute_transaction_fee": {
                    "agreement": hash,
                    "unit_fees": [{
                        "unit_definition": hash,
                        "unit_index": 1,
                        "spender_pay_percent": { "numerator": 1, "denominator": 100 },
                        "fee_cap": "5",
                        "fee_trigger": "100",
                        "exempt_agents": []
                    }]
                }
            },
            "migration": {
                "closing_notaries": [],
                "closing_threshold": 0,
                "upgrade_targets": [],
                "opening_predecessors": []
            }
        }))
        .expect("a per-unit-fee global definition must decode")
    }

    #[test]
    fn global_definition_decodes_with_per_unit_fees() {
        let global_definition = global_definition();
        let unit_fees = &global_definition
            .system_rave_agreements
            .compute_transaction_fee
            .unit_fees;
        assert_eq!(unit_fees.len(), 1);
        assert_eq!(unit_fees[0].index_key(), "1");
        assert_eq!(
            spend_tag(Ledger::empty(), &UnitMap::new(), unit_fees)
                .fees_owed
                .get_unit_indexes(),
            vec!["1".to_string()]
        );
    }

    // -----------------------------------------------------------------
    // Lane resolution
    // -----------------------------------------------------------------

    const LANE: u8 = 0x70;
    const CURRENT: u8 = 0x71;
    const PENDING: u8 = 0x72;
    const OTHER_LANE: u8 = 0x60;
    const OTHER_CURRENT: u8 = 0x61;
    const THIRD_LANE: u8 = 0x50;
    const THIRD_CURRENT: u8 = 0x51;

    const BRIDGE: u8 = 0xB0;
    const STRANGER: u8 = 0xB1;
    const BASE_UNIT: u32 = 0;
    const HOT: u32 = 1;
    const OTHER_UNIT: u32 = 2;

    fn agent_key(seed: u8) -> AgentPubKeyB64 {
        AgentPubKey::from_raw_32(vec![seed; 32]).into()
    }

    fn service_units(units: &[u32]) -> UnitIndexMap {
        UnitIndexMap(
            units
                .iter()
                .map(|unit| (unit.to_string(), action_hash(0xE0).into()))
                .collect(),
        )
    }

    /// A version of a lane's definition whose agreements are its own, so a
    /// context shows which version it was read from.
    fn lane_version(version: u8, bridging_agent: u8, units: &[u32]) -> LaneDefinition {
        LaneDefinition {
            effective_start_date: Timestamp(0),
            expiration_date: Timestamp(0),
            special_agents: CommonSpecialAgents {
                bridging_agent: AddressBook {
                    pub_key: agent_key(bridging_agent),
                    address_book_data: Value::Null,
                },
                ops_accounts: vec![],
                service_infrastructure_account: None,
                unit_issuers: Default::default(),
            },
            rave_agreements: CommonRAVEAgreements {
                credit_limit_adjustment: Some(action_hash(version ^ 0x10).into()),
                bridging_agreement: Some(action_hash(version ^ 0x20).into()),
                proof_of_service: action_hash(version ^ 0x30).into(),
            },
            additional_special_agents: vec![],
            additional_rave_agreements: vec![],
            service_units: service_units(units),
        }
    }

    /// A version's seed, its bridging agent and its service units.
    type Version = (u8, u8, &'static [u32]);

    #[derive(Default)]
    struct FakeConductor {
        global: Option<GlobalDefinitionExt>,
        lanes: Vec<LaneExt>,
        in_force: HashMap<ActionHash, ActionHash>,
        versions: HashMap<ActionHash, LaneDefinition>,
        parked: RefCell<HashMap<ActionHash, Vec<Transaction>>>,
        parked_on: RefCell<HashMap<ActionHash, ActionHash>>,
        calls: RefCell<Vec<&'static str>>,
        written: Cell<u8>,
        rave_leaves: HashSet<ActionHash>,
        raved: RefCell<HashSet<ActionHash>>,
        unreadable_once_raved: Cell<bool>,
        rave_delay_ms: u64,
        parked_reads: RefCell<Vec<ActionHash>>,
        link_reads: RefCell<Vec<ActionHash>>,
        fails_on: Option<(ActionHash, &'static str)>,
    }

    /// A conductor on which the global definition's lane bridges for the test
    /// config's agent through `CL_EA` and `BR_EA`, with nothing parked on
    /// either.
    fn bridging_conductor() -> FakeConductor {
        let mut global = global_bridged_by(1, &[HOT]);
        let agreements = &mut global.lane_def.rave_agreements;
        agreements.credit_limit_adjustment = Some(action_hash(CL_EA).into());
        agreements.bridging_agreement = Some(action_hash(BR_EA).into());
        FakeConductor {
            global: Some(global),
            ..FakeConductor::default()
        }
        .consumed(action_hash(CL_EA))
        .consumed(action_hash(BR_EA))
    }

    fn proofs_in(payload: &Value) -> Vec<Value> {
        payload["proof_of_deposit"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    fn basic_properties(origin: u8) -> LaneBasicPropertiesExt {
        LaneBasicPropertiesExt {
            id: action_hash(origin).into(),
            name: format!("lane {origin}"),
            abbreviation: String::new(),
            description: String::new(),
            url: String::new(),
            theme: String::new(),
            lane_editors: vec![],
        }
    }

    impl FakeConductor {
        fn parking(mut self, agreement: ActionHash, links: &[Transaction]) -> Self {
            for link in links {
                self.parked_on
                    .get_mut()
                    .insert(link.id.clone().into(), agreement.clone());
            }
            self.parked.get_mut().insert(agreement, links.to_vec());
            self
        }

        fn call(&self, name: &'static str) {
            self.calls.borrow_mut().push(name);
        }

        /// The ActionHash of the next write, which this conductor numbers from
        /// `0xC0`.
        fn write(&self, name: &'static str) -> ActionHash {
            self.call(name);
            let seed = 0xC0 + self.written.get();
            self.written.set(self.written.get() + 1);
            action_hash(seed)
        }

        fn park(&self, agreement: &ActionHash, link: Transaction) -> ActionHashB64 {
            self.parked_on
                .borrow_mut()
                .insert(link.id.clone().into(), agreement.clone());
            let id = link.id.clone();
            self.parked
                .borrow_mut()
                .entry(agreement.clone())
                .or_default()
                .push(link);
            id
        }

        fn consumed(self, agreement: ActionHash) -> Self {
            self.parking(agreement, &[])
        }

        fn check_fails_on(&self, hash: &ActionHash) -> Result<()> {
            match &self.fails_on {
                Some((failing, failure)) if failing == hash => {
                    Err(anyhow::anyhow!(*failure).context(format!("failed to read {hash}")))
                }
                _ => Ok(()),
            }
        }

        fn with_undefined_lane(mut self, origin: u8) -> Self {
            self.lanes.push(LaneExt {
                basic_properties: basic_properties(origin),
                definition: None,
            });
            self
        }

        fn with_lane(mut self, origin: u8, versions: &[Version], in_force: Option<u8>) -> Self {
            let &(newest, agent, units) = versions.last().expect("a lane has a definition");
            self.lanes.push(LaneExt {
                basic_properties: basic_properties(origin),
                definition: Some(LaneDefinitionExt::from(
                    action_hash(origin),
                    action_hash(newest),
                    lane_version(newest, agent, units),
                )),
            });
            if let Some(in_force) = in_force {
                self.in_force
                    .insert(action_hash(newest), action_hash(in_force));
            }
            for &(version, agent, units) in versions {
                self.versions
                    .insert(action_hash(version), lane_version(version, agent, units));
            }
            self
        }

        fn naming_no_adjustment(mut self, version: u8) -> Self {
            let version = action_hash(version);
            let listed = self
                .lanes
                .iter_mut()
                .filter_map(|lane| lane.definition.as_mut())
                .filter(|newest| ActionHash::from(newest.definition_hash.clone()) == version)
                .map(|newest| &mut newest.rave_agreements);
            let read = &mut self
                .versions
                .get_mut(&version)
                .expect("a version the conductor serves")
                .rave_agreements;
            for agreements in listed.chain([read]) {
                agreements.credit_limit_adjustment = None;
            }
            self
        }
    }

    impl ConductorReads for FakeConductor {
        async fn global_definition(&self) -> Result<GlobalDefinitionExt> {
            self.call("global_definition");
            let global = self.global.as_ref().context("no global definition")?;
            Ok(off_the_wire(global))
        }

        async fn all_lanes(&self) -> Result<Vec<LaneExt>> {
            self.call("all_lanes");
            Ok(off_the_wire(&self.lanes))
        }

        async fn version_in_force(&self, newest: ActionHash) -> Result<Option<ActionHash>> {
            self.call("version_in_force");
            Ok(self.in_force.get(&newest).cloned())
        }

        async fn lane_definition(&self, version: ActionHash) -> Result<LaneDefinition> {
            self.call("lane_definition");
            let definition = self
                .versions
                .get(&version)
                .with_context(|| format!("no lane definition {version}"))?;
            lane_definition_of(&off_the_wire(&lane_definition_record(version, definition)))
        }

        async fn parked_links(&self, agreement: &ActionHash) -> Result<Vec<Transaction>> {
            self.call("parked_links");
            self.parked_reads.borrow_mut().push(agreement.clone());
            self.check_fails_on(agreement)?;
            if self.raved.borrow().contains(agreement) && self.unreadable_once_raved.take() {
                anyhow::bail!("Websocket error: Timeout");
            }
            let parked = self.parked.borrow();
            let links = parked
                .get(agreement)
                .with_context(|| format!("no agreement {agreement}"))?;
            Ok(off_the_wire(links))
        }

        async fn agreement_of(&self, link: ActionHash) -> Result<ActionHash> {
            self.call("agreement_of");
            self.link_reads.borrow_mut().push(link.clone());
            self.check_fails_on(&link)?;
            let agreement = self
                .parked_on
                .borrow()
                .get(&link)
                .cloned()
                .with_context(|| format!("no parked link {link}"))?;
            agreement_parked_on(&off_the_wire(&parked_link_record(link, agreement)))
        }

        async fn ledger(&self) -> Result<Ledger> {
            self.call("ledger");
            Ok(Ledger::empty())
        }
    }

    impl ConductorWrites for FakeConductor {
        async fn create_parked_link(&self, input: &CreateParkedLinkInput) -> Result<ActionHashB64> {
            let ParkedLinkType::ParkedData((data, _)) = &input.parked_link_type else {
                anyhow::bail!("the bridge parks only data on the CL EA");
            };
            let seed = self.write("create_parked_link").get_raw_32()[0];
            Ok(self.park(&input.ea_id, parked_tx(seed, &proofs_in(&data.payload))))
        }

        async fn execute_rave(&self, input: &RAVEExecuteInputs) -> Result<ActionHash> {
            let rave = self.write("execute_rave");
            tokio::time::sleep(Duration::from_millis(self.rave_delay_ms)).await;
            let consumed: HashSet<ActionHash> = input
                .links
                .iter()
                .map(|t| t.id.clone().into())
                .filter(|link| !self.rave_leaves.contains(link))
                .collect();
            if let Some(links) = self.parked.borrow_mut().get_mut(&input.ea_id) {
                links.retain(|link| !consumed.contains(&link.id.clone().into()));
            }
            self.raved.borrow_mut().insert(input.ea_id.clone());
            Ok(rave)
        }

        async fn create_parked_spend(
            &self,
            input: &CreateParkedSpendInput,
        ) -> Result<ActionHashB64> {
            let seed = self.write("create_parked_spend").get_raw_32()[0];
            let mut spend = parked_spend_tx(seed, &proofs_in(&input.spender_payload));
            spend.amount = input.amount.clone();
            Ok(self.park(&input.ea_id, spend))
        }
    }

    async fn reconcile(
        orch: &BridgeOrchestrator,
        cl_links: &[Transaction],
        br_links: &[Transaction],
    ) -> ReconcileCounts {
        let conductor = FakeConductor::default()
            .parking(action_hash(CL_EA), cl_links)
            .parking(action_hash(BR_EA), br_links);
        reconcile_in_force(orch, &conductor, CL_EA, BR_EA).await
    }

    async fn reconcile_in_force(
        orch: &BridgeOrchestrator,
        conductor: &FakeConductor,
        credit_limit: u8,
        bridging: u8,
    ) -> ReconcileCounts {
        reconcile_on(orch, conductor, &in_force(credit_limit, bridging)).await
    }

    fn in_force(credit_limit: u8, bridging: u8) -> DepositContext {
        DepositContext {
            lane: "the test lane".to_string(),
            lane_definitions: vec![],
            lane_definition_count: 0,
            credit_limit_adjustment: action_hash(credit_limit).into(),
            bridging_agreement: action_hash(bridging).into(),
        }
    }

    async fn reconcile_on(
        orch: &BridgeOrchestrator,
        conductor: &FakeConductor,
        context: &DepositContext,
    ) -> ReconcileCounts {
        orch.reconcile_pipeline(conductor, &mut LiveLinks::default(), context)
            .await
            .unwrap()
    }

    async fn reconcile_fails(
        orch: &BridgeOrchestrator,
        conductor: &FakeConductor,
    ) -> anyhow::Error {
        orch.reconcile_pipeline(
            conductor,
            &mut LiveLinks::default(),
            &in_force(CL_EA, BR_EA),
        )
        .await
        .expect_err("the reconcile must fail")
    }

    fn forget_agreements(orch: &BridgeOrchestrator, ids: &[i64]) {
        let db = rusqlite::Connection::open(&orch.cfg.db_path).unwrap();
        for id in ids {
            db.execute(
                "UPDATE work_items SET cl_ea_id = NULL, br_ea_id = NULL WHERE id = ?1",
                [id],
            )
            .unwrap();
        }
    }

    fn lock_row(orch: &BridgeOrchestrator, id: i64) -> WorkItem {
        [
            crate::state::WorkState::Queued,
            crate::state::WorkState::Succeeded,
        ]
        .into_iter()
        .flat_map(|state| orch.db.list_work_items("lock", state, 100).unwrap())
        .find(|row| row.id == id)
        .expect("the row is queued or succeeded")
    }

    /// Encoded as a zome returns it, decoded as `ham` reads it.
    fn off_the_wire<T: serde::de::DeserializeOwned>(
        answer: &(impl Serialize + std::fmt::Debug),
    ) -> T {
        rmp_serde::from_slice(&ExternIO::encode(answer).unwrap().0)
            .expect("the orchestrator must decode what the conductor answers")
    }

    fn record(hash: ActionHash, data: ActionData, entry: RecordEntry) -> Record {
        let action = Action {
            header: ActionHeader {
                author: AgentPubKey::from_raw_32(vec![9u8; 32]),
                timestamp: Timestamp(0),
                action_seq: 5,
                prev_action: Some(action_hash(0x01)),
            },
            data,
        };
        Record::new(
            SignedActionHashed::with_presigned(
                ActionHashed::with_pre_hashed(action, hash),
                Signature([0; 64]),
            ),
            entry,
        )
    }

    fn lane_definition_record(version: ActionHash, definition: &LaneDefinition) -> Record {
        record(
            version,
            ActionData::Create(CreateData {
                entry_type: EntryType::AgentPubKey,
                entry_hash: EntryHash::from_raw_32(vec![1u8; 32]),
            }),
            RecordEntry::Present(Entry::try_from(definition).unwrap()),
        )
    }

    fn parked_link_record(link: ActionHash, agreement: ActionHash) -> Record {
        record(
            link,
            ActionData::CreateLink(CreateLinkData {
                base_address: agreement.into(),
                target_address: AgentPubKey::from_raw_32(vec![1u8; 32]).into(),
                zome_index: 0.into(),
                link_type: 0.into(),
                tag: LinkTag::new(vec![]),
            }),
            RecordEntry::NA,
        )
    }

    fn global_bridged_by(agent: u8, units: &[u32]) -> GlobalDefinitionExt {
        let mut global = global_definition();
        global.lane_def.special_agents.bridging_agent.pub_key = agent_key(agent);
        global.lane_def.service_units = service_units(units);
        global
    }

    async fn resolve(
        conductor: &FakeConductor,
        global: &GlobalDefinitionExt,
        unit: u32,
    ) -> Result<DepositContext> {
        let global: GlobalDefinitionExt = off_the_wire(global);
        BridgeOrchestrator::resolve_deposit_context(conductor, &agent_key(BRIDGE), unit, &global)
            .await
    }

    fn assert_on_version(context: &DepositContext, version: u8) {
        let agreements = lane_version(version, BRIDGE, &[]).rave_agreements;
        assert_eq!(context.lane_definitions, vec![action_hash(version)]);
        assert_eq!(
            Some(context.credit_limit_adjustment.clone()),
            agreements.credit_limit_adjustment
        );
        assert_eq!(
            Some(context.bridging_agreement.clone()),
            agreements.bridging_agreement
        );
    }

    fn assert_on_global_lane(context: &DepositContext, global: &GlobalDefinitionExt) {
        assert!(context.lane_definitions.is_empty());
        assert_eq!(
            Some(context.credit_limit_adjustment.clone()),
            global.lane_def.rave_agreements.credit_limit_adjustment
        );
        assert_eq!(
            Some(context.bridging_agreement.clone()),
            global.lane_def.rave_agreements.bridging_agreement
        );
    }

    fn origin_of(lane: u8) -> String {
        ActionHashB64::from(action_hash(lane)).to_string()
    }

    #[tokio::test]
    async fn the_bridge_runs_on_the_one_lane_naming_its_agent_and_unit() {
        let conductor = FakeConductor::default()
            .with_lane(
                OTHER_LANE,
                &[(OTHER_CURRENT, BRIDGE, &[OTHER_UNIT])],
                Some(OTHER_CURRENT),
            )
            .with_lane(
                LANE,
                &[(CURRENT, BRIDGE, &[OTHER_UNIT, HOT])],
                Some(CURRENT),
            )
            .with_lane(
                THIRD_LANE,
                &[(THIRD_CURRENT, STRANGER, &[HOT])],
                Some(THIRD_CURRENT),
            );

        let context = resolve(&conductor, &global_definition(), HOT)
            .await
            .unwrap();

        assert_on_version(&context, CURRENT);
        assert_eq!(context.lane_definition_count, 3);
        assert!(context.lane.contains(&origin_of(LANE)), "{}", context.lane);
    }

    #[tokio::test]
    async fn a_pending_lane_version_leaves_the_bridge_on_the_version_in_force() {
        let conductor = FakeConductor::default().with_lane(
            LANE,
            &[(CURRENT, BRIDGE, &[HOT]), (PENDING, BRIDGE, &[HOT])],
            Some(CURRENT),
        );

        let context = resolve(&conductor, &global_definition(), HOT)
            .await
            .unwrap();

        assert_on_version(&context, CURRENT);
    }

    #[tokio::test]
    async fn past_the_boundary_the_bridge_cites_the_new_version() {
        let conductor = FakeConductor::default().with_lane(
            LANE,
            &[(CURRENT, BRIDGE, &[HOT]), (PENDING, BRIDGE, &[HOT])],
            Some(PENDING),
        );

        let context = resolve(&conductor, &global_definition(), HOT)
            .await
            .unwrap();

        assert_on_version(&context, PENDING);
    }

    #[tokio::test]
    async fn a_lane_is_matched_by_its_version_in_force_not_its_pending_one() {
        let handed_away = FakeConductor::default().with_lane(
            LANE,
            &[(CURRENT, BRIDGE, &[HOT]), (PENDING, STRANGER, &[HOT])],
            Some(CURRENT),
        );
        assert_on_version(
            &resolve(&handed_away, &global_definition(), HOT)
                .await
                .unwrap(),
            CURRENT,
        );

        let handed_over = FakeConductor::default().with_lane(
            LANE,
            &[(CURRENT, BRIDGE, &[OTHER_UNIT]), (PENDING, BRIDGE, &[HOT])],
            Some(CURRENT),
        );
        let err = resolve(&handed_over, &global_definition(), HOT)
            .await
            .expect_err("the DNA refuses HOT credit on a lane until the version listing it begins");
        assert!(
            format!("{err:#}").starts_with("no lane in force"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn a_lane_with_no_version_in_force_is_passed_over() {
        let conductor = FakeConductor::default()
            .with_lane(OTHER_LANE, &[(OTHER_CURRENT, BRIDGE, &[HOT])], None)
            .with_undefined_lane(THIRD_LANE)
            .with_lane(LANE, &[(CURRENT, BRIDGE, &[HOT])], Some(CURRENT));

        let context = resolve(&conductor, &global_definition(), HOT)
            .await
            .unwrap();

        assert_on_version(&context, CURRENT);
        assert_eq!(
            context.lane_definition_count, 2,
            "a spend naming no lane is measured for every lane with a definition, in force or not"
        );
    }

    #[tokio::test]
    async fn a_lane_that_cannot_be_read_fails_the_cycle() {
        let mut conductor = FakeConductor::default()
            .with_lane(
                OTHER_LANE,
                &[(OTHER_CURRENT, BRIDGE, &[HOT])],
                Some(OTHER_CURRENT),
            )
            .with_lane(LANE, &[(CURRENT, BRIDGE, &[HOT])], Some(CURRENT));
        conductor.versions.remove(&action_hash(OTHER_CURRENT));

        resolve(&conductor, &global_definition(), HOT)
            .await
            .expect_err("a lane that cannot be read may be the second match");
    }

    #[test]
    fn a_deposit_is_credited_in_the_hot_unit_index() {
        let mut orch = test_orchestrator("hot-unit-index");
        orch.cfg.hot_unit_index = 3;
        enqueue_lock(&orch, "lock:unit:a", "0xd1");

        let (_, amount) = orch
            .extract_lock_proof(VAULT, &pending_rows(&orch)[0])
            .unwrap();

        assert_eq!(amount.get_unit_indexes(), vec!["3".to_string()]);
    }

    #[tokio::test]
    async fn no_lane_naming_the_agent_and_unit_fails_the_cycle() {
        let conductor = FakeConductor::default()
            .with_lane(
                OTHER_LANE,
                &[(OTHER_CURRENT, BRIDGE, &[OTHER_UNIT])],
                Some(OTHER_CURRENT),
            )
            .with_lane(LANE, &[(CURRENT, STRANGER, &[HOT])], Some(CURRENT));

        for global in [global_definition(), global_bridged_by(BRIDGE, &[])] {
            let err = resolve(&conductor, &global, HOT)
                .await
                .expect_err("a bridge with no lane would report clean cycles and credit nothing");
            let message = format!("{err:#}");
            assert!(message.starts_with("no lane in force"), "{message}");
            assert!(
                message.contains(&agent_key(BRIDGE).to_string()),
                "{message}"
            );
            assert!(message.contains("service unit 1"), "{message}");
        }
    }

    #[tokio::test]
    async fn two_lanes_naming_the_agent_and_unit_fail_the_cycle() {
        let three_lanes = FakeConductor::default()
            .with_lane(
                OTHER_LANE,
                &[(OTHER_CURRENT, BRIDGE, &[HOT])],
                Some(OTHER_CURRENT),
            )
            .with_lane(LANE, &[(CURRENT, BRIDGE, &[HOT])], Some(CURRENT))
            .with_lane(
                THIRD_LANE,
                &[(THIRD_CURRENT, BRIDGE, &[HOT])],
                Some(THIRD_CURRENT),
            );

        let err = resolve(&three_lanes, &global_definition(), HOT)
            .await
            .expect_err("the bridge cannot tell which lane its deposits belong to");
        let message = format!("{err:#}");
        for lane in [OTHER_LANE, LANE, THIRD_LANE] {
            assert!(message.contains(&origin_of(lane)), "{message}");
        }

        let one_lane =
            FakeConductor::default().with_lane(LANE, &[(CURRENT, BRIDGE, &[HOT])], Some(CURRENT));
        let err = resolve(&one_lane, &global_bridged_by(BRIDGE, &[HOT]), HOT)
            .await
            .expect_err("the global lane counts as a lane");
        let message = format!("{err:#}");
        assert!(message.contains("global definition's lane"), "{message}");
        assert!(message.contains(&origin_of(LANE)), "{message}");
    }

    #[tokio::test]
    async fn the_global_lane_carries_the_bridge_when_it_is_the_one() {
        let conductor = FakeConductor::default()
            .with_lane(
                OTHER_LANE,
                &[(OTHER_CURRENT, BRIDGE, &[OTHER_UNIT])],
                Some(OTHER_CURRENT),
            )
            .with_lane(LANE, &[(CURRENT, STRANGER, &[HOT])], Some(CURRENT));
        let global = global_bridged_by(BRIDGE, &[HOT]);

        let context = resolve(&conductor, &global, HOT).await.unwrap();

        assert_on_global_lane(&context, &global);
        assert_eq!(context.lane_definition_count, 2);
    }

    #[tokio::test]
    async fn base_unit_credit_is_raised_on_the_global_lane_alone() {
        let conductor = FakeConductor::default().with_lane(
            LANE,
            &[(CURRENT, BRIDGE, &[BASE_UNIT])],
            Some(CURRENT),
        );

        let global = global_bridged_by(BRIDGE, &[]);
        let context = resolve(&conductor, &global, BASE_UNIT).await.unwrap();
        assert_on_global_lane(&context, &global);

        let err = resolve(&conductor, &global_definition(), BASE_UNIT)
            .await
            .expect_err("a named lane's credit limit adjustment cannot raise base unit credit");
        assert!(
            format!("{err:#}").starts_with("no lane in force"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn the_bridge_refuses_a_lane_of_its_own_naming_no_adjustment() {
        let conductor = FakeConductor::default()
            .with_lane(LANE, &[(CURRENT, BRIDGE, &[HOT])], Some(CURRENT))
            .naming_no_adjustment(CURRENT);

        let err = resolve(&conductor, &global_definition(), HOT)
            .await
            .expect_err("the bridge raises credit through its lane's adjustment");
        let message = format!("{err:#}");
        assert!(message.contains(&origin_of(LANE)), "{message}");
        assert!(
            message.ends_with("sets no credit limit adjustment"),
            "{message}"
        );

        let mut global = global_bridged_by(BRIDGE, &[HOT]);
        global.lane_def.rave_agreements.credit_limit_adjustment = None;
        let err = resolve(&FakeConductor::default(), &global, HOT)
            .await
            .expect_err("the global lane is refused the same way");
        assert_eq!(
            format!("{err:#}"),
            "the global definition's lane sets no credit limit adjustment"
        );
    }

    #[tokio::test]
    async fn lanes_naming_no_adjustment_leave_the_bridge_on_its_lane() {
        let conductor = FakeConductor::default()
            .with_lane(
                THIRD_LANE,
                &[(THIRD_CURRENT, STRANGER, &[HOT])],
                Some(THIRD_CURRENT),
            )
            .naming_no_adjustment(THIRD_CURRENT)
            .with_lane(
                OTHER_LANE,
                &[(OTHER_CURRENT, BRIDGE, &[OTHER_UNIT])],
                Some(OTHER_CURRENT),
            )
            .naming_no_adjustment(OTHER_CURRENT)
            .with_lane(LANE, &[(CURRENT, BRIDGE, &[HOT])], Some(CURRENT));
        let mut global = global_definition();
        global.lane_def.rave_agreements.credit_limit_adjustment = None;

        let context = resolve(&conductor, &global, HOT).await.unwrap();

        assert_on_version(&context, CURRENT);
    }

    #[test]
    fn a_lane_definition_record_decodes_off_the_wire() {
        let definition = lane_version(CURRENT, BRIDGE, &[HOT]);
        let record = lane_definition_record(action_hash(CURRENT), &definition);

        let read = lane_definition_of(&off_the_wire(&record)).unwrap();

        assert_eq!(read.rave_agreements, definition.rave_agreements);
        assert_eq!(read.special_agents, definition.special_agents);
        assert_eq!(read.service_units, definition.service_units);
    }

    #[test]
    fn apply_rave_link_cap_no_cap_returns_input_unchanged() {
        let links = vec![parked_tx(1, &[]), parked_tx(2, &[])];
        let original_ids: Vec<String> = links.iter().map(|t| t.id.to_string()).collect();

        let (unchanged, deferred) = apply_rave_link_cap(links.clone(), None);
        assert_eq!(deferred, 0);
        assert_eq!(
            unchanged
                .iter()
                .map(|t| t.id.to_string())
                .collect::<Vec<_>>(),
            original_ids
        );

        // `Some(0)` normalises to "no cap" (the config layer already warns
        // at startup when `RAVE_MAX_LINKS=0` is seen; this asserts the
        // helper does the sane thing if a zero slips through anyway).
        let (unchanged2, deferred2) = apply_rave_link_cap(links, Some(0));
        assert_eq!(deferred2, 0);
        assert_eq!(
            unchanged2
                .iter()
                .map(|t| t.id.to_string())
                .collect::<Vec<_>>(),
            original_ids
        );
    }

    #[test]
    fn apply_rave_link_cap_cap_ge_len_returns_input_unchanged() {
        let links = vec![parked_tx(1, &[]), parked_tx(2, &[])];
        let original_len = links.len();

        let (out, deferred) = apply_rave_link_cap(links.clone(), Some(original_len));
        assert_eq!(out.len(), original_len);
        assert_eq!(deferred, 0);

        let (out, deferred) = apply_rave_link_cap(links, Some(original_len + 5));
        assert_eq!(out.len(), original_len);
        assert_eq!(deferred, 0);
    }

    #[test]
    fn apply_rave_link_cap_cap_lt_len_truncates_and_reports_deferred() {
        let links = vec![
            parked_tx(1, &[]),
            parked_tx(2, &[]),
            parked_tx(3, &[]),
            parked_tx(4, &[]),
            parked_tx(5, &[]),
        ];

        let (out, deferred) = apply_rave_link_cap(links, Some(2));
        assert_eq!(out.len(), 2);
        assert_eq!(deferred, 3);
    }

    #[test]
    fn apply_rave_link_cap_preserves_head_order() {
        // Mirrors the S4 ordering contract: deposits are pushed first,
        // withdrawals second. A cap below total should keep deposits and
        // drop withdrawals, not re-order anything.
        let deposits = vec![parked_spend_tx(10, &[]), parked_spend_tx(11, &[])];
        let withdrawals = vec![parked_spend_tx(20, &[]), parked_spend_tx(21, &[])];
        let deposit_ids: Vec<String> = deposits.iter().map(|t| t.id.to_string()).collect();

        let mut pooled = deposits.clone();
        pooled.extend(withdrawals.clone());

        let (out, deferred) = apply_rave_link_cap(pooled, Some(2));
        assert_eq!(deferred, 2);
        assert_eq!(out.len(), 2);
        assert_eq!(
            out.iter().map(|t| t.id.to_string()).collect::<Vec<_>>(),
            deposit_ids,
            "cap at 2 over a deposits-first pool must keep the two deposits and drop both withdrawals"
        );
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
    fn links_by_lock_lowercases_mixed_case_hashes() {
        let tx = parked_tx(0x11, &[proof("7", "0xABCDEF0123456789")]);
        let expected_id = tx.id.to_string();
        let map = links_by_lock(&[tx], &bridging_agent());
        assert_eq!(
            map.get(&LockKey::new("7", "0xabcdef0123456789"))
                .map(String::as_str),
            Some(expected_id.as_str()),
            "uppercase tx_hash in the live parked payload must be normalised to lowercase"
        );
    }

    #[test]
    fn links_by_lock_indexes_parked_spend_payloads_too() {
        let tx_spend = parked_spend_tx(0x22, &[proof("8", "0xdeadbeef")]);
        let expected = tx_spend.id.to_string();
        let map = links_by_lock(&[tx_spend], &bridging_agent());
        assert_eq!(
            map.get(&LockKey::new("8", "0xdeadbeef"))
                .map(String::as_str),
            Some(expected.as_str())
        );
    }

    #[test]
    fn links_by_lock_keys_each_proof_by_its_lock_id_as_well_as_its_transaction() {
        let tx = parked_tx(0x23, &[proof("1", "0xab"), proof("2", "0xab")]);
        let map = links_by_lock(std::slice::from_ref(&tx), &bridging_agent());
        assert_eq!(map.len(), 2);
        for lock_id in ["1", "2"] {
            assert_eq!(
                map.get(&LockKey::new(lock_id, "0xab")),
                Some(&tx.id.to_string())
            );
        }
        assert_eq!(map.get(&LockKey::new("3", "0xab")), None);
        assert_eq!(map.get(&LockKey::new("1", "0xcd")), None);
    }

    #[test]
    fn links_by_lock_skips_non_parked_and_missing_hashes() {
        let mut rave_tx = parked_tx(0x33, &[]);
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
        let map = links_by_lock(&[rave_tx], &bridging_agent());
        assert!(
            map.is_empty(),
            "payloads missing proof_of_deposit must not be indexed"
        );
    }

    #[tokio::test]
    async fn reconcile_advances_new_row_when_its_proof_is_in_a_live_cl_link() {
        // S1 recovery: a row at step='new' whose proof is in a live CL
        // parked link means `create_parked_link` silently succeeded on a
        // previous cycle. Advance to cl_link_created with the observed
        // link hash so S2 picks it up.
        let orch = test_orchestrator("reconcile-s1-advance");
        let row_id = enqueue_lock(&orch, "lock:r:1", "0xabc123");
        let live_link = parked_tx(0x10, &[proof("lock:r:1", "0xabc123")]);
        let expected_hash = live_link.id.to_string();

        reconcile(&orch, &[live_link], &[]).await;

        let row = orch
            .db
            .list_pending_by_step("lock", WorkStep::ClLinkCreated, 10)
            .unwrap()
            .into_iter()
            .find(|r| r.id == row_id)
            .expect("row must have advanced to cl_link_created");
        assert_eq!(row.cl_link_hash.as_deref(), Some(expected_hash.as_str()));
        assert_eq!(row.cl_ea_id, Some(ea(CL_EA)));
    }

    #[tokio::test]
    async fn reconcile_matches_a_row_stored_with_a_mixed_case_tx_hash() {
        let orch = test_orchestrator("reconcile-s1-mixed-case");
        enqueue_lock(&orch, "lock:r:case", " 0xABC123");

        let counts = reconcile(
            &orch,
            &[parked_tx(0x12, &[proof("lock:r:case", "0xabc123")])],
            &[],
        )
        .await;

        assert_eq!(counts.s1_advanced, 1);
    }

    #[tokio::test]
    async fn reconcile_leaves_new_row_untouched_when_its_proof_is_absent_from_live_cl() {
        let orch = test_orchestrator("reconcile-s1-noop");
        let row_id = enqueue_lock(&orch, "lock:r:2", "0xabc999");
        let unrelated = parked_tx(0x20, &[proof("lock:r:other", "0xdeadbeef")]);

        reconcile(&orch, &[unrelated], &[]).await;

        let rows = orch
            .db
            .list_pending_by_step("lock", WorkStep::New, 10)
            .unwrap();
        assert!(
            rows.iter().any(|r| r.id == row_id),
            "row must remain at step='new' when no live link matches"
        );
    }

    #[tokio::test]
    async fn reconcile_advances_cl_link_created_when_hash_no_longer_live() {
        // S2 recovery: a stored `cl_link_hash` that is no longer in the
        // live CL set means the CL RAVE consumed it. Advance to
        // cl_rave_executed without needing to see the RAVE's own
        // ActionHash (that's the "emergent idempotency" property).
        let orch = test_orchestrator("reconcile-s2-advance");
        let row_id = enqueue_lock(&orch, "lock:r:3", "0xfeedface");
        let stored_hash = action_hash(0x30).to_string();
        orch.db
            .advance_to_cl_link_created(row_id, &stored_hash, &ea(CL_EA))
            .unwrap();

        reconcile(&orch, &[], &[]).await;

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

    #[tokio::test]
    async fn reconcile_leaves_cl_link_created_untouched_when_hash_still_live() {
        // Positive-stability: when a CL link is still live, the row
        // must stay at cl_link_created so the cycle's S2 step has
        // something to consume. Otherwise we'd double-consume the link
        // on the next RAVE.
        let orch = test_orchestrator("reconcile-s2-noop");
        let row_id = enqueue_lock(&orch, "lock:r:4", "0xfeedface");
        let stored_hash = action_hash(0x40).to_string();
        orch.db
            .advance_to_cl_link_created(row_id, &stored_hash, &ea(CL_EA))
            .unwrap();

        // Build a live CL set that contains our stored hash.
        let live = parked_tx(0x40, &[proof("lock:r:4", "0xfeedface")]);
        assert_eq!(live.id.to_string(), stored_hash);

        reconcile(&orch, &[live], &[]).await;

        let rows = orch
            .db
            .list_pending_by_step("lock", WorkStep::ClLinkCreated, 10)
            .unwrap();
        assert!(
            rows.iter().any(|r| r.id == row_id),
            "row must stay at cl_link_created while its link is still live"
        );
    }

    #[tokio::test]
    async fn reconcile_advances_cl_rave_executed_when_its_proof_is_in_a_live_bridging_spend() {
        // S3 recovery: `create_parked_spend` silently succeeded on a
        // previous cycle; the bridging-EA live set now carries our
        // proof in a parked spend. Advance to br_spend_created.
        let orch = test_orchestrator("reconcile-s3-advance");
        let row_id = enqueue_lock(&orch, "lock:r:5", "0xcafef00d");
        orch.db
            .advance_to_cl_link_created(row_id, &action_hash(0x50).to_string(), &ea(CL_EA))
            .unwrap();
        orch.db.advance_to_cl_rave_executed(row_id, None).unwrap();

        let live_spend = parked_spend_tx(0x51, &[proof("lock:r:5", "0xcafef00d")]);
        let expected = live_spend.id.to_string();

        reconcile(&orch, &[], &[live_spend]).await;

        let row = orch
            .db
            .list_pending_by_step("lock", WorkStep::BrSpendCreated, 10)
            .unwrap()
            .into_iter()
            .find(|r| r.id == row_id)
            .expect("row must have advanced to br_spend_created");
        assert_eq!(row.br_spend_hash.as_deref(), Some(expected.as_str()));
        assert_eq!(row.br_ea_id, Some(ea(BR_EA)));
    }

    #[tokio::test]
    async fn reconcile_advances_br_spend_created_to_succeeded_when_hash_no_longer_live() {
        // S4 recovery / terminal: a stored `br_spend_hash` that has
        // dropped out of the bridging live set means the bridging RAVE
        // consumed the spend. Advance to br_rave_executed + succeeded
        // so the cycle doesn't re-process it.
        let orch = test_orchestrator("reconcile-s4-advance");
        let row_id = enqueue_lock(&orch, "lock:r:6", "0xfacefeed");
        let spend_hash = action_hash(0x60).to_string();
        orch.db
            .advance_to_cl_link_created(row_id, &action_hash(0x61).to_string(), &ea(CL_EA))
            .unwrap();
        orch.db.advance_to_cl_rave_executed(row_id, None).unwrap();
        orch.db
            .advance_to_br_spend_created(row_id, &spend_hash, &ea(BR_EA))
            .unwrap();

        reconcile(&orch, &[], &[]).await;

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
            .build_cl_batch(VAULT, &rows, 16 * 1024)
            .expect("batch construction must succeed for well-formed rows");
        assert_eq!(
            batch.ids.len(),
            3,
            "build_cl_batch must include all three rows in a single batch"
        );
        assert!(batch.ids.contains(&id_a));
        assert!(batch.ids.contains(&id_b));
        assert!(batch.ids.contains(&id_c));

        let shared_hash = "uhCkkSHARED";
        for id in &batch.ids {
            orch.db
                .advance_to_cl_link_created(*id, shared_hash, &ea(CL_EA))
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

    #[tokio::test]
    async fn reconcile_pipeline_returns_counts_with_one_advance_per_step_transition() {
        // Operability contract: reconcile_pipeline must return exactly
        // one increment per advance_to_* call it issues, so operators
        // can look at a single summary line per cycle and know which
        // stages were recovering rows.
        //
        // Seed one row at each of the four step transitions, then
        // construct the live CL / bridging sets so every row has
        // exactly one advancement available.
        let orch = test_orchestrator("reconcile-counts-each-step");

        // S1: step='new', its proof is in a live CL link.
        let id_s1 = enqueue_lock(&orch, "lock:counts:s1", "0xa1");
        let s1_live = parked_tx(0x81, &[proof("lock:counts:s1", "0xa1")]);

        // S2: step='cl_link_created' with hash that is NOT in live CL
        // set (RAVE consumed it).
        let id_s2 = enqueue_lock(&orch, "lock:counts:s2", "0xa2");
        let s2_stored = action_hash(0x82).to_string();
        orch.db
            .advance_to_cl_link_created(id_s2, &s2_stored, &ea(CL_EA))
            .unwrap();

        // S3: step='cl_rave_executed', its proof is in a live bridging
        // parked spend.
        let id_s3 = enqueue_lock(&orch, "lock:counts:s3", "0xa3");
        orch.db
            .advance_to_cl_link_created(id_s3, &action_hash(0x83).to_string(), &ea(CL_EA))
            .unwrap();
        orch.db.advance_to_cl_rave_executed(id_s3, None).unwrap();
        let s3_live_spend = parked_spend_tx(0x84, &[proof("lock:counts:s3", "0xa3")]);

        // S4: step='br_spend_created' with hash that is NOT in live
        // bridging set (bridging RAVE consumed it).
        let id_s4 = enqueue_lock(&orch, "lock:counts:s4", "0xa4");
        orch.db
            .advance_to_cl_link_created(id_s4, &action_hash(0x85).to_string(), &ea(CL_EA))
            .unwrap();
        orch.db.advance_to_cl_rave_executed(id_s4, None).unwrap();
        orch.db
            .advance_to_br_spend_created(id_s4, &action_hash(0x86).to_string(), &ea(BR_EA))
            .unwrap();

        let counts = reconcile(&orch, &[s1_live], &[s3_live_spend]).await;

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

    #[tokio::test]
    async fn reconcile_pipeline_returns_zeroed_counts_when_no_rows_advance() {
        // Negative case: an empty DB plus empty live sets must yield
        // `ReconcileCounts::default()`. This pins down the "quiet
        // cycle" baseline so a future refactor can't silently count
        // phantom advances.
        let orch = test_orchestrator("reconcile-counts-quiet");
        let counts = reconcile(&orch, &[], &[]).await;
        assert_eq!(counts, ReconcileCounts::default());
    }

    #[tokio::test]
    async fn reconcile_is_idempotent_when_run_twice_against_same_live_sets() {
        // The reconciler runs as the first phase of every cycle. A
        // double-run (e.g. a cycle that retries its own prelude)
        // MUST NOT produce a different outcome than a single run.
        let orch = test_orchestrator("reconcile-idempotent");
        let a = enqueue_lock(&orch, "lock:r:a", "0xaaaa");
        let b = enqueue_lock(&orch, "lock:r:b", "0xbbbb");
        let live_a = parked_tx(0x70, &[proof("lock:r:a", "0xaaaa")]);

        reconcile(&orch, std::slice::from_ref(&live_a), &[]).await;
        reconcile(&orch, std::slice::from_ref(&live_a), &[]).await;

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

    fn enqueue_at_cl_rave_executed(orch: &BridgeOrchestrator, item_id: &str, tx_hash: &str) -> i64 {
        let id = enqueue_lock(orch, item_id, tx_hash);
        orch.db
            .advance_to_cl_link_created(id, &action_hash(0x5F).to_string(), &ea(CL_EA))
            .unwrap();
        orch.db.advance_to_cl_rave_executed(id, None).unwrap();
        id
    }

    fn rows_at(orch: &BridgeOrchestrator, step: WorkStep) -> Vec<WorkItem> {
        orch.db.list_pending_by_step("lock", step, 100).unwrap()
    }

    fn s4_consumes(orch: &BridgeOrchestrator, spends: &[Transaction]) -> usize {
        orch.advance_consumed(
            WorkStep::BrSpendCreated,
            &links_by_lock(spends, &bridging_agent()),
            |id| orch.db.advance_to_br_rave_executed(id, Some("s4")),
        )
        .unwrap()
    }

    /// The batch S1 builds when its tag has room for the first proof alone.
    fn s1_batch_of_one(orch: &BridgeOrchestrator) -> ProofBatch {
        let rows = rows_at(orch, WorkStep::New);
        let (proof, amount) = orch.extract_lock_proof(VAULT, &rows[0]).unwrap();
        orch.build_cl_batch(VAULT, &rows, cl_estimate(&amount, &[proof]))
            .unwrap()
    }

    /// The batch S3 builds when its tag has room for the first proof alone.
    fn s3_batch_of_one(orch: &BridgeOrchestrator) -> ProofBatch {
        let rows = rows_at(orch, WorkStep::ClRaveExecuted);
        let ctx = tag_context(Ledger::empty(), &[]);
        let (proof, amount) = orch.extract_lock_proof(VAULT, &rows[0]).unwrap();
        let cap = spend_estimate(&ctx, &amount, &[proof]);
        orch.build_spend_batch(VAULT, &rows, cap, &ctx).unwrap()
    }

    #[tokio::test]
    async fn of_two_locks_in_one_transaction_reconcile_records_only_the_one_s1_wrote() {
        let orch = test_orchestrator("one-tx-two-locks-s1");
        let first = enqueue_lock(&orch, "lock:twice:1", "0x7A");
        let second = enqueue_lock(&orch, "lock:twice:2", "0x7A");
        let batch = s1_batch_of_one(&orch);
        assert_eq!((batch.ids.clone(), batch.capped), (vec![first], true));
        let written = parked_tx(0x90, &batch.proofs);

        let counts = reconcile(&orch, std::slice::from_ref(&written), &[]).await;

        assert_eq!(counts.s1_advanced, 1);
        let recorded = lock_row(&orch, first);
        assert_eq!(recorded.step, WorkStep::ClLinkCreated);
        assert_eq!(recorded.cl_link_hash, Some(written.id.to_string()));
        assert_eq!(lock_row(&orch, second).step, WorkStep::New);
        assert_eq!(
            s1_batch_of_one(&orch).ids,
            vec![second],
            "the next S1 batch writes the second lock"
        );
    }

    #[tokio::test]
    async fn of_two_locks_in_one_transaction_reconcile_records_only_the_one_s3_wrote() {
        let orch = test_orchestrator("one-tx-two-locks-s3");
        let first = enqueue_at_cl_rave_executed(&orch, "lock:twice:1", "0x7B");
        let second = enqueue_at_cl_rave_executed(&orch, "lock:twice:2", "0x7B");
        let batch = s3_batch_of_one(&orch);
        assert_eq!((batch.ids.clone(), batch.capped), (vec![first], true));
        let written = parked_spend_tx(0x91, &batch.proofs);

        let counts = reconcile(&orch, &[], std::slice::from_ref(&written)).await;

        assert_eq!(counts.s3_advanced, 1);
        let recorded = lock_row(&orch, first);
        assert_eq!(recorded.step, WorkStep::BrSpendCreated);
        assert_eq!(recorded.br_spend_hash, Some(written.id.to_string()));
        assert_eq!(lock_row(&orch, second).step, WorkStep::ClRaveExecuted);
        assert_eq!(
            s3_batch_of_one(&orch).ids,
            vec![second],
            "the next S3 batch writes the second lock"
        );
    }

    #[tokio::test]
    async fn of_two_locks_in_one_transaction_each_succeeds_only_once_its_own_spend_is_consumed() {
        let orch = test_orchestrator("one-tx-two-locks-s4");
        let first = enqueue_at_cl_rave_executed(&orch, "lock:twice:1", "0x7c");
        let second = enqueue_at_cl_rave_executed(&orch, "lock:twice:2", "0x7c");
        let first_spend = parked_spend_tx(0x92, &s3_batch_of_one(&orch).proofs);
        orch.record_br_spend(first, &first_spend.id.to_string(), &in_force(CL_EA, BR_EA))
            .unwrap();

        reconcile(&orch, &[], std::slice::from_ref(&first_spend)).await;
        assert_eq!(s4_consumes(&orch, std::slice::from_ref(&first_spend)), 1);

        assert_eq!(
            lock_row(&orch, first).state,
            crate::state::WorkState::Succeeded
        );
        let waiting = lock_row(&orch, second);
        assert_eq!(
            (waiting.state, waiting.step),
            (crate::state::WorkState::Queued, WorkStep::ClRaveExecuted),
            "the second depositor is not credited by the first lock's spend"
        );

        let second_spend = parked_spend_tx(0x93, &s3_batch_of_one(&orch).proofs);
        let counts = reconcile(&orch, &[], std::slice::from_ref(&second_spend)).await;
        assert_eq!(counts.s3_advanced, 1);
        assert_eq!(
            lock_row(&orch, second).br_spend_hash,
            Some(second_spend.id.to_string())
        );
        assert_eq!(s4_consumes(&orch, &[second_spend]), 1);
        assert_eq!(
            lock_row(&orch, second).state,
            crate::state::WorkState::Succeeded
        );
    }

    #[test]
    fn a_rave_advances_only_the_rows_whose_own_proof_it_took() {
        let orch = test_orchestrator("rave-own-proof");
        let link = parked_tx(0x94, &[proof("lock:own:1", "0x7d")]);
        let other_link = parked_tx(0x97, &[proof("lock:own:3", "0x7d")]);
        let carried = enqueue_lock(&orch, "lock:own:1", "0x7d");
        let missing = enqueue_lock(&orch, "lock:own:2", "0x7d");
        let elsewhere = enqueue_lock(&orch, "lock:own:3", "0x7d");
        for id in [carried, missing, elsewhere] {
            orch.db
                .advance_to_cl_link_created(id, &link.id.to_string(), &ea(CL_EA))
                .unwrap();
        }

        let advanced = orch
            .advance_consumed(
                WorkStep::ClLinkCreated,
                &links_by_lock(&[link, other_link], &bridging_agent()),
                |id| orch.db.advance_to_cl_rave_executed(id, Some("s2")),
            )
            .unwrap();

        assert_eq!(advanced, 2);
        assert_eq!(lock_row(&orch, carried).step, WorkStep::ClRaveExecuted);
        assert_eq!(
            lock_row(&orch, elsewhere).step,
            WorkStep::ClRaveExecuted,
            "a row whose own proof the RAVE took in another link was credited"
        );
        assert_eq!(lock_row(&orch, missing).step, WorkStep::ClLinkCreated);

        let spend = parked_spend_tx(0x95, &[proof("lock:own:1", "0x7d")]);
        for id in [carried, missing] {
            orch.db
                .advance_to_br_spend_created(id, &spend.id.to_string(), &ea(BR_EA))
                .unwrap();
        }
        assert_eq!(s4_consumes(&orch, &[spend]), 1);
        assert_eq!(
            lock_row(&orch, carried).state,
            crate::state::WorkState::Succeeded
        );
        assert_eq!(
            lock_row(&orch, missing).state,
            crate::state::WorkState::Queued,
            "a spend that did not carry the row's proof did not credit its depositor"
        );
    }

    #[tokio::test]
    async fn a_proof_copied_into_another_agents_spend_is_not_the_rows_own() {
        let orch = test_orchestrator("foreign-copy");
        let id = enqueue_at_cl_rave_executed(&orch, "lock:copied:1", "0x8a");
        let copy = signed_by_another(parked_spend_tx(0xAA, &[proof("lock:copied:1", "0x8a")]));

        let counts = reconcile(&orch, &[], std::slice::from_ref(&copy)).await;

        assert_eq!(counts.s3_advanced, 0);
        assert_eq!(lock_row(&orch, id).step, WorkStep::ClRaveExecuted);
        orch.db
            .advance_to_br_spend_created(id, &copy.id.to_string(), &ea(BR_EA))
            .unwrap();
        assert_eq!(
            s4_consumes(&orch, &[copy]),
            0,
            "a RAVE taking another agent's spend credits no deposit of ours"
        );
    }

    #[tokio::test]
    async fn the_agents_own_spend_still_matches_with_a_copy_listed_after_it() {
        let orch = test_orchestrator("own-before-copy");
        let id = enqueue_at_cl_rave_executed(&orch, "lock:copied:2", "0x8b");
        let own = parked_spend_tx(0xAB, &[proof("lock:copied:2", "0x8b")]);
        let copy = signed_by_another(parked_spend_tx(0xAC, &[proof("lock:copied:2", "0x8b")]));

        let counts = reconcile(&orch, &[], &[own.clone(), copy]).await;

        assert_eq!(counts.s3_advanced, 1);
        assert_eq!(lock_row(&orch, id).br_spend_hash, Some(own.id.to_string()));
    }

    #[tokio::test]
    async fn another_agents_spend_carrying_a_proof_is_selected_as_a_withdrawal() {
        let signer = CouponSigner::with_key(PrivateKeySigner::random());
        let mut copy = signed_by_another(parked_withdrawal_tx(0xAD));
        if let TransactionDetails::ParkedSpend {
            attached_payload, ..
        } = &mut copy.details
        {
            attached_payload["proof_of_deposit"] = json!([proof("lock:copied:3", "0x8c")]);
        }

        let selection = select_bridging_links(
            Some(&signer),
            &bridging_agent(),
            &[copy.clone()],
            usize::MAX,
            1,
        )
        .await
        .unwrap();

        assert!(selection.deposits.is_empty());
        assert_eq!(ids(&selection.withdrawals), ids(&[copy]));
    }

    #[test]
    fn a_rave_advances_every_row_of_a_batch_its_consumed_link_carried() {
        let orch = test_orchestrator("rave-batch");
        let link = parked_tx(
            0x96,
            &[proof("lock:batch:1", "0x7e"), proof("lock:batch:2", "0x7f")],
        );
        let rows = [
            enqueue_lock(&orch, "lock:batch:1", "0x7e"),
            enqueue_lock(&orch, "lock:batch:2", "0x7f"),
        ];
        for id in rows {
            orch.db
                .advance_to_cl_link_created(id, &link.id.to_string(), &ea(CL_EA))
                .unwrap();
        }

        let advanced = orch
            .advance_consumed(
                WorkStep::ClLinkCreated,
                &links_by_lock(&[link], &bridging_agent()),
                |id| orch.db.advance_to_cl_rave_executed(id, Some("s2")),
            )
            .unwrap();

        assert_eq!(advanced, 2);
        for id in rows {
            assert_eq!(lock_row(&orch, id).step, WorkStep::ClRaveExecuted);
        }
    }

    /// Every call a cycle makes carrying one deposit from `new` to `succeeded`.
    const ONE_DEPOSIT_CYCLE: [&str; 13] = [
        "global_definition",
        "all_lanes",
        "parked_links",
        "parked_links",
        "create_parked_link",
        "parked_links",
        "execute_rave",
        "parked_links",
        "ledger",
        "create_parked_spend",
        "parked_links",
        "execute_rave",
        "parked_links",
    ];

    #[tokio::test]
    async fn one_cycle_carries_a_deposit_through_all_four_stages() {
        let orch = test_orchestrator("cycle-all-stages");
        let id = enqueue_lock(&orch, "lock:cycle:1", "0x80");
        let conductor = bridging_conductor();

        orch.run_bridge_cycle(&conductor).await.unwrap();

        let row = lock_row(&orch, id);
        assert_eq!(
            (row.state, row.step),
            (crate::state::WorkState::Succeeded, WorkStep::BrRaveExecuted)
        );
        assert_eq!(conductor.calls.take(), ONE_DEPOSIT_CYCLE);
        for agreement in [CL_EA, BR_EA] {
            assert!(conductor.parked.borrow()[&action_hash(agreement)].is_empty());
        }
    }

    /// Two deposits at `step`, each recorded on its own live link.
    fn two_parked_deposits(
        orch: &BridgeOrchestrator,
        step: WorkStep,
    ) -> ([i64; 2], [Transaction; 2]) {
        let context = in_force(CL_EA, BR_EA);
        let parked = |seed: u8, lock: &str, tx_hash: &str| match step {
            WorkStep::ClLinkCreated => {
                let id = enqueue_lock(orch, lock, tx_hash);
                let link = parked_tx(seed, &[proof(lock, tx_hash)]);
                orch.record_cl_link(id, &link.id.to_string(), &context)
                    .unwrap();
                (id, link)
            }
            _ => {
                let id = enqueue_at_cl_rave_executed(orch, lock, tx_hash);
                let spend = parked_spend_tx(seed, &[proof(lock, tx_hash)]);
                orch.record_br_spend(id, &spend.id.to_string(), &context)
                    .unwrap();
                (id, spend)
            }
        };
        let (first, first_link) = parked(0xA8, "lock:rave:1", "0x88");
        let (second, second_link) = parked(0xA9, "lock:rave:2", "0x89");
        ([first, second], [first_link, second_link])
    }

    fn leaving(conductor: FakeConductor, links: &[&Transaction]) -> FakeConductor {
        FakeConductor {
            rave_leaves: links.iter().map(|link| link.id.clone().into()).collect(),
            ..conductor
        }
    }

    #[tokio::test]
    async fn s2_advances_only_the_rows_whose_link_its_rave_took() {
        let orch = test_orchestrator("s2-refused-link");
        let ([taken, refused], links) = two_parked_deposits(&orch, WorkStep::ClLinkCreated);
        let conductor = leaving(
            bridging_conductor().parking(action_hash(CL_EA), &links),
            &[&links[1]],
        );

        orch.run_bridge_cycle(&conductor).await.unwrap();

        assert_ne!(lock_row(&orch, taken).step, WorkStep::ClLinkCreated);
        assert_eq!(
            lock_row(&orch, refused).step,
            WorkStep::ClLinkCreated,
            "a link the RAVE left parked raised no credit limit"
        );
        assert_eq!(
            ids(&conductor.parked.borrow()[&action_hash(CL_EA)]),
            ids(&links[1..])
        );
    }

    #[tokio::test]
    async fn s4_advances_only_the_rows_whose_spend_its_rave_took() {
        let orch = test_orchestrator("s4-refused-spend");
        let ([taken, refused], links) = two_parked_deposits(&orch, WorkStep::BrSpendCreated);
        let conductor = leaving(
            bridging_conductor().parking(action_hash(BR_EA), &links),
            &[&links[1]],
        );

        orch.run_bridge_cycle(&conductor).await.unwrap();

        assert_eq!(
            lock_row(&orch, taken).state,
            crate::state::WorkState::Succeeded
        );
        let waiting = lock_row(&orch, refused);
        assert_eq!(
            (waiting.state, waiting.step),
            (crate::state::WorkState::Queued, WorkStep::BrSpendCreated),
            "a spend the RAVE left parked credited no one"
        );
    }

    /// Each RAVE stage, as the step its rows wait at and the agreement it runs on.
    const RAVE_STAGES: [(WorkStep, u8); 2] = [
        (WorkStep::ClLinkCreated, CL_EA),
        (WorkStep::BrSpendCreated, BR_EA),
    ];

    fn taken_at(step: &WorkStep) -> WorkStep {
        match step {
            WorkStep::ClLinkCreated => WorkStep::ClRaveExecuted,
            _ => WorkStep::BrRaveExecuted,
        }
    }

    #[tokio::test]
    async fn a_rave_outcome_splits_the_links_sent_by_whether_they_left() {
        let orch = test_orchestrator("rave-outcome");
        let taken = parked_tx(0xB4, &[proof("lock:outcome:1", "0x91")]);
        let left = parked_tx(0xB5, &[proof("lock:outcome:2", "0x92")]);
        let conductor =
            FakeConductor::default().parking(action_hash(CL_EA), std::slice::from_ref(&left));

        let outcome = orch
            .rave_outcome(
                &conductor,
                "s2",
                &action_hash(CL_EA),
                "rave",
                &[taken.clone(), left.clone()],
            )
            .await
            .unwrap();

        assert_eq!(ids(&outcome.taken), ids(&[taken]));
        assert_eq!(ids(&outcome.left), ids(&[left]));
    }

    #[tokio::test]
    async fn a_rave_that_takes_no_link_advances_no_row() {
        for (step, agreement) in RAVE_STAGES {
            let orch = test_orchestrator("rave-takes-none");
            let (rows, links) = two_parked_deposits(&orch, step.clone());
            let conductor = leaving(
                bridging_conductor().parking(action_hash(agreement), &links),
                &[&links[0], &links[1]],
            );

            orch.run_bridge_cycle(&conductor).await.unwrap();

            for id in rows {
                assert_eq!(lock_row(&orch, id).step, step);
            }
        }
    }

    #[tokio::test]
    async fn after_a_failed_read_of_what_the_rave_took_only_the_next_reconcile_advances() {
        for (step, agreement) in RAVE_STAGES {
            let orch = test_orchestrator("rave-unread");
            let (rows, links) = two_parked_deposits(&orch, step.clone());
            let conductor = FakeConductor {
                unreadable_once_raved: Cell::new(true),
                ..leaving(
                    bridging_conductor().parking(action_hash(agreement), &links),
                    &[&links[1]],
                )
            };

            let e = orch
                .run_bridge_cycle(&conductor)
                .await
                .expect_err("a RAVE whose outcome cannot be read ends the cycle");

            assert_eq!(classify_cycle_failure(&e), CycleFailureAction::Cooldown);
            assert!(format!("{e:#}").contains("the next reconcile advances its rows"));
            for id in rows {
                assert_eq!(lock_row(&orch, id).step, step);
            }
            reconcile_in_force(&orch, &conductor, CL_EA, BR_EA).await;
            assert_eq!(lock_row(&orch, rows[0]).step, taken_at(&step));
            assert_eq!(lock_row(&orch, rows[1]).step, step);
        }
    }

    #[tokio::test]
    async fn a_slow_rave_ends_the_cycle_before_it_reads_what_it_took() {
        for (step, agreement) in RAVE_STAGES {
            let mut orch = test_orchestrator("rave-slow");
            orch.cfg.slow_call_threshold_ms = 5;
            let (rows, links) = two_parked_deposits(&orch, step.clone());
            let conductor = FakeConductor {
                rave_delay_ms: 20,
                ..bridging_conductor().parking(action_hash(agreement), &links)
            };

            orch.run_bridge_cycle(&conductor).await.unwrap();

            assert_eq!(conductor.calls.take().last(), Some(&"execute_rave"));
            for id in rows {
                assert_eq!(lock_row(&orch, id).step, step);
            }
            reconcile_in_force(&orch, &conductor, CL_EA, BR_EA).await;
            for id in rows {
                assert_eq!(lock_row(&orch, id).step, taken_at(&step));
            }
        }
    }

    #[tokio::test]
    async fn a_deposit_parked_before_its_lane_changed_agreements_waits_on_the_old_ones() {
        let orch = test_orchestrator("replaced-agreements");
        let lane = |in_force| {
            FakeConductor::default().with_lane(
                LANE,
                &[(CURRENT, BRIDGE, &[HOT]), (PENDING, BRIDGE, &[HOT])],
                Some(in_force),
            )
        };
        let before = resolve(&lane(CURRENT), &global_definition(), HOT)
            .await
            .unwrap();
        let after = resolve(&lane(PENDING), &global_definition(), HOT)
            .await
            .unwrap();
        assert_ne!(
            before.credit_limit_adjustment,
            after.credit_limit_adjustment
        );
        assert_ne!(before.bridging_agreement, after.bridging_agreement);

        let link = parked_tx(0x31, &[proof("lock:replaced:cl", "0xc1")]);
        let on_link = enqueue_lock(&orch, "lock:replaced:cl", "0xc1");
        orch.record_cl_link(on_link, &link.id.to_string(), &before)
            .unwrap();
        let spend = parked_spend_tx(0x32, &[proof("lock:replaced:br", "0xc2")]);
        let on_spend = enqueue_lock(&orch, "lock:replaced:br", "0xc2");
        orch.record_cl_link(on_spend, &action_hash(0x33).to_string(), &before)
            .unwrap();
        orch.db.advance_to_cl_rave_executed(on_spend, None).unwrap();
        orch.record_br_spend(on_spend, &spend.id.to_string(), &before)
            .unwrap();

        let hash = |agreement: &ActionHashB64| ActionHash::from(agreement.clone());
        let conductor = lane(PENDING)
            .parking(hash(&before.credit_limit_adjustment), &[link])
            .parking(hash(&before.bridging_agreement), &[spend])
            .consumed(hash(&after.credit_limit_adjustment))
            .consumed(hash(&after.bridging_agreement));

        assert_eq!(
            reconcile_on(&orch, &conductor, &after).await,
            ReconcileCounts::default()
        );
        assert_eq!(lock_row(&orch, on_link).step, WorkStep::ClLinkCreated);
        let waiting = lock_row(&orch, on_spend);
        assert_eq!(
            (waiting.state, waiting.step),
            (crate::state::WorkState::Queued, WorkStep::BrSpendCreated),
            "a spend still parked on the replaced bridging agreement never bridged"
        );

        let conductor = conductor
            .consumed(hash(&before.credit_limit_adjustment))
            .consumed(hash(&before.bridging_agreement));
        assert_eq!(
            reconcile_on(&orch, &conductor, &after).await,
            ReconcileCounts {
                s2_advanced: 1,
                s4_advanced: 1,
                ..Default::default()
            }
        );
        assert_eq!(lock_row(&orch, on_link).step, WorkStep::ClRaveExecuted);
        assert_eq!(
            lock_row(&orch, on_spend).state,
            crate::state::WorkState::Succeeded
        );
    }

    #[tokio::test]
    async fn rows_parked_on_the_agreements_in_force_read_only_those() {
        let orch = test_orchestrator("in-force-reads");
        let link = parked_tx(0x34, &[proof("lock:in-force:cl", "0xc4")]);
        let waiting = enqueue_lock(&orch, "lock:in-force:cl", "0xc4");
        orch.db
            .advance_to_cl_link_created(waiting, &link.id.to_string(), &ea(CL_EA))
            .unwrap();
        let done = enqueue_lock(&orch, "lock:in-force:br", "0xc5");
        orch.db
            .advance_to_cl_link_created(done, &action_hash(0x35).to_string(), &ea(CL_EA))
            .unwrap();
        orch.db.advance_to_cl_rave_executed(done, None).unwrap();
        orch.db
            .advance_to_br_spend_created(done, &action_hash(0x36).to_string(), &ea(BR_EA))
            .unwrap();
        let conductor = FakeConductor::default()
            .parking(action_hash(CL_EA), &[link])
            .consumed(action_hash(BR_EA));

        let counts = reconcile_in_force(&orch, &conductor, CL_EA, BR_EA).await;

        assert_eq!(
            counts,
            ReconcileCounts {
                s4_advanced: 1,
                ..Default::default()
            }
        );
        let mut reads = conductor.parked_reads.take();
        reads.sort();
        assert_eq!(reads, vec![action_hash(CL_EA), action_hash(BR_EA)]);
    }

    #[tokio::test]
    async fn a_row_that_names_no_agreement_learns_it_from_its_link() {
        const REPLACED_CL_EA: u8 = 0xE0;
        let orch = test_orchestrator("unnamed-agreement");
        let link = parked_tx(0x37, &[proof("lock:unnamed:cl", "0xc7")]);
        let on_link = enqueue_lock(&orch, "lock:unnamed:cl", "0xc7");
        orch.db
            .advance_to_cl_link_created(on_link, &link.id.to_string(), &ea(REPLACED_CL_EA))
            .unwrap();
        let spend = parked_spend_tx(0x38, &[proof("lock:unnamed:br", "0xc8")]);
        let on_spend = enqueue_lock(&orch, "lock:unnamed:br", "0xc8");
        orch.db
            .advance_to_cl_link_created(on_spend, &action_hash(0x39).to_string(), &ea(CL_EA))
            .unwrap();
        orch.db.advance_to_cl_rave_executed(on_spend, None).unwrap();
        orch.db
            .advance_to_br_spend_created(on_spend, &spend.id.to_string(), &ea(BR_EA))
            .unwrap();
        forget_agreements(&orch, &[on_link, on_spend]);
        let conductor = FakeConductor::default()
            .parking(action_hash(REPLACED_CL_EA), &[link])
            .consumed(action_hash(CL_EA))
            .parking(action_hash(BR_EA), &[spend])
            .consumed(action_hash(BR_EA));

        let counts = reconcile_in_force(&orch, &conductor, CL_EA, BR_EA).await;

        assert_eq!(
            counts,
            ReconcileCounts {
                s4_advanced: 1,
                ..Default::default()
            }
        );
        let waiting = lock_row(&orch, on_link);
        assert_eq!(waiting.step, WorkStep::ClLinkCreated);
        assert_eq!(waiting.cl_ea_id, Some(ea(REPLACED_CL_EA)));
        let done = lock_row(&orch, on_spend);
        assert_eq!(done.state, crate::state::WorkState::Succeeded);
        assert_eq!(done.br_ea_id, Some(ea(BR_EA)));
    }

    #[tokio::test]
    async fn a_row_that_cannot_be_checked_waits_without_holding_up_the_others() {
        const UNREADABLE_EA: u8 = 0xE1;
        let orch = test_orchestrator("unreadable-agreement");
        let batch = action_hash(0x41).to_string();
        let on_unreadable: Vec<i64> = ["0xd1", "0xd2"]
            .into_iter()
            .map(|tx| {
                let id = enqueue_lock(&orch, &format!("lock:unreadable:{tx}"), tx);
                orch.db
                    .advance_to_cl_link_created(id, &batch, &ea(UNREADABLE_EA))
                    .unwrap();
                id
            })
            .collect();
        let at_spend = |tx: &str, spend: u8| {
            let id = enqueue_lock(&orch, &format!("lock:unreadable:{tx}"), tx);
            orch.db
                .advance_to_cl_link_created(id, &action_hash(0x42).to_string(), &ea(CL_EA))
                .unwrap();
            orch.db.advance_to_cl_rave_executed(id, None).unwrap();
            orch.db
                .advance_to_br_spend_created(id, &action_hash(spend).to_string(), &ea(BR_EA))
                .unwrap();
            id
        };
        let unknown_link = at_spend("0xd3", 0x43);
        forget_agreements(&orch, &[unknown_link]);
        let consumed = at_spend("0xd4", 0x44);
        let conductor = FakeConductor::default()
            .consumed(action_hash(CL_EA))
            .consumed(action_hash(BR_EA));

        let counts = reconcile_in_force(&orch, &conductor, CL_EA, BR_EA).await;

        assert_eq!(
            counts,
            ReconcileCounts {
                s4_advanced: 1,
                ..Default::default()
            }
        );
        for id in on_unreadable {
            let row = lock_row(&orch, id);
            assert_eq!(row.step, WorkStep::ClLinkCreated);
            assert_eq!(row.cl_ea_id, Some(ea(UNREADABLE_EA)));
        }
        let waiting = lock_row(&orch, unknown_link);
        assert_eq!(
            (waiting.step, waiting.br_ea_id),
            (WorkStep::BrSpendCreated, None)
        );
        assert_eq!(
            lock_row(&orch, consumed).state,
            crate::state::WorkState::Succeeded
        );
        assert_eq!(
            conductor
                .parked_reads
                .borrow()
                .iter()
                .filter(|read| **read == action_hash(UNREADABLE_EA))
                .count(),
            1,
            "a failed read is not retried for each row that shares it"
        );
    }

    #[tokio::test]
    async fn an_agreement_in_force_that_cannot_be_read_fails_the_cycle() {
        let orch = test_orchestrator("in-force-unreadable");
        let spend = enqueue_lock(&orch, "lock:in-force-unreadable", "0xe1");
        orch.db
            .advance_to_cl_link_created(spend, &action_hash(0x51).to_string(), &ea(CL_EA))
            .unwrap();
        orch.db.advance_to_cl_rave_executed(spend, None).unwrap();
        orch.db
            .advance_to_br_spend_created(spend, &action_hash(0x52).to_string(), &ea(BR_EA))
            .unwrap();

        for readable in [CL_EA, BR_EA] {
            let conductor = FakeConductor::default().consumed(action_hash(readable));
            reconcile_fails(&orch, &conductor).await;
            assert_eq!(lock_row(&orch, spend).step, WorkStep::BrSpendCreated);
        }
    }

    #[tokio::test]
    async fn a_conductor_failure_fails_the_cycle_rather_than_one_row() {
        let orch = test_orchestrator("conductor-failure");
        let (agreement, link) = (action_hash(0xE2), action_hash(0x53));
        let waiting = enqueue_lock(&orch, "lock:conductor-failure", "0xe2");
        orch.db
            .advance_to_cl_link_created(waiting, &link.to_string(), &agreement.to_string())
            .unwrap();
        let behind = enqueue_lock(&orch, "lock:behind-the-failure", "0xe9");
        orch.db
            .advance_to_cl_link_created(behind, &action_hash(0x57).to_string(), &ea(CL_EA))
            .unwrap();

        for failing in [agreement, link] {
            for (failure, action) in [
                ("Websocket closed", CycleFailureAction::Reconnect),
                ("Websocket error: Timeout", CycleFailureAction::Cooldown),
                (
                    "Source chain error: deadline has elapsed",
                    CycleFailureAction::Cooldown,
                ),
            ] {
                let conductor = FakeConductor {
                    fails_on: Some((failing.clone(), failure)),
                    ..FakeConductor::default()
                        .consumed(action_hash(CL_EA))
                        .consumed(action_hash(BR_EA))
                };

                let e = reconcile_fails(&orch, &conductor).await;

                assert_eq!(classify_cycle_failure(&e), action, "{e:#}");
                assert!(format!("{e:#}").contains("lock:conductor-failure"), "{e:#}");
                for id in [waiting, behind] {
                    assert_eq!(lock_row(&orch, id).step, WorkStep::ClLinkCreated);
                }
            }
            forget_agreements(&orch, &[waiting]);
        }
    }

    #[tokio::test]
    async fn rows_sharing_a_link_read_its_agreement_once() {
        const REPLACED_CL_EA: u8 = 0xE3;
        let orch = test_orchestrator("shared-link");
        let known = parked_tx(0x55, &[proof("lock:shared-link:0xe4", "0xe4")]);
        let unknown = action_hash(0x56);
        let batch = |link: &str, txs: &[&str]| -> Vec<i64> {
            txs.iter()
                .map(|tx| {
                    let id = enqueue_lock(&orch, &format!("lock:shared-link:{tx}"), tx);
                    orch.db
                        .advance_to_cl_link_created(id, link, &ea(CL_EA))
                        .unwrap();
                    id
                })
                .collect()
        };
        let on_known = batch(&known.id.to_string(), &["0xe4", "0xe5", "0xe6"]);
        let on_unknown = batch(&unknown.to_string(), &["0xe7", "0xe8"]);
        forget_agreements(&orch, &[on_known.as_slice(), &on_unknown].concat());
        let conductor = FakeConductor::default()
            .parking(action_hash(REPLACED_CL_EA), &[known])
            .consumed(action_hash(CL_EA))
            .consumed(action_hash(BR_EA));

        reconcile_in_force(&orch, &conductor, CL_EA, BR_EA).await;

        let mut reads = conductor.link_reads.take();
        reads.sort();
        assert_eq!(reads, vec![action_hash(0x55), unknown]);
        for id in on_known {
            assert_eq!(lock_row(&orch, id).cl_ea_id, Some(ea(REPLACED_CL_EA)));
        }
        for id in on_unknown {
            assert_eq!(lock_row(&orch, id).cl_ea_id, None);
        }
    }

    #[tokio::test]
    async fn an_agreement_the_state_db_refuses_to_record_fails_the_cycle() {
        let orch = test_orchestrator("refused-record");
        let link = parked_tx(0x54, &[proof("lock:refused-record", "0xe3")]);
        let waiting = enqueue_lock(&orch, "lock:refused-record", "0xe3");
        orch.db
            .advance_to_cl_link_created(waiting, &link.id.to_string(), &ea(CL_EA))
            .unwrap();
        forget_agreements(&orch, &[waiting]);
        rusqlite::Connection::open(&orch.cfg.db_path)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER refuse BEFORE UPDATE ON work_items
                 BEGIN SELECT RAISE(FAIL, 'database or disk is full'); END;",
            )
            .unwrap();
        let conductor = FakeConductor::default()
            .parking(action_hash(CL_EA), &[link])
            .consumed(action_hash(BR_EA));

        let e = reconcile_fails(&orch, &conductor).await;

        assert!(
            format!("{e:#}").contains("database or disk is full"),
            "{e:#}"
        );
        assert_eq!(lock_row(&orch, waiting).cl_ea_id, None);
    }

    #[test]
    fn only_a_link_names_the_agreement_it_was_parked_on() {
        let parked = parked_link_record(action_hash(0x3A), action_hash(CL_EA));
        assert_eq!(
            agreement_parked_on(&off_the_wire(&parked)).unwrap(),
            action_hash(CL_EA)
        );

        let not_a_link =
            lane_definition_record(action_hash(CURRENT), &lane_version(CURRENT, BRIDGE, &[HOT]));
        agreement_parked_on(&off_the_wire(&not_a_link))
            .expect_err("a record that is not a link was parked on nothing");
    }

    // -----------------------------------------------------------------
    // Deadline-elapsed mitigation tests
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn timed_call_propagates_ok_result_with_elapsed_ms() {
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
        // The error path returns no elapsed to assert on.
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

    fn stage_ejections(orch: &BridgeOrchestrator) -> u32 {
        let mut total = None;
        orch.reporter
            .update(|h| total = Some(h.stage_ejections_total));
        total.expect("reporter state was contended")
    }

    #[tokio::test]
    async fn spend_tag_ledger_ejects_a_slow_read_before_the_spend() {
        let mut orch = test_orchestrator("spend-tag-ledger-slow");
        orch.cfg.slow_call_threshold_ms = 5;
        let ledger = orch
            .spend_tag_ledger(async {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                Ok(Ledger::empty())
            })
            .await
            .expect("a slow read ejects the stage rather than failing the cycle");
        assert!(ledger.is_none());
        assert_eq!(stage_ejections(&orch), 1);
    }

    #[tokio::test]
    async fn spend_tag_ledger_returns_a_prompt_read() {
        let mut orch = test_orchestrator("spend-tag-ledger-prompt");
        orch.cfg.slow_call_threshold_ms = 15_000;
        let read = Ledger::new(vec![(1, "5")], vec![], vec![(2, "1")], vec![]);
        let ledger = orch
            .spend_tag_ledger({
                let read = read.clone();
                async move { Ok(read) }
            })
            .await
            .expect("a prompt read is the ledger the tag is sized from");
        assert_eq!(ledger, Some(read));
        assert_eq!(stage_ejections(&orch), 0);
    }

    #[tokio::test]
    async fn spend_tag_ledger_propagates_a_failed_read() {
        let orch = test_orchestrator("spend-tag-ledger-failed");
        let err = orch
            .spend_tag_ledger(async { Err::<Ledger, _>(anyhow::anyhow!("conductor gone")) })
            .await
            .expect_err("a failed read is the cycle's error, not an empty ledger");
        let chain = format!("{err:#}");
        assert!(chain.contains("failed to read the bridging agent's ledger"));
        assert!(chain.contains("conductor gone"));
    }

    #[test]
    fn pressure_cooldown_ms_doubles_up_to_cap_with_defaults() {
        // Defaults used in production (HAM_PRESSURE_COOLDOWN_MS=30000,
        // HAM_PRESSURE_COOLDOWN_MAX_MS=90000). The progression pins
        // down the operator-facing behaviour: two clean doublings,
        // then saturation at the cap.
        let base = 30_000u64;
        let cap = 90_000u64;
        assert_eq!(
            BridgeOrchestrator::pressure_cooldown_ms(base, cap, 1),
            30_000
        );
        assert_eq!(
            BridgeOrchestrator::pressure_cooldown_ms(base, cap, 2),
            60_000
        );
        assert_eq!(
            BridgeOrchestrator::pressure_cooldown_ms(base, cap, 3),
            90_000
        );
        assert_eq!(
            BridgeOrchestrator::pressure_cooldown_ms(base, cap, 4),
            90_000
        );
        assert_eq!(
            BridgeOrchestrator::pressure_cooldown_ms(base, cap, 50),
            90_000
        );
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

    // -----------------------------------------------------------------
    // Cycle-failure disposition tests
    //
    // `classify_cycle_failure` is the single source of truth for the
    // cycle loop's three-way reaction to a failed cycle. These pin each
    // branch — most importantly the terminal `UnclassifiedCooldown`
    // fallback (B111): before it existed, an error matching none of ham's
    // classifiers fell off the end of the chain and retried at full tempo.
    //
    // Errors are fed as their rendered `Display` text, exactly as a `Ham`
    // caller receives them (every `Ham` method wraps upstream with
    // `anyhow!("…: {}", e)`). The strings here are ones the *currently
    // pinned* ham classifies deterministically — `ResponderDropped` (now
    // `is_connection_error` on ham's side) is pinned in ham's own suite,
    // not here, since this crate builds against the pinned `ham` rev.
    // -----------------------------------------------------------------

    #[test]
    fn classify_cycle_failure_routes_connection_errors_to_reconnect() {
        let e = anyhow::anyhow!(
            "Failed to call zome: Websocket error: Websocket closed: No connection"
        );
        assert_eq!(classify_cycle_failure(&e), CycleFailureAction::Reconnect);
    }

    #[test]
    fn classify_cycle_failure_routes_slow_calls_to_cooldown() {
        // Server-side source-chain pressure and a client-side per-request
        // timeout share the one cooldown disposition (socket stays up).
        let pressure =
            anyhow::anyhow!("Failed to call zome: Source chain error: deadline has elapsed");
        assert_eq!(
            classify_cycle_failure(&pressure),
            CycleFailureAction::Cooldown
        );
        let timeout = anyhow::anyhow!("Failed to call zome: Websocket error: Timeout");
        assert_eq!(
            classify_cycle_failure(&timeout),
            CycleFailureAction::Cooldown
        );
    }

    #[test]
    fn classify_cycle_failure_routes_unclassified_errors_to_the_terminal_fallback() {
        // The B111 fix: an error matching none of ham's classifiers must
        // route to the cooldown fallback instead of a full-tempo retry. A
        // zome/guest logic error is the common live case; a deserialize
        // failure another; plus a wholly unexpected string.
        for msg in [
            "Failed to call zome: guest error: validation failed",
            "Failed to deserialize response: invalid type",
            "some entirely unexpected failure",
        ] {
            let e = anyhow::anyhow!("{msg}");
            assert_eq!(
                classify_cycle_failure(&e),
                CycleFailureAction::UnclassifiedCooldown,
                "unclassified error {msg:?} must route to the terminal cooldown fallback"
            );
        }
    }

    // -----------------------------------------------------------------
    // Shared cooldown-helper tests
    //
    // Both cooldown branches route through `pressure_backoff` and
    // `sleep_or_shutdown`, so these pin the two seams the branches no
    // longer spell out inline.
    // -----------------------------------------------------------------

    #[test]
    fn pressure_backoff_pairs_each_cooldown_with_its_severity() {
        // The helper must stay exactly the composition of the two functions
        // it wraps — both cooldown branches now depend on that pairing.
        let orch = test_orchestrator("pressure-backoff");
        let base = orch.cfg.ham_pressure_cooldown_ms;
        let cap = orch.cfg.ham_pressure_cooldown_max_ms;
        for attempt in 0..8 {
            let expected_cooldown = BridgeOrchestrator::pressure_cooldown_ms(base, cap, attempt);
            assert_eq!(
                orch.pressure_backoff(attempt),
                (
                    expected_cooldown,
                    BridgeOrchestrator::pressure_severity(attempt, expected_cooldown, cap)
                ),
                "attempt {attempt} must match the underlying pair"
            );
        }

        // Pin the concrete curve the defaults produce (30s → 60s → 90s →
        // capped), so a change to either half shows up here rather than only
        // at the two call sites.
        assert_eq!(orch.pressure_backoff(1), (30_000, PressureSeverity::Warn));
        assert_eq!(orch.pressure_backoff(2), (60_000, PressureSeverity::Warn));
        assert_eq!(orch.pressure_backoff(3), (90_000, PressureSeverity::Warn));
        assert_eq!(orch.pressure_backoff(4), (90_000, PressureSeverity::Stuck));
    }

    #[tokio::test]
    async fn sleep_or_shutdown_wakes_immediately_on_shutdown() {
        // The wait stays interruptible: a shutdown arriving mid-cooldown
        // returns at once instead of sitting out the full backoff.
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), sleep_or_shutdown(60_000, &mut rx))
            .await
            .expect("shutdown must cut the cooldown short");
    }

    #[tokio::test]
    async fn sleep_or_shutdown_waits_out_the_duration_without_shutdown() {
        // `_tx` stays bound so the channel is not closed under the receiver.
        let (_tx, mut rx) = tokio::sync::watch::channel(false);
        let started = std::time::Instant::now();
        sleep_or_shutdown(50, &mut rx).await;
        assert!(
            started.elapsed() >= Duration::from_millis(50),
            "cooldown must not return before its duration elapses"
        );
    }

    // -----------------------------------------------------------------
    // execute_rave wire contract
    //
    // The guest reports a field this payload omits as
    // `WasmErrorInner::Deserialize(<bytes>)`, with serde's reason
    // discarded, so the drift is named here instead.
    // -----------------------------------------------------------------

    /// Encoded the way `Ham::call_zome` puts it on the wire.
    fn encoded_execute_rave_payload() -> Vec<u8> {
        let payload = RAVEExecuteInputs {
            ea_id: action_hash(0xEA),
            executor_inputs: Value::Null,
            links: vec![parked_tx(1, &[])],
            global_definition: action_hash(0xAA),
            lane_definitions: vec![action_hash(0xAB)],
            strategy: GetStrategy::Local,
        };
        ExternIO::encode(&payload)
            .expect("the S2 payload must encode")
            .0
    }

    fn keys(map: &BTreeMap<String, IgnoredAny>) -> BTreeSet<&str> {
        map.keys().map(String::as_str).collect()
    }

    #[test]
    fn the_payload_names_every_input_field_the_dna_decodes() {
        let decoded: BTreeMap<String, IgnoredAny> =
            rmp_serde::from_slice(&encoded_execute_rave_payload()).unwrap();
        assert_eq!(
            keys(&decoded),
            BTreeSet::from([
                "ea_id",
                "executor_inputs",
                "global_definition",
                "lane_definitions",
                "links",
                "strategy",
            ])
        );
    }

    #[test]
    fn every_link_names_every_transaction_field_the_dna_decodes() {
        #[derive(Deserialize)]
        struct Payload {
            links: Vec<BTreeMap<String, IgnoredAny>>,
        }
        let decoded: Payload = rmp_serde::from_slice(&encoded_execute_rave_payload()).unwrap();
        assert_eq!(
            keys(&decoded.links[0]),
            BTreeSet::from([
                "amount",
                "counterparty",
                "creator",
                "details",
                "fee",
                "history",
                "id",
                "timestamp",
                "tx_type",
            ])
        );
    }

    #[test]
    fn a_parked_link_names_every_detail_field_the_dna_decodes() {
        #[derive(Deserialize)]
        struct Payload {
            links: Vec<Link>,
        }
        #[derive(Deserialize)]
        struct Link {
            details: BTreeMap<String, BTreeMap<String, IgnoredAny>>,
        }
        let decoded: Payload = rmp_serde::from_slice(&encoded_execute_rave_payload()).unwrap();
        let parked = decoded.links[0]
            .details
            .get("Parked")
            .expect("a parked link is tagged `Parked`");
        assert_eq!(
            keys(parked),
            BTreeSet::from([
                "attached_payload",
                "consumed_link",
                "ct_role_id",
                "ea_id",
                "executor",
                "role_display_name",
                "smart_agreement_title",
            ])
        );
    }

    fn parked_withdrawal_tx(seed: u8) -> Transaction {
        let mut tx = parked_spend_tx(seed, &[]);
        tx.amount = UnitMap::from(vec![(1, "5")]);
        if let TransactionDetails::ParkedSpend {
            attached_payload, ..
        } = &mut tx.details
        {
            *attached_payload =
                json!({ "withdraw_to_address": format!("{:#x}", Address::repeat_byte(seed)) });
        }
        tx
    }

    fn ids(links: &[Transaction]) -> Vec<String> {
        links.iter().map(|tx| tx.id.to_string()).collect()
    }

    #[tokio::test]
    async fn without_a_signer_deposits_go_and_every_withdrawal_stays_parked() {
        let deposit = parked_spend_tx(0x60, &[proof("lock:d", "0xd0")]);
        let links = [
            parked_withdrawal_tx(0x61),
            deposit.clone(),
            parked_withdrawal_tx(0x62),
        ];

        let selection = select_bridging_links(None, &bridging_agent(), &links, usize::MAX, 1)
            .await
            .unwrap();

        assert_eq!(ids(&selection.deposits), ids(&[deposit]));
        assert!(selection.withdrawals.is_empty());
        assert!(selection.coupons.is_empty());
        assert_eq!(selection.withdrawals_found, 2);
    }

    #[tokio::test]
    async fn with_a_signer_each_withdrawal_goes_with_its_coupon() {
        let signer = CouponSigner::with_key(PrivateKeySigner::random());
        let deposit = parked_spend_tx(0x60, &[proof("lock:d", "0xd0")]);
        let withdrawals = [parked_withdrawal_tx(0x61), parked_withdrawal_tx(0x62)];
        let links = [
            withdrawals[0].clone(),
            deposit.clone(),
            withdrawals[1].clone(),
        ];

        let selection =
            select_bridging_links(Some(&signer), &bridging_agent(), &links, usize::MAX, 1)
                .await
                .unwrap();

        assert_eq!(ids(&selection.deposits), ids(&[deposit]));
        assert_eq!(ids(&selection.withdrawals), ids(&withdrawals));
        assert_eq!(
            selection.coupons.keys().cloned().collect::<BTreeSet<_>>(),
            ids(&withdrawals).into_iter().collect()
        );
        assert_eq!(selection.withdrawals_found, 2);
    }

    #[tokio::test]
    async fn the_coupons_budget_holds_back_withdrawals_past_the_first() {
        let signer = CouponSigner::with_key(PrivateKeySigner::random());
        let withdrawals = [parked_withdrawal_tx(0x61), parked_withdrawal_tx(0x62)];

        let selection = select_bridging_links(Some(&signer), &bridging_agent(), &withdrawals, 1, 1)
            .await
            .unwrap();

        assert_eq!(ids(&selection.withdrawals), ids(&withdrawals[..1]));
        assert_eq!(selection.coupons.len(), 1);
        assert_eq!(selection.withdrawals_found, 2);
    }

    #[tokio::test]
    async fn a_withdrawal_is_paid_from_the_hot_unit_index() {
        let signer = CouponSigner::with_key(PrivateKeySigner::random());
        let mut withdrawal = parked_withdrawal_tx(0x61);
        withdrawal.amount = UnitMap::from(vec![(1, "7"), (3, "5")]);

        let selection = select_bridging_links(
            Some(&signer),
            &bridging_agent(),
            &[withdrawal.clone()],
            usize::MAX,
            3,
        )
        .await
        .unwrap();

        let coupon = selection.coupons[&withdrawal.id.to_string()]
            .as_str()
            .unwrap();
        let amount = coupon.split(',').nth(3);
        assert_eq!(amount, Some("5000000000000000000"), "{coupon}");
    }

    #[tokio::test]
    async fn a_withdrawal_no_coupon_can_pay_stays_parked_and_the_rest_go() {
        let signer = CouponSigner::with_key(PrivateKeySigner::random());
        let paid_to = |seed, recipient: &str, amount: &[(u32, &str)]| {
            let mut tx = parked_withdrawal_tx(seed);
            tx.amount = UnitMap::from(amount.to_vec());
            if let TransactionDetails::ParkedSpend {
                attached_payload, ..
            } = &mut tx.details
            {
                *attached_payload = json!({ "withdraw_to_address": recipient });
            }
            tx
        };
        let to = "0x1111111111111111111111111111111111111111";
        let payable = paid_to(0x60, to, &[(1, "5")]);
        let deposit = parked_spend_tx(0x61, &[proof("lock:d", "0xd0")]);
        let links = [
            paid_to(0x62, to, &[(2, "5")]),
            paid_to(0x63, to, &[(1, "0")]),
            paid_to(0x64, to, &[(1, "-5")]),
            paid_to(0x65, "0xdead", &[(1, "5")]),
            payable.clone(),
            deposit.clone(),
        ];

        let selection =
            select_bridging_links(Some(&signer), &bridging_agent(), &links, usize::MAX, 1)
                .await
                .unwrap();

        assert_eq!(ids(&selection.withdrawals), ids(&[payable]));
        assert_eq!(ids(&selection.deposits), ids(&[deposit]));
        assert_eq!(selection.coupons.len(), 1);
        assert_eq!(selection.withdrawals_found, 5);
    }

    #[tokio::test]
    async fn with_ethereum_off_reconcile_still_records_what_is_already_parked() {
        let mut orch = test_orchestrator("ethereum-off-reconcile");
        orch.ethereum = None;
        let new = enqueue_lock(&orch, "lock:off:1", "0xa1");
        let link_created = enqueue_lock(&orch, "lock:off:2", "0xa2");
        orch.db
            .advance_to_cl_link_created(link_created, &action_hash(0x70).to_string(), &ea(CL_EA))
            .unwrap();
        let rave_executed = enqueue_lock(&orch, "lock:off:3", "0xa3");
        orch.db
            .advance_to_cl_link_created(rave_executed, &action_hash(0x71).to_string(), &ea(CL_EA))
            .unwrap();
        orch.db
            .advance_to_cl_rave_executed(rave_executed, None)
            .unwrap();
        let spend_created = enqueue_lock(&orch, "lock:off:4", "0xa4");
        orch.db
            .advance_to_cl_link_created(spend_created, &action_hash(0x72).to_string(), &ea(CL_EA))
            .unwrap();
        orch.db
            .advance_to_cl_rave_executed(spend_created, None)
            .unwrap();
        orch.db
            .advance_to_br_spend_created(spend_created, &action_hash(0x73).to_string(), &ea(BR_EA))
            .unwrap();

        let counts = reconcile(
            &orch,
            &[parked_tx(0x74, &[proof("lock:off:1", "0xa1")])],
            &[parked_spend_tx(0x75, &[proof("lock:off:3", "0xa3")])],
        )
        .await;

        assert_eq!(
            counts,
            ReconcileCounts {
                s1_advanced: 1,
                s2_advanced: 1,
                s3_advanced: 1,
                s4_advanced: 1,
            }
        );
        let at = |step| {
            orch.db
                .list_pending_by_step("lock", step, 10)
                .unwrap()
                .into_iter()
                .map(|row| row.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(at(WorkStep::ClLinkCreated), vec![new]);
        assert_eq!(at(WorkStep::ClRaveExecuted), vec![link_created]);
        assert_eq!(at(WorkStep::BrSpendCreated), vec![rave_executed]);
        assert!(at(WorkStep::New).is_empty());
    }
}
