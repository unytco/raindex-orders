use alloy::primitives::Address;
use holo_hash::ActionHashB64;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

#[cfg(not(test))]
const ANSWER_WITHIN: Duration = Duration::from_secs(10);
#[cfg(test)]
const ANSWER_WITHIN: Duration = Duration::from_millis(50);

pub type Answering<'a> = Pin<Box<dyn Future<Output = Answers> + Send + 'a>>;

/// Its `Display` names it in the `startup.screening` log.
pub trait Screener: Send + Sync + fmt::Display {
    fn name(&self) -> &'static str;
    fn answer<'a>(&'a self, addresses: &'a [Address]) -> Answering<'a>;
}

pub struct Answers {
    pub list: Option<Arc<ListVersion>>,
    /// Unix seconds.
    pub at: i64,
    pub of: Vec<(Address, Answer)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Listed,
    NotListed,
    /// The list always answers. A provider that can fail answers this, and the transfer waits.
    #[allow(dead_code)]
    Failed(String),
}

pub struct Screened {
    pub provider: &'static str,
    pub list: Option<Arc<ListVersion>>,
    pub at: i64,
    /// `None` for an address with no answer.
    pub listed: Vec<(Address, Option<bool>)>,
    pub verdict: Verdict,
    /// False when the provider gave nothing within its bound, so it is asked nothing more in the
    /// same pass or cycle.
    pub in_time: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Clear,
    Hit(Vec<Address>),
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

/// The verdict table: a hit when any address is listed, clear only when every address has an
/// answer and none is listed, and otherwise no verdict.
pub async fn screen(screener: &dyn Screener, addresses: &[Address]) -> Screened {
    let provider = screener.name();
    let Ok(answers) = tokio::time::timeout(ANSWER_WITHIN, screener.answer(addresses)).await else {
        return Screened {
            provider,
            list: None,
            at: chrono::Utc::now().timestamp(),
            listed: addresses.iter().map(|address| (*address, None)).collect(),
            verdict: Verdict::NoAnswer(format!("no answer within {ANSWER_WITHIN:?}")),
            in_time: false,
        };
    };
    let mut causes = Vec::new();
    if addresses.is_empty() {
        causes.push("no address to screen".to_string());
    }
    let listed: Vec<(Address, Option<bool>)> = addresses
        .iter()
        .map(|address| {
            let given: Vec<&Answer> = answers
                .of
                .iter()
                .filter(|(answered, _)| answered == address)
                .map(|(_, answer)| answer)
                .collect();
            let failed = given.iter().find_map(|answer| match answer {
                Answer::Failed(cause) => Some(cause),
                _ => None,
            });
            let listed = if given.contains(&&Answer::Listed) {
                Some(true)
            } else if let Some(cause) = failed {
                causes.push(format!("{address:#x}: {cause}"));
                None
            } else if given.is_empty() {
                causes.push(format!("no answer for {address:#x}"));
                None
            } else {
                Some(false)
            };
            (*address, listed)
        })
        .collect();
    let hits: Vec<Address> = listed
        .iter()
        .filter(|(_, listed)| *listed == Some(true))
        .map(|(address, _)| *address)
        .collect();
    let verdict = if !hits.is_empty() {
        Verdict::Hit(hits)
    } else if causes.is_empty() {
        Verdict::Clear
    } else {
        Verdict::NoAnswer(causes.join("; "))
    };
    Screened {
        provider,
        list: answers.list,
        at: answers.at,
        listed,
        verdict,
        in_time: true,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ListVersion {
    version: String,
    content: Option<String>,
}

impl ListVersion {
    /// The sha256 of `sorted`, lowercased and comma-joined, kept with that text.
    fn of_sorted(sorted: &[Address]) -> Self {
        let content = sorted
            .iter()
            .map(|address| format!("{address:#x}"))
            .collect::<Vec<_>>()
            .join(",");
        Self {
            version: hex::encode(Sha256::digest(&content)),
            content: Some(content),
        }
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn content(&self) -> Option<&str> {
        self.content.as_deref()
    }
}

#[derive(Debug)]
pub struct DenyList {
    addresses: Vec<Address>,
    list: Arc<ListVersion>,
}

impl DenyList {
    pub fn new(addresses: impl IntoIterator<Item = Address>) -> Self {
        let mut addresses: Vec<Address> = addresses.into_iter().collect();
        addresses.sort();
        addresses.dedup();
        let list = Arc::new(ListVersion::of_sorted(&addresses));
        Self { addresses, list }
    }
}

impl fmt::Display for DenyList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} of {} address(es), version {}",
            self.name(),
            self.addresses.len(),
            self.list.version
        )
    }
}

impl Screener for DenyList {
    fn name(&self) -> &'static str {
        "denylist"
    }

    fn answer<'a>(&'a self, addresses: &'a [Address]) -> Answering<'a> {
        let of = addresses
            .iter()
            .map(|address| {
                let answer = if self.addresses.binary_search(address).is_ok() {
                    Answer::Listed
                } else {
                    Answer::NotListed
                };
                (*address, answer)
            })
            .collect();
        Box::pin(std::future::ready(Answers {
            list: Some(Arc::clone(&self.list)),
            at: chrono::Utc::now().timestamp(),
            of,
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
        Transfer::Lock(item_id.strip_prefix("lock:").unwrap_or(item_id).to_string())
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
            (_, Some(spend)) => spend
                .parse::<ActionHashB64>()
                .map(|_| Transfer::Withdrawal(spend.to_string()))
                .map_err(|e| format!("{spend} is not a spend's action hash: {e:?}")),
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

    fn listed() -> Address {
        LISTED.parse().unwrap()
    }

    #[tokio::test]
    async fn the_list_hits_on_any_listed_address_and_clears_only_when_none_is() {
        let list = DenyList::new([listed()]);
        let clean = Address::repeat_byte(0x11);

        let hit = screen(&list, &[clean, listed()]).await;

        assert_eq!(hit.verdict, Verdict::Hit(vec![listed()]));
        assert_eq!(hit.provider, "denylist");
        assert_eq!(hit.listed, [(clean, Some(false)), (listed(), Some(true))]);
        assert_eq!(screen(&list, &[clean]).await.verdict, Verdict::Clear);
    }

    #[test]
    fn one_set_of_addresses_has_one_version_whatever_its_order_or_repeats() {
        let other = Address::repeat_byte(0x11);
        let one = DenyList::new([listed(), other]);

        assert_eq!(one.list, DenyList::new([other, listed(), other]).list);
        assert_eq!(
            one.list.content(),
            Some(format!("0x1111111111111111111111111111111111111111,{LISTED}").as_str())
        );
        assert_eq!(
            one.list.version(),
            "935f7ea6880f70e19c0ac696f55eac56d6d0d57f115231aee88be7a1b8250af3",
            "printf '%s' <content> | sha256sum"
        );
        assert_ne!(one.list.version(), DenyList::new([listed()]).list.version());
    }

    #[tokio::test]
    async fn each_answer_names_its_list_version_and_time() {
        let list = DenyList::new([listed()]);
        let before = chrono::Utc::now().timestamp();

        let screened = screen(&list, &[Address::repeat_byte(0x11)]).await;

        assert_eq!(screened.list.as_deref(), Some(list.list.as_ref()));
        assert!(screened.at >= before);
        assert_eq!(
            list.to_string(),
            format!("denylist of 1 address(es), version {}", list.list.version())
        );
    }

    /// Gives these answers whatever it is asked, or none ever.
    struct Provider(Option<Vec<(Address, Answer)>>);

    impl fmt::Display for Provider {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("test provider")
        }
    }

    impl Screener for Provider {
        fn name(&self) -> &'static str {
            "test"
        }

        fn answer<'a>(&'a self, _: &'a [Address]) -> Answering<'a> {
            match &self.0 {
                Some(of) => Box::pin(std::future::ready(Answers {
                    list: None,
                    at: 7,
                    of: of.clone(),
                })),
                None => Box::pin(std::future::pending()),
            }
        }
    }

    #[tokio::test]
    async fn a_verdict_is_clear_only_when_every_address_is_answered() {
        let (a, b) = (Address::repeat_byte(0xA), Address::repeat_byte(0xB));
        let verdict = |of: Vec<(Address, Answer)>| async {
            screen(&Provider(Some(of)), &[a, b]).await.verdict
        };

        assert_eq!(
            verdict(vec![(a, Answer::NotListed), (b, Answer::NotListed)]).await,
            Verdict::Clear
        );
        assert_eq!(
            verdict(vec![(a, Answer::NotListed)]).await,
            Verdict::NoAnswer(format!("no answer for {b:#x}"))
        );
        assert_eq!(
            verdict(vec![
                (a, Answer::NotListed),
                (b, Answer::Failed("down".into()))
            ])
            .await,
            Verdict::NoAnswer(format!("{b:#x}: down"))
        );
        assert_eq!(
            verdict(vec![
                (a, Answer::Listed),
                (b, Answer::Failed("down".into()))
            ])
            .await,
            Verdict::Hit(vec![a]),
            "one listed address holds the transfer whatever the rest"
        );
        assert_eq!(
            verdict(vec![
                (a, Answer::NotListed),
                (b, Answer::NotListed),
                (a, Answer::Listed)
            ])
            .await,
            Verdict::Hit(vec![a]),
            "a later answer that lists an address is not lost behind an earlier one"
        );
        assert_eq!(
            verdict(vec![
                (a, Answer::NotListed),
                (b, Answer::NotListed),
                (b, Answer::Failed("one list is down".into()))
            ])
            .await,
            Verdict::NoAnswer(format!("{b:#x}: one list is down"))
        );
        assert_eq!(
            screen(&Provider(Some(vec![])), &[]).await.verdict,
            Verdict::NoAnswer("no address to screen".into())
        );
    }

    #[tokio::test]
    async fn a_provider_that_does_not_answer_in_time_gives_no_verdict() {
        let a = Address::repeat_byte(0xA);

        let screened = screen(&Provider(None), &[a]).await;

        assert_eq!(
            screened.verdict,
            Verdict::NoAnswer("no answer within 50ms".into())
        );
        assert!(!screened.in_time);
        assert_eq!(screened.listed, [(a, None)]);
        assert_eq!(screened.provider, "test");
    }

    #[test]
    fn a_transfer_id_names_a_lock_or_a_withdrawal() {
        let spend = holo_hash::ActionHash::from_raw_32(vec![7; 32]).to_string();
        for id in ["lock:7".to_string(), format!("withdrawal:{spend}")] {
            assert_eq!(id.parse::<Transfer>().unwrap().to_string(), id);
        }
        let mistyped = format!("withdrawal:{}", &spend[..spend.len() - 1]);
        assert!(mistyped.parse::<Transfer>().is_err(), "{mistyped}");
        assert_eq!(Transfer::of_lock_row("lock:7"), Transfer::Lock("7".into()));
        assert_eq!(
            Transfer::of_lock_row("lock:lock:7").to_string(),
            "lock:lock:7"
        );
        for bad in [
            "7",
            "lock:",
            "withdrawal:",
            "withdrawal:uhCkkAbC",
            "spend:uhCkk",
        ] {
            assert!(bad.parse::<Transfer>().is_err(), "{bad}");
        }
    }
}
