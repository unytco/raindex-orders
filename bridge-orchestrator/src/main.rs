mod config;
#[cfg(test)]
mod fake_rpc;
mod lock_flow;
mod orchestrator;
mod preflight;
mod retention;
mod screening;
mod signer;
mod state;
mod stop;
mod watchtower_reporter;

use alloy::primitives::Address;
use anyhow::Result;
use clap::builder::NonEmptyStringValueParser;
use clap::{Parser, Subcommand};
use config::Config;
use orchestrator::{BridgeOrchestrator, EthereumSide};
use screening::{Action, Transfer, Verdict};
use signer::CouponSigner;
use state::{StateFilter, StateStore, WorkState};
use tracing::{info, warn};
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
        #[arg(long, conflicts_with = "all", required_unless_present = "all")]
        non_in_progress: bool,
        /// Delete every row from work_items but those in screening, held or blocked.
        #[arg(
            long,
            conflicts_with = "non_in_progress",
            required_unless_present = "non_in_progress"
        )]
        all: bool,
        /// Only with `--non-in-progress`: delete terminal rows whose
        /// `updated_at` is older than this many seconds. Applied to
        /// both `succeeded` and `failed` rows.
        #[arg(long, conflicts_with = "all")]
        older_than_s: Option<u64>,
    },
    /// List each transfer the bridging agent's network still holds, for a migration's close.
    /// Run it with the orchestrator stopped.
    InTransit {
        /// After listing, mark failed each pending row in transit or carried by a listed link, all
        /// or none, to be paid by hand on the new network. Exits 0 once they are marked.
        #[arg(long)]
        mark_failed: bool,
    },
    /// Release a held transfer: a held lock is credited, and a held withdrawal gets its coupon.
    Release(Decision),
    /// Block a held transfer, or one not yet held: it is never credited or given a coupon.
    Block(Decision),
    /// Print each transfer that is held, blocked or in screening, as JSON lines.
    Screenings {
        /// Only the transfers that wait: held, or a lock in screening.
        #[arg(long)]
        open: bool,
    },
    /// Screen one address as `run` would, recording nothing.
    Screen { address: Address },
}

#[derive(clap::Args, Debug)]
struct Decision {
    /// lock:<lockId> or withdrawal:<action hash>.
    transfer: Transfer,
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    by: String,
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    reason: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    init_logging();

    let args = Args::parse();
    let config = Config::from_env()?;
    let ethereum = config::ethereum_from_env()?;

    match args.command {
        Command::Run => {
            info!("bridge-orchestrator starting");
            config::require_durable_conductor(&config.conductor_config)?;
            let ethereum = match ethereum {
                Some(chain) => {
                    let screener = config::screener_from_env(chain.network)?;
                    let signer = CouponSigner::from_env(chain.network)?;
                    preflight::check(
                        chain.network,
                        &chain.rpc_url,
                        chain.lock_vault_address,
                        &signer,
                    )
                    .await?;
                    info!(
                        event = "startup.checks_passed",
                        order = %signer.order().order_hash,
                        signer = %signer.order().signer,
                        "startup checks passed: the chain, vault and claim order match, and the order accepts the signer"
                    );
                    Some(EthereumSide {
                        chain,
                        signer,
                        screener,
                    })
                }
                None => {
                    info!(
                        event = "startup.ethereum_off",
                        "NETWORK=none: Ethereum is off. No chain is checked, no lock is watched and no coupon is signed, so withdrawals stay parked"
                    );
                    let ignored = config::ignored_without_ethereum(|key| std::env::var(key).ok());
                    if !ignored.is_empty() {
                        info!(
                            event = "startup.ethereum_variables_ignored",
                            "NETWORK=none ignores {}",
                            ignored.join(", ")
                        );
                    }
                    None
                }
            };
            BridgeOrchestrator::new(config, ethereum)?.run().await?;
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
        Command::InTransit { mark_failed } => {
            orchestrator::in_transit::run(config, ethereum, mark_failed).await?;
        }
        Command::Release(decision) => act(&config, &decision, Action::Release)?,
        Command::Block(decision) => act(&config, &decision, Action::Block)?,
        Command::Screenings { open } => {
            let db = StateStore::open_existing(&config.db_path)?;
            for transfer in db.screenings(open)? {
                println!("{}", serde_json::to_string(&transfer)?);
            }
        }
        Command::Screen { address } => {
            let Some(chain) = ethereum else {
                anyhow::bail!("NETWORK=none ignores SCREENING, so no provider screens");
            };
            let Some(screener) = config::screener_from_env(chain.network)? else {
                anyhow::bail!("SCREENING=off, so no provider screens");
            };
            let screened = screening::screen(screener.as_ref(), &[address]).await;
            let cause = match &screened.verdict {
                Verdict::NoAnswer(cause) => Some(cause.clone()),
                _ => None,
            };
            println!(
                "{}",
                serde_json::json!({
                    "address": format!("{address:#x}"),
                    "provider": screened.provider,
                    "verdict": screened.verdict.name(),
                    "cause": cause,
                })
            );
            if let Some(cause) = cause {
                anyhow::bail!("{} did not answer: {cause}", screened.provider);
            }
        }
    }

    Ok(())
}

fn act(config: &Config, decision: &Decision, action: Action) -> Result<()> {
    let Decision {
        transfer,
        by,
        reason,
    } = decision;
    let db = StateStore::open_existing(&config.db_path)?;
    let acted = db.act(transfer, action, by, reason)?;
    warn!(
        event = format!("bridge.screening.{}", action.event()),
        transfer = %transfer,
        by,
        reason,
        "[screening] {transfer} {} by {by}: {reason}. It was {}, and is {}",
        action.event(),
        acted.was,
        acted.now
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_still_takes_the_detected_state() {
        let args =
            Args::try_parse_from(["bridge-orchestrator", "status", "--state", "detected"]).unwrap();
        assert!(matches!(
            args.command,
            Command::Status {
                state: Some(WorkState::Detected),
                ..
            }
        ));
    }
}
