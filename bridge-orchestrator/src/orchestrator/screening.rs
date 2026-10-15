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

pub(super) const LOCKS_SCREENED: &str = "screened/lock";
pub(super) const WITHDRAWALS_SCREENED: &str = "screened/withdrawal";

/// A transfer screening stopped, with what to log of it.
pub(super) struct ScreenedOut {
    /// The told key. A withdrawal's is its bare spend, as `forget_unparked` keeps only parked link
    /// IDs.
    id: String,
    transfer: String,
    amount: String,
    tx_hash: Option<String>,
    screened: Screened,
}

impl ScreenedOut {
    fn log(&self) {
        let Self {
            transfer,
            amount,
            tx_hash,
            screened,
            ..
        } = self;
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
                "[screening] {transfer} of {amount} HOT is a hit: {provider} lists {listed:?}"
            ),
            Verdict::NoAnswer(cause) => warn!(
                event = "bridge.screening.no_answer",
                transfer,
                amount,
                tx_hash,
                provider,
                cause,
                "[screening] {transfer} of {amount} HOT waits with no verdict: {cause}"
            ),
            Verdict::Clear => {}
        }
    }
}

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

    /// A failed lock read still screens the locks already recorded.
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
        let missed = AtomicBool::new(false);
        let mut waiting = Vec::new();
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
            let field = |name: &str| row.payload_json[name].as_str().map(str::to_string);
            let out = ScreenedOut {
                id: row.item_id.clone(),
                transfer: row.item_id.clone(),
                amount: field("amount_hot").unwrap_or_default(),
                tx_hash: field("tx_hash"),
                screened,
            };
            match out.screened.verdict {
                Verdict::Clear => self
                    .db
                    .settle_screened_lock(&row.item_id, WorkState::Queued)?,
                Verdict::Hit(_) => {
                    self.db
                        .settle_screened_lock(&row.item_id, WorkState::Held)?;
                    out.log();
                }
                Verdict::NoAnswer(_) => waiting.push(out),
            }
        }
        self.tell_waiting(LOCKS_SCREENED, &waiting);
        Ok(())
    }

    pub(super) fn tell_waiting(&self, check: &str, waiting: &[ScreenedOut]) {
        for out in self.untold(check, waiting, |out| out.id.clone()) {
            out.log();
        }
    }
}

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
    waiting: Mutex<Vec<ScreenedOut>>,
}

impl<'a> WithdrawalGate<'a> {
    pub fn new(db: &'a StateStore, screener: Option<&'a dyn Screener>) -> Self {
        Self {
            db,
            screener,
            missed: AtomicBool::new(false),
            waiting: Mutex::new(Vec::new()),
        }
    }

    pub async fn pays(&self, spend: &str, to: Address, amount: &str) -> Result<bool> {
        if let Some(hold) = self.db.withdrawal_hold(spend)? {
            return Ok(hold == Hold::Released);
        }
        let Some(screener) = self.screener else {
            return Ok(true);
        };
        let out = ScreenedOut {
            id: spend.to_string(),
            transfer: format!("withdrawal:{spend}"),
            amount: amount.to_string(),
            tx_hash: None,
            screened: screen_unless_missed(screener, &[to], &self.missed).await,
        };
        match &out.screened.verdict {
            // A person's block made while it was screened wins.
            Verdict::Clear => Ok(self.db.withdrawal_hold(spend)?.is_none()),
            Verdict::Hit(listed) => {
                let hold = self.db.hold_withdrawal(spend, listed)?;
                out.log();
                Ok(hold == Hold::Released)
            }
            Verdict::NoAnswer(_) => {
                self.waiting
                    .lock()
                    .expect("waiting mutex poisoned")
                    .push(out);
                Ok(false)
            }
        }
    }

    pub(super) fn into_waiting(self) -> Vec<ScreenedOut> {
        self.waiting.into_inner().expect("waiting mutex poisoned")
    }
}
