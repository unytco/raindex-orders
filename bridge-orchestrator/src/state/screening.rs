use super::{StateStore, WorkState};
use crate::screening::{Action, Role, Screened, Transfer, Verdict};
use alloy::primitives::Address;
use anyhow::Result;
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

    fn of<'a>(events: impl IntoIterator<Item = &'a str>) -> Option<Self> {
        events.into_iter().fold(None, |standing, event| {
            Some(match (standing, event) {
                (Some(Standing::Blocked), _) | (_, "blocked") => Standing::Blocked,
                (Some(Standing::Released), _) | (_, "released") => Standing::Released,
                (Some(decided @ (Standing::Clear | Standing::Held)), _) => decided,
                (_, "clear") => Standing::Clear,
                (_, "hit") => Standing::Held,
                _ => Standing::Screening,
            })
        })
    }
}

impl fmt::Display for Standing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
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
    role: Role,
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
    /// Records `screened` against `transfer`, and moves `lock_row` out of `screening` by its
    /// verdict. A no-answer from the provider whose no-answer is the transfer's last record is
    /// not recorded again, and returns false.
    pub fn record_verdict(
        &self,
        transfer: &Transfer,
        screened: &Screened,
        addresses: &[(Address, Role)],
        lock_row: Option<i64>,
    ) -> Result<bool> {
        let mut conn = self.conn.lock().expect("db mutex poisoned");
        let tx = conn.transaction()?;
        let id = transfer.to_string();
        if let Verdict::NoAnswer(_) = screened.verdict {
            let last: Option<(String, Option<String>)> = tx
                .query_row(
                    "SELECT event, provider FROM screening_records
                     WHERE transfer = ?1 ORDER BY id DESC LIMIT 1",
                    [&id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if last == Some(("no_answer".into(), Some(screened.provider.into()))) {
                return Ok(false);
            }
        }
        if let Some(list) = &screened.list {
            tx.execute(
                "INSERT OR IGNORE INTO screening_lists (version, addresses) VALUES (?1, ?2)",
                params![list.sha256, list.joined()],
            )?;
        }
        let (listed, cause, moved_to) = match &screened.verdict {
            Verdict::Clear => (Some(&[][..]), None, Some(WorkState::Queued)),
            Verdict::Hit(listed) => (Some(&listed[..]), None, Some(WorkState::Held)),
            Verdict::NoAnswer(cause) => (None, Some(cause), None),
        };
        let addresses: Vec<ScreenedAddress> = addresses
            .iter()
            .map(|(address, role)| ScreenedAddress {
                address: format!("{address:#x}"),
                role: *role,
                listed: listed.map(|listed| listed.contains(address)),
            })
            .collect();
        tx.execute(
            "INSERT INTO screening_records (transfer, event, provider, list_version, addresses, cause, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                screened.verdict.name(),
                screened.provider,
                screened.list.as_ref().map(|list| &list.sha256),
                serde_json::to_string(&addresses)?,
                cause,
                screened.at,
            ],
        )?;
        if let (Some(row), Some(to)) = (lock_row, moved_to) {
            tx.execute(
                "UPDATE work_items SET state = ?2, updated_at = strftime('%s', 'now')
                 WHERE id = ?1 AND state = 'screening'",
                params![row, to.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    pub fn withdrawal_standing(&self, spend: &str) -> Result<Option<Standing>> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        standing(&conn, &Transfer::Withdrawal(spend.to_string()))
    }

    /// A person's `release` or `block` of `transfer`, with the record of it, or a refusal naming
    /// the state it is in.
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
            None => standing(&tx, transfer)?
                .ok_or_else(|| anyhow::anyhow!("{id} has no screening record"))?
                .to_string(),
        };
        let now = match (action, was.as_str()) {
            (Action::Release, "held") if lock_row.is_some() => "queued",
            (Action::Release, "held") => "released",
            (Action::Block, "held" | "screening" | "blocked") => "blocked",
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

    /// Each screened transfer, and each lock waiting in `screening`, `held` or `blocked`, with its
    /// state and records. `open` keeps only those that wait: held, or with no verdict.
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
                    addresses: addresses.and_then(|text| serde_json::from_str(&text).ok()),
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
                Standing::of(records.iter().map(|record| record.event.as_str()))
                    .map(|standing| standing.to_string())
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

fn standing(conn: &Connection, transfer: &Transfer) -> Result<Option<Standing>> {
    let events = conn
        .prepare("SELECT event FROM screening_records WHERE transfer = ?1 ORDER BY id")?
        .query_map([transfer.to_string()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Standing::of(events.iter().map(String::as_str)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screening::{DenyList, Screener};

    const LISTED: Address = Address::repeat_byte(0xBA);
    const CLEAN: Address = Address::repeat_byte(0x11);
    const WALLET: Address = Address::repeat_byte(0x22);

    fn store(dir: &tempfile::TempDir) -> StateStore {
        StateStore::open(dir.path().join("screening.db")).unwrap()
    }

    fn lock_in(db: &StateStore, id: &str, state: WorkState) -> i64 {
        db.enqueue(
            state.clone(),
            "lock",
            "create_parked_link",
            &format!("lock:{id}"),
            &format!("lock:{id}:key"),
            &json!({ "lock_id": id }),
        )
        .unwrap();
        db.list_work_items("lock", state, 100)
            .unwrap()
            .into_iter()
            .find(|row| row.item_id == format!("lock:{id}"))
            .unwrap()
            .id
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
        DenyList::new([LISTED]).screen(addresses).await
    }

    fn silent() -> Screened {
        Screened {
            provider: "silent",
            list: None,
            at: 1,
            verdict: Verdict::NoAnswer("it timed out".into()),
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

    #[tokio::test]
    async fn a_verdict_moves_a_lock_out_of_screening_and_keeps_each_address_with_its_list() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(&dir);
        let clear = lock_in(&db, "1", WorkState::Screening);
        let hit = lock_in(&db, "2", WorkState::Screening);
        let roles = |sender| [(sender, Role::Sender), (WALLET, Role::From)];

        for (row, id, sender) in [(clear, "1", CLEAN), (hit, "2", LISTED)] {
            let verdict = screened(&[sender, WALLET]).await;
            assert!(db
                .record_verdict(
                    &Transfer::Lock(id.into()),
                    &verdict,
                    &roles(sender),
                    Some(row)
                )
                .unwrap());
        }

        assert_eq!(lock_state(&db, "1"), "queued");
        assert_eq!(lock_state(&db, "2"), "held");
        let held = records(&db, &Transfer::Lock("2".into()));
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
        let row = lock_in(&db, "1", WorkState::Screening);
        act(&db, &Transfer::Lock("1".into()), Action::Block).unwrap();

        db.record_verdict(
            &Transfer::Lock("1".into()),
            &screened(&[CLEAN]).await,
            &[(CLEAN, Role::Sender)],
            Some(row),
        )
        .unwrap();

        assert_eq!(lock_state(&db, "1"), "blocked");
    }

    #[test]
    fn a_no_answer_is_recorded_once_until_another_record_follows_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(&dir);
        let row = lock_in(&db, "1", WorkState::Screening);
        let lock = Transfer::Lock("1".into());
        let record = |screened: &Screened| {
            db.record_verdict(&lock, screened, &[(CLEAN, Role::Sender)], Some(row))
                .unwrap()
        };

        assert!(record(&silent()));
        assert!(!record(&silent()), "the same provider's silence again");
        let mut other = silent();
        other.provider = "other";
        assert!(record(&other));

        let events: Vec<_> = records(&db, &lock)
            .iter()
            .map(|record| (record["event"].clone(), record["provider"].clone()))
            .collect();
        assert_eq!(
            events,
            [
                (json!("no_answer"), json!("silent")),
                (json!("no_answer"), json!("other"))
            ]
        );
        assert_eq!(records(&db, &lock)[0]["cause"], "it timed out");
        assert_eq!(
            records(&db, &lock)[0]["addresses"][0]["listed"],
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
        let lock = |id: &str| Transfer::Lock(id.into());

        assert_eq!(
            act(&db, &lock("held"), Action::Release).unwrap(),
            ("held".into(), "queued".into())
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
        let spend = |id: &str| Transfer::Withdrawal(id.into());
        let record = |id: &str, screened: &Screened| {
            db.record_verdict(
                &spend(id),
                screened,
                &[(LISTED, Role::WithdrawToAddress)],
                None,
            )
            .unwrap();
        };
        let standing = |id: &str| db.withdrawal_standing(id).unwrap();

        assert_eq!(standing("a"), None);
        record("a", &silent());
        assert_eq!(standing("a"), Some(Standing::Screening));
        record("a", &screened(&[LISTED]).await);
        assert_eq!(standing("a"), Some(Standing::Held));
        assert_eq!(
            act(&db, &spend("a"), Action::Release).unwrap(),
            ("held".into(), "released".into())
        );
        assert!(standing("a").unwrap().pays());

        record("b", &screened(&[LISTED]).await);
        act(&db, &spend("b"), Action::Block).unwrap();
        act(&db, &spend("b"), Action::Block).unwrap();
        assert_eq!(standing("b"), Some(Standing::Blocked));
        assert!(!standing("b").unwrap().pays());

        record("c", &screened(&[CLEAN]).await);
        assert_eq!(standing("c"), Some(Standing::Clear));
        assert!(standing("c").unwrap().pays());

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
            ("d", Action::Block, "withdrawal:d has no screening record"),
        ] {
            let e = act(&db, &spend(id), action).unwrap_err();
            assert!(format!("{e:#}").contains(refusal), "{e:#}");
        }
    }

    #[tokio::test]
    async fn screenings_lists_each_transfer_and_open_keeps_only_those_that_wait() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(&dir);
        lock_in(&db, "1", WorkState::Screening);
        let held = lock_in(&db, "2", WorkState::Screening);
        let cleared = lock_in(&db, "3", WorkState::Screening);
        lock_in(&db, "4", WorkState::Queued);
        for (row, id, sender) in [(held, "2", LISTED), (cleared, "3", CLEAN)] {
            db.record_verdict(
                &Transfer::Lock(id.into()),
                &screened(&[sender]).await,
                &[(sender, Role::Sender)],
                Some(row),
            )
            .unwrap();
        }
        let withdrawal = Transfer::Withdrawal("uhCkkSPEND".into());
        db.record_verdict(
            &withdrawal,
            &silent(),
            &[(CLEAN, Role::WithdrawToAddress)],
            None,
        )
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
        let one = db
            .screenings(Some(&Transfer::Lock("2".into())), false)
            .unwrap();
        assert_eq!(one.len(), 1);
        assert!(one[0]["first_screened_at"].as_i64().is_some());
        let waiting = db
            .screenings(Some(&Transfer::Lock("1".into())), true)
            .unwrap();
        assert_eq!(waiting[0]["first_screened_at"], Value::Null);
        assert_eq!(waiting[0]["records"], json!([]));
    }

    #[tokio::test]
    async fn neither_clear_mode_nor_retention_deletes_a_record_or_a_lock_that_waits() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(&dir);
        let screened_row = lock_in(&db, "screened", WorkState::Screening);
        db.record_verdict(
            &Transfer::Lock("screened".into()),
            &screened(&[LISTED]).await,
            &[(LISTED, Role::Sender)],
            Some(screened_row),
        )
        .unwrap();
        lock_in(&db, "waiting", WorkState::Screening);
        lock_in(&db, "blocked", WorkState::Blocked);
        let released = lock_in(&db, "released", WorkState::Held);
        act(&db, &Transfer::Lock("released".into()), Action::Release).unwrap();
        db.advance_to_br_rave_executed(released, None).unwrap();
        lock_in(&db, "failed", WorkState::Failed);
        db.conn
            .lock()
            .unwrap()
            .execute("UPDATE work_items SET updated_at = 0, created_at = 0", [])
            .unwrap();
        let everything = |db: &StateStore| db.screenings(None, false).unwrap();
        let before = everything(&db);

        db.prune_terminal_older_than(0, 0).unwrap();
        db.clear_non_in_progress().unwrap();
        let cleared = db.clear_all().unwrap();

        assert_eq!(
            cleared, 0,
            "only the waiting locks were left, and they stay"
        );
        let after = everything(&db);
        assert_eq!(
            after
                .iter()
                .map(|transfer| transfer["records"].clone())
                .collect::<Vec<_>>(),
            before
                .iter()
                .map(|transfer| transfer["records"].clone())
                .collect::<Vec<_>>()
        );
        for (id, state) in [
            ("screened", "held"),
            ("waiting", "screening"),
            ("blocked", "blocked"),
        ] {
            assert_eq!(lock_state(&db, id), state);
        }
        assert_eq!(
            records(&db, &Transfer::Lock("released".into()))[0]["event"],
            "released",
            "the record of a credited lock outlives its row"
        );
    }

    #[test]
    fn a_database_an_earlier_binary_kept_opens_with_its_rows_and_takes_screening() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("screening.db");
        drop(StateStore::open(&path).unwrap());
        let earlier = Connection::open(&path).unwrap();
        earlier
            .execute_batch(
                "DROP TABLE screening_records; DROP TABLE screening_lists;
                 UPDATE schema_meta SET version = 3;
                 INSERT INTO work_items (flow, task_type, item_id, idempotency_key, payload_json, state)
                 VALUES ('lock', 'create_parked_link', 'lock:1', 'lock:1:key', '{}', 'queued');",
            )
            .unwrap();

        let db = StateStore::open(&path).unwrap();

        let version: i64 = earlier
            .query_row("SELECT version FROM schema_meta", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            version, 4,
            "a binary before screening refuses it from now on"
        );
        assert_eq!(
            db.list_work_items("lock", WorkState::Queued, 10)
                .unwrap()
                .len(),
            1
        );
        assert!(db.screenings(None, false).unwrap().is_empty());
    }
}
