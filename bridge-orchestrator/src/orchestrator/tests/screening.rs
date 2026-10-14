use super::*;
use crate::screening::{Action, DenyList, Screened, Screening, Transfer, Verdict};
use crate::state::{Standing, WorkState};

const LISTED: Address = Address::repeat_byte(0xBA);

fn screening_with(orch: &mut BridgeOrchestrator, screener: impl Screener + 'static) {
    orch.ethereum.as_mut().unwrap().screener = Some(Box::new(screener));
}

/// A lock in `screening`, sent by `sender`.
fn lock_to_screen(orch: &BridgeOrchestrator, lock: &str, tx_hash: &str, sender: Address) -> i64 {
    let id = enqueue_lock(orch, lock, tx_hash);
    let mut payload = lock_row(orch, id).payload_json;
    payload["sender"] = json!(format!("{sender:?}"));
    payload["from"] = json!(format!("{sender:?}"));
    rusqlite::Connection::open(&orch.cfg.db_path)
        .unwrap()
        .execute(
            "UPDATE work_items SET state = 'screening', payload_json = ?2 WHERE id = ?1",
            rusqlite::params![id, payload.to_string()],
        )
        .unwrap();
    id
}

fn state_of(orch: &BridgeOrchestrator, id: i64) -> (WorkState, WorkStep) {
    let row = orch
        .db
        .list_flow("lock")
        .unwrap()
        .into_iter()
        .find(|row| row.id == id)
        .unwrap();
    (row.state, row.step)
}

async fn screen_locks(orch: &BridgeOrchestrator) {
    orch.screen_waiting_locks(orch.screener().unwrap(), &running())
        .await
        .unwrap();
}

fn release(orch: &BridgeOrchestrator, transfer: Transfer) {
    orch.db
        .act(&transfer, Action::Release, "Ana", "a false hit")
        .unwrap();
}

/// A provider that never answers.
struct Silent;

impl std::fmt::Display for Silent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("silent")
    }
}

impl Screener for Silent {
    fn screen<'a>(&'a self, _: &'a [Address]) -> Screening<'a> {
        Box::pin(std::future::ready(Screened {
            provider: "silent",
            list: None,
            at: 0,
            verdict: Verdict::NoAnswer("it timed out".to_string()),
        }))
    }
}

#[tokio::test]
async fn a_lock_in_screening_held_or_blocked_never_reaches_s1_nor_is_advanced_by_reconcile() {
    let orch = test_orchestrator("screening-gate");
    let waiting =
        [("screening", "0xd1"), ("held", "0xd2"), ("blocked", "0xd3")].map(|(state, tx_hash)| {
            let id = enqueue_lock(&orch, &format!("lock:{state}"), tx_hash);
            set_state(&orch, id, state);
            id
        });
    let carrying = parked_tx(
        0xD4,
        &[
            proof("lock:screening", "0xd1"),
            proof("lock:held", "0xd2"),
            proof("lock:blocked", "0xd3"),
        ],
    );

    assert_eq!(
        reconcile(&orch, std::slice::from_ref(&carrying), &[]).await,
        ReconcileCounts::default()
    );
    let conductor = bridging_conductor();
    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();

    assert!(!conductor.calls.take().contains(&"create_parked_link"));
    for (id, state) in
        waiting
            .into_iter()
            .zip([WorkState::Screening, WorkState::Held, WorkState::Blocked])
    {
        assert_eq!(state_of(&orch, id), (state, WorkStep::New));
    }
}

#[tokio::test]
async fn a_clear_lock_is_credited_and_a_held_one_only_once_a_person_releases_it() {
    let mut orch = test_orchestrator("screening-credit");
    screening_with(&mut orch, DenyList::new([LISTED]));
    let clear = lock_to_screen(&orch, "lock:clear", "0xe1", Address::repeat_byte(0x11));
    let held = lock_to_screen(&orch, "lock:held", "0xe2", LISTED);

    let logged = logged_at(tracing::Level::INFO, screen_locks(&orch)).await;

    assert_eq!(state_of(&orch, clear).0, WorkState::Queued);
    assert_eq!(state_of(&orch, held).0, WorkState::Held);
    assert!(logged.contains("bridge.screening.clear"), "{logged}");
    assert!(
        logged.contains("bridge.screening.hit") && logged.contains(&format!("{LISTED:#x}")),
        "{logged}"
    );

    let conductor = bridging_conductor();
    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();
    assert_eq!(
        state_of(&orch, clear),
        (WorkState::Succeeded, WorkStep::BrRaveExecuted)
    );
    assert_eq!(state_of(&orch, held), (WorkState::Held, WorkStep::New));

    release(&orch, Transfer::of_lock_row("lock:held"));
    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();
    assert_eq!(
        state_of(&orch, held),
        (WorkState::Succeeded, WorkStep::BrRaveExecuted)
    );
}

#[tokio::test]
async fn a_held_withdrawal_gets_no_coupon_until_a_person_releases_it() {
    let mut orch = test_orchestrator("screening-withdrawal");
    let held = parked_withdrawal_tx(0xBA);
    let clear = parked_withdrawal_tx(0x11);
    screening_with(&mut orch, DenyList::new([LISTED]));
    let conductor =
        bridging_conductor().parking(action_hash(BR_EA), &[held.clone(), clear.clone()]);
    let spend = held.id.to_string();

    let logged = logged_at(tracing::Level::INFO, async {
        orch.run_bridge_cycle(&conductor, &running()).await.unwrap()
    })
    .await;

    assert_eq!(
        ids(&conductor.parked.borrow()[&action_hash(BR_EA)]),
        ids(std::slice::from_ref(&held))
    );
    assert_eq!(
        orch.db.withdrawal_standing(&spend).unwrap(),
        Some(Standing::Held)
    );
    assert!(logged.contains("bridge.screening.hit"), "{logged}");
    assert!(logged.contains("deferred=0 unpayable=0 held=1"), "{logged}");

    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();
    assert_eq!(
        ids(&conductor.parked.borrow()[&action_hash(BR_EA)]),
        ids(std::slice::from_ref(&held))
    );
    let screened = orch
        .db
        .screenings(Some(&Transfer::Withdrawal(spend.clone())), false)
        .unwrap();
    assert_eq!(
        screened[0]["records"].as_array().unwrap().len(),
        1,
        "a hit is never screened again"
    );

    release(&orch, Transfer::Withdrawal(spend));
    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();
    assert!(conductor.parked.borrow()[&action_hash(BR_EA)].is_empty());
}

#[tokio::test]
async fn with_no_answer_a_lock_waits_in_screening_and_a_withdrawal_stays_parked() {
    let mut orch = test_orchestrator("screening-silent");
    screening_with(&mut orch, Silent);
    let lock = lock_to_screen(&orch, "lock:silent", "0xe3", Address::repeat_byte(0x11));
    let withdrawal = parked_withdrawal_tx(0x11);
    let conductor =
        bridging_conductor().parking(action_hash(BR_EA), std::slice::from_ref(&withdrawal));

    for _ in 0..2 {
        screen_locks(&orch).await;
        orch.run_bridge_cycle(&conductor, &running()).await.unwrap();
    }

    assert_eq!(state_of(&orch, lock), (WorkState::Screening, WorkStep::New));
    assert!(!conductor.calls.take().contains(&"create_parked_link"));
    assert_eq!(
        ids(&conductor.parked.borrow()[&action_hash(BR_EA)]),
        ids(std::slice::from_ref(&withdrawal))
    );
    for transfer in [
        Transfer::of_lock_row("lock:silent"),
        Transfer::Withdrawal(withdrawal.id.to_string()),
    ] {
        let records = orch.db.screenings(Some(&transfer), false).unwrap()[0]["records"].clone();
        assert_eq!(
            records.as_array().unwrap().len(),
            1,
            "{transfer}: {records}"
        );
        assert_eq!(records[0]["event"], "no_answer");
    }
}

#[tokio::test]
async fn with_screening_off_nothing_that_waits_moves_and_only_an_unscreened_withdrawal_is_paid() {
    let mut orch = test_orchestrator("screening-off");
    screening_with(&mut orch, DenyList::new([LISTED]));
    let waiting = lock_to_screen(&orch, "lock:waiting", "0xe4", Address::repeat_byte(0x11));
    let held = parked_withdrawal_tx(0xBA);
    orch.run_bridge_cycle(
        &bridging_conductor().parking(action_hash(BR_EA), std::slice::from_ref(&held)),
        &running(),
    )
    .await
    .unwrap();
    orch.ethereum.as_mut().unwrap().screener = None;
    let unscreened = parked_withdrawal_tx(0x12);
    let conductor =
        bridging_conductor().parking(action_hash(BR_EA), &[held.clone(), unscreened.clone()]);

    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();

    assert_eq!(
        state_of(&orch, waiting),
        (WorkState::Screening, WorkStep::New)
    );
    assert_eq!(
        ids(&conductor.parked.borrow()[&action_hash(BR_EA)]),
        ids(&[held])
    );
    assert_eq!(
        orch.db
            .withdrawal_standing(&unscreened.id.to_string())
            .unwrap(),
        None,
        "screening off records nothing"
    );
}

#[tokio::test]
async fn the_start_names_the_list_or_with_screening_off_each_lock_that_waits() {
    let orch = test_orchestrator("screening-start");
    let path = orch.cfg.db_path.clone();
    lock_to_screen(&orch, "lock:waiting", "0xe5", LISTED);
    let held = lock_to_screen(&orch, "lock:held", "0xe6", LISTED);
    set_state(&orch, held, "held");
    drop(orch);
    let start = |screener: Option<Box<dyn Screener>>| {
        let side = EthereumSide {
            chain: Ethereum {
                network: crate::config::Network::BaseSepolia,
                rpc_url: "http://localhost:0".to_string(),
                lock_vault_address: VAULT,
                confirmations: 5,
            },
            signer: CouponSigner::with_key(PrivateKeySigner::random()),
            screener,
        };
        let path = path.clone();
        logged_at(tracing::Level::INFO, async move {
            BridgeOrchestrator::new(test_config(path), Some(side)).unwrap();
        })
    };

    let list = DenyList::new([LISTED]);
    let described = list.to_string();
    let on = start(Some(Box::new(list))).await;
    let off = start(None).await;

    assert!(
        on.contains("startup.screening") && on.contains(&described),
        "{on}"
    );
    assert!(
        off.contains("screening is off, so 2 lock(s) stay where they wait: lock:waiting (screening), lock:held (held)"),
        "{off}"
    );
}
