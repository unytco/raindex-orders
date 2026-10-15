use super::{StateStore, WorkState};
use crate::screening::{Action, Role, Screened, Transfer, Verdict};
use alloy::primitives::Address;
use anyhow::Result;
use rusqlite::types::Type;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fmt;

pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS screening_records (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            transfer TEXT NOT NULL,
            event TEXT NOT NULL,
            provider TEXT,
            list_version TEXT,
            addresses TEXT,
            cause TEXT,
            by_name TEXT,
            reason TEXT,
            at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_screening_records_transfer
            ON screening_records(transfer, id);
         CREATE TABLE IF NOT EXISTS screening_lists (
            version TEXT PRIMARY KEY,
            addresses TEXT NOT NULL
         );",
    )?;
    Ok(())
}

/// Where a withdrawal's screening stands, which its records alone decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    Unscreened,
    Screening,
    Clear,
    Held,
    Released,
    Blocked,
}

impl Standing {
    pub fn pays(self) -> bool {
        matches!(self, Standing::Clear | Standing::Released)
    }

    fn of<'a>(events: impl IntoIterator<Item = &'a str>) -> Self {
        events
            .into_iter()
            .fold(Standing::Unscreened, |standing, event| {
                match (standing, event) {
                    (Standing::Blocked, _) | (_, "blocked") => Standing::Blocked,
                    (Standing::Released, _) | (_, "released") => Standing::Released,
                    (Standing::Held, _) => Standing::Held,
                    (_, "clear") => Standing::Clear,
                    (_, "hit") => Standing::Held,
                    _ => Standing::Screening,
                }
            })
    }

    /// Decided: no screening changes it, only a person.
    fn decided(self) -> bool {
        matches!(
            self,
            Standing::Held | Standing::Released | Standing::Blocked
        )
    }
}

impl fmt::Display for Standing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Standing::Unscreened => "unscreened",
            Standing::Screening => "screening",
            Standing::Clear => "clear",
            Standing::Held => "held",
            Standing::Released => "released",
            Standing::Blocked => "blocked",
        })
    }
}

pub struct Acted {
    pub was: String,
    pub now: String,
}

#[derive(Serialize)]
struct ScreenedAddress {
    address: String,
    role: Option<Role>,
    listed: Option<bool>,
}

#[derive(Serialize)]
struct Record {
    event: String,
    at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    list_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    addresses: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cause: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

const VERDICTS: [&str; 3] = ["clear", "hit", "no_answer"];

impl StateStore {
    /// Records `screened` for the lock `item_id`, and moves it out of `screening` by the
    /// verdict, unless a person moved it meanwhile.
    pub fn record_lock_verdict(
        &self,
        item_id: &str,
        screened: &Screened,
        roles: &[(Address, Role)],
    ) -> Result<()> {
        let mut conn = self.conn.lock().expect("db mutex poisoned");
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_verdict(&tx, item_id, screened, roles)?;
        let moved_to = match screened.verdict {
            Verdict::Clear => WorkState::Queued,
            Verdict::Hit(_) => WorkState::Held,
            Verdict::NoAnswer(_) => return Ok(tx.commit()?),
        };
        tx.execute(
            "UPDATE work_items SET state = ?2, updated_at = strftime('%s', 'now')
             WHERE flow = 'lock' AND item_id = ?1 AND state = 'screening'",
            params![item_id, moved_to.to_string()],
        )?;
        Ok(tx.commit()?)
    }

    /// Records `screened` for the withdrawal `spend`, unless its standing was decided meanwhile,
    /// and returns its standing.
    pub fn record_withdrawal_verdict(
        &self,
        spend: &str,
        screened: &Screened,
        to: Address,
    ) -> Result<Standing> {
        let mut conn = self.conn.lock().expect("db mutex poisoned");
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let transfer = Transfer::Withdrawal(spend.to_string()).to_string();
        let before = standing(&tx, &transfer)?;
        if before.decided() {
            return Ok(before);
        }
        insert_verdict(&tx, &transfer, screened, &[(to, Role::WithdrawToAddress)])?;
        let after = standing(&tx, &transfer)?;
        tx.commit()?;
        Ok(after)
    }

    pub fn withdrawal_standing(&self, spend: &str) -> Result<Standing> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        standing(&conn, &Transfer::Withdrawal(spend.to_string()).to_string())
    }

    /// The list version of the withdrawal's last clear verdict.
    pub fn cleared_on(&self, spend: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        Ok(conn
            .query_row(
                "SELECT list_version FROM screening_records
                 WHERE transfer = ?1 AND event = 'clear' ORDER BY id DESC LIMIT 1",
                [Transfer::Withdrawal(spend.to_string()).to_string()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    pub fn act(
        &self,
        transfer: &Transfer,
        action: Action,
        by: &str,
        reason: &str,
    ) -> Result<Acted> {
        let mut conn = self.conn.lock().expect("db mutex poisoned");
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = transfer.to_string();
        let lock_row = match transfer {
            Transfer::Lock(_) => {
                let rows = tx
                    .prepare(
                        "SELECT id, state FROM work_items WHERE flow = 'lock' AND item_id = ?1",
                    )?
                    .query_map([&id], |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                match rows.as_slice() {
                    [row] => Some(row.clone()),
                    [] => anyhow::bail!("{id} has no row in {}", self.path.display()),
                    _ => anyhow::bail!("{id} names {} rows", rows.len()),
                }
            }
            Transfer::Withdrawal(_) => None,
        };
        let was = match &lock_row {
            Some((_, state)) => state.clone(),
            None => standing(&tx, &id)?.to_string(),
        };
        let now = match (action, was.as_str()) {
            (Action::Release, "held") if lock_row.is_some() => "queued",
            (Action::Release, "held") => "released",
            (Action::Block, "held" | "screening" | "unscreened" | "blocked") => "blocked",
            (Action::Release, _) => anyhow::bail!("{id} is {was}: release takes only a held transfer"),
            (Action::Block, _) => anyhow::bail!(
                "{id} is {was}: block takes only a held transfer, one with no verdict, or a blocked one"
            ),
        };
        if let Some((row, _)) = lock_row {
            tx.execute(
                "UPDATE work_items SET state = ?2, updated_at = strftime('%s', 'now')
                 WHERE id = ?1 AND state = ?3",
                params![row, now, was],
            )?;
        }
        tx.execute(
            "INSERT INTO screening_records (transfer, event, by_name, reason, at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                action.event(),
                by,
                reason,
                chrono::Utc::now().timestamp()
            ],
        )?;
        tx.commit()?;
        Ok(Acted {
            was,
            now: now.to_string(),
        })
    }

    /// Each screened transfer, and each lock in `screening`, `held` or `blocked`, with its state
    /// and records. `open` keeps only those that wait: held, or with no verdict.
    pub fn screenings(&self, only: Option<&Transfer>, open: bool) -> Result<Vec<Value>> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let only = only.map(ToString::to_string);
        let mut transfers: Vec<(String, Vec<Record>)> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT transfer, event, provider, list_version, addresses, cause, by_name, reason, at
             FROM screening_records WHERE ?1 IS NULL OR transfer = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([&only], |row| {
            let addresses: Option<String> = row.get(4)?;
            Ok((
                row.get::<_, String>(0)?,
                Record {
                    event: row.get(1)?,
                    provider: row.get(2)?,
                    list_version: row.get(3)?,
                    addresses: addresses
                        .map(|text| serde_json::from_str(&text))
                        .transpose()
                        .map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(4, Type::Text, Box::new(e))
                        })?,
                    cause: row.get(5)?,
                    by: row.get(6)?,
                    reason: row.get(7)?,
                    at: row.get(8)?,
                },
            ))
        })?;
        for row in rows {
            let (transfer, record) = row?;
            let at = *index.entry(transfer.clone()).or_insert_with(|| {
                transfers.push((transfer, Vec::new()));
                transfers.len() - 1
            });
            transfers[at].1.push(record);
        }
        let waiting = conn
            .prepare(
                "SELECT item_id FROM work_items
                 WHERE flow = 'lock' AND state IN ('screening', 'held', 'blocked')
                   AND (?1 IS NULL OR item_id = ?1)
                 ORDER BY id",
            )?
            .query_map([&only], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for item_id in waiting {
            if !index.contains_key(&item_id) {
                index.insert(item_id.clone(), transfers.len());
                transfers.push((item_id, Vec::new()));
            }
        }

        let mut out = Vec::new();
        for (transfer, records) in transfers {
            let state = if transfer.starts_with("lock:") {
                conn.query_row(
                    "SELECT state FROM work_items WHERE flow = 'lock' AND item_id = ?1
                     ORDER BY id DESC LIMIT 1",
                    [&transfer],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            } else {
                Some(Standing::of(records.iter().map(|record| record.event.as_str())).to_string())
            };
            if open && !matches!(state.as_deref(), Some("screening" | "held")) {
                continue;
            }
            let first_screened_at = records
                .iter()
                .find(|record| VERDICTS.contains(&record.event.as_str()))
                .map(|record| record.at);
            out.push(json!({
                "transfer": transfer,
                "state": state,
                "first_screened_at": first_screened_at,
                "records": records,
            }));
        }
        Ok(out)
    }
}

fn standing(conn: &Connection, transfer: &str) -> Result<Standing> {
    let events = conn
        .prepare("SELECT event FROM screening_records WHERE transfer = ?1 ORDER BY id")?
        .query_map([transfer], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Standing::of(events.iter().map(String::as_str)))
}

/// A no-answer with the same provider and cause as the transfer's last record is not recorded
/// again.
fn insert_verdict(
    conn: &Connection,
    transfer: &str,
    screened: &Screened,
    roles: &[(Address, Role)],
) -> Result<()> {
    if let Verdict::NoAnswer(cause) = &screened.verdict {
        let last: Option<(String, Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT event, provider, cause FROM screening_records
                 WHERE transfer = ?1 ORDER BY id DESC LIMIT 1",
                [transfer],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if last
            == Some((
                "no_answer".into(),
                Some(screened.provider.into()),
                Some(cause.clone()),
            ))
        {
            return Ok(());
        }
    }
    if let Some(content) = screened.list.as_ref().and_then(|list| list.content()) {
        conn.execute(
            "INSERT OR IGNORE INTO screening_lists (version, addresses) VALUES (?1, ?2)",
            params![screened.list.as_ref().map(|list| list.version()), content],
        )?;
    }
    let addresses: Vec<ScreenedAddress> = screened
        .listed
        .iter()
        .map(|(address, listed)| ScreenedAddress {
            address: format!("{address:#x}"),
            role: roles
                .iter()
                .find(|(screened, _)| screened == address)
                .map(|(_, role)| *role),
            listed: *listed,
        })
        .collect();
    let cause = match &screened.verdict {
        Verdict::NoAnswer(cause) => Some(cause),
        _ => None,
    };
    conn.execute(
        "INSERT INTO screening_records (transfer, event, provider, list_version, addresses, cause, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            transfer,
            screened.verdict.name(),
            screened.provider,
            screened.list.as_ref().map(|list| list.version()),
            serde_json::to_string(&addresses)?,
            cause,
            screened.at,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screening::{screen, DenyList, Screener};

    const LISTED: Address = Address::repeat_byte(0xBA);
    const CLEAN: Address = Address::repeat_byte(0x11);
    const WALLET: Address = Address::repeat_byte(0x22);

    fn store(dir: &tempfile::TempDir) -> StateStore {
        StateStore::open(dir.path().join("screening.db")).unwrap()
    }

    fn lock_in(db: &StateStore, id: &str, state: WorkState) {
        db.enqueue(
            state,
            "lock",
            "create_parked_link",
            &format!("lock:{id}"),
            &format!("lock:{id}:key"),
            &json!({ "lock_id": id }),
        )
        .unwrap();
    }

    fn lock_state(db: &StateStore, id: &str) -> String {
        db.list_flow("lock")
            .unwrap()
            .into_iter()
            .find(|row| row.item_id == format!("lock:{id}"))
            .unwrap()
            .state
            .to_string()
    }

    async fn screened(addresses: &[Address]) -> Screened {
        screen(&DenyList::new([LISTED]), addresses).await
    }

    fn silent(cause: &str) -> Screened {
        Screened {
            provider: "silent",
            list: None,
            at: 1,
            listed: vec![(CLEAN, None)],
            verdict: Verdict::NoAnswer(cause.into()),
            in_time: true,
        }
    }

    fn records(db: &StateStore, transfer: &Transfer) -> Vec<Value> {
        db.screenings(Some(transfer), false)
            .unwrap()
            .first()
            .map(|screened| screened["records"].as_array().unwrap().clone())
            .unwrap_or_default()
    }

    fn act(db: &StateStore, transfer: &Transfer, action: Action) -> Result<(String, String)> {
        db.act(transfer, action, "Ana", "checked by hand")
            .map(|acted| (acted.was, acted.now))
    }

    fn lock(id: &str) -> Transfer {
        Transfer::Lock(id.into())
    }

    fn spend(id: &str) -> Transfer {
        Transfer::Withdrawal(id.into())
    }

    #[tokio::test]
    async fn a_verdict_moves_a_lock_out_of_screening_and_keeps_each_address_with_its_list() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(&dir);
        for (id, sender) in [("1", CLEAN), ("2", LISTED)] {
            lock_in(&db, id, WorkState::Screening);
            db.record_lock_verdict(
                &format!("lock:{id}"),
                &screened(&[sender, WALLET]).await,
                &[(sender, Role::Sender), (WALLET, Role::From)],
            )
            .unwrap();
        }

        assert_eq!(lock_state(&db, "1"), "queued");
        assert_eq!(lock_state(&db, "2"), "held");
        let held = records(&db, &lock("2"));
        assert_eq!(held.len(), 1);
        assert_eq!(held[0]["event"], "hit");
        assert_eq!(held[0]["provider"], "denylist");
        assert_eq!(
            held[0]["addresses"],
            json!([
                {"address": format!("{LISTED:#x}"), "role": "sender", "listed": true},
                {"address": format!("{WALLET:#x}"), "role": "from", "listed": false},
            ])
        );
        let version = held[0]["list_version"].as_str().unwrap();
        let kept: String = db
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT addresses FROM screening_lists WHERE version = ?1",
                [version],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(kept, format!("{LISTED:#x}"));
    }

    #[tokio::test]
    async fn a_verdict_leaves_a_lock_a_person_moved_meanwhile_where_they_put_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(&dir);
        lock_in(&db, "1", WorkState::Screening);
        act(&db, &lock("1"), Action::Block).unwrap();

        db.record_lock_verdict(
            "lock:1",
            &screened(&[CLEAN]).await,
            &[(CLEAN, Role::Sender)],
        )
        .unwrap();

        assert_eq!(lock_state(&db, "1"), "blocked");
    }

    #[test]
    fn a_no_answer_is_recorded_again_only_for_another_provider_or_cause() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(&dir);
        lock_in(&db, "1", WorkState::Screening);
        let record = |screened: &Screened| {
            db.record_lock_verdict("lock:1", screened, &[(CLEAN, Role::Sender)])
                .unwrap()
        };

        record(&silent("it timed out"));
        record(&silent("it timed out"));
        record(&silent("the key was refused"));
        let mut other = silent("the key was refused");
        other.provider = "other";
        record(&other);

        let events: Vec<_> = records(&db, &lock("1"))
            .iter()
            .map(|record| (record["provider"].clone(), record["cause"].clone()))
            .collect();
        assert_eq!(
            events,
            [
                (json!("silent"), json!("it timed out")),
                (json!("silent"), json!("the key was refused")),
                (json!("other"), json!("the key was refused")),
            ]
        );
        assert_eq!(
            records(&db, &lock("1"))[0]["addresses"][0]["listed"],
            Value::Null
        );
        assert_eq!(lock_state(&db, "1"), "screening");
    }

    #[test]
    fn release_and_block_take_a_lock_only_from_the_states_they_name() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(&dir);
        for (id, state) in [
            ("held", WorkState::Held),
            ("waiting", WorkState::Screening),
            ("queued", WorkState::Queued),
            ("held-then-blocked", WorkState::Held),
        ] {
            lock_in(&db, id, state);
        }

        assert_eq!(
            act(&db, &lock("held"), Action::Release).unwrap(),
            ("held".into(), "queued".into())
        );
        let e = act(&db, &lock("waiting"), Action::Release).unwrap_err();
        assert!(
            format!("{e:#}")
                .contains("lock:waiting is screening: release takes only a held transfer"),
            "{e:#}"
        );
        assert_eq!(
            act(&db, &lock("waiting"), Action::Block).unwrap(),
            ("screening".into(), "blocked".into())
        );
        assert_eq!(
            act(&db, &lock("held-then-blocked"), Action::Block).unwrap(),
            ("held".into(), "blocked".into())
        );
        assert_eq!(
            act(&db, &lock("held-then-blocked"), Action::Block).unwrap(),
            ("blocked".into(), "blocked".into()),
            "a blocked lock takes another record, such as for the vault admin moving its HOT"
        );
        assert_eq!(records(&db, &lock("held-then-blocked")).len(), 2);

        for (id, action, refusal) in [
            ("held", Action::Release, "lock:held is queued: release takes only a held transfer"),
            ("queued", Action::Block, "lock:queued is queued: block takes only a held transfer, one with no verdict, or a blocked one"),
            ("waiting", Action::Release, "lock:waiting is blocked: release takes only a held transfer"),
            ("none", Action::Block, "lock:none has no row"),
        ] {
            let e = act(&db, &lock(id), action).unwrap_err();
            assert!(format!("{e:#}").contains(refusal), "{e:#}");
        }
        assert_eq!(lock_state(&db, "queued"), "queued");
        assert!(records(&db, &lock("queued")).is_empty());
    }

    #[tokio::test]
    async fn a_withdrawals_standing_is_what_its_records_say() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(&dir);
        let record = |id: &str, screened: &Screened| {
            db.record_withdrawal_verdict(id, screened, LISTED).unwrap()
        };
        let standing = |id: &str| db.withdrawal_standing(id).unwrap();

        assert_eq!(standing("a"), Standing::Unscreened);
        assert_eq!(record("a", &silent("down")), Standing::Screening);
        assert_eq!(record("a", &screened(&[LISTED]).await), Standing::Held);
        assert_eq!(
            act(&db, &spend("a"), Action::Release).unwrap(),
            ("held".into(), "released".into())
        );
        assert!(standing("a").pays());

        record("b", &screened(&[LISTED]).await);
        act(&db, &spend("b"), Action::Block).unwrap();
        act(&db, &spend("b"), Action::Block).unwrap();
        assert_eq!(standing("b"), Standing::Blocked);
        assert!(!standing("b").pays());

        assert_eq!(record("c", &screened(&[CLEAN]).await), Standing::Clear);
        assert!(standing("c").pays());
        assert_eq!(
            db.cleared_on("c").unwrap().as_deref(),
            DenyList::new([LISTED]).version()
        );
        let relisted = screen(&DenyList::new([LISTED, CLEAN]), &[CLEAN]).await;
        record("f", &screened(&[CLEAN]).await);
        assert_eq!(
            record("f", &relisted),
            Standing::Held,
            "a clear verdict gives way to a later one"
        );
        assert_eq!(record("e", &screened(&[CLEAN]).await), Standing::Clear);
        assert_eq!(record("e", &silent("down")), Standing::Screening);

        assert_eq!(
            act(&db, &spend("never-screened"), Action::Block).unwrap(),
            ("unscreened".into(), "blocked".into())
        );
        assert_eq!(standing("never-screened"), Standing::Blocked);

        for (id, action, refusal) in [
            (
                "a",
                Action::Block,
                "withdrawal:a is released: block takes only",
            ),
            (
                "b",
                Action::Release,
                "withdrawal:b is blocked: release takes only",
            ),
            (
                "c",
                Action::Block,
                "withdrawal:c is clear: block takes only",
            ),
            (
                "d",
                Action::Release,
                "withdrawal:d is unscreened: release takes only",
            ),
        ] {
            let e = act(&db, &spend(id), action).unwrap_err();
            assert!(format!("{e:#}").contains(refusal), "{e:#}");
        }
    }

    #[tokio::test]
    async fn a_verdict_on_a_withdrawal_a_person_decided_meanwhile_is_not_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(&dir);
        db.record_withdrawal_verdict("w", &silent("down"), CLEAN)
            .unwrap();
        act(&db, &spend("w"), Action::Block).unwrap();

        let standing = db
            .record_withdrawal_verdict("w", &screened(&[CLEAN]).await, CLEAN)
            .unwrap();

        assert_eq!(standing, Standing::Blocked);
        assert!(!standing.pays());
        let events: Vec<_> = records(&db, &spend("w"))
            .iter()
            .map(|record| record["event"].clone())
            .collect();
        assert_eq!(events, [json!("no_answer"), json!("blocked")]);
    }

    #[tokio::test]
    async fn screenings_lists_each_transfer_and_open_keeps_only_those_that_wait() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(&dir);
        for (id, state) in [
            ("1", WorkState::Screening),
            ("2", WorkState::Screening),
            ("3", WorkState::Screening),
            ("4", WorkState::Queued),
        ] {
            lock_in(&db, id, state);
        }
        for (id, sender) in [("2", LISTED), ("3", CLEAN)] {
            db.record_lock_verdict(
                &format!("lock:{id}"),
                &screened(&[sender]).await,
                &[(sender, Role::Sender)],
            )
            .unwrap();
        }
        db.record_withdrawal_verdict("uhCkkSPEND", &silent("down"), CLEAN)
            .unwrap();
        let listed = |open| -> Vec<(Value, Value)> {
            db.screenings(None, open)
                .unwrap()
                .into_iter()
                .map(|transfer| (transfer["transfer"].clone(), transfer["state"].clone()))
                .collect()
        };

        assert_eq!(
            listed(false),
            [
                (json!("lock:2"), json!("held")),
                (json!("lock:3"), json!("queued")),
                (json!("withdrawal:uhCkkSPEND"), json!("screening")),
                (json!("lock:1"), json!("screening")),
            ]
        );
        assert_eq!(
            listed(true),
            [
                (json!("lock:2"), json!("held")),
                (json!("withdrawal:uhCkkSPEND"), json!("screening")),
                (json!("lock:1"), json!("screening")),
            ]
        );
        let one = db.screenings(Some(&lock("2")), false).unwrap();
        assert_eq!(one.len(), 1);
        assert!(one[0]["first_screened_at"].as_i64().is_some());
        let waiting = db.screenings(Some(&lock("1")), true).unwrap();
        assert_eq!(waiting[0]["first_screened_at"], Value::Null);
        assert_eq!(waiting[0]["records"], json!([]));
    }

    #[test]
    fn a_record_that_cannot_be_read_fails_the_listing_instead_of_dropping_from_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(&dir);
        db.record_withdrawal_verdict("w", &silent("down"), CLEAN)
            .unwrap();
        db.conn
            .lock()
            .unwrap()
            .execute("UPDATE screening_records SET addresses = 'not json'", [])
            .unwrap();

        assert!(db.screenings(None, false).is_err());
    }

    #[tokio::test]
    async fn neither_clear_mode_nor_retention_deletes_a_record_or_a_lock_that_waits() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(&dir);
        lock_in(&db, "screened", WorkState::Screening);
        db.record_lock_verdict(
            "lock:screened",
            &screened(&[LISTED]).await,
            &[(LISTED, Role::Sender)],
        )
        .unwrap();
        lock_in(&db, "waiting", WorkState::Screening);
        lock_in(&db, "blocked", WorkState::Blocked);
        lock_in(&db, "released", WorkState::Held);
        act(&db, &lock("released"), Action::Release).unwrap();
        let released = db.list_work_items("lock", WorkState::Queued, 10).unwrap()[0].id;
        db.advance_to_br_rave_executed(released, None).unwrap();
        lock_in(&db, "failed", WorkState::Failed);
        db.conn
            .lock()
            .unwrap()
            .execute("UPDATE work_items SET updated_at = 0, created_at = 0", [])
            .unwrap();
        let records_of_each = |db: &StateStore| -> Vec<Value> {
            db.screenings(None, false)
                .unwrap()
                .iter()
                .map(|transfer| transfer["records"].clone())
                .collect()
        };
        let before = records_of_each(&db);

        db.prune_terminal_older_than(0, 0).unwrap();
        db.clear_non_in_progress().unwrap();
        let cleared = db.clear_all().unwrap();

        assert_eq!(
            cleared, 0,
            "only the waiting locks were left, and they stay"
        );
        assert_eq!(records_of_each(&db), before);
        for (id, state) in [
            ("screened", "held"),
            ("waiting", "screening"),
            ("blocked", "blocked"),
        ] {
            assert_eq!(lock_state(&db, id), state);
        }
        assert_eq!(
            records(&db, &lock("released"))[0]["event"],
            "released",
            "the record of a credited lock outlives its row"
        );
    }

    #[test]
    fn a_database_that_is_not_there_is_refused_and_not_made() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.db");

        let e = StateStore::open_existing(path.to_str().unwrap())
            .err()
            .unwrap();

        assert!(format!("{e:#}").contains("does not exist"), "{e:#}");
        assert!(!path.exists());
    }
}
