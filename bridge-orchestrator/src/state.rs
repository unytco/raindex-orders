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

/// Pipeline progress for a lock row. Independent of `WorkState` (which tracks
/// orchestrator ownership); `step` tracks which zome calls in the four-stage
/// bridge pipeline have been proven to have landed on-chain.
///
/// Advancement is always driven by chain truth — a row is only moved to a
/// later step once we either observed the returned `ActionHash` from the
/// relevant zome call, or the reconciler at the top of a cycle confirmed the
/// expected side-effect against a fresh `get_parked_links_by_ea` probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum WorkStep {
    /// No on-chain work observed yet; eligible for S1 (`create_parked_link`).
    New,
    /// S1 landed; `cl_link_hash` references the live parked link on the
    /// credit-limit EA.
    ClLinkCreated,
    /// S2 landed; the CL `execute_rave` has consumed the parked link.
    ClRaveExecuted,
    /// S3 landed; `br_spend_hash` references the live parked spend on the
    /// bridging EA.
    BrSpendCreated,
    /// S4 landed; the bridging `execute_rave` has consumed the parked spend.
    /// Terminal for the pipeline — the row is simultaneously marked
    /// `state='succeeded'`.
    BrRaveExecuted,
}

impl std::fmt::Display for WorkStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let v = match self {
            WorkStep::New => "new",
            WorkStep::ClLinkCreated => "cl_link_created",
            WorkStep::ClRaveExecuted => "cl_rave_executed",
            WorkStep::BrSpendCreated => "br_spend_created",
            WorkStep::BrRaveExecuted => "br_rave_executed",
        };
        write!(f, "{}", v)
    }
}

impl std::str::FromStr for WorkStep {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "new" => Ok(Self::New),
            "cl_link_created" => Ok(Self::ClLinkCreated),
            "cl_rave_executed" => Ok(Self::ClRaveExecuted),
            "br_spend_created" => Ok(Self::BrSpendCreated),
            "br_rave_executed" => Ok(Self::BrRaveExecuted),
            _ => Err(format!("Unknown step: {}", s)),
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
    pub step: WorkStep,
    pub cl_link_hash: Option<String>,
    pub cl_rave_hash: Option<String>,
    pub br_spend_hash: Option<String>,
    pub br_rave_hash: Option<String>,
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

/// Status-row transfer fields derived from a work-item payload.
/// Returned as a named struct (instead of a 5-tuple) so the status
/// endpoint's builder can use field access and clippy's
/// `type_complexity` lint stays green. All fields are `Option<String>`
/// because they are absent for non-lock/non-create_parked_link rows.
#[derive(Default)]
struct TransferFields {
    direction: Option<String>,
    transfer_type: Option<String>,
    amount_raw: Option<String>,
    beneficiary: Option<String>,
    counterparty: Option<String>,
}

impl StateStore {
    fn extract_transfer_fields(flow: &str, task_type: &str, payload: &Value) -> TransferFields {
        if flow == "lock" && task_type == "create_parked_link" {
            return TransferFields {
                direction: Some("transfer_in".to_string()),
                transfer_type: Some("lock".to_string()),
                amount_raw: extract_lock_amount_hot(payload),
                beneficiary: payload
                    .get("holochain_agent")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                counterparty: payload
                    .get("sender")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            };
        }

        TransferFields::default()
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
        // Bump attempts for every row we recover so a subsequent cycle can
        // detect the retry and run the source-chain dedup. Must happen before
        // the max_attempts check so rows that just crossed the threshold this
        // recovery are correctly promoted to 'failed'.
        conn.execute(
            "UPDATE work_items
             SET attempts = attempts + 1,
                 updated_at = strftime('%s', 'now')
             WHERE state IN ('claimed', 'in_flight')",
            [],
        )?;
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
            let fields = Self::extract_transfer_fields(
                row.get::<_, String>(1)?.as_str(),
                row.get::<_, String>(2)?.as_str(),
                &payload,
            );
            Ok(StatusRow {
                id: row.get(0)?,
                flow: row.get(1)?,
                task_type: row.get(2)?,
                item_id: row.get(3)?,
                direction: fields.direction,
                transfer_type: fields.transfer_type,
                amount_raw: fields.amount_raw,
                beneficiary: fields.beneficiary,
                counterparty: fields.counterparty,
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
            "SELECT id, flow, task_type, item_id, idempotency_key, payload_json, state, attempts, max_attempts, next_retry_at, last_attempt_at, error_class, last_error, created_at, updated_at, step, cl_link_hash, cl_rave_hash, br_spend_hash, br_rave_hash
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

    /// List all non-terminal rows (state IN `queued` or `in_flight`) for the
    /// given flow at the given pipeline step, ordered oldest-first. This is
    /// the core query used by the step-driven bridge cycle: each stage
    /// (S1..S4) selects its input by `step` value rather than by a
    /// dedup-derived decision.
    pub fn list_pending_by_step(
        &self,
        flow: &str,
        step: WorkStep,
        limit: usize,
    ) -> Result<Vec<WorkItem>> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, flow, task_type, item_id, idempotency_key, payload_json, state, attempts, max_attempts, next_retry_at, last_attempt_at, error_class, last_error, created_at, updated_at, step, cl_link_hash, cl_rave_hash, br_spend_hash, br_rave_hash
             FROM work_items
             WHERE flow = ?1 AND step = ?2 AND state IN ('queued', 'in_flight')
             ORDER BY created_at ASC, id ASC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![flow, step.to_string(), limit as i64],
            row_to_work_item,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Advance a row to `step='cl_link_created'`, recording the ActionHash
    /// returned by `create_parked_link`. `state` is reset to `queued` so the
    /// row is eligible for the next stage.
    pub fn advance_to_cl_link_created(&self, id: i64, cl_link_hash: &str) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE work_items
             SET step='cl_link_created',
                 cl_link_hash=?2,
                 state='queued',
                 attempts=0,
                 error_class=NULL,
                 last_error=NULL,
                 next_retry_at=NULL,
                 updated_at=strftime('%s', 'now')
             WHERE id=?1",
            params![id, cl_link_hash],
        )?;
        Ok(())
    }

    /// Advance a row to `step='cl_rave_executed'`. `cl_rave_hash` is optional
    /// — we record it when we observed the RAVE's returned ActionHash
    /// directly, and leave it NULL when the reconciler inferred the advance
    /// from the parked link no longer being live.
    pub fn advance_to_cl_rave_executed(&self, id: i64, cl_rave_hash: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE work_items
             SET step='cl_rave_executed',
                 cl_rave_hash=?2,
                 state='queued',
                 attempts=0,
                 error_class=NULL,
                 last_error=NULL,
                 next_retry_at=NULL,
                 updated_at=strftime('%s', 'now')
             WHERE id=?1",
            params![id, cl_rave_hash],
        )?;
        Ok(())
    }

    /// Advance a row to `step='br_spend_created'`, recording the ActionHash
    /// returned by `create_parked_spend`.
    pub fn advance_to_br_spend_created(&self, id: i64, br_spend_hash: &str) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE work_items
             SET step='br_spend_created',
                 br_spend_hash=?2,
                 state='queued',
                 attempts=0,
                 error_class=NULL,
                 last_error=NULL,
                 next_retry_at=NULL,
                 updated_at=strftime('%s', 'now')
             WHERE id=?1",
            params![id, br_spend_hash],
        )?;
        Ok(())
    }

    /// Advance a row to `step='br_rave_executed'` and simultaneously mark it
    /// `state='succeeded'` — the bridging RAVE is the terminal stage of the
    /// lock pipeline.
    pub fn advance_to_br_rave_executed(&self, id: i64, br_rave_hash: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE work_items
             SET step='br_rave_executed',
                 br_rave_hash=?2,
                 state='succeeded',
                 attempts=0,
                 error_class=NULL,
                 last_error=NULL,
                 next_retry_at=NULL,
                 updated_at=strftime('%s', 'now')
             WHERE id=?1",
            params![id, br_rave_hash],
        )?;
        Ok(())
    }

    /// Terminally fail a single row with `error_class='permanent'`. Used
    /// by the cycle for per-lock failure modes that cannot possibly succeed
    /// on retry (malformed payload, tag-size estimation bug, or a single
    /// proof that is structurally larger than the link tag cap).
    pub fn mark_failed_permanent(&self, id: i64, error: &str) -> Result<()> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE work_items
             SET state='failed',
                 error_class='permanent',
                 last_error=?2,
                 next_retry_at=NULL,
                 updated_at=strftime('%s', 'now')
             WHERE id=?1",
            params![id, error],
        )?;
        Ok(())
    }

    /// Promote any `queued` rows that have already exhausted their retry
    /// budget to `failed` with `error_class='permanent'`. Intended to be
    /// called at the top of each cycle so a broken lock cannot loop
    /// forever in a long-running session (the `recover_stale_items`
    /// equivalent only runs on startup).
    pub fn fail_exhausted_queued(&self, flow: &str) -> Result<usize> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let updated = conn.execute(
            "UPDATE work_items
             SET state='failed',
                 error_class='permanent',
                 last_error = coalesce(last_error || '; ', '') || 'Exceeded max attempts in-cycle',
                 next_retry_at=NULL,
                 updated_at=strftime('%s', 'now')
             WHERE flow=?1 AND state='queued' AND attempts >= max_attempts",
            params![flow],
        )?;
        Ok(updated)
    }

    pub fn reset_in_flight_to_queued(&self, flow: &str, error: &str) -> Result<usize> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        // Bump attempts so the next cycle knows this lock has been tried at
        // least once; the bridge orchestrator uses attempts > 0 as the gate
        // for the expensive RAVE-history dedup scan.
        let updated = conn.execute(
            "UPDATE work_items
             SET state='queued',
                 attempts=attempts+1,
                 error_class='transient',
                 last_error=?2,
                 last_attempt_at=strftime('%s', 'now'),
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
        if !cols.iter().any(|c| c == "step") {
            conn.execute(
                "ALTER TABLE work_items ADD COLUMN step TEXT NOT NULL DEFAULT 'new'",
                [],
            )?;
        }
        if !cols.iter().any(|c| c == "cl_link_hash") {
            conn.execute("ALTER TABLE work_items ADD COLUMN cl_link_hash TEXT", [])?;
        }
        if !cols.iter().any(|c| c == "cl_rave_hash") {
            conn.execute("ALTER TABLE work_items ADD COLUMN cl_rave_hash TEXT", [])?;
        }
        if !cols.iter().any(|c| c == "br_spend_hash") {
            conn.execute("ALTER TABLE work_items ADD COLUMN br_spend_hash TEXT", [])?;
        }
        if !cols.iter().any(|c| c == "br_rave_hash") {
            conn.execute("ALTER TABLE work_items ADD COLUMN br_rave_hash TEXT", [])?;
        }
        Ok(())
    }
}

fn row_to_work_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkItem> {
    let payload: String = row.get(5)?;
    let state_str: String = row.get(6)?;
    let step_str: String = row.get(15)?;
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
        step: step_str.parse().unwrap_or(WorkStep::New),
        cl_link_hash: row.get(16)?,
        cl_rave_hash: row.get(17)?,
        br_spend_hash: row.get(18)?,
        br_rave_hash: row.get(19)?,
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
                "SELECT id, flow, task_type, item_id, idempotency_key, payload_json, state, attempts, max_attempts, next_retry_at, last_attempt_at, error_class, last_error, created_at, updated_at, step, cl_link_hash, cl_rave_hash, br_spend_hash, br_rave_hash
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

    fn insert_work_item_with_attempts(
        path: &str,
        item_id: &str,
        state: WorkState,
        attempts: i64,
        max_attempts: i64,
    ) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute(
            "INSERT INTO work_items (flow, task_type, item_id, idempotency_key, payload_json, state, attempts, max_attempts, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, strftime('%s', 'now'), strftime('%s', 'now'))",
            rusqlite::params![
                "lock",
                "create_parked_link",
                item_id,
                format!("{}:key", item_id),
                serde_json::json!({"lock_id": item_id}).to_string(),
                state.to_string(),
                attempts,
                max_attempts,
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
    fn recover_stale_items_bumps_attempts() {
        // Guards the invariant that `attempts > 0` reliably means "this lock
        // has been tried before", which is the gate the bridge orchestrator
        // uses to decide whether to run the expensive RAVE-history dedup
        // scan. Historically the prod binary never incremented `attempts`
        // because the only caller lived inside `#[cfg(test)] claim_next`,
        // so every row read 0 forever.
        let path = test_db_path("recover-bumps-attempts");
        let item_id = {
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
            assert_eq!(item.attempts, 1, "claim_next should bump attempts to 1");
            store.mark_in_flight(item.id).unwrap();
            item.id
        };

        let store = StateStore::open(&path).unwrap();
        let items = store
            .list_work_items("lock", WorkState::Queued, 10)
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].id, item_id,
            "recovered item should be the same row we stashed"
        );
        assert_eq!(
            items[0].attempts, 2,
            "recover_stale_items must bump attempts so retry gate can fire"
        );
    }

    #[test]
    fn reset_in_flight_to_queued_bumps_attempts() {
        // Per-cycle error path (as opposed to startup crash-recovery). The
        // bridge orchestrator calls `reset_in_flight_to_queued` when a cycle
        // fails mid-write; the next cycle uses `attempts > 0` to decide it
        // needs to scan applied RAVE history before issuing any
        // `create_parked_*` call, so the bump *must* happen here.
        let path = test_db_path("reset-bumps-attempts");
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
        assert_eq!(item.attempts, 1);
        store.mark_in_flight(item.id).unwrap();

        let affected = store
            .reset_in_flight_to_queued("lock", "simulated cycle error")
            .unwrap();
        assert_eq!(affected, 1, "one in_flight row should have been reset");

        let items = store
            .list_work_items("lock", WorkState::Queued, 10)
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].attempts, 2,
            "reset_in_flight_to_queued must bump attempts so retry gate can fire"
        );
        assert_eq!(
            items[0].last_error.as_deref(),
            Some("simulated cycle error"),
            "last_error should carry the cycle error context"
        );
    }

    #[test]
    fn mark_failed_permanent_sets_permanent_error_class() {
        // Per-lock terminal failure helper used by the cycle whenever a
        // single lock cannot be processed on any retry (malformed payload,
        // tag-size encoder bug, oversize proof, etc).
        let path = test_db_path("mark-failed-permanent");
        let store = StateStore::open(&path).unwrap();
        store
            .enqueue_queued(
                "lock",
                "create_parked_link",
                "lock:1",
                "lock:1:create_parked_link",
                &serde_json::json!({"lock_id": "1"}),
            )
            .unwrap();

        let item = store.claim_next(Some("lock")).unwrap().unwrap();
        store
            .mark_failed_permanent(item.id, "proof extraction failed: bogus payload")
            .unwrap();

        let failed = store
            .list_work_items("lock", WorkState::Failed, 10)
            .unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].id, item.id);
        assert_eq!(failed[0].error_class.as_deref(), Some("permanent"));
        assert_eq!(
            failed[0].last_error.as_deref(),
            Some("proof extraction failed: bogus payload")
        );
        assert_eq!(failed[0].next_retry_at, None);

        let queued = store
            .list_work_items("lock", WorkState::Queued, 10)
            .unwrap();
        assert!(queued.is_empty());
    }

    #[test]
    fn fail_exhausted_queued_promotes_rows_over_cap() {
        // Per-cycle safety valve: queued rows whose `attempts` have already
        // reached `max_attempts` must be promoted to `failed` so they don't
        // keep re-entering the deep dedup scan every cycle.
        let path = test_db_path("fail-exhausted-over-cap");
        let store = StateStore::open(&path).unwrap();

        insert_work_item_with_attempts(&path, "lock:over", WorkState::Queued, 8, 8);
        insert_work_item_with_attempts(&path, "lock:equal-over", WorkState::Queued, 9, 8);
        insert_work_item_with_attempts(&path, "lock:under", WorkState::Queued, 3, 8);

        let promoted = store.fail_exhausted_queued("lock").unwrap();
        assert_eq!(promoted, 2, "both at-cap and over-cap rows should promote");

        let failed = store
            .list_work_items("lock", WorkState::Failed, 10)
            .unwrap();
        assert_eq!(failed.len(), 2);
        for row in &failed {
            assert_eq!(row.error_class.as_deref(), Some("permanent"));
            assert!(row
                .last_error
                .as_deref()
                .unwrap_or_default()
                .contains("Exceeded max attempts in-cycle"));
        }

        // The within-budget row stays queued and keeps its attempts intact.
        let queued = store
            .list_work_items("lock", WorkState::Queued, 10)
            .unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].item_id, "lock:under");
        assert_eq!(queued[0].attempts, 3);
    }

    #[test]
    fn fail_exhausted_queued_leaves_other_states_alone() {
        // Only queued rows can be promoted — in_flight / claimed must wait
        // for `recover_stale_items` / `reset_in_flight_to_queued`, and
        // terminal states are untouched.
        let path = test_db_path("fail-exhausted-state-scope");
        let store = StateStore::open(&path).unwrap();

        insert_work_item_with_attempts(&path, "lock:in-flight", WorkState::InFlight, 8, 8);
        insert_work_item_with_attempts(&path, "lock:claimed", WorkState::Claimed, 8, 8);
        insert_work_item_with_attempts(&path, "lock:succeeded", WorkState::Succeeded, 8, 8);
        insert_work_item_with_attempts(&path, "lock:failed", WorkState::Failed, 8, 8);

        let promoted = store.fail_exhausted_queued("lock").unwrap();
        assert_eq!(
            promoted, 0,
            "only queued rows are in scope; other states must not be touched"
        );

        for (state, expected) in [
            (WorkState::InFlight, 1),
            (WorkState::Claimed, 1),
            (WorkState::Succeeded, 1),
            (WorkState::Failed, 1),
        ] {
            let rows = store.list_work_items("lock", state.clone(), 10).unwrap();
            assert_eq!(
                rows.len(),
                expected,
                "state {:?} should be unchanged",
                state
            );
        }
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

    // -----------------------------------------------------------------
    // Per-step pipeline tracking (plan: "per-step lock tracking")
    // -----------------------------------------------------------------

    /// Helper: queue a fresh lock row and return its DB id.
    fn enqueue_one(store: &StateStore, item_id: &str) -> i64 {
        store
            .enqueue_queued(
                "lock",
                "create_parked_link",
                item_id,
                &format!("{}:key", item_id),
                &serde_json::json!({"lock_id": item_id}),
            )
            .unwrap();
        store
            .list_work_items("lock", WorkState::Queued, 10)
            .unwrap()
            .into_iter()
            .find(|w| w.item_id == item_id)
            .expect("just-inserted row must be listable")
            .id
    }

    #[test]
    fn new_row_defaults_to_step_new_with_no_hashes() {
        // Schema migration invariant: every freshly-enqueued row lands at
        // `step='new'` with all pipeline hashes NULL. This is the state
        // the S1 batch builder expects for every lock it processes.
        let path = test_db_path("step-default");
        let store = StateStore::open(&path).unwrap();
        let id = enqueue_one(&store, "lock:step-default:1");
        let items = store
            .list_work_items("lock", WorkState::Queued, 10)
            .unwrap();
        let row = items.into_iter().find(|w| w.id == id).unwrap();
        assert_eq!(row.step, WorkStep::New);
        assert!(row.cl_link_hash.is_none());
        assert!(row.cl_rave_hash.is_none());
        assert!(row.br_spend_hash.is_none());
        assert!(row.br_rave_hash.is_none());
    }

    #[test]
    fn list_pending_by_step_filters_on_step_and_nonterminal_state() {
        // The S1..S3 batch builders each call `list_pending_by_step` with
        // the step they expect as input. Confirm it (a) matches exact
        // step, (b) ignores terminal (`succeeded`/`failed`) rows, and
        // (c) returns both `queued` and `in_flight` rows — because a
        // freshly-marked in_flight row from earlier in the same cycle is
        // still valid input until the cycle's outer error handler resets
        // it.
        let path = test_db_path("list-pending-by-step");
        let store = StateStore::open(&path).unwrap();
        let id_new1 = enqueue_one(&store, "lock:s:new:1");
        let id_new2 = enqueue_one(&store, "lock:s:new:2");
        let id_cl = enqueue_one(&store, "lock:s:cl");
        let id_done = enqueue_one(&store, "lock:s:done");

        store.advance_to_cl_link_created(id_cl, "uhCkkCL").unwrap();
        store
            .advance_to_cl_link_created(id_done, "uhCkkDONE")
            .unwrap();
        store
            .advance_to_cl_rave_executed(id_done, Some("uhCkkRAVE1"))
            .unwrap();
        store
            .advance_to_br_spend_created(id_done, "uhCkkSPEND")
            .unwrap();
        store
            .advance_to_br_rave_executed(id_done, Some("uhCkkRAVE2"))
            .unwrap();

        // Mark one of the 'new' rows in_flight to prove it's still
        // returned (the cycle still owns the batch until it either
        // advances or errors out).
        store.mark_in_flight(id_new2).unwrap();

        let new_rows = store
            .list_pending_by_step("lock", WorkStep::New, 100)
            .unwrap();
        let new_ids: Vec<i64> = new_rows.iter().map(|r| r.id).collect();
        assert_eq!(new_ids.len(), 2);
        assert!(new_ids.contains(&id_new1));
        assert!(new_ids.contains(&id_new2));

        let cl_rows = store
            .list_pending_by_step("lock", WorkStep::ClLinkCreated, 100)
            .unwrap();
        assert_eq!(cl_rows.len(), 1);
        assert_eq!(cl_rows[0].id, id_cl);
        assert_eq!(cl_rows[0].cl_link_hash.as_deref(), Some("uhCkkCL"));

        // The fully-advanced row is `state='succeeded'`; it must not
        // appear at any step lookup, even though its `step` column is
        // `br_rave_executed`.
        let done_rows = store
            .list_pending_by_step("lock", WorkStep::BrRaveExecuted, 100)
            .unwrap();
        assert!(done_rows.is_empty());
    }

    #[test]
    fn advance_to_cl_link_created_records_hash_and_clears_error() {
        // Invariant: every step advance clears the row's last error and
        // resets state to `queued` so the next stage can pick it up.
        // Without this the in_flight row stuck halfway through a cycle
        // would keep its stale error text forever.
        let path = test_db_path("advance-cl-link");
        let store = StateStore::open(&path).unwrap();
        let id = enqueue_one(&store, "lock:cl-link:1");
        store.mark_in_flight(id).unwrap();
        store.schedule_retry(id, "boom", 0).unwrap();

        store.advance_to_cl_link_created(id, "uhCkkABC123").unwrap();

        let row = store
            .list_pending_by_step("lock", WorkStep::ClLinkCreated, 10)
            .unwrap()
            .into_iter()
            .find(|w| w.id == id)
            .unwrap();
        assert_eq!(row.step, WorkStep::ClLinkCreated);
        assert_eq!(row.state, WorkState::Queued);
        assert_eq!(row.cl_link_hash.as_deref(), Some("uhCkkABC123"));
        assert!(
            row.last_error.is_none(),
            "error text must be cleared on advance"
        );
        assert!(row.next_retry_at.is_none());
    }

    #[test]
    fn advance_helpers_reset_attempts_so_each_step_gets_fresh_retry_budget() {
        // Each zome call in the four-stage pipeline is independently
        // retryable. If attempts carried over from S1 into S2..S4, a
        // row that makes forward progress could still exhaust its
        // `max_attempts` budget and get permanently failed while
        // actually being healthy. Every `advance_to_*` helper must
        // therefore reset `attempts` to zero.
        let path = test_db_path("advance-resets-attempts");
        let store = StateStore::open(&path).unwrap();

        // S1 → cl_link_created: burn two attempts first.
        let id_s1 = enqueue_one(&store, "lock:attempts:s1");
        let _ = store.claim_next(Some("lock")).unwrap().unwrap();
        let _ = store.claim_next(Some("lock")).unwrap();
        store.advance_to_cl_link_created(id_s1, "uhCkkS1").unwrap();
        let row = store
            .list_pending_by_step("lock", WorkStep::ClLinkCreated, 10)
            .unwrap()
            .into_iter()
            .find(|w| w.id == id_s1)
            .unwrap();
        assert_eq!(
            row.attempts, 0,
            "cl_link_created advance must reset attempts"
        );

        // S2 → cl_rave_executed: seed attempts via mark_in_flight + schedule_retry.
        let id_s2 = enqueue_one(&store, "lock:attempts:s2");
        store.advance_to_cl_link_created(id_s2, "uhCkkS2A").unwrap();
        store.mark_in_flight(id_s2).unwrap();
        store.schedule_retry(id_s2, "boom s2", 0).unwrap();
        store.advance_to_cl_rave_executed(id_s2, None).unwrap();
        let row = store
            .list_pending_by_step("lock", WorkStep::ClRaveExecuted, 10)
            .unwrap()
            .into_iter()
            .find(|w| w.id == id_s2)
            .unwrap();
        assert_eq!(
            row.attempts, 0,
            "cl_rave_executed advance must reset attempts"
        );

        // S3 → br_spend_created.
        let id_s3 = enqueue_one(&store, "lock:attempts:s3");
        store.advance_to_cl_link_created(id_s3, "uhCkkS3A").unwrap();
        store.advance_to_cl_rave_executed(id_s3, None).unwrap();
        store.mark_in_flight(id_s3).unwrap();
        store.schedule_retry(id_s3, "boom s3", 0).unwrap();
        store
            .advance_to_br_spend_created(id_s3, "uhCkkS3B")
            .unwrap();
        let row = store
            .list_pending_by_step("lock", WorkStep::BrSpendCreated, 10)
            .unwrap()
            .into_iter()
            .find(|w| w.id == id_s3)
            .unwrap();
        assert_eq!(
            row.attempts, 0,
            "br_spend_created advance must reset attempts"
        );

        // S4 → br_rave_executed (terminal).
        let id_s4 = enqueue_one(&store, "lock:attempts:s4");
        store.advance_to_cl_link_created(id_s4, "uhCkkS4A").unwrap();
        store.advance_to_cl_rave_executed(id_s4, None).unwrap();
        store
            .advance_to_br_spend_created(id_s4, "uhCkkS4B")
            .unwrap();
        store.mark_in_flight(id_s4).unwrap();
        store.schedule_retry(id_s4, "boom s4", 0).unwrap();
        store.advance_to_br_rave_executed(id_s4, None).unwrap();
        let row = store
            .list_work_items("lock", WorkState::Succeeded, 10)
            .unwrap()
            .into_iter()
            .find(|w| w.id == id_s4)
            .unwrap();
        assert_eq!(
            row.attempts, 0,
            "br_rave_executed advance must reset attempts"
        );
    }

    #[test]
    fn advance_to_cl_rave_executed_accepts_optional_hash() {
        // The reconciler advances rows whose link is no longer live
        // WITHOUT knowing the triggering RAVE's ActionHash, so the
        // advance helper must accept `None` and leave `cl_rave_hash`
        // NULL in that case. When the orchestrator advances after its
        // own successful RAVE, it passes `Some(hash)` and the column
        // gets populated for audit.
        let path = test_db_path("advance-cl-rave");
        let store = StateStore::open(&path).unwrap();
        let id_inferred = enqueue_one(&store, "lock:cl-rave:inferred");
        let id_observed = enqueue_one(&store, "lock:cl-rave:observed");

        for id in [id_inferred, id_observed] {
            store.advance_to_cl_link_created(id, "uhCkkLINK").unwrap();
        }
        store
            .advance_to_cl_rave_executed(id_inferred, None)
            .unwrap();
        store
            .advance_to_cl_rave_executed(id_observed, Some("uhCkkRAVE"))
            .unwrap();

        let rows = store
            .list_pending_by_step("lock", WorkStep::ClRaveExecuted, 10)
            .unwrap();
        let inferred = rows.iter().find(|w| w.id == id_inferred).unwrap();
        let observed = rows.iter().find(|w| w.id == id_observed).unwrap();
        assert!(inferred.cl_rave_hash.is_none());
        assert_eq!(observed.cl_rave_hash.as_deref(), Some("uhCkkRAVE"));
    }

    #[test]
    fn advance_to_br_rave_executed_marks_row_succeeded() {
        // Terminal stage: `br_rave_executed` and `state='succeeded'`
        // must be set atomically in the same UPDATE. Otherwise a crash
        // between advancing step and setting state would leave the row
        // eligible for S4 again and cause a duplicate RAVE on recovery.
        let path = test_db_path("advance-br-rave");
        let store = StateStore::open(&path).unwrap();
        let id = enqueue_one(&store, "lock:br-rave:1");
        store.advance_to_cl_link_created(id, "uhCkkA").unwrap();
        store
            .advance_to_cl_rave_executed(id, Some("uhCkkB"))
            .unwrap();
        store.advance_to_br_spend_created(id, "uhCkkC").unwrap();
        store
            .advance_to_br_rave_executed(id, Some("uhCkkD"))
            .unwrap();

        let succeeded = store
            .list_work_items("lock", WorkState::Succeeded, 10)
            .unwrap();
        let row = succeeded.into_iter().find(|w| w.id == id).unwrap();
        assert_eq!(row.step, WorkStep::BrRaveExecuted);
        assert_eq!(row.br_rave_hash.as_deref(), Some("uhCkkD"));
        assert_eq!(row.br_spend_hash.as_deref(), Some("uhCkkC"));
    }

    #[test]
    fn pipeline_row_survives_reopen_with_hashes_intact() {
        // Schema-migration + round-trip guard. A row that walked halfway
        // through the pipeline in one process must deserialise back to
        // the same `step`/hash tuple when the binary restarts, so the
        // reconciler can pick up exactly where the previous run left
        // off. This is the property that prevents the "we restarted and
        // re-ran the CL RAVE" duplicate-transaction symptom.
        let path = test_db_path("reopen-roundtrip");
        let id = {
            let store = StateStore::open(&path).unwrap();
            let id = enqueue_one(&store, "lock:reopen:1");
            store.advance_to_cl_link_created(id, "uhCkkLINK").unwrap();
            store
                .advance_to_cl_rave_executed(id, Some("uhCkkRAVE"))
                .unwrap();
            id
        };
        let store = StateStore::open(&path).unwrap();
        let rows = store
            .list_pending_by_step("lock", WorkStep::ClRaveExecuted, 10)
            .unwrap();
        let row = rows.into_iter().find(|w| w.id == id).unwrap();
        assert_eq!(row.step, WorkStep::ClRaveExecuted);
        assert_eq!(row.cl_link_hash.as_deref(), Some("uhCkkLINK"));
        assert_eq!(row.cl_rave_hash.as_deref(), Some("uhCkkRAVE"));
    }

    #[test]
    fn ensure_work_item_columns_adds_step_and_hash_columns_to_legacy_schema() {
        // Simulate upgrading a pre-plan database: drop the new columns
        // from the schema after they've been created, then re-run
        // `StateStore::open` which invokes `ensure_work_item_columns`.
        // Every pre-existing row must end up at `step='new'` (the
        // column default) and with NULL hashes, without any data loss.
        let path = test_db_path("migration");
        let store = StateStore::open(&path).unwrap();
        let _id = enqueue_one(&store, "lock:migrate:1");
        drop(store);

        // Simulate an older binary that never had these columns by
        // rebuilding the table without them.
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE work_items_legacy AS
                SELECT id, flow, task_type, item_id, idempotency_key, payload_json,
                       state, attempts, max_attempts, next_retry_at, last_attempt_at,
                       error_class, last_error, created_at, updated_at
                FROM work_items;
                DROP TABLE work_items;
                ALTER TABLE work_items_legacy RENAME TO work_items;
                "#,
            )
            .unwrap();
        }

        // Re-open: `ensure_work_item_columns` should re-add every
        // pipeline column without failing on the missing columns.
        let store = StateStore::open(&path).unwrap();
        let rows = store
            .list_work_items("lock", WorkState::Queued, 10)
            .unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.step, WorkStep::New);
        assert!(row.cl_link_hash.is_none());
        assert!(row.cl_rave_hash.is_none());
        assert!(row.br_spend_hash.is_none());
        assert!(row.br_rave_hash.is_none());
    }
}
