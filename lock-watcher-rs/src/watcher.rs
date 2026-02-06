use crate::config::Config;
use crate::database::LockDatabase;
use crate::ham::Ham;
use crate::holochain_bridge;
use crate::types::{LockRecord, LockStatus};
use alloy::primitives::U256;
use alloy::providers::{Provider, ProviderBuilder, RootProvider};
use alloy::rpc::types::{BlockTransactionsKind, Filter, Log};
use alloy::sol;
use alloy::sol_types::SolEvent;
use alloy::transports::http::{Client, Http};
use anyhow::{Context, Result};
use holo_hash::{ActionHash, ActionHashB64};
use tracing::{debug, error, info, warn};

// Define the Lock event using alloy's sol! macro
sol! {
    #[derive(Debug)]
    event Lock(
        address indexed sender,
        uint256 amount,
        bytes32 indexed holochainAgent,
        uint256 lockId
    );
}

/// Lock event watcher
pub struct LockWatcher {
    config: Config,
    db: LockDatabase,
}

impl LockWatcher {
    /// Create a new lock watcher
    pub fn new(config: Config, db: LockDatabase) -> Self {
        Self { config, db }
    }

    /// Create an HTTP provider
    fn create_provider(&self) -> Result<RootProvider<Http<Client>>> {
        let provider = ProviderBuilder::new().on_http(self.config.network_config.rpc_url.parse()?);
        Ok(provider)
    }

    /// Get the current block number
    pub async fn get_current_block_number(&self) -> Result<u64> {
        let provider = self.create_provider()?;
        let block_number = provider.get_block_number().await?;
        Ok(block_number)
    }

    /// Fetch historical Lock events from a range of blocks
    pub async fn fetch_historical_locks(
        &self,
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<LockRecord>> {
        info!(
            "Fetching locks from block {} to {}...",
            from_block, to_block
        );

        let provider = self.create_provider()?;

        let filter = Filter::new()
            .address(self.config.network_config.lock_vault_address)
            .event_signature(Lock::SIGNATURE_HASH)
            .from_block(from_block)
            .to_block(to_block);

        let logs = provider.get_logs(&filter).await?;

        if !logs.is_empty() {
            info!("Found {} Lock event(s)", logs.len());
        }

        let mut records = Vec::new();
        for log in logs {
            if let Some(record) = self.process_lock_log(&provider, log).await? {
                records.push(record);
            }
        }

        // Update last processed block
        self.db
            .set_last_processed_block(self.config.network_config.chain_id, to_block)?;

        Ok(records)
    }

    /// Process a single Lock event log
    async fn process_lock_log(
        &self,
        provider: &RootProvider<Http<Client>>,
        log: Log,
    ) -> Result<Option<LockRecord>> {
        // Decode the log
        let decoded = log
            .log_decode::<Lock>()
            .context("Failed to decode Lock event")?;

        let Lock {
            sender,
            amount,
            holochainAgent,
            lockId,
        } = decoded.inner.data;

        let tx_hash = log
            .transaction_hash
            .context("Log missing transaction hash")?;
        let block_number = log.block_number.context("Log missing block number")?;

        // Get block timestamp
        let block = provider
            .get_block_by_number(block_number.into(), BlockTransactionsKind::Hashes)
            .await?
            .context("Block not found")?;

        let timestamp = block.header.timestamp;

        let lock_record = LockRecord {
            id: 0,
            lock_id: lockId.to_string(),
            sender: format!("{:?}", sender),
            amount: amount.to_string(),
            holochain_agent: format!("0x{}", hex::encode(holochainAgent)),
            tx_hash: format!("0x{}", hex::encode(tx_hash)),
            block_number,
            timestamp,
            status: LockStatus::Pending,
            error_message: None,
            created_at: 0,
            updated_at: 0,
        };

        // Store in database
        match self.db.store_lock(&lock_record) {
            Ok(id) => {
                if id > 0 {
                    // Log the new lock event prominently
                    info!("========================================");
                    info!("NEW LOCK DETECTED!");
                    info!("========================================");
                    info!("  Lock ID:          {}", lock_record.lock_id);
                    info!("  Sender:           {}", lock_record.sender);
                    info!(
                        "  Amount:           {} HOT",
                        format_amount(&lock_record.amount)
                    );
                    info!("  Holochain Agent:  {}", lock_record.holochain_agent);
                    info!("  TX Hash:          {}", lock_record.tx_hash);
                    info!("  Block:            {}", lock_record.block_number);
                    info!("========================================");
                    info!("Lock will be sent to Holochain after confirmations");
                    info!("========================================");

                    Ok(Some(lock_record))
                } else {
                    debug!("Lock {} already processed", lock_record.lock_id);
                    Ok(None)
                }
            }
            Err(e) => {
                warn!("Failed to store lock {}: {}", lock_record.lock_id, e);
                Ok(None)
            }
        }
    }

    /// Check pending locks for confirmation and mark as confirmed
    pub async fn check_confirmations(&self) -> Result<()> {
        let current_block = self.get_current_block_number().await?;
        let required_confirmations = self.config.network_config.confirmations;

        let pending_locks = self.db.get_locks_by_status(LockStatus::Pending)?;

        for lock in pending_locks {
            let confirmations = current_block.saturating_sub(lock.block_number);

            if confirmations >= required_confirmations {
                info!(
                    "Lock {} confirmed ({} confirmations)",
                    lock.lock_id, confirmations
                );
                self.db
                    .update_lock_status(&lock.lock_id, LockStatus::Confirmed, None)?;
            }
        }

        Ok(())
    }

    /// Process confirmed locks: call Holochain create_parked_link via Ham, then mark Processed or Failed.
    async fn process_confirmed_locks(&self, ham: Option<&Ham>) -> Result<()> {
        let Some(ref hc) = self.config.holochain else {
            debug!("[process_confirmed_locks] No Holochain config present, skipping");
            return Ok(());
        };
        let Some(ham) = ham else {
            debug!("[process_confirmed_locks] No Holochain connection available, skipping");
            return Ok(());
        };

        let confirmed = self.db.get_locks_by_status(LockStatus::Confirmed)?;
        if confirmed.is_empty() {
            debug!("[process_confirmed_locks] No confirmed locks to process");
            return Ok(());
        }

        info!(
            "[process_confirmed_locks] Found {} confirmed lock(s) to process",
            confirmed.len()
        );

        let contract_hex = format!("{:x}", self.config.network_config.lock_vault_address);

        for (i, lock) in confirmed.iter().enumerate() {
            info!(
                "[process_confirmed_locks] [{}/{}] Processing lock {} (sender: {}, amount: {} HOT)",
                i + 1,
                confirmed.len(),
                lock.lock_id,
                lock.sender,
                format_amount(&lock.amount),
            );

            debug!(
                "[process_confirmed_locks] Building create_parked_link payload for lock {} with contract {}",
                lock.lock_id, contract_hex
            );

            let payload = match holochain_bridge::build_create_parked_link_payload(
                hc,
                lock,
                &contract_hex,
            ) {
                Ok(p) => {
                    info!(
                        "[process_confirmed_locks] Payload built successfully for lock {}",
                        lock.lock_id
                    );
                    p
                }
                Err(e) => {
                    let msg = e.to_string();
                    warn!(
                        "[process_confirmed_locks] Skipping zome call for lock {} (invalid payload): {}",
                        lock.lock_id, msg
                    );
                    self.db.update_lock_status(
                        &lock.lock_id,
                        LockStatus::Failed,
                        Some(msg.as_str()),
                    )?;
                    continue;
                }
            };

            info!(
                "[process_confirmed_locks] Calling zome: transactor/create_parked_link for lock {}",
                lock.lock_id
            );

            match ham
                .call_zome::<_, ActionHashB64>(
                    &hc.role_name,
                    "transactor",
                    "create_parked_link",
                    &payload,
                )
                .await
            {
                Ok(tx_hash) => {
                    info!(
                        "[process_confirmed_locks] Zome call committed successfully for lock {} — marking as Processed: {}",
                        lock.lock_id, tx_hash.to_string()
                    );
                    self.db
                        .update_lock_status(&lock.lock_id, LockStatus::Processed, None)?;
                    let payload =
                        holochain_bridge::build_bridging_agent_initiate_deposit_payload(hc);
                    if let Err(e) = ham
                        .call_zome::<_, String>(
                            &hc.role_name,
                            "transactor",
                            "blockchain_bridging_agent_initiate_deposit",
                            &payload,
                        )
                        .await
                    {
                        warn!("bridging_agent_initiate failed {}", e);
                    }
                    info!("bridging_agent_initiate committed successfully");
                }
                Err(e) => {
                    let msg = e.to_string();
                    warn!(
                        "[process_confirmed_locks] Zome call failed for lock {}: {} — marking as Failed",
                        lock.lock_id, msg
                    );
                    self.db.update_lock_status(
                        &lock.lock_id,
                        LockStatus::Failed,
                        Some(msg.as_str()),
                    )?;
                }
            }
        }

        info!("[process_confirmed_locks] Finished processing all confirmed locks");

        Ok(())
    }

    /// Run a single processing cycle
    pub async fn run_cycle(&self, ham: Option<&Ham>) -> Result<()> {
        let last_processed = self
            .db
            .get_last_processed_block(self.config.network_config.chain_id)?;
        let current_block = self.get_current_block_number().await?;

        match last_processed {
            Some(last) if current_block > last => {
                self.fetch_historical_locks(last + 1, current_block).await?;
            }
            None => {
                // First run - start from current block
                self.db
                    .set_last_processed_block(self.config.network_config.chain_id, current_block)?;
                info!("Initialized - watching from block {}", current_block);
            }
            _ => {}
        }

        // Check pending locks for confirmations
        self.check_confirmations().await?;

        // Process confirmed locks: call Holochain create_parked_link via Ham, update status
        self.process_confirmed_locks(ham).await?;

        Ok(())
    }

    /// Run the watcher continuously
    pub async fn run(&self) -> Result<()> {
        info!("========================================");
        info!("Lock Watcher Started");
        info!("========================================");
        info!("Network:       {:?}", self.config.network);
        info!(
            "Lock Vault:    {:?}",
            self.config.network_config.lock_vault_address
        );
        info!(
            "Confirmations: {}",
            self.config.network_config.confirmations
        );
        info!("Poll Interval: {}ms", self.config.poll_interval_ms);
        info!("========================================");

        // Connect to Holochain via Ham if config is present.
        let ham = match &self.config.holochain {
            Some(hc) => {
                info!(
                    "[run] Connecting to Holochain (admin port: {})...",
                    hc.admin_port
                );
                Some(
                    Ham::connect(hc.admin_port, hc.app_port, &hc.app_id)
                        .await
                        .context("Failed to connect Ham to Holochain")?,
                )
            }
            None => {
                info!("[run] No Holochain config — running without bridge");
                None
            }
        };

        info!("Watching for Lock events...");
        info!("");

        let poll_interval = std::time::Duration::from_millis(self.config.poll_interval_ms);

        loop {
            if let Err(e) = self.run_cycle(ham.as_ref()).await {
                error!("Error in processing cycle: {:?}", e);
            }

            tokio::time::sleep(poll_interval).await;
        }
    }
}

/// Format amount with 18 decimals for display
pub fn format_amount(amount: &str) -> String {
    let amount: U256 = amount.parse().unwrap_or_default();
    let decimals = U256::from(10).pow(U256::from(18));
    let whole = amount / decimals;
    let frac = (amount % decimals) / U256::from(10).pow(U256::from(14)); // 4 decimal places

    if frac.is_zero() {
        whole.to_string()
    } else {
        format!("{}.{:04}", whole, frac)
    }
}
