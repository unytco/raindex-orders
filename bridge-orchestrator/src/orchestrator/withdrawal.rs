use super::{
    action_hash_from, parked_on, timed_call, BridgeOrchestrator, ConductorReads, ConductorWrites,
    LiveLinks, RaveOutcome, Receipt, ReconcileCounts,
};
use crate::state::{WithdrawalPayload, WorkItem, WorkState, WorkStep, WITHDRAWAL_FLOW};
use anyhow::Result;
use ham::ShutdownRx;
use holo_hash::ActionHash;
use rave_engine::types::{ReceiptInput, Transaction};
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use tracing::{debug, error, info, warn};

/// The conductor answered, and refused. Holochain renders its validation's
/// rejection of a write as `InvalidCommit`, which `ham` classes as source chain
/// pressure, and an error the zome returned as `Guest`.
pub(super) fn refused(e: &anyhow::Error) -> bool {
    let text = format!("{e:#}");
    text.contains("InvalidCommit error") || text.contains("Guest(")
}

/// The transactor zome's refusal of an agent with a proven chain fork, which
/// holds for good.
const CHAIN_FORK_REFUSAL: &str = "carries a ChainFork warrant";

const SPEND_GONE: &str = "spend_gone";

impl BridgeOrchestrator {
    /// `links` without each withdrawal whose row is not pending at `sent`: a
    /// coupon for it would pay HOT that no row takes off the credit limit.
    pub(super) fn without_settled_withdrawals(
        &self,
        links: &[Transaction],
    ) -> Result<Vec<Transaction>> {
        let mut sendable = Vec::new();
        let mut withheld = Vec::new();
        for link in links {
            match self.db.withdrawal(&link.id.to_string())? {
                Some(row)
                    if row.step != WorkStep::Sent
                        || !matches!(row.state, WorkState::Queued | WorkState::InFlight) =>
                {
                    withheld.push((link, row))
                }
                _ => sendable.push(link.clone()),
            }
        }
        for (tx, row) in self.untold("withdrawal_withheld", &withheld, |(tx, _)| {
            tx.id.to_string()
        }) {
            error!(
                event = "bridge.withdrawal_withheld",
                link = %tx.id,
                creator = %tx.creator,
                "[bridge/withdrawals] withdrawal {} stays parked with no coupon, as its row is {} at {}",
                tx.id,
                row.state,
                row.step
            );
        }
        Ok(sendable)
    }

    /// A withdrawal the RAVE left stays at `sent`: the DNA drops a handed link
    /// no longer parked, so an earlier S4 whose reply was lost may have taken
    /// it, and only the chain can say.
    pub(super) fn record_paid(
        &self,
        outcome: &RaveOutcome,
        withdrawals: &HashSet<String>,
        rave: &str,
    ) -> Result<usize> {
        let mut paid = 0;
        for spend in outcome.taken.iter().map(|t| t.id.to_string()) {
            if !withdrawals.contains(&spend) {
                continue;
            }
            let moved = match self.db.withdrawal(&spend)? {
                Some(row) => self.db.advance_to_paid(row.id, rave)?,
                None => false,
            };
            if moved {
                paid += 1;
            } else {
                error!(
                    event = "bridge.withdrawal.unrecorded",
                    spend,
                    "[bridge/s4] RAVE {rave} took withdrawal {spend}, which has no row waiting at sent"
                );
            }
        }
        Ok(paid)
    }

    /// S5. `fresh` is this cycle's S4 RAVE, which no receipt can name yet. Any
    /// other RAVE is collected only once this cycle's chain read found it due,
    /// as reconcile acts on every other outcome.
    pub(super) async fn collect_withdrawals(
        &self,
        conductor: &(impl ConductorReads + ConductorWrites),
        live: &mut LiveLinks,
        fresh: Option<&ActionHash>,
        stop: &ShutdownRx,
    ) -> Result<usize> {
        let mut collected = 0;
        for (rave, rows) in self.paid_by_rave()? {
            if Some(&rave) != fresh {
                match live.receipt(conductor, &rave).await {
                    Ok(Receipt::Due) => {}
                    Ok(Receipt::Written(_) | Receipt::NoRave) | Err(_) => continue,
                }
            }
            for row in &rows {
                self.db.mark_in_flight(row.id)?;
            }
            let started = std::time::Instant::now();
            let written = timed_call(
                "s5",
                "create_receipt_from_rave",
                conductor.create_receipt_from_rave(&ReceiptInput {
                    hash: rave.clone().into(),
                    note: None,
                }),
            )
            .await;
            let (receipt, elapsed_ms) = match written {
                Ok(written) => written,
                Err(e) if refused(&e) => {
                    self.receipt_refused(&rave, &rows, &e)?;
                    let elapsed_ms = started.elapsed().as_millis();
                    if self.ends_cycle("s5", "create_receipt_from_rave", elapsed_ms, stop)? {
                        break;
                    }
                    continue;
                }
                Err(e) => {
                    return Err(e.context(format!(
                        "S5 did not receipt RAVE {rave}, which took {}",
                        item_ids(&rows)
                    )))
                }
            };
            info!(
                "[bridge/s5] create_receipt_from_rave: RAVE {} paid {} withdrawal(s), receipt={}",
                rave,
                rows.len(),
                receipt
            );
            for row in &rows {
                if self.db.advance_to_collected(row.id, &receipt.to_string())? {
                    collected += 1;
                }
            }
            if self.ends_cycle("s5", "create_receipt_from_rave", elapsed_ms, stop)? {
                break;
            }
        }
        Ok(collected)
    }

    fn receipt_refused(
        &self,
        rave: &ActionHash,
        rows: &[WorkItem],
        e: &anyhow::Error,
    ) -> Result<()> {
        let why = format!("{e:#}");
        if why.contains(CHAIN_FORK_REFUSAL) {
            error!(
                event = "bridge.receipt.forked",
                rave = %rave,
                "[bridge/s5] RAVE {rave} is never receipted, so the HOT of {} stays on the credit limit and the debt: {why}",
                item_ids(rows)
            );
            for row in rows {
                self.db.mark_failed_permanent(
                    row.id,
                    &format!("its RAVE {rave} is never receipted ({why}): its HOT stays on the credit limit and the debt, and is not taken off by hand"),
                )?;
            }
        } else {
            error!(
                event = "bridge.receipt.refused",
                rave = %rave,
                "[bridge/s5] RAVE {rave}, which took {}, waits for the next cycle, as the conductor refused its receipt: {why}",
                item_ids(rows)
            );
            let ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
            self.db.requeue_in_flight(&ids, &why)?;
        }
        Ok(())
    }

    /// A row whose RAVE does not read goes to a person.
    fn paid_by_rave(&self) -> Result<BTreeMap<ActionHash, Vec<WorkItem>>> {
        let mut by_rave: BTreeMap<ActionHash, Vec<WorkItem>> = BTreeMap::new();
        for row in self
            .db
            .list_pending_by_step(WITHDRAWAL_FLOW, WorkStep::Paid, 5000)?
        {
            match action_hash_from(row.br_rave_hash.as_deref().unwrap_or_default()) {
                Ok(rave) => by_rave.entry(rave).or_default().push(row),
                Err(e) => self.fail_for_a_person(
                    &row,
                    "bridge.reconcile.rave_unreadable",
                    &format!("its RAVE does not read: {e:#}"),
                )?,
            }
        }
        Ok(by_rave)
    }

    pub(super) async fn reconcile_withdrawals(
        &self,
        conductor: &impl ConductorReads,
        live: &mut LiveLinks,
        bridging: &ActionHash,
        counts: &mut ReconcileCounts,
    ) -> Result<()> {
        let gone_before = self
            .deferred
            .lock()
            .expect("deferred mutex poisoned")
            .remove(SPEND_GONE)
            .unwrap_or_default();
        let mut gone = HashSet::new();
        for row in self
            .db
            .list_pending_by_step(WITHDRAWAL_FLOW, WorkStep::Sent, 5000)?
        {
            if let Some(rave) = self
                .withdrawal_taken(conductor, live, &row, bridging, &gone_before, &mut gone)
                .await?
            {
                if self.db.advance_to_paid(row.id, &rave.to_string())? {
                    counts.s4_advanced += 1;
                }
            }
        }
        self.deferred
            .lock()
            .expect("deferred mutex poisoned")
            .insert(SPEND_GONE, gone);

        for (rave, rows) in self.paid_by_rave()? {
            let receipt = match live.receipt(conductor, &rave).await {
                Ok(Receipt::Written(receipt)) => receipt,
                Ok(Receipt::Due) => continue,
                Ok(Receipt::NoRave) => {
                    for row in &rows {
                        self.fail_for_a_person(
                            row,
                            "bridge.reconcile.rave_not_held",
                            &format!(
                                "its RAVE {rave} is not in the bridging agent's chain history"
                            ),
                        )?;
                    }
                    continue;
                }
                Err(e) => {
                    let what = format!(
                        "the receipt of its RAVE {rave}, which took {}",
                        item_ids(&rows)
                    );
                    Self::unresolved(&rows[0], &what, e)?;
                    continue;
                }
            };
            for row in &rows {
                if self.db.advance_to_collected(row.id, &receipt.to_string())? {
                    debug!(
                        "[bridge/reconcile] {} paid → collected (receipt {} of RAVE {})",
                        row.item_id, receipt, rave
                    );
                    counts.s5_advanced += 1;
                }
            }
        }
        Ok(())
    }

    /// The RAVE that took a withdrawal row's spend. `None` while the spend
    /// stays parked or cannot be checked, and when the row's payload does not
    /// read or no RAVE of the bridging agent's records the spend consumed. The
    /// row goes to a person on an unreadable payload, or when its spend was
    /// gone untaken in `gone_before` too, as one listing can miss a parked spend.
    async fn withdrawal_taken(
        &self,
        conductor: &impl ConductorReads,
        live: &mut LiveLinks,
        row: &WorkItem,
        in_force: &ActionHash,
        gone_before: &HashSet<String>,
        gone: &mut HashSet<String>,
    ) -> Result<Option<ActionHash>> {
        let withdrawal = match WithdrawalPayload::deserialize(&row.payload_json) {
            Ok(withdrawal) => withdrawal,
            Err(e) => {
                return self
                    .fail_for_a_person(
                        row,
                        "bridge.payload_unreadable",
                        &format!("its payload does not read: {e}"),
                    )
                    .map(|()| None)
            }
        };
        let spend = withdrawal.spend.as_str();
        let agreement = match parked_on(conductor, live, spend, Some(&withdrawal.agreement)).await {
            Ok((agreement, true)) => {
                if agreement != *in_force {
                    error!(
                        event = "bridge.reconcile.superseded_agreement",
                        "[bridge/reconcile] {} waits on agreement {}, no longer in force: only a RAVE on it can take spend {}",
                        row.item_id,
                        agreement,
                        spend
                    );
                }
                return Ok(None);
            }
            Ok((agreement, false)) => agreement,
            Err(e) => {
                return Self::unresolved(row, &format!("its spend {spend}"), e).map(|()| None)
            }
        };
        let untaken = format!("left agreement {agreement}, and no RAVE in the bridging agent's chain history records it consumed");
        // A spend is written on its withdrawer's chain, so its sequence bounds
        // nothing on the bridging agent's.
        match live.consumer(conductor, spend, 0).await {
            Ok(Some(rave)) => Ok(Some(rave)),
            Ok(None) if gone_before.contains(spend) => self
                .for_a_person(row, "bridge.reconcile.link_not_consumed", spend, &untaken)
                .map(|()| None),
            Ok(None) => {
                gone.insert(spend.to_string());
                warn!(
                    event = "bridge.reconcile.spend_gone",
                    spend,
                    "[bridge/reconcile] {} waits a cycle, as its spend {spend} {untaken}",
                    row.item_id
                );
                Ok(None)
            }
            Err(e) => Self::unresolved(row, &format!("its spend {spend}"), e).map(|()| None),
        }
    }

    pub(super) fn withdrawal_of(&self, item: &WorkItem) -> Option<WithdrawalPayload> {
        match WithdrawalPayload::deserialize(&item.payload_json) {
            Ok(withdrawal) => Some(withdrawal),
            Err(e) => {
                error!(
                    event = "bridge.payload_unreadable",
                    "[bridge] {} has an unreadable withdrawal payload: {}", item.item_id, e
                );
                None
            }
        }
    }
}

fn item_ids(rows: &[WorkItem]) -> String {
    let ids: Vec<&str> = rows.iter().map(|row| row.item_id.as_str()).collect();
    ids.join(", ")
}
