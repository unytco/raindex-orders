use alloy::primitives::U256;
use anyhow::{Context, Result};
use clap::ValueEnum;
use rusqlite::{params, Connection, OptionalExtension, ToSql};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, Mutex};

const SCHEMA_VERSION: i64 = 1;
#[cfg(test)]
const DEFAULT_MAX_ATTEMPTS: i64 = 8;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    Detected,
    Queued,
    Claimed,
    InFlight,
    Succeeded,
    Failed,
}

impl std::fmt::Display for WorkState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let v = match self {
            WorkState::Detected => "detected",
            WorkState::Queued => "queued",
            WorkState::Claimed => "claimed",
            WorkState::InFlight => "in_flight",
            WorkState::Succeeded => "succeeded",
            WorkState::Failed => "failed",
        };
        write!(f, "{}", v)
    }
}

impl std::str::FromStr for WorkState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "detected" => Ok(Self::Detected),
            "queued" => Ok(Self::Queued),
            "claimed" => Ok(Self::Claimed),
            "in_flight" => Ok(Self::InFlight),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            _ => Err(format!("Unknown state: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: i64,
    pub flow: String,
    pub task_type: String,
    pub item_id: String,
    pub idempotency_key: String,
    pub payload_json: Value,
    pub state: WorkState,
    pub attempts: i64,
    pub max_attempts: i64,
    pub next_retry_at: Option<i64>,
    pub last_attempt_at: Option<i64>,
    pub error_class: Option<String>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusRow {
    pub id: i64,
    pub flow: String,
    pub task_type: String,
    pub item_id: String,
    pub direction: Option<String>,
    pub transfer_type: Option<String>,
    pub amount_raw: Option<String>,
    pub beneficiary: Option<String>,
    pub counterparty: Option<String>,
    pub status: WorkState,
    pub attempts: i64,
    pub max_attempts: i64,
    pub next_retry_at: Option<i64>,
    pub error_class: Option<String>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct StateFilter {
    pub flow: Option<String>,
    pub state: Option<WorkState>,
    pub item_id: Option<String>,
    pub limit: usize,
}

pub struct StateStore {
    conn: Arc<Mutex<Connection>>,
}

impl Clone for StateStore {
    fn clone(&self) -> Self {
        Self {
            conn: Arc::clone(&self.conn),
        }
    }
}

impl StateStore {
    fn extract_transfer_fields(
        flow: &str,
        task_type: &str,
        payload: &Value,
    ) -> (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) {
        if flow == "lock" && task_type == "create_parked_link" {
            return (
                Some("transfer_in".to_string()),
                Some("lock".to_string()),
                extract_lock_amount_hot(payload),
                payload
                    .get("holochain_agent")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                payload
                    .get("sender")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            );
        }

        (None, None, None, None, None)
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent).context("Failed to create db directory")?;
        }
        let conn = Connection::open(path).context("Failed to open sqlite database")?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.init_schema()?;
        store.recover_stale_items()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "CREATE TABLE IF NOT EXISTS schema_meta (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                version INTEGER NOT NULL
            )",
            [],
        )?;
        let version: Option<i64> = conn
            .query_row("SELECT version FROM schema_meta WHERE id = 1", [], |row| {
                row.get(0)
            })
            .optional()?;
        match version {
            None => {
                conn.execute(
                    "INSERT INTO schema_meta (id, version) VALUES (1, ?1)",
                    [SCHEMA_VERSION],
                )?;
            }
            Some(v) if v == SCHEMA_VERSION => {}
            Some(v) if v > SCHEMA_VERSION => {
                anyhow::bail!(
                    "database schema version {} is newer than binary version {}",
                    v,
                    SCHEMA_VERSION
                );
            }
            Some(v) => {
                anyhow::bail!(
                    "unsupported database schema version {}, expected {}",
                    v,
                    SCHEMA_VERSION
                );
            }
        }

        conn.execute(
            "CREATE TABLE IF NOT EXISTS work_items (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                flow TEXT NOT NULL,
                task_type TEXT NOT NULL,
                item_id TEXT NOT NULL,
                idempotency_key TEXT NOT NULL UNIQUE,
                payload_json TEXT NOT NULL,
                state TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                max_attempts INTEGER NOT NULL DEFAULT 8,
                next_retry_at INTEGER,
                last_attempt_at INTEGER,
                error_class TEXT,
                last_error TEXT,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
                updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            )",
            [],
        )?;
        self.ensure_work_item_columns(&conn)?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_work_items_state_created ON work_items(state, created_at)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_work_items_flow_state ON work_items(flow, state)",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS checkpoints (
                checkpoint_key TEXT PRIMARY KEY,
                checkpoint_value TEXT NOT NULL,
                updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            )",
            [],
        )?;
        Ok(())
    }

    fn recover_stale_items(&self) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE work_items
             SET state = 'queued',
                 next_retry_at = NULL,
                 error_class = 'transient',
                 last_error = coalesce(last_error || '; ', '') || 'Recovered from stale in-progress state on startup',
                 updated_at = strftime('%s', 'now')
             WHERE state IN ('claimed', 'in_flight')
               AND attempts < max_attempts",
            [],
        )?;
        conn.execute(
            "UPDATE work_items
             SET state = 'failed',
                 error_class = 'permanent',
                 next_retry_at = NULL,
                 last_error = coalesce(last_error || '; ', '') || 'Exceeded max attempts during startup recovery',
                 updated_at = strftime('%s', 'now')
             WHERE state IN ('claimed', 'in_flight')
               AND attempts >= max_attempts",
            [],
        )?;
        Ok(())
    }

    pub fn enqueue_detected(
        &self,
        flow: &str,
        task_type: &str,
        item_id: &str,
        idempotency_key: &str,
        payload_json: &Value,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "INSERT OR IGNORE INTO work_items (flow, task_type, item_id, idempotency_key, payload_json, state)
             VALUES (?1, ?2, ?3, ?4, ?5, 'detected')",
            params![
                flow,
                task_type,
                item_id,
                idempotency_key,
                serde_json::to_string(payload_json)?
            ],
        )?;
        Ok(())
    }

    pub fn move_detected_to_queued(&self, idempotency_key: &str) -> Result<bool> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let changed = conn.execute(
            "UPDATE work_items
             SET state='queued', updated_at=strftime('%s', 'now')
             WHERE idempotency_key = ?1 AND state = 'detected'",
            [idempotency_key],
        )?;
        Ok(changed > 0)
    }

    pub fn mark_in_flight(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE work_items
             SET state='in_flight', updated_at=strftime('%s', 'now')
             WHERE id=?1",
            [id],
        )?;
        Ok(())
    }

    pub fn mark_succeeded(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE work_items
             SET state='succeeded',
                 error_class=NULL,
                 last_error=NULL,
                 next_retry_at=NULL,
                 updated_at=strftime('%s', 'now')
             WHERE id=?1",
            [id],
        )?;
        Ok(())
    }

    pub fn get_checkpoint_u64(&self, key: &str) -> Result<Option<u64>> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let v: Option<String> = conn
            .query_row(
                "SELECT checkpoint_value FROM checkpoints WHERE checkpoint_key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()?;
        match v {
            Some(v) => Ok(Some(v.parse().context("checkpoint is not u64")?)),
            None => Ok(None),
        }
    }

    pub fn set_checkpoint_u64(&self, key: &str, value: u64) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "INSERT INTO checkpoints (checkpoint_key, checkpoint_value, updated_at)
             VALUES (?1, ?2, strftime('%s', 'now'))
             ON CONFLICT(checkpoint_key) DO UPDATE
               SET checkpoint_value=excluded.checkpoint_value, updated_at=excluded.updated_at",
            params![key, value.to_string()],
        )?;
        Ok(())
    }

    pub fn status(&self, filter: StateFilter) -> Result<Vec<StatusRow>> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut query = "SELECT id, flow, task_type, item_id, payload_json, state, attempts, max_attempts, next_retry_at, error_class, last_error, created_at, updated_at
                         FROM work_items"
            .to_string();
        let mut clauses = Vec::new();
        let mut params: Vec<Box<dyn ToSql>> = Vec::new();

        if let Some(flow) = filter.flow {
            clauses.push("flow = ?".to_string());
            params.push(Box::new(flow));
        }
        if let Some(state) = filter.state {
            clauses.push("state = ?".to_string());
            params.push(Box::new(state.to_string()));
        }
        if let Some(item_id) = filter.item_id {
            clauses.push("item_id = ?".to_string());
            params.push(Box::new(item_id));
        }
        if !clauses.is_empty() {
            query.push_str(" WHERE ");
            query.push_str(&clauses.join(" AND "));
        }
        query.push_str(" ORDER BY created_at DESC, id DESC LIMIT ?");
        params.push(Box::new(filter.limit as i64));
        let params_ref: Vec<&dyn ToSql> = params.iter().map(|p| p.as_ref() as &dyn ToSql).collect();

        let mut stmt = conn.prepare(&query)?;
        let rows = stmt.query_map(params_ref.as_slice(), |row| {
            let payload_str: String = row.get(4)?;
            let payload = serde_json::from_str::<Value>(&payload_str).unwrap_or(Value::Null);
            let state_str: String = row.get(5)?;
            let (direction, transfer_type, amount_raw, beneficiary, counterparty) =
                Self::extract_transfer_fields(
                    row.get::<_, String>(1)?.as_str(),
                    row.get::<_, String>(2)?.as_str(),
                    &payload,
                );
            Ok(StatusRow {
                id: row.get(0)?,
                flow: row.get(1)?,
                task_type: row.get(2)?,
                item_id: row.get(3)?,
                direction,
                transfer_type,
                amount_raw,
                beneficiary,
                counterparty,
                status: state_str.parse().unwrap_or(WorkState::Failed),
                attempts: row.get(6)?,
                max_attempts: row.get(7)?,
                next_retry_at: row.get(8)?,
                error_class: row.get(9)?,
                last_error: row.get(10)?,
                created_at: row.get(11)?,
                updated_at: row.get(12)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn clear_non_in_progress(&self) -> Result<usize> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let deleted = conn.execute(
            "DELETE FROM work_items
             WHERE state IN ('succeeded', 'failed')",
            [],
        )?;
        Ok(deleted)
    }

    pub fn clear_all(&self) -> Result<usize> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let deleted = conn.execute("DELETE FROM work_items", [])?;
        Ok(deleted)
    }

    pub fn list_work_items(
        &self,
        flow: &str,
        state: WorkState,
        limit: usize,
    ) -> Result<Vec<WorkItem>> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, flow, task_type, item_id, idempotency_key, payload_json, state, attempts, max_attempts, next_retry_at, last_attempt_at, error_class, last_error, created_at, updated_at
             FROM work_items
             WHERE flow = ?1 AND state = ?2
             ORDER BY created_at ASC, id ASC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![flow, state.to_string(), limit as i64],
            row_to_work_item,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn reset_in_flight_to_queued(&self, flow: &str, error: &str) -> Result<usize> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let updated = conn.execute(
            "UPDATE work_items
             SET state='queued',
                 error_class='transient',
                 last_error=?2,
                 updated_at=strftime('%s', 'now')
             WHERE state='in_flight' AND flow=?1",
            params![flow, error],
        )?;
        Ok(updated)
    }
}

fn json_value_to_string(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(v)) => Some(v.clone()),
        Some(Value::Number(v)) => Some(v.to_string()),
        Some(Value::Bool(v)) => Some(v.to_string()),
        _ => None,
    }
}

fn extract_lock_amount_hot(payload: &Value) -> Option<String> {
    if let Some(amount_hot) = payload.get("amount_hot").and_then(Value::as_str) {
        return Some(amount_hot.to_string());
    }
    let amount = payload
        .get("amount_raw_wei")
        .or_else(|| payload.get("amount"))?;
    let amount = json_value_to_string(Some(amount))?;
    if amount.contains('.') {
        return Some(amount);
    }
    if !amount.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // Legacy payload compatibility:
    // - long integer strings are interpreted as wei and converted to HOT
    // - shorter integer strings are treated as already-converted HOT
    if amount.len() >= 13 {
        return format_wei_as_hot(&amount);
    }
    Some(amount)
}

fn format_wei_as_hot(amount_wei: &str) -> Option<String> {
    let amount: U256 = amount_wei.parse().ok()?;
    let decimals = U256::from(10).pow(U256::from(18));
    let whole = amount / decimals;
    let frac = (amount % decimals) / U256::from(10).pow(U256::from(12));
    if frac.is_zero() {
        Some(whole.to_string())
    } else {
        Some(format!("{}.{:06}", whole, frac))
    }
}

impl StateStore {
    fn ensure_work_item_columns(&self, conn: &Connection) -> Result<()> {
        let mut stmt = conn.prepare("PRAGMA table_info(work_items)")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
        let cols: Vec<String> = rows.collect::<Result<Vec<_>, _>>()?;

        if !cols.iter().any(|c| c == "max_attempts") {
            conn.execute(
                "ALTER TABLE work_items ADD COLUMN max_attempts INTEGER NOT NULL DEFAULT 8",
                [],
            )?;
        }
        if !cols.iter().any(|c| c == "next_retry_at") {
            conn.execute(
                "ALTER TABLE work_items ADD COLUMN next_retry_at INTEGER",
                [],
            )?;
        }
        if !cols.iter().any(|c| c == "last_attempt_at") {
            conn.execute(
                "ALTER TABLE work_items ADD COLUMN last_attempt_at INTEGER",
                [],
            )?;
        }
        if !cols.iter().any(|c| c == "error_class") {
            conn.execute("ALTER TABLE work_items ADD COLUMN error_class TEXT", [])?;
        }
        Ok(())
    }
}

fn row_to_work_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkItem> {
    let payload: String = row.get(5)?;
    let state_str: String = row.get(6)?;
    Ok(WorkItem {
        id: row.get(0)?,
        flow: row.get(1)?,
        task_type: row.get(2)?,
        item_id: row.get(3)?,
        idempotency_key: row.get(4)?,
        payload_json: serde_json::from_str(&payload).unwrap_or(Value::Null),
        state: state_str.parse().unwrap_or(WorkState::Failed),
        attempts: row.get(7)?,
        max_attempts: row.get(8)?,
        next_retry_at: row.get(9)?,
        last_attempt_at: row.get(10)?,
        error_class: row.get(11)?,
        last_error: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

#[cfg(test)]
impl StateStore {
    pub fn enqueue_queued(
        &self,
        flow: &str,
        task_type: &str,
        item_id: &str,
        idempotency_key: &str,
        payload_json: &Value,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "INSERT OR IGNORE INTO work_items (flow, task_type, item_id, idempotency_key, payload_json, state, next_retry_at, max_attempts)
             VALUES (?1, ?2, ?3, ?4, ?5, 'queued', NULL, ?6)",
            params![
                flow,
                task_type,
                item_id,
                idempotency_key,
                serde_json::to_string(payload_json)?,
                DEFAULT_MAX_ATTEMPTS
            ],
        )?;
        Ok(())
    }

    pub fn claim_next(&self, preferred_flow: Option<&str>) -> Result<Option<WorkItem>> {
        let mut conn = self.conn.lock().expect("db mutex poisoned");
        let tx = conn.transaction()?;

        let next_id = if let Some(flow) = preferred_flow {
            tx.query_row(
                "SELECT id FROM work_items
                 WHERE state='queued' AND flow = ?1
                   AND (next_retry_at IS NULL OR next_retry_at <= strftime('%s', 'now'))
                   AND attempts < max_attempts
                 ORDER BY created_at ASC, id ASC LIMIT 1",
                [flow],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        } else {
            None
        }
        .or_else(|| {
            tx.query_row(
                "SELECT id FROM work_items
                 WHERE state='queued'
                   AND (next_retry_at IS NULL OR next_retry_at <= strftime('%s', 'now'))
                   AND attempts < max_attempts
                 ORDER BY created_at ASC, id ASC LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .ok()
            .flatten()
        });

        let Some(id) = next_id else {
            tx.commit()?;
            return Ok(None);
        };

        tx.execute(
            "UPDATE work_items
             SET state='claimed',
                 attempts=attempts+1,
                 last_attempt_at=strftime('%s', 'now'),
                 updated_at=strftime('%s', 'now')
             WHERE id = ?1 AND state='queued'",
            [id],
        )?;

        let item = tx
            .query_row(
                "SELECT id, flow, task_type, item_id, idempotency_key, payload_json, state, attempts, max_attempts, next_retry_at, last_attempt_at, error_class, last_error, created_at, updated_at
                 FROM work_items WHERE id = ?1",
                [id],
                row_to_work_item,
            )
            .optional()?;
        tx.commit()?;
        Ok(item)
    }

    pub fn schedule_retry(&self, id: i64, err: &str, next_retry_at: i64) -> Result<bool> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let updated = conn.execute(
            "UPDATE work_items
             SET state='queued',
                 error_class='transient',
                 last_error=?2,
                 next_retry_at=?3,
                 updated_at=strftime('%s', 'now')
             WHERE id=?1 AND state='in_flight' AND attempts < max_attempts",
            params![id, err, next_retry_at],
        )?;
        Ok(updated > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_db_path(name: &str) -> String {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("/tmp/bridge-orchestrator-{}-{}.db", name, ts)
    }

    fn insert_work_item_with_state(path: &str, item_id: &str, state: WorkState) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute(
            "INSERT INTO work_items (flow, task_type, item_id, idempotency_key, payload_json, state, attempts, max_attempts, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 8, strftime('%s', 'now'), strftime('%s', 'now'))",
            rusqlite::params![
                "lock",
                "create_parked_link",
                item_id,
                format!("{}:key", item_id),
                serde_json::json!({"lock_id": item_id}).to_string(),
                state.to_string()
            ],
        )
        .unwrap();
    }

    #[test]
    fn recovers_stale_in_progress_items_on_startup() {
        let path = test_db_path("recover");
        {
            let store = StateStore::open(&path).unwrap();
            store
                .enqueue_queued(
                    "lock",
                    "create_parked_link",
                    "lock:1",
                    "lock:1:create_parked_link",
                    &serde_json::json!({"lock_id":"1"}),
                )
                .unwrap();
            let item = store.claim_next(Some("lock")).unwrap().unwrap();
            store.mark_in_flight(item.id).unwrap();
        }
        let store = StateStore::open(&path).unwrap();
        let items = store
            .list_work_items("lock", WorkState::Queued, 10)
            .unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0]
            .last_error
            .clone()
            .unwrap_or_default()
            .contains("Recovered from stale in-progress state"));
    }

    #[test]
    fn claim_respects_next_retry_due_time() {
        let path = test_db_path("retry-due");
        let store = StateStore::open(&path).unwrap();
        store
            .enqueue_queued(
                "lock",
                "create_parked_link",
                "lock:1",
                "lock:1:create_parked_link",
                &serde_json::json!({"lock_id":"1"}),
            )
            .unwrap();

        let claimed = store.claim_next(Some("lock")).unwrap().unwrap();
        store.mark_in_flight(claimed.id).unwrap();
        let future = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;
        let scheduled = store
            .schedule_retry(claimed.id, "temporary outage", future)
            .unwrap();
        assert!(scheduled);
        assert!(store.claim_next(Some("lock")).unwrap().is_none());
    }

    #[test]
    fn startup_recovery_marks_exhausted_as_failed() {
        let path = test_db_path("recover-exhausted");
        {
            let store = StateStore::open(&path).unwrap();
            store
                .enqueue_queued(
                    "lock",
                    "create_parked_link",
                    "lock:1",
                    "lock:1:create_parked_link",
                    &serde_json::json!({"lock_id":"1"}),
                )
                .unwrap();

            for _ in 0..7 {
                let item = store.claim_next(Some("lock")).unwrap().unwrap();
                store.mark_in_flight(item.id).unwrap();
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64;
                assert!(store.schedule_retry(item.id, "temporary", now - 1).unwrap());
            }

            let item = store.claim_next(Some("lock")).unwrap().unwrap();
            assert_eq!(item.attempts, 8);
            store.mark_in_flight(item.id).unwrap();
        }

        let reopened = StateStore::open(&path).unwrap();
        let failed = reopened
            .list_work_items("lock", WorkState::Failed, 10)
            .unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].error_class.as_deref(), Some("permanent"));
        assert!(failed[0]
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("Exceeded max attempts during startup recovery"));
    }

    #[test]
    fn status_enriches_lock_transfer_fields() {
        let path = test_db_path("status-lock");
        let store = StateStore::open(&path).unwrap();
        store
            .enqueue_queued(
                "lock",
                "create_parked_link",
                "lock:100",
                "lock:100:create_parked_link",
                &serde_json::json!({
                    "lock_id": "100",
                    "sender": "0xabc123",
                    "amount": "2500000000000000000",
                    "amount_raw_wei": "2500000000000000000",
                    "amount_hot": "2.500000",
                    "holochain_agent": "uhCAkLockAgent"
                }),
            )
            .unwrap();

        let rows = store
            .status(StateFilter {
                flow: Some("lock".to_string()),
                state: None,
                item_id: Some("lock:100".to_string()),
                limit: 10,
            })
            .unwrap();

        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.direction.as_deref(), Some("transfer_in"));
        assert_eq!(row.transfer_type.as_deref(), Some("lock"));
        assert_eq!(row.amount_raw.as_deref(), Some("2.500000"));
        assert_eq!(row.beneficiary.as_deref(), Some("uhCAkLockAgent"));
        assert_eq!(row.counterparty.as_deref(), Some("0xabc123"));
        assert_eq!(row.status, WorkState::Queued);
    }

    #[test]
    fn status_lock_legacy_wei_amount_is_converted() {
        let path = test_db_path("status-lock-legacy-wei");
        let store = StateStore::open(&path).unwrap();
        store
            .enqueue_queued(
                "lock",
                "create_parked_link",
                "lock:legacy",
                "lock:legacy:create_parked_link",
                &serde_json::json!({
                    "lock_id": "legacy",
                    "sender": "0xsender",
                    "amount": "1000000000000000000",
                    "holochain_agent": "uhCAkLegacyAgent"
                }),
            )
            .unwrap();

        let rows = store
            .status(StateFilter {
                flow: Some("lock".to_string()),
                state: None,
                item_id: Some("lock:legacy".to_string()),
                limit: 10,
            })
            .unwrap();

        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.amount_raw.as_deref(), Some("1"));
    }

    #[test]
    fn status_does_not_mark_lock_initiate_deposit_as_transfer_in() {
        let path = test_db_path("status-lock-initiate");
        let store = StateStore::open(&path).unwrap();
        store
            .enqueue_queued(
                "lock",
                "initiate_deposit",
                "lock:200:initiate",
                "lock:200:initiate_deposit",
                &serde_json::json!({
                    "lock_id": "200",
                    "amount_hot": "4.000000"
                }),
            )
            .unwrap();

        let rows = store
            .status(StateFilter {
                flow: Some("lock".to_string()),
                state: None,
                item_id: Some("lock:200:initiate".to_string()),
                limit: 10,
            })
            .unwrap();

        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert!(row.direction.is_none());
        assert!(row.transfer_type.is_none());
        assert!(row.amount_raw.is_none());
    }

    #[test]
    fn clear_non_in_progress_deletes_only_terminal_rows() {
        let path = test_db_path("clear-non-in-progress");
        let store = StateStore::open(&path).unwrap();

        insert_work_item_with_state(&path, "lock:queued", WorkState::Queued);
        insert_work_item_with_state(&path, "lock:claimed", WorkState::Claimed);
        insert_work_item_with_state(&path, "lock:inflight", WorkState::InFlight);
        insert_work_item_with_state(&path, "lock:succeeded", WorkState::Succeeded);
        insert_work_item_with_state(&path, "lock:failed", WorkState::Failed);

        let deleted = store.clear_non_in_progress().unwrap();
        assert_eq!(deleted, 2);

        let rows = store
            .status(StateFilter {
                flow: Some("lock".to_string()),
                state: None,
                item_id: None,
                limit: 20,
            })
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| {
            matches!(
                row.status,
                WorkState::Queued | WorkState::Claimed | WorkState::InFlight
            )
        }));
    }

    #[test]
    fn clear_all_deletes_everything() {
        let path = test_db_path("clear-all");
        let store = StateStore::open(&path).unwrap();

        insert_work_item_with_state(&path, "lock:queued", WorkState::Queued);
        insert_work_item_with_state(&path, "lock:succeeded", WorkState::Succeeded);
        insert_work_item_with_state(&path, "lock:failed", WorkState::Failed);

        let deleted = store.clear_all().unwrap();
        assert_eq!(deleted, 3);

        let rows = store
            .status(StateFilter {
                flow: Some("lock".to_string()),
                state: None,
                item_id: None,
                limit: 20,
            })
            .unwrap();
        assert!(rows.is_empty());
    }
}
