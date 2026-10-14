use alloy::primitives::Address;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;

pub type Screening<'a> = Pin<Box<dyn Future<Output = Screened> + Send + 'a>>;

/// A provider that screens a transfer's addresses. Its `Display` describes it for the startup log.
pub trait Screener: Send + Sync + fmt::Display {
    fn screen<'a>(&'a self, addresses: &'a [Address]) -> Screening<'a>;
}

pub struct Screened {
    pub provider: &'static str,
    pub list: Option<Arc<ListVersion>>,
    /// Unix seconds.
    pub at: i64,
    pub verdict: Verdict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Clear,
    Hit(Vec<Address>),
    /// The list always answers. A provider that can fail answers this, and the transfer waits.
    #[allow(dead_code)]
    NoAnswer(String),
}

impl Verdict {
    pub fn name(&self) -> &'static str {
        match self {
            Verdict::Clear => "clear",
            Verdict::Hit(_) => "hit",
            Verdict::NoAnswer(_) => "no_answer",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ListVersion {
    pub sha256: String,
    pub addresses: Vec<Address>,
}

impl ListVersion {
    /// Lowercased, sorted, deduplicated and comma-joined, as `SCREENING_DENYLIST` takes them.
    pub fn joined(&self) -> String {
        self.addresses
            .iter()
            .map(|address| format!("{address:#x}"))
            .collect::<Vec<_>>()
            .join(",")
    }
}

#[derive(Debug)]
pub struct DenyList {
    list: Arc<ListVersion>,
}

impl DenyList {
    pub const PROVIDER: &'static str = "denylist";

    pub fn new(addresses: impl IntoIterator<Item = Address>) -> Self {
        let mut addresses: Vec<Address> = addresses.into_iter().collect();
        addresses.sort();
        addresses.dedup();
        let mut list = ListVersion {
            sha256: String::new(),
            addresses,
        };
        list.sha256 = hex::encode(Sha256::digest(list.joined()));
        Self {
            list: Arc::new(list),
        }
    }
}

impl fmt::Display for DenyList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} of {} address(es), version {}",
            Self::PROVIDER,
            self.list.addresses.len(),
            self.list.sha256
        )
    }
}

impl Screener for DenyList {
    fn screen<'a>(&'a self, addresses: &'a [Address]) -> Screening<'a> {
        let listed: Vec<Address> = addresses
            .iter()
            .filter(|address| self.list.addresses.binary_search(address).is_ok())
            .copied()
            .collect();
        Box::pin(std::future::ready(Screened {
            provider: Self::PROVIDER,
            list: Some(Arc::clone(&self.list)),
            at: chrono::Utc::now().timestamp(),
            verdict: if listed.is_empty() {
                Verdict::Clear
            } else {
                Verdict::Hit(listed)
            },
        }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Sender,
    From,
    WithdrawToAddress,
}

/// `lock:<lockId>`, the lock row's item ID, or `withdrawal:<action hash>` of the spend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transfer {
    Lock(String),
    Withdrawal(String),
}

impl Transfer {
    pub fn of_lock_row(item_id: &str) -> Self {
        Transfer::Lock(item_id.trim_start_matches("lock:").to_string())
    }
}

impl fmt::Display for Transfer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Transfer::Lock(id) => write!(f, "lock:{id}"),
            Transfer::Withdrawal(spend) => write!(f, "withdrawal:{spend}"),
        }
    }
}

impl FromStr for Transfer {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match (s.strip_prefix("lock:"), s.strip_prefix("withdrawal:")) {
            (Some(id), _) if !id.is_empty() => Ok(Transfer::Lock(id.to_string())),
            (_, Some(spend)) if !spend.is_empty() => Ok(Transfer::Withdrawal(spend.to_string())),
            _ => Err(format!(
                "{s} is neither lock:<lockId> nor withdrawal:<action hash>"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Release,
    Block,
}

impl Action {
    pub fn event(self) -> &'static str {
        match self {
            Action::Release => "released",
            Action::Block => "blocked",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTED: &str = "0x8589427373d6d84e98730d7795d8f6f8731fda16";

    #[tokio::test]
    async fn the_list_matches_an_address_in_any_letter_case() {
        let list = DenyList::new([LISTED.parse().unwrap()]);
        let upper: Address = "0x8589427373D6D84E98730D7795D8F6F8731FDA16"
            .parse()
            .unwrap();
        let checksummed: Address = "0x8589427373D6D84E98730D7795D8f6f8731FDa16"
            .parse()
            .unwrap();
        let clean = Address::repeat_byte(0x11);

        let screened = list.screen(&[clean, upper]).await;

        assert_eq!(screened.verdict, Verdict::Hit(vec![upper]));
        assert_eq!(screened.provider, "denylist");
        assert_eq!(
            list.screen(&[checksummed]).await.verdict,
            Verdict::Hit(vec![checksummed])
        );
        assert_eq!(list.screen(&[clean]).await.verdict, Verdict::Clear);
    }

    #[test]
    fn one_set_of_addresses_has_one_version_whatever_its_case_order_or_repeats() {
        let a: Address = LISTED.parse().unwrap();
        let b = Address::repeat_byte(0xAB);
        let one = DenyList::new([a, b]);
        let other = DenyList::new([b, a, b]);

        assert_eq!(one.list, other.list);
        assert_eq!(
            one.list.joined(),
            format!("{LISTED},0xabababababababababababababababababababab")
        );
        assert_eq!(
            one.list.sha256,
            hex::encode(Sha256::digest(one.list.joined()))
        );
        assert_ne!(one.list.sha256, DenyList::new([a]).list.sha256);
    }

    #[tokio::test]
    async fn each_answer_names_its_list_version_and_time() {
        let list = DenyList::new([Address::repeat_byte(0xAB)]);
        let before = chrono::Utc::now().timestamp();

        let screened = list.screen(&[Address::repeat_byte(0x11)]).await;

        assert_eq!(screened.list.as_deref(), Some(list.list.as_ref()));
        assert!(screened.at >= before);
        assert_eq!(
            list.to_string(),
            format!("denylist of 1 address(es), version {}", list.list.sha256)
        );
    }

    #[test]
    fn a_transfer_id_names_a_lock_or_a_withdrawal() {
        for id in ["lock:7", "withdrawal:uhCkkAbC"] {
            assert_eq!(id.parse::<Transfer>().unwrap().to_string(), id);
        }
        assert_eq!(Transfer::of_lock_row("lock:7"), Transfer::Lock("7".into()));
        for bad in ["7", "lock:", "withdrawal:", "spend:uhCkk"] {
            assert!(bad.parse::<Transfer>().is_err(), "{bad}");
        }
    }
}
