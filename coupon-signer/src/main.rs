use alloy::primitives::{keccak256, Address, B256, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use anyhow::{Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// The signed context that Raindex expects
/// This matches the SignedContextV1 struct in rain.interpreter.interface
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedContext {
    /// The signer's address
    pub signer: String,
    /// The context values as hex strings (uint256[])
    pub context: Vec<String>,
    /// The signature (r || s || v as hex)
    pub signature: String,
}

/// Full coupon with metadata for easier use
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimCoupon {
    /// Human-readable fields
    pub recipient: String,
    pub amount: String,
    pub amount_wei: String,
    pub expiry: u64,
    pub nonce: u64,
    /// The signed context for Raindex
    pub signed_context: SignedContext,
}

fn main() -> Result<()> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    let args = Args::parse();

    // Load private key from environment
    let private_key = env::var("SIGNER_PRIVATE_KEY")
        .context("SIGNER_PRIVATE_KEY environment variable not set")?;

    // Parse the private key
    let signer: PrivateKeySigner = private_key
        .parse()
        .context("Invalid private key format")?;

    let signer_address = signer.address();

    // Parse all addresses and values
    let recipient: Address = args.recipient.parse().context("Invalid recipient address")?;
    let order_hash: B256 = args.order_hash.parse().context("Invalid order hash")?;
    let order_owner: Address = args.order_owner.parse().context("Invalid order owner address")?;
    let orderbook: Address = args.orderbook.parse().context("Invalid orderbook address")?;
    let token: Address = args.token.parse().context("Invalid token address")?;
    let vault_id: U256 = args.vault_id.parse().context("Invalid vault ID")?;

    // Parse amount
    let amount = parse_amount(&args.amount)?;

    // Calculate expiry timestamp
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let expiry = now + args.expiry_seconds;

    // Use provided nonce or generate from timestamp
    let nonce = args.nonce.unwrap_or(now);

    // Build the context array (matches holo-claim.rain expectations)
    // [0] recipient address
    // [1] amount
    // [2] expiry timestamp
    // [3] order hash
    // [4] order owner
    // [5] orderbook address
    // [6] token address
    // [7] output vault id
    // [8] nonce
    let context: Vec<U256> = vec![
        U256::from_be_slice(&{
            let mut padded = [0u8; 32];
            padded[12..].copy_from_slice(recipient.as_slice());
            padded
        }),
        amount,
        U256::from(expiry),
        U256::from_be_bytes(order_hash.0),
        U256::from_be_slice(&{
            let mut padded = [0u8; 32];
            padded[12..].copy_from_slice(order_owner.as_slice());
            padded
        }),
        U256::from_be_slice(&{
            let mut padded = [0u8; 32];
            padded[12..].copy_from_slice(orderbook.as_slice());
            padded
        }),
        U256::from_be_slice(&{
            let mut padded = [0u8; 32];
            padded[12..].copy_from_slice(token.as_slice());
            padded
        }),
        vault_id,
        U256::from(nonce),
    ];

    // Create the message hash (same as SignContext.sol)
    // keccak256(abi.encodePacked(context))
    let packed: Vec<u8> = context
        .iter()
        .flat_map(|v| v.to_be_bytes::<32>())
        .collect();
    let context_hash = keccak256(&packed);

    // Apply Ethereum signed message prefix (toEthSignedMessageHash)
    // "\x19Ethereum Signed Message:\n32" + hash
    let prefixed = keccak256(
        [
            b"\x19Ethereum Signed Message:\n32".as_slice(),
            context_hash.as_slice(),
        ]
        .concat(),
    );

    // Sign the prefixed hash
    let signature = tokio::runtime::Runtime::new()?
        .block_on(signer.sign_hash(&prefixed))?;

    // Encode signature as bytes (r || s || v) - same as SignContext.sol
    let sig_bytes = {
        let mut bytes = [0u8; 65];
        bytes[0..32].copy_from_slice(&signature.r().to_be_bytes::<32>());
        bytes[32..64].copy_from_slice(&signature.s().to_be_bytes::<32>());
        // v is 27 or 28 for ecrecover
        bytes[64] = if signature.v() { 28 } else { 27 };
        bytes
    };

    let signed_context = SignedContext {
        signer: format!("{:?}", signer_address),
        context: context.iter().map(|v| format!("0x{:064x}", v)).collect(),
        signature: format!("0x{}", hex::encode(sig_bytes)),
    };

    let coupon = ClaimCoupon {
        recipient: format!("{:?}", recipient),
        amount: args.amount.clone(),
        amount_wei: amount.to_string(),
        expiry,
        nonce,
        signed_context,
    };

    // Output the coupon
    match args.output.as_str() {
        "compact" => {
            // Compact: just the essential data
            println!("signer={}", coupon.signed_context.signer);
            println!("signature={}", coupon.signed_context.signature);
            for (i, ctx) in coupon.signed_context.context.iter().enumerate() {
                println!("context[{}]={}", i, ctx);
            }
        }
        "hex" => {
            // Raw hex for direct contract interaction
            println!("{}", coupon.signed_context.signature);
        }
        "json" => {
            // JSON format
            println!("{}", serde_json::to_string_pretty(&coupon)?);
        }
        "ui" | _ => {
            // UI format (default): signer,signature,context[0],context[1],...
            // Context values as decimal strings (what the UI deserializeSignedContext expects)
            let context_decimals: Vec<String> = context.iter().map(|v| v.to_string()).collect();
            println!(
                "{},{},{}",
                coupon.signed_context.signer,
                coupon.signed_context.signature,
                context_decimals.join(",")
            );
        }
    }

    // Print helpful info to stderr
    eprintln!();
    eprintln!("Coupon created successfully!");
    eprintln!("Signer: {}", coupon.signed_context.signer);
    eprintln!("Recipient: {}", coupon.recipient);
    eprintln!("Amount: {} ({} wei)", coupon.amount, coupon.amount_wei);
    eprintln!("Expiry: {} ({}s from now)", coupon.expiry, args.expiry_seconds);
    eprintln!("Nonce: {}", coupon.nonce);

    if args.output == "ui" || args.output.is_empty() {
        eprintln!();
        eprintln!("Copy the line above and paste it into the bridge UI claim page.");
    }

    Ok(())
}

/// Parse amount string, supporting decimal notation
fn parse_amount(amount_str: &str) -> Result<U256> {
    // Check if it's a decimal number
    if amount_str.contains('.') {
        let parts: Vec<&str> = amount_str.split('.').collect();
        if parts.len() != 2 {
            anyhow::bail!("Invalid amount format");
        }

        let whole: U256 = parts[0]
            .parse()
            .context("Invalid whole number part")?;

        let decimals_str = parts[1];
        let decimals_len = decimals_str.len();

        if decimals_len > 18 {
            anyhow::bail!("Too many decimal places (max 18)");
        }

        let frac: U256 = decimals_str
            .parse()
            .context("Invalid decimal part")?;

        // Scale whole part to wei (18 decimals)
        let scale = U256::from(10).pow(U256::from(18));
        let frac_scale = U256::from(10).pow(U256::from(18 - decimals_len));

        Ok(whole * scale + frac * frac_scale)
    } else {
        // Assume it's already in wei if no decimal point
        // But if it's a small number, assume it's in HOT
        let value: U256 = amount_str.parse().context("Invalid amount")?;

        // If the number is less than 1000, assume it's HOT and convert to wei
        if value < U256::from(1000) {
            let scale = U256::from(10).pow(U256::from(18));
            Ok(value * scale)
        } else {
            // Assume it's already in wei
            Ok(value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_amount_decimal() {
        let amount = parse_amount("1.5").unwrap();
        assert_eq!(amount, U256::from(1_500_000_000_000_000_000u64));
    }

    #[test]
    fn test_parse_amount_whole() {
        let amount = parse_amount("10").unwrap();
        // 10 < 1000, so treated as HOT
        assert_eq!(amount, U256::from(10_000_000_000_000_000_000u128));
    }

    #[test]
    fn test_parse_amount_wei() {
        let amount = parse_amount("1000000000000000000").unwrap();
        // Large number, treated as wei
        assert_eq!(amount, U256::from(1_000_000_000_000_000_000u64));
    }
}
