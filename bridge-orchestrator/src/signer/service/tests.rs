use super::cell::{Cell, Reading};
use super::checks::Code;
use super::config::ServiceConfig;
use super::records::Records;
use super::{router, Service};
use crate::config::{changed_settings, with_network_defaults, Network};
use crate::orchestrator::tests::screening::Down;
use crate::orchestrator::tests::{
    action_hash, bridging_agent, logged_at, parked_tx, parked_withdrawal_tx, test_db_path,
    with_payload, BR_EA,
};
use crate::signer::safe::tests::{
    owner, owner_key, FakeSafe, OWNER_KEYS, SAFE, WORKED_EXAMPLE, WORKED_EXAMPLE_SIGNATURE,
};
use crate::signer::safe::{safe_message_hash, Owner, OwnerService, SafeOwners, SignBody, Signed};
use crate::signer::{
    amount_word, context_digest, coupon_context, pad_address, probe_context, probe_nonce,
    withdrawal_nonce, ClaimOrder, CouponSigner, Payout,
};
use alloy::primitives::{Address, B256, U256};
use anyhow::anyhow;
use holo_hash::{ActionHash, ActionHashB64, AgentPubKey, AgentPubKeyB64};
use rave_engine::types::{ParkedLinkStanding, Transaction, TransactionDetails, UnitMap};
use serde_json::{json, Value};
use std::sync::Arc;

const TOKEN: &str = "signer-test-token";
const NOW: u64 = 1_750_000_000;
pub(crate) const WEEK: u64 = 604_800;
const RECIPIENT: &str = "0x1111111111111111111111111111111111111111";
const VAULT: &str = "0xe3e064e3c2eef66cb93da8d8114f5084e92f48d6";

/// The signer's cell, which answers `standing` for `withdrawal()`, holds
/// each of `parked` as parked, and finds the bridging agreement of
/// `bridging_agent()` at HOT unit 1. `None` is a call that fails.
struct FakeCell {
    up: bool,
    standing: Option<ParkedLinkStanding>,
    parked: Vec<Transaction>,
    agreement: Option<ActionHashB64>,
}

impl FakeCell {
    fn holding(spend: Transaction) -> Self {
        Self::answering(Some(ParkedLinkStanding::Parked(Box::new(spend))))
    }

    fn answering(standing: Option<ParkedLinkStanding>) -> Self {
        Self {
            up: true,
            standing,
            parked: Vec::new(),
            agreement: Some(action_hash(BR_EA).into()),
        }
    }
}

impl Cell for FakeCell {
    fn answers(&self) -> Reading<'_, ()> {
        let answer = if self.up {
            Ok(())
        } else {
            Err(anyhow!("down"))
        };
        Box::pin(std::future::ready(answer))
    }

    fn standing<'a>(&'a self, link: &'a ActionHash) -> Reading<'a, ParkedLinkStanding> {
        let answer = match &self.standing {
            Some(_) if *link != withdrawal() => Ok(self
                .parked
                .iter()
                .find(|spend| spend.id.as_ref() == link)
                .map_or(ParkedLinkStanding::NotHeld, |spend| {
                    ParkedLinkStanding::Parked(Box::new(spend.clone()))
                })),
            Some(standing) => Ok(standing.clone()),
            None => Err(anyhow!("the zome call failed")),
        };
        Box::pin(std::future::ready(answer))
    }

    fn bridging_agreement<'a>(
        &'a self,
        agent: &'a AgentPubKeyB64,
        hot_unit_index: u32,
    ) -> Reading<'a, ActionHashB64> {
        let answer = match &self.agreement {
            Some(agreement) if *agent == bridging_agent() && hot_unit_index == 1 => {
                Ok(agreement.clone())
            }
            _ => Err(anyhow!("no lane in force")),
        };
        Box::pin(std::future::ready(answer))
    }
}

fn withdrawal() -> ActionHash {
    ActionHash::from_raw_32(vec![7; 32])
}

fn spend() -> Transaction {
    paying(json!({ "withdraw_contract_address": VAULT, "withdraw_to_address": RECIPIENT }))
}

fn paying(payload: Value) -> Transaction {
    let mut spend = with_payload(parked_withdrawal_tx(0x60), payload);
    spend.amount = UnitMap::from(vec![(1, "1.5")]);
    spend
}

/// Owner 0's signer for the Safe of the worked example in
/// docs/signer-service.md, on mainnet, with `changes` made to its settings.
fn settings(db: &str, changes: &[(&'static str, Option<&str>)]) -> impl Fn(&str) -> Option<String> {
    let order = ClaimOrder::vector();
    let set = vec![
        ("NETWORK", "mainnet".to_string()),
        ("CLAIM_SIGNER", SAFE.to_string()),
        ("ORDER_HASH", order.order_hash.to_string()),
        ("ORDER_OWNER", order.order_owner.to_string()),
        ("ORDERBOOK_ADDRESS", order.orderbook.to_string()),
        ("TOKEN_ADDRESS", order.token.to_string()),
        ("VAULT_ID", order.vault_id.to_string()),
        ("SAFE_OWNER_KEY", OWNER_KEYS[0].to_string()),
        ("SIGNER_BEARER_TOKEN", TOKEN.to_string()),
        ("SIGNER_BIND_PORT", "0".to_string()),
        ("SIGNER_DB_PATH", db.to_string()),
        ("SCREENING", "on".to_string()),
        ("SCREENING_DENYLIST", Address::repeat_byte(0xBA).to_string()),
        (
            "HOLOCHAIN_BRIDGING_AGENT_PUBKEY",
            bridging_agent().to_string(),
        ),
    ];
    changed_settings(set, changes)
}

fn service(cell: FakeCell, changes: &[(&'static str, Option<&str>)]) -> Service {
    service_at(&test_db_path("signer"), cell, changes)
}

fn service_at(db: &str, cell: FakeCell, changes: &[(&'static str, Option<&str>)]) -> Service {
    let Ok(config) = ServiceConfig::from_settings(settings(db, changes)) else {
        panic!("the test settings were refused");
    };
    Service {
        records: Records::open(&config.db_path).unwrap(),
        config,
        cell: Arc::new(cell),
    }
}

fn request(chain_id: u64, withdrawal: Option<&ActionHash>, context: Vec<B256>) -> SignBody {
    let digest = context_digest(&context);
    SignBody {
        safe: SAFE,
        chain_id,
        owner: owner(0),
        withdrawal: withdrawal.map(|w| ActionHashB64::from(w.clone()).to_string()),
        safe_message_hash: safe_message_hash(chain_id, SAFE, &digest),
        coupon_digest: digest,
        context,
    }
}

/// The coupon of `spend()` at NOW, as the orchestrator asks for it, with
/// `edit` made to its words and the hashes made of the edited words.
fn coupon(edit: impl FnOnce(&mut Vec<B256>)) -> SignBody {
    let mut words = coupon_context(
        RECIPIENT.parse().unwrap(),
        U256::from(1_500_000_000_000_000_000u128),
        U256::from(NOW + WEEK),
        ClaimOrder::vector().words(),
        withdrawal_nonce(&withdrawal()),
    )
    .unwrap();
    edit(&mut words);
    request(1, Some(&withdrawal()), words)
}

fn probe(edit: impl FnOnce(&mut Vec<B256>)) -> SignBody {
    let mut words = probe_context(SAFE, ClaimOrder::vector().words()).unwrap();
    edit(&mut words);
    request(1, None, words)
}

fn bytes(body: &SignBody) -> Vec<u8> {
    serde_json::to_vec(body).unwrap()
}

async fn answer(service: &Service, body: &[u8]) -> Result<Signed, Code> {
    service
        .sign(Some(&format!("Bearer {TOKEN}")), body, NOW)
        .await
        .map_err(|refusal| refusal.code)
}

async fn reason(service: &Service, body: &SignBody) -> String {
    service
        .sign(Some(&format!("Bearer {TOKEN}")), &bytes(body), NOW)
        .await
        .err()
        .map(|refusal| refusal.reason)
        .unwrap_or_default()
}

async fn refused(service: &Service, body: &SignBody) -> Option<Code> {
    answer(service, &bytes(body)).await.err()
}

fn signed(answer: Result<Signed, Code>) -> Signed {
    answer.unwrap_or_else(|code| panic!("refused with {code:?}"))
}

#[tokio::test]
async fn the_worked_example_is_signed_as_the_doc_gives_it() {
    let service = service(FakeCell::holding(spend()), &[]);

    let signed = signed(answer(&service, WORKED_EXAMPLE.as_bytes()).await);

    assert_eq!(signed.owner, owner(0));
    assert_eq!(signed.signature.to_string(), WORKED_EXAMPLE_SIGNATURE);
    assert_eq!(
        serde_json::to_value(coupon(|_| {})).unwrap(),
        serde_json::from_str::<Value>(WORKED_EXAMPLE).unwrap()
    );
}

/// Every check made before the cell is asked, but the expiry's. The startup
/// probe is signed and records nothing.
#[tokio::test]
async fn each_check_of_the_request_refuses_with_its_code() {
    let service = service(FakeCell::holding(spend()), &[]);
    let good = bytes(&coupon(|_| {}));
    for authorization in [
        None,
        Some(TOKEN.to_string()),
        Some(format!("Bearer {TOKEN}x")),
        Some(format!("Bearer {}", &TOKEN[1..])),
        Some("Bearer ".to_string()),
    ] {
        let answer = service.sign(authorization.as_deref(), &good, NOW).await;
        assert_eq!(
            answer.err().map(|refusal| refusal.code),
            Some(Code::AuthFailed),
            "{authorization:?}"
        );
    }

    let edited = |edit: &dyn Fn(&mut serde_json::Map<String, Value>)| {
        let mut body = serde_json::to_value(coupon(|_| {})).unwrap();
        edit(body.as_object_mut().unwrap());
        body.to_string().into_bytes()
    };
    let withdrawal_of = |value: Value| {
        edited(&move |body| {
            body.insert("withdrawal".to_string(), value.clone());
        })
    };
    let agent: AgentPubKeyB64 = AgentPubKey::from_raw_32(vec![7; 32]).into();
    let bad_requests = [
        b"not json".to_vec(),
        edited(&|body| {
            body.remove("safeMessageHash");
        }),
        edited(&|body| {
            body.remove("withdrawal");
        }),
        bytes(&coupon(|words| {
            words.pop();
        })),
        bytes(&coupon(|words| words.push(B256::ZERO))),
        withdrawal_of(json!(agent.to_string())),
        withdrawal_of(json!("uhCkkBwcHBwc")),
        withdrawal_of(json!(withdrawal().get_raw_39())),
    ];
    for body in bad_requests {
        assert_eq!(
            answer(&service, &body).await.err(),
            Some(Code::BadRequest),
            "{}",
            String::from_utf8_lossy(&body)
        );
    }

    let mut other_safe = coupon(|_| {});
    other_safe.safe = owner(1);
    let mut other_owner = coupon(|_| {});
    other_owner.owner = owner(1);
    let mut wrong_digest = coupon(|_| {});
    wrong_digest.coupon_digest = B256::repeat_byte(1);
    let mut wrong_hash = coupon(|_| {});
    wrong_hash.safe_message_hash =
        request(84_532, Some(&withdrawal()), wrong_hash.context.clone()).safe_message_hash;
    let mut probe_wrong_hash = probe(|_| {});
    probe_wrong_hash.safe_message_hash = wrong_hash.safe_message_hash;
    let mut cases = vec![
        (other_safe, Code::WrongSafeOrChain),
        (
            request(84_532, Some(&withdrawal()), coupon(|_| {}).context),
            Code::WrongSafeOrChain,
        ),
        (other_owner, Code::WrongOwner),
        (
            coupon(|words| words[8] = probe_nonce()),
            Code::NonceMismatch,
        ),
        (wrong_digest, Code::HashMismatch),
        (wrong_hash, Code::HashMismatch),
        (
            request(1, None, coupon(|_| {}).context),
            Code::ProbeMismatch,
        ),
        (probe_wrong_hash, Code::HashMismatch),
    ];
    for word in 3..8 {
        cases.push((
            coupon(|words| words[word] = B256::repeat_byte(9)),
            Code::OrderMismatch,
        ));
        cases.push((
            probe(|words| words[word] = B256::repeat_byte(9)),
            Code::OrderMismatch,
        ));
    }
    for word in [0, 1, 2, 8] {
        cases.push((
            probe(|words| words[word] = B256::repeat_byte(9)),
            Code::ProbeMismatch,
        ));
    }
    for (body, code) in cases {
        assert_eq!(
            refused(&service, &body).await,
            Some(code),
            "{}",
            serde_json::to_string(&body).unwrap()
        );
    }
    let mut wrong_digest = coupon(|_| {});
    wrong_digest.coupon_digest = B256::repeat_byte(1);
    assert!(reason(&service, &wrong_digest)
        .await
        .starts_with("couponDigest"));
    let mut wrong_hash = coupon(|_| {});
    wrong_hash.safe_message_hash = B256::repeat_byte(1);
    assert!(reason(&service, &wrong_hash)
        .await
        .starts_with("safeMessageHash"));
    let mut unheld_and_wrong_hash = coupon(|_| {});
    unheld_and_wrong_hash.coupon_digest = B256::repeat_byte(1);
    let unheld = service_with(Some(ParkedLinkStanding::NotHeld));
    assert_eq!(
        refused(&unheld, &unheld_and_wrong_hash).await,
        Some(Code::HashMismatch)
    );

    let with_a_field_it_does_not_know = edited(&|body| {
        body.insert("memo".to_string(), json!("ignored"));
    });
    signed(answer(&service, &with_a_field_it_does_not_know).await);

    let probing = service_at(&test_db_path("signer"), FakeCell::holding(spend()), &[]);
    let probed = signed(answer(&probing, &bytes(&probe(|_| {}))).await);
    assert_eq!(probed.owner, owner(0));
    assert_eq!(
        records_in(&probing.config.db_path),
        0,
        "the probe left a record"
    );
}

/// `now - EXPIRY_TOLERANCE_SECONDS < word 2 <= now + EXPIRY_SECONDS + EXPIRY_TOLERANCE_SECONDS`.
#[tokio::test]
async fn the_expiry_is_signed_only_within_its_window() {
    let at = |expiry: U256| coupon(|words| words[2] = B256::from(expiry));
    let defaults = service(FakeCell::holding(spend()), &[]);
    let configured = service(
        FakeCell::holding(spend()),
        &[
            ("EXPIRY_SECONDS", Some("3600")),
            ("EXPIRY_TOLERANCE_SECONDS", Some("0")),
        ],
    );

    for (service, expiry, signs) in [
        (&defaults, U256::ZERO, false),
        (&defaults, U256::from(NOW - 600), false),
        (&defaults, U256::from(NOW - 599), true),
        (&defaults, U256::from(NOW + WEEK + 600), true),
        (&defaults, U256::from(NOW + WEEK + 601), false),
        (&defaults, U256::MAX, false),
        (&configured, U256::from(NOW), false),
        (&configured, U256::from(NOW + 1), true),
        (&configured, U256::from(NOW + 3600), true),
        (&configured, U256::from(NOW + 3601), false),
    ] {
        let expected = (!signs).then_some(Code::ExpiryOutOfRange);
        assert_eq!(refused(service, &at(expiry)).await, expected, "{expiry}");
    }
}

#[tokio::test]
async fn the_cell_says_whether_the_withdrawal_is_still_parked_and_on_which_lane() {
    for (standing, code) in [
        (Some(ParkedLinkStanding::Unparked), Code::NotAWithdrawal),
        (
            Some(ParkedLinkStanding::NotAParkedLink),
            Code::NotAWithdrawal,
        ),
        (Some(ParkedLinkStanding::NotHeld), Code::UnknownWithdrawal),
        (None, Code::NotReady),
    ] {
        let service = service_with(standing.clone());
        assert_eq!(
            refused(&service, &coupon(|_| {})).await,
            Some(code),
            "{standing:?}"
        );
        if code == Code::NotAWithdrawal {
            let answered = format!("answers {:?}", standing.unwrap());
            assert!(reason(&service, &coupon(|_| {})).await.contains(&answered));
        }
    }

    let other_link = ActionHash::from_raw_32(vec![8; 32]);
    let held = service(FakeCell::holding(spend()), &[]);
    let asked = request(
        1,
        Some(&other_link),
        coupon(|words| words[8] = withdrawal_nonce(&other_link)).context,
    );
    assert_eq!(refused(&held, &asked).await, Some(Code::UnknownWithdrawal));

    let lost_lane = FakeCell {
        agreement: None,
        ..FakeCell::holding(spend())
    };
    let other_agent: AgentPubKeyB64 = AgentPubKey::from_raw_32(vec![2; 32]).into();
    for (cell, changes) in [
        (lost_lane, vec![]),
        (
            FakeCell::holding(spend()),
            vec![("HOLOCHAIN_BRIDGING_AGENT_PUBKEY", other_agent.to_string())],
        ),
        (
            FakeCell::holding(spend()),
            vec![("HOT_UNIT_INDEX", "2".to_string())],
        ),
    ] {
        let changes: Vec<_> = changes
            .iter()
            .map(|(k, v)| (*k, Some(v.as_str())))
            .collect();
        let service = service(cell, &changes);
        assert_eq!(
            refused(&service, &coupon(|_| {})).await,
            Some(Code::NotReady),
            "{changes:?}"
        );
    }
}

#[tokio::test]
async fn only_a_withdrawal_the_bridge_pays_is_signed_as_it_pays() {
    let in_role = |role: &str| {
        let mut spend = spend();
        if let TransactionDetails::ParkedSpend { ct_role_id, .. } = &mut spend.details {
            *ct_role_id = role.to_string();
        }
        spend
    };
    let mut on_another_agreement = spend();
    let mut by_another_executor = spend();
    if let TransactionDetails::ParkedSpend { ea_id, .. } = &mut on_another_agreement.details {
        *ea_id = action_hash(0xEC).into();
    }
    if let TransactionDetails::ParkedSpend { executor, .. } = &mut by_another_executor.details {
        *executor = AgentPubKey::from_raw_32(vec![2; 32]).into();
    }
    let paid = |to: &str| json!({ "withdraw_contract_address": VAULT, "withdraw_to_address": to });
    let amount = |amount: &[(u32, &str)]| {
        let mut spend = spend();
        spend.amount = UnitMap::from(amount.to_vec());
        spend
    };
    let not_withdrawals = [
        parked_tx(0x61, &[]),
        on_another_agreement,
        by_another_executor,
        in_role("bridging_agent"),
        in_role("depositor"),
        paying(
            json!({ "withdraw_contract_address": "0xE3E064e3C2EEf66cb93dA8D8114F5084E92F48D6", "withdraw_to_address": RECIPIENT }),
        ),
        paying(
            json!({ "withdraw_contract_address": "0x2222222222222222222222222222222222222222", "withdraw_to_address": RECIPIENT }),
        ),
        paying(json!({ "withdraw_to_address": RECIPIENT })),
        paying(json!({ "withdraw_contract_address": VAULT })),
        paying(paid("0x11111111111111111111111111111111111111")),
        paying(paid("0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAeD")),
        amount(&[]),
        amount(&[(1, "1.5"), (2, "1")]),
        amount(&[(2, "1.5")]),
        amount(&[(1, "0")]),
    ];
    for spend in not_withdrawals {
        let service = service(FakeCell::holding(spend.clone()), &[]);
        assert_eq!(
            refused(&service, &coupon(|_| {})).await,
            Some(Code::NotAWithdrawal),
            "{:?} {:?}",
            spend.details,
            spend.amount
        );
    }

    let paid_out = service(FakeCell::holding(spend()), &[]);
    let one_wei = amount_word(U256::from(1)).unwrap();
    let other = pad_address(Address::repeat_byte(0x22));
    for (edited, code, word) in [
        (
            coupon(|words| words[1] = one_wei),
            Code::AmountMismatch,
            one_wei,
        ),
        (
            coupon(|words| words[0] = other),
            Code::RecipientMismatch,
            other,
        ),
    ] {
        assert_eq!(refused(&paid_out, &edited).await, Some(code));
        assert!(reason(&paid_out, &edited).await.contains(&word.to_string()));
    }

    let checksummed = "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed";
    let service = service(FakeCell::holding(paying(paid(checksummed))), &[]);
    let to = pad_address(checksummed.parse().unwrap());
    assert_eq!(
        refused(&service, &coupon(|words| words[0] = to)).await,
        None
    );
}

pub(crate) fn records_in(db: &str) -> u64 {
    rusqlite::Connection::open(db)
        .unwrap()
        .query_row("SELECT count(*) FROM records", [], |row| row.get(0))
        .unwrap()
}

#[tokio::test]
async fn a_listed_recipient_is_refused_and_one_the_screener_cannot_answer_for_waits() {
    let listed = service(
        FakeCell::holding(spend()),
        &[(
            "SCREENING_DENYLIST",
            Some(&format!("{},{RECIPIENT}", Address::repeat_byte(0xBA))),
        )],
    );
    assert_eq!(
        refused(&listed, &coupon(|_| {})).await,
        Some(Code::RecipientListed)
    );
    assert_eq!(records_in(&listed.config.db_path), 0);

    let mut down = service(FakeCell::holding(spend()), &[]);
    down.config.screener = Some(Box::new(Down));
    assert_eq!(refused(&down, &coupon(|_| {})).await, Some(Code::NotReady));

    let unscreened = service(
        FakeCell::holding(spend()),
        &[
            ("NETWORK", Some("base-sepolia")),
            ("SCREENING", Some("off")),
            ("SCREENING_DENYLIST", Some(RECIPIENT)),
        ],
    );
    let on_sepolia = request(84_532, Some(&withdrawal()), coupon(|_| {}).context);
    assert_eq!(refused(&unscreened, &on_sepolia).await, None);
}

#[tokio::test]
async fn the_records_outlive_a_restart_and_stop_a_second_order() {
    let db = test_db_path("signer-records");
    let first = service_at(&db, FakeCell::holding(spend()), &[]);
    signed(answer(&first, &bytes(&coupon(|_| {}))).await);
    drop(first);

    let restarted = service_at(&db, FakeCell::holding(spend()), &[]);
    signed(answer(&restarted, &bytes(&coupon(|_| {}))).await);

    let rotated = B256::repeat_byte(3);
    let on_rotated = service_at(
        &db,
        FakeCell::holding(spend()),
        &[("ORDER_HASH", Some(&rotated.to_string()))],
    );
    assert_eq!(
        refused(&on_rotated, &coupon(|words| words[3] = rotated)).await,
        Some(Code::AlreadySigned)
    );
    assert_eq!(records_in(&db), 1);
}

#[tokio::test]
async fn a_signer_that_cannot_write_its_record_does_not_sign() {
    let unwritable = service(FakeCell::holding(spend()), &[]);
    rusqlite::Connection::open(&unwritable.config.db_path)
        .unwrap()
        .execute(
            "CREATE TRIGGER refuse BEFORE INSERT ON records BEGIN SELECT RAISE(ABORT, 'refused'); END",
            [],
        )
        .unwrap();

    assert_eq!(
        refused(&unwritable, &coupon(|_| {})).await,
        Some(Code::NotReady)
    );
}

async fn serve(service: Service) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = router(Arc::new(service));
    tokio::spawn(async move { axum::serve(listener, router).await });
    url
}

pub(crate) async fn served_owner(i: usize, db: &str, parked: &[Transaction]) -> OwnerService {
    let cell = FakeCell {
        parked: parked.to_vec(),
        ..FakeCell::holding(spend())
    };
    let url = serve(service_at(
        db,
        cell,
        &[("SAFE_OWNER_KEY", Some(OWNER_KEYS[i]))],
    ))
    .await;
    OwnerService::new(Network::Mainnet, owner(i), &url)
        .unwrap()
        .with_token(Some(TOKEN.to_string()))
        .unwrap()
}

#[tokio::test]
async fn the_orchestrator_gets_its_coupons_signed_here() {
    let chain = FakeSafe::new(2, &[0, 1, 2]);
    let rpc = chain.serve().await;
    let here = served_owner(0, &test_db_path("signer"), &[]).await;

    let identity = here.identity().await.unwrap();
    assert_eq!(
        (identity.owner, identity.safe, identity.chain_id),
        (owner(0), SAFE, 1)
    );

    let owners = SafeOwners::new(
        SAFE,
        1,
        vec![Owner::Key(owner_key(1)), Owner::Service(here)],
        rpc,
    )
    .unwrap();
    let signer = CouponSigner::with_key(owners);
    signer.probe_coupon().await.unwrap();
    let terms = || Payout::new(RECIPIENT, "1.5").unwrap();
    signer.coupon(terms(), &withdrawal()).await.unwrap();

    let unheld = ActionHash::from_raw_32(vec![8; 32]);
    let err = format!("{:#}", signer.coupon(terms(), &unheld).await.unwrap_err());
    assert!(err.contains("refused: unknown_withdrawal"), "{err}");
}

#[tokio::test]
async fn the_endpoints_answer_as_the_contract_says() {
    let client = reqwest::Client::new();
    let up = serve(service(FakeCell::holding(spend()), &[])).await;
    let down = serve(service(
        FakeCell {
            up: false,
            ..FakeCell::holding(spend())
        },
        &[],
    ))
    .await;

    let lost = service(FakeCell::holding(spend()), &[]);
    rusqlite::Connection::open(&lost.config.db_path)
        .unwrap()
        .execute("DROP TABLE records", [])
        .unwrap();
    let without_records = serve(lost).await;

    let healthz = |url: &str| client.get(format!("{url}/healthz")).send();
    let healthy = healthz(&up).await.unwrap();
    assert_eq!(healthy.status(), 200);
    for url in [&down, &without_records] {
        let unhealthy = healthz(url).await.unwrap();
        assert_eq!(unhealthy.status(), 503, "{url}");
        assert_eq!(unhealthy.text().await.unwrap(), "");
    }

    for answer in [
        client.get(format!("{up}/signer")).send().await.unwrap(),
        client
            .get(format!("{up}/signer"))
            .bearer_auth("other-token")
            .send()
            .await
            .unwrap(),
        client
            .post(format!("{up}/sign"))
            .json(&coupon(|_| {}))
            .send()
            .await
            .unwrap(),
    ] {
        assert_eq!(answer.status(), 401);
        let body: Value = answer.json().await.unwrap();
        assert_eq!(body["code"], "auth_failed", "{body}");
    }

    let post = |body: Vec<u8>| {
        client
            .post(format!("{up}/sign"))
            .bearer_auth(TOKEN)
            .body(body)
            .send()
    };
    let malformed = post(b"{}".to_vec()).await.unwrap();
    assert_eq!(malformed.status(), 400);
    let body: Value = malformed.json().await.unwrap();
    assert_eq!(body["code"], "bad_request", "{body}");

    let expired = post(bytes(&coupon(|_| {}))).await.unwrap();
    assert_eq!(expired.status(), 409);
    let body: Value = expired.json().await.unwrap();
    assert_eq!(body["code"], "expiry_out_of_range", "{body}");
    assert!(!body["reason"].as_str().unwrap().is_empty(), "{body}");
}

#[tokio::test]
async fn each_answer_is_logged_at_its_level_without_the_token_or_the_key() {
    let service = service(FakeCell::holding(spend()), &[]);
    let good = bytes(&coupon(|_| {}));

    let logged = logged_at(tracing::Level::INFO, async {
        let _ = service.sign(Some("Bearer wrong"), b"{}", NOW).await;
        let _ = service.sign(Some(TOKEN), &good, NOW).await;
        let _ = service
            .sign(Some(&format!("Bearer {TOKEN}x")), &good, NOW)
            .await;
        let _ = service.identity(None);
        let _ = answer(&service, b"{}").await;
        let _ = refused(&service, &coupon(|words| words[1] = B256::ZERO)).await;
        let mut other_owner = coupon(|_| {});
        other_owner.owner = owner(1);
        let _ = refused(&service, &other_owner).await;
        let _ = refused(&service, &probe(|_| {})).await;
        let _ = refused(&service, &coupon(|_| {})).await;
        let unheld = service_with(Some(ParkedLinkStanding::NotHeld));
        let _ = refused(&unheld, &coupon(|_| {})).await;
        let silent = service_with(None);
        let _ = refused(&silent, &coupon(|_| {})).await;
    })
    .await;

    let lines: Vec<&str> = logged.lines().collect();
    let at = |level: &str, event: &str| {
        lines
            .iter()
            .filter(|line| line.contains(level) && line.contains(&format!("event=\"{event}\"")))
            .count()
    };
    assert_eq!(at("WARN", "signer.unauthorized"), 4, "{logged}");
    assert_eq!(at("ERROR", "signer.refused"), 3, "{logged}");
    assert_eq!(at("WARN", "signer.waiting"), 2, "{logged}");
    assert_eq!(at("INFO", "signer.signed"), 2, "{logged}");
    for code in [
        "bad_request",
        "amount_mismatch",
        "unknown_withdrawal",
        "not_ready",
    ] {
        assert!(
            logged.contains(&format!("code=\"{code}\"")),
            "{code}: {logged}"
        );
    }
    assert!(logged.contains("the zome call failed"), "{logged}");
    let withdrawal = ActionHashB64::from(withdrawal()).to_string();
    for event in [
        "signer.signed",
        "wrong_owner",
        "amount_mismatch",
        "not_ready",
    ] {
        assert!(
            lines
                .iter()
                .any(|line| line.contains(event)
                    && line.contains(&format!("withdrawal={withdrawal}"))),
            "{event}: {logged}"
        );
    }
    assert!(
        logged.contains(&format!("asks owner {}", owner(1))),
        "{logged}"
    );
    assert!(!logged.contains(TOKEN), "{logged}");
    assert!(!logged.contains(&OWNER_KEYS[0][2..]), "{logged}");
}

fn service_with(standing: Option<ParkedLinkStanding>) -> Service {
    service(FakeCell::answering(standing), &[])
}

fn refused_config(changes: &[(&'static str, Option<&str>)]) -> String {
    match ServiceConfig::from_settings(settings("unused.db", changes)) {
        Ok(_) => panic!("the settings were accepted"),
        Err(err) => format!("{err:#}"),
    }
}

#[test]
fn the_settings_name_every_fault_and_never_the_key_or_the_token() {
    let almost_a_key = &OWNER_KEYS[0][..64];
    let refusal = refused_config(&[
        ("CLAIM_SIGNER", None),
        ("ORDER_HASH", Some("0x12")),
        ("SAFE_OWNER_KEY", Some(almost_a_key)),
        ("SIGNER_BIND_ADDR", Some("localhost")),
        ("SIGNER_BIND_PORT", Some("65536")),
        ("SIGNER_DB_PATH", None),
        ("EXPIRY_SECONDS", Some("0")),
        ("EXPIRY_TOLERANCE_SECONDS", Some("-1")),
        ("SCREENING", Some("maybe")),
        ("HOLOCHAIN_BRIDGING_AGENT_PUBKEY", None),
        ("HOT_UNIT_INDEX", Some("hot")),
        ("HOLOCHAIN_ADMIN_PORT", Some("admin")),
    ]);

    for fault in [
        "CLAIM_SIGNER is required",
        "ORDER_HASH=0x12 is not a 32-byte hash",
        "SAFE_OWNER_KEY is not a secp256k1 private key",
        "SIGNER_BIND_ADDR=localhost is not an IP address",
        "SIGNER_BIND_PORT=65536 is not a port",
        "SIGNER_DB_PATH is required",
        "EXPIRY_SECONDS=0 is not a number of seconds",
        "EXPIRY_TOLERANCE_SECONDS=-1 is not a number of seconds",
        "SCREENING=maybe is not on or off",
        "HOLOCHAIN_BRIDGING_AGENT_PUBKEY required",
        "Invalid HOT_UNIT_INDEX",
        "Invalid HOLOCHAIN_ADMIN_PORT",
    ] {
        assert!(refusal.contains(fault), "{fault}: {refusal}");
    }
    assert!(!refusal.contains(&almost_a_key[2..]), "{refusal}");
    assert!(!refusal.contains(TOKEN), "{refusal}");

    assert!(refused_config(&[("SIGNER_BEARER_TOKEN", None)])
        .contains("SIGNER_BEARER_TOKEN is required"));
    assert!(refused_config(&[("SIGNER_BEARER_TOKEN", Some("token "))])
        .contains("SIGNER_BEARER_TOKEN holds a character outside visible ASCII"));
    assert!(refused_config(&[("SAFE_OWNER_KEY", None)]).contains("SAFE_OWNER_KEY is required"));
    assert_eq!(
        refused_config(&[
            ("NETWORK", Some("none")),
            ("CLAIM_SIGNER", None),
            ("ORDERBOOK_ADDRESS", None),
            ("VAULT_ID", None),
            ("SCREENING", Some("maybe")),
        ]),
        "NETWORK=none is not base-sepolia or mainnet; CLAIM_SIGNER is required; SCREENING=maybe is not on or off"
    );
    assert!(refused_config(&[(
        "SAFE_OWNER_KEY",
        Some("0xdcbe53cbf4cbee212fe6339821058f2787c7726ae0684335118cdea2e8adaafd")
    )])
    .contains("SAFE_OWNER_KEY is the test signer"));
    assert!(refused_config(&[("SCREENING", Some("off"))])
        .contains("SCREENING=off is refused on mainnet"));
    assert_eq!(
        refused_config(&[("ORDERBOOK_ADDRESS", None), ("VAULT_ID", None)]),
        "ORDERBOOK_ADDRESS is required; VAULT_ID is required"
    );
}

#[test]
fn testnet_defaults_the_orderbook_and_vault_but_never_the_safe() {
    let testnet = [
        ("NETWORK", None),
        ("ORDERBOOK_ADDRESS", None),
        ("VAULT_ID", None),
        ("SCREENING", Some("off")),
        ("SIGNER_BIND_PORT", Some("8123")),
    ];
    let without_safe: Vec<_> = testnet
        .iter()
        .copied()
        .chain([("CLAIM_SIGNER", None)])
        .collect();
    assert_eq!(refused_config(&without_safe), "CLAIM_SIGNER is required");

    let Ok(config) = ServiceConfig::from_settings(settings("unused.db", &testnet)) else {
        panic!("TestNet's settings were refused");
    };
    let default = |key: &str| with_network_defaults(Network::BaseSepolia, |_| None)(key).unwrap();
    assert_eq!(config.network, Network::BaseSepolia);
    assert_eq!(
        config.order[2],
        pad_address(default("ORDERBOOK_ADDRESS").parse().unwrap())
    );
    assert_eq!(
        config.order[4],
        default("VAULT_ID").parse::<B256>().unwrap()
    );
    assert_eq!(config.bind.to_string(), "127.0.0.1:8123");
    assert_eq!(config.expiry_seconds.get(), WEEK);
    assert_eq!(config.expiry_tolerance_seconds, 600);
    assert_eq!(config.hot_unit_index, 1);
    assert!(config.screener.is_none());
}
