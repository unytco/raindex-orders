use super::{
    bridging_spend, connect_ham, deposit_proofs, normalize_tx_hash, own_deposit,
    BridgeOrchestrator, BridgingSpend, Conductor, ConductorReads, LockKey,
};
use crate::config::{Config, Ethereum};
use crate::state::{StateStore, WorkItem, WorkState, WorkStep};
use crate::watchtower_reporter::ReporterState;
use anyhow::{Context, Result};
use holo_hash::ActionHash;
use rave_engine::types::UnitMap;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::Path;
use tracing::info;

const PAID_BY_HAND: &str =
    "in transit at the old network's close; it is paid by hand on the new network";

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
        let rows: Vec<&WorkItem> = self.rows.values().collect();
        db.mark_all_failed_permanent(&rows, PAID_BY_HAND)?;
        let items: Vec<&str> = rows.iter().map(|row| row.item_id.as_str()).collect();
        info!(
            event = "bridge.in_transit.marked_failed",
            "[bridge/in-transit] marked {} row(s) failed, to be paid by hand on the new network: {}",
            items.len(),
            items.join(", ")
        );
        Ok(())
    }
}

pub async fn run(cfg: Config, ethereum: Option<Ethereum>, mark_failed: bool) -> Result<()> {
    let exists = Path::new(&cfg.db_path)
        .try_exists()
        .with_context(|| format!("DB_PATH {} cannot be read", cfg.db_path))?;
    anyhow::ensure!(
        exists,
        "DB_PATH {} does not exist: opening it would create an empty database, which lists nothing in transit",
        cfg.db_path
    );
    let db = StateStore::open(&cfg.db_path)?;
    if let Some(chain) = ethereum {
        let configured = format!("{:#x}", chain.lock_vault_address);
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
        .in_transit(&Conductor {
            ham: &ham,
            role_name: &orchestrator.cfg.role_name,
        })
        .await?;
    found.settle(&mut std::io::stdout(), &orchestrator.db, mark_failed)
}

impl BridgeOrchestrator {
    pub(super) async fn in_transit(&self, conductor: &impl ConductorReads) -> Result<Found> {
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
                        matches!(bridging_spend(spend, agent), Some(BridgingSpend::Deposit))
                    })
                    .map(|spend| (&context.bridging_agreement, spend)),
            );
        let unreadable = self.db.unreadable_rows("lock")?;
        anyhow::ensure!(
            unreadable.is_empty(),
            "rows whose state or step cannot be read: {}",
            unreadable.join(", ")
        );
        let pending: Vec<(WorkItem, Option<LockKey>)> = self
            .db
            .list_flow("lock")?
            .into_iter()
            .filter(|row| !matches!(row.state, WorkState::Succeeded | WorkState::Failed))
            .map(|row| {
                let lock = self.lock_key(&row);
                (row, lock)
            })
            .collect();

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
        let withdrawals = bridging_links.iter().filter_map(|spend| {
            let Some(BridgingSpend::Withdrawal(withdraw_to)) = bridging_spend(spend, agent) else {
                return None;
            };
            Some(InTransit::Withdrawal {
                agreement: context.bridging_agreement.to_string(),
                spend: spend.id.to_string(),
                spender: spend.creator.to_string(),
                amount: spend.amount.clone(),
                withdraw_to_address: withdraw_to.map(str::to_string),
            })
        });

        let mut listed = Vec::new();
        let mut rows = BTreeMap::new();
        let mut unmarked = Vec::new();
        let mut unmatchable = Vec::new();
        for (row, lock) in pending {
            let in_transit = matches!(row.step, WorkStep::ClLinkCreated | WorkStep::BrSpendCreated)
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
