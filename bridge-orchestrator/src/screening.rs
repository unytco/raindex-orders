use alloy::primitives::Address;
use holo_hash::ActionHashB64;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::time::Duration;

#[cfg(not(test))]
const ANSWER_WITHIN: Duration = Duration::from_secs(10);
#[cfg(test)]
const ANSWER_WITHIN: Duration = Duration::from_millis(50);

pub type Answering<'a> = Pin<Box<dyn Future<Output = Vec<(Address, Answer)>> + Send + 'a>>;

/// Its `Display` names it in the `startup.screening` log.
pub trait Screener: Send + Sync + fmt::Display {
    fn name(&self) -> &'static str;
    fn answer<'a>(&'a self, addresses: &'a [Address]) -> Answering<'a>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Listed,
    NotListed,
    /// The list always answers. A provider that can fail answers this, and the transfer waits.
    #[allow(dead_code)]
    Failed(String),
}

#[derive(Debug)]
pub struct Screened {
    pub provider: &'static str,
    pub verdict: Verdict,
    /// False when the provider gave nothing within `ANSWER_WITHIN`.
    pub in_time: bool,
}

impl Screened {
    pub fn no_answer(provider: &'static str, cause: String) -> Self {
        Self {
            provider,
            verdict: Verdict::NoAnswer(cause),
            in_time: true,
        }
    }
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

/// A hit when any address is listed. Clear only when every address is answered and no answer
/// failed. Otherwise no verdict.
pub async fn screen(screener: &dyn Screener, addresses: &[Address]) -> Screened {
    let provider = screener.name();
    let Ok(answers) = tokio::time::timeout(ANSWER_WITHIN, screener.answer(addresses)).await else {
        return Screened {
            in_time: false,
            ..Screened::no_answer(provider, format!("no answer within {ANSWER_WITHIN:?}"))
        };
    };
    let mut causes = Vec::new();
    if addresses.is_empty() {
        causes.push("no address to screen".to_string());
    }
    let mut hits = Vec::new();
    for address in addresses {
        let given: Vec<&Answer> = answers
            .iter()
            .filter(|(answered, _)| answered == address)
            .map(|(_, answer)| answer)
            .collect();
        let failed = given.iter().find_map(|answer| match answer {
            Answer::Failed(cause) => Some(cause),
            _ => None,
        });
        if given.contains(&&Answer::Listed) {
            hits.push(*address);
        } else if let Some(cause) = failed {
            causes.push(format!("{address:#x}: {cause}"));
        } else if given.is_empty() {
            causes.push(format!("no answer for {address:#x}"));
        }
    }
    let verdict = if !hits.is_empty() {
        Verdict::Hit(hits)
    } else if causes.is_empty() {
        Verdict::Clear
    } else {
        Verdict::NoAnswer(causes.join("; "))
    };
    Screened {
        provider,
        verdict,
        in_time: true,
    }
}

#[derive(Debug)]
pub struct DenyList {
    addresses: Vec<Address>,
}

impl DenyList {
    pub fn new(addresses: impl IntoIterator<Item = Address>) -> Self {
        let mut addresses: Vec<Address> = addresses.into_iter().collect();
        addresses.sort();
        addresses.dedup();
        Self { addresses }
    }
}

impl fmt::Display for DenyList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} of {} address(es)", self.name(), self.addresses.len())
    }
}

impl Screener for DenyList {
    fn name(&self) -> &'static str {
        "denylist"
    }

    fn answer<'a>(&'a self, addresses: &'a [Address]) -> Answering<'a> {
        let answers = addresses
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
        Box::pin(std::future::ready(answers))
    }
}

/// `lock:<lockId>`, the lock row's item ID, or `withdrawal:<action hash>` of the spend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transfer {
    Lock(String),
    Withdrawal(String),
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
                Some(of) => Box::pin(std::future::ready(of.clone())),
                None => Box::pin(std::future::pending()),
            }
        }
    }

    #[tokio::test]
    async fn a_verdict_is_clear_only_when_every_address_is_answered_in_time() {
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

        let late = screen(&Provider(None), &[a]).await;
        assert_eq!(
            late.verdict,
            Verdict::NoAnswer("no answer within 50ms".into())
        );
        assert!(!late.in_time);
    }
}
