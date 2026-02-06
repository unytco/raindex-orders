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
///   ORDER_HASH, ORDER_OWNER, ORDERBOOK_ADDRESS, TOKEN_ADDRESS, VAULT_ID - Order/context (see also sign subcommand).
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct SignArgs {
    /// Amount of HOT to claim (use decimal like "1.5" for 1.5 HOT, or wei amount)
    #[arg(short, long)]
    amount: String,

    /// Recipient Ethereum address that can claim the HOT
    #[arg(short, long)]
    recipient: String,

    #[command(flatten)]
    context: signer::SignerContext,
}

fn main() -> Result<()> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    // Branch: `coupon-signer withdrawer` runs the withdrawer command. Check before parsing sign args.
    let args_vec: Vec<String> = env::args().collect();
    if args_vec.get(1).map(|s| s.as_str()) == Some("withdrawer") {
        let prog = args_vec[0].clone();
        let rest: Vec<String> = args_vec.into_iter().skip(2).collect();
        let withdrawer_args =
            withdrawer::WithdrawerArgs::parse_from(std::iter::once(prog).chain(rest));
        return withdrawer::run_withdrawer(withdrawer_args);
    }

    let args = SignArgs::parse();
    let (coupon, _) =
        signer::generate_coupon_with_context(&args.amount, &args.recipient, &args.context)?;

    // Print helpful info to stderr
    eprintln!();
    eprintln!("Coupon created successfully!");
    eprintln!("Signer: {}", coupon.signed_context.signer);
    eprintln!("Recipient: {}", coupon.recipient);
    eprintln!("Amount: {} ({} wei)", coupon.amount, coupon.amount_wei);
    eprintln!(
        "Expiry: {} ({}s from now)",
        coupon.expiry, args.context.expiry_seconds
    );
    eprintln!("Nonce: {}", coupon.nonce);

    if args.context.output == "ui" || args.context.output.is_empty() {
        eprintln!();
        eprintln!("Copy the line above and paste it into the bridge UI claim page.");
    }

    Ok(())
}
