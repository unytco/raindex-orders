use crate::config::Ethereum;
use crate::preflight::RPC_TIMEOUT;
use crate::state::StateStore;
use crate::stop::ensure_running;
use alloy::primitives::U256;
use alloy::providers::{Provider, ProviderBuilder, RootProvider};
use alloy::rpc::types::{BlockTransactionsKind, Filter, Log};
use alloy::sol;
use alloy::sol_types::SolEvent;
use alloy::transports::http::{Client, Http};
use anyhow::{anyhow, Context, Result};
use ham::ShutdownRx;
use serde_json::json;
use std::future::IntoFuture;
use std::time::Duration;
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
    cfg: Ethereum,
    db: StateStore,
    stop: ShutdownRx,
    rpc_timeout: Duration,
}

impl LockFlow {
    pub fn new(cfg: Ethereum, db: StateStore, stop: ShutdownRx) -> Self {
        Self {
            cfg,
            db,
            stop,
            rpc_timeout: RPC_TIMEOUT,
        }
    }

    pub async fn run_cycle(&self) -> Result<()> {
        let provider = self.provider()?;
        let current_block = self
            .request("eth_blockNumber", || provider.get_block_number())
            .await?;
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
            let logs = self
                .request("eth_getLogs", || provider.get_logs(&filter))
                .await?;
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

    async fn request<T, E, F>(&self, method: &str, send: impl FnOnce() -> F) -> Result<T>
    where
        F: IntoFuture<Output = Result<T, E>>,
        E: std::error::Error + Send + Sync + 'static,
    {
        ensure_running(&self.stop)?;
        let answer = tokio::time::timeout(self.rpc_timeout, send())
            .await
            .map_err(|_| {
                anyhow!(
                    "{} did not answer {method} within {:?}",
                    self.cfg.network.rpc_url_var(),
                    self.rpc_timeout
                )
            })?;
        Ok(answer?)
    }

    async fn process_lock_log(
        &self,
        provider: &RootProvider<Http<Client>>,
        log: Log,
    ) -> Result<()> {
        let decoded = log
            .log_decode::<Lock>()
            .context("Failed to decode Lock event")?;
        let tx_hash = log
            .transaction_hash
            .context("Lock log missing transaction hash")?;
        let block_number = log.block_number.context("Lock log missing block number")?;
        let block = self
            .request("eth_getBlockByNumber", || {
                provider.get_block_by_number(block_number.into(), BlockTransactionsKind::Hashes)
            })
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
        let candidates =
            self.db
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
                    payload
                        .get("lock_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Network;
    use crate::fake_rpc::serve;
    use crate::state::WorkState;
    use crate::stop::is_stopped;
    use alloy::primitives::{Address, B256};
    use serde_json::Value;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::watch;

    fn vault() -> Address {
        Address::repeat_byte(0x44)
    }

    fn lock_flow(rpc_url: String, db: StateStore, stop: ShutdownRx) -> LockFlow {
        let chain = Ethereum {
            network: Network::Sepolia,
            rpc_url,
            lock_vault_address: vault(),
            confirmations: 5,
        };
        LockFlow::new(chain, db, stop)
    }

    fn store(dir: &tempfile::TempDir) -> StateStore {
        StateStore::open(dir.path().join("locks.db")).unwrap()
    }

    /// The `Lock` log for lock 7, made in block `block`.
    fn lock_log(block: u64) -> Value {
        let event = Lock {
            sender: Address::repeat_byte(0x11),
            amount: U256::from(10).pow(U256::from(18)),
            holochainAgent: B256::repeat_byte(0x22),
            lockId: U256::from(7),
        };
        serde_json::to_value(Log {
            inner: alloy::primitives::Log {
                address: vault(),
                data: event.encode_log_data(),
            },
            block_number: Some(block),
            transaction_hash: Some(B256::repeat_byte(0x33)),
            ..Default::default()
        })
        .unwrap()
    }

    fn block(number: u64) -> Value {
        let mut block = alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
        block.header.inner.number = number;
        serde_json::to_value(block).unwrap()
    }

    #[tokio::test]
    async fn a_stop_during_the_lock_read_keeps_the_windows_read_and_sends_nothing_more() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(&dir);
        db.set_checkpoint_u64(LOCK_CHECKPOINT_KEY, 0).unwrap();
        let (stop, stopped) = watch::channel(false);
        let sent = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = Arc::clone(&sent);
        let url = serve(move |method, _| {
            let mut seen = seen.lock().unwrap();
            seen.push(method.to_string());
            let windows = seen.iter().filter(|m| *m == "eth_getLogs").count();
            match method {
                "eth_blockNumber" => Ok(json!("0x19")),
                "eth_getLogs" if windows == 1 => Ok(json!([lock_log(5)])),
                "eth_getLogs" => {
                    stop.send_replace(true);
                    Ok(json!([]))
                }
                "eth_getBlockByNumber" => Ok(block(5)),
                other => Err(json!({ "code": -32601, "message": format!("no {other}") })),
            }
        })
        .await;

        let e = lock_flow(url, db.clone(), stopped)
            .run_cycle()
            .await
            .expect_err("a stop ends the read before its third window");

        assert!(is_stopped(&e), "{e:#}");
        assert_eq!(
            *sent.lock().unwrap(),
            [
                "eth_blockNumber",
                "eth_getLogs",
                "eth_getBlockByNumber",
                "eth_getLogs"
            ]
        );
        assert_eq!(
            db.get_checkpoint_u64(LOCK_CHECKPOINT_KEY).unwrap(),
            Some(20)
        );
        let detected = db.list_work_items("lock", WorkState::Detected, 10).unwrap();
        assert_eq!(
            detected
                .iter()
                .map(|row| row.item_id.as_str())
                .collect::<Vec<_>>(),
            ["lock:7"]
        );
    }

    #[tokio::test]
    async fn a_request_to_an_rpc_that_never_answers_ends_within_its_bound_stop_or_not() {
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", silent.local_addr().unwrap());
        let accepted = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&accepted);
        let held = tokio::spawn(async move {
            let mut open = Vec::new();
            while let Ok((socket, _)) = silent.accept().await {
                counted.fetch_add(1, Ordering::SeqCst);
                open.push(socket);
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let (stop, stopped) = watch::channel(false);
        let mut flow = lock_flow(url, store(&dir), stopped);
        assert_eq!(flow.rpc_timeout, Duration::from_secs(30));
        flow.rpc_timeout = Duration::from_millis(200);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            stop.send_replace(true);
        });

        let e = tokio::time::timeout(Duration::from_secs(10), flow.run_cycle())
            .await
            .expect("the lock read waited on a silent RPC with no end")
            .expect_err("a silent RPC answered");
        held.abort();

        assert_eq!(
            format!("{e:#}"),
            "SEPOLIA_RPC_URL did not answer eth_blockNumber within 200ms"
        );
        assert_eq!(accepted.load(Ordering::SeqCst), 1, "no request after it");
    }
}
