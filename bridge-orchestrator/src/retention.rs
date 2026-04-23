//! Periodic retention task for the orchestrator's local SQLite.
//!
//! Runs as a detached `tokio::spawn` task — same pattern as
//! [`crate::watchtower_reporter`] — so any failure is logged and
//! swallowed without touching the bridge cycle. By default it wakes
//! once an hour, deletes terminal `work_items` rows older than the
//! per-state retention windows, and logs a single `info!` line if
//! anything was pruned.
//!
//! The task uses the writer mutex (DELETE is a write), but the
//! cadence is 1h and the SQL hits the `idx_work_items_state_created`
//! index — so each tick holds the mutex for well under a millisecond
//! in practice. If volume ever grows enough to matter we can switch
//! to `DELETE ... WHERE id IN (SELECT id ... LIMIT n)` batches.

use std::time::Duration;

use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};

use crate::config::RetentionConfig;
use crate::state::StateStore;

/// Spawn the retention loop. Returns the `JoinHandle` so callers can
/// `drop(...)` it (matches the reporter wiring and keeps the
/// `non_binding_let_on_future` clippy lint quiet).
///
/// When `cfg.enabled` is `false` the spawned task exits immediately,
/// so callers don't have to special-case the disabled path.
pub fn spawn(cfg: RetentionConfig, db: StateStore) -> JoinHandle<()> {
    tokio::spawn(async move {
        if !cfg.enabled {
            tracing::info!(
                event = "bridge_orchestrator.retention.disabled",
                "retention task disabled via BRIDGE_RETENTION_DISABLED"
            );
            return;
        }

        tracing::info!(
            event = "bridge_orchestrator.retention.started",
            tick_interval_ms = cfg.tick_interval_ms,
            succeeded_max_age_s = cfg.succeeded_max_age_s,
            failed_max_age_s = cfg.failed_max_age_s,
            "retention task started"
        );

        let mut tick = interval(Duration::from_millis(cfg.tick_interval_ms));
        // Skip missed ticks after a suspend / long stall; we only
        // care about the next scheduled opportunity, never about
        // catching up on a backlog of retention ticks.
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            tick.tick().await;
            run_tick(&cfg, &db).await;
        }
    })
}

async fn run_tick(cfg: &RetentionConfig, db: &StateStore) {
    let db_clone = db.clone();
    let succeeded_max_age_s = cfg.succeeded_max_age_s;
    let failed_max_age_s = cfg.failed_max_age_s;

    let join_result = tokio::task::spawn_blocking(move || {
        db_clone.prune_terminal_older_than(succeeded_max_age_s, failed_max_age_s)
    })
    .await;

    match join_result {
        Ok(Ok(stats)) if stats.total() > 0 => {
            tracing::info!(
                event = "bridge_orchestrator.retention.pruned",
                succeeded = stats.succeeded_deleted,
                failed = stats.failed_deleted,
                "pruned terminal work_items rows"
            );
        }
        Ok(Ok(_)) => {
            // Nothing to prune this tick — emit at trace level so
            // steady-state runs stay quiet but the path is
            // observable when --trace is enabled.
            tracing::trace!(
                event = "bridge_orchestrator.retention.idle",
                "no terminal rows eligible for prune"
            );
        }
        Ok(Err(e)) => {
            tracing::warn!(
                event = "bridge_orchestrator.retention.prune_failed",
                error = %e,
                "retention prune failed; will retry next tick"
            );
        }
        Err(join_err) => {
            tracing::warn!(
                event = "bridge_orchestrator.retention.join_error",
                error = %join_err,
                "retention spawn_blocking join failed; will retry next tick"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::WorkState;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_db_path(name: &str) -> String {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("/tmp/bridge-orchestrator-retention-{}-{}.db", name, ts)
    }

    fn insert_row(path: &str, item_id: &str, state: WorkState) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute(
            "INSERT INTO work_items (flow, task_type, item_id, idempotency_key, payload_json, state, attempts, max_attempts, created_at, updated_at)
             VALUES ('lock','create_parked_link',?1,?2,'{}',?3,0,8,strftime('%s','now'),strftime('%s','now'))",
            rusqlite::params![item_id, format!("{}:key", item_id), state.to_string()],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn disabled_config_short_circuits_immediately() {
        let path = test_db_path("disabled");
        let db = StateStore::open(&path).unwrap();
        let cfg = RetentionConfig {
            enabled: false,
            tick_interval_ms: 1,
            succeeded_max_age_s: 0,
            failed_max_age_s: 0,
        };
        let handle = spawn(cfg, db);
        // A disabled task returns immediately; if we can await the
        // handle without timing out, the short-circuit works.
        handle.await.expect("disabled task should join cleanly");
    }

    #[tokio::test]
    async fn enabled_tick_deletes_stale_rows() {
        let path = test_db_path("enabled");
        let db = StateStore::open(&path).unwrap();

        insert_row(&path, "retain:young-succ", WorkState::Succeeded);
        insert_row(&path, "retain:old-succ", WorkState::Succeeded);
        insert_row(&path, "retain:queued", WorkState::Queued);

        // Backdate the "old" row by 2 hours.
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE work_items SET updated_at = CAST(strftime('%s','now') AS INTEGER) - 7200
             WHERE item_id = 'retain:old-succ'",
            [],
        )
        .unwrap();
        drop(conn);

        // Succeeded window = 1h, failed window irrelevant (no failed rows).
        let cfg = RetentionConfig {
            enabled: true,
            tick_interval_ms: 5,
            succeeded_max_age_s: 3_600,
            failed_max_age_s: 3_600,
        };

        let handle = spawn(cfg, db.clone());
        // Give the task at least one tick to run.
        tokio::time::sleep(Duration::from_millis(60)).await;
        handle.abort();
        let _ = handle.await;

        let stats = db.aggregate_stats().unwrap();
        // Young succeeded should remain; queued should remain; old succeeded deleted.
        assert_eq!(stats.succeeded_total, 1);
        assert_eq!(stats.queued, 1);
    }
}
