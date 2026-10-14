use super::*;
use crate::screening::{Answer, Answering, Answers, DenyList, Transfer};
use crate::state::{Standing, WorkState};

const LISTED: Address = Address::repeat_byte(0xBA);
const CLEAN: Address = Address::repeat_byte(0x11);

fn screening_with(orch: &mut BridgeOrchestrator, screener: impl Screener + 'static) {
    orch.ethereum.as_mut().unwrap().screener = Some(Box::new(screener));
}

fn lock_to_screen(orch: &BridgeOrchestrator, lock: &str, tx_hash: &str, sender: Address) -> i64 {
    lock_from(orch, lock, tx_hash, sender, sender)
}

fn lock_from(
    orch: &BridgeOrchestrator,
    lock: &str,
    tx_hash: &str,
    sender: Address,
    from: Address,
) -> i64 {
    let id = enqueue_lock(orch, lock, tx_hash);
    let mut payload = lock_row(orch, id).payload_json;
    payload["sender"] = json!(format!("{sender:?}"));
    payload["from"] = json!(format!("{from:?}"));
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

fn decide(orch: &BridgeOrchestrator, transfer: Transfer, action: crate::screening::Action) {
    orch.db.act(&transfer, action, "Ana", "checked").unwrap();
}

fn records_of(orch: &BridgeOrchestrator, transfer: Transfer) -> Vec<Value> {
    orch.db.screenings(Some(&transfer), false).unwrap()[0]["records"]
        .as_array()
        .unwrap()
        .clone()
}

fn parked_on_bridging(conductor: &FakeConductor) -> Vec<String> {
    ids(&conductor.parked.borrow()[&action_hash(BR_EA)])
}

struct Silent;

impl std::fmt::Display for Silent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("silent")
    }
}

impl Screener for Silent {
    fn name(&self) -> &'static str {
        "silent"
    }

    fn answer<'a>(&'a self, addresses: &'a [Address]) -> Answering<'a> {
        Box::pin(std::future::ready(Answers {
            list: None,
            at: 0,
            of: addresses
                .iter()
                .map(|address| (*address, Answer::Failed("it timed out".to_string())))
                .collect(),
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

#[test]
fn the_lock_read_records_locks_for_screening_exactly_when_a_screener_is_configured() {
    let mut orch = test_orchestrator("screening-wiring");
    assert!(!orch.lock_flow(&running()).unwrap().screening);

    screening_with(&mut orch, DenyList::new([LISTED]));

    assert!(orch.lock_flow(&running()).unwrap().screening);
}

#[tokio::test]
async fn a_failed_lock_read_still_screens_the_locks_already_waiting() {
    let mut orch = test_orchestrator("screening-after-failed-read");
    screening_with(&mut orch, DenyList::new([LISTED]));
    let held = lock_to_screen(&orch, "lock:held", "0xe8", LISTED);
    let lock_flow = orch.lock_flow(&running()).unwrap();

    let logged = logged_at(
        tracing::Level::ERROR,
        orch.read_and_screen_locks(&lock_flow, &running()),
    )
    .await;

    assert!(logged.contains("bridge.lock_read_failed"), "{logged}");
    assert_eq!(state_of(&orch, held).0, WorkState::Held);
}

#[tokio::test]
async fn a_clear_lock_is_credited_and_a_held_one_only_once_a_person_releases_it() {
    let mut orch = test_orchestrator("screening-credit");
    screening_with(&mut orch, DenyList::new([LISTED]));
    let clear = lock_to_screen(&orch, "lock:clear", "0xe1", CLEAN);
    let held = lock_to_screen(&orch, "lock:held", "0xe2", LISTED);
    let sent_by_listed = lock_from(&orch, "lock:sent-by-listed", "0xe7", CLEAN, LISTED);

    let logged = logged_at(tracing::Level::ERROR, screen_locks(&orch)).await;

    assert_eq!(state_of(&orch, clear).0, WorkState::Queued);
    assert_eq!(state_of(&orch, held).0, WorkState::Held);
    assert_eq!(state_of(&orch, sent_by_listed).0, WorkState::Held);
    let records = records_of(&orch, Transfer::of_lock_row("lock:sent-by-listed"));
    assert_eq!(records[0]["addresses"][1]["role"], "from");
    assert_eq!(records[0]["addresses"][1]["listed"], true);
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

    decide(
        &orch,
        Transfer::of_lock_row("lock:held"),
        crate::screening::Action::Release,
    );
    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();
    assert_eq!(
        state_of(&orch, held),
        (WorkState::Succeeded, WorkStep::BrRaveExecuted)
    );
}

#[tokio::test]
async fn a_lock_that_names_no_address_to_screen_waits_and_is_reported() {
    let mut orch = test_orchestrator("screening-unreadable");
    screening_with(&mut orch, DenyList::new([LISTED]));
    let id = lock_to_screen(&orch, "lock:no-from", "0xe9", CLEAN);
    rusqlite::Connection::open(&orch.cfg.db_path)
        .unwrap()
        .execute(
            "UPDATE work_items SET payload_json = json_remove(payload_json, '$.from') WHERE id = ?1",
            [id],
        )
        .unwrap();

    let logged = logged_at(tracing::Level::ERROR, screen_locks(&orch)).await;

    assert_eq!(state_of(&orch, id).0, WorkState::Screening);
    assert!(
        logged.contains("bridge.screening.unreadable")
            && logged.contains("its payload has no from"),
        "{logged}"
    );
}

#[tokio::test]
async fn s4_counts_a_withdrawal_screening_holds_apart_and_signs_it_no_coupon() {
    let orch = test_orchestrator("screening-select");
    let signer = CouponSigner::with_key(PrivateKeySigner::random());
    let list = DenyList::new([Address::repeat_byte(0xBA)]);
    let held = parked_withdrawal_tx(0xBA);
    let clear = parked_withdrawal_tx(0x11);

    let selection = select_bridging_links(
        Some(&signer),
        &WithdrawalGate {
            db: &orch.db,
            screener: Some(&list),
        },
        &bridging_agent(),
        &[held.clone(), clear.clone()],
        usize::MAX,
        1,
    )
    .await
    .unwrap();

    assert_eq!(selection.held, 1);
    assert_eq!(
        ids(&selection.withdrawals),
        ids(std::slice::from_ref(&clear))
    );
    assert_eq!(
        selection.coupons.keys().cloned().collect::<Vec<_>>(),
        ids(&[clear])
    );
    assert_eq!(
        orch.db.withdrawal_standing(&held.id.to_string()).unwrap(),
        Standing::Held
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

    let logged = logged_at(tracing::Level::ERROR, async {
        orch.run_bridge_cycle(&conductor, &running()).await.unwrap()
    })
    .await;

    assert_eq!(
        parked_on_bridging(&conductor),
        ids(std::slice::from_ref(&held))
    );
    assert_eq!(orch.db.withdrawal_standing(&spend).unwrap(), Standing::Held);
    assert!(logged.contains("bridge.screening.hit"), "{logged}");

    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();
    assert_eq!(
        parked_on_bridging(&conductor),
        ids(std::slice::from_ref(&held))
    );
    assert_eq!(
        records_of(&orch, Transfer::Withdrawal(spend.clone())).len(),
        1,
        "a hit is never screened again"
    );

    decide(
        &orch,
        Transfer::Withdrawal(spend),
        crate::screening::Action::Release,
    );
    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();
    assert!(parked_on_bridging(&conductor).is_empty());
}

#[tokio::test]
async fn a_blocked_withdrawal_is_never_screened_again_or_paid() {
    let mut orch = test_orchestrator("screening-blocked-withdrawal");
    screening_with(&mut orch, Silent);
    let blocked = parked_withdrawal_tx(0x11);
    let conductor =
        bridging_conductor().parking(action_hash(BR_EA), std::slice::from_ref(&blocked));
    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();
    decide(
        &orch,
        Transfer::Withdrawal(blocked.id.to_string()),
        crate::screening::Action::Block,
    );
    screening_with(&mut orch, DenyList::new([LISTED]));

    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();

    assert_eq!(
        parked_on_bridging(&conductor),
        ids(std::slice::from_ref(&blocked))
    );
    let events: Vec<_> = records_of(&orch, Transfer::Withdrawal(blocked.id.to_string()))
        .iter()
        .map(|record| record["event"].clone())
        .collect();
    assert_eq!(events, [json!("no_answer"), json!("blocked")]);
}

struct BlockedWhileAsked {
    db: StateStore,
    spend: String,
}

impl std::fmt::Display for BlockedWhileAsked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("blocked while asked")
    }
}

impl Screener for BlockedWhileAsked {
    fn name(&self) -> &'static str {
        "slow"
    }

    fn answer<'a>(&'a self, addresses: &'a [Address]) -> Answering<'a> {
        self.db
            .act(
                &Transfer::Withdrawal(self.spend.clone()),
                crate::screening::Action::Block,
                "Ana",
                "listed elsewhere",
            )
            .unwrap();
        Box::pin(std::future::ready(Answers {
            list: None,
            at: 0,
            of: addresses
                .iter()
                .map(|address| (*address, Answer::NotListed))
                .collect(),
        }))
    }
}

#[tokio::test]
async fn a_block_made_while_a_withdrawal_is_screened_wins_over_a_clear_answer() {
    let mut orch = test_orchestrator("screening-block-race");
    let withdrawal = parked_withdrawal_tx(0x11);
    let spend = withdrawal.id.to_string();
    let db = orch.db.clone();
    screening_with(
        &mut orch,
        BlockedWhileAsked {
            db,
            spend: spend.clone(),
        },
    );
    let conductor =
        bridging_conductor().parking(action_hash(BR_EA), std::slice::from_ref(&withdrawal));

    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();

    assert_eq!(
        parked_on_bridging(&conductor),
        ids(std::slice::from_ref(&withdrawal))
    );
    assert_eq!(
        orch.db.withdrawal_standing(&spend).unwrap(),
        Standing::Blocked
    );
}

#[tokio::test]
async fn with_no_answer_a_lock_waits_and_a_withdrawal_stays_parked_until_one_comes() {
    let mut orch = test_orchestrator("screening-silent");
    screening_with(&mut orch, Silent);
    let lock = lock_to_screen(&orch, "lock:silent", "0xe3", CLEAN);
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
        parked_on_bridging(&conductor),
        ids(std::slice::from_ref(&withdrawal))
    );
    for transfer in [
        Transfer::of_lock_row("lock:silent"),
        Transfer::Withdrawal(withdrawal.id.to_string()),
    ] {
        let records = records_of(&orch, transfer.clone());
        assert_eq!(records.len(), 1, "{transfer}: {records:?}");
        assert_eq!(records[0]["event"], "no_answer");
    }

    screening_with(&mut orch, DenyList::new([LISTED]));
    screen_locks(&orch).await;
    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();

    assert_eq!(state_of(&orch, lock).0, WorkState::Succeeded);
    assert!(parked_on_bridging(&conductor).is_empty());
}

#[tokio::test]
async fn with_screening_off_nothing_that_waits_moves_and_only_an_unscreened_withdrawal_is_paid() {
    let mut orch = test_orchestrator("screening-off");
    screening_with(&mut orch, DenyList::new([LISTED]));
    let waiting = lock_to_screen(&orch, "lock:waiting", "0xe4", CLEAN);
    let held = parked_withdrawal_tx(0xBA);
    orch.run_bridge_cycle(
        &bridging_conductor().parking(action_hash(BR_EA), std::slice::from_ref(&held)),
        &running(),
    )
    .await
    .unwrap();
    let unanswered = parked_withdrawal_tx(0x13);
    screening_with(&mut orch, Silent);
    orch.run_bridge_cycle(
        &bridging_conductor().parking(action_hash(BR_EA), std::slice::from_ref(&unanswered)),
        &running(),
    )
    .await
    .unwrap();
    orch.ethereum.as_mut().unwrap().screener = None;
    let unscreened = parked_withdrawal_tx(0x12);
    let conductor = bridging_conductor().parking(
        action_hash(BR_EA),
        &[held.clone(), unanswered.clone(), unscreened.clone()],
    );

    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();

    assert_eq!(
        state_of(&orch, waiting),
        (WorkState::Screening, WorkStep::New)
    );
    assert_eq!(parked_on_bridging(&conductor), ids(&[held, unanswered]));
    assert_eq!(
        orch.db
            .withdrawal_standing(&unscreened.id.to_string())
            .unwrap(),
        Standing::Unscreened,
        "screening off records nothing"
    );
}

#[test]
fn the_start_names_the_list_or_with_screening_off_each_lock_that_waits() {
    let orch = test_orchestrator("screening-start");
    let list = DenyList::new([LISTED]);
    assert_eq!(
        screening_at_start(&orch.db, Some(&list)).unwrap(),
        format!("screening is on: {list}")
    );
    assert_eq!(
        screening_at_start(&orch.db, None).unwrap(),
        "screening is off"
    );

    lock_to_screen(&orch, "lock:waiting", "0xe5", LISTED);
    let held = lock_to_screen(&orch, "lock:held", "0xe6", LISTED);
    set_state(&orch, held, "held");

    assert_eq!(
        screening_at_start(&orch.db, None).unwrap(),
        "screening is off, so 2 lock(s) stay where they wait: lock:waiting (screening), lock:held (held)"
    );
}
