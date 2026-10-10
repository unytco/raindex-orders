use super::{
    bridging_spend, connect_ham, own_deposit, BridgeOrchestrator, BridgingSpend, Conductor,
    ConductorReads,
};
use crate::config::Config;
use crate::state::{StateStore, WorkState, WorkStep};
use crate::watchtower_reporter::ReporterState;
use anyhow::Result;
use holo_hash::ActionHash;
use rave_engine::types::UnitMap;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use tracing::info;

pub(super) const PAID_BY_HAND: &str =
    "in transit at the old network's close; it is paid by hand on the new network";

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum InTransit {
    Row {
        item_id: String,
        lock_id: Option<String>,
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
    pub(super) listed: Vec<InTransit>,
    rows: Vec<i64>,
}

impl Found {
    pub(super) fn settle(&self, db: &StateStore, mark_failed: bool) -> Result<()> {
        if !mark_failed {
            anyhow::ensure!(
                self.listed.is_empty(),
                "the old network holds {} transfer(s) in transit",
                self.listed.len()
            );
            return Ok(());
        }
        db.mark_all_failed_permanent(&self.rows, PAID_BY_HAND)?;
        info!(
            event = "bridge.in_transit.marked_failed",
            rows = ?self.rows,
            "[bridge/in-transit] marked {} row(s) failed, to be paid by hand on the new network",
            self.rows.len()
        );
        Ok(())
    }
}

pub async fn run(cfg: Config, mark_failed: bool) -> Result<()> {
    anyhow::ensure!(
        Path::new(&cfg.db_path).exists(),
        "DB_PATH {} does not exist, so it holds no row to read",
        cfg.db_path
    );
    let db = StateStore::open(&cfg.db_path)?;
    let ham = connect_ham(&cfg).await?;
    let orchestrator = BridgeOrchestrator {
        cfg,
        db,
        reporter: ReporterState::new(),
        ethereum: None,
        deferred: Default::default(),
    };
    let found = orchestrator
        .in_transit(&Conductor {
            ham: &ham,
            role_name: &orchestrator.cfg.role_name,
        })
        .await?;
    for listed in &found.listed {
        println!("{}", serde_json::to_string(listed)?);
    }
    found.settle(&orchestrator.db, mark_failed)
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
        let mut links = Vec::new();
        let mut carried = HashSet::new();
        for (agreement, link) in deposits {
            let locks = self.links_by_lock(std::slice::from_ref(link));
            let mut lock_ids: Vec<String> = locks.keys().map(|lock| lock.lock_id.clone()).collect();
            lock_ids.sort();
            carried.extend(locks.into_keys());
            links.push(InTransit::DepositLink {
                agreement: agreement.to_string(),
                link: link.id.to_string(),
                lock_ids,
            });
        }
        let withdrawals = bridging_links.iter().filter_map(|spend| {
            let Some(BridgingSpend::Withdrawal(payload)) = bridging_spend(spend, agent) else {
                return None;
            };
            Some(InTransit::Withdrawal {
                agreement: context.bridging_agreement.to_string(),
                spend: spend.id.to_string(),
                spender: spend.creator.to_string(),
                amount: spend.amount.clone(),
                withdraw_to_address: payload
                    .get("withdraw_to_address")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        });

        let mut listed = Vec::new();
        let mut rows = BTreeSet::new();
        for row in self.db.list_flow("lock")? {
            if matches!(row.state, WorkState::Succeeded | WorkState::Failed) {
                continue;
            }
            let lock = self.lock_key(&row);
            let waits_on_its_link =
                matches!(row.step, WorkStep::ClLinkCreated | WorkStep::BrSpendCreated);
            if waits_on_its_link {
                listed.push(InTransit::Row {
                    item_id: row.item_id.clone(),
                    lock_id: lock.as_ref().map(|lock| lock.lock_id.clone()),
                    step: row.step.clone(),
                    link: row.parked_link().map(|(link, _)| link.to_string()),
                });
            }
            if waits_on_its_link || lock.is_some_and(|lock| carried.contains(&lock)) {
                rows.insert(row.id);
            }
        }
        listed.extend(links);
        listed.extend(withdrawals);
        Ok(Found {
            listed,
            rows: rows.into_iter().collect(),
        })
    }
}
