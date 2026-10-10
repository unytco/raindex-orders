use crate::config::Ethereum;
use crate::preflight::{rpc_failure, RPC_TIMEOUT};
use crate::state::{StateStore, LOCK_CHECKPOINT_KEY};
use crate::stop::ensure_running;
use alloy::primitives::U256;
use alloy::providers::{Provider, ProviderBuilder, RootProvider};
use alloy::rpc::types::{BlockTransactionsKind, Filter, Log};
use alloy::sol;
use alloy::sol_types::SolEvent;
use alloy::transports::http::{Client, Http};
use alloy::transports::TransportResult;
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
        let confirmed = self
            .request("eth_blockNumber", || provider.get_block_number())
            .await?
            .saturating_sub(self.cfg.confirmations);
        let Some(read_to) = self.db.get_checkpoint_u64(LOCK_CHECKPOINT_KEY)? else {
            self.db.set_checkpoint_u64(LOCK_CHECKPOINT_KEY, confirmed)?;
            return Ok(());
        };
        let mut cursor = read_to + 1;
        while cursor <= confirmed {
            let end = (cursor + MAX_BLOCK_RANGE - 1).min(confirmed);
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
        Ok(())
    }

    fn row_key(&self, lock_id: &str) -> String {
        format!(
            "lock:{:#x}:{lock_id}:create_parked_link",
            self.cfg.lock_vault_address
        )
    }

    fn provider(&self) -> Result<RootProvider<Http<Client>>> {
        Ok(ProviderBuilder::new().on_http(self.cfg.rpc_url.parse()?))
    }

    async fn request<T, F>(&self, method: &str, send: impl FnOnce() -> F) -> Result<T>
    where
        F: IntoFuture<Output = TransportResult<T>>,
    {
        ensure_running(&self.stop)?;
        let rpc = self.cfg.network.rpc_url_var();
        tokio::time::timeout(self.rpc_timeout, send())
            .await
            .map_err(|_| {
                anyhow!(
                    "{rpc} did not answer {method} within {:?}",
                    self.rpc_timeout
                )
            })?
            .map_err(|e| rpc_failure(rpc, method, &e))
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
        let idempotency_key = self.row_key(&data.lockId.to_string());
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
        self.db.enqueue_queued(
            "lock",
            "create_parked_link",
            &item_id,
            &idempotency_key,
            &payload,
        )?;
        info!(
            "[lock-flow] lock queued id={} amount={} agent={} tx={} block={}",
            item_id,
            payload["amount_hot"].as_str().unwrap_or("0"),
            payload["holochain_agent"].as_str().unwrap_or("unknown"),
            payload["tx_hash"].as_str().unwrap_or("unknown"),
            block_number
        );
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
    use crate::config::{ethereum_settings, test_settings, Network};
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
        lock_flow_on(Network::Sepolia, rpc_url, db, stop)
    }

    fn lock_flow_on(
        network: Network,
        rpc_url: String,
        db: StateStore,
        stop: ShutdownRx,
    ) -> LockFlow {
        let vault = format!("{:#x}", vault());
        let chain = ethereum_settings(test_settings(&[
            ("NETWORK", network.name()),
            (network.rpc_url_var(), &rpc_url),
            (network.lock_vault_var(), &vault),
        ]))
        .unwrap()
        .unwrap();
        LockFlow::new(chain, db, stop)
    }

    fn store(dir: &tempfile::TempDir) -> StateStore {
        StateStore::open(dir.path().join("locks.db")).unwrap()
    }

    /// The `Lock` log for lock `id`, made in block `block`.
    fn lock_log(id: u64, block: u64) -> Value {
        let event = Lock {
            sender: Address::repeat_byte(0x11),
            amount: U256::from(10).pow(U256::from(18)),
            holochainAgent: B256::repeat_byte(0x22),
            lockId: U256::from(id),
        };
        serde_json::to_value(Log {
            inner: alloy::primitives::Log {
                address: vault(),
                data: event.encode_log_data(),
            },
            block_number: Some(block),
            transaction_hash: Some(B256::with_last_byte(id as u8)),
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
                "eth_blockNumber" => Ok(json!("0x1e")),
                "eth_getLogs" if windows == 1 => Ok(json!([lock_log(7, 5)])),
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
        let queued = db.list_work_items("lock", WorkState::Queued, 10).unwrap();
        assert_eq!(
            queued
                .iter()
                .map(|row| row.item_id.as_str())
                .collect::<Vec<_>>(),
            ["lock:7"]
        );
    }

    type Windows = Arc<Mutex<Vec<(u64, u64)>>>;

    /// An RPC for a chain with its head at `head` and each `(lock id, block)` of
    /// `locks`, and the block range of each `eth_getLogs` it answered.
    async fn chain(head: u64, locks: &[(u64, u64)]) -> (String, Windows) {
        let locks = locks.to_vec();
        let windows = Windows::default();
        let asked = Arc::clone(&windows);
        let url = serve(move |method, params| match method {
            "eth_blockNumber" => Ok(json!(format!("{head:#x}"))),
            "eth_getLogs" => {
                let range = quantity(&params[0]["fromBlock"])..=quantity(&params[0]["toBlock"]);
                asked.lock().unwrap().push((*range.start(), *range.end()));
                Ok(locks
                    .iter()
                    .filter(|(_, block)| range.contains(block))
                    .map(|&(id, block)| lock_log(id, block))
                    .collect())
            }
            "eth_getBlockByNumber" => Ok(block(quantity(&params[0]))),
            other => Err(json!({ "code": -32601, "message": format!("no {other}") })),
        })
        .await;
        (url, windows)
    }

    fn quantity(hex: &Value) -> u64 {
        u64::from_str_radix(hex.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
    }

    fn lock_rows(db: &StateStore) -> Vec<crate::state::WorkItem> {
        [WorkState::Detected, WorkState::Queued]
            .into_iter()
            .flat_map(|state| db.list_work_items("lock", state, 10).unwrap())
            .collect()
    }

    fn lock_ids(db: &StateStore) -> Vec<String> {
        db.list_work_items("lock", WorkState::Queued, 10)
            .unwrap()
            .into_iter()
            .map(|row| row.item_id)
            .collect()
    }

    #[tokio::test]
    async fn the_read_goes_no_further_than_five_blocks_below_the_head_on_both_networks() {
        for network in [Network::Sepolia, Network::Mainnet] {
            let dir = tempfile::tempdir().unwrap();
            let db = store(&dir);
            db.set_checkpoint_u64(LOCK_CHECKPOINT_KEY, 80).unwrap();
            let (url, windows) = chain(100, &[(7, 95), (8, 96)]).await;

            lock_flow_on(network, url, db.clone(), watch::channel(false).1)
                .run_cycle()
                .await
                .unwrap();

            assert_eq!(
                *windows.lock().unwrap(),
                [(81, 90), (91, 95)],
                "{network:?}"
            );
            assert_eq!(
                db.get_checkpoint_u64(LOCK_CHECKPOINT_KEY).unwrap(),
                Some(95)
            );
            assert_eq!(lock_ids(&db), ["lock:7"], "{network:?}");
        }
    }

    #[tokio::test]
    async fn a_lock_is_recorded_queued_once_its_block_has_its_confirmations_and_not_before() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(&dir);
        db.set_checkpoint_u64(LOCK_CHECKPOINT_KEY, 0).unwrap();
        for (head, recorded) in [(9, vec![]), (10, vec!["lock:7"])] {
            let (url, _) = chain(head, &[(7, 5)]).await;
            lock_flow(url, db.clone(), watch::channel(false).1)
                .run_cycle()
                .await
                .unwrap();

            assert_eq!(lock_rows(&db).len(), recorded.len(), "head {head}");
            assert_eq!(lock_ids(&db), recorded, "head {head}");
        }
    }

    #[tokio::test]
    async fn an_empty_database_starts_at_the_newest_confirmed_block_on_both_networks() {
        for network in [Network::Sepolia, Network::Mainnet] {
            let dir = tempfile::tempdir().unwrap();
            let db = store(&dir);
            let (url, windows) = chain(100, &[(6, 95)]).await;
            lock_flow_on(network, url, db.clone(), watch::channel(false).1)
                .run_cycle()
                .await
                .unwrap();
            assert_eq!(
                db.get_checkpoint_u64(LOCK_CHECKPOINT_KEY).unwrap(),
                Some(95)
            );
            assert!(windows.lock().unwrap().is_empty(), "{network:?}");

            let (url, _) = chain(101, &[(6, 95), (7, 96)]).await;
            lock_flow_on(network, url, db.clone(), watch::channel(false).1)
                .run_cycle()
                .await
                .unwrap();
            assert_eq!(lock_ids(&db), ["lock:7"], "{network:?}");
        }
    }

    #[tokio::test]
    async fn locks_an_earlier_binary_left_detected_are_read_again_from_the_chain() {
        for version in [1, 2] {
            let dir = tempfile::tempdir().unwrap();
            let db = store(&dir);
            db.set_checkpoint_u64(LOCK_CHECKPOINT_KEY, 100).unwrap();
            let earlier = rusqlite::Connection::open(dir.path().join("locks.db")).unwrap();
            for (id, block) in [(7, 90), (8, 92)] {
                earlier
                    .execute(
                        "INSERT INTO work_items (flow, task_type, item_id, idempotency_key, payload_json, state)
                         VALUES ('lock', 'create_parked_link', ?1, ?2, ?3, 'detected')",
                        rusqlite::params![
                            format!("lock:{id}"),
                            format!("lock:{id}:create_parked_link"),
                            json!({ "lock_id": id.to_string(), "block_number": block }).to_string()
                        ],
                    )
                    .unwrap();
            }
            earlier
                .execute("UPDATE schema_meta SET version = ?1", [version])
                .unwrap();
            drop(db);

            let db = store(&dir);
            assert!(lock_rows(&db).is_empty(), "version {version}");
            assert_eq!(
                db.get_checkpoint_u64(LOCK_CHECKPOINT_KEY).unwrap(),
                Some(89),
                "version {version}"
            );

            let (url, _) = chain(100, &[(7, 90)]).await;
            lock_flow(url, db.clone(), watch::channel(false).1)
                .run_cycle()
                .await
                .unwrap();
            assert_eq!(lock_ids(&db), ["lock:7"], "version {version}");
        }
    }

    #[tokio::test]
    async fn a_lock_read_twice_is_one_row_keyed_by_its_vault() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(&dir);
        let flow = lock_flow(
            chain(10, &[(7, 5)]).await.0,
            db.clone(),
            watch::channel(false).1,
        );

        for _ in 0..2 {
            db.set_checkpoint_u64(LOCK_CHECKPOINT_KEY, 0).unwrap();
            flow.run_cycle().await.unwrap();
        }

        let rows = lock_rows(&db);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].item_id, "lock:7");
        assert_eq!(
            rows[0].idempotency_key,
            format!("lock:{:#x}:7:create_parked_link", vault())
        );
    }

    #[tokio::test]
    async fn a_lock_an_earlier_binary_recorded_is_not_recorded_again_once_bound() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(&dir);
        rusqlite::Connection::open(dir.path().join("locks.db"))
            .unwrap()
            .execute(
                "INSERT INTO work_items (flow, task_type, item_id, idempotency_key, payload_json, state)
                 VALUES ('lock', 'create_parked_link', 'lock:7', 'lock:7:create_parked_link', '{}', 'queued')",
                [],
            )
            .unwrap();

        db.bind_vault(&format!("{:#x}", vault())).unwrap();
        db.set_checkpoint_u64(LOCK_CHECKPOINT_KEY, 0).unwrap();
        lock_flow(
            chain(10, &[(7, 5)]).await.0,
            db.clone(),
            watch::channel(false).1,
        )
        .run_cycle()
        .await
        .unwrap();

        let rows = lock_rows(&db);
        assert_eq!(
            rows.len(),
            1,
            "the lock already recorded is not recorded again"
        );
        assert_eq!(
            rows[0].idempotency_key,
            format!("lock:{:#x}:7:create_parked_link", vault())
        );
    }

    #[tokio::test]
    async fn a_failed_request_names_its_variable_and_cause_but_not_the_rpc_key() {
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v3/secret-api-key", closed.local_addr().unwrap());
        drop(closed);
        let dir = tempfile::tempdir().unwrap();

        let e = lock_flow(url, store(&dir), watch::channel(false).1)
            .run_cycle()
            .await
            .expect_err("a closed port answered");

        let message = format!("{e:#}");
        assert!(
            message.starts_with("SEPOLIA_RPC_URL: eth_blockNumber failed: "),
            "{message}"
        );
        assert!(!message.contains("secret-api-key"), "{message}");
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
