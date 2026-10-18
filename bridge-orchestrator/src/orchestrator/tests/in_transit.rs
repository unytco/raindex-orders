use super::*;

const PAID_BY_HAND: &str =
    "in transit at the old network's close; it is paid by hand on the new network";

#[derive(Debug)]
struct Checked {
    listed: Vec<Value>,
    exit: Result<()>,
}

async fn check(
    orch: &BridgeOrchestrator,
    conductor: &FakeConductor,
    mark_failed: bool,
) -> Result<Checked> {
    let found = orch
        .in_transit(conductor, orch.screener(), orch.vault().as_deref())
        .await?;
    let mut printed = Vec::new();
    let exit = found.settle(&mut printed, &orch.db, mark_failed);
    Ok(Checked {
        listed: String::from_utf8(printed)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect(),
        exit,
    })
}

fn assert_lists(listed: &[Value], expected: &[Value]) {
    assert_eq!(listed.len(), expected.len(), "{listed:#?}");
    for entry in expected {
        assert!(listed.contains(entry), "{entry} is not in {listed:#?}");
    }
}

fn snapshot(orch: &BridgeOrchestrator) -> BTreeMap<i64, Value> {
    orch.db
        .list_flow("lock")
        .unwrap()
        .into_iter()
        .map(|row| (row.id, serde_json::to_value(&row).unwrap()))
        .collect()
}

fn without_the_mark(row: &Value) -> Value {
    let mut row = row.clone();
    for field in [
        "state",
        "error_class",
        "last_error",
        "next_retry_at",
        "updated_at",
    ] {
        row.as_object_mut().unwrap().remove(field);
    }
    row
}

fn waiting_at(
    orch: &BridgeOrchestrator,
    step: WorkStep,
    lock: &str,
    tx_hash: &str,
    link: u8,
) -> i64 {
    let link = action_hash(link).to_string();
    match step {
        WorkStep::ClLinkCreated => {
            let id = enqueue_lock(orch, lock, tx_hash);
            orch.db
                .advance_to_cl_link_created(id, &link, &ea(CL_EA))
                .unwrap();
            id
        }
        _ => {
            let id = enqueue_at_cl_rave_executed(orch, lock, tx_hash);
            orch.db
                .advance_to_br_spend_created(id, &link, &ea(BR_EA))
                .unwrap();
            id
        }
    }
}

fn paid(orch: &BridgeOrchestrator, lock: &str, tx_hash: &str) -> i64 {
    let id = enqueue_lock(orch, lock, tx_hash);
    orch.db.advance_to_br_rave_executed(id, None).unwrap();
    id
}

fn spend_as(mut spend: Transaction, role: &str, payload: Value) -> Transaction {
    if let TransactionDetails::ParkedSpend {
        attached_payload,
        ct_role_id,
        ..
    } = &mut spend.details
    {
        *attached_payload = payload;
        *ct_role_id = role.to_string();
    }
    spend
}

fn listed_row(lock: &str, step: &str, link: u8) -> Value {
    listed_row_in("queued", lock, step, Some(link))
}

fn listed_row_in(state: &str, lock: &str, step: &str, link: Option<u8>) -> Value {
    json!({
        "kind": "row",
        "item_id": lock,
        "lock_id": lock,
        "state": state,
        "step": step,
        "link": link.map(|link| action_hash(link).to_string()),
    })
}

fn listed_link(agreement: u8, link: &Transaction, lock_ids: &[&str]) -> Value {
    json!({
        "kind": "deposit_link",
        "agreement": ea(agreement),
        "link": link.id.to_string(),
        "lock_ids": lock_ids,
    })
}

fn listed_withdrawal(seed: u8) -> Value {
    let spend = parked_withdrawal_tx(seed);
    json!({
        "kind": "withdrawal",
        "agreement": ea(BR_EA),
        "spend": spend.id.to_string(),
        "spender": spend.creator.to_string(),
        "amount": spend.amount,
        "withdraw_to_address": format!("{:#x}", Address::repeat_byte(seed)),
    })
}

fn failing_on(agreement: u8) -> FakeConductor {
    FakeConductor {
        fails_on: Some((action_hash(agreement), "Websocket closed")),
        ..bridging_conductor()
    }
}

#[tokio::test]
async fn with_no_pending_row_and_no_live_link_nothing_is_in_transit() {
    let orch = test_orchestrator("in-transit-nothing");
    for mark_failed in [false, true] {
        let checked = check(&orch, &bridging_conductor(), mark_failed)
            .await
            .unwrap();
        assert_lists(&checked.listed, &[]);
        checked.exit.unwrap();
    }

    enqueue_lock(&orch, "lock:carried-on:1", "0xc1");
    enqueue_at_cl_rave_executed(&orch, "lock:carried-on:2", "0xc2");
    paid(&orch, "lock:carried-on:3", "0xc3");
    let before = snapshot(&orch);

    for mark_failed in [false, true] {
        let checked = check(&orch, &bridging_conductor(), mark_failed)
            .await
            .unwrap();
        assert_lists(&checked.listed, &[]);
        checked
            .exit
            .expect("the new orchestrator carries on a row that waits on no link");
    }
    assert_eq!(snapshot(&orch), before);
}

#[tokio::test]
async fn a_row_waiting_on_its_link_is_in_transit_in_any_pending_state_until_it_is_failed() {
    let orch = test_orchestrator("in-transit-rows");
    let rows = [
        waiting_at(&orch, WorkStep::ClLinkCreated, "lock:waits:1", "0xc4", 0x61),
        waiting_at(
            &orch,
            WorkStep::BrSpendCreated,
            "lock:waits:2",
            "0xc5",
            0x62,
        ),
        waiting_at(&orch, WorkStep::ClLinkCreated, "lock:waits:3", "0xc6", 0x63),
        waiting_at(
            &orch,
            WorkStep::BrSpendCreated,
            "lock:waits:4",
            "0xc7",
            0x64,
        ),
    ];
    set_state(&orch, rows[2], "in_flight");
    set_state(&orch, rows[3], "claimed");

    let checked = check(&orch, &bridging_conductor(), false).await.unwrap();
    assert_lists(
        &checked.listed,
        &[
            listed_row("lock:waits:1", "cl_link_created", 0x61),
            listed_row("lock:waits:2", "br_spend_created", 0x62),
            listed_row_in("in_flight", "lock:waits:3", "cl_link_created", Some(0x63)),
            listed_row_in("claimed", "lock:waits:4", "br_spend_created", Some(0x64)),
        ],
    );
    checked.exit.expect_err("rows in transit");

    for id in rows {
        orch.db
            .mark_failed_permanent(id, "resolved by a person")
            .unwrap();
    }
    let checked = check(&orch, &bridging_conductor(), false).await.unwrap();
    assert_lists(&checked.listed, &[]);
    checked.exit.unwrap();
}

#[tokio::test]
async fn a_row_a_write_was_in_flight_for_is_in_transit_at_any_step() {
    let orch = test_orchestrator("in-transit-interrupted");
    let rows = [
        enqueue_lock(&orch, "lock:interrupted:1", "0xee"),
        enqueue_at_cl_rave_executed(&orch, "lock:interrupted:2", "0xef"),
    ];
    set_state(&orch, rows[0], "in_flight");
    set_state(&orch, rows[1], "claimed");
    let expected = [
        listed_row_in("in_flight", "lock:interrupted:1", "new", None),
        listed_row_in("claimed", "lock:interrupted:2", "cl_rave_executed", None),
    ];

    let checked = check(&orch, &bridging_conductor(), false).await.unwrap();
    assert_lists(&checked.listed, &expected);
    checked
        .exit
        .expect_err("a write may have reached the conductor");

    let checked = check(&orch, &bridging_conductor(), true).await.unwrap();
    assert_lists(&checked.listed, &expected);
    checked.exit.unwrap();
    for id in rows {
        assert_eq!(
            failed_row(&orch, id).last_error.as_deref(),
            Some(PAID_BY_HAND)
        );
    }
}

#[tokio::test]
async fn each_live_withdrawal_and_deposit_link_is_in_transit_on_its_own() {
    let orch = test_orchestrator("in-transit-live");
    let unrecorded = parked_tx(
        0x65,
        &[
            proof("lock:no-row:2", "0xc9"),
            proof("lock:no-row:1", "0xc8"),
        ],
    );
    let mut unreadable = parked_tx(0x66, &[]);
    if let TransactionDetails::Parked {
        attached_payload, ..
    } = &mut unreadable.details
    {
        attached_payload["proof_of_deposit"] = json!("not a list");
    }
    let spend = parked_spend_tx(0x67, &[proof("lock:no-row:3", "0xca")]);
    let mut unaddressed = listed_withdrawal(0x6E);
    unaddressed["withdraw_to_address"] = Value::Null;

    for (agreement, parked, expected) in [
        (
            CL_EA,
            vec![unrecorded.clone()],
            vec![listed_link(
                CL_EA,
                &unrecorded,
                &["lock:no-row:1", "lock:no-row:2"],
            )],
        ),
        (
            CL_EA,
            vec![unreadable.clone()],
            vec![listed_link(CL_EA, &unreadable, &[])],
        ),
        (
            BR_EA,
            vec![spend.clone()],
            vec![listed_link(BR_EA, &spend, &["lock:no-row:3"])],
        ),
        (
            BR_EA,
            vec![parked_withdrawal_tx(0x68), parked_withdrawal_tx(0x69)],
            vec![listed_withdrawal(0x68), listed_withdrawal(0x69)],
        ),
        (
            BR_EA,
            vec![spend_as(
                parked_withdrawal_tx(0x6E),
                WITHDRAWER_ROLE,
                json!({ "withdraw_contract_address": format!("{VAULT:#x}") }),
            )],
            vec![unaddressed],
        ),
    ] {
        let conductor = bridging_conductor().parking(action_hash(agreement), &parked);

        let checked = check(&orch, &conductor, false).await.unwrap();

        assert_lists(&checked.listed, &expected);
        checked.exit.expect_err("in transit");
    }

    let conductor = bridging_conductor()
        .parking(action_hash(CL_EA), std::slice::from_ref(&unrecorded))
        .parking(action_hash(BR_EA), &[parked_withdrawal_tx(0x68)]);
    let checked = check(&orch, &conductor, false).await.unwrap();
    assert_lists(
        &checked.listed,
        &[
            listed_link(CL_EA, &unrecorded, &["lock:no-row:1", "lock:no-row:2"]),
            listed_withdrawal(0x68),
        ],
    );
    checked.exit.expect_err("in transit");
}

#[tokio::test]
async fn another_agents_link_and_a_spend_in_another_role_are_not_in_transit() {
    let orch = test_orchestrator("in-transit-passed-by");
    enqueue_lock(&orch, "lock:copied:1", "0xcb");
    enqueue_at_cl_rave_executed(&orch, "lock:copied:2", "0xcc");
    let (credit, bridged) = (
        [proof("lock:copied:1", "0xcb")],
        [proof("lock:copied:2", "0xcc")],
    );
    let conductor = bridging_conductor()
        .parking(
            action_hash(CL_EA),
            &[signed_by_another(parked_tx(0x6A, &credit))],
        )
        .parking(
            action_hash(BR_EA),
            &[
                spend_as(
                    signed_by_another(parked_spend_tx(0x6B, &[])),
                    BRIDGING_AGENT_ROLE,
                    json!({ "proof_of_deposit": bridged, "withdraw_to_address": "0x11" }),
                ),
                spend_as(
                    parked_spend_tx(0x6C, &[]),
                    ORACLE_ROLE,
                    json!({ "proof_of_deposit": bridged }),
                ),
            ],
        );
    let before = snapshot(&orch);

    for mark_failed in [false, true] {
        let checked = check(&orch, &conductor, mark_failed).await.unwrap();
        assert_lists(&checked.listed, &[]);
        checked.exit.unwrap();
    }
    assert_eq!(snapshot(&orch), before, "a copied proof marks no row");
}

#[tokio::test]
async fn a_row_whose_state_or_step_cannot_be_read_fails_the_check() {
    for (column, garbled) in [("state", "Queued"), ("step", "cl_link_create")] {
        let orch = test_orchestrator("in-transit-garbled");
        let lock = enqueue_lock(&orch, "lock:garbled:1", "0xdd");
        let spend = parked_withdrawal_tx(0xDE);
        sent(&orch, &spend);
        let withdrawal = withdrawal_row(&orch, &spend);
        let db = rusqlite::Connection::open(&orch.cfg.db_path).unwrap();
        for id in [lock, withdrawal.id] {
            db.execute(
                &format!("UPDATE work_items SET {column} = ?2 WHERE id = ?1"),
                rusqlite::params![id, garbled],
            )
            .unwrap();
        }
        let before = snapshot(&orch);

        for mark_failed in [false, true] {
            let e = check(&orch, &bridging_conductor(), mark_failed)
                .await
                .expect_err("a row it cannot read");
            for item in ["lock:garbled:1", &withdrawal.item_id] {
                assert!(format!("{e:#}").contains(item), "{e:#}");
            }
        }
        assert_eq!(snapshot(&orch), before);
    }
}

#[tokio::test]
async fn an_agreement_that_cannot_be_read_fails_the_check() {
    let orch = test_orchestrator("in-transit-unreadable");
    for agreement in [CL_EA, BR_EA] {
        let e = check(&orch, &failing_on(agreement), false)
            .await
            .expect_err("a read that fails");
        assert!(format!("{e:#}").contains("Websocket closed"), "{e:#}");
    }
}

#[tokio::test]
async fn mark_failed_fails_each_row_in_transit_and_changes_no_other() {
    let orch = test_orchestrator("in-transit-mark");
    let parked = waiting_at(&orch, WorkStep::ClLinkCreated, "lock:mark:1", "0xd1", 0x70);
    let spent = waiting_at(&orch, WorkStep::BrSpendCreated, "lock:mark:2", "0xd2", 0x71);
    let written = enqueue_lock(&orch, "lock:mark:3", "0xd3");
    set_state(&orch, written, "in_flight");
    let paid = paid(&orch, "lock:mark:4", "0xd4");
    let failed = enqueue_lock(&orch, "lock:mark:5", "0xd5");
    orch.db
        .mark_failed_permanent(failed, "resolved by a person")
        .unwrap();
    let carried_on = enqueue_lock(&orch, "lock:mark:6", "0xd6");
    let spent_late = enqueue_at_cl_rave_executed(&orch, "lock:mark:7", "0xd7");
    let own = parked_tx(0x70, &[proof("lock:mark:1", "0xd1")]);
    let late = parked_tx(
        0x73,
        &[
            proof("lock:mark:3", "0xd3"),
            proof("lock:mark:4", "0xd4"),
            proof("lock:mark:5", "0xd5"),
        ],
    );
    let late_spend = parked_spend_tx(0x72, &[proof("lock:mark:7", "0xd7")]);
    let conductor = bridging_conductor()
        .parking(action_hash(CL_EA), &[own.clone(), late.clone()])
        .parking(action_hash(BR_EA), std::slice::from_ref(&late_spend));
    let before = snapshot(&orch);

    let checked = check(&orch, &conductor, true).await.unwrap();

    let links = [
        listed_link(CL_EA, &own, &["lock:mark:1"]),
        listed_link(CL_EA, &late, &["lock:mark:3", "lock:mark:4", "lock:mark:5"]),
        listed_link(BR_EA, &late_spend, &["lock:mark:7"]),
    ];
    let rows = [
        listed_row("lock:mark:1", "cl_link_created", 0x70),
        listed_row("lock:mark:2", "br_spend_created", 0x71),
        listed_row_in("in_flight", "lock:mark:3", "new", None),
    ];
    assert_lists(&checked.listed, &[&rows[..], &links].concat());
    checked.exit.unwrap();
    let after = snapshot(&orch);
    for id in [parked, spent, written, spent_late] {
        let row = failed_row(&orch, id);
        assert_eq!(row.error_class.as_deref(), Some("permanent"));
        assert_eq!(row.last_error.as_deref(), Some(PAID_BY_HAND));
        assert_eq!(
            without_the_mark(&after[&id]),
            without_the_mark(&before[&id])
        );
    }
    for id in [paid, failed, carried_on] {
        assert_eq!(after[&id], before[&id]);
    }

    let checked = check(&orch, &conductor, false).await.unwrap();
    assert_lists(&checked.listed, &links);
    checked.exit.expect_err("the links stay live");
}

#[tokio::test]
async fn mark_failed_takes_a_row_by_its_own_lock_only() {
    let orch = test_orchestrator("in-transit-mark-lock");
    let sibling = enqueue_lock(&orch, "lock:match:1", "0xe1");
    let own = enqueue_lock(&orch, "lock:match:2", "0xE3AB");
    let link = parked_tx(
        0x74,
        &[
            proof("lock:match:1", "0xe2"),
            proof("lock:match:2", "0xe3ab"),
        ],
    );
    let conductor = bridging_conductor().parking(action_hash(CL_EA), &[link]);
    let before = snapshot(&orch);

    check(&orch, &conductor, true).await.unwrap().exit.unwrap();

    assert_eq!(
        failed_row(&orch, own).last_error.as_deref(),
        Some(PAID_BY_HAND)
    );
    assert_eq!(
        snapshot(&orch)[&sibling],
        before[&sibling],
        "the same lock ID in another transaction is another lock"
    );
}

#[tokio::test]
async fn mark_failed_records_a_link_no_row_records_and_a_withdrawal_only_in_its_list() {
    let orch = test_orchestrator("in-transit-mark-unrecorded");
    enqueue_lock(&orch, "lock:unmarked:1", "0xd8");
    paid(&orch, "lock:unmarked:2", "0xd9");
    let unrecorded = parked_spend_tx(0x69, &[proof("lock:no-row:4", "0xda")]);
    let withdrawal = spend_as(
        parked_withdrawal_tx(0x6A),
        WITHDRAWER_ROLE,
        json!({
            "withdraw_contract_address": format!("{VAULT:#x}"),
            "withdraw_to_address": format!("{:#x}", Address::repeat_byte(0x6A)),
            "proof_of_deposit": [proof("lock:unmarked:1", "0xd8")],
        }),
    );
    let conductor =
        bridging_conductor().parking(action_hash(BR_EA), &[unrecorded.clone(), withdrawal]);
    let before = snapshot(&orch);

    let checked = check(&orch, &conductor, true).await.unwrap();

    assert_lists(
        &checked.listed,
        &[
            listed_link(BR_EA, &unrecorded, &["lock:no-row:4"]),
            listed_withdrawal(0x6A),
        ],
    );
    checked.exit.unwrap();
    assert_eq!(snapshot(&orch), before);
}

#[tokio::test]
async fn mark_failed_changes_no_row_when_a_read_or_the_second_write_fails() {
    let orch = test_orchestrator("in-transit-mark-refused");
    waiting_at(
        &orch,
        WorkStep::ClLinkCreated,
        "lock:refused:1",
        "0xdb",
        0x6B,
    );
    let second = waiting_at(
        &orch,
        WorkStep::BrSpendCreated,
        "lock:refused:2",
        "0xdc",
        0x6C,
    );
    let before = snapshot(&orch);

    for agreement in [CL_EA, BR_EA] {
        check(&orch, &failing_on(agreement), true)
            .await
            .expect_err("a read that fails");
        assert_eq!(snapshot(&orch), before);
    }

    rusqlite::Connection::open(&orch.cfg.db_path)
        .unwrap()
        .execute_batch(&format!(
            "CREATE TRIGGER refuse BEFORE UPDATE ON work_items WHEN OLD.id = {second}
             BEGIN SELECT RAISE(FAIL, 'database or disk is full'); END;"
        ))
        .unwrap();
    let checked = check(&orch, &bridging_conductor(), true).await.unwrap();

    let e = checked.exit.expect_err("the second write is refused");
    assert!(
        format!("{e:#}").contains("database or disk is full"),
        "{e:#}"
    );
    assert_eq!(snapshot(&orch), before, "the first write is undone with it");
    assert_lists(
        &checked.listed,
        &[
            listed_row("lock:refused:1", "cl_link_created", 0x6B),
            listed_row("lock:refused:2", "br_spend_created", 0x6C),
        ],
    );
}

fn with_proofs(mut link: Transaction, proofs: Value) -> Transaction {
    if let TransactionDetails::Parked {
        attached_payload, ..
    } = &mut link.details
    {
        attached_payload["proof_of_deposit"] = proofs;
    }
    link
}

#[tokio::test]
async fn mark_failed_marks_no_row_when_a_link_may_carry_a_row_it_cannot_match() {
    let readable = proof("lock:no-row:5", "0xe4");
    let cases = [
        (
            json!([readable, { "lock_id": "lock:blind:1" }]),
            None,
            "which names no lock but may be that of lock:blind:1",
        ),
        (
            json!([readable, { "tx_hash": "0xE5" }]),
            None,
            "which names no lock but may be that of lock:blind:1",
        ),
        (
            json!([readable]),
            Some("{}"),
            "row lock:blind:1 has a lock that cannot be read",
        ),
    ];
    for (proofs, payload, why) in cases {
        let orch = test_orchestrator("in-transit-mark-blind");
        let blind = enqueue_lock(&orch, "lock:blind:1", "0xe5");
        waiting_at(&orch, WorkStep::ClLinkCreated, "lock:blind:2", "0xe6", 0x78);
        if let Some(payload) = payload {
            rusqlite::Connection::open(&orch.cfg.db_path)
                .unwrap()
                .execute(
                    "UPDATE work_items SET payload_json = ?2 WHERE id = ?1",
                    rusqlite::params![blind, payload],
                )
                .unwrap();
        }
        let link = with_proofs(parked_tx(0x76, &[]), proofs);
        let conductor =
            bridging_conductor().parking(action_hash(CL_EA), std::slice::from_ref(&link));
        let before = snapshot(&orch);

        let checked = check(&orch, &conductor, true).await.unwrap();

        let e = format!("{:#}", checked.exit.expect_err("it cannot tell"));
        assert!(e.contains(why), "{e}");
        assert_eq!(snapshot(&orch), before);
        assert_lists(
            &checked.listed,
            &[
                listed_row("lock:blind:2", "cl_link_created", 0x78),
                listed_link(CL_EA, &link, &["lock:no-row:5"]),
            ],
        );
        let e = check(&orch, &conductor, false).await.unwrap().exit;
        let e = format!("{:#}", e.expect_err("in transit"));
        assert!(e.contains("--mark-failed would mark no row"), "{e}");
        assert!(e.contains(why), "{e}");
    }
}

#[tokio::test]
async fn a_proof_that_can_be_no_unmarked_rows_lock_stops_no_mark() {
    let orch = test_orchestrator("in-transit-mark-nameless");
    let carried = enqueue_lock(&orch, "lock:named:1", "0xea");
    let clear = enqueue_lock(&orch, "lock:named:2", "0xeb");
    let waiting = waiting_at(&orch, WorkStep::ClLinkCreated, "lock:named:3", "0xec", 0x7D);
    let carried_later = enqueue_lock(&orch, "lock:named:4", "0xed");
    let unlisted = with_proofs(parked_tx(0x7B, &[]), json!("not a list"));
    let partly = with_proofs(
        parked_tx(0x7C, &[]),
        json!([
            proof("lock:named:1", "0xea"),
            {},
            { "lock_id": "lock:elsewhere" },
            { "lock_id": "lock:named:3" },
            { "tx_hash": "0xED" },
        ]),
    );
    let later = parked_tx(0x7E, &[proof("lock:named:4", "0xed")]);
    let conductor = bridging_conductor().parking(
        action_hash(CL_EA),
        &[unlisted.clone(), partly.clone(), later.clone()],
    );
    let before = snapshot(&orch);

    let checked = check(&orch, &conductor, true).await.unwrap();

    checked.exit.unwrap();
    assert_lists(
        &checked.listed,
        &[
            listed_row("lock:named:3", "cl_link_created", 0x7D),
            listed_link(CL_EA, &unlisted, &[]),
            listed_link(CL_EA, &partly, &["lock:named:1"]),
            listed_link(CL_EA, &later, &["lock:named:4"]),
        ],
    );
    for id in [carried, waiting, carried_later] {
        assert_eq!(
            failed_row(&orch, id).last_error.as_deref(),
            Some(PAID_BY_HAND)
        );
    }
    assert_eq!(snapshot(&orch)[&clear], before[&clear]);
}

#[tokio::test]
async fn mark_failed_marks_no_row_when_a_row_it_would_mark_is_gone() {
    let orch = test_orchestrator("in-transit-mark-gone");
    waiting_at(&orch, WorkStep::ClLinkCreated, "lock:gone:1", "0xe7", 0x79);
    let gone = waiting_at(&orch, WorkStep::BrSpendCreated, "lock:gone:2", "0xe8", 0x7A);
    let found = orch
        .in_transit(&bridging_conductor(), None, None)
        .await
        .unwrap();
    delete_rows(&orch, &[gone]);
    let before = snapshot(&orch);

    let e = found
        .settle(&mut Vec::new(), &orch.db, true)
        .expect_err("a row it would mark is gone");

    assert!(
        format!("{e:#}").contains("no longer in the database"),
        "{e:#}"
    );
    assert_eq!(snapshot(&orch), before);
}

#[tokio::test]
async fn mark_failed_marks_no_row_when_a_row_changed_since_it_was_read() {
    for change in ["succeeded", "failed", "next step", "in flight"] {
        let orch = test_orchestrator("in-transit-mark-changed");
        waiting_at(
            &orch,
            WorkStep::BrSpendCreated,
            "lock:changed:1",
            "0xf1",
            0x7F,
        );
        let late = waiting_at(
            &orch,
            WorkStep::ClLinkCreated,
            "lock:changed:2",
            "0xf2",
            0x80,
        );
        let found = orch
            .in_transit(&bridging_conductor(), None, None)
            .await
            .unwrap();
        match change {
            "succeeded" => orch.db.advance_to_br_rave_executed(late, None),
            "failed" => orch.db.mark_failed_permanent(late, "resolved by a person"),
            "next step" => orch.db.advance_to_cl_rave_executed(late, None),
            _ => orch.db.mark_in_flight(late),
        }
        .unwrap();
        let before = snapshot(&orch);

        let e = found
            .settle(&mut Vec::new(), &orch.db, true)
            .expect_err("a row changed since it was read");

        assert!(
            format!("{e:#}").contains("lock:changed:2 is no longer queued at cl_link_created"),
            "{change}: {e:#}"
        );
        assert_eq!(snapshot(&orch), before, "{change}: nothing is marked");
    }
}

#[tokio::test]
async fn in_transit_marks_each_withdrawal_s4_would_not_pay_so_it_is_not_paid_by_hand() {
    use crate::screening::{Action, DenyList, Transfer};
    let mut orch = test_orchestrator("in-transit-screened");
    let [held, blocked, clear, listed] = [0x6A, 0x6B, 0x6C, 0x6D].map(parked_withdrawal_tx);
    orch.db
        .hold_withdrawal(&held.id.to_string(), &[Address::repeat_byte(0x6A)])
        .unwrap();
    orch.db
        .act(
            &Transfer::Withdrawal(blocked.id.to_string()),
            Action::Block,
            "Ana",
            "listed",
        )
        .unwrap();
    let mistyped_to = format!(" {:#x}", Address::repeat_byte(0x70));
    let mistyped = with_payload(
        parked_withdrawal_tx(0x70),
        json!({
            "withdraw_contract_address": format!("{VAULT:#x}"),
            "withdraw_to_address": mistyped_to,
        }),
    );
    let other_vault = with_payload(
        parked_withdrawal_tx(0x71),
        json!({
            "withdraw_contract_address": "0x2222222222222222222222222222222222222222",
            "withdraw_to_address": format!("{:#x}", Address::repeat_byte(0x71)),
        }),
    );
    let bad_checksum_to = "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAeD";
    let bad_checksum = with_payload(
        parked_withdrawal_tx(0x72),
        json!({
            "withdraw_contract_address": format!("{VAULT:#x}"),
            "withdraw_to_address": bad_checksum_to,
        }),
    );
    let refused_amounts = [
        (0x73, vec![(1, "5"), (2, "5")], "the amount holds unit 2"),
        (0x74, vec![(1, "0")], "the amount is zero"),
        (
            0x75,
            vec![(1, "-5")],
            "the amount \"-5\" is not a HOT amount",
        ),
        (0x76, vec![], "the amount \"\" is not a HOT amount"),
    ]
    .map(|(seed, amount, why)| {
        let mut spend = parked_withdrawal_tx(seed);
        spend.amount = UnitMap::from(amount);
        (seed, spend, why)
    });
    let listed_hit = format!("withdrawal:{}", listed.id);
    let parked: Vec<Transaction> = [
        held,
        blocked,
        clear,
        listed,
        mistyped,
        other_vault,
        bad_checksum,
    ]
    .into_iter()
    .chain(refused_amounts.iter().map(|(_, spend, _)| spend.clone()))
    .collect();
    let conductor = bridging_conductor().parking(action_hash(BR_EA), &parked);
    let expected = |marks: [Option<&str>; 7]| {
        let mut expected = [0x6A, 0x6B, 0x6C, 0x6D, 0x70, 0x71, 0x72].map(listed_withdrawal);
        expected[4]["withdraw_to_address"] = json!(mistyped_to);
        expected[4]["unpayable"] = json!(format!(
            "the recipient {mistyped_to:?} is not an address: odd number of digits"
        ));
        expected[6]["withdraw_to_address"] = json!(bad_checksum_to);
        expected[6]["unpayable"] = json!(format!(
            "the recipient {bad_checksum_to:?} fails its EIP-55 checksum"
        ));
        expected[5]["stranded"] = json!(format!(
            "names withdraw_contract_address \"0x2222222222222222222222222222222222222222\", not vault {VAULT:#x}"
        ));
        for (listed, mark) in expected.iter_mut().zip(marks) {
            if let Some(mark) = mark {
                listed["screening"] = json!(mark);
            }
        }
        let refused = refused_amounts.iter().map(|(seed, spend, why)| {
            let mut listed = listed_withdrawal(*seed);
            listed["amount"] = json!(spend.amount);
            listed["unpayable"] = json!(why);
            listed["screening"] = json!("unscreened");
            listed
        });
        expected.into_iter().chain(refused).collect::<Vec<_>>()
    };

    let off = check(&orch, &conductor, false).await.unwrap();
    assert_lists(
        &off.listed,
        &expected([
            Some("held"),
            Some("blocked"),
            None,
            None,
            Some("unscreened"),
            None,
            Some("unscreened"),
        ]),
    );

    let denied = [0x6D, 0x71, 0x73, 0x74, 0x75, 0x76].map(Address::repeat_byte);
    orch.ethereum.as_mut().unwrap().screener = Some(Box::new(DenyList::new(
        denied.into_iter().chain([bad_checksum_to.parse().unwrap()]),
    )));
    let logged = logged_at(tracing::Level::ERROR, async {
        let on = check(&orch, &conductor, false).await.unwrap();
        assert_lists(
            &on.listed,
            &expected([
                Some("held"),
                Some("blocked"),
                None,
                Some("held"),
                Some("unscreened"),
                Some("held"),
                Some("unscreened"),
            ]),
        );
    })
    .await;
    assert_eq!(
        logged
            .lines()
            .filter(|line| line.contains("bridge.screening.hit") && line.contains(&listed_hit))
            .count(),
        1,
        "{logged}"
    );
}

#[tokio::test]
async fn a_withdrawal_row_is_in_transit_until_its_hot_is_off_the_credit_limit() {
    let orch = test_orchestrator("in-transit-withdrawal-rows");
    let spends = [0x50, 0x51, 0x52, 0x53, 0x54, 0x55].map(parked_withdrawal_tx);
    for spend in &spends {
        sent(&orch, spend);
    }
    let [sent_, paid, collected, linked, succeeded, failed] =
        spends.each_ref().map(|spend| withdrawal_row(&orch, spend));
    let rave = action_hash(0x40).to_string();
    for row in [&paid, &collected] {
        orch.db.advance_to_paid(row.id, &rave).unwrap();
    }
    orch.db
        .advance_to_collected(collected.id, &action_hash(0x41).to_string())
        .unwrap();
    let link = parked_tx(
        0x42,
        &[json!({ "method": "withdraw", "spend": spends[3].id.to_string() })],
    );
    for row in [&linked, &succeeded] {
        orch.db
            .advance_to_cl_link_created(row.id, &link.id.to_string(), &ea(CL_EA))
            .unwrap();
    }
    orch.db
        .advance_to_cl_rave_executed(succeeded.id, None)
        .unwrap();
    orch.db
        .mark_failed_permanent(failed.id, "resolved by a person")
        .unwrap();
    let lock = enqueue_lock(&orch, "lock:beside-withdrawals:1", "0xe0");
    set_state(&orch, lock, "in_flight");
    let conductor = bridging_conductor().parking(action_hash(CL_EA), std::slice::from_ref(&link));
    let listed = |row: &WorkItem, step: &str, link: Option<&Transaction>| {
        json!({
            "kind": "row",
            "item_id": row.item_id,
            "lock_id": null,
            "state": "queued",
            "step": step,
            "link": link.map(|link| link.id.to_string()),
        })
    };
    let expected = [
        listed(&sent_, "sent", None),
        listed(&paid, "paid", None),
        listed(&collected, "collected", None),
        listed(&linked, "cl_link_created", Some(&link)),
        listed_link(CL_EA, &link, &[]),
        listed_row_in("in_flight", "lock:beside-withdrawals:1", "new", None),
    ];

    let checked = check(&orch, &conductor, false).await.unwrap();
    assert_lists(&checked.listed, &expected);
    checked
        .exit
        .expect_err("their HOT is not off the credit limit");

    let checked = check(&orch, &conductor, true).await.unwrap();
    assert_lists(&checked.listed, &expected);
    checked.exit.unwrap();
    let by_step = [
        "in transit at the old network's close; if a RAVE took its spend, its HOT is taken off the credit limit by hand only together with collecting what its RAVE allocated, or neither is done",
        "in transit at the old network's close; its HOT is taken off the credit limit by hand only together with collecting what its RAVE allocated, or neither is done",
        "in transit at the old network's close; its HOT is taken off the credit limit by hand",
        "in transit at the old network's close; if S2's RAVE did not take its link, its HOT is taken off the credit limit by hand",
    ];
    for (spend, last_error) in spends[..4].iter().zip(by_step) {
        let row = withdrawal_row(&orch, spend);
        assert_eq!(
            (row.state, row.last_error.as_deref()),
            (WorkState::Failed, Some(last_error)),
            "{}",
            row.item_id
        );
    }
    assert_eq!(
        failed_row(&orch, lock).last_error.as_deref(),
        Some(PAID_BY_HAND)
    );
    assert_eq!(
        withdrawal_row(&orch, &spends[4]).state,
        WorkState::Succeeded
    );
    assert_eq!(
        withdrawal_row(&orch, &spends[5]).last_error.as_deref(),
        Some("resolved by a person")
    );
}
