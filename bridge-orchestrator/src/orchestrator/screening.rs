use super::BridgeOrchestrator;
use crate::lock_flow::LockFlow;
use crate::screening::{screen, Role, Screened, Screener, Transfer, Verdict};
use crate::state::{Standing, StateStore, WorkItem, WorkState};
use alloy::primitives::Address;
use anyhow::{Context, Result};
use ham::ShutdownRx;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{error, info, warn};

impl BridgeOrchestrator {
    pub(super) fn screener(&self) -> Option<&dyn Screener> {
        self.ethereum.as_ref()?.screener.as_deref()
    }

    pub(super) fn lock_flow(&self, stop: &ShutdownRx) -> Option<LockFlow> {
        let side = self.ethereum.as_ref()?;
        Some(LockFlow::new(
            side.chain.clone(),
            self.db.clone(),
            stop.clone(),
            side.screener.is_some(),
        ))
    }

    /// A failed lock read does not keep the locks it recorded earlier from being screened.
    pub(super) async fn read_and_screen_locks(&self, lock_flow: &LockFlow, stop: &ShutdownRx) {
        if let Err(e) = lock_flow.run_cycle().await {
            if !crate::stop::is_stopped(&e) {
                error!(
                    event = "bridge.lock_read_failed",
                    "[lock-flow] cycle failed: {e:#}"
                );
            }
        }
        let Some(screener) = self.screener() else {
            return;
        };
        if let Err(e) = self.screen_waiting_locks(screener, stop).await {
            error!(
                event = "bridge.screening.pass_failed",
                "[screening] the pass over the locks in screening failed: {e:#}"
            );
        }
    }

    pub(super) async fn screen_waiting_locks(
        &self,
        screener: &dyn Screener,
        stop: &ShutdownRx,
    ) -> Result<()> {
        let waiting = self
            .db
            .list_work_items("lock", WorkState::Screening, 5000)?;
        let mut unreadable = Vec::new();
        for row in &waiting {
            if *stop.borrow() {
                break;
            }
            let roles = match lock_addresses(row) {
                Ok(roles) => roles,
                Err(e) => {
                    unreadable.push((row, e));
                    continue;
                }
            };
            let addresses: Vec<Address> = roles.iter().map(|(address, _)| *address).collect();
            let screened = screen(screener, &addresses).await;
            self.db
                .record_lock_verdict(&row.item_id, &screened, &roles)?;
            let amount = row.payload_json["amount_hot"].as_str().unwrap_or("unknown");
            log_verdict(&Transfer::of_lock_row(&row.item_id), &screened, amount);
            if !screened.in_time {
                break;
            }
        }
        for (row, e) in self.untold("screening_unreadable", &unreadable, |(row, _)| {
            row.item_id.clone()
        }) {
            error!(
                event = "bridge.screening.unreadable",
                transfer = %row.item_id,
                "[screening] {} stays in screening, as it names no address to screen: {e:#}",
                row.item_id
            );
        }
        Ok(())
    }
}

fn lock_addresses(row: &WorkItem) -> Result<Vec<(Address, Role)>> {
    let address = |field: &str| -> Result<Address> {
        let raw = row.payload_json[field]
            .as_str()
            .with_context(|| format!("its payload has no {field}"))?;
        raw.parse()
            .with_context(|| format!("its {field} {raw} is not an address"))
    };
    let sender = address("sender")?;
    let from = address("from")?;
    let mut addresses = vec![(sender, Role::Sender)];
    if from != sender {
        addresses.push((from, Role::From));
    }
    Ok(addresses)
}

/// A recorded standing decides before the screener does, so turning screening off pays no held
/// or waiting withdrawal.
pub struct WithdrawalGate<'a> {
    db: &'a StateStore,
    screener: Option<&'a dyn Screener>,
    unanswered: AtomicBool,
}

impl<'a> WithdrawalGate<'a> {
    pub fn new(db: &'a StateStore, screener: Option<&'a dyn Screener>) -> Self {
        Self {
            db,
            screener,
            unanswered: AtomicBool::new(false),
        }
    }

    pub async fn pays(&self, spend: &str, to: Address, amount: &str) -> Result<bool> {
        let screener = match (self.db.withdrawal_standing(spend)?, self.screener) {
            (Standing::Unscreened | Standing::Screening, Some(screener)) => screener,
            (Standing::Unscreened, None) => return Ok(true),
            (standing, _) => return Ok(standing.pays()),
        };
        if self.unanswered.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let screened = screen(screener, &[to]).await;
        if !screened.in_time {
            self.unanswered.store(true, Ordering::Relaxed);
        }
        log_verdict(&Transfer::Withdrawal(spend.to_string()), &screened, amount);
        Ok(self
            .db
            .record_withdrawal_verdict(spend, &screened, to)?
            .pays())
    }
}

fn log_verdict(transfer: &Transfer, screened: &Screened, amount: &str) {
    let provider = screened.provider;
    let list_version = screened.list.as_ref().map(|list| list.version());
    match &screened.verdict {
        Verdict::Clear => info!(
            event = "bridge.screening.clear",
            transfer = %transfer,
            provider,
            list_version,
            "[screening] {transfer} is clear"
        ),
        Verdict::Hit(listed) => error!(
            event = "bridge.screening.hit",
            transfer = %transfer,
            provider,
            list_version,
            amount,
            "[screening] {transfer} of {amount} HOT is held for a person: {provider} lists {}",
            listed
                .iter()
                .map(|address| format!("{address:#x}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Verdict::NoAnswer(cause) => warn!(
            event = "bridge.screening.no_answer",
            transfer = %transfer,
            provider,
            "[screening] {transfer} waits, as {provider} gave no answer: {cause}"
        ),
    }
}
