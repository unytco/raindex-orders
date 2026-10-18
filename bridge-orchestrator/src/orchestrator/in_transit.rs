use super::{
    bridging_spend, connect_ham, deposit_proofs, hot_amount, normalize_tx_hash, own_deposit,
    vault_key, withdrawal_terms, BridgeOrchestrator, BridgingSpend, Conductor, ConductorReads,
    LockKey, WithdrawalGate, WITHDRAWALS_SCREENED,
};
use crate::config::{Config, Ethereum};
use crate::screening::Screener;
use crate::state::{StateStore, WorkItem, WorkState, WorkStep, WITHDRAWAL_FLOW};
use crate::watchtower_reporter::ReporterState;
use anyhow::Result;
use holo_hash::ActionHash;
use rave_engine::types::UnitMap;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use tracing::info;

const IN_TRANSIT: &str = "in transit at the old network's close";
const TAKEN_WITH_ITS_RAVE: &str = "its HOT is taken off the credit limit by hand only together with collecting what its RAVE allocated, or neither is done";

fn settled_by_hand(row: &WorkItem) -> String {
    let left = if row.flow != WITHDRAWAL_FLOW {
        "it is paid by hand on the new network"
    } else {
        match row.step {
            WorkStep::Sent => {
                return format!("{IN_TRANSIT}; if a RAVE took its spend, {TAKEN_WITH_ITS_RAVE}")
            }
            WorkStep::Paid => TAKEN_WITH_ITS_RAVE,
            WorkStep::Collected => "its HOT is taken off the credit limit by hand",
            WorkStep::ClLinkCreated
            | WorkStep::New
            | WorkStep::ClRaveExecuted
            | WorkStep::BrSpendCreated
            | WorkStep::BrRaveExecuted => {
                "if S2's RAVE did not take its link, its HOT is taken off the credit limit by hand"
            }
        }
    };
    format!("{IN_TRANSIT}; {left}")
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum InTransit {
    Row {
        item_id: String,
        lock_id: Option<String>,
        state: WorkState,
        step: WorkStep,
        link: Option<String>,
    },
    DepositLink {
        agreement: String,
        link: String,
        lock_ids: Vec<String>,
    },
    Withdrawal {
        agreement: String,
        spend: String,
        spender: String,
        amount: UnitMap,
        withdraw_to_address: Option<String>,
        /// Not cleared by screening, so not to be paid by hand.
        #[serde(skip_serializing_if = "Option::is_none")]
        screening: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        stranded: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        unpayable: Option<String>,
    },
}

pub(super) struct Found {
    listed: Vec<InTransit>,
    rows: BTreeMap<i64, WorkItem>,
    unmatchable: Vec<String>,
}

impl Found {
    pub(super) fn settle(
        &self,
        out: &mut impl Write,
        db: &StateStore,
        mark_failed: bool,
    ) -> Result<()> {
        for listed in &self.listed {
            writeln!(out, "{}", serde_json::to_string(listed)?)?;
        }
        let cannot_tell = format!(
            "it cannot tell which rows the listed links carry: {}",
            self.unmatchable.join("; ")
        );
        if !mark_failed {
            anyhow::ensure!(
                self.listed.is_empty(),
                "the old network holds transfers in transit: {} line(s) listed{}",
                self.listed.len(),
                if self.unmatchable.is_empty() {
                    String::new()
                } else {
                    format!(", and --mark-failed would mark no row, as {cannot_tell}")
                }
            );
            return Ok(());
        }
        anyhow::ensure!(
            self.unmatchable.is_empty(),
            "marks no row, as {cannot_tell}"
        );
        let rows: Vec<(&WorkItem, String)> = self
            .rows
            .values()
            .map(|row| (row, settled_by_hand(row)))
            .collect();
        db.mark_all_failed_permanent(&rows)?;
        let items: Vec<&str> = rows.iter().map(|(row, _)| row.item_id.as_str()).collect();
        info!(
            event = "bridge.in_transit.marked_failed",
            "[bridge/in-transit] marked {} row(s) failed, to be settled by hand: {}",
            items.len(),
            items.join(", ")
        );
        Ok(())
    }
}

pub async fn run(cfg: Config, ethereum: Option<Ethereum>, mark_failed: bool) -> Result<()> {
    let db = StateStore::open_existing(&cfg.db_path)?;
    let screener = match &ethereum {
        Some(chain) => crate::config::screener_from_env(chain.network)?,
        None => None,
    };
    let vault = ethereum.map(|chain| vault_key(chain.lock_vault_address));
    if let Some(configured) = &vault {
        let served = db.vault()?;
        anyhow::ensure!(
            served.as_deref() == Some(configured.as_str()),
            "DB_PATH {} serves vault {}, and vault {configured} is configured",
            cfg.db_path,
            served.as_deref().unwrap_or("none")
        );
    }
    let ham = connect_ham(&cfg).await?;
    let orchestrator = BridgeOrchestrator {
        cfg,
        db,
        reporter: ReporterState::new(),
        ethereum: None,
        deferred: Default::default(),
        told: Default::default(),
    };
    let found = orchestrator
        .in_transit(
            &Conductor {
                ham: &ham,
                role_name: &orchestrator.cfg.role_name,
            },
            screener.as_deref(),
            vault.as_deref(),
        )
        .await?;
    found.settle(&mut std::io::stdout(), &orchestrator.db, mark_failed)
}

impl BridgeOrchestrator {
    /// Holds a listed withdrawal.
    pub(super) async fn in_transit(
        &self,
        conductor: &impl ConductorReads,
        screener: Option<&dyn Screener>,
        vault: Option<&str>,
    ) -> Result<Found> {
        let agent = &self.cfg.bridging_agent_pubkey;
        let global_definition = conductor.global_definition().await?;
        let context = Self::resolve_deposit_context(
            conductor,
            agent,
            self.cfg.hot_unit_index,
            &global_definition,
        )
        .await?;
        let credit_limit: ActionHash = context.credit_limit_adjustment.clone().into();
        let bridging: ActionHash = context.bridging_agreement.clone().into();
        let credit_limit_links = conductor.parked_links(&credit_limit).await?;
        let bridging_links = conductor.parked_links(&bridging).await?;

        let deposits = credit_limit_links
            .iter()
            .filter(|link| own_deposit(link, agent))
            .map(|link| (&context.credit_limit_adjustment, link))
            .chain(
                bridging_links
                    .iter()
                    .filter(|spend| {
                        matches!(
                            bridging_spend(spend, agent, vault),
                            Some(BridgingSpend::Deposit)
                        )
                    })
                    .map(|spend| (&context.bridging_agreement, spend)),
            );
        let unreadable = self.db.unreadable_rows()?;
        anyhow::ensure!(
            unreadable.is_empty(),
            "rows whose state or step cannot be read: {}",
            unreadable.join(", ")
        );
        let mut pending: Vec<(WorkItem, Option<LockKey>)> = Vec::new();
        for row in self.db.list_all()? {
            if matches!(row.state, WorkState::Succeeded | WorkState::Failed) {
                continue;
            }
            let lock = (row.flow != WITHDRAWAL_FLOW)
                .then(|| self.lock_key(&row))
                .flatten();
            pending.push((row, lock));
        }

        let mut links = Vec::new();
        let mut carried = HashSet::new();
        let mut nameless = Vec::new();
        for (agreement, link) in deposits {
            let proofs = match deposit_proofs(link) {
                Some(Value::Array(proofs)) => proofs.as_slice(),
                Some(proof) => std::slice::from_ref(proof),
                None => &[],
            };
            let mut lock_ids = Vec::new();
            for proof in proofs {
                match LockKey::of_proof(proof) {
                    Some(lock) => {
                        lock_ids.push(lock.lock_id.clone());
                        carried.insert(lock);
                    }
                    None => nameless.push((link, proof)),
                }
            }
            lock_ids.sort();
            lock_ids.dedup();
            links.push(InTransit::DepositLink {
                agreement: agreement.to_string(),
                link: link.id.to_string(),
                lock_ids,
            });
        }
        let gate = WithdrawalGate::new(&self.db, screener);
        let mut withdrawals = Vec::new();
        for spend in &bridging_links {
            let (withdraw_to, stranded) = match bridging_spend(spend, agent, vault) {
                Some(BridgingSpend::Withdrawal(to)) => (to, None),
                Some(BridgingSpend::OtherVault { to, why }) => (to, Some(why)),
                _ => continue,
            };
            let id = spend.id.to_string();
            let hot_unit_index = self.cfg.hot_unit_index;
            let terms = withdraw_to.map(|to| withdrawal_terms(spend, to, hot_unit_index));
            let pays = match &terms {
                Some(Ok(terms)) => {
                    let hot = hot_amount(spend, hot_unit_index);
                    gate.pays(&id, terms.recipient(), &hot).await?
                }
                Some(Err(_)) => false,
                None => true,
            };
            let screening = if pays {
                None
            } else {
                Some(
                    self.db
                        .withdrawal_hold(&id)?
                        .map_or("unscreened".to_string(), |hold| hold.to_string()),
                )
            };
            withdrawals.push(InTransit::Withdrawal {
                agreement: context.bridging_agreement.to_string(),
                spend: id,
                spender: spend.creator.to_string(),
                amount: spend.amount.clone(),
                withdraw_to_address: withdraw_to.map(str::to_string),
                screening,
                stranded,
                unpayable: terms.and_then(Result::err).map(|e| format!("{e:#}")),
            });
        }
        self.tell_waiting(WITHDRAWALS_SCREENED, &gate.into_waiting());

        let mut listed = Vec::new();
        let mut rows = BTreeMap::new();
        let mut unmarked = Vec::new();
        let mut unmatchable = Vec::new();
        for (row, lock) in pending {
            let in_transit = row.flow == WITHDRAWAL_FLOW
                || matches!(row.step, WorkStep::ClLinkCreated | WorkStep::BrSpendCreated)
                || matches!(row.state, WorkState::InFlight | WorkState::Claimed);
            if in_transit {
                listed.push(InTransit::Row {
                    item_id: row.item_id.clone(),
                    lock_id: lock.as_ref().map(|lock| lock.lock_id.clone()),
                    state: row.state.clone(),
                    step: row.step.clone(),
                    link: row.parked_link().map(|(link, _)| link.to_string()),
                });
            }
            match lock {
                _ if in_transit => {
                    rows.insert(row.id, row);
                }
                Some(lock) if carried.contains(&lock) => {
                    rows.insert(row.id, row);
                }
                Some(lock) => unmarked.push((row.item_id, lock)),
                None if !carried.is_empty() => unmatchable.push(format!(
                    "row {} has a lock that cannot be read, which the next cycle of `run` fails for a person",
                    row.item_id
                )),
                None => {}
            }
        }
        for (link, proof) in nameless {
            let named = |field: &str| proof.get(field).and_then(Value::as_str);
            let may_be: Vec<&str> = unmarked
                .iter()
                .filter(|(_, lock)| {
                    named("lock_id") == Some(lock.lock_id.as_str())
                        || named("tx_hash").map(normalize_tx_hash).as_ref() == Some(&lock.tx_hash)
                })
                .map(|(item_id, _)| item_id.as_str())
                .collect();
            if !may_be.is_empty() {
                unmatchable.push(format!(
                    "deposit link {} carries proof {proof}, which names no lock but may be that of {}",
                    link.id,
                    may_be.join(", ")
                ));
            }
        }
        listed.extend(links);
        listed.extend(withdrawals);
        Ok(Found {
            listed,
            rows,
            unmatchable,
        })
    }
}
