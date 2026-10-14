use crate::config::{log_testnet_defaults, with_network_defaults, Network};
use alloy::primitives::{address, keccak256, Address, Bytes, B256, U256};
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
    "CLAIM_BYTECODE",
    "CLAIM_INPUT_TOKEN",
];

const MAX_EXPIRY_SECONDS: u64 = 365 * 24 * 60 * 60;

/// HOT's decimals on Ethereum, which the coupon's amount float carries as its
/// exponent.
const HOT_DECIMALS: u32 = 18;

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
    pub vault_id: B256,
    /// The order's `valid-signer`.
    pub signer: Address,
    pub interpreter: Address,
    pub store: Address,
    /// The order's expression, parsed: Raindex V6 orders carry their bytecode.
    pub bytecode: Bytes,
    pub input_token: Address,
}

/// A coupon's signer, its nine context words and the signature over them.
pub struct SignedCoupon {
    pub signer: Address,
    pub context: Vec<B256>,
    pub signature: Vec<u8>,
}

pub type Signing<'a> = Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>>;

/// The claim order's `valid-signer`. Raindex checks its signature of `digest`
/// with OpenZeppelin's `SignatureChecker`: a key's 65-byte signature, or what a
/// contract's EIP-1271 `isValidSignature` accepts.
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
        let vault_id = parsed(&setting, "VAULT_ID", "a 32-byte vault ID", &mut faults);
        let signer = parsed(&setting, "CLAIM_SIGNER", "an address", &mut faults);
        let interpreter = parsed(&setting, "CLAIM_INTERPRETER", "an address", &mut faults);
        let store = parsed(&setting, "CLAIM_STORE", "an address", &mut faults);
        let bytecode = parsed(&setting, "CLAIM_BYTECODE", "hex bytecode", &mut faults);
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
            bytecode,
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
                Some(bytecode),
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
                bytecode,
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
    pub async fn coupon(&self, terms: Payout, withdrawal: &ActionHash) -> Result<String> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        self.coupon_at(terms, withdrawal, now).await
    }

    async fn coupon_at(&self, terms: Payout, withdrawal: &ActionHash, now: u64) -> Result<String> {
        let coupon = self
            .sign(
                terms.recipient,
                terms.wei,
                U256::from(now) + U256::from(self.expiry_seconds.get()),
                withdrawal_nonce(withdrawal),
            )
            .await?;
        Ok(serialize(&coupon))
    }

    /// A coupon for one wei to the signer itself, which expires at the latest
    /// second a float of exponent 0 can hold, with a nonce no withdrawal has.
    /// Simulated, it shows whether the claim order accepts this key.
    pub async fn probe_coupon(&self) -> Result<SignedCoupon> {
        let nonce = keccak256(b"bridge-orchestrator startup probe");
        self.sign(self.key.address(), U256::from(1), MAX_EXPIRY, nonce)
            .await
    }

    async fn sign(
        &self,
        recipient: Address,
        amount: U256,
        expiry: U256,
        nonce: B256,
    ) -> Result<SignedCoupon> {
        let order = &self.order;
        let context: Vec<B256> = vec![
            pad_address(recipient),
            amount_word(amount)?,
            B256::from(expiry),
            order.order_hash,
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

/// The largest coefficient a decimal float holds: int224::MAX.
const MAX_FLOAT_COEFFICIENT: U256 =
    U256::from_limbs([u64::MAX, u64::MAX, u64::MAX, u64::MAX >> 33]);

/// The largest integer a float of exponent 0 holds.
const MAX_EXPIRY: U256 = MAX_FLOAT_COEFFICIENT;

/// The coupon as the website reads it: the signer, the signature, then each
/// context word as a decimal number, all separated by commas.
fn serialize(coupon: &SignedCoupon) -> String {
    let context: Vec<String> = coupon
        .context
        .iter()
        .map(|word| U256::from_be_bytes(word.0).to_string())
        .collect();
    format!(
        "{:?},0x{},{}",
        coupon.signer,
        hex::encode(&coupon.signature),
        context.join(",")
    )
}

/// The amount word of a coupon: the decimal float Raindex V6 pays out, with the
/// wei as its coefficient and -18 as its exponent, so 0xffffffee followed by the
/// wei in the low 224 bits. Converting back loses nothing. A float's
/// coefficient is an int224, which no HOT amount comes near.
pub(crate) fn amount_word(wei: U256) -> Result<B256> {
    anyhow::ensure!(
        wei <= MAX_FLOAT_COEFFICIENT,
        "the amount {wei} wei is too large for a decimal float"
    );
    let exponent = (-(HOT_DECIMALS as i32)) as u32;
    let word = (U256::from(exponent) << 224) | wei;
    Ok(B256::from(word))
}

#[cfg(test)]
impl ClaimOrder {
    /// The claim order the coupon test vector names: test/TestCouponSignature.t.sol.
    pub fn vector() -> Self {
        Self {
            order_hash: "0x5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9"
                .parse()
                .unwrap(),
            order_owner: address!("E3E064e3C2EEf66cb93dA8D8114F5084E92F48D6"),
            orderbook: address!("37FC0EFec37D19f8A221aa4F8F7600C9ba2AcD20"),
            token: address!("6c6EE5e31d828De241282B9606C8e98Ea48526E2"),
            vault_id: "0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b"
                .parse()
                .unwrap(),
            signer: TEST_SIGNER,
            interpreter: address!("4c52eee7Fb6aeD0F1a30130d96d02409D9d886cf"),
            store: address!("1Aa775533E28B1D843e1A589034984E3a62005DC"),
            bytecode: Bytes::from_static(&[0x00, 0xab]),
            input_token: address!("1234567890123456789012345678901234567890"),
        }
    }
}

#[cfg(test)]
impl CouponSigner {
    pub fn with_key(key: impl CouponKey + 'static) -> Self {
        Self::for_order(key, ClaimOrder::vector())
    }

    pub fn for_order(key: impl CouponKey + 'static, order: ClaimOrder) -> Self {
        Self {
            key: Box::new(key),
            order,
            expiry_seconds: DEFAULT_EXPIRY_SECONDS,
        }
    }
}

/// What the signer signs: the EIP-191 hash of the packed words, as Raindex's
/// LibContext.build checks it.
pub(crate) fn context_digest(context: &[B256]) -> B256 {
    let packed: Vec<u8> = context.iter().flat_map(|word| word.0).collect();
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
fn withdrawal_nonce(withdrawal: &ActionHash) -> B256 {
    keccak256(withdrawal.get_raw_39())
}

fn pad_address(addr: Address) -> B256 {
    addr.into_word()
}

#[derive(Debug)]
pub struct Payout {
    recipient: Address,
    wei: U256,
}

impl Payout {
    /// `amount` is HOT as a decimal. HOT has 18 decimals on Ethereum.
    pub fn new(recipient: &str, amount: &str) -> Result<Self> {
        let recipient = recipient
            .parse()
            .with_context(|| format!("the recipient {recipient:?} is not an address"))?;
        let wei = hot_in_wei(amount)
            .with_context(|| format!("the amount {amount:?} is not a HOT amount"))?;
        anyhow::ensure!(!wei.is_zero(), "the amount is zero");
        Ok(Self { recipient, wei })
    }

    pub fn recipient(&self) -> Address {
        self.recipient
    }
}

fn hot_in_wei(amount: &str) -> Option<U256> {
    let (whole, fraction) = amount.split_once('.').unwrap_or((amount, "0"));
    let is_digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    if !is_digits(whole) || !is_digits(fraction) || fraction.len() > 18 {
        return None;
    }
    let ten = U256::from(10);
    whole
        .parse::<U256>()
        .ok()?
        .checked_mul(ten.pow(U256::from(18)))?
        .checked_add(fraction.parse::<U256>().ok()? * ten.pow(U256::from(18 - fraction.len())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_settings;
    use holo_hash::ActionHashB64;

    const NOW: u64 = 1_750_000_000;
    const TEST_SIGNER_KEY: &str =
        "0xdcbe53cbf4cbee212fe6339821058f2787c7726ae0684335118cdea2e8adaafd";

    async fn coupon(signer: &CouponSigner, withdrawal: &ActionHash, now: u64) -> String {
        let terms = Payout::new("0x1111111111111111111111111111111111111111", "1.5").unwrap();
        signer.coupon_at(terms, withdrawal, now).await.unwrap()
    }

    /// Context field 8, after the signer and signature.
    fn nonce(coupon: &str) -> &str {
        let fields: Vec<&str> = coupon.split(',').collect();
        assert_eq!(fields.len(), 11, "coupon is signer,signature,c0..c8");
        fields[10]
    }

    #[test]
    fn a_payout_is_a_positive_hot_amount_to_an_address() {
        let to = "0x1111111111111111111111111111111111111111";
        let wei = |amount| Payout::new(to, amount).map(|payout| payout.wei).ok();
        assert_eq!(
            wei("1.5"),
            Some(U256::from(15) * U256::from(10).pow(U256::from(17)))
        );
        assert_eq!(
            wei("5"),
            Some(U256::from(5) * U256::from(10).pow(U256::from(18)))
        );
        assert_eq!(wei("0.000000000000000001"), Some(U256::from(1)));
        for refused in [
            "",
            ".",
            "5.",
            ".5",
            "0",
            "0.0",
            "-5",
            "0x10",
            "1.5.0",
            " 1",
            "1e18",
            "0.0000000000000000001",
            &"9".repeat(60),
        ] {
            assert_eq!(wei(refused), None, "{refused:?}");
        }
        assert_eq!(
            format!("{:#}", Payout::new("0xdead", "1").unwrap_err()),
            "the recipient \"0xdead\" is not an address: invalid string length"
        );
    }

    /// The amount word of the test vector: 1.5 HOT is the wei with exponent -18.
    #[test]
    fn the_amount_word_is_the_wei_with_exponent_minus_18() {
        let word = amount_word(U256::from(1_500_000_000_000_000_000u128)).unwrap();

        assert_eq!(
            word,
            "0xffffffee000000000000000000000000000000000000000014d1120d7b160000"
                .parse::<B256>()
                .unwrap()
        );
        assert_eq!(
            amount_word(U256::from(1)).unwrap(),
            "0xffffffee00000000000000000000000000000000000000000000000000000001"
                .parse::<B256>()
                .unwrap()
        );
        let too_large = (U256::from(1) << 223) + U256::from(1);
        assert!(amount_word(too_large).is_err());
        assert!(amount_word(U256::from(1) << 223).is_err());
        assert!(amount_word((U256::from(1) << 223) - U256::from(1)).is_ok());
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

    /// The coupon test vector: anvil's third account signs the vector's words,
    /// and test/TestCouponSignature.t.sol signs the same with forge's `vm.sign`
    /// and expects this signature. ui/src/lib/coupon.test.ts reads this coupon.
    #[tokio::test]
    async fn a_key_signs_the_coupon_the_claim_order_checks() {
        let key: PrivateKeySigner =
            "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a"
                .parse()
                .unwrap();
        let signer = CouponSigner::with_key(key);

        let coupon = coupon(&signer, &ActionHash::from_raw_32(vec![7; 32]), NOW).await;

        assert_eq!(coupon, GOLDEN_COUPON);
        let fields: Vec<&str> = coupon.split(',').collect();
        let words: Vec<B256> = fields[2..]
            .iter()
            .map(|field| B256::from(field.parse::<U256>().unwrap()))
            .collect();
        assert_eq!(
            context_digest(&words),
            "0xf32a9e86bb3d75c00acec45f208001679a5f2a7976c5ce3327b6de7cecb0781f"
                .parse::<B256>()
                .unwrap()
        );
    }

    const GOLDEN_COUPON: &str = "0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc,0x452449c0ce920f0e30878832f19e55a7ad3c173dfb7074cef53cc1cc1db54cd43df1a26a92e096b59ba413f28cd2c0c1f4aa6276b56ea14eb612c07fcd76eb191b,97433442488726861213578988847752201310395502865,115792088752037155414859468704681636286916632540171964433728779348048145154048,1750604800,42941365433660945573526868378572896801276519612010928011051331832534249141753,1300945060633283894583661816534861012306758682838,319615586448914156470689800627502155326116515104,619044111217614189527117770386815914092009957090,108043606565222972236900316128309391016550688326814185311821020602083120460619,8496889498503184870230947373316602526857109467185425766089433217409414560631";

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
        let context: Vec<B256> = fields[2..]
            .iter()
            .map(|f| B256::from(f.parse::<U256>().unwrap()))
            .collect();
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

    /// The vector's claim order as a deploy record names it, with the test
    /// signer's key.
    const VECTOR_SIGNER_SETTINGS: [(&str, &str); 11] = [
        (
            "ORDER_HASH",
            "0x5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9",
        ),
        ("ORDER_OWNER", "0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6"),
        (
            "ORDERBOOK_ADDRESS",
            "0x37FC0EFec37D19f8A221aa4F8F7600C9ba2AcD20",
        ),
        (
            "TOKEN_ADDRESS",
            "0x6c6EE5e31d828De241282B9606C8e98Ea48526E2",
        ),
        (
            "VAULT_ID",
            "0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b",
        ),
        ("CLAIM_SIGNER", "0x8E72b7568738da52ca3DCd9b24E178127A4E7d37"),
        (
            "CLAIM_INTERPRETER",
            "0x4c52eee7Fb6aeD0F1a30130d96d02409D9d886cf",
        ),
        ("CLAIM_STORE", "0x1Aa775533E28B1D843e1A589034984E3a62005DC"),
        ("CLAIM_BYTECODE", "0x00ab"),
        (
            "CLAIM_INPUT_TOKEN",
            "0x1234567890123456789012345678901234567890",
        ),
        ("SIGNER_PRIVATE_KEY", TEST_SIGNER_KEY),
    ];

    fn settings_with(changes: &[(&'static str, Option<&str>)]) -> impl Fn(&str) -> Option<String> {
        let mut set: Vec<(&'static str, String)> = VECTOR_SIGNER_SETTINGS
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
    fn the_settings_parse_into_the_claim_order() {
        let signer = CouponSigner::from_settings(Network::BaseSepolia, settings_with(&[])).unwrap();

        assert_eq!(signer.order(), &ClaimOrder::vector());
        assert_eq!(signer.key.address(), TEST_SIGNER);
        assert_eq!(signer.expiry_seconds, DEFAULT_EXPIRY_SECONDS);
    }

    const MAINNET_KEY: &str = "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a";

    /// The vector's settings with a key mainnet takes, then `changes`.
    fn mainnet_settings_with(
        changes: &[(&'static str, Option<&str>)],
    ) -> impl Fn(&str) -> Option<String> {
        let mut all = vec![("SIGNER_PRIVATE_KEY", Some(MAINNET_KEY))];
        all.extend_from_slice(changes);
        settings_with(&all)
    }

    #[test]
    fn mainnet_names_each_unset_signer_variable() {
        for (key, _) in VECTOR_SIGNER_SETTINGS {
            let message = refusal(Network::Mainnet, mainnet_settings_with(&[(key, None)]));
            assert_eq!(message, format!("{key} is required"));
        }
    }

    /// TestNet's values are the ones the same on every chain: the orderbook, the
    /// interpreter, the store, the vault ID and the test signer. Its mock HOT,
    /// vault and claim order are chain-specific and have no defaults, so a
    /// base-sepolia run names those as mainnet does.
    #[test]
    fn base_sepolia_takes_testnet_values_for_what_is_the_same_on_every_chain() {
        let key = test_settings(&[("SIGNER_PRIVATE_KEY", TEST_SIGNER_KEY)]);

        assert_eq!(
            refusal(Network::BaseSepolia, key),
            "ORDER_HASH is required; ORDER_OWNER is required; TOKEN_ADDRESS is required; CLAIM_BYTECODE is required; CLAIM_INPUT_TOKEN is required"
        );
        assert_eq!(
            refusal(Network::BaseSepolia, test_settings(&[])),
            "ORDER_HASH is required; ORDER_OWNER is required; TOKEN_ADDRESS is required; CLAIM_BYTECODE is required; CLAIM_INPUT_TOKEN is required; SIGNER_PRIVATE_KEY is required"
        );
        let given = settings_with(&[
            ("ORDERBOOK_ADDRESS", None),
            ("VAULT_ID", None),
            ("CLAIM_SIGNER", None),
            ("CLAIM_INTERPRETER", None),
            ("CLAIM_STORE", None),
        ]);
        let signer = CouponSigner::from_settings(Network::BaseSepolia, given).unwrap();
        assert_eq!(signer.order(), &ClaimOrder::vector());
    }

    #[test]
    fn each_malformed_signer_variable_is_named() {
        for (key, _) in VECTOR_SIGNER_SETTINGS {
            let message = refusal(Network::BaseSepolia, settings_with(&[(key, Some("0xZZ"))]));
            assert!(message.starts_with(key), "{message}");
            assert!(!message.contains("is required"), "{message}");
        }
        for value in ["a week", "0", "31536001"] {
            let message = refusal(
                Network::BaseSepolia,
                settings_with(&[("EXPIRY_SECONDS", Some(value))]),
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
            Network::BaseSepolia,
            settings_with(&[("SIGNER_PRIVATE_KEY", Some(almost_a_key))]),
        );

        assert_eq!(message, "SIGNER_PRIVATE_KEY is not a secp256k1 private key");
    }

    #[test]
    fn mainnet_refuses_the_test_signer_and_base_sepolia_takes_it() {
        let message = refusal(Network::Mainnet, settings_with(&[]));
        assert!(
            message.contains("test signer 0x8E72b7568738da52ca3DCd9b24E178127A4E7d37"),
            "{message}"
        );
        assert!(CouponSigner::from_settings(Network::BaseSepolia, settings_with(&[])).is_ok());

        let mainnet = CouponSigner::from_settings(
            Network::Mainnet,
            settings_with(&[
                ("SIGNER_PRIVATE_KEY", Some(MAINNET_KEY)),
                (
                    "CLAIM_SIGNER",
                    Some("0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC"),
                ),
            ]),
        );
        assert!(mainnet.is_ok());
    }

    #[test]
    fn the_probe_coupon_pays_the_key_one_wei_and_expires_last() {
        let key: PrivateKeySigner = TEST_SIGNER_KEY.parse().unwrap();
        let signer = CouponSigner::with_key(key);

        let probe = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(signer.probe_coupon())
            .unwrap();

        assert_eq!(probe.signer, TEST_SIGNER);
        assert_eq!(probe.context[0], pad_address(TEST_SIGNER));
        assert_eq!(probe.context[1], amount_word(U256::from(1)).unwrap());
        // int224::MAX with exponent 0: the latest expiry a float holds as an integer.
        assert_eq!(
            probe.context[2],
            "0x000000007fffffffffffffffffffffffffffffffffffffffffffffffffffffff"
                .parse::<B256>()
                .unwrap()
        );
        let recovered = alloy::primitives::PrimitiveSignature::try_from(probe.signature.as_slice())
            .unwrap()
            .recover_address_from_prehash(&context_digest(&probe.context))
            .unwrap();
        assert_eq!(recovered, TEST_SIGNER);
    }

    #[test]
    fn expiry_seconds_is_read_when_set() {
        let signer = CouponSigner::from_settings(
            Network::BaseSepolia,
            settings_with(&[("EXPIRY_SECONDS", Some("3600"))]),
        )
        .unwrap();

        assert_eq!(signer.expiry_seconds.get(), 3600);
    }
}
