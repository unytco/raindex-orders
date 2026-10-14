use super::BridgeOrchestrator;
use crate::screening::{Role, Screened, Screener, Transfer, Verdict};
use crate::state::{Standing, StateStore, WorkItem, WorkState};
use alloy::primitives::Address;
use anyhow::{Context, Result};
use ham::ShutdownRx;
use tracing::{error, info, warn};

impl BridgeOrchestrator {
    pub(super) fn screener(&self) -> Option<&dyn Screener> {
        self.ethereum.as_ref()?.screener.as_deref()
    }

    /// Screens each lock in `screening`: a clear verdict queues it, a hit holds it, and with no
    /// verdict it waits for the next pass.
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
            let addresses = match lock_addresses(row) {
                Ok(addresses) => addresses,
                Err(e) => {
                    unreadable.push((row, e));
                    continue;
                }
            };
            let plain: Vec<Address> = addresses.iter().map(|(address, _)| *address).collect();
            let screened = screener.screen(&plain).await;
            let transfer = Transfer::of_lock_row(&row.item_id);
            if self
                .db
                .record_verdict(&transfer, &screened, &addresses, Some(row.id))?
            {
                let amount = row.payload_json["amount_hot"].as_str().unwrap_or("unknown");
                log_verdict(&transfer, &screened, amount);
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

/// The lock's `sender`, and its transaction's `from` where that is another address.
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

/// S4's check before it signs a withdrawal's coupon. What is recorded for the withdrawal decides
/// first, screening on or off, so turning screening off pays no held or waiting withdrawal.
pub struct WithdrawalGate<'a> {
    pub db: &'a StateStore,
    pub screener: Option<&'a dyn Screener>,
}

impl WithdrawalGate<'_> {
    pub async fn pays(&self, spend: &str, to: Address, amount: &str) -> Result<bool> {
        let screener = match (self.db.withdrawal_standing(spend)?, self.screener) {
            (Some(standing), _) if standing.pays() => return Ok(true),
            (None | Some(Standing::Screening), Some(screener)) => screener,
            (None, None) => return Ok(true),
            (Some(_), _) => return Ok(false),
        };
        let screened = screener.screen(&[to]).await;
        let transfer = Transfer::Withdrawal(spend.to_string());
        if self
            .db
            .record_verdict(&transfer, &screened, &[(to, Role::WithdrawToAddress)], None)?
        {
            log_verdict(&transfer, &screened, amount);
        }
        Ok(screened.verdict == Verdict::Clear)
    }
}

fn log_verdict(transfer: &Transfer, screened: &Screened, amount: &str) {
    let provider = screened.provider;
    let list_version = screened.list.as_ref().map(|list| list.sha256.as_str());
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
            "[screening] {transfer} waits, as {provider} did not answer: {cause}"
        ),
    }
}
