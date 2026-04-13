mod config;
mod ham;
mod lock_flow;
mod orchestrator;
mod signer;
mod state;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::Config;
use orchestrator::BridgeOrchestrator;
use state::{StateFilter, WorkState};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run lock detection and periodic bridge cycle with single-writer execution.
    Run,
    /// Inspect orchestrator state from SQLite.
    Status {
        #[arg(long)]
        flow: Option<String>,
        #[arg(long)]
        state: Option<WorkState>,
        #[arg(long)]
        item_id: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Clear orchestrator work items from SQLite.
    Clear {
        /// Delete only rows that are not in progress (succeeded, failed).
        #[arg(
            long,
            conflicts_with = "all",
            required_unless_present = "all"
        )]
        non_in_progress: bool,
        /// Delete all rows from work_items.
        #[arg(
            long,
            conflicts_with = "non_in_progress",
            required_unless_present = "non_in_progress"
        )]
        all: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    init_logging();

    let args = Args::parse();
    let config = Config::from_env()?;

    match args.command {
        Command::Run => {
            info!("bridge-orchestrator starting");
            BridgeOrchestrator::new(config)?.run().await?;
        }
        Command::Status {
            flow,
            state,
            item_id,
            limit,
        } => {
            let db = state::StateStore::open(&config.db_path)?;
            let rows = db.status(StateFilter {
                flow,
                state,
                item_id,
                limit,
            })?;
            for row in rows {
                println!("{}", serde_json::to_string(&row)?);
            }
        }
        Command::Clear {
            non_in_progress,
            all,
        } => {
            let db = state::StateStore::open(&config.db_path)?;
            let (mode, deleted_count) = if non_in_progress {
                ("non_in_progress", db.clear_non_in_progress()?)
            } else if all {
                ("all", db.clear_all()?)
            } else {
                unreachable!("clap enforces one clear mode flag")
            };
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "mode": mode,
                    "deleted_count": deleted_count
                }))?
            );
        }
    }

    Ok(())
}

fn init_logging() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_ansi(false)
        .with_target(false)
        .init();
}
