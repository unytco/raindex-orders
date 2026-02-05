mod signer;
mod withdrawer;

use anyhow::Result;
use clap::Parser;
use std::env;

/// CLI tool for signing HoloFuel claim coupons for Raindex
///
/// This generates SignedContext data that can be used with the holo-claim.rain order.
///
/// Configuration can be set via environment variables or CLI arguments.
/// CLI arguments take precedence over environment variables.
///
/// Environment variables:
///   SIGNER_PRIVATE_KEY - Required. The private key for signing coupons.
///   ORDER_HASH         - The deployed order hash
///   ORDER_OWNER        - The order owner address
///   ORDERBOOK_ADDRESS  - The orderbook contract address
///   TOKEN_ADDRESS      - The output token address (HOT/TROT)
///   VAULT_ID           - The output vault ID
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Amount of HOT to claim (use decimal like "1.5" for 1.5 HOT, or wei amount)
    #[arg(short, long)]
    amount: String,

    /// Recipient Ethereum address that can claim the HOT
    #[arg(short, long)]
    recipient: String,

    /// Expiry time in seconds from now (default: 1 week)
    #[arg(short, long, default_value = "604800")]
    expiry_seconds: u64,

    /// Order hash (from the deployed Raindex order). Falls back to ORDER_HASH env var.
    #[arg(long, env = "ORDER_HASH")]
    order_hash: String,

    /// Order owner address. Falls back to ORDER_OWNER env var.
    #[arg(long, env = "ORDER_OWNER")]
    order_owner: String,

    /// Orderbook address. Falls back to ORDERBOOK_ADDRESS env var.
    #[arg(long, env = "ORDERBOOK_ADDRESS")]
    orderbook: String,

    /// Output token address (HOT or TROT). Falls back to TOKEN_ADDRESS env var.
    #[arg(long, env = "TOKEN_ADDRESS")]
    token: String,

    /// Output vault ID. Falls back to VAULT_ID env var.
    #[arg(long, env = "VAULT_ID")]
    vault_id: String,

    /// Nonce (unique per coupon, defaults to timestamp)
    #[arg(short, long)]
    nonce: Option<u64>,

    /// Output format: json, compact, hex, or ui (for the bridge UI)
    #[arg(short, long, default_value = "ui")]
    output: String,
}

fn main() -> Result<()> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    // Branch: `coupon-signer withdrawer` runs the withdrawer command without touching sign logic.
    let args_vec: Vec<String> = env::args().collect();
    if args_vec.get(1).map(|s| s.as_str()) == Some("withdrawer") {
        let prog = args_vec[0].clone();
        let rest: Vec<String> = args_vec.into_iter().skip(2).collect();
        let withdrawer_args =
            withdrawer::WithdrawerArgs::parse_from(std::iter::once(prog).chain(rest));
        return withdrawer::run_withdrawer(withdrawer_args);
    }
    let args = Args::parse();
    let ctx = signer::SignerContext::from_args(&args);
    let (coupon, _) = signer::generate_coupon_with_context(&args.amount, &args.recipient, &ctx)?;

    // Print helpful info to stderr
    eprintln!();
    eprintln!("Coupon created successfully!");
    eprintln!("Signer: {}", coupon.signed_context.signer);
    eprintln!("Recipient: {}", coupon.recipient);
    eprintln!("Amount: {} ({} wei)", coupon.amount, coupon.amount_wei);
    eprintln!(
        "Expiry: {} ({}s from now)",
        coupon.expiry, args.expiry_seconds
    );
    eprintln!("Nonce: {}", coupon.nonce);

    if args.output == "ui" || args.output.is_empty() {
        eprintln!();
        eprintln!("Copy the line above and paste it into the bridge UI claim page.");
    }

    Ok(())
}
