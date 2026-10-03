use alloy::primitives::{keccak256, Address, B256, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use anyhow::{Context, Result};
use holo_hash::ActionHash;
use serde::{Deserialize, Serialize};
use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct SignerContext {
    pub order_hash: String,
    pub order_owner: String,
    pub orderbook: String,
    pub token: String,
    pub vault_id: String,
    pub expiry_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedContext {
    pub signer: String,
    pub context: Vec<String>,
    pub signature: String,
}

pub fn signer_context_from_env() -> Result<SignerContext> {
    Ok(SignerContext {
        order_hash: env::var("ORDER_HASH").context("ORDER_HASH not set")?,
        order_owner: env::var("ORDER_OWNER").context("ORDER_OWNER not set")?,
        orderbook: env::var("ORDERBOOK_ADDRESS").context("ORDERBOOK_ADDRESS not set")?,
        token: env::var("TOKEN_ADDRESS").context("TOKEN_ADDRESS not set")?,
        vault_id: env::var("VAULT_ID").context("VAULT_ID not set")?,
        expiry_seconds: env::var("EXPIRY_SECONDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(604800),
    })
}

/// Signs the claim coupon for the withdrawal whose transaction ID is `withdrawal`.
pub async fn generate_coupon(
    amount: &str,
    recipient: &str,
    withdrawal: &ActionHash,
    ctx: &SignerContext,
) -> Result<String> {
    let private_key = env::var("SIGNER_PRIVATE_KEY")
        .context("SIGNER_PRIVATE_KEY environment variable not set")?;
    let signer: PrivateKeySigner = private_key.parse().context("Invalid private key format")?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    sign_coupon(&signer, amount, recipient, withdrawal, ctx, now).await
}

async fn sign_coupon(
    signer: &PrivateKeySigner,
    amount: &str,
    recipient: &str,
    withdrawal: &ActionHash,
    ctx: &SignerContext,
    now: u64,
) -> Result<String> {
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

    let expiry = now + ctx.expiry_seconds;

    let context: Vec<U256> = vec![
        pad_address(recipient),
        amount,
        U256::from(expiry),
        U256::from_be_bytes(order_hash.0),
        pad_address(order_owner),
        pad_address(orderbook),
        pad_address(token),
        vault_id,
        withdrawal_nonce(withdrawal),
    ];

    let packed: Vec<u8> = context.iter().flat_map(|v| v.to_be_bytes::<32>()).collect();
    let context_hash = keccak256(&packed);
    let prefixed = keccak256(
        [
            b"\x19Ethereum Signed Message:\n32".as_slice(),
            context_hash.as_slice(),
        ]
        .concat(),
    );
    let signature = signer.sign_hash(&prefixed).await?;
    let mut bytes = [0u8; 65];
    bytes[0..32].copy_from_slice(&signature.r().to_be_bytes::<32>());
    bytes[32..64].copy_from_slice(&signature.s().to_be_bytes::<32>());
    bytes[64] = if signature.v() { 28 } else { 27 };
    let signed = SignedContext {
        signer: format!("{:?}", signer_address),
        context: context.iter().map(|v| v.to_string()).collect(),
        signature: format!("0x{}", hex::encode(bytes)),
    };

    Ok(format!(
        "{},{},{}",
        signed.signer,
        signed.signature,
        signed.context.join(",")
    ))
}

/// keccak256 of the withdrawal's 39-byte action hash. The claim order refuses a
/// nonce it has already seen, so every coupon signed for one withdrawal shares a
/// nonce and at most one of them can be claimed.
fn withdrawal_nonce(withdrawal: &ActionHash) -> U256 {
    U256::from_be_bytes(keccak256(withdrawal.get_raw_39()).0)
}

fn pad_address(addr: Address) -> U256 {
    U256::from_be_slice(&{
        let mut padded = [0u8; 32];
        padded[12..].copy_from_slice(addr.as_slice());
        padded
    })
}

fn parse_amount(amount_str: &str) -> Result<U256> {
    if amount_str.contains('.') {
        let parts: Vec<&str> = amount_str.split('.').collect();
        if parts.len() != 2 {
            anyhow::bail!("Invalid amount format");
        }
        let whole: U256 = parts[0].parse().context("Invalid whole number part")?;
        let decimals_str = parts[1];
        if decimals_str.len() > 18 {
            anyhow::bail!("Too many decimal places (max 18)");
        }
        let frac: U256 = decimals_str.parse().context("Invalid decimal part")?;
        let scale = U256::from(10).pow(U256::from(18));
        let frac_scale = U256::from(10).pow(U256::from(18 - decimals_str.len()));
        Ok(whole * scale + frac * frac_scale)
    } else {
        let value: U256 = amount_str.parse().context("Invalid amount")?;
        let scale = U256::from(10).pow(U256::from(18));
        Ok(value * scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use holo_hash::ActionHashB64;

    const NOW: u64 = 1_750_000_000;

    fn ctx() -> SignerContext {
        SignerContext {
            order_hash: "0x5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9"
                .to_string(),
            order_owner: "0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6".to_string(),
            orderbook: "0xfca89cD12Ba1346b1ac570ed988AB43b812733fe".to_string(),
            token: "0xeaC8eEEE9f84F3E3F592e9D8604100eA1b788749".to_string(),
            vault_id: "0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b"
                .to_string(),
            expiry_seconds: 604800,
        }
    }

    async fn coupon(signer: &PrivateKeySigner, withdrawal: &ActionHash, now: u64) -> String {
        let recipient = "0x1111111111111111111111111111111111111111";
        sign_coupon(signer, "1.5", recipient, withdrawal, &ctx(), now)
            .await
            .unwrap()
    }

    /// Context field 8, after the signer and signature.
    fn nonce(coupon: &str) -> &str {
        let fields: Vec<&str> = coupon.split(',').collect();
        assert_eq!(fields.len(), 11, "coupon is signer,signature,c0..c8");
        fields[10]
    }

    #[tokio::test]
    async fn withdrawals_signed_in_the_same_second_get_different_nonces() {
        let signer = PrivateKeySigner::random();
        let first = coupon(&signer, &ActionHash::from_raw_32(vec![1; 32]), NOW).await;
        let second = coupon(&signer, &ActionHash::from_raw_32(vec![2; 32]), NOW).await;

        assert_ne!(nonce(&first), nonce(&second));
    }

    #[tokio::test]
    async fn a_withdrawal_signed_again_keeps_its_nonce() {
        let signer = PrivateKeySigner::random();
        let withdrawal: ActionHash =
            ActionHashB64::from_b64_str("uhCkkWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlrx-wQB")
                .unwrap()
                .into();

        let first = coupon(&signer, &withdrawal, NOW).await;
        let later = coupon(&signer, &withdrawal, NOW + 3600).await;

        assert_eq!(nonce(&first), nonce(&later));
        // keccak256 of the ID's 39 raw bytes, computed independently with viem.
        assert_eq!(
            nonce(&first),
            "35225625112360308450652048405547205103100380385573023600097326644231926170802"
        );
    }
}
