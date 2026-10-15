use super::{StateStore, WorkState};
use crate::screening::{Action, Transfer};
use alloy::primitives::Address;
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::fmt;

pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS withdrawal_holds (
            spend TEXT PRIMARY KEY,
            state TEXT NOT NULL,
            listed TEXT NOT NULL,
            by_name TEXT,
            reason TEXT,
            at INTEGER NOT NULL
         )",
        [],
    )?;
    Ok(())
}

/// A withdrawal held by screening, or released or blocked by a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    Held,
    Released,
    Blocked,
}

impl Hold {
    fn of(state: &str) -> Self {
        match state {
            "released" => Hold::Released,
            "blocked" => Hold::Blocked,
            _ => Hold::Held,
        }
    }
}

impl fmt::Display for Hold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Hold::Held => "held",
            Hold::Released => "released",
            Hold::Blocked => "blocked",
        })
    }
}

pub struct Acted {
    pub was: String,
    pub now: String,
}

impl StateStore {
    /// A no-op once a person has moved the lock out of `screening`.
    pub fn settle_screened_lock(&self, item_id: &str, to: WorkState) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE work_items SET state = ?2, updated_at = strftime('%s', 'now')
             WHERE flow = 'lock' AND item_id = ?1 AND state = 'screening'",
            params![item_id, to.to_string()],
        )?;
        Ok(())
    }

    pub fn withdrawal_hold(&self, spend: &str) -> Result<Option<Hold>> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        hold_of(&conn, spend)
    }

    /// Holds `spend` for a person, unless a person decided it meanwhile, and returns its hold.
    pub fn hold_withdrawal(&self, spend: &str, listed: &[Address]) -> Result<Hold> {
        let mut conn = self.conn.lock().expect("db mutex poisoned");
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO withdrawal_holds (spend, state, listed, at)
             VALUES (?1, 'held', ?2, strftime('%s', 'now'))",
            params![spend, joined(listed)],
        )?;
        let hold = hold_of(&tx, spend)?.unwrap_or(Hold::Held);
        tx.commit()?;
        Ok(hold)
    }

    /// A person's `release` or `block` of `transfer`, or a refusal naming the state it is in.
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
        let was = match transfer {
            Transfer::Lock(_) => {
                let states = tx
                    .prepare("SELECT state FROM work_items WHERE flow = 'lock' AND item_id = ?1")?
                    .query_map([&id], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                match states.as_slice() {
                    [state] => state.clone(),
                    [] => anyhow::bail!("{id} has no row in {}", self.path.display()),
                    _ => anyhow::bail!("{id} names {} rows", states.len()),
                }
            }
            Transfer::Withdrawal(spend) => {
                hold_of(&tx, spend)?.map_or("unheld".to_string(), |hold| hold.to_string())
            }
        };
        let now = match (action, transfer, was.as_str()) {
            (Action::Release, Transfer::Lock(_), "held") => "queued",
            (Action::Release, Transfer::Withdrawal(_), "held") => "released",
            (Action::Block, Transfer::Lock(_), "held" | "screening") => "blocked",
            (Action::Block, Transfer::Withdrawal(_), "held" | "unheld") => "blocked",
            (Action::Release, ..) => {
                anyhow::bail!("{id} is {was}: release takes only a held transfer")
            }
            (Action::Block, ..) => anyhow::bail!(
                "{id} is {was}: block takes only a held transfer, or one with no verdict"
            ),
        };
        match transfer {
            Transfer::Lock(_) => tx.execute(
                "UPDATE work_items SET state = ?2, updated_at = strftime('%s', 'now')
                 WHERE flow = 'lock' AND item_id = ?1 AND state = ?3",
                params![id, now, was],
            )?,
            Transfer::Withdrawal(spend) => tx.execute(
                "INSERT INTO withdrawal_holds (spend, state, listed, by_name, reason, at)
                 VALUES (?1, ?2, '', ?3, ?4, strftime('%s', 'now'))
                 ON CONFLICT(spend) DO UPDATE SET
                    state = excluded.state, by_name = excluded.by_name,
                    reason = excluded.reason, at = excluded.at",
                params![spend, now, by, reason],
            )?,
        };
        tx.commit()?;
        Ok(Acted {
            was,
            now: now.to_string(),
        })
    }

    /// `open` leaves out the blocked.
    pub fn screenings(&self, open: bool) -> Result<Vec<Value>> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut out = Vec::new();
        let mut locks = conn.prepare(
            "SELECT item_id, state, payload_json FROM work_items
             WHERE flow = 'lock' AND state IN ('screening', 'held', 'blocked')
               AND NOT (?1 AND state = 'blocked')
             ORDER BY id",
        )?;
        for row in locks.query_map([open], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })? {
            let (item_id, state, payload) = row?;
            let payload: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
            out.push(json!({
                "transfer": item_id,
                "state": state,
                "sender": payload["sender"],
                "from": payload["from"],
                "amount": payload["amount_hot"],
            }));
        }
        let mut withdrawals = conn.prepare(
            "SELECT spend, state, listed, by_name, reason, at FROM withdrawal_holds
             WHERE state IN ('held', 'blocked') AND NOT (?1 AND state = 'blocked')
             ORDER BY at, spend",
        )?;
        for row in withdrawals.query_map([open], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })? {
            let (spend, state, listed, by, reason, at) = row?;
            let listed: Vec<&str> = listed.split(',').filter(|a| !a.is_empty()).collect();
            out.push(json!({
                "transfer": Transfer::Withdrawal(spend).to_string(),
                "state": state,
                "listed": listed,
                "by": by,
                "reason": reason,
                "at": at,
            }));
        }
        Ok(out)
    }
}

fn hold_of(conn: &Connection, spend: &str) -> Result<Option<Hold>> {
    Ok(conn
        .query_row(
            "SELECT state FROM withdrawal_holds WHERE spend = ?1",
            [spend],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|state| Hold::of(&state)))
}

fn joined(addresses: &[Address]) -> String {
    addresses
        .iter()
        .map(|address| format!("{address:#x}"))
        .collect::<Vec<_>>()
        .join(",")
}
