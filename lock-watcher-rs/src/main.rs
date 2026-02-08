mod config;
mod database;
mod types;
mod watcher;

use crate::config::Config;
use crate::database::LockDatabase;
use crate::watcher::LockWatcher;
use anyhow::Result;
use tracing::info;
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    FmtSubscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .init();

    info!("Lock Watcher starting...");

    // Load configuration
    let config = Config::from_env()?;

    info!("Network: {:?}", config.network);
    info!("RPC URL: {}", config.network_config.rpc_url);
    info!("Database: {}", config.db_path);

    // Open database
    let db = LockDatabase::open(&config.db_path)?;
    info!("Database opened successfully");

    // Create watcher
    let watcher = LockWatcher::new(config, db);

    // Run the watcher
    watcher.run().await?;

    Ok(())
}
