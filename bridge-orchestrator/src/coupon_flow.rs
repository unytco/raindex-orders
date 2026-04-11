use crate::config::Config;
use crate::ham::Ham;
use crate::state::StateStore;
use anyhow::Result;
use rave_engine::types::Transaction;
use tracing::info;

pub struct CouponFlow {
    cfg: Config,
    db: StateStore,
}

impl CouponFlow {
    pub fn new(cfg: Config, db: StateStore) -> Self {
        Self { cfg, db }
    }

    pub async fn run_cycle(&self, ham: &Ham) -> Result<()> {
        info!("coupon scan started ea_id={}", self.cfg.bridging_agreement_id);
        let links: Vec<Transaction> = ham
            .call_zome(
                &self.cfg.role_name,
                "transactor",
                "get_parked_links_by_ea",
                &self.cfg.bridging_agreement_id,
            )
            .await?;
        let candidate_count = links.len();

        for tx in links {
            let item_id = format!("coupon:{:?}", tx.id);
            let idempotency_key = format!("coupon:{:?}:execute_rave", tx.id);
            let amount = tx
                .amount
                .get("1")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "0".to_string());
            info!(
                "coupon candidate found id={} amount_raw={}",
                item_id, amount
            );
            let payload = serde_json::to_value(&tx)?;
            let changed = self.db.enqueue_queued_or_requeue_transient_failed(
                "coupon",
                "execute_rave",
                &item_id,
                &idempotency_key,
                &payload,
            )?;
            info!("coupon queued id={}", item_id);
            if changed {
                info!("coupon requeued from scan id={}", item_id);
            }
        }

        info!(
            "coupon scan finished ea_id={} candidates={}",
            self.cfg.bridging_agreement_id,
            candidate_count
        );
        Ok(())
    }
}
