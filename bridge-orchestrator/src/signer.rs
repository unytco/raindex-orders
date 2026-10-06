use crate::config::{log_testnet_defaults, with_network_defaults, Network};
use alloy::primitives::{address, keccak256, Address, B256, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use anyhow::{Context, Result};
use holo_hash::ActionHash;
use std::env;
use std::future::Future;
use std::num::NonZeroU64;
use std::pin::Pin;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

/// The coupon signer whose private key is committed in `src/Constants.sol`.
pub const TEST_SIGNER: Address = address!("8E72b7568738da52ca3DCd9b24E178127A4E7d37");

const CLAIM_ORDER_VARIABLES: [&str; 10] = [
    "ORDER_HASH",
    "ORDER_OWNER",
    "ORDERBOOK_ADDRESS",
    "TOKEN_ADDRESS",
    "VAULT_ID",
    "CLAIM_SIGNER",
    "CLAIM_INTERPRETER",
    "CLAIM_STORE",
    "CLAIM_EXPRESSION",
    "CLAIM_INPUT_TOKEN",
];

const MAX_EXPIRY_SECONDS: u64 = 365 * 24 * 60 * 60;

pub(crate) fn env_variables() -> impl Iterator<Item = &'static str> {
    CLAIM_ORDER_VARIABLES
        .into_iter()
        .chain(["SIGNER_PRIVATE_KEY", "EXPIRY_SECONDS"])
}

const DEFAULT_EXPIRY_SECONDS: NonZeroU64 = match NonZeroU64::new(604_800) {
    Some(seconds) => seconds,
    None => unreachable!(),
};

/// The claim order coupons are signed for, as the deploy record prints it.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimOrder {
    pub order_hash: B256,
    pub order_owner: Address,
    pub orderbook: Address,
    pub token: Address,
    pub vault_id: U256,
    /// The order's `valid-signer`.
    pub signer: Address,
    pub interpreter: Address,
    pub store: Address,
    pub expression: Address,
    pub input_token: Address,
}

/// A coupon's signer, its nine context words and the signature over them.
pub struct SignedCoupon {
    pub signer: Address,
    pub context: Vec<U256>,
    pub signature: Vec<u8>,
}

pub type Signing<'a> = Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>>;

/// The claim order's `valid-signer`. The orderbook checks its signature of
/// `digest` with OpenZeppelin's `SignatureChecker`: a key's 65-byte signature, or
/// what a contract's EIP-1271 `isValidSignature` accepts.
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
    expiry_seconds: NonZeroU64,
}

impl CouponSigner {
    pub fn from_env(network: Network) -> Result<Self> {
        Self::from_settings(network, |key| env::var(key).ok())
    }

    fn from_settings(network: Network, setting: impl Fn(&str) -> Option<String>) -> Result<Self> {
        log_testnet_defaults(network, &setting, &CLAIM_ORDER_VARIABLES);
        let setting = with_network_defaults(network, setting);
        let mut faults = Vec::new();
        let order_hash = parsed(&setting, "ORDER_HASH", "a 32-byte hash", &mut faults);
        let order_owner = parsed(&setting, "ORDER_OWNER", "an address", &mut faults);
        let orderbook = parsed(&setting, "ORDERBOOK_ADDRESS", "an address", &mut faults);
        let token = parsed(&setting, "TOKEN_ADDRESS", "an address", &mut faults);
        let vault_id = parsed(&setting, "VAULT_ID", "a uint256", &mut faults);
        let signer = parsed(&setting, "CLAIM_SIGNER", "an address", &mut faults);
        let interpreter = parsed(&setting, "CLAIM_INTERPRETER", "an address", &mut faults);
        let store = parsed(&setting, "CLAIM_STORE", "an address", &mut faults);
        let expression = parsed(&setting, "CLAIM_EXPRESSION", "an address", &mut faults);
        let input_token = parsed(&setting, "CLAIM_INPUT_TOKEN", "an address", &mut faults);
        let expiry_seconds = match setting("EXPIRY_SECONDS") {
            None => Some(DEFAULT_EXPIRY_SECONDS),
            Some(raw) => match raw.parse::<NonZeroU64>() {
                Ok(seconds) if seconds.get() <= MAX_EXPIRY_SECONDS => Some(seconds),
                _ => {
                    faults.push(format!(
                        "EXPIRY_SECONDS={raw} is not a number of seconds from 1 to {MAX_EXPIRY_SECONDS}"
                    ));
                    None
                }
            },
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

        let order = match (
            order_hash,
            order_owner,
            orderbook,
            token,
            vault_id,
            signer,
            interpreter,
            store,
            expression,
            input_token,
        ) {
            (
                Some(order_hash),
                Some(order_owner),
                Some(orderbook),
                Some(token),
                Some(vault_id),
                Some(signer),
                Some(interpreter),
                Some(store),
                Some(expression),
                Some(input_token),
            ) => Some(ClaimOrder {
                order_hash,
                order_owner,
                orderbook,
                token,
                vault_id,
                signer,
                interpreter,
                store,
                expression,
                input_token,
            }),
            _ => None,
        };
        match (order, expiry_seconds, key) {
            (Some(order), Some(expiry_seconds), Some(key)) if faults.is_empty() => Ok(Self {
                key: Box::new(key),
                order,
                expiry_seconds,
            }),
            _ => anyhow::bail!("{}", faults.join("; ")),
        }
    }

    pub fn order(&self) -> &ClaimOrder {
        &self.order
    }

    pub fn address(&self) -> Address {
        self.key.address()
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
        let coupon = self
            .sign(
                recipient,
                parse_amount(amount)?,
                U256::from(now) + U256::from(self.expiry_seconds.get()),
                withdrawal_nonce(withdrawal),
            )
            .await?;
        let context: Vec<String> = coupon.context.iter().map(U256::to_string).collect();
        Ok(format!(
            "{:?},0x{},{}",
            coupon.signer,
            hex::encode(coupon.signature),
            context.join(",")
        ))
    }

    /// A coupon for one wei to the signer itself, which never expires, with a nonce
    /// no withdrawal has. Simulated, it shows whether the claim order accepts this key.
    pub async fn probe_coupon(&self) -> Result<SignedCoupon> {
        let nonce = U256::from_be_bytes(keccak256(b"bridge-orchestrator startup probe").0);
        self.sign(self.key.address(), U256::from(1), U256::MAX, nonce)
            .await
    }

    async fn sign(
        &self,
        recipient: Address,
        amount: U256,
        expiry: U256,
        nonce: U256,
    ) -> Result<SignedCoupon> {
        let order = &self.order;
        let context: Vec<U256> = vec![
            pad_address(recipient),
            amount,
            expiry,
            U256::from_be_bytes(order.order_hash.0),
            pad_address(order.order_owner),
            pad_address(order.orderbook),
            pad_address(order.token),
            order.vault_id,
            nonce,
        ];
        let signature = self.key.sign(&context_digest(&context)).await?;
        Ok(SignedCoupon {
            signer: self.key.address(),
            context,
            signature,
        })
    }
}

#[cfg(test)]
impl ClaimOrder {
    /// The claim order on Sepolia, from TestNet's defaults.
    pub fn sepolia() -> Self {
        let value = with_network_defaults(Network::Sepolia, |_| None);
        let address = |key: &str| value(key).unwrap().parse::<Address>().unwrap();
        Self {
            order_hash: value("ORDER_HASH").unwrap().parse().unwrap(),
            order_owner: address("ORDER_OWNER"),
            orderbook: address("ORDERBOOK_ADDRESS"),
            token: address("TOKEN_ADDRESS"),
            vault_id: value("VAULT_ID").unwrap().parse().unwrap(),
            signer: address("CLAIM_SIGNER"),
            interpreter: address("CLAIM_INTERPRETER"),
            store: address("CLAIM_STORE"),
            expression: address("CLAIM_EXPRESSION"),
            input_token: address("CLAIM_INPUT_TOKEN"),
        }
    }
}

#[cfg(test)]
impl CouponSigner {
    pub fn with_key(key: impl CouponKey + 'static) -> Self {
        Self::for_order(key, ClaimOrder::sepolia())
    }

    pub fn for_order(key: impl CouponKey + 'static, order: ClaimOrder) -> Self {
        Self {
            key: Box::new(key),
            order,
            expiry_seconds: DEFAULT_EXPIRY_SECONDS,
        }
    }
}

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
    use crate::config::test_settings;
    use holo_hash::ActionHashB64;

    const NOW: u64 = 1_750_000_000;
    const TEST_SIGNER_KEY: &str =
        "0xdcbe53cbf4cbee212fe6339821058f2787c7726ae0684335118cdea2e8adaafd";

    fn sepolia_order() -> ClaimOrder {
        CouponSigner::with_key(PrivateKeySigner::random()).order
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
        let signer = CouponSigner::with_key(PrivateKeySigner::random());
        let first = coupon(&signer, &ActionHash::from_raw_32(vec![1; 32]), NOW).await;
        let second = coupon(&signer, &ActionHash::from_raw_32(vec![2; 32]), NOW).await;

        assert_ne!(nonce(&first), nonce(&second));
    }

    #[tokio::test]
    async fn a_withdrawal_signed_again_keeps_its_nonce() {
        let signer = CouponSigner::with_key(PrivateKeySigner::random());
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
        let signer = CouponSigner::with_key(key);

        let coupon = coupon(&signer, &ActionHash::from_raw_32(vec![7; 32]), NOW).await;

        assert_eq!(coupon, GOLDEN_COUPON);
    }

    const GOLDEN_COUPON: &str = "0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc,0x5700e22974b0b133fd972f920b36e055e35ab5e83c08db389845451c5532232846d64b38b077e85d8c9e311fd7d22b15af7a78f58b5cb11089b9b07b429bade11c,97433442488726861213578988847752201310395502865,1500000000000000000,1750604800,42941365433660945573526868378572896801276519612010928011051331832534249141753,1300945060633283894583661816534861012306758682838,1442425860134574572653119565674449928420894127102,1340384803335777240622375245841800624658726815561,108043606565222972236900316128309391016550688326814185311821020602083120460619,8496889498503184870230947373316602526857109467185425766089433217409414560631";

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
        let signer = CouponSigner::with_key(ContractSigner);
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

    const SEPOLIA_SIGNER_SETTINGS: [(&str, &str); 11] = [
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
        ("CLAIM_SIGNER", "0x8E72b7568738da52ca3DCd9b24E178127A4E7d37"),
        (
            "CLAIM_INTERPRETER",
            "0x8853d126bc23a45b9f807739b6ea0b38ef569005",
        ),
        ("CLAIM_STORE", "0x23f77e7bc935503e437166498d7d72f2ea290e1f"),
        (
            "CLAIM_EXPRESSION",
            "0x0a1369aee76570cc7404492d55a5d1468d5a9b4b",
        ),
        (
            "CLAIM_INPUT_TOKEN",
            "0x555FA2F68dD9B7dB6c8cA1F03bFc317ce61e9028",
        ),
        ("SIGNER_PRIVATE_KEY", TEST_SIGNER_KEY),
    ];

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
        let set: Vec<(&str, &str)> = set
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        test_settings(&set)
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

    const MAINNET_KEY: &str = "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a";

    /// The Sepolia settings with a key mainnet takes, then `changes`.
    fn mainnet_settings_with(
        changes: &[(&'static str, Option<&str>)],
    ) -> impl Fn(&str) -> Option<String> {
        let mut all = vec![("SIGNER_PRIVATE_KEY", Some(MAINNET_KEY))];
        all.extend_from_slice(changes);
        sepolia_settings_with(&all)
    }

    #[test]
    fn mainnet_names_each_unset_signer_variable() {
        for (key, _) in SEPOLIA_SIGNER_SETTINGS {
            let message = refusal(Network::Mainnet, mainnet_settings_with(&[(key, None)]));
            assert_eq!(message, format!("{key} is required"));
        }
    }

    #[test]
    fn sepolia_takes_testnet_values_for_all_but_the_key() {
        let key = test_settings(&[("SIGNER_PRIVATE_KEY", TEST_SIGNER_KEY)]);
        let signer = CouponSigner::from_settings(Network::Sepolia, key).unwrap();

        assert_eq!(signer.order(), &ClaimOrder::sepolia());
        assert_eq!(
            refusal(Network::Sepolia, test_settings(&[])),
            "SIGNER_PRIVATE_KEY is required"
        );
        let empty = test_settings(&[("SIGNER_PRIVATE_KEY", TEST_SIGNER_KEY), ("ORDER_HASH", "")]);
        assert_eq!(
            CouponSigner::from_settings(Network::Sepolia, empty)
                .unwrap()
                .order(),
            &ClaimOrder::sepolia()
        );
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
        for value in ["a week", "0", "31536001"] {
            let message = refusal(
                Network::Sepolia,
                sepolia_settings_with(&[("EXPIRY_SECONDS", Some(value))]),
            );
            assert_eq!(
                message,
                format!("EXPIRY_SECONDS={value} is not a number of seconds from 1 to 31536000")
            );
        }
    }

    #[test]
    fn every_fault_is_named_in_one_refusal() {
        let message = refusal(
            Network::Mainnet,
            mainnet_settings_with(&[
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
            sepolia_settings_with(&[
                ("SIGNER_PRIVATE_KEY", Some(other_key)),
                (
                    "CLAIM_SIGNER",
                    Some("0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC"),
                ),
            ]),
        );
        assert!(mainnet.is_ok());
    }

    #[test]
    fn the_probe_coupon_pays_the_key_one_wei_and_never_expires() {
        let key: PrivateKeySigner = TEST_SIGNER_KEY.parse().unwrap();
        let signer = CouponSigner::with_key(key);

        let probe = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(signer.probe_coupon())
            .unwrap();

        assert_eq!(probe.signer, TEST_SIGNER);
        assert_eq!(probe.context[0], pad_address(TEST_SIGNER));
        assert_eq!(probe.context[1], U256::from(1));
        assert_eq!(probe.context[2], U256::MAX);
        let recovered = alloy::primitives::PrimitiveSignature::try_from(probe.signature.as_slice())
            .unwrap()
            .recover_address_from_prehash(&context_digest(&probe.context))
            .unwrap();
        assert_eq!(recovered, TEST_SIGNER);
    }

    #[test]
    fn expiry_seconds_is_read_when_set() {
        let signer = CouponSigner::from_settings(
            Network::Sepolia,
            sepolia_settings_with(&[("EXPIRY_SECONDS", Some("3600"))]),
        )
        .unwrap();

        assert_eq!(signer.expiry_seconds.get(), 3600);
    }
}
