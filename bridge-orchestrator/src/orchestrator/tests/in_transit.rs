use super::*;
use crate::orchestrator::in_transit::PAID_BY_HAND;

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
    let found = orch.in_transit(conductor).await?;
    Ok(Checked {
        listed: found
            .listed
            .iter()
            .map(|listed| serde_json::to_value(listed).unwrap())
            .collect(),
        exit: found.settle(&orch.db, mark_failed),
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

fn listed_row(lock: &str, step: &str, link: u8) -> Value {
    json!({
        "kind": "row",
        "item_id": lock,
        "lock_id": lock,
        "step": step,
        "link": action_hash(link).to_string(),
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

    let checked = check(&orch, &bridging_conductor(), false).await.unwrap();
    assert_lists(&checked.listed, &[]);
    checked.exit.unwrap();

    enqueue_lock(&orch, "lock:carried-on:1", "0xc1");
    enqueue_at_cl_rave_executed(&orch, "lock:carried-on:2", "0xc2");
    paid(&orch, "lock:carried-on:3", "0xc3");

    let checked = check(&orch, &bridging_conductor(), false).await.unwrap();
    assert_lists(&checked.listed, &[]);
    checked
        .exit
        .expect("the new orchestrator carries on a row that waits on no link");
}

#[tokio::test]
async fn a_row_waiting_on_its_link_is_in_transit_until_it_is_failed() {
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
    ];

    let checked = check(&orch, &bridging_conductor(), false).await.unwrap();
    assert_lists(
        &checked.listed,
        &[
            listed_row("lock:waits:1", "cl_link_created", 0x61),
            listed_row("lock:waits:2", "br_spend_created", 0x62),
        ],
    );
    checked.exit.expect_err("a row in transit");

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
async fn a_live_withdrawal_and_a_link_no_row_records_are_in_transit() {
    let orch = test_orchestrator("in-transit-live");
    let unrecorded = parked_tx(
        0x63,
        &[
            proof("lock:no-row:2", "0xc7"),
            proof("lock:no-row:1", "0xc6"),
        ],
    );
    let conductor = bridging_conductor()
        .parking(action_hash(CL_EA), std::slice::from_ref(&unrecorded))
        .parking(action_hash(BR_EA), &[parked_withdrawal_tx(0x64)]);

    let checked = check(&orch, &conductor, false).await.unwrap();

    assert_lists(
        &checked.listed,
        &[
            listed_link(CL_EA, &unrecorded, &["lock:no-row:1", "lock:no-row:2"]),
            listed_withdrawal(0x64),
        ],
    );
    checked.exit.expect_err("links in transit");
}

#[tokio::test]
async fn a_foreign_spend_in_the_bridging_agents_role_is_not_in_transit() {
    let orch = test_orchestrator("in-transit-foreign");
    let lock = [proof("lock:foreign:1", "0xc8")];
    let conductor = bridging_conductor()
        .parking(
            action_hash(CL_EA),
            &[signed_by_another(parked_tx(0x6D, &lock))],
        )
        .parking(
            action_hash(BR_EA),
            &[signed_by_another(parked_spend_tx(0x65, &lock))],
        );

    let checked = check(&orch, &conductor, false).await.unwrap();

    assert_lists(&checked.listed, &[]);
    checked.exit.unwrap();
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
    let parked = waiting_at(&orch, WorkStep::ClLinkCreated, "lock:mark:1", "0xd1", 0x66);
    let spent = waiting_at(&orch, WorkStep::BrSpendCreated, "lock:mark:2", "0xd2", 0x67);
    let written = enqueue_lock(&orch, "lock:mark:3", "0xd3");
    let paid = paid(&orch, "lock:mark:4", "0xd4");
    let failed = enqueue_lock(&orch, "lock:mark:5", "0xd5");
    orch.db
        .mark_failed_permanent(failed, "resolved by a person")
        .unwrap();
    let carried_on = enqueue_lock(&orch, "lock:mark:6", "0xd6");
    let late = parked_tx(
        0x68,
        &[
            proof("lock:mark:3", "0xd3"),
            proof("lock:mark:4", "0xd4"),
            proof("lock:mark:5", "0xd5"),
        ],
    );
    let conductor = bridging_conductor().parking(action_hash(CL_EA), std::slice::from_ref(&late));
    let before = snapshot(&orch);

    let checked = check(&orch, &conductor, true).await.unwrap();

    assert_lists(
        &checked.listed,
        &[
            listed_row("lock:mark:1", "cl_link_created", 0x66),
            listed_row("lock:mark:2", "br_spend_created", 0x67),
            listed_link(CL_EA, &late, &["lock:mark:3", "lock:mark:4", "lock:mark:5"]),
        ],
    );
    checked.exit.unwrap();
    let after = snapshot(&orch);
    for id in [parked, spent, written] {
        let row = failed_row(&orch, id);
        assert_eq!(row.error_class.as_deref(), Some("permanent"));
        assert_eq!(row.last_error.as_deref(), Some(PAID_BY_HAND));
        assert_eq!(after[&id]["step"], before[&id]["step"]);
    }
    for id in [paid, failed, carried_on] {
        assert_eq!(after[&id], before[&id]);
    }
}

#[tokio::test]
async fn mark_failed_records_a_link_no_row_records_and_a_withdrawal_only_in_its_list() {
    let orch = test_orchestrator("in-transit-mark-unrecorded");
    enqueue_lock(&orch, "lock:unmarked:1", "0xd7");
    paid(&orch, "lock:unmarked:2", "0xd8");
    let unrecorded = parked_spend_tx(0x69, &[proof("lock:no-row:3", "0xd9")]);
    let conductor = bridging_conductor().parking(
        action_hash(BR_EA),
        &[unrecorded.clone(), parked_withdrawal_tx(0x6A)],
    );
    let before = snapshot(&orch);

    let checked = check(&orch, &conductor, true).await.unwrap();

    assert_lists(
        &checked.listed,
        &[
            listed_link(BR_EA, &unrecorded, &["lock:no-row:3"]),
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
        "0xda",
        0x6B,
    );
    let second = waiting_at(
        &orch,
        WorkStep::BrSpendCreated,
        "lock:refused:2",
        "0xdb",
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
}
