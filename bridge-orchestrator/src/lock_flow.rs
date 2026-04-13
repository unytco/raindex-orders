use crate::config::Config;
use crate::state::StateStore;
use alloy::primitives::U256;
use alloy::providers::{Provider, ProviderBuilder, RootProvider};
use alloy::rpc::types::{BlockTransactionsKind, Filter, Log};
use alloy::sol;
use alloy::sol_types::SolEvent;
use alloy::transports::http::{Client, Http};
use anyhow::{Context, Result};
use serde_json::json;
use tracing::info;

sol! {
    #[derive(Debug)]
    event Lock(
        address indexed sender,
        uint256 amount,
        bytes32 indexed holochainAgent,
        uint256 lockId
    );
}

const MAX_BLOCK_RANGE: u64 = 10;
const LOCK_CHECKPOINT_KEY: &str = "lock.last_processed_block";

pub struct LockFlow {
    cfg: Config,
    db: StateStore,
}

impl LockFlow {
    pub fn new(cfg: Config, db: StateStore) -> Self {
        Self { cfg, db }
    }

    pub async fn run_cycle(&self) -> Result<()> {
        let provider = self.provider()?;
        let current_block = provider.get_block_number().await?;
        let mut from_block = self.db.get_checkpoint_u64(LOCK_CHECKPOINT_KEY)?;
        if from_block.is_none() {
            self.db
                .set_checkpoint_u64(LOCK_CHECKPOINT_KEY, current_block)?;
            return Ok(());
        }
        let mut cursor = from_block.take().unwrap_or(current_block) + 1;
        while cursor <= current_block {
            let end = (cursor + MAX_BLOCK_RANGE - 1).min(current_block);
            let filter = Filter::new()
                .address(self.cfg.lock_vault_address)
                .event_signature(Lock::SIGNATURE_HASH)
                .from_block(cursor)
                .to_block(end);
            let logs = provider.get_logs(&filter).await?;
            for log in logs {
                self.process_lock_log(&provider, log).await?;
            }
            self.db.set_checkpoint_u64(LOCK_CHECKPOINT_KEY, end)?;
            cursor = end + 1;
        }

        self.promote_confirmed(current_block)?;
        Ok(())
    }

    fn provider(&self) -> Result<RootProvider<Http<Client>>> {
        Ok(ProviderBuilder::new().on_http(self.cfg.rpc_url.parse()?))
    }

    async fn process_lock_log(
        &self,
        provider: &RootProvider<Http<Client>>,
        log: Log,
    ) -> Result<()> {
        let decoded = log.log_decode::<Lock>().context("Failed to decode Lock event")?;
        let tx_hash = log
            .transaction_hash
            .context("Lock log missing transaction hash")?;
        let block_number = log.block_number.context("Lock log missing block number")?;
        let block = provider
            .get_block_by_number(block_number.into(), BlockTransactionsKind::Hashes)
            .await?
            .context("Block not found")?;
        let data = decoded.inner.data;
        let amount_wei = data.amount.to_string();
        let amount_hot = format_amount(&amount_wei);
        let sender = format!("{:?}", data.sender);
        let holochain_agent = format!("0x{}", hex::encode(data.holochainAgent));
        let tx_hash_hex = format!("0x{}", hex::encode(tx_hash));
        let item_id = format!("lock:{}", data.lockId);
        let idempotency_key = format!("lock:{}:create_parked_link", data.lockId);
        let payload = json!({
            "lock_id": data.lockId.to_string(),
            "sender": sender,
            "amount": amount_wei,
            "amount_raw_wei": amount_wei,
            "amount_hot": amount_hot,
            "holochain_agent": holochain_agent,
            "tx_hash": tx_hash_hex,
            "block_number": block_number,
            "timestamp": block.header.timestamp,
            "required_confirmations": self.cfg.confirmations,
        });
        self.db.enqueue_detected(
            "lock",
            "create_parked_link",
            &item_id,
            &idempotency_key,
            &payload,
        )?;
        info!(
            "[lock-flow] lock detected id={} amount={} agent={} tx={} block={}",
            item_id,
            payload["amount_hot"].as_str().unwrap_or("0"),
            payload["holochain_agent"].as_str().unwrap_or("unknown"),
            payload["tx_hash"].as_str().unwrap_or("unknown"),
            block_number
        );
        Ok(())
    }

    fn promote_confirmed(&self, current_block: u64) -> Result<()> {
        let candidates = self
            .db
            .list_work_items("lock", crate::state::WorkState::Detected, 5000)?;
        for item in candidates {
            let payload = item.payload_json;
            let block_number = payload
                .get("block_number")
                .and_then(|v| v.as_u64())
                .unwrap_or_default();
            let confirmations = current_block.saturating_sub(block_number);
            if confirmations >= self.cfg.confirmations {
                let idempotency_key = format!(
                    "lock:{}:create_parked_link",
                    payload.get("lock_id").and_then(|v| v.as_str()).unwrap_or("unknown")
                );
                if self.db.move_detected_to_queued(&idempotency_key)? {
                    let amount = payload
                        .get("amount_hot")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .or_else(|| {
                            payload
                                .get("amount_raw_wei")
                                .and_then(|v| v.as_str())
                                .map(format_amount)
                        })
                        .unwrap_or_else(|| "0".to_string());
                    let agent = payload
                        .get("holochain_agent")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    info!(
                        "[lock-flow] lock queued id={} confirmations={} amount={} agent={}",
                        item.item_id, confirmations, amount, agent
                    );
                }
            }
        }
        Ok(())
    }
}

pub fn format_amount(amount: &str) -> String {
    let amount: U256 = amount.parse().unwrap_or_default();
    let decimals = U256::from(10).pow(U256::from(18));
    let whole = amount / decimals;
    let frac = (amount % decimals) / U256::from(10).pow(U256::from(12));
    if frac.is_zero() {
        whole.to_string()
    } else {
        format!("{}.{:06}", whole, frac)
    }
}
