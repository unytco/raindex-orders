//! A Safe as the claim order's coupon signer: its owners sign the SafeMessage
//! hash of each coupon, here with keys this process holds or at their own signer
//! services, and the Safe's EIP-1271 `isValidSignature` checks M of them
//! (docs/signer-service.md).

use super::{SignRequest, Signing, SIGN_TIMEOUT};
use crate::config::Network;
use crate::preflight::RPC_TIMEOUT;
use alloy::primitives::{keccak256, Address, Bytes, PrimitiveSignature, B256, U256};
use alloy::providers::{ProviderBuilder, RootProvider};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use alloy::sol;
use alloy::sol_types::SolValue;
use alloy::transports::http::{Client, Http};
use anyhow::{anyhow, bail, Context, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use holo_hash::ActionHashB64;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

sol! {
    /// What a Safe answers about itself, and the EIP-1271 check its
    /// CompatibilityFallbackHandler answers for it.
    #[sol(rpc)]
    interface ISafe {
        function getThreshold() external view returns (uint256);
        function getOwners() external view returns (address[] memory);
        function domainSeparator() external view returns (bytes32);
        function isValidSignature(bytes32 dataHash, bytes calldata signature) external view returns (bytes4);
    }
}

/// EIP-1271's answer for a valid signature.
pub const EIP1271_MAGIC_VALUE: [u8; 4] = [0x16, 0x26, 0xba, 0x7e];

/// A Safe coupon signer has at least 2 owners to ask, its threshold being 2 or
/// more, and the website reads at most 20 owner signatures.
/// The most a signer service's answer may hold. A signature answer is under
/// 300 bytes; a refusal's reason is cut to 200 characters in the log anyway.
const MAX_ANSWER_BYTES: usize = 64 * 1024;

const MIN_OWNERS: usize = 2;
const MAX_OWNERS: usize = 20;

/// How long a signer service has to answer `GET /signer`. The answer reads no
/// Holochain node, so a service that takes longer is down, and the startup
/// check, which asks every service at once, waits this long at most for them.
pub const IDENTITY_TIMEOUT: Duration = Duration::from_secs(5);

/// Why two sets of `threshold` of a Safe's `owners` owners can share none, or
/// nothing when its threshold is more than half its owners. Each signer
/// service refuses a withdrawal it signed for another claim order hash, so
/// after a rotation the owners that signed a withdrawal for the old order
/// refuse it for the new one. Only a threshold of more than half the owners
/// puts one of them in every set that can sign
/// (docs/signer-service.md#one-order-per-withdrawal).
pub fn disjoint_quorums(safe: Address, threshold: usize, owners: usize) -> Option<String> {
    (threshold.saturating_mul(2) <= owners).then(|| {
        format!(
            "CLAIM_SIGNER {safe} is a Safe of threshold {threshold} with {owners} owners, so two sets of {threshold} of its owners can share none: after a claim order rotation, owners that never signed a withdrawal could sign it again for the new order. Its threshold must be more than half its owners (docs/signer-service.md#one-order-per-withdrawal)"
        )
    })
}

/// A Safe's EIP-712 domain separator, as Safe 1.3.0 and later compute it:
/// `EIP712Domain(uint256 chainId,address verifyingContract)`.
pub fn safe_domain_separator(chain_id: u64, safe: Address) -> B256 {
    keccak256(
        (
            keccak256(b"EIP712Domain(uint256 chainId,address verifyingContract)"),
            U256::from(chain_id),
            safe,
        )
            .abi_encode(),
    )
}

/// The hash a Safe's CompatibilityFallbackHandler has its owners sign for
/// `isValidSignature(digest, signature)` to accept: `getMessageHashForSafe(safe,
/// abi.encode(digest))`, the EIP-712 hash of `SafeMessage(bytes message)` with the
/// digest's 32 bytes as the message, under the Safe's domain.
pub fn safe_message_hash(chain_id: u64, safe: Address, digest: &B256) -> B256 {
    let message = keccak256(
        (
            keccak256(b"SafeMessage(bytes message)"),
            keccak256(digest.abi_encode()),
        )
            .abi_encode(),
    );
    keccak256(
        [
            &[0x19, 0x01][..],
            safe_domain_separator(chain_id, safe).as_slice(),
            message.as_slice(),
        ]
        .concat(),
    )
}

/// One owner of the Safe, signing here with its key or at its signer service.
pub enum Owner {
    Key(PrivateKeySigner),
    Service(OwnerService),
}

impl Owner {
    pub fn address(&self) -> Address {
        match self {
            Owner::Key(key) => Signer::address(key),
            Owner::Service(service) => service.owner,
        }
    }

    /// The owner and where it signs, for logs: here, or the service's host.
    pub fn name(&self) -> String {
        match self {
            Owner::Key(_) => format!("{} (a key held here)", self.address()),
            Owner::Service(service) => {
                format!("{} (signer service at {})", self.address(), service.host())
            }
        }
    }

    async fn sign(&self, request: &OwnerRequest<'_>) -> Result<Vec<u8>> {
        match self {
            Owner::Key(key) => {
                let signature = key.sign_hash(&request.safe_message_hash).await?;
                let mut bytes = Vec::with_capacity(65);
                bytes.extend_from_slice(&signature.r().to_be_bytes::<32>());
                bytes.extend_from_slice(&signature.s().to_be_bytes::<32>());
                bytes.push(if signature.v() { 28 } else { 27 });
                Ok(bytes)
            }
            Owner::Service(service) => service.request_signature(request).await,
        }
    }
}

/// What one owner is asked to sign for one coupon.
pub struct OwnerRequest<'a> {
    pub safe: Address,
    pub chain_id: u64,
    pub sign: &'a SignRequest,
    pub safe_message_hash: B256,
}

/// An owner that did not answer within SIGN_TIMEOUT.
#[derive(Debug)]
struct TimedOut;

impl std::fmt::Display for TimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "did not answer within {SIGN_TIMEOUT:?}")
    }
}

impl std::error::Error for TimedOut {}

/// An owner that was not asked this cycle, as one that timed out earlier in
/// it. Named in a coupon's error like any other, but not warned about on each
/// coupon: the timeout was warned about on the coupon it happened on.
#[derive(Debug)]
struct NotAsked(String);

impl std::fmt::Display for NotAsked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotAsked {}

/// One owner's signature, checked: it recovers to the owner over the coupon's
/// SafeMessage hash, and the owner is one of the Safe's.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnerSignature {
    pub owner: Address,
    pub signature: [u8; 65],
}

/// The Safe CLAIM_SIGNER and the owners this orchestrator asks to sign for it.
/// Each coupon asks every owner at once, checks each answer, and takes the M
/// lowest owner addresses that signed, M being the Safe's threshold read for that
/// coupon, sorted as the Safe requires. Then it asks the Safe's own
/// `isValidSignature`, so a coupon the Safe would refuse is never handed out.
///
/// Owner keys held here are one party, however many: protection comes from
/// owners at independent signer services, each checking the withdrawal on its
/// own Holochain node before it signs.
///
/// An owner that did not answer within SIGN_TIMEOUT is not asked again for the
/// rest of the cycle while the other owners are at least the threshold, so a
/// silent service costs one timeout per cycle, not one per withdrawal. The
/// startup check leaves out, until the first cycle, an owner whose service did
/// not answer `GET /signer`.
///
/// A Safe whose threshold is half its owners or fewer signs nothing
/// (`disjoint_quorums`), UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes or not.
pub struct SafeOwners {
    safe: Address,
    chain_id: u64,
    owners: Vec<Owner>,
    rpc_url: String,
    /// UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes: on mainnet, start even when this
    /// process alone can sign as the Safe, or holds owner keys of a Safe that is
    /// also the vault admin.
    one_party_allowed: bool,
    /// The owners left out this cycle, each with why: it did not answer within
    /// `sign_timeout`, or its service did not answer `GET /signer` at startup.
    left_out: Mutex<HashMap<Address, String>>,
    /// How long each owner has to sign: SIGN_TIMEOUT, shorter in tests.
    sign_timeout: std::time::Duration,
    /// How long the RPC has to answer each read of the Safe for a coupon:
    /// RPC_TIMEOUT, shorter in tests. Nothing else bounds those reads, and an
    /// RPC that took one and never answered would hold the bridge cycle up for
    /// good, the deposits behind the withdrawal included.
    rpc_timeout: Duration,
    /// The read of the Safe that did not answer within `rpc_timeout` this
    /// cycle, if one did not. The other coupons of the cycle are not read: each
    /// would cost the cycle another `rpc_timeout`. A new cycle reads again.
    rpc_down: Mutex<Option<String>>,
}

impl std::fmt::Debug for SafeOwners {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SafeOwners")
            .field("safe", &self.safe)
            .field("chain_id", &self.chain_id)
            .field("owners", &self.addresses())
            .finish()
    }
}

/// What a Safe answers now: its threshold and owners.
pub struct SafeState {
    pub threshold: usize,
    pub owners: Vec<Address>,
}

impl SafeOwners {
    /// `owners` of `safe` on chain `chain_id`, read and checked through
    /// `rpc_url`. Refuses fewer than 2 owners, more than 20, or one twice.
    pub fn new(safe: Address, chain_id: u64, owners: Vec<Owner>, rpc_url: String) -> Result<Self> {
        let keys = owners.iter().filter(|o| matches!(o, Owner::Key(_))).count();
        if keys == owners.len() {
            anyhow::ensure!(
                (MIN_OWNERS..=MAX_OWNERS).contains(&keys),
                "SAFE_OWNER_KEYS holds {keys} keys: a Safe coupon signer needs the keys of its threshold of owners, 2 to 20, or SAFE_OWNER_SERVICES"
            );
        } else {
            anyhow::ensure!(
                (MIN_OWNERS..=MAX_OWNERS).contains(&owners.len()),
                "SAFE_OWNER_KEYS and SAFE_OWNER_SERVICES name {} owners: a Safe coupon signer asks at least its threshold of owners, 2 to 20",
                owners.len()
            );
        }
        for (i, owner) in owners.iter().enumerate() {
            let address = owner.address();
            if let Some(first) = owners[..i].iter().find(|o| o.address() == address) {
                anyhow::bail!(match (first, owner) {
                    (Owner::Key(_), Owner::Key(_)) => format!("SAFE_OWNER_KEYS holds the key of {address} twice"),
                    (Owner::Service(_), Owner::Service(_)) =>
                        format!("SAFE_OWNER_SERVICES names {address} twice"),
                    _ => format!(
                        "{address} is in both SAFE_OWNER_KEYS and SAFE_OWNER_SERVICES: each owner signs in one place"
                    ),
                });
            }
        }
        Ok(Self {
            safe,
            chain_id,
            owners,
            rpc_url,
            one_party_allowed: false,
            left_out: Mutex::default(),
            sign_timeout: SIGN_TIMEOUT,
            rpc_timeout: RPC_TIMEOUT,
            rpc_down: Mutex::default(),
        })
    }

    /// These owners, with UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes when `allowed`.
    pub fn allowing_one_party(self, allowed: bool) -> Self {
        Self {
            one_party_allowed: allowed,
            ..self
        }
    }

    /// These owners, each given `timeout` to sign.
    #[cfg(test)]
    pub fn with_sign_timeout(self, timeout: std::time::Duration) -> Self {
        Self {
            sign_timeout: timeout,
            ..self
        }
    }

    /// These owners, whose RPC is given `timeout` to answer each read of the Safe.
    #[cfg(test)]
    pub fn with_rpc_timeout(self, timeout: Duration) -> Self {
        Self {
            rpc_timeout: timeout,
            ..self
        }
    }

    /// A new cycle: every owner is asked again, those left out included, and
    /// the Safe is read again after an RPC timeout.
    pub fn new_cycle(&self) {
        self.left_out.lock().unwrap().clear();
        *self.rpc_down.lock().unwrap() = None;
    }

    /// Leaves `owner` out until the next cycle, as one that timed out, `why`
    /// saying what it did not answer.
    pub fn leave_out(&self, owner: Address, why: String) {
        self.left_out.lock().unwrap().insert(owner, why);
    }

    /// Whether UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes is set.
    pub fn one_party_allowed(&self) -> bool {
        self.one_party_allowed
    }

    pub fn safe(&self) -> Address {
        self.safe
    }

    pub fn owners(&self) -> &[Owner] {
        &self.owners
    }

    pub fn addresses(&self) -> Vec<Address> {
        self.owners.iter().map(Owner::address).collect()
    }

    /// The owners whose keys this process holds.
    pub fn local_keys(&self) -> usize {
        self.owners
            .iter()
            .filter(|o| matches!(o, Owner::Key(_)))
            .count()
    }

    /// The hash the owners sign for `digest`: its SafeMessage hash.
    pub fn message_hash(&self, digest: &B256) -> B256 {
        safe_message_hash(self.chain_id, self.safe, digest)
    }

    fn provider(&self) -> Result<RootProvider<Http<Client>>> {
        let url = self
            .rpc_url
            .parse()
            .map_err(|_| anyhow!("the RPC URL for CLAIM_SIGNER {} is not a URL", self.safe))?;
        Ok(ProviderBuilder::new().on_http(url))
    }

    /// `read`, a read of the Safe through the RPC, within `rpc_timeout`. The
    /// RPC not answering is an error like any other: the withdrawal stays
    /// parked for the cycle, and the cycle goes on. It is also the cycle's one
    /// RPC timeout: the reads for its other coupons fail at once, naming the
    /// read that did not answer, until `new_cycle`.
    async fn within<T>(
        &self,
        what: &str,
        read: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        if let Some(down) = self.rpc_down.lock().unwrap().as_ref() {
            bail!("CLAIM_SIGNER {}: {what} was not read: {down}", self.safe);
        }
        match tokio::time::timeout(self.rpc_timeout, read).await {
            Ok(answer) => answer,
            Err(_) => {
                let timed_out = format!("{what} did not answer within {:?}", self.rpc_timeout);
                *self.rpc_down.lock().unwrap() = Some(format!("{timed_out} earlier this cycle"));
                bail!("CLAIM_SIGNER {}: {timed_out}", self.safe)
            }
        }
    }

    /// The Safe's threshold and owners, as it answers them now.
    pub async fn state(&self) -> Result<SafeState> {
        let provider = self.provider()?;
        let contract = ISafe::new(self.safe, &provider);
        let failed = |what: &str| format!("CLAIM_SIGNER {}: {what} failed", self.safe);
        self.within("getThreshold() and getOwners()", async {
            let threshold = contract
                .getThreshold()
                .call()
                .await
                .map_err(|_| anyhow!(failed("getThreshold()")))?
                ._0;
            let owners = contract
                .getOwners()
                .call()
                .await
                .map_err(|_| anyhow!(failed("getOwners()")))?
                ._0;
            Ok(SafeState {
                threshold: usize::try_from(threshold).unwrap_or(usize::MAX),
                owners,
            })
        })
        .await
    }

    /// Whether the Safe's `isValidSignature(digest, signature)` answers the
    /// EIP-1271 magic value: what Raindex V6 asks it for a coupon.
    pub async fn accepts(&self, digest: &B256, signature: &[u8]) -> Result<bool> {
        let provider = self.provider()?;
        let answer = self
            .within("isValidSignature(...)", async {
                Ok(ISafe::new(self.safe, &provider)
                    .isValidSignature(*digest, Bytes::copy_from_slice(signature))
                    .call()
                    .await)
            })
            .await?;
        match answer {
            Ok(answer) => Ok(answer._0.0 == EIP1271_MAGIC_VALUE),
            Err(alloy::contract::Error::TransportError(
                alloy::transports::RpcError::ErrorResp(_),
            )) => Ok(false),
            Err(_) => Err(anyhow!(
                "CLAIM_SIGNER {}: isValidSignature(...) could not be asked",
                self.safe
            )),
        }
    }

    /// Asks every owner at once to sign `request`'s SafeMessage hash, each within
    /// SIGN_TIMEOUT, and checks each answer against the Safe's owners now. An
    /// owner left out this cycle is not asked while the others are at least the
    /// Safe's threshold. Returns the Safe's state and, for each owner, its
    /// checked signature or why not, in the owners' order.
    ///
    /// The startup probe, a request with no withdrawal, ends as soon as the
    /// threshold of owners have signed, leaving out the owners still to answer:
    /// it needs M signatures, and a service that never answers would otherwise
    /// hold the startup check up for its SIGN_TIMEOUT. A withdrawal waits for
    /// every owner, so that each refusal is logged.
    ///
    /// A Safe whose threshold is half its owners or fewer is refused before any
    /// owner is asked. UNSAFE_ALLOW_ONE_PARTY_SIGNER does not lift this: it is
    /// about who holds the keys, and this is about paying one withdrawal twice.
    pub async fn collect(
        &self,
        request: &SignRequest,
    ) -> Result<(SafeState, Vec<(Address, Result<OwnerSignature>)>)> {
        let state = self.state().await?;
        if let Some(fault) = disjoint_quorums(self.safe, state.threshold, state.owners.len()) {
            bail!("{fault}. No owner was asked");
        }
        let owner_request = OwnerRequest {
            safe: self.safe,
            chain_id: self.chain_id,
            sign: request,
            safe_message_hash: self.message_hash(&request.digest),
        };
        let owner_request = &owner_request;
        let state_ref = &state;
        let skipped = {
            let left_out = self.left_out.lock().unwrap();
            let others = self
                .owners
                .iter()
                .filter(|o| !left_out.contains_key(&o.address()))
                .count();
            if others >= state.threshold {
                left_out.clone()
            } else {
                HashMap::new()
            }
        };
        let skipped = &skipped;
        let mut pending =
            self.owners
                .iter()
                .enumerate()
                .map(|(i, owner)| async move {
                    let address = owner.address();
                    if let Some(why) = skipped.get(&address) {
                        let err = NotAsked(format!("{} was not asked: {why}", owner.name()));
                        return (i, address, Err(anyhow::Error::new(err)), None);
                    }
                    let (answer, timed_out) =
                        match tokio::time::timeout(self.sign_timeout, owner.sign(owner_request))
                            .await
                        {
                            Ok(Ok(signature)) => (
                                checked(
                                    self.safe,
                                    address,
                                    &signature,
                                    &owner_request.safe_message_hash,
                                    &state_ref.owners,
                                )
                                .with_context(|| owner.name()),
                                false,
                            ),
                            Ok(Err(err)) => {
                                let timed_out = err.downcast_ref::<TimedOut>().is_some();
                                (Err(err.context(owner.name())), timed_out)
                            }
                            Err(_) => (
                                Err(anyhow!(
                                    "{} did not sign within {:?}",
                                    owner.name(),
                                    self.sign_timeout
                                )),
                                true,
                            ),
                        };
                    (i, address, answer, Some(timed_out))
                })
                .collect::<FuturesUnordered<_>>();
        let enough = request.withdrawal.is_none().then_some(state.threshold);
        let mut answers = Vec::with_capacity(self.owners.len());
        let mut signed = 0;
        while let Some(answer) = pending.next().await {
            signed += usize::from(answer.2.is_ok());
            answers.push(answer);
            if enough.is_some_and(|m| signed >= m) {
                break;
            }
        }
        drop(pending);
        answers.sort_by_key(|(i, ..)| *i);
        let mut left_out = self.left_out.lock().unwrap();
        let answers = answers
            .into_iter()
            .map(|(_, address, answer, asked)| {
                match asked {
                    Some(true) => {
                        left_out.insert(
                            address,
                            format!(
                                "it did not answer within {:?} earlier this cycle",
                                self.sign_timeout
                            ),
                        );
                    }
                    Some(false) => {
                        left_out.remove(&address);
                    }
                    None => {}
                }
                (address, answer)
            })
            .collect();
        drop(left_out);
        Ok((state, answers))
    }

    /// The Safe's signature of `request`: M owner signatures, the M lowest owner
    /// addresses of those that signed, ascending, which the Safe has accepted.
    /// Fewer than M is an error naming why each other owner did not sign. Each
    /// owner that was asked and did not sign is warned about; one not asked
    /// this cycle is not, as its timeout was.
    pub async fn sign_request(&self, request: &SignRequest) -> Result<Vec<u8>> {
        let (state, answers) = self.collect(request).await?;
        let threshold = state.threshold;
        let mut signed = Vec::new();
        let mut failures = Vec::new();
        for (_, answer) in answers {
            match answer {
                Ok(signature) => signed.push(signature),
                Err(err) => {
                    let asked = err.downcast_ref::<NotAsked>().is_none();
                    failures.push((asked, format!("{err:#}")));
                }
            }
        }
        anyhow::ensure!(
            threshold >= 1 && signed.len() >= threshold,
            "{} of the {} owners asked signed, and CLAIM_SIGNER {} needs {threshold}: {}",
            signed.len(),
            self.owners.len(),
            self.safe,
            failures
                .iter()
                .map(|(_, why)| why.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
        for (_, failure) in failures.iter().filter(|(asked, _)| *asked) {
            tracing::warn!(
                event = "coupon.owner_did_not_sign",
                safe = %self.safe,
                "an owner of CLAIM_SIGNER did not sign a coupon, which {threshold} others signed: {failure}"
            );
        }
        let signature = joined(signed, threshold);
        anyhow::ensure!(
            self.accepts(&request.digest, &signature).await?,
            "CLAIM_SIGNER {} refused the signatures of {threshold} of its owners: its isValidSignature did not accept them",
            self.safe
        );
        Ok(signature)
    }
}

/// The `threshold` lowest owners of `signed`, ascending, their signatures joined
/// as the Safe reads them.
pub fn joined(mut signed: Vec<OwnerSignature>, threshold: usize) -> Vec<u8> {
    signed.sort_by_key(|s| s.owner);
    signed.dedup_by_key(|s| s.owner);
    signed
        .iter()
        .take(threshold)
        .flat_map(|s| s.signature)
        .collect()
}

/// `signature` as an owner's signature the Safe reads: 65 bytes r, s, v, by
/// `owner`, over `hash` (v 27 or 28; 0 or 1 is taken as those) or over its
/// EIP-191 message hash (v 31 or 32, Safe's eth_sign form), and `owner` one of
/// `owners`.
fn checked(
    safe: Address,
    owner: Address,
    signature: &[u8],
    hash: &B256,
    owners: &[Address],
) -> Result<OwnerSignature> {
    let mut bytes: [u8; 65] = signature.try_into().map_err(|_| {
        anyhow!(
            "answered {} bytes, not a 65-byte signature",
            signature.len()
        )
    })?;
    if bytes[64] < 2 {
        bytes[64] += 27;
    }
    let (v, signed) = match bytes[64] {
        27 | 28 => (bytes[64], *hash),
        31 | 32 => (
            bytes[64] - 4,
            keccak256(
                [
                    b"\x19Ethereum Signed Message:\n32".as_slice(),
                    hash.as_slice(),
                ]
                .concat(),
            ),
        ),
        other => anyhow::bail!("answered a signature whose v is {other}, not 27 or 28"),
    };
    let mut plain = bytes;
    plain[64] = v;
    let recovered = PrimitiveSignature::try_from(plain.as_slice())
        .ok()
        .and_then(|s| s.recover_address_from_prehash(&signed).ok());
    anyhow::ensure!(
        recovered == Some(owner),
        "answered a signature that is not {owner}'s over the coupon's SafeMessage hash {hash}"
    );
    anyhow::ensure!(
        owners.contains(&owner),
        "{owner} is not an owner of CLAIM_SIGNER {safe} now"
    );
    Ok(OwnerSignature {
        owner,
        signature: bytes,
    })
}

/// One owner of the Safe, signing at an independent party's signer service,
/// which holds the owner's key and checks each withdrawal on its own Holochain
/// node before it signs (docs/signer-service.md). The service is sent the
/// coupon's words and the withdrawal, and answers the owner's signature of the
/// coupon's SafeMessage hash.
pub struct OwnerService {
    owner: Address,
    url: String,
    client: reqwest::Client,
}

/// A signer service's answer to `POST /sign`.
#[derive(Deserialize)]
struct Signed {
    owner: Address,
    signature: Bytes,
}

/// A signer service's answer to `GET /signer`.
#[derive(Debug, Deserialize)]
pub struct Identity {
    pub owner: Address,
    pub safe: Address,
    #[serde(rename = "chainId")]
    pub chain_id: u64,
}

/// A signer service's refusal, `409 { "code": "...", "reason": "..." }`.
#[derive(Deserialize)]
struct Refused {
    code: String,
    #[serde(default)]
    reason: String,
}

impl OwnerService {
    pub fn new(network: Network, owner: Address, url: &str) -> Result<Self> {
        let parsed = reqwest::Url::parse(url)
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
            .with_context(|| {
                format!(
                    "SAFE_OWNER_SERVICES names a URL for {owner} that is not an http or https URL"
                )
            })?;
        anyhow::ensure!(
            parsed.query().is_none() && parsed.fragment().is_none(),
            "SAFE_OWNER_SERVICES names a URL with a query or fragment for {owner}: name the service's base URL"
        );
        // No redirects: a 307 or 308 could move the request to plain http on
        // another host, past the https rule below.
        let client = reqwest::Client::builder()
            .timeout(SIGN_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("the HTTP client for SAFE_OWNER_SERVICES")?;
        let service = Self {
            owner,
            url: url.trim_end_matches('/').to_string(),
            client,
        };
        anyhow::ensure!(
            network != Network::Mainnet || !service.is_plain_http_off_host(),
            "SAFE_OWNER_SERVICES names a plain http URL at {} for {owner}: mainnet takes https, or http on this machine",
            service.host()
        );
        Ok(service)
    }

    /// Where the service is, for logs: its host and port, never its path, which
    /// may carry a credential.
    pub fn host(&self) -> String {
        reqwest::Url::parse(&self.url)
            .ok()
            .and_then(|url| {
                url.host_str().map(|host| match url.port() {
                    Some(port) => format!("{host}:{port}"),
                    None => host.to_string(),
                })
            })
            .unwrap_or_default()
    }

    /// Whether the URL is plain http to another machine: a signature crosses it,
    /// so mainnet takes https or this machine only.
    fn is_plain_http_off_host(&self) -> bool {
        reqwest::Url::parse(&self.url).is_ok_and(|url| {
            url.scheme() == "http"
                && !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
        })
    }

    fn unreachable(&self, err: &reqwest::Error) -> anyhow::Error {
        if err.is_timeout() {
            anyhow::Error::new(TimedOut)
        } else {
            anyhow!("could not be reached")
        }
    }

    /// `GET /signer`: the owner, Safe and chain the service signs for, within
    /// IDENTITY_TIMEOUT.
    pub async fn identity(&self) -> Result<Identity> {
        tokio::time::timeout(IDENTITY_TIMEOUT, self.ask_identity())
            .await
            .map_err(|_| anyhow!("did not answer GET /signer within {IDENTITY_TIMEOUT:?}"))?
    }

    async fn ask_identity(&self) -> Result<Identity> {
        let answer = self
            .client
            .get(format!("{}/signer", self.url))
            .send()
            .await
            .map_err(|err| self.unreachable(&err))?;
        let status = answer.status();
        anyhow::ensure!(
            status.is_success(),
            "answered GET /signer with HTTP {}",
            status.as_u16()
        );
        let body = self.read_answer(answer).await?;
        serde_json::from_slice(&body).map_err(|_| {
            anyhow!("answered GET /signer with something other than {{\"owner\", \"safe\", \"chainId\"}}")
        })
    }

    /// The answer's body, refused when it holds more than MAX_ANSWER_BYTES.
    async fn read_answer(&self, mut answer: reqwest::Response) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        while let Some(chunk) = answer.chunk().await.map_err(|err| self.unreachable(&err))? {
            anyhow::ensure!(
                body.len() + chunk.len() <= MAX_ANSWER_BYTES,
                "answered more than {MAX_ANSWER_BYTES} bytes"
            );
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    async fn request_signature(&self, request: &OwnerRequest<'_>) -> Result<Vec<u8>> {
        let body = json!({
            "safe": request.safe,
            "chainId": request.chain_id,
            "owner": self.owner,
            "withdrawal": request
                .sign
                .withdrawal
                .as_ref()
                .map(|w| ActionHashB64::from(w.clone()).to_string()),
            "context": request.sign.context.iter().map(|word| word.to_string()).collect::<Vec<_>>(),
            "couponDigest": request.sign.digest,
            "safeMessageHash": request.safe_message_hash,
        });
        let answer = self
            .client
            .post(format!("{}/sign", self.url))
            .json(&body)
            .send()
            .await
            .map_err(|err| self.unreachable(&err))?;
        let status = answer.status();
        if status == reqwest::StatusCode::CONFLICT {
            let body = self.read_answer(answer).await?;
            let refused = serde_json::from_slice::<Refused>(&body)
                .map(|r| {
                    let reason: String = r.reason.chars().take(200).collect();
                    let code: String = r.code.chars().take(40).collect();
                    if reason.is_empty() {
                        code
                    } else {
                        format!("{code}: {reason}")
                    }
                })
                .unwrap_or_else(|_| "no reason given".to_string());
            anyhow::bail!("refused: {refused}");
        }
        anyhow::ensure!(status.is_success(), "answered HTTP {}", status.as_u16());
        let body = self.read_answer(answer).await?;
        let signed: Signed = serde_json::from_slice(&body).map_err(|_| {
            anyhow!("answered with something other than {{\"owner\", \"signature\"}}")
        })?;
        anyhow::ensure!(signed.owner == self.owner, "answered for {}", signed.owner);
        Ok(signed.signature.to_vec())
    }
}

impl super::CouponKey for SafeOwners {
    fn address(&self) -> Address {
        self.safe
    }

    fn sign<'a>(&'a self, request: &'a SignRequest) -> Signing<'a> {
        Box::pin(self.sign_request(request))
    }

    fn safe(&self) -> Option<&SafeOwners> {
        Some(self)
    }

    fn new_cycle(&self) {
        SafeOwners::new_cycle(self);
    }
}

#[cfg(test)]
pub(crate) mod tests;
