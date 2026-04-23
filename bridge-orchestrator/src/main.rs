mod config;
mod lock_flow;
mod orchestrator;
mod retention;
mod signer;
mod state;
mod watchtower_reporter;

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
        /// Only with `--non-in-progress`: delete terminal rows whose
        /// `updated_at` is older than this many seconds. Applied to
        /// both `succeeded` and `failed` rows. When omitted the flag
        /// behaves like the previous (unbounded) `--non-in-progress`.
        #[arg(long, conflicts_with = "all")]
        older_than_s: Option<u64>,
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
            older_than_s,
        } => {
            let db = state::StateStore::open(&config.db_path)?;
            let output = if all {
                let deleted = db.clear_all()?;
                serde_json::json!({
                    "mode": "all",
                    "deleted_count": deleted,
                })
            } else if non_in_progress {
                if let Some(age_s) = older_than_s {
                    // Apply the same window to both state buckets so
                    // this flag is a single knob; callers who want
                    // different succeeded/failed windows should rely
                    // on the in-process retention task instead.
                    let stats = db.prune_terminal_older_than(age_s, age_s)?;
                    serde_json::json!({
                        "mode": "non_in_progress_older_than",
                        "older_than_s": age_s,
                        "succeeded_deleted": stats.succeeded_deleted,
                        "failed_deleted": stats.failed_deleted,
                        "deleted_count": stats.total(),
                    })
                } else {
                    let deleted = db.clear_non_in_progress()?;
                    serde_json::json!({
                        "mode": "non_in_progress",
                        "deleted_count": deleted,
                    })
                }
            } else {
                unreachable!("clap enforces one clear mode flag")
            };
            println!("{}", serde_json::to_string(&output)?);
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
