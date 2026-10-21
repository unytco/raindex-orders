use super::*;
use crate::fake_rpc::{read_request, serve};
use crate::signer::{context_digest, CouponSigner, Payout};
use alloy::primitives::address;
use alloy::signers::SignerSync;
use alloy::sol_types::SolCall;
use holo_hash::{ActionHash, ActionHashB64};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

/// Anvil's accounts 1, 2 and 3: the owners of the Safe of threshold 2 at SAFE
/// on mainnet in test/TestSafeCouponSignature.t.sol. Then account 4, a fourth
/// owner for a Safe of 4.
pub(crate) const OWNER_KEYS: [&str; 4] = [
    "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d",
    "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a",
    "0x7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6",
    "0x47e179ec197488593b187f80a00eb0da91f1b9d0b13f8733639f19c30a34926a",
];
pub(crate) const SAFE: Address = address!("3838Fc4E0cba211c3D6D202539bB2dc2B72C7a22");
const NOW: u64 = 1_750_000_000;

pub(crate) fn owner_key(i: usize) -> PrivateKeySigner {
    OWNER_KEYS[i].parse().unwrap()
}

pub(crate) fn owner(i: usize) -> Address {
    Signer::address(&owner_key(i))
}

/// The vector test/TestSafeCouponSignature.t.sol pins against the Safe 1.4.1
/// contracts on a fork of mainnet, and docs/signer-service.md gives as its
/// worked example: the hash the Safe's fallback handler has the owners sign for
/// the coupon vector's digest.
#[test]
fn the_safe_message_hash_is_what_the_fallback_handler_computes() {
    let digest: B256 = "0xf32a9e86bb3d75c00acec45f208001679a5f2a7976c5ce3327b6de7cecb0781f"
        .parse()
        .unwrap();

    assert_eq!(
        safe_message_hash(1, SAFE, &digest),
        "0xd02d17b3c4226662353fa285586b495d7fcf937967ac510d2ebf535047b68d34"
            .parse::<B256>()
            .unwrap()
    );
    assert_ne!(
        safe_message_hash(84_532, SAFE, &digest),
        safe_message_hash(1, SAFE, &digest)
    );
}

/// A Safe of `threshold` with `owners` on chain `chain_id`, as Safe 1.4.1
/// answers getThreshold, getOwners and isValidSignature: the first `threshold`
/// 65-byte signatures each an owner's, ascending, over the SafeMessage hash
/// (v 27 or 28) or its EIP-191 hash (v 31 or 32).
#[derive(Clone)]
struct FakeSafe {
    chain_id: u64,
    threshold: u64,
    owners: Vec<Address>,
    /// Asks of isValidSignature, as the signatures it was given.
    asked: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl FakeSafe {
    fn new(threshold: u64, owners: &[usize]) -> Self {
        Self {
            chain_id: 1,
            threshold,
            owners: owners.iter().map(|i| owner(*i)).collect(),
            asked: Arc::default(),
        }
    }

    fn accepts(&self, digest: &B256, signature: &[u8]) -> bool {
        let hash = safe_message_hash(self.chain_id, SAFE, digest);
        let needed = self.threshold as usize;
        if signature.len() < 65 * needed {
            return false;
        }
        let mut last = Address::ZERO;
        for part in signature.chunks(65).take(needed) {
            let mut part: [u8; 65] = part.try_into().unwrap();
            let signed = if part[64] > 30 {
                part[64] -= 4;
                keccak256(
                    [
                        b"\x19Ethereum Signed Message:\n32".as_slice(),
                        hash.as_slice(),
                    ]
                    .concat(),
                )
            } else {
                hash
            };
            let Some(recovered) = PrimitiveSignature::try_from(part.as_slice())
                .ok()
                .and_then(|s| s.recover_address_from_prehash(&signed).ok())
            else {
                return false;
            };
            if recovered <= last || !self.owners.contains(&recovered) {
                return false;
            }
            last = recovered;
        }
        true
    }

    async fn serve(&self) -> String {
        let safe = self.clone();
        serve(move |method, params| {
            assert_eq!(method, "eth_call", "the signer sent {method}");
            let call = &params[0];
            let to: Address = call["to"].as_str().unwrap().parse().unwrap();
            assert_eq!(to, SAFE);
            let input = call["input"].as_str().or(call["data"].as_str()).unwrap();
            let input = hex::decode(input.trim_start_matches("0x")).unwrap();
            let selector: [u8; 4] = input[..4].try_into().unwrap();
            let answer = match selector {
                ISafe::getThresholdCall::SELECTOR => U256::from(safe.threshold).abi_encode(),
                ISafe::getOwnersCall::SELECTOR => safe.owners.abi_encode(),
                ISafe::isValidSignatureCall::SELECTOR => {
                    let asked = ISafe::isValidSignatureCall::abi_decode(&input, true).unwrap();
                    safe.asked.lock().unwrap().push(asked.signature.to_vec());
                    if !safe.accepts(&asked.dataHash, &asked.signature) {
                        return Err(
                            json!({"code": 3, "message": "execution reverted: GS026", "data": "0x"}),
                        );
                    }
                    alloy::primitives::FixedBytes::<4>::from(EIP1271_MAGIC_VALUE).abi_encode()
                }
                other => panic!("the signer called 0x{}", hex::encode(other)),
            };
            Ok(Value::String(format!("0x{}", hex::encode(answer))))
        })
        .await
    }
}

/// A signer service as docs/signer-service.md describes it, on this machine:
/// what it answers to `POST /sign` and `GET /signer`, and the requests it was
/// sent.
pub(crate) struct FakeService {
    pub(crate) url: String,
    pub(crate) requests: Arc<Mutex<Vec<(String, Value)>>>,
}

/// Serves `answer(path, body)` as `(status, body)` to every request.
pub(crate) async fn fake_service<A>(answer: A) -> FakeService
where
    A: Fn(&str, &Value) -> (&'static str, String) + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/holo/", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    let answer = Arc::new(answer);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let (seen, answer) = (Arc::clone(&seen), Arc::clone(&answer));
            tokio::spawn(async move {
                let mut head = [0u8; 256];
                let peeked = socket.peek(&mut head).await.unwrap();
                let line = String::from_utf8_lossy(&head[..peeked]);
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let body = read_request(&mut socket).await;
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let (status, reply) = answer(&path, &body);
                seen.lock().unwrap().push((path, body));
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            });
        }
    });
    FakeService { url, requests }
}

/// The SafeMessage hash a service computes itself from the words it is sent,
/// never the hash it is sent.
pub(crate) fn own_hash(body: &Value) -> B256 {
    let words: Vec<B256> = body["context"]
        .as_array()
        .unwrap()
        .iter()
        .map(|word| word.as_str().unwrap().parse().unwrap())
        .collect();
    safe_message_hash(1, SAFE, &context_digest(&words))
}

pub(crate) fn signature_json(owner: Address, signature: &[u8]) -> String {
    json!({"owner": owner, "signature": format!("0x{}", hex::encode(signature))}).to_string()
}

/// A service holding `key`, an owner of SAFE, which signs as its checks would
/// let it: the SafeMessage hash of the words, with `v` turned by `v_form`.
pub(crate) async fn owner_service_with(
    key: PrivateKeySigner,
    v_form: fn(&B256, &PrivateKeySigner) -> Vec<u8>,
) -> FakeService {
    fake_service(move |path, body| match path {
        "/holo/signer" => (
            "200 OK",
            json!({"owner": key.address(), "safe": SAFE, "chainId": 1}).to_string(),
        ),
        "/holo/sign" => (
            "200 OK",
            signature_json(key.address(), &v_form(&own_hash(body), &key)),
        ),
        _ => ("404 Not Found", "{}".to_string()),
    })
    .await
}

pub(crate) fn typed_data_signature(hash: &B256, key: &PrivateKeySigner) -> Vec<u8> {
    key.sign_hash_sync(hash).unwrap().as_bytes().to_vec()
}

pub(crate) async fn owner_service(key: PrivateKeySigner) -> FakeService {
    owner_service_with(key, typed_data_signature).await
}

pub(crate) fn service_owner(key: &PrivateKeySigner, service: &FakeService) -> Owner {
    Owner::Service(OwnerService::new(Network::Mainnet, key.address(), &service.url).unwrap())
}

fn safe_owners(owners: Vec<Owner>, rpc_url: &str) -> SafeOwners {
    SafeOwners::new(SAFE, 1, owners, rpc_url.to_string()).unwrap()
}

fn request(withdrawal: Option<ActionHash>) -> SignRequest {
    let context: Vec<B256> = (0u8..9).map(B256::with_last_byte).collect();
    SignRequest {
        digest: context_digest(&context),
        context,
        withdrawal,
    }
}

fn withdrawal() -> ActionHash {
    ActionHash::from_raw_32(vec![7; 32])
}

/// The owners each 65-byte part of `signature` recovers to over `hash`.
fn signers_of(signature: &[u8], hash: &B256) -> Vec<Address> {
    signature
        .chunks(65)
        .map(|part| {
            PrimitiveSignature::try_from(part)
                .unwrap()
                .recover_address_from_prehash(hash)
                .unwrap()
        })
        .collect()
}

fn sorted(mut addresses: Vec<Address>) -> Vec<Address> {
    addresses.sort();
    addresses
}

/// A Safe-signed coupon names the Safe and carries M owner signatures of its
/// SafeMessage hash, ascending, whatever order the keys were given in, and the
/// Safe was asked to accept them before the coupon was handed out.
#[tokio::test]
async fn owner_keys_sign_the_safe_message_hash_in_ascending_owner_order() {
    let chain = FakeSafe::new(2, &[0, 1, 2]);
    let url = chain.serve().await;
    let owners = safe_owners(
        vec![Owner::Key(owner_key(2)), Owner::Key(owner_key(0))],
        &url,
    );
    let signer = CouponSigner::with_key(owners);
    let terms = Payout::new("0x1111111111111111111111111111111111111111", "1.5").unwrap();

    let coupon = signer.coupon_at(terms, &withdrawal(), NOW).await.unwrap();
    let fields: Vec<&str> = coupon.split(',').collect();
    let context: Vec<B256> = fields[2..]
        .iter()
        .map(|f| B256::from(f.parse::<U256>().unwrap()))
        .collect();
    let signature = hex::decode(&fields[1][2..]).unwrap();
    let hash = safe_message_hash(1, SAFE, &context_digest(&context));

    assert_eq!(fields[0], format!("{SAFE:?}"));
    assert_eq!(
        signers_of(&signature, &hash),
        sorted(vec![owner(0), owner(2)])
    );
    assert_eq!(chain.asked.lock().unwrap().clone(), vec![signature]);
}

#[test]
fn the_owners_asked_are_two_to_twenty_and_none_twice() {
    let refused = |owners: Vec<Owner>| {
        format!(
            "{:#}",
            SafeOwners::new(SAFE, 1, owners, "http://127.0.0.1:1".to_string()).unwrap_err()
        )
    };
    let service = |i: usize| {
        Owner::Service(
            OwnerService::new(Network::Mainnet, owner(i), "https://signer.example").unwrap(),
        )
    };

    assert_eq!(
        refused(vec![Owner::Key(owner_key(0))]),
        "SAFE_OWNER_KEYS holds 1 keys: a Safe coupon signer needs the keys of its threshold of owners, 2 to 20, or SAFE_OWNER_SERVICES"
    );
    assert_eq!(
        refused((0..21).map(|_| Owner::Key(PrivateKeySigner::random())).collect()),
        "SAFE_OWNER_KEYS holds 21 keys: a Safe coupon signer needs the keys of its threshold of owners, 2 to 20, or SAFE_OWNER_SERVICES"
    );
    assert_eq!(
        refused(vec![service(0)]),
        "SAFE_OWNER_KEYS and SAFE_OWNER_SERVICES name 1 owners: a Safe coupon signer asks at least its threshold of owners, 2 to 20"
    );
    assert_eq!(
        refused(vec![
            Owner::Key(owner_key(0)),
            Owner::Key(owner_key(1)),
            Owner::Key(owner_key(0))
        ]),
        format!("SAFE_OWNER_KEYS holds the key of {} twice", owner(0))
    );
    assert_eq!(
        refused(vec![service(1), service(1)]),
        format!("SAFE_OWNER_SERVICES names {} twice", owner(1))
    );
    assert_eq!(
        refused(vec![Owner::Key(owner_key(1)), service(1)]),
        format!(
            "{} is in both SAFE_OWNER_KEYS and SAFE_OWNER_SERVICES: each owner signs in one place",
            owner(1)
        )
    );
    assert!(SafeOwners::new(
        SAFE,
        1,
        vec![Owner::Key(owner_key(0)), service(1)],
        String::new()
    )
    .is_ok());
}

/// The service is sent the Safe, the chain, its owner, the withdrawal, the
/// nine words, their digest and their SafeMessage hash, and the coupon carries
/// the owners' signatures that come back.
#[tokio::test]
async fn a_signer_service_is_sent_the_coupon_and_answers_its_owners_signature() {
    let chain = FakeSafe::new(2, &[0, 1, 2]);
    let url = chain.serve().await;
    let (a, b) = (
        owner_service(owner_key(0)).await,
        owner_service(owner_key(1)).await,
    );
    let owners = safe_owners(
        vec![
            service_owner(&owner_key(0), &a),
            service_owner(&owner_key(1), &b),
        ],
        &url,
    );
    let asked = request(Some(withdrawal()));

    let signature = owners.sign_request(&asked).await.unwrap();

    let hash = safe_message_hash(1, SAFE, &asked.digest);
    assert_eq!(
        signers_of(&signature, &hash),
        sorted(vec![owner(0), owner(1)])
    );
    let requests = a.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "/holo/sign");
    assert_eq!(
        requests[0].1,
        json!({
            "safe": SAFE,
            "chainId": 1,
            "owner": owner(0),
            "withdrawal": ActionHashB64::from(withdrawal()).to_string(),
            "context": asked.context.iter().map(|w| w.to_string()).collect::<Vec<_>>(),
            "couponDigest": asked.digest,
            "safeMessageHash": hash,
        })
    );
    let identity = match &owners.owners()[0] {
        Owner::Service(service) => service.identity().await.unwrap(),
        Owner::Key(_) => unreachable!(),
    };
    assert_eq!(
        (identity.owner, identity.safe, identity.chain_id),
        (owner(0), SAFE, 1)
    );
}

/// The startup probe pays no withdrawal: a service is sent it with
/// `"withdrawal": null`, and knows it by its words (docs/signer-service.md).
#[tokio::test]
async fn the_startup_probe_is_sent_with_no_withdrawal() {
    let chain = FakeSafe::new(2, &[0, 1, 2]);
    let url = chain.serve().await;
    let a = owner_service(owner_key(0)).await;
    let owners = safe_owners(
        vec![service_owner(&owner_key(0), &a), Owner::Key(owner_key(1))],
        &url,
    );

    owners.sign_request(&request(None)).await.unwrap();

    assert_eq!(a.requests.lock().unwrap()[0].1["withdrawal"], Value::Null);
}

/// All three owners sign; the coupon carries the two lowest, ascending.
#[tokio::test]
async fn a_coupon_carries_the_m_lowest_owners_that_signed() {
    let chain = FakeSafe::new(2, &[0, 1, 2]);
    let url = chain.serve().await;
    let mut owners = Vec::new();
    for i in [2, 0, 1] {
        owners.push(service_owner(
            &owner_key(i),
            &owner_service(owner_key(i)).await,
        ));
    }
    let owners = safe_owners(owners, &url);
    let asked = request(Some(withdrawal()));

    let signature = owners.sign_request(&asked).await.unwrap();

    let all = sorted(vec![owner(0), owner(1), owner(2)]);
    assert_eq!(
        signers_of(&signature, &safe_message_hash(1, SAFE, &asked.digest)),
        all[..2].to_vec()
    );
}

/// A service that cannot be reached is left out; the others still sign M.
#[tokio::test]
async fn a_coupon_is_signed_while_m_owners_sign_and_one_is_down() {
    let chain = FakeSafe::new(2, &[0, 1, 2]);
    let url = chain.serve().await;
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let down = format!("http://{}/", closed.local_addr().unwrap());
    drop(closed);
    let owners = safe_owners(
        vec![
            Owner::Service(OwnerService::new(Network::Mainnet, owner(0), &down).unwrap()),
            service_owner(&owner_key(1), &owner_service(owner_key(1)).await),
            service_owner(&owner_key(2), &owner_service(owner_key(2)).await),
        ],
        &url,
    );

    let signature = owners
        .sign_request(&request(Some(withdrawal())))
        .await
        .unwrap();

    assert_eq!(signature.len(), 130);
}

/// Fewer than M is an error that names each owner that did not sign and why,
/// and the Safe is not asked.
#[tokio::test]
async fn fewer_than_m_owners_signing_is_an_error_naming_why_each_other_did_not() {
    let chain = FakeSafe::new(2, &[0, 1, 2]);
    let url = chain.serve().await;
    let refusing = fake_service(|_, _| {
        (
            "409 Conflict",
            json!({"code": "amount_mismatch", "reason": "the amount 40 HOT is not the spend's 30 HOT"})
                .to_string(),
        )
    })
    .await;
    let wrong_key = owner_service_with(owner_key(0), |hash, _| {
        PrivateKeySigner::random()
            .sign_hash_sync(hash)
            .unwrap()
            .as_bytes()
            .to_vec()
    })
    .await;
    let owners = safe_owners(
        vec![
            service_owner(&owner_key(0), &wrong_key),
            service_owner(&owner_key(1), &refusing),
            Owner::Key(owner_key(2)),
        ],
        &url,
    );
    let hosts: Vec<String> = owners.owners().iter().map(|o| o.name()).collect();
    let asked = request(Some(withdrawal()));

    let err = format!("{:#}", owners.sign_request(&asked).await.unwrap_err());

    let hash = safe_message_hash(1, SAFE, &asked.digest);
    assert_eq!(
        err,
        format!(
            "1 of the 3 owners asked signed, and CLAIM_SIGNER {SAFE} needs 2: {}: answered a signature that is not {}'s over the coupon's SafeMessage hash {hash}; {}: refused: amount_mismatch: the amount 40 HOT is not the spend's 30 HOT",
            hosts[0],
            owner(0),
            hosts[1]
        )
    );
    assert!(chain.asked.lock().unwrap().is_empty());
}

#[tokio::test]
async fn each_wrong_answer_from_a_signer_service_is_named() {
    let chain = FakeSafe::new(3, &[0, 1, 2]);
    let url = chain.serve().await;
    let reason = |service: FakeService| {
        let url = url.clone();
        async move {
            let owners = safe_owners(
                vec![
                    service_owner(&owner_key(0), &service),
                    Owner::Key(owner_key(1)),
                    Owner::Key(owner_key(2)),
                ],
                &url,
            );
            let name = owners.owners()[0].name();
            let err = format!(
                "{:#}",
                owners
                    .sign_request(&request(Some(withdrawal())))
                    .await
                    .unwrap_err()
            );
            err.split_once(&format!("{name}: "))
                .map(|(_, why)| why.to_string())
                .unwrap_or(err)
        }
    };

    assert_eq!(
        reason(fake_service(|_, _| ("500 Internal Server Error", "{}".to_string())).await).await,
        "answered HTTP 500"
    );
    assert_eq!(
        reason(fake_service(|_, _| ("200 OK", "signed".to_string())).await).await,
        "answered with something other than {\"owner\", \"signature\"}"
    );
    assert_eq!(
        reason(fake_service(|_, _| ("409 Conflict", "no".to_string())).await).await,
        "refused: no reason given"
    );
    assert_eq!(
        reason(
            fake_service(|_, body| (
                "200 OK",
                signature_json(
                    owner(1),
                    &typed_data_signature(&own_hash(body), &owner_key(1))
                )
            ))
            .await
        )
        .await,
        format!("answered for {}", owner(1))
    );
    assert_eq!(
        reason(fake_service(|_, _| ("200 OK", signature_json(owner(0), &[1; 64]))).await).await,
        "answered 64 bytes, not a 65-byte signature"
    );
    assert_eq!(
        reason(
            owner_service_with(owner_key(0), |hash, key| {
                let mut signature = typed_data_signature(hash, key);
                signature[64] = 29;
                signature
            })
            .await
        )
        .await,
        "answered a signature whose v is 29, not 27 or 28"
    );
}

/// Signatures with v 0 or 1 are taken as 27 or 28, and Safe's eth_sign form,
/// v 31 or 32 over the EIP-191 hash of the SafeMessage hash, as it is.
#[tokio::test]
async fn an_owner_signature_with_v_0_or_1_or_in_the_eth_sign_form_is_taken() {
    let chain = FakeSafe::new(2, &[0, 1, 2]);
    let url = chain.serve().await;
    let raw_v = owner_service_with(owner_key(0), |hash, key| {
        let mut signature = typed_data_signature(hash, key);
        signature[64] -= 27;
        signature
    })
    .await;
    let eth_sign = owner_service_with(owner_key(1), |hash, key| {
        let mut signature = key
            .sign_hash_sync(&keccak256(
                [
                    b"\x19Ethereum Signed Message:\n32".as_slice(),
                    hash.as_slice(),
                ]
                .concat(),
            ))
            .unwrap()
            .as_bytes()
            .to_vec();
        signature[64] += 4;
        signature
    })
    .await;
    let owners = safe_owners(
        vec![
            service_owner(&owner_key(0), &raw_v),
            service_owner(&owner_key(1), &eth_sign),
        ],
        &url,
    );

    let signature = owners
        .sign_request(&request(Some(withdrawal())))
        .await
        .unwrap();

    let vs: Vec<u8> = signature.chunks(65).map(|part| part[64]).collect();
    let mut expected: Vec<(Address, bool)> = vec![(owner(0), false), (owner(1), true)];
    expected.sort();
    for (v, (_, eth_signed)) in vs.iter().zip(expected) {
        assert!(
            if eth_signed {
                [31, 32].contains(v)
            } else {
                [27, 28].contains(v)
            },
            "{vs:?}"
        );
    }
}

/// An owner the Safe no longer lists does not count, though it signed.
#[tokio::test]
async fn an_owner_the_safe_no_longer_lists_is_not_counted() {
    let chain = FakeSafe::new(2, &[0, 1]);
    let url = chain.serve().await;
    let owners = safe_owners(
        vec![Owner::Key(owner_key(1)), Owner::Key(owner_key(2))],
        &url,
    );

    let err = format!(
        "{:#}",
        owners
            .sign_request(&request(Some(withdrawal())))
            .await
            .unwrap_err()
    );

    assert!(
        err.ends_with(&format!(
            "{} is not an owner of CLAIM_SIGNER {SAFE} now",
            owner(2)
        )),
        "{err}"
    );
    assert!(err.starts_with("1 of the 2 owners asked signed"), "{err}");
}

/// A Safe that refuses the owners' signatures, here because it is on another
/// chain than the one they signed for, fails the coupon.
#[tokio::test]
async fn a_coupon_the_safe_refuses_is_an_error() {
    let mut chain = FakeSafe::new(2, &[0, 1, 2]);
    chain.chain_id = 84_532;
    let url = chain.serve().await;
    let owners = safe_owners(
        vec![Owner::Key(owner_key(0)), Owner::Key(owner_key(1))],
        &url,
    );

    let err = format!(
        "{:#}",
        owners
            .sign_request(&request(Some(withdrawal())))
            .await
            .unwrap_err()
    );

    assert_eq!(
        err,
        format!("CLAIM_SIGNER {SAFE} refused the signatures of 2 of its owners: its isValidSignature did not accept them")
    );
}

/// The tests of a silent service run on the real clock, with `SHORT_SIGN_TIMEOUT`
/// to sign: under a paused clock the RPC's read timer would run out while the
/// Safe's answer is on its way over the socket.
const SHORT_SIGN_TIMEOUT: Duration = Duration::from_millis(500);

#[tokio::test]
async fn a_signer_service_that_does_not_answer_is_left_out_after_the_timeout() {
    let chain = FakeSafe::new(2, &[0, 1, 2]);
    let url = chain.serve().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    let owners = safe_owners(
        vec![
            Owner::Service(OwnerService::new(Network::Mainnet, owner(0), &silent).unwrap()),
            Owner::Key(owner_key(1)),
        ],
        &url,
    )
    .with_sign_timeout(SHORT_SIGN_TIMEOUT);
    let name = owners.owners()[0].name();

    let err = format!(
        "{:#}",
        owners
            .sign_request(&request(Some(withdrawal())))
            .await
            .unwrap_err()
    );

    assert!(
        err.ends_with(&format!("{name}: did not answer within 500ms"))
            || err.ends_with(&format!("{name} did not sign within 500ms")),
        "{err}"
    );
}

/// A signer service that takes the connection and never answers.
async fn silent_service() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    url
}

/// Why `owners`' owner `i` did not sign `request`, when asked with the others.
async fn why_not(owners: &SafeOwners, i: usize) -> String {
    let (_, answers) = owners.collect(&request(Some(withdrawal()))).await.unwrap();
    format!("{:#}", answers[i].1.as_ref().unwrap_err())
}

/// Whether `why` says the owner `name` ran out of SHORT_SIGN_TIMEOUT.
fn timed_out(why: &str, name: &str) -> bool {
    why == format!("{name}: did not answer within 500ms")
        || why == format!("{name} did not sign within 500ms")
}

/// A service that does not answer costs one timeout a cycle, not one for each
/// withdrawal: while the other owners are at least the threshold it is not
/// asked again this cycle, and the others sign. A new cycle asks it again.
#[tokio::test]
async fn a_signer_service_that_timed_out_is_not_asked_again_this_cycle_while_the_others_reach_m() {
    let chain = FakeSafe::new(2, &[0, 1, 2]);
    let url = chain.serve().await;
    let silent = silent_service().await;
    let owners = safe_owners(
        vec![
            Owner::Service(OwnerService::new(Network::Mainnet, owner(0), &silent).unwrap()),
            Owner::Key(owner_key(1)),
            Owner::Key(owner_key(2)),
        ],
        &url,
    )
    .with_sign_timeout(SHORT_SIGN_TIMEOUT);
    let name = owners.owners()[0].name();

    let first = why_not(&owners, 0).await;
    assert!(timed_out(&first, &name), "{first}");

    for _ in 0..3 {
        let signature = owners
            .sign_request(&request(Some(withdrawal())))
            .await
            .unwrap();
        assert_eq!(signature.len(), 130);
    }
    assert_eq!(
        why_not(&owners, 0).await,
        format!("{name} was not asked: it did not answer within 500ms earlier this cycle")
    );

    owners.new_cycle();
    let again = why_not(&owners, 0).await;
    assert!(timed_out(&again, &name), "{again}");
}

/// An owner that timed out is still asked while the other owners are fewer
/// than the threshold: no coupon could be signed without it.
#[tokio::test]
async fn a_signer_service_that_timed_out_is_asked_again_while_the_others_are_fewer_than_m() {
    let chain = FakeSafe::new(2, &[0, 1, 2]);
    let url = chain.serve().await;
    let silent = silent_service().await;
    let owners = safe_owners(
        vec![
            Owner::Service(OwnerService::new(Network::Mainnet, owner(0), &silent).unwrap()),
            Owner::Key(owner_key(1)),
        ],
        &url,
    )
    .with_sign_timeout(SHORT_SIGN_TIMEOUT);
    let name = owners.owners()[0].name();

    for _ in 0..2 {
        let why = why_not(&owners, 0).await;
        assert!(timed_out(&why, &name), "{why}");
    }
}

/// A signer service that, while `silent` is set, takes the connection and
/// never answers, and otherwise refuses with `not_ready`.
async fn sometimes_silent_service(silent: Arc<std::sync::atomic::AtomicBool>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((mut socket, _)) = listener.accept().await {
            if silent.load(std::sync::atomic::Ordering::SeqCst) {
                held.push(socket);
                continue;
            }
            tokio::spawn(async move {
                read_request(&mut socket).await;
                let reply = json!({"code": "not_ready"}).to_string();
                let response = format!(
                    "HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            });
        }
    });
    url
}

/// An owner that timed out and then answered, asked again because the others
/// were fewer than the threshold, counts as answering again: another owner
/// that times out is then the one left out. On the real clock, with 500 ms to
/// sign: a paused clock would run out while a service's answer is on its way.
#[tokio::test]
async fn an_owner_that_answers_after_timing_out_is_asked_again_this_cycle() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let chain = FakeSafe::new(2, &[0, 1, 2]);
    let url = chain.serve().await;
    let (a_silent, c_silent) = (
        Arc::new(AtomicBool::new(true)),
        Arc::new(AtomicBool::new(false)),
    );
    let a = sometimes_silent_service(Arc::clone(&a_silent)).await;
    let c = sometimes_silent_service(Arc::clone(&c_silent)).await;
    let owners = safe_owners(
        vec![
            Owner::Service(OwnerService::new(Network::Mainnet, owner(0), &a).unwrap()),
            Owner::Key(owner_key(1)),
            Owner::Service(OwnerService::new(Network::Mainnet, owner(2), &c).unwrap()),
        ],
        &url,
    )
    .with_sign_timeout(std::time::Duration::from_millis(500));
    let (a_name, c_name) = (owners.owners()[0].name(), owners.owners()[2].name());
    let not_asked = |name: &str| {
        format!("{name} was not asked: it did not answer within 500ms earlier this cycle")
    };
    let timed_out = |why: &str, name: &str| why == format!("{name} did not sign within 500ms");

    let why = why_not(&owners, 0).await;
    assert!(timed_out(&why, &a_name), "{why}");

    a_silent.store(false, Ordering::SeqCst);
    c_silent.store(true, Ordering::SeqCst);
    let (_, answers) = owners.collect(&request(Some(withdrawal()))).await.unwrap();
    assert_eq!(
        format!("{:#}", answers[0].1.as_ref().unwrap_err()),
        not_asked(&a_name)
    );
    let why = format!("{:#}", answers[2].1.as_ref().unwrap_err());
    assert!(timed_out(&why, &c_name), "{why}");

    let (_, answers) = owners.collect(&request(Some(withdrawal()))).await.unwrap();
    assert_eq!(
        format!("{:#}", answers[0].1.as_ref().unwrap_err()),
        format!("{a_name}: refused: not_ready")
    );

    assert_eq!(why_not(&owners, 2).await, not_asked(&c_name));
}

#[tokio::test]
async fn an_unreachable_signer_service_is_named_by_its_host_alone() {
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/token-in-the-path", closed.local_addr().unwrap());
    drop(closed);
    let service = OwnerService::new(Network::Mainnet, owner(0), &url).unwrap();

    let message = format!("{:#}", service.identity().await.unwrap_err());

    assert_eq!(message, "could not be reached");
    assert!(!service.host().contains("token-in-the-path"));
}

#[test]
fn mainnet_takes_https_or_this_machine_for_a_signer_service() {
    let refused = |url: &str| {
        OwnerService::new(Network::Mainnet, owner(0), url)
            .err()
            .map(|err| format!("{err:#}"))
    };

    assert_eq!(
        refused("http://signer.example"),
        Some(format!(
            "SAFE_OWNER_SERVICES names a plain http URL at signer.example for {}: mainnet takes https, or http on this machine",
            owner(0)
        ))
    );
    assert_eq!(refused("https://signer.example/holo"), None);
    assert_eq!(refused("http://127.0.0.1:8080"), None);
    assert!(OwnerService::new(Network::BaseSepolia, owner(0), "http://signer.example").is_ok());
    assert_eq!(
        refused("ftp://signer.example"),
        Some(format!(
            "SAFE_OWNER_SERVICES names a URL for {} that is not an http or https URL",
            owner(0)
        ))
    );
    assert_eq!(
        refused("https://signer.example/?token=1"),
        Some(format!(
            "SAFE_OWNER_SERVICES names a URL with a query or fragment for {}: name the service's base URL",
            owner(0)
        ))
    );
}

/// The worked example in docs/signer-service.md, word for word: the coupon test
/// vector's request to the service of owner 0x7099…79C8 of SAFE on mainnet, and
/// that owner's signature, which ui/scripts/safe-message.test.ts reproduces with
/// viem.
#[tokio::test]
async fn the_worked_example_in_the_signer_service_doc() {
    let words = [
        "0x0000000000000000000000001111111111111111111111111111111111111111",
        "0xffffffee000000000000000000000000000000000000000014d1120d7b160000",
        "0x0000000000000000000000000000000000000000000000000000000068581c00",
        "0x5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9",
        "0x000000000000000000000000e3e064e3c2eef66cb93da8d8114f5084e92f48d6",
        "0x00000000000000000000000037fc0efec37d19f8a221aa4f8f7600c9ba2acd20",
        "0x0000000000000000000000006c6ee5e31d828de241282b9606c8e98ea48526e2",
        "0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b",
        "0x12c911abe545a1669fcbcdaeeede5352ee5a6743f59dbacdc0251b1e4c2f7b77",
    ];
    let context: Vec<B256> = words.iter().map(|w| w.parse().unwrap()).collect();
    assert_eq!(
        context[8],
        keccak256(withdrawal().get_raw_39()),
        "the nonce is keccak256 of the withdrawal's 39 bytes"
    );
    assert_eq!(
        context[1],
        amount_of("1.5").unwrap(),
        "the amount word is the spend's \"1.5\" HOT in wei, exponent -18"
    );
    let chain = FakeSafe::new(2, &[0, 1, 2]);
    let url = chain.serve().await;
    let service = owner_service(owner_key(0)).await;
    let owners = safe_owners(
        vec![
            service_owner(&owner_key(0), &service),
            Owner::Key(owner_key(1)),
        ],
        &url,
    );
    let asked = SignRequest {
        digest: context_digest(&context),
        context,
        withdrawal: Some(withdrawal()),
    };

    owners.sign_request(&asked).await.unwrap();

    assert_eq!(
        service.requests.lock().unwrap()[0].1,
        serde_json::from_str::<Value>(
            r#"{
              "safe": "0x3838fc4e0cba211c3d6d202539bb2dc2b72c7a22",
              "chainId": 1,
              "owner": "0x70997970c51812dc3a010c7d01b50e0d17dc79c8",
              "withdrawal": "uhCkkBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBweIGAsC",
              "context": [
                "0x0000000000000000000000001111111111111111111111111111111111111111",
                "0xffffffee000000000000000000000000000000000000000014d1120d7b160000",
                "0x0000000000000000000000000000000000000000000000000000000068581c00",
                "0x5eeff397dac16f82057e20da98cf183daf95a0695980a196270e9e0922a275f9",
                "0x000000000000000000000000e3e064e3c2eef66cb93da8d8114f5084e92f48d6",
                "0x00000000000000000000000037fc0efec37d19f8a221aa4f8f7600c9ba2acd20",
                "0x0000000000000000000000006c6ee5e31d828de241282b9606c8e98ea48526e2",
                "0xeede83a4244afae4fef82c8f5b97df1f18bfe3193e65ba02052e37f6171b334b",
                "0x12c911abe545a1669fcbcdaeeede5352ee5a6743f59dbacdc0251b1e4c2f7b77"
              ],
              "couponDigest": "0xf32a9e86bb3d75c00acec45f208001679a5f2a7976c5ce3327b6de7cecb0781f",
              "safeMessageHash": "0xd02d17b3c4226662353fa285586b495d7fcf937967ac510d2ebf535047b68d34"
            }"#
        )
        .unwrap()
    );
    let hash: B256 = "0xd02d17b3c4226662353fa285586b495d7fcf937967ac510d2ebf535047b68d34"
        .parse()
        .unwrap();
    assert_eq!(
        format!("0x{}", hex::encode(typed_data_signature(&hash, &owner_key(0)))),
        "0xba849cad3e89c4cfe1e5bb91fe102512c6c419a63eb7ef051875c6a378fa983e1512f61cf01f253ea5a62c247dd7b1ae2ea2a70d69e7262320f7d5fdf9e97d4a1c"
    );
}

/// A service that redirects is not followed: a 307 or 308 could move the
/// request to plain http on another host, past the https rule.
#[tokio::test]
async fn a_signer_service_redirect_is_not_followed() {
    let target = owner_service(owner_key(0)).await;
    let location: &'static str = Box::leak(
        format!("307 Temporary Redirect\r\nLocation: {}signer", target.url).into_boxed_str(),
    );
    let redirecting = fake_service(move |_, _| (location, "{}".to_string())).await;
    let service = OwnerService::new(Network::Mainnet, owner(0), &redirecting.url).unwrap();

    let message = format!("{:#}", service.identity().await.unwrap_err());

    assert_eq!(message, "answered GET /signer with HTTP 307");
    assert!(target.requests.lock().unwrap().is_empty());
}

/// An answer over MAX_ANSWER_BYTES is refused, not read into memory whole.
#[tokio::test]
async fn a_signer_service_answer_over_the_size_cap_is_refused() {
    let big =
        fake_service(|_, _| ("200 OK", format!("\"{}\"", "a".repeat(MAX_ANSWER_BYTES)))).await;
    let service = OwnerService::new(Network::Mainnet, owner(0), &big.url).unwrap();

    let message = format!("{:#}", service.identity().await.unwrap_err());

    assert_eq!(
        message,
        format!("answered more than {MAX_ANSWER_BYTES} bytes")
    );
}

/// Word 1 as the orchestrator makes it from a spend's HOT amount, a decimal
/// string: `None` where it refuses the amount.
fn amount_of(amount: &str) -> Option<B256> {
    let payout = Payout::new("0x1111111111111111111111111111111111111111", amount).ok()?;
    Some(crate::signer::amount_word(payout.wei).unwrap())
}

/// The amount conversion table in docs/signer-service.md ("The amount"): each
/// decimal string's word, and the strings that are refused.
#[test]
fn the_amount_vectors_in_the_signer_service_doc() {
    for (amount, word) in [
        (
            "1.5",
            "0xffffffee000000000000000000000000000000000000000014d1120d7b160000",
        ),
        (
            "5",
            "0xffffffee00000000000000000000000000000000000000004563918244f40000",
        ),
        (
            "0.000001",
            "0xffffffee0000000000000000000000000000000000000000000000e8d4a51000",
        ),
        (
            "0.000000000000000001",
            "0xffffffee00000000000000000000000000000000000000000000000000000001",
        ),
    ] {
        assert_eq!(amount_of(amount), Some(word.parse().unwrap()), "{amount:?}");
    }
    for refused in [
        "0",
        "0.0",
        "-1",
        "1.0000000000000000001",
        "1e18",
        ".5",
        "5.",
        "",
    ] {
        assert_eq!(amount_of(refused), None, "{refused:?}");
    }
    // A spend's amount is a ZFuel, which serialises as such a string, with at
    // most 6 fractional digits and no trailing zeros.
    for amount in ["1.5", "5", "0.000001"] {
        assert_eq!(
            amount.parse::<zfuel::fuel::ZFuel>().unwrap().to_string(),
            amount
        );
    }
}

/// A service that keeps the record "One order per withdrawal" in
/// docs/signer-service.md requires: the order hash, word 3, it signed each
/// withdrawal for. It refuses a withdrawal recorded under another order hash
/// with already_signed, and otherwise records it and signs.
async fn recording_service(key: PrivateKeySigner) -> FakeService {
    let record = Mutex::new(std::collections::HashMap::<String, Value>::new());
    fake_service(move |path, body| match path {
        "/holo/signer" => (
            "200 OK",
            json!({"owner": key.address(), "safe": SAFE, "chainId": 1}).to_string(),
        ),
        "/holo/sign" => {
            let withdrawal = body["withdrawal"].as_str().unwrap().to_string();
            let order = body["context"][3].clone();
            let mut record = record.lock().unwrap();
            if record
                .get(&withdrawal)
                .is_some_and(|signed| *signed != order)
            {
                return (
                    "409 Conflict",
                    json!({"code": "already_signed"}).to_string(),
                );
            }
            record.insert(withdrawal, order);
            (
                "200 OK",
                signature_json(key.address(), &typed_data_signature(&own_hash(body), &key)),
            )
        }
        _ => ("404 Not Found", "{}".to_string()),
    })
    .await
}

/// The request for `withdrawal()` under the claim order whose hash is 32
/// bytes of `order`.
fn request_for_order(order: u8) -> SignRequest {
    let mut context: Vec<B256> = (0u8..9).map(B256::with_last_byte).collect();
    context[3] = B256::repeat_byte(order);
    SignRequest {
        digest: context_digest(&context),
        context,
        withdrawal: Some(withdrawal()),
    }
}

/// Owner `i`'s service asked for `request` alone, as any orchestrator can.
async fn asked_alone(i: usize, service: &FakeService, request: &SignRequest) -> Result<Vec<u8>> {
    let Owner::Service(service) = service_owner(&owner_key(i), service) else {
        unreachable!()
    };
    service
        .request_signature(&OwnerRequest {
            safe: SAFE,
            chain_id: 1,
            sign: request,
            safe_message_hash: safe_message_hash(1, SAFE, &request.digest),
        })
        .await
}

/// Lal's replay on PR #2. In a Safe of 2 of 4, owners A and B sign withdrawal W
/// for claim order O1. After a rotation to O2, A refuses W with already_signed,
/// but C and D never signed W, have no record of it, and would sign it for O2:
/// a second coupon for W, which O1's used nonce does not stop on O2. So a Safe
/// whose threshold is half its owners or fewer signs no coupon, and no owner is
/// asked. With 3 of 4, every three owners hold one that signed W for O1, and it
/// refuses: the records stop the second coupon once quorums intersect.
#[tokio::test]
async fn a_safe_whose_quorums_can_be_disjoint_signs_no_coupon() {
    let (o1, o2) = (request_for_order(1), request_for_order(2));
    let mut services = Vec::new();
    for i in 0..4 {
        services.push(recording_service(owner_key(i)).await);
    }
    for i in [0, 1] {
        asked_alone(i, &services[i], &o1).await.unwrap();
    }
    let control = asked_alone(0, &services[0], &o2).await.unwrap_err();
    assert_eq!(format!("{control:#}"), "refused: already_signed");

    let chain = FakeSafe::new(2, &[0, 1, 2, 3]);
    let url = chain.serve().await;
    let owners = safe_owners(
        vec![
            service_owner(&owner_key(2), &services[2]),
            service_owner(&owner_key(3), &services[3]),
        ],
        &url,
    );
    let err = owners
        .sign_request(&o2)
        .await
        .expect_err("C and D signed W for O2");
    assert_eq!(
        format!("{err:#}"),
        format!("CLAIM_SIGNER {SAFE} is a Safe of threshold 2 with 4 owners, so two sets of 2 of its owners can share none: after a claim order rotation, owners that never signed a withdrawal could sign it again for the new order. Its threshold must be more than half its owners (docs/signer-service.md#one-order-per-withdrawal). No owner was asked")
    );
    for service in &services[2..] {
        assert!(service.requests.lock().unwrap().is_empty());
    }
    let one_party = safe_owners(
        vec![
            service_owner(&owner_key(2), &services[2]),
            service_owner(&owner_key(3), &services[3]),
        ],
        &url,
    )
    .allowing_one_party(true);
    let overridden = one_party
        .sign_request(&o2)
        .await
        .expect_err("C and D signed W for O2 with UNSAFE_ALLOW_ONE_PARTY_SIGNER=yes");
    assert_eq!(format!("{overridden:#}"), format!("{err:#}"));
    for service in &services[2..] {
        assert!(service.requests.lock().unwrap().is_empty());
    }

    let mut services = Vec::new();
    for i in 0..4 {
        services.push(recording_service(owner_key(i)).await);
    }
    for i in [0, 1, 2] {
        asked_alone(i, &services[i], &o1).await.unwrap();
    }
    let chain = FakeSafe::new(3, &[0, 1, 2, 3]);
    let url = chain.serve().await;
    for left_out in 0..4 {
        let owners = safe_owners(
            (0..4)
                .filter(|i| *i != left_out)
                .map(|i| service_owner(&owner_key(i), &services[i]))
                .collect(),
            &url,
        );
        let err = format!(
            "{:#}",
            owners
                .sign_request(&o2)
                .await
                .expect_err("three owners signed W for O2")
        );
        assert!(err.contains("needs 3"), "{err}");
        assert!(err.contains("refused: already_signed"), "{err}");
    }
    assert!(chain.asked.lock().unwrap().is_empty());
}

/// An RPC for a Safe of 2 of owners 0 and 1 that answers getThreshold and
/// getOwners, and takes isValidSignature without ever answering it.
async fn rpc_holding_is_valid_signature() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let body = read_request(&mut socket).await;
                let call: Value = serde_json::from_slice(&body).unwrap();
                let input = call["params"][0]["input"]
                    .as_str()
                    .or(call["params"][0]["data"].as_str())
                    .unwrap();
                let input = hex::decode(input.trim_start_matches("0x")).unwrap();
                let selector: [u8; 4] = input[..4].try_into().unwrap();
                let result = match selector {
                    ISafe::getThresholdCall::SELECTOR => U256::from(2).abi_encode(),
                    ISafe::getOwnersCall::SELECTOR => vec![owner(0), owner(1)].abi_encode(),
                    _ => std::future::pending().await,
                };
                let reply = json!({
                    "jsonrpc": "2.0",
                    "id": call["id"],
                    "result": format!("0x{}", hex::encode(result)),
                })
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            });
        }
    });
    url
}

/// Each coupon reads the Safe's threshold and owners through the RPC, and has
/// the Safe accept the signatures through it. Nothing else bounds those reads:
/// an RPC that took one and never answered would hold the bridge cycle up for
/// good, the deposits behind the withdrawal included. Each read has
/// `rpc_timeout` instead, and the withdrawal stays parked for the cycle.
#[tokio::test]
async fn an_rpc_that_never_answers_a_read_of_the_safe_parks_the_coupon_within_the_rpc_timeout() {
    for (rpc, read) in [
        (silent_service().await, "getThreshold() and getOwners()"),
        (
            rpc_holding_is_valid_signature().await,
            "isValidSignature(...)",
        ),
    ] {
        let owners = safe_owners(
            vec![Owner::Key(owner_key(0)), Owner::Key(owner_key(1))],
            &rpc,
        )
        .with_rpc_timeout(Duration::from_millis(300));
        let started = std::time::Instant::now();

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            owners.sign_request(&request(Some(withdrawal()))),
        )
        .await;

        let err = result
            .unwrap_or_else(|_| panic!("the coupon hung for 5s on an RPC holding {read}"))
            .unwrap_err();
        assert_eq!(
            format!("{err:#}"),
            format!("CLAIM_SIGNER {SAFE}: {read} did not answer within 300ms")
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{read} took {:?}",
            started.elapsed()
        );
    }
}

/// One RPC timeout is enough for a cycle: once a read of the Safe has not
/// answered within `rpc_timeout`, the other coupons of the cycle are not read,
/// and fail at once naming that read. Otherwise each parked withdrawal would
/// cost the cycle an `rpc_timeout`, the deposits behind them waiting. A new
/// cycle reads again.
#[tokio::test]
async fn after_an_rpc_timeout_the_other_coupons_of_the_cycle_are_not_read_until_a_new_cycle() {
    let rpc = silent_service().await;
    let owners = safe_owners(
        vec![Owner::Key(owner_key(0)), Owner::Key(owner_key(1))],
        &rpc,
    )
    .with_rpc_timeout(Duration::from_millis(300));
    let read = "getThreshold() and getOwners()";
    let timed_out = format!("CLAIM_SIGNER {SAFE}: {read} did not answer within 300ms");
    let request = request(Some(withdrawal()));

    let first = owners.sign_request(&request).await.unwrap_err();
    assert_eq!(format!("{first:#}"), timed_out);

    let started = std::time::Instant::now();
    let second = owners.sign_request(&request).await.unwrap_err();
    assert_eq!(
        format!("{second:#}"),
        format!("CLAIM_SIGNER {SAFE}: {read} was not read: {read} did not answer within 300ms earlier this cycle")
    );
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "the second coupon waited {:?} on the RPC that had timed out",
        started.elapsed()
    );

    owners.new_cycle();
    let started = std::time::Instant::now();
    let third = owners.sign_request(&request).await.unwrap_err();
    assert_eq!(format!("{third:#}"), timed_out);
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "a new cycle did not read the Safe again: {:?}",
        started.elapsed()
    );
}

#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Lines {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// What `run` logs at `level` or above. As orchestrator.rs's capture: every
/// level is enabled and filtered at the writer, and a second dispatcher stays
/// registered, so a callsite another test hit first is never left disabled.
async fn logged_at(level: tracing::Level, run: impl std::future::Future) -> String {
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::fmt::writer::MakeWriterExt;
    static SECOND: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
    SECOND.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    let lines = Lines::default();
    let writer = lines.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer((move || writer.clone()).with_max_level(level))
        .finish();
    run.with_subscriber(subscriber).await;
    let text = lines.0.lock().unwrap().clone();
    String::from_utf8(text).unwrap()
}

/// An owner that timed out is warned about once, on the coupon it timed out
/// on. The coupons the others sign for the rest of the cycle, which do not ask
/// it, do not warn about it again: a service down for hours would otherwise
/// be warned about on every coupon of every cycle.
#[tokio::test]
async fn an_owner_not_asked_this_cycle_is_not_warned_about_on_each_coupon() {
    let chain = FakeSafe::new(2, &[0, 1, 2]);
    let url = chain.serve().await;
    let silent = silent_service().await;
    let owners = safe_owners(
        vec![
            Owner::Service(OwnerService::new(Network::Mainnet, owner(0), &silent).unwrap()),
            Owner::Key(owner_key(1)),
            Owner::Key(owner_key(2)),
        ],
        &url,
    )
    .with_sign_timeout(SHORT_SIGN_TIMEOUT);
    let name = owners.owners()[0].name();
    let request = request(Some(withdrawal()));

    let first = logged_at(tracing::Level::WARN, async {
        owners.sign_request(&request).await.unwrap();
    })
    .await;
    assert!(
        first.contains("coupon.owner_did_not_sign") && first.contains(&name),
        "the coupon the owner timed out on did not warn about it: {first:?}"
    );

    let second = logged_at(tracing::Level::WARN, async {
        owners.sign_request(&request).await.unwrap();
    })
    .await;
    assert!(
        !second.contains("coupon.owner_did_not_sign"),
        "a coupon that did not ask the owner warned about it: {second:?}"
    );
    assert_eq!(
        why_not(&owners, 0).await,
        format!("{name} was not asked: it did not answer within 500ms earlier this cycle")
    );
}
