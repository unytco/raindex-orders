use crate::config::Network;
use alloy::primitives::{address, keccak256, Address, B256, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use anyhow::{Context, Result};
use holo_hash::ActionHash;
use std::env;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

/// The coupon signer whose private key is committed in `src/Constants.sol`.
pub const TEST_SIGNER: Address = address!("8E72b7568738da52ca3DCd9b24E178127A4E7d37");

const DEFAULT_EXPIRY_SECONDS: u64 = 604_800;

/// The claim order every coupon names, and the vault it pays out of.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimOrder {
    pub order_hash: B256,
    pub order_owner: Address,
    pub orderbook: Address,
    pub token: Address,
    pub vault_id: U256,
}

pub type Signing<'a> = Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>>;

/// Signs a coupon for the claim order's `valid-signer`. The orderbook checks the
/// signature with OpenZeppelin's `SignatureChecker` over `digest`, the EIP-191
/// digest of the coupon's context: a key answers with its own 65-byte signature,
/// a contract signer such as a Safe with what its EIP-1271 `isValidSignature`
/// accepts.
pub trait CouponKey: Send + Sync {
    fn address(&self) -> Address;
    fn sign<'a>(&'a self, digest: &'a B256) -> Signing<'a>;
}

impl CouponKey for PrivateKeySigner {
    fn address(&self) -> Address {
        Signer::address(self)
    }

    fn sign<'a>(&'a self, digest: &'a B256) -> Signing<'a> {
        Box::pin(async move {
            let signature = self.sign_hash(digest).await?;
            let mut bytes = Vec::with_capacity(65);
            bytes.extend_from_slice(&signature.r().to_be_bytes::<32>());
            bytes.extend_from_slice(&signature.s().to_be_bytes::<32>());
            bytes.push(if signature.v() { 28 } else { 27 });
            Ok(bytes)
        })
    }
}

pub struct CouponSigner {
    key: Box<dyn CouponKey>,
    order: ClaimOrder,
    expiry_seconds: u64,
}

impl CouponSigner {
    /// Parsed at startup, naming every variable that is unset or malformed. On
    /// mainnet the test signer is refused, as its key is public.
    pub fn from_env(network: Network) -> Result<Self> {
        Self::from_settings(network, |key| env::var(key).ok())
    }

    fn from_settings(network: Network, setting: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let mut faults = Vec::new();
        let order_hash = parsed(&setting, "ORDER_HASH", "a 32-byte hash", &mut faults);
        let order_owner = parsed(&setting, "ORDER_OWNER", "an address", &mut faults);
        let orderbook = parsed(&setting, "ORDERBOOK_ADDRESS", "an address", &mut faults);
        let token = parsed(&setting, "TOKEN_ADDRESS", "an address", &mut faults);
        let vault_id = parsed(&setting, "VAULT_ID", "a uint256", &mut faults);
        let expiry_seconds = match setting("EXPIRY_SECONDS") {
            None => Some(DEFAULT_EXPIRY_SECONDS),
            Some(_) => parsed(
                &setting,
                "EXPIRY_SECONDS",
                "a number of seconds",
                &mut faults,
            ),
        };

        let key = match setting("SIGNER_PRIVATE_KEY") {
            None => {
                faults.push("SIGNER_PRIVATE_KEY is required".to_string());
                None
            }
            Some(raw) => match raw.parse::<PrivateKeySigner>() {
                Ok(key) => Some(key),
                Err(_) => {
                    faults.push("SIGNER_PRIVATE_KEY is not a secp256k1 private key".to_string());
                    None
                }
            },
        };
        if network == Network::Mainnet && key.as_ref().map(CouponKey::address) == Some(TEST_SIGNER)
        {
            faults.push(format!(
                "SIGNER_PRIVATE_KEY is the test signer {TEST_SIGNER}, whose key is public: mainnet refuses it"
            ));
        }

        match (
            order_hash,
            order_owner,
            orderbook,
            token,
            vault_id,
            expiry_seconds,
            key,
        ) {
            (
                Some(order_hash),
                Some(order_owner),
                Some(orderbook),
                Some(token),
                Some(vault_id),
                Some(expiry_seconds),
                Some(key),
            ) if faults.is_empty() => Ok(Self {
                key: Box::new(key),
                order: ClaimOrder {
                    order_hash,
                    order_owner,
                    orderbook,
                    token,
                    vault_id,
                },
                expiry_seconds,
            }),
            _ => anyhow::bail!("{}", faults.join("; ")),
        }
    }

    pub fn order(&self) -> &ClaimOrder {
        &self.order
    }

    /// The claim coupon for the withdrawal whose transaction ID is `withdrawal`.
    pub async fn coupon(
        &self,
        amount: &str,
        recipient: &str,
        withdrawal: &ActionHash,
    ) -> Result<String> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        self.coupon_at(amount, recipient, withdrawal, now).await
    }

    async fn coupon_at(
        &self,
        amount: &str,
        recipient: &str,
        withdrawal: &ActionHash,
        now: u64,
    ) -> Result<String> {
        let recipient: Address = recipient.parse().context("Invalid recipient address")?;
        let order = &self.order;
        let context: Vec<U256> = vec![
            pad_address(recipient),
            parse_amount(amount)?,
            U256::from(now + self.expiry_seconds),
            U256::from_be_bytes(order.order_hash.0),
            pad_address(order.order_owner),
            pad_address(order.orderbook),
            pad_address(order.token),
            order.vault_id,
            withdrawal_nonce(withdrawal),
        ];

        let signature = self.key.sign(&context_digest(&context)).await?;
        let context: Vec<String> = context.iter().map(U256::to_string).collect();
        Ok(format!(
            "{:?},0x{},{}",
            self.key.address(),
            hex::encode(signature),
            context.join(",")
        ))
    }
}

#[cfg(test)]
impl CouponSigner {
    /// A signer for the Sepolia claim order.
    pub fn with_key(key: impl CouponKey + 'static) -> Self {
        Self {
            key: Box::new(key),
            order: ClaimOrder {
                order_hash: "0x5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9"
                    .parse()
                    .unwrap(),
                order_owner: address!("E3E064e3C2EEf66cb93dA8D8114F5084E92F48D6"),
                orderbook: address!("fca89cD12Ba1346b1ac570ed988AB43b812733fe"),
                token: address!("eaC8eEEE9f84F3E3F592e9D8604100eA1b788749"),
                vault_id: "0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b"
                    .parse()
                    .unwrap(),
            },
            expiry_seconds: DEFAULT_EXPIRY_SECONDS,
        }
    }
}

/// What the orderbook passes to `SignatureChecker`: keccak256 of the packed
/// context words, under the EIP-191 personal-message prefix.
fn context_digest(context: &[U256]) -> B256 {
    let packed: Vec<u8> = context.iter().flat_map(|v| v.to_be_bytes::<32>()).collect();
    keccak256(
        [
            b"\x19Ethereum Signed Message:\n32".as_slice(),
            keccak256(&packed).as_slice(),
        ]
        .concat(),
    )
}

fn parsed<T: FromStr>(
    setting: &impl Fn(&str) -> Option<String>,
    key: &str,
    what: &str,
    faults: &mut Vec<String>,
) -> Option<T> {
    match setting(key) {
        None => {
            faults.push(format!("{key} is required"));
            None
        }
        Some(raw) => match raw.parse() {
            Ok(value) => Some(value),
            Err(_) => {
                faults.push(format!("{key}={raw} is not {what}"));
                None
            }
        },
    }
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
    const TEST_SIGNER_KEY: &str =
        "0xdcbe53cbf4cbee212fe6339821058f2787c7726ae0684335118cdea2e8adaafd";

    fn sepolia_order() -> ClaimOrder {
        CouponSigner::with_key(PrivateKeySigner::random()).order
    }

    fn signer_with(key: impl CouponKey + 'static) -> CouponSigner {
        CouponSigner::with_key(key)
    }

    async fn coupon(signer: &CouponSigner, withdrawal: &ActionHash, now: u64) -> String {
        let recipient = "0x1111111111111111111111111111111111111111";
        signer
            .coupon_at("1.5", recipient, withdrawal, now)
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
        let signer = signer_with(PrivateKeySigner::random());
        let first = coupon(&signer, &ActionHash::from_raw_32(vec![1; 32]), NOW).await;
        let second = coupon(&signer, &ActionHash::from_raw_32(vec![2; 32]), NOW).await;

        assert_ne!(nonce(&first), nonce(&second));
    }

    #[tokio::test]
    async fn a_withdrawal_signed_again_keeps_its_nonce() {
        let signer = signer_with(PrivateKeySigner::random());
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

    /// Anvil's third account. test/TestCouponSignature.t.sol signs the same
    /// context with forge's `vm.sign` and expects this coupon's signature.
    #[tokio::test]
    async fn a_key_signs_the_coupon_the_claim_order_checks() {
        let key: PrivateKeySigner =
            "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a"
                .parse()
                .unwrap();
        let signer = signer_with(key);

        let coupon = coupon(&signer, &ActionHash::from_raw_32(vec![7; 32]), NOW).await;

        assert_eq!(coupon, GOLDEN_COUPON);
    }

    const GOLDEN_COUPON: &str = "0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc,0x5700e22974b0b133fd972f920b36e055e35ab5e83c08db389845451c5532232846d64b38b077e85d8c9e311fd7d22b15af7a78f58b5cb11089b9b07b429bade11c,97433442488726861213578988847752201310395502865,1500000000000000000,1750604800,42941365433660945573526868378572896801276519612010928011051331832534249141753,1300945060633283894583661816534861012306758682838,1442425860134574572653119565674449928420894127102,1340384803335777240622375245841800624658726815561,108043606565222972236900316128309391016550688326814185311821020602083120460619,8496889498503184870230947373316602526857109467185425766089433217409414560631";

    /// A contract signer, as a Safe would be: its own address, and a signature
    /// of any length that its `isValidSignature` accepts.
    struct ContractSigner;

    impl CouponKey for ContractSigner {
        fn address(&self) -> Address {
            address!("5afe5afe5afe5afe5afe5afe5afe5afe5afe5afe")
        }

        fn sign<'a>(&'a self, digest: &'a B256) -> Signing<'a> {
            Box::pin(async move { Ok([digest.as_slice(), digest.as_slice()].concat()) })
        }
    }

    #[tokio::test]
    async fn a_contract_signer_names_itself_and_signs_the_same_digest() {
        let signer = signer_with(ContractSigner);
        let withdrawal = ActionHash::from_raw_32(vec![7; 32]);

        let coupon = coupon(&signer, &withdrawal, NOW).await;
        let fields: Vec<&str> = coupon.split(',').collect();
        let context: Vec<U256> = fields[2..].iter().map(|f| f.parse().unwrap()).collect();
        let digest = context_digest(&context);

        assert_eq!(fields[0], format!("{:?}", ContractSigner.address()));
        assert_eq!(
            fields[1],
            format!(
                "0x{}",
                hex::encode([digest.as_slice(), digest.as_slice()].concat())
            )
        );
    }

    const SEPOLIA_SIGNER_SETTINGS: [(&str, &str); 6] = [
        (
            "ORDER_HASH",
            "0x5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9",
        ),
        ("ORDER_OWNER", "0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6"),
        (
            "ORDERBOOK_ADDRESS",
            "0xfca89cD12Ba1346b1ac570ed988AB43b812733fe",
        ),
        (
            "TOKEN_ADDRESS",
            "0xeaC8eEEE9f84F3E3F592e9D8604100eA1b788749",
        ),
        (
            "VAULT_ID",
            "0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b",
        ),
        ("SIGNER_PRIVATE_KEY", TEST_SIGNER_KEY),
    ];

    fn settings(set: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> {
        move |key| {
            set.iter()
                .find(|(set_key, _)| *set_key == key)
                .map(|(_, value)| value.clone())
        }
    }

    fn sepolia_settings_with(
        changes: &[(&'static str, Option<&str>)],
    ) -> impl Fn(&str) -> Option<String> {
        let mut set: Vec<(&'static str, String)> = SEPOLIA_SIGNER_SETTINGS
            .iter()
            .map(|(key, value)| (*key, value.to_string()))
            .collect();
        for (key, value) in changes {
            set.retain(|(set_key, _)| set_key != key);
            if let Some(value) = value {
                set.push((key, value.to_string()));
            }
        }
        settings(set)
    }

    fn refusal(network: Network, setting: impl Fn(&str) -> Option<String>) -> String {
        match CouponSigner::from_settings(network, setting) {
            Ok(_) => panic!("the signer settings were accepted"),
            Err(err) => format!("{err:#}"),
        }
    }

    #[test]
    fn the_sepolia_settings_parse_into_the_claim_order() {
        let signer =
            CouponSigner::from_settings(Network::Sepolia, sepolia_settings_with(&[])).unwrap();

        assert_eq!(signer.order(), &sepolia_order());
        assert_eq!(signer.key.address(), TEST_SIGNER);
        assert_eq!(signer.expiry_seconds, DEFAULT_EXPIRY_SECONDS);
    }

    #[test]
    fn each_unset_signer_variable_is_named() {
        for (key, _) in SEPOLIA_SIGNER_SETTINGS {
            let message = refusal(Network::Sepolia, sepolia_settings_with(&[(key, None)]));
            assert_eq!(message, format!("{key} is required"));
        }
    }

    #[test]
    fn each_malformed_signer_variable_is_named() {
        for (key, _) in SEPOLIA_SIGNER_SETTINGS {
            let message = refusal(
                Network::Sepolia,
                sepolia_settings_with(&[(key, Some("0xZZ"))]),
            );
            assert!(message.starts_with(key), "{message}");
            assert!(!message.contains("is required"), "{message}");
        }
        let message = refusal(
            Network::Sepolia,
            sepolia_settings_with(&[("EXPIRY_SECONDS", Some("a week"))]),
        );
        assert_eq!(message, "EXPIRY_SECONDS=a week is not a number of seconds");
    }

    #[test]
    fn every_fault_is_named_in_one_refusal() {
        let message = refusal(
            Network::Sepolia,
            sepolia_settings_with(&[
                ("ORDER_HASH", None),
                ("TOKEN_ADDRESS", Some("hot")),
                ("SIGNER_PRIVATE_KEY", None),
            ]),
        );

        assert_eq!(
            message,
            "ORDER_HASH is required; TOKEN_ADDRESS=hot is not an address; SIGNER_PRIVATE_KEY is required"
        );
    }

    #[test]
    fn a_malformed_private_key_is_not_repeated() {
        let almost_a_key = &TEST_SIGNER_KEY[..TEST_SIGNER_KEY.len() - 2];
        let message = refusal(
            Network::Sepolia,
            sepolia_settings_with(&[("SIGNER_PRIVATE_KEY", Some(almost_a_key))]),
        );

        assert_eq!(message, "SIGNER_PRIVATE_KEY is not a secp256k1 private key");
    }

    #[test]
    fn mainnet_refuses_the_test_signer_and_sepolia_takes_it() {
        let message = refusal(Network::Mainnet, sepolia_settings_with(&[]));
        assert!(
            message.contains("test signer 0x8E72b7568738da52ca3DCd9b24E178127A4E7d37"),
            "{message}"
        );
        assert!(CouponSigner::from_settings(Network::Sepolia, sepolia_settings_with(&[])).is_ok());

        let other_key = "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a";
        let mainnet = CouponSigner::from_settings(
            Network::Mainnet,
            sepolia_settings_with(&[("SIGNER_PRIVATE_KEY", Some(other_key))]),
        );
        assert!(mainnet.is_ok());
    }

    #[test]
    fn expiry_seconds_is_read_when_set() {
        let signer = CouponSigner::from_settings(
            Network::Sepolia,
            sepolia_settings_with(&[("EXPIRY_SECONDS", Some("3600"))]),
        )
        .unwrap();

        assert_eq!(signer.expiry_seconds, 3600);
    }
}
