use alloy::primitives::{keccak256, Address, B256, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use anyhow::{Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

/// Order/context used for signing. Single source of truth: flattened into sign CLI (main) or loaded from env (withdrawer).
#[derive(Parser, Debug, Clone)]
pub struct SignerContext {
    /// Order hash (from the deployed Raindex order). Falls back to ORDER_HASH env var.
    #[arg(long, env = "ORDER_HASH")]
    pub order_hash: String,

    /// Order owner address. Falls back to ORDER_OWNER env var.
    #[arg(long, env = "ORDER_OWNER")]
    pub order_owner: String,

    /// Orderbook address. Falls back to ORDERBOOK_ADDRESS env var.
    #[arg(long, env = "ORDERBOOK_ADDRESS")]
    pub orderbook: String,

    /// Output token address (HOT or TROT). Falls back to TOKEN_ADDRESS env var.
    #[arg(long, env = "TOKEN_ADDRESS")]
    pub token: String,

    /// Output vault ID. Falls back to VAULT_ID env var.
    #[arg(long, env = "VAULT_ID")]
    pub vault_id: String,

    /// Expiry time in seconds from now (default: 1 week)
    #[arg(long, env = "EXPIRY_SECONDS", default_value = "604800")]
    pub expiry_seconds: u64,

    /// Nonce (unique per coupon, defaults to timestamp)
    #[arg(long, env = "NONCE")]
    pub nonce: Option<u64>,

    /// Output format: json, compact, hex, or ui (for the bridge UI)
    #[arg(long, env = "OUTPUT", default_value = "ui")]
    pub output: String,
}

impl SignerContext {
    /// Load from environment (ORDER_HASH, ORDER_OWNER, etc.). Used by withdrawer so it doesn't parse sign CLI.
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            order_hash: env::var("ORDER_HASH").context("ORDER_HASH not set")?,
            order_owner: env::var("ORDER_OWNER").context("ORDER_OWNER not set")?,
            orderbook: env::var("ORDERBOOK_ADDRESS").context("ORDERBOOK_ADDRESS not set")?,
            token: env::var("TOKEN_ADDRESS").context("TOKEN_ADDRESS not set")?,
            vault_id: env::var("VAULT_ID").context("VAULT_ID not set")?,
            expiry_seconds: env::var("EXPIRY_SECONDS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(604800),
            nonce: env::var("NONCE").ok().and_then(|s| s.parse().ok()),
            output: env::var("OUTPUT").unwrap_or_else(|_| "ui".to_string()),
        })
    }
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

/// Generate a coupon using explicit context. Called by main (with context from Args) or by withdrawer (with context from env).
pub async fn generate_coupon_with_context(
    amount: &str,
    recipient: &str,
    ctx: &SignerContext,
) -> Result<(ClaimCoupon, String)> {
    // Load private key from environment
    let private_key = env::var("SIGNER_PRIVATE_KEY")
        .context("SIGNER_PRIVATE_KEY environment variable not set")?;

    let signer: PrivateKeySigner = private_key.parse().context("Invalid private key format")?;
    let signer_address = signer.address();

    let recipient: Address = recipient.parse().context("Invalid recipient address")?;
    let order_hash: B256 = ctx.order_hash.parse().context("Invalid order hash")?;
    let order_owner: Address = ctx
        .order_owner
        .parse()
        .context("Invalid order owner address")?;
    let orderbook: Address = ctx.orderbook.parse().context("Invalid orderbook address")?;
    let token: Address = ctx.token.parse().context("Invalid token address")?;
    let vault_id: U256 = ctx.vault_id.parse().context("Invalid vault ID")?;

    let amount = parse_amount(amount)?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let expiry = now + ctx.expiry_seconds;
    let nonce = ctx.nonce.unwrap_or(now);

    let context: Vec<U256> = build_context(
        recipient,
        amount,
        expiry,
        order_hash,
        order_owner,
        orderbook,
        token,
        vault_id,
        nonce,
    );

    let (signed_context, coupon) = sign_and_build_coupon(
        &signer,
        signer_address,
        amount,
        recipient,
        &context,
        expiry,
        nonce,
    )
    .await?;

    let output = format_output(&coupon, &context, &signed_context, &ctx.output);
    Ok((coupon, output))
}

/// Build the context array (matches holo-claim.rain expectations).
/// [0] recipient address,
/// [1] amount,
/// [2] expiry,
/// [3] order hash,
/// [4] order owner,
/// [5] orderbook address,
/// [6] token address,
/// [7] output vault id,
/// [8] nonce.
fn build_context(
    recipient: Address,
    amount: U256,
    expiry: u64,
    order_hash: B256,
    order_owner: Address,
    orderbook: Address,
    token: Address,
    vault_id: U256,
    nonce: u64,
) -> Vec<U256> {
    vec![
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
    ]
}

/// Hash context, apply Ethereum signed message prefix (toEthSignedMessageHash), sign, and build coupon.
/// Same as SignContext.sol: keccak256(abi.encodePacked(context)) then "\x19Ethereum Signed Message:\n32" + hash.
async fn sign_and_build_coupon(
    signer: &PrivateKeySigner,
    signer_address: alloy::primitives::Address,
    amount: U256,
    recipient: Address,
    context: &[U256],
    expiry: u64,
    nonce: u64,
) -> Result<(SignedContext, ClaimCoupon)> {
    // keccak256(abi.encodePacked(context))
    let packed: Vec<u8> = context.iter().flat_map(|v| v.to_be_bytes::<32>()).collect();
    let context_hash = keccak256(&packed);
    // toEthSignedMessageHash: "\x19Ethereum Signed Message:\n32" + hash
    let prefixed = keccak256(
        [
            b"\x19Ethereum Signed Message:\n32".as_slice(),
            context_hash.as_slice(),
        ]
        .concat(),
    );

    let signature = signer.sign_hash(&prefixed).await?;

    // Encode signature as r || s || v (same as SignContext.sol). v is 27 or 28 for ecrecover.
    let sig_bytes = {
        let mut bytes = [0u8; 65];
        bytes[0..32].copy_from_slice(&signature.r().to_be_bytes::<32>());
        bytes[32..64].copy_from_slice(&signature.s().to_be_bytes::<32>());
        bytes[64] = if signature.v() { 28 } else { 27 };
        bytes
    };

    let signed_context = SignedContext {
        signer: format!("{:?}", signer_address),
        context: context.iter().map(|v| format!("0x{:064x}", v)).collect(),
        signature: format!("0x{}", hex::encode(sig_bytes)),
    };

    let amount_str = amount.to_string();
    let coupon = ClaimCoupon {
        recipient: format!("{:?}", recipient),
        amount: amount_str.clone(),
        amount_wei: amount_str,
        expiry,
        nonce,
        signed_context: signed_context.clone(),
    };
    Ok((signed_context, coupon))
}

/// Format coupon for stdout: compact, hex, json, or ui (default). UI format is signer,signature,context[0],...
fn format_output(
    coupon: &ClaimCoupon,
    context: &[U256],
    signed_context: &SignedContext,
    output: &str,
) -> String {
    let o = match output {
        "compact" => {
            println!("signer={}", signed_context.signer);
            let line = format!("signature={}", signed_context.signature);
            println!("{}", line);
            for (i, ctx) in signed_context.context.iter().enumerate() {
                println!("context[{}]={}", i, ctx);
            }
            line
        }
        "hex" => {
            let line = format!("{}", signed_context.signature);
            println!("{}", line);
            line
        }
        "json" => {
            let line = serde_json::to_string_pretty(coupon).expect("serialize");
            println!("{}", line);
            line
        }
        _ => {
            // UI format: context values as decimal strings (what the UI deserializeSignedContext expects)
            let context_decimals: Vec<String> = context.iter().map(|v| v.to_string()).collect();
            let line = format!(
                "{},{},{}",
                signed_context.signer,
                signed_context.signature,
                context_decimals.join(",")
            );
            println!("{}", line);
            line
        }
    };
    o
}

/// Parse amount string, supporting decimal notation
fn parse_amount(amount_str: &str) -> Result<U256> {
    // Check if it's a decimal number
    if amount_str.contains('.') {
        let parts: Vec<&str> = amount_str.split('.').collect();
        if parts.len() != 2 {
            anyhow::bail!("Invalid amount format");
        }

        let whole: U256 = parts[0].parse().context("Invalid whole number part")?;

        let decimals_str = parts[1];
        let decimals_len = decimals_str.len();

        if decimals_len > 18 {
            anyhow::bail!("Too many decimal places (max 18)");
        }

        let frac: U256 = decimals_str.parse().context("Invalid decimal part")?;

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
