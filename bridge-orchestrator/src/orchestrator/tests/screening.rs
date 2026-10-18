use super::*;
use crate::screening::{Action, Answer, Answering, DenyList, Transfer};
use crate::state::{Hold, WorkState};

const LISTED: Address = Address::repeat_byte(0xBA);
const CLEAN: Address = Address::repeat_byte(0x11);

struct Down;

impl std::fmt::Display for Down {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("down")
    }
}

impl Screener for Down {
    fn name(&self) -> &'static str {
        "down"
    }

    fn answer<'a>(&'a self, addresses: &'a [Address]) -> Answering<'a> {
        let answers = addresses
            .iter()
            .map(|address| (*address, Answer::Failed("down".into())))
            .collect();
        Box::pin(std::future::ready(answers))
    }
}

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

fn decide(orch: &BridgeOrchestrator, transfer: Transfer, action: Action) {
    orch.db.act(&transfer, action, "Ana", "checked").unwrap();
}

fn hold_of(orch: &BridgeOrchestrator, withdrawal: &Transaction) -> Option<Hold> {
    orch.db.withdrawal_hold(&withdrawal.id.to_string()).unwrap()
}

fn parked_on_bridging(conductor: &FakeConductor) -> Vec<String> {
    ids(&conductor.parked.borrow()[&action_hash(BR_EA)])
}

fn lines_of(logged: &str, event: &str, transfer: &str) -> usize {
    logged
        .lines()
        .filter(|line| line.contains(event) && line.contains(transfer))
        .count()
}

#[tokio::test]
async fn a_clear_lock_is_queued_and_credited_and_a_listed_one_only_once_a_person_releases_it() {
    let mut orch = test_orchestrator("screening-credit");
    screening_with(&mut orch, DenyList::new([LISTED]));
    assert!(orch.lock_flow(&running()).unwrap().screening);
    let clear = lock_to_screen(&orch, "lock:clear", "0xe1", CLEAN);
    let held = lock_from(&orch, "lock:held", "0xe2", LISTED, CLEAN);
    let sent_by_listed = lock_from(&orch, "lock:sent-by-listed", "0xe7", CLEAN, LISTED);
    let blocked = lock_to_screen(&orch, "lock:blocked", "0xe3", LISTED);

    let logged = logged_at(tracing::Level::ERROR, async {
        screen_locks(&orch).await;
        screen_locks(&orch).await;
    })
    .await;

    assert_eq!(state_of(&orch, clear).0, WorkState::Queued);
    assert_eq!(state_of(&orch, held).0, WorkState::Held);
    assert_eq!(state_of(&orch, sent_by_listed).0, WorkState::Held);
    assert_eq!(
        lines_of(&logged, "bridge.screening.hit", "lock:sent-by-listed"),
        1
    );
    assert!(
        logged.contains(&format!("{LISTED:#x}")) && logged.contains("0xe7"),
        "the line names the listed address and the lock's transaction: {logged}"
    );

    decide(&orch, Transfer::Lock("blocked".into()), Action::Block);
    let conductor = bridging_conductor();
    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();
    assert_eq!(
        state_of(&orch, clear),
        (WorkState::Succeeded, WorkStep::BrRaveExecuted)
    );
    assert_eq!(state_of(&orch, held), (WorkState::Held, WorkStep::New));

    decide(&orch, Transfer::Lock("held".into()), Action::Release);
    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();
    assert_eq!(
        state_of(&orch, held),
        (WorkState::Succeeded, WorkStep::BrRaveExecuted)
    );
    assert_eq!(
        state_of(&orch, blocked),
        (WorkState::Blocked, WorkStep::New)
    );
}

#[tokio::test]
async fn s4_screens_each_withdrawal_it_would_pay_against_the_list_in_force() {
    let orch = test_orchestrator("screening-select");
    let signer = CouponSigner::with_key(PrivateKeySigner::random());
    let held = parked_withdrawal_tx(0xBA);
    let clear = parked_withdrawal_tx(0x11);
    let mut unpayable = parked_withdrawal_tx(0xBB);
    unpayable.amount = UnitMap::from(vec![(1, "5"), (2, "5")]);
    if let TransactionDetails::ParkedSpend {
        attached_payload, ..
    } = &mut unpayable.details
    {
        attached_payload["withdraw_to_address"] = json!(format!("{LISTED:#x}"));
    }
    let select = async |list: &DenyList| {
        select_bridging_links(
            Some((&signer, VAULT)),
            &WithdrawalGate::new(&orch.db, Some(list)),
            &bridging_agent(),
            &[held.clone(), clear.clone(), unpayable.clone()],
            usize::MAX,
            1,
        )
        .await
        .unwrap()
    };

    let selection = select(&DenyList::new([LISTED])).await;

    assert_eq!(selection.held, 1);
    assert_eq!(
        selection.coupons.keys().cloned().collect::<Vec<_>>(),
        ids(std::slice::from_ref(&clear))
    );
    assert_eq!(hold_of(&orch, &held), Some(Hold::Held));
    assert_eq!(
        hold_of(&orch, &clear),
        None,
        "a clear verdict is not stored"
    );
    assert_eq!(unpayable_ids(&selection), ids(&[unpayable.clone()]));
    assert_eq!(
        hold_of(&orch, &unpayable),
        None,
        "a withdrawal no coupon can pay is never screened"
    );

    let after_the_list_changed = select(&DenyList::new([LISTED, CLEAN])).await;

    assert_eq!(after_the_list_changed.held, 2);
    assert!(
        after_the_list_changed.coupons.is_empty(),
        "an address listed after an earlier unsigned cycle is not paid"
    );
    assert_eq!(hold_of(&orch, &clear), Some(Hold::Held));
}

#[tokio::test]
async fn a_held_withdrawal_gets_no_coupon_until_a_person_releases_it_and_never_once_blocked() {
    let mut orch = test_orchestrator("screening-withdrawal");
    let held = parked_withdrawal_tx(0xBA);
    let blocked = parked_withdrawal_tx(0xBB);
    let clear = parked_withdrawal_tx(0x11);
    screening_with(&mut orch, Down);
    let conductor = bridging_conductor().parking(
        action_hash(BR_EA),
        &[held.clone(), blocked.clone(), clear.clone()],
    );

    let logged = logged_at(tracing::Level::WARN, async {
        orch.run_bridge_cycle(&conductor, &running()).await.unwrap();
        orch.run_bridge_cycle(&conductor, &running()).await.unwrap();
        assert_eq!(
            parked_on_bridging(&conductor).len(),
            3,
            "no answer pays nothing"
        );
        screening_with(
            &mut orch,
            DenyList::new([LISTED, Address::repeat_byte(0xBB)]),
        );
        orch.run_bridge_cycle(&conductor, &running()).await.unwrap();
        orch.run_bridge_cycle(&conductor, &running()).await.unwrap();
    })
    .await;

    assert_eq!(
        parked_on_bridging(&conductor),
        ids(&[held.clone(), blocked.clone()])
    );
    assert_eq!(hold_of(&orch, &held), Some(Hold::Held));
    let transfer = format!("withdrawal:{}", held.id);
    assert_eq!(
        lines_of(&logged, "bridge.screening.no_answer", &transfer),
        1
    );
    assert_eq!(
        lines_of(&logged, "bridge.screening.hit", &transfer),
        1,
        "{logged}"
    );

    decide(
        &orch,
        Transfer::Withdrawal(held.id.to_string()),
        Action::Release,
    );
    decide(
        &orch,
        Transfer::Withdrawal(blocked.id.to_string()),
        Action::Block,
    );
    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();
    assert_eq!(
        parked_on_bridging(&conductor),
        ids(std::slice::from_ref(&blocked))
    );
    assert_eq!(hold_of(&orch, &blocked), Some(Hold::Blocked));
}

#[tokio::test]
async fn with_screening_off_nothing_that_waits_moves_and_only_an_unheld_withdrawal_is_paid() {
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
    let blocked = parked_withdrawal_tx(0x13);
    decide(
        &orch,
        Transfer::Withdrawal(blocked.id.to_string()),
        Action::Block,
    );
    orch.ethereum.as_mut().unwrap().screener = None;
    let unheld = parked_withdrawal_tx(0x12);
    let conductor = bridging_conductor().parking(
        action_hash(BR_EA),
        &[held.clone(), blocked.clone(), unheld.clone()],
    );

    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();

    assert_eq!(
        state_of(&orch, waiting),
        (WorkState::Screening, WorkStep::New)
    );
    assert_eq!(parked_on_bridging(&conductor), ids(&[held, blocked]));
    assert_eq!(hold_of(&orch, &unheld), None, "screening off holds nothing");
}
