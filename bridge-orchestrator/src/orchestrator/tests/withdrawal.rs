use super::*;
use crate::orchestrator::withdrawal::refused;

fn hot_withdrawal(seed: u8) -> Transaction {
    let mut withdrawal = parked_withdrawal_tx(seed);
    withdrawal.amount = UnitMap::from(vec![(1, "7.25")]);
    withdrawal
}

fn withdrawal_rows(orch: &BridgeOrchestrator) -> Vec<WorkItem> {
    orch.db.list_flow(WITHDRAWAL_FLOW).unwrap()
}

/// Each `withdraw` proof the bridge parked, with the link's role and amount.
fn withdraw_proofs(conductor: &FakeConductor) -> Vec<(String, Option<UnitMap>, Value)> {
    conductor
        .parked_writes
        .borrow()
        .iter()
        .filter_map(|input| match &input.parked_link_type {
            ParkedLinkType::ParkedData((data, _)) => Some(data.clone()),
            _ => None,
        })
        .flat_map(|data| {
            proofs_in(&data.payload)
                .into_iter()
                .filter(|proof| proof["method"] == WITHDRAW_METHOD)
                .map(move |proof| (data.ct_role_id.clone(), data.amount.clone(), proof))
        })
        .collect()
}

fn calls_to(conductor: &FakeConductor, name: &str) -> usize {
    conductor
        .calls
        .borrow()
        .iter()
        .filter(|call| **call == name)
        .count()
}

fn restarted(orch: &BridgeOrchestrator) -> BridgeOrchestrator {
    BridgeOrchestrator::new(
        test_config(orch.cfg.db_path.clone()),
        Some(EthereumSide {
            chain: Ethereum {
                network: Network::BaseSepolia,
                rpc_url: "http://localhost:0".to_string(),
                lock_vault_address: VAULT,
                confirmations: 5,
            },
            signer: CouponSigner::with_key(PrivateKeySigner::random()),
            screener: None,
        }),
    )
    .unwrap()
}

/// Runs `cycles` cycles, failing each that errs as the cycle loop does.
async fn cycles(orch: &BridgeOrchestrator, conductor: &FakeConductor, cycles: usize) {
    for _ in 0..cycles {
        if let Err(e) = orch.run_bridge_cycle(conductor, &running()).await {
            orch.cycle_failed(&e, &running());
        }
    }
}

#[tokio::test]
async fn a_paid_withdrawal_is_receipted_on_its_rave_and_taken_off_the_credit_limit_once() {
    let orch = test_orchestrator("withdrawal-reduced");
    let withdrawal = hot_withdrawal(0x63);
    let conductor =
        bridging_conductor().parking(action_hash(BR_EA), std::slice::from_ref(&withdrawal));

    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();

    let s4 = action_hash(0xC0);
    assert_eq!(conductor.receipts(), [(s4.clone(), action_hash(0xC1))]);
    let [row] = &withdrawal_rows(&orch)[..] else {
        panic!("one row for the one withdrawal");
    };
    assert_eq!(
        (
            row.step.clone(),
            row.br_rave_hash.clone(),
            row.receipt_hash.clone()
        ),
        (
            WorkStep::Collected,
            Some(s4.to_string()),
            Some(action_hash(0xC1).to_string())
        )
    );
    assert!(
        withdraw_proofs(&conductor).is_empty(),
        "the next S1 parks it"
    );

    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();

    let proof = json!({
        "method": "withdraw",
        "contract_address": format!("{VAULT:#x}"),
        "amount": "7.25",
        "depositor_wallet_address": withdrawal.creator.to_string(),
        "spend": withdrawal.id.to_string(),
    });
    assert_eq!(
        withdraw_proofs(&conductor),
        [(
            ORACLE_ROLE.to_string(),
            Some(UnitMap::from(vec![(1, "7.25")])),
            proof
        )]
    );
    assert_eq!(
        conductor.executor_inputs.borrow()[&action_hash(CL_EA)],
        json!({ "call_method": "bridge" })
    );
    assert!(conductor.parked.borrow()[&action_hash(CL_EA)].is_empty());
    let row = &withdrawal_rows(&orch)[0];
    assert_eq!(
        (row.state.clone(), row.step.clone()),
        (WorkState::Succeeded, WorkStep::ClRaveExecuted)
    );

    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();
    restarted(&orch)
        .run_bridge_cycle(&conductor, &running())
        .await
        .unwrap();

    assert_eq!(calls_to(&conductor, "create_receipt_from_rave"), 1);
    assert_eq!(withdraw_proofs(&conductor).len(), 1);
    assert_eq!(withdrawal_rows(&orch).len(), 1);
}

#[tokio::test]
async fn a_reply_lost_after_any_withdrawal_write_resumes_at_its_step_and_writes_nothing_twice() {
    // The writes in order: S4's RAVE, S5's receipt, S1's link, S2's RAVE.
    for write in 0..4 {
        let orch = test_orchestrator("withdrawal-reply-lost");
        let withdrawal = hot_withdrawal(0x64);
        let conductor = FakeConductor {
            loses_reply_to: Some(write),
            ..bridging_conductor().parking(action_hash(BR_EA), std::slice::from_ref(&withdrawal))
        };

        let mut lost = false;
        for _ in 0..2 {
            if orch.run_bridge_cycle(&conductor, &running()).await.is_err() {
                lost = true;
                break;
            }
        }
        assert!(lost, "write {write}'s reply is lost");
        cycles(&restarted(&orch), &conductor, 3).await;

        let row = &withdrawal_rows(&orch)[..];
        assert_eq!(
            row.iter()
                .map(|row| (row.state.clone(), row.step.clone()))
                .collect::<Vec<_>>(),
            [(WorkState::Succeeded, WorkStep::ClRaveExecuted)],
            "write {write}"
        );
        assert_eq!(conductor.receipts().len(), 1, "write {write}");
        assert_eq!(
            calls_to(&conductor, "create_receipt_from_rave"),
            1,
            "write {write}"
        );
        assert_eq!(withdraw_proofs(&conductor).len(), 1, "write {write}");
    }
}

#[tokio::test]
async fn a_withdrawal_an_earlier_s4_took_after_its_reply_was_lost_is_paid_once_on_that_rave() {
    let orch = test_orchestrator("withdrawal-lands-late");
    let withdrawal = hot_withdrawal(0x6C);
    let conductor = FakeConductor {
        lands_late: Some(0),
        seqs: HashMap::from([(withdrawal.id.clone().into(), 1000)]),
        ..bridging_conductor().parking(action_hash(BR_EA), std::slice::from_ref(&withdrawal))
    };

    cycles(&orch, &conductor, 4).await;

    let late = action_hash(0xC0);
    assert!(
        conductor.chain.borrow().iter().any(
            |chained| matches!(chained, Chained::Rave(rave, consumed) if *rave == action_hash(0xC1) && consumed.is_empty())
        ),
        "the second S4 was handed the spend the first took as it started"
    );
    assert_eq!(
        conductor
            .receipts()
            .into_iter()
            .map(|(rave, _)| rave)
            .collect::<Vec<_>>(),
        std::slice::from_ref(&late)
    );
    assert_eq!(withdraw_proofs(&conductor).len(), 1);
    let row = &withdrawal_rows(&orch)[0];
    assert_eq!(
        (row.state.clone(), row.br_rave_hash.clone()),
        (WorkState::Succeeded, Some(late.to_string()))
    );
}

#[tokio::test]
async fn a_withdrawal_s4_leaves_parked_is_neither_receipted_nor_taken_off_the_credit_limit() {
    let orch = test_orchestrator("withdrawal-left");
    let withdrawal = hot_withdrawal(0x65);
    let mut negative = parked_withdrawal_tx(0x66);
    negative.amount = UnitMap::from(vec![(1, "-5")]);
    let parked = [withdrawal.clone(), negative];
    let conductor = leaving(
        bridging_conductor().parking(action_hash(BR_EA), &parked),
        &[&withdrawal],
    );

    cycles(&orch, &conductor, 2).await;

    assert_eq!(calls_to(&conductor, "execute_rave"), 2);
    assert!(conductor.receipts().is_empty());
    assert!(withdraw_proofs(&conductor).is_empty());
    let rows = withdrawal_rows(&orch);
    assert_eq!(
        rows.iter()
            .map(|row| (row.item_id.clone(), row.step.clone()))
            .collect::<Vec<_>>(),
        [(format!("withdrawal:{}", withdrawal.id), WorkStep::Sent)],
        "one S4 left waits to be sent again, and one no coupon pays has no row"
    );

    let taking = bridging_conductor().parking(action_hash(BR_EA), &parked);
    cycles(&orch, &taking, 2).await;

    assert_eq!(taking.receipts().len(), 1);
    assert_eq!(withdraw_proofs(&taking).len(), 1);
    assert_eq!(withdrawal_rows(&orch).len(), 1);
}

#[test]
fn s4_records_its_withdrawal_paid_however_many_older_rows_wait_at_sent() {
    let orch = test_orchestrator("withdrawal-behind-sent-rows");
    rusqlite::Connection::open(&orch.cfg.db_path)
        .unwrap()
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5000)
             INSERT INTO work_items
                 (flow, task_type, item_id, idempotency_key, payload_json, state, step)
             SELECT ?1, 'reduce_credit_limit', 'withdrawal:older' || i,
                    'withdrawal:older' || i, '{}', 'queued', 'sent'
             FROM n",
            [WITHDRAWAL_FLOW],
        )
        .unwrap();
    let withdrawal = hot_withdrawal(0x6D);
    sent(&orch, &withdrawal);
    let rave = action_hash(0x6E).to_string();

    let paid = orch
        .record_paid(
            &RaveOutcome {
                taken: vec![withdrawal.clone()],
                left: vec![],
            },
            &HashSet::from([withdrawal.id.to_string()]),
            &rave,
        )
        .unwrap();

    assert_eq!(paid, 1);
    let row = orch
        .db
        .withdrawal(&withdrawal.id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!((row.step, row.br_rave_hash), (WorkStep::Paid, Some(rave)));
}

#[tokio::test]
async fn a_sent_withdrawal_whose_spend_left_untaken_or_whose_rave_is_not_held_goes_to_a_person() {
    let orch = test_orchestrator("withdrawal-unresolvable");
    let [left, parked, paid, garbled] = [0x67, 0x68, 0x69, 0x6A].map(parked_withdrawal_tx);
    for spend in [&left, &parked, &paid, &garbled] {
        sent(&orch, spend);
    }
    rusqlite::Connection::open(&orch.cfg.db_path)
        .unwrap()
        .execute(
            "UPDATE work_items SET payload_json = '{}' WHERE item_id = ?1",
            [format!("withdrawal:{}", garbled.id)],
        )
        .unwrap();
    let row_of = |spend: &Transaction| orch.db.withdrawal(&spend.id.to_string()).unwrap().unwrap();
    orch.db
        .advance_to_paid(row_of(&paid).id, &action_hash(0x6B).to_string())
        .unwrap();
    let conductor = bridging_conductor().parking(action_hash(BR_EA), std::slice::from_ref(&parked));

    let parked_again =
        || bridging_conductor().parking(action_hash(BR_EA), &[left.clone(), parked.clone()]);
    let waits = |why: &str| {
        let row = row_of(&left);
        assert_eq!(
            (row.state, row.step),
            (WorkState::Queued, WorkStep::Sent),
            "{why}"
        );
    };

    reconcile_in_force(&orch, &conductor, CL_EA, BR_EA).await;
    waits("one read that misses a spend fails no row");
    reconcile_in_force(&orch, &parked_again(), CL_EA, BR_EA).await;
    reconcile_in_force(&orch, &conductor, CL_EA, BR_EA).await;
    waits("a read that finds the spend parked starts the wait again");
    reconcile_in_force(&orch, &conductor, CL_EA, BR_EA).await;

    let failed = |spend: &Transaction| {
        let row = row_of(spend);
        assert_eq!(row.state, WorkState::Failed, "{}", row.item_id);
        row.last_error.unwrap()
    };
    assert!(failed(&left).contains("no RAVE"));
    assert!(failed(&paid).contains("is not in the bridging agent's chain history"));
    assert!(failed(&garbled).contains("its payload does not read"));
    let waiting = row_of(&parked);
    assert_eq!(
        (waiting.state, waiting.step),
        (WorkState::Queued, WorkStep::Sent)
    );

    let still_parked = parked_again();
    orch.run_bridge_cycle(&still_parked, &running())
        .await
        .unwrap();

    let coupons = &still_parked.executor_inputs.borrow()[&action_hash(BR_EA)]["coupons"];
    assert_eq!(
        coupons.as_object().unwrap().keys().collect::<Vec<_>>(),
        [&parked.id.to_string()],
        "no coupon pays a withdrawal whose row is failed"
    );
    assert_eq!(row_of(&left).state, WorkState::Failed);
    assert_eq!(row_of(&parked).step, WorkStep::Collected);
}

#[tokio::test]
async fn a_paid_withdrawal_whose_receipt_cannot_be_read_back_is_collected_only_once_it_can() {
    let orch = test_orchestrator("withdrawal-receipt-unread");
    let withdrawal = parked_withdrawal_tx(0x6B);
    sent(&orch, &withdrawal);
    let id = withdrawal_rows(&orch)[0].id;
    orch.db
        .advance_to_paid(id, &action_hash(0xE8).to_string())
        .unwrap();
    let chain = || {
        bridging_conductor()
            .taken(std::slice::from_ref(&withdrawal))
            .taken(&[])
            .taken(&[])
    };
    let unreadable = FakeConductor {
        chain_fails_below: Some(FIRST_RAVE_SEQ + 2),
        ..chain()
    };

    orch.run_bridge_cycle(&unreadable, &running())
        .await
        .unwrap();

    assert_eq!(calls_to(&unreadable, "create_receipt_from_rave"), 0);
    assert_eq!(withdrawal_rows(&orch)[0].step, WorkStep::Paid);

    let readable = chain();
    orch.run_bridge_cycle(&readable, &running()).await.unwrap();

    assert_eq!(
        readable.receipts(),
        [(action_hash(0xE8), action_hash(0xC0))]
    );
    assert_eq!(withdrawal_rows(&orch)[0].step, WorkStep::Collected);
}

#[tokio::test]
async fn one_receipt_collects_each_withdrawal_its_rave_took_and_one_link_takes_them_off() {
    let orch = test_orchestrator("withdrawals-one-rave");
    let [first, second, left] = [0x5A, 0x5B, 0x5C].map(hot_withdrawal);
    let conductor = leaving(
        bridging_conductor().parking(
            action_hash(BR_EA),
            &[first.clone(), second.clone(), left.clone()],
        ),
        &[&left],
    );

    cycles(&orch, &conductor, 2).await;

    assert_eq!(
        conductor.receipts(),
        [(action_hash(0xC0), action_hash(0xC1))]
    );
    let row = |spend: &Transaction| orch.db.withdrawal(&spend.id.to_string()).unwrap().unwrap();
    for paid in [&first, &second] {
        let row = row(paid);
        assert_eq!(
            (row.state, row.receipt_hash),
            (WorkState::Succeeded, Some(action_hash(0xC1).to_string()))
        );
    }
    assert_eq!(row(&left).step, WorkStep::Sent);
    assert_eq!(conductor.parked_writes.borrow().len(), 1);
    let proofs = withdraw_proofs(&conductor);
    assert_eq!(
        proofs
            .iter()
            .map(|(_, amount, proof)| (amount.clone(), proof["spend"].clone()))
            .collect::<Vec<_>>(),
        [first.id.to_string(), second.id.to_string()]
            .map(|spend| (Some(UnitMap::from(vec![(1, "14.5")])), json!(spend)))
    );
}

#[tokio::test]
async fn a_refused_receipt_holds_back_only_its_own_rave_and_a_forked_withdrawer_fails_its_rave_at_once(
) {
    let orch = test_orchestrator("withdrawal-receipt-refused");
    let [refused, forked, receipted] = [0x5D, 0x5E, 0x5F].map(hot_withdrawal);
    let conductor = FakeConductor {
        refuses_receipts: HashMap::from([
            (action_hash(0xE8), INVALID_COMMIT),
            (action_hash(0xE9), CHAIN_FORKED),
        ]),
        ..bridging_conductor()
            .taken(std::slice::from_ref(&refused))
            .taken(std::slice::from_ref(&forked))
            .taken(std::slice::from_ref(&receipted))
    };
    let row = |spend: &Transaction| orch.db.withdrawal(&spend.id.to_string()).unwrap().unwrap();
    for (spend, rave) in [(&refused, 0xE8), (&forked, 0xE9), (&receipted, 0xEA)] {
        sent(&orch, spend);
        orch.db
            .advance_to_paid(row(spend).id, &action_hash(rave).to_string())
            .unwrap();
    }

    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();

    assert_eq!(
        conductor.receipts(),
        [(action_hash(0xEA), action_hash(0xC0))]
    );
    assert_eq!(row(&receipted).step, WorkStep::Collected);
    let waiting = row(&refused);
    assert_eq!(
        (&waiting.state, &waiting.step, waiting.attempts),
        (&WorkState::Queued, &WorkStep::Paid, 1)
    );
    assert!(waiting
        .last_error
        .as_deref()
        .unwrap()
        .contains("The accept is already used"));
    let failed = row(&forked);
    assert_eq!(
        (failed.state, failed.step),
        (WorkState::Failed, WorkStep::Paid)
    );
    let why = failed.last_error.unwrap();
    assert!(why.contains("carries a ChainFork warrant"), "{why}");
    assert!(why.contains("is not taken off by hand"), "{why}");

    let attempts = waiting.max_attempts as usize;
    cycles(&orch, &conductor, attempts).await;

    let exhausted = row(&refused);
    assert_eq!(
        (exhausted.state, exhausted.step),
        (WorkState::Failed, WorkStep::Paid)
    );
    assert!(exhausted
        .last_error
        .unwrap()
        .contains("Exceeded max attempts in-cycle"));
    assert_eq!(
        calls_to(&conductor, "create_receipt_from_rave"),
        attempts + 2
    );
    assert_eq!(withdraw_proofs(&conductor).len(), 1);
}

#[tokio::test]
async fn a_slow_refused_receipt_ends_the_cycle() {
    let mut orch = test_orchestrator("withdrawal-receipt-refused-slowly");
    orch.cfg.slow_call_threshold_ms = 5;
    let [refused, next] = [0x5D, 0x5F].map(hot_withdrawal);
    let conductor = FakeConductor {
        refuses_receipts: HashMap::from([(action_hash(0xE8), INVALID_COMMIT)]),
        receipt_delay_ms: 20,
        ..bridging_conductor()
            .taken(std::slice::from_ref(&refused))
            .taken(std::slice::from_ref(&next))
    };
    let row = |spend: &Transaction| orch.db.withdrawal(&spend.id.to_string()).unwrap().unwrap();
    for (spend, rave) in [(&refused, 0xE8), (&next, 0xE9)] {
        sent(&orch, spend);
        orch.db
            .advance_to_paid(row(spend).id, &action_hash(rave).to_string())
            .unwrap();
    }

    orch.run_bridge_cycle(&conductor, &running()).await.unwrap();

    assert_eq!(calls_to(&conductor, "create_receipt_from_rave"), 1);
    assert_eq!(
        [&refused, &next].map(|spend| {
            let row = row(spend);
            (row.state, row.step, row.attempts)
        }),
        [
            (WorkState::Queued, WorkStep::Paid, 1),
            (WorkState::Queued, WorkStep::Paid, 0)
        ]
    );
}

const INVALID_COMMIT: &str = r#"Failed to call zome: External API wire error: InternalError("Source chain error: InvalidCommit error: Validation failed while committing: The accept is already used")"#;
const CHAIN_FORKED: &str = r#"Failed to call zome: External API wire error: InternalError("Wasm runtime error while working with Ribosome: RuntimeError: transactor::entries:72: Guest(\"Agent AgentPubKey(uhCAk) carries a ChainFork warrant; nothing is transacted with it\")")"#;

#[test]
fn only_the_conductor_s_validation_or_its_zome_refuses_a_write() {
    for refusal in [INVALID_COMMIT, CHAIN_FORKED] {
        assert!(refused(&anyhow::anyhow!("{refusal}")), "{refusal}");
    }
    for failure in [
        r#"Failed to call zome: Unable to sign zome call: {"error":"TimedOut"}"#,
        "Failed to call zome: Websocket error: Timeout",
        "Failed to call zome: Websocket error: Websocket closed: No connection",
        r#"Failed to call zome: External API wire error: InternalError("Source chain error: deadline has elapsed")"#,
        r#"Failed to call zome: External API wire error: InternalError("Wasm runtime error while working with Ribosome: RuntimeError: transactor::entries:1: Host(\"network error\")")"#,
        r#"Failed to call zome: External API wire error: InternalError("Source chain error: The commit could not be completed but may be retried: DepMissingFromDht([])")"#,
        r#"Failed to call zome: External API wire error: InternalError("Source chain error: Attempted to commit a bundle to the source chain, but the source chain head has moved since the bundle began. Bundle head: None, Current head: None")"#,
        "Failed to deserialize response: invalid type",
    ] {
        assert!(!refused(&anyhow::anyhow!("{failure}")), "{failure}");
    }
    assert!(!refused(&Stopped.into()));
}
