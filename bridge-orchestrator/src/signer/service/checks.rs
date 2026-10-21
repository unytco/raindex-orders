use super::config::ServiceConfig;
use super::Service;
use crate::orchestrator::{bridging_spend, vault_key, withdrawal_terms, BridgingSpend};
use crate::screening::{screen, Verdict};
use crate::signer::safe::{safe_message_hash, Identity, SignBody, Signed};
use crate::signer::{
    amount_word, context_digest, pad_address, probe_context, signature_of, withdrawal_nonce, Payout,
};
use alloy::primitives::{B256, U256};
use axum::http::StatusCode;
use holo_hash::{ActionHash, ActionHashB64};
use rave_engine::types::{ParkedLinkStanding, Transaction, TransactionDetails};
use sha2::{Digest, Sha256};
use tracing::{error, info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    AuthFailed,
    BadRequest,
    WrongSafeOrChain,
    WrongOwner,
    OrderMismatch,
    ProbeMismatch,
    NonceMismatch,
    ExpiryOutOfRange,
    HashMismatch,
    NotAWithdrawal,
    UnknownWithdrawal,
    NotReady,
    AmountMismatch,
    RecipientMismatch,
    RecipientListed,
    AlreadySigned,
}

impl Code {
    pub fn name(self) -> &'static str {
        match self {
            Code::AuthFailed => "auth_failed",
            Code::BadRequest => "bad_request",
            Code::WrongSafeOrChain => "wrong_safe_or_chain",
            Code::WrongOwner => "wrong_owner",
            Code::OrderMismatch => "order_mismatch",
            Code::ProbeMismatch => "probe_mismatch",
            Code::NonceMismatch => "nonce_mismatch",
            Code::ExpiryOutOfRange => "expiry_out_of_range",
            Code::HashMismatch => "hash_mismatch",
            Code::NotAWithdrawal => "not_a_withdrawal",
            Code::UnknownWithdrawal => "unknown_withdrawal",
            Code::NotReady => "not_ready",
            Code::AmountMismatch => "amount_mismatch",
            Code::RecipientMismatch => "recipient_mismatch",
            Code::RecipientListed => "recipient_listed",
            Code::AlreadySigned => "already_signed",
        }
    }

    pub fn status(self) -> StatusCode {
        match self {
            Code::AuthFailed => StatusCode::UNAUTHORIZED,
            Code::BadRequest => StatusCode::BAD_REQUEST,
            _ => StatusCode::CONFLICT,
        }
    }
}

#[derive(Debug)]
pub struct Refusal {
    pub code: Code,
    pub reason: String,
    withdrawal: Option<ActionHashB64>,
}

impl Refusal {
    fn new(code: Code, reason: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
            withdrawal: None,
        }
    }

    fn of(self, withdrawal: &ActionHash) -> Self {
        Self {
            withdrawal: Some(withdrawal.clone().into()),
            ..self
        }
    }

    fn log(&self) {
        let code = self.code.name();
        let withdrawal = self.withdrawal.as_ref().map(tracing::field::display);
        let asked = match &self.withdrawal {
            Some(withdrawal) => format!("the coupon of withdrawal {withdrawal}"),
            None => "a request".to_string(),
        };
        let reason = &self.reason;
        match self.code {
            Code::AuthFailed => warn!(
                event = "signer.unauthorized",
                "refused a request without this signer's token"
            ),
            // Both can clear on their own: the withdrawal can still reach this
            // node, and what is down can come back.
            Code::NotReady | Code::UnknownWithdrawal => warn!(
                event = "signer.waiting",
                code, withdrawal, "cannot sign {asked} yet, {code}: {reason}"
            ),
            _ => error!(
                event = "signer.refused",
                code, withdrawal, "refused {asked}, {code}: {reason}"
            ),
        }
    }
}

fn logged<T>(answer: Result<T, Refusal>) -> Result<T, Refusal> {
    if let Err(refusal) = &answer {
        refusal.log();
    }
    answer
}

impl Service {
    pub(super) fn identity(&self, authorization: Option<&str>) -> Result<Identity, Refusal> {
        logged(self.authorized(authorization).map(|()| Identity {
            owner: self.config.key.address(),
            safe: self.config.safe,
            chain_id: self.config.network.chain_id(),
        }))
    }

    /// Signs its own SafeMessage hash of the coupon, or answers the first check
    /// that fails.
    pub(super) async fn sign(
        &self,
        authorization: Option<&str>,
        body: &[u8],
        now: u64,
    ) -> Result<Signed, Refusal> {
        logged(self.checked(authorization, body, now).await)
    }

    async fn checked(
        &self,
        authorization: Option<&str>,
        body: &[u8],
        now: u64,
    ) -> Result<Signed, Refusal> {
        self.authorized(authorization)?;
        let (request, withdrawal) = parsed(body)?;
        let answer = self.answer(&request, withdrawal.as_ref(), now).await;
        match &withdrawal {
            Some(withdrawal) => answer.map_err(|refusal| refusal.of(withdrawal)),
            None => answer,
        }
    }

    async fn answer(
        &self,
        request: &SignBody,
        withdrawal: Option<&ActionHash>,
        now: u64,
    ) -> Result<Signed, Refusal> {
        let config = &self.config;
        let chain_id = config.network.chain_id();
        if request.safe != config.safe || request.chain_id != chain_id {
            return Err(Refusal::new(
                Code::WrongSafeOrChain,
                format!(
                    "the request is for Safe {} on chain {}, and this signer signs for Safe {} on chain {chain_id}",
                    request.safe, request.chain_id, config.safe
                ),
            ));
        }
        let owner = config.key.address();
        if request.owner != owner {
            return Err(Refusal::new(
                Code::WrongOwner,
                format!(
                    "the request asks owner {}, and this signer signs as owner {owner}",
                    request.owner
                ),
            ));
        }
        if let Some(word) = (3..8).find(|&word| request.context[word] != config.order[word - 3]) {
            return Err(Refusal::new(
                Code::OrderMismatch,
                format!(
                    "word {word} is {}, and this signer's claim order has {}",
                    request.context[word],
                    config.order[word - 3]
                ),
            ));
        }
        match withdrawal {
            None => self.probe(request),
            Some(withdrawal) => self.withdrawal(request, withdrawal, now).await,
        }
    }

    fn authorized(&self, authorization: Option<&str>) -> Result<(), Refusal> {
        // Digests are compared, so the time a refusal takes says nothing about the token.
        let digest = |value: &str| Sha256::digest(value.as_bytes());
        if authorization.map(digest) == Some(digest(&self.config.authorization)) {
            Ok(())
        } else {
            Err(Refusal::new(
                Code::AuthFailed,
                "the request does not carry this signer's token",
            ))
        }
    }

    fn probe(&self, request: &SignBody) -> Result<Signed, Refusal> {
        if probe_context(self.config.safe, self.config.order)
            .ok()
            .as_ref()
            != Some(&request.context)
        {
            return Err(Refusal::new(
                Code::ProbeMismatch,
                "withdrawal is null and the words are not the startup probe's",
            ));
        }
        let hash = self.own_hash(request)?;
        Ok(self.signed(&hash, None))
    }

    async fn withdrawal(
        &self,
        request: &SignBody,
        withdrawal: &ActionHash,
        now: u64,
    ) -> Result<Signed, Refusal> {
        let config = &self.config;
        let words = &request.context;
        if words[8] != withdrawal_nonce(withdrawal) {
            return Err(Refusal::new(
                Code::NonceMismatch,
                "word 8 is not keccak256 of the withdrawal's 39 bytes",
            ));
        }
        let expiry = U256::from_be_bytes(words[2].0);
        let tolerance = config.expiry_tolerance_seconds;
        let after = U256::from(now.saturating_sub(tolerance));
        let latest =
            U256::from(now) + U256::from(config.expiry_seconds.get()) + U256::from(tolerance);
        if expiry <= after || expiry > latest {
            return Err(Refusal::new(
                Code::ExpiryOutOfRange,
                format!("the expiry {expiry} is not after {after} and at most {latest}"),
            ));
        }
        let hash = self.own_hash(request)?;
        let spend = match self.cell.standing(withdrawal).await {
            Ok(ParkedLinkStanding::Parked(spend)) => spend,
            // Reissuing the coupon of a withdrawal that is no longer parked is
            // not handled here.
            Ok(standing @ (ParkedLinkStanding::Unparked | ParkedLinkStanding::NotAParkedLink)) => {
                return Err(Refusal::new(
                    Code::NotAWithdrawal,
                    format!(
                        "this signer's node answers {standing:?} for the withdrawal, not Parked"
                    ),
                ));
            }
            Ok(ParkedLinkStanding::NotHeld) => {
                return Err(Refusal::new(
                    Code::UnknownWithdrawal,
                    "this signer's node does not hold the withdrawal",
                ));
            }
            Err(err) => {
                return Err(Refusal::new(
                    Code::NotReady,
                    format!("this signer's cell did not answer: {err:#}"),
                ));
            }
        };
        let agreement = self
            .cell
            .bridging_agreement(&config.bridging_agent, config.hot_unit_index)
            .await
            .map_err(|err| {
                Refusal::new(
                    Code::NotReady,
                    format!("this signer cannot find the bridging agreement: {err:#}"),
                )
            })?;
        let not_paid = |why: String| {
            Refusal::new(
                Code::NotAWithdrawal,
                format!("the spend is not a withdrawal the bridge pays: {why}"),
            )
        };
        let payout = payout(&spend, &agreement, config).map_err(not_paid)?;
        let amount = amount_word(payout.wei).map_err(|err| not_paid(format!("{err:#}")))?;
        if words[1] != amount {
            return Err(Refusal::new(
                Code::AmountMismatch,
                format!(
                    "word 1 is {}, and this signer's word for the spend's HOT is {amount}",
                    words[1]
                ),
            ));
        }
        if words[0] != pad_address(payout.recipient) {
            return Err(Refusal::new(
                Code::RecipientMismatch,
                format!(
                    "word 0 is {}, and the spend's withdraw_to_address is {:#x}",
                    words[0], payout.recipient
                ),
            ));
        }
        if let Some(screener) = &config.screener {
            match screen(screener.as_ref(), &[payout.recipient]).await.verdict {
                Verdict::Clear => {}
                Verdict::Hit(_) => {
                    return Err(Refusal::new(
                        Code::RecipientListed,
                        format!(
                            "the recipient {:#x} is on this signer's {}",
                            payout.recipient,
                            screener.name()
                        ),
                    ));
                }
                Verdict::NoAnswer(cause) => {
                    return Err(Refusal::new(
                        Code::NotReady,
                        format!("the recipient could not be screened: {cause}"),
                    ));
                }
            }
        }
        match self.records.record(withdrawal, words[3], now) {
            Ok(None) => Ok(self.signed(&hash, Some(withdrawal))),
            Ok(Some(order)) => Err(Refusal::new(
                Code::AlreadySigned,
                format!("this signer signed the withdrawal for order hash {order}"),
            )),
            Err(err) => Err(Refusal::new(
                Code::NotReady,
                format!("this signer cannot use its records: {err:#}"),
            )),
        }
    }

    fn own_hash(&self, request: &SignBody) -> Result<B256, Refusal> {
        let digest = context_digest(&request.context);
        let hash = safe_message_hash(self.config.network.chain_id(), self.config.safe, &digest);
        let mismatch = |sent: &str| {
            Refusal::new(
                Code::HashMismatch,
                format!("{sent} is not the one this signer makes of the words"),
            )
        };
        if request.coupon_digest != digest {
            return Err(mismatch("couponDigest"));
        }
        if request.safe_message_hash != hash {
            return Err(mismatch("safeMessageHash"));
        }
        Ok(hash)
    }

    fn signed(&self, hash: &B256, withdrawal: Option<&ActionHash>) -> Signed {
        let signature =
            signature_of(&self.config.key, hash).expect("a secp256k1 key signs any 32-byte hash");
        match withdrawal {
            Some(withdrawal) => {
                let withdrawal = ActionHashB64::from(withdrawal.clone());
                info!(
                    event = "signer.signed",
                    withdrawal = %withdrawal,
                    "signed the coupon of withdrawal {withdrawal}"
                );
            }
            None => info!(event = "signer.signed", "signed the startup probe"),
        }
        Signed {
            owner: self.config.key.address(),
            signature: signature.into(),
        }
    }
}

fn parsed(body: &[u8]) -> Result<(SignBody, Option<ActionHash>), Refusal> {
    let bad = |reason: String| Refusal::new(Code::BadRequest, reason);
    let request: SignBody = serde_json::from_slice(body)
        .map_err(|err| bad(format!("the body is not a sign request: {err}")))?;
    if request.context.len() != 9 {
        return Err(bad(format!(
            "context holds {} words, not 9",
            request.context.len()
        )));
    }
    // holo_hash parses only the spelling it writes: 53 characters, no padding.
    let withdrawal = request
        .withdrawal
        .as_deref()
        .map(|raw| {
            ActionHashB64::from_b64_str(raw)
                .map(ActionHash::from)
                .map_err(|err| {
                    bad(format!(
                        "withdrawal {raw:?} is not a canonical action hash: {err}"
                    ))
                })
        })
        .transpose()?;
    Ok((request, withdrawal))
}

fn payout(
    spend: &Transaction,
    agreement: &ActionHashB64,
    config: &ServiceConfig,
) -> Result<Payout, String> {
    let vault = vault_key(config.order_owner);
    let (
        TransactionDetails::ParkedSpend {
            ea_id, executor, ..
        },
        Some(kind),
    ) = (
        &spend.details,
        bridging_spend(spend, &config.bridging_agent, Some(&vault)),
    )
    else {
        return Err("it is not a parked spend".to_string());
    };
    if ea_id != agreement {
        return Err(format!(
            "it is parked on agreement {ea_id}, not the bridging agreement {agreement}"
        ));
    }
    if *executor != config.bridging_agent {
        return Err(format!(
            "its executor is {executor}, not the bridging agent {}",
            config.bridging_agent
        ));
    }
    let to = match kind {
        BridgingSpend::Withdrawal(Some(to)) => to,
        BridgingSpend::Withdrawal(None) => {
            return Err("it names no withdraw_to_address".to_string())
        }
        BridgingSpend::OtherVault { why, .. } => return Err(format!("it {why}")),
        BridgingSpend::OtherRole(role) => {
            return Err(format!("its role is {role}, not withdrawer"))
        }
        BridgingSpend::Deposit => return Err("it is the bridging agent's deposit".to_string()),
    };
    withdrawal_terms(spend, to, config.hot_unit_index).map_err(|err| format!("{err:#}"))
}
