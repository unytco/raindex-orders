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
use screening::{Action, Screener, Transfer, Verdict};
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
        /// Delete all rows from work_items.
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
    Release {
        /// lock:<lockId> or withdrawal:<action hash>.
        transfer: Transfer,
        #[arg(long, value_parser = NonEmptyStringValueParser::new())]
        by: String,
        #[arg(long, value_parser = NonEmptyStringValueParser::new())]
        reason: String,
    },
    /// Block a held transfer, or one with no verdict: it is never credited or paid.
    Block {
        /// lock:<lockId> or withdrawal:<action hash>.
        transfer: Transfer,
        #[arg(long, value_parser = NonEmptyStringValueParser::new())]
        by: String,
        #[arg(long, value_parser = NonEmptyStringValueParser::new())]
        reason: String,
    },
    /// Print each screened transfer with its state and records, as JSON lines.
    Screenings {
        /// lock:<lockId> or withdrawal:<action hash>.
        #[arg(long)]
        transfer: Option<Transfer>,
        /// Only the transfers that wait: held, or with no verdict.
        #[arg(long)]
        open: bool,
    },
    /// Screen one address as `run` would, recording nothing.
    Screen { address: Address },
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
                    let screener = config::screening_from_env(chain.network)?
                        .map(|list| Box::new(list) as Box<dyn Screener>);
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
        Command::Release {
            transfer,
            by,
            reason,
        } => act(&config, &transfer, Action::Release, &by, &reason)?,
        Command::Block {
            transfer,
            by,
            reason,
        } => act(&config, &transfer, Action::Block, &by, &reason)?,
        Command::Screenings { transfer, open } => {
            let db = StateStore::open(&config.db_path)?;
            for transfer in db.screenings(transfer.as_ref(), open)? {
                println!("{}", serde_json::to_string(&transfer)?);
            }
        }
        Command::Screen { address } => {
            let Some(chain) = ethereum else {
                anyhow::bail!("NETWORK=none ignores SCREENING, so no provider screens");
            };
            let Some(screener) = config::screening_from_env(chain.network)? else {
                anyhow::bail!("SCREENING=off, so no provider screens");
            };
            let screened = screener.screen(&[address]).await;
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
                    "list_version": screened.list.as_ref().map(|list| &list.sha256),
                    "at": screened.at,
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

fn act(config: &Config, transfer: &Transfer, action: Action, by: &str, reason: &str) -> Result<()> {
    let db = StateStore::open(&config.db_path)?;
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

    #[test]
    fn status_takes_each_screening_state() {
        for (word, state) in [
            ("screening", WorkState::Screening),
            ("held", WorkState::Held),
            ("blocked", WorkState::Blocked),
        ] {
            let args =
                Args::try_parse_from(["bridge-orchestrator", "status", "--state", word]).unwrap();
            assert!(
                matches!(args.command, Command::Status { state: Some(ref parsed), .. } if *parsed == state),
                "{word}"
            );
        }
    }

    #[test]
    fn release_and_block_name_who_acted_and_why() {
        for command in ["release", "block"] {
            let full = [command, "lock:7", "--by", "Ana", "--reason", "a false hit"];
            assert!(Args::try_parse_from(["bridge-orchestrator"].iter().chain(&full)).is_ok());
            for missing in ["--by", "--reason"] {
                let at = full.iter().position(|arg| *arg == missing).unwrap();
                let without: Vec<&str> = full
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != at && *i != at + 1)
                    .map(|(_, arg)| *arg)
                    .collect();
                assert!(
                    Args::try_parse_from(["bridge-orchestrator"].iter().chain(&without)).is_err(),
                    "{command} without {missing}"
                );
            }
            assert!(
                Args::try_parse_from([
                    "bridge-orchestrator",
                    command,
                    "7",
                    "--by",
                    "Ana",
                    "--reason",
                    "x"
                ])
                .is_err(),
                "an ID names its kind of transfer"
            );
        }
    }
}
