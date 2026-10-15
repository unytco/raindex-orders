use super::BridgeOrchestrator;
use crate::lock_flow::LockFlow;
use crate::screening::{screen, Screened, Screener, Verdict};
use crate::state::{Hold, StateStore, WorkItem, WorkState};
use alloy::primitives::Address;
use anyhow::{Context, Result};
use ham::ShutdownRx;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tracing::{error, warn};

/// Lock item IDs are no parked links, so `forget_unparked` leaves these checks' told sets alone.
pub(super) const LOCKS_SCREENED: &str = "screened/lock";
pub(super) const WITHDRAWALS_SCREENED: &str = "screened/withdrawal";

/// A transfer screening stopped, with what to log of it.
pub(super) struct ScreenedOut {
    /// What it is told once by: a lock's item ID, or a withdrawal's spend.
    id: String,
    transfer: String,
    amount: String,
    tx_hash: Option<String>,
    screened: Screened,
}

/// Screens `addresses`, unless the provider already missed its bound in this pass or cycle.
async fn screen_unless_missed(
    screener: &dyn Screener,
    addresses: &[Address],
    missed: &AtomicBool,
) -> Screened {
    if missed.load(Ordering::Relaxed) {
        return Screened::no_answer(
            screener.name(),
            "not asked, as it gave no answer in time earlier".to_string(),
        );
    }
    let screened = screen(screener, addresses).await;
    if !screened.in_time {
        missed.store(true, Ordering::Relaxed);
    }
    screened
}

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

    /// A clear lock goes to `queued`, a listed one to `held`, and one with no verdict waits.
    pub(super) async fn screen_waiting_locks(
        &self,
        screener: &dyn Screener,
        stop: &ShutdownRx,
    ) -> Result<()> {
        let missed = AtomicBool::new(false);
        let mut screened_out = Vec::new();
        for row in self
            .db
            .list_work_items("lock", WorkState::Screening, 5000)?
        {
            if *stop.borrow() {
                break;
            }
            let screened = match lock_addresses(&row) {
                Ok(addresses) => screen_unless_missed(screener, &addresses, &missed).await,
                Err(e) => Screened::no_answer(screener.name(), format!("{e:#}")),
            };
            match screened.verdict {
                Verdict::Clear => {
                    self.db
                        .settle_screened_lock(&row.item_id, WorkState::Queued)?;
                    continue;
                }
                Verdict::Hit(_) => self
                    .db
                    .settle_screened_lock(&row.item_id, WorkState::Held)?,
                Verdict::NoAnswer(_) => {}
            }
            let field = |name: &str| row.payload_json[name].as_str().map(str::to_string);
            screened_out.push(ScreenedOut {
                id: row.item_id.clone(),
                transfer: row.item_id.clone(),
                amount: field("amount_hot").unwrap_or_default(),
                tx_hash: field("tx_hash"),
                screened,
            });
        }
        self.log_screened_out(LOCKS_SCREENED, &screened_out);
        Ok(())
    }

    /// One told set per verdict, so a hit after a no-answer is still told.
    pub(super) fn log_screened_out(&self, check: &str, screened_out: &[ScreenedOut]) {
        for verdict in ["hit", "no_answer"] {
            let of_verdict: Vec<&ScreenedOut> = screened_out
                .iter()
                .filter(|out| out.screened.verdict.name() == verdict)
                .collect();
            for out in self.untold(&format!("{check}/{verdict}"), &of_verdict, |out| {
                out.id.clone()
            }) {
                let ScreenedOut {
                    transfer,
                    amount,
                    tx_hash,
                    screened,
                    ..
                } = *out;
                let provider = screened.provider;
                let tx_hash = tx_hash.as_deref();
                match &screened.verdict {
                    Verdict::Hit(listed) => error!(
                        event = "bridge.screening.hit",
                        transfer,
                        listed = ?listed,
                        amount,
                        tx_hash,
                        provider,
                        "[screening] {transfer} of {amount} HOT is held for a person: {provider} lists {listed:?}"
                    ),
                    Verdict::NoAnswer(cause) => warn!(
                        event = "bridge.screening.no_answer",
                        transfer,
                        amount,
                        tx_hash,
                        provider,
                        cause,
                        "[screening] {transfer} of {amount} HOT waits, as {provider} gave no answer: {cause}"
                    ),
                    Verdict::Clear => {}
                }
            }
        }
    }
}

/// The lock's `sender`, and its transaction's `from` where that is another address.
fn lock_addresses(row: &WorkItem) -> Result<Vec<Address>> {
    let address = |field: &str| -> Result<Address> {
        let raw = row.payload_json[field]
            .as_str()
            .with_context(|| format!("its payload has no {field}"))?;
        raw.parse()
            .with_context(|| format!("its {field} {raw} is not an address"))
    };
    let sender = address("sender")?;
    let from = address("from")?;
    Ok(if from == sender {
        vec![sender]
    } else {
        vec![sender, from]
    })
}

/// S4's check before it signs a withdrawal's coupon. A withdrawal's hold decides first, screening
/// on or off, so turning screening off pays no held or blocked withdrawal.
pub struct WithdrawalGate<'a> {
    db: &'a StateStore,
    screener: Option<&'a dyn Screener>,
    missed: AtomicBool,
    screened_out: Mutex<Vec<ScreenedOut>>,
}

impl<'a> WithdrawalGate<'a> {
    pub fn new(db: &'a StateStore, screener: Option<&'a dyn Screener>) -> Self {
        Self {
            db,
            screener,
            missed: AtomicBool::new(false),
            screened_out: Mutex::new(Vec::new()),
        }
    }

    pub async fn pays(&self, spend: &str, to: Address, amount: &str) -> Result<bool> {
        if let Some(hold) = self.db.withdrawal_hold(spend)? {
            return Ok(hold == Hold::Released);
        }
        let Some(screener) = self.screener else {
            return Ok(true);
        };
        let screened = screen_unless_missed(screener, &[to], &self.missed).await;
        let pays = match &screened.verdict {
            // A person's block made while it was screened wins.
            Verdict::Clear => return Ok(self.db.withdrawal_hold(spend)?.is_none()),
            Verdict::Hit(listed) => self.db.hold_withdrawal(spend, listed)? == Hold::Released,
            Verdict::NoAnswer(_) => false,
        };
        self.screened_out
            .lock()
            .expect("screened_out mutex poisoned")
            .push(ScreenedOut {
                id: spend.to_string(),
                transfer: format!("withdrawal:{spend}"),
                amount: amount.to_string(),
                tx_hash: None,
                screened,
            });
        Ok(pays)
    }

    pub(super) fn into_screened_out(self) -> Vec<ScreenedOut> {
        self.screened_out
            .into_inner()
            .expect("screened_out mutex poisoned")
    }
}
