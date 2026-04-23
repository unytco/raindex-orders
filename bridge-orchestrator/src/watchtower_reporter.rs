//! Failure-isolated watchtower reporter.
//!
//! Posts a small, DNA-scoped health and throughput snapshot to the
//! watchtower Worker over HTTPS. Runs as a detached tokio task;
//! failures are logged and swallowed, never propagated into the bridge
//! cycle.
//!
//! Design constraints (user-driven):
//! - The reporter MUST NOT affect the bridge cycle. All work happens in
//!   a separate task with strict per-request timeouts and log-and-forget
//!   error handling.
//! - Payload is small (~1 KB) and collection is bounded: one aggregate
//!   SELECT plus a snapshot of in-memory health counters.
//! - The canonical string and header names mirror the existing watchtower
//!   observer (`{observer_id}\n{ts}\n{nonce}\n{body_sha256_hex}`) so the
//!   Worker can reuse its existing auth logic.

use crate::config::WatchtowerReporterConfig;
use crate::state::{BridgeAggregateStats, StateStore};
use anyhow::{Context, Result};
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const BINARY_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Per-request timeout for the HTTPS POST. Kept short so a hung Worker
/// can never pile up reporter ticks.
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Hard upper bound on a single tick (collect + sign + POST). Guards
/// against any unexpected combination of slow sqlite + slow HTTP.
const TICK_BUDGET: Duration = Duration::from_secs(20);

/// Minimum inter-tick sleep after a failed attempt. Ensures we never
/// busy-loop hammering a broken Worker even if config is wrong.
const MIN_BACKOFF_AFTER_ERROR: Duration = Duration::from_secs(15);

type HmacSha256 = Hmac<Sha256>;

/// In-memory health counters the orchestrator updates on cycle
/// transitions. Cheap to clone on read; the reporter never holds the
/// lock across I/O.
#[derive(Debug, Clone, Default)]
pub struct ReporterHealth {
    pub last_cycle_started_at_ms: Option<i64>,
    pub last_cycle_finished_at_ms: Option<i64>,
    pub last_cycle_duration_ms: Option<u64>,
    pub consecutive_failed_cycles: u32,
    pub reconnect_failures_total: u32,
    pub reconnects_ok_total: u32,
    pub pressure_active: bool,
    pub pressure_consecutive: u32,
    pub stage_ejections_total: u32,
    pub last_error: Option<String>,
    pub last_error_at_ms: Option<i64>,
}

/// Handle the orchestrator uses to publish health updates. Cloneable
/// (shared across tasks); updates are synchronous and bounded — they
/// only touch a small mutex-guarded struct, never I/O.
#[derive(Clone, Default)]
pub struct ReporterState {
    inner: Arc<Mutex<ReporterHealth>>,
    started_at: Arc<OnceInstant>,
}

/// One-shot initialized `Instant` used for uptime. `OnceLock` would
/// work too but `std::sync::OnceLock<Instant>` is annoyingly new; we
/// roll a tiny lazy container that works on stable.
#[derive(Default)]
struct OnceInstant {
    inner: std::sync::OnceLock<Instant>,
}

impl OnceInstant {
    fn get_or_init(&self) -> Instant {
        *self.inner.get_or_init(Instant::now)
    }
}

impl ReporterState {
    pub fn new() -> Self {
        let s = Self::default();
        // Prime the uptime anchor so every later read returns a
        // monotonic delta against startup.
        let _ = s.started_at.get_or_init();
        s
    }

    pub fn uptime_s(&self) -> u64 {
        self.started_at.get_or_init().elapsed().as_secs()
    }

    /// Synchronous-ish mutation. `try_lock` keeps us fully non-blocking:
    /// on the astronomically-rare case that another writer holds the
    /// mutex, we drop the update rather than stall the bridge cycle.
    pub fn update(&self, f: impl FnOnce(&mut ReporterHealth)) {
        if let Ok(mut guard) = self.inner.try_lock() {
            f(&mut guard);
        } else {
            tracing::trace!(
                event = "watchtower_reporter.update_contended",
                "skipped reporter-state update under contention"
            );
        }
    }

    async fn snapshot(&self) -> ReporterHealth {
        self.inner.lock().await.clone()
    }
}

/// On-the-wire payload. Kept tiny and forward-compatible; additional
/// optional fields can be added without bumping the schema version.
#[derive(Debug, Serialize)]
struct BridgePayload<'a> {
    schema_version: u32,
    observer_id: &'a str,
    collected_at: String,
    dna_b64: &'a str,
    self_health: PayloadSelfHealth<'a>,
    backlog: PayloadBacklog,
    throughput: PayloadThroughput,
}

#[derive(Debug, Serialize)]
struct PayloadSelfHealth<'a> {
    uptime_s: u64,
    binary_version: &'a str,
    last_cycle_at_iso: Option<String>,
    last_cycle_ms: Option<u64>,
    consecutive_failed_cycles: u32,
    reconnect_failures_total: u32,
    reconnects_ok_total: u32,
    pressure_active: bool,
    pressure_consecutive: u32,
    stage_ejections_total: u32,
    is_stuck: bool,
    last_error: Option<String>,
    last_error_at_iso: Option<String>,
}

#[derive(Debug, Serialize)]
struct PayloadBacklog {
    detected: i64,
    queued: i64,
    claimed: i64,
    in_flight: i64,
    succeeded_total: i64,
    failed_total: i64,
    oldest_queued_age_s: Option<i64>,
}

#[derive(Debug, Serialize)]
struct PayloadThroughput {
    succeeded_1h: i64,
    failed_1h: i64,
    succeeded_24h: i64,
    failed_24h: i64,
    avg_time_to_succeed_s_24h: Option<f64>,
}

/// Spawn the reporter in a detached tokio task. Returns immediately;
/// the returned `JoinHandle` is intentionally *not* awaited by the
/// caller, so any panic in the reporter cannot bring down the
/// orchestrator.
pub fn spawn(
    cfg: WatchtowerReporterConfig,
    state: ReporterState,
    db: StateStore,
    stuck_threshold_ms: u64,
) -> tokio::task::JoinHandle<()> {
    tracing::info!(
        event = "watchtower_reporter.spawned",
        observer_id = %cfg.observer_id,
        dna_b64 = %cfg.dna_b64,
        report_interval_ms = cfg.report_interval_ms,
        "watchtower reporter task started"
    );

    let client = match build_client() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                event = "watchtower_reporter.disabled",
                error = %e,
                "watchtower reporter disabled: failed to build http client"
            );
            return tokio::spawn(async {});
        }
    };

    tokio::spawn(async move {
        let interval = Duration::from_millis(cfg.report_interval_ms);
        loop {
            let tick_started = Instant::now();
            let outcome = tokio::time::timeout(
                TICK_BUDGET,
                run_tick(&cfg, &state, &db, &client, stuck_threshold_ms),
            )
            .await;

            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!(
                        event = "watchtower_reporter.tick_failed",
                        error = %e,
                        "reporter tick failed (ignored)"
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        event = "watchtower_reporter.tick_timeout",
                        budget_ms = TICK_BUDGET.as_millis() as u64,
                        "reporter tick exceeded budget (ignored)"
                    );
                }
            }

            let elapsed = tick_started.elapsed();
            let sleep_for = interval
                .checked_sub(elapsed)
                .unwrap_or(MIN_BACKOFF_AFTER_ERROR);
            tokio::time::sleep(sleep_for).await;
        }
    })
}

fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(HTTP_REQUEST_TIMEOUT)
        .connect_timeout(Duration::from_secs(5))
        .build()
        .context("build reqwest client")
}

async fn run_tick(
    cfg: &WatchtowerReporterConfig,
    state: &ReporterState,
    db: &StateStore,
    client: &reqwest::Client,
    stuck_threshold_ms: u64,
) -> Result<()> {
    // Run the SQL aggregate on a blocking thread. We deliberately open
    // a dedicated read-only sqlite connection per tick rather than
    // going through `StateStore::aggregate_stats`, which would take the
    // writer mutex and briefly contend with the bridge cycle. Opening
    // a fresh read-only connection is ~sub-millisecond against an
    // already-present db file, which is rounded-to-nothing next to our
    // 60s reporter period.
    let db_clone = db.clone();
    let stats: BridgeAggregateStats = tokio::task::spawn_blocking(move || -> Result<BridgeAggregateStats> {
        let conn = db_clone.open_read_only_connection()?;
        crate::state::compute_aggregate_stats(&conn)
    })
    .await
    .context("reporter: spawn_blocking join")?
    .context("reporter: aggregate_stats")?;

    let health = state.snapshot().await;
    let uptime_s = state.uptime_s();
    let now_ms = chrono::Utc::now().timestamp_millis();

    let is_stuck = matches!(
        health.last_cycle_finished_at_ms,
        Some(t) if now_ms.saturating_sub(t) as u64 > stuck_threshold_ms
    );

    let payload = BridgePayload {
        schema_version: cfg.schema_version,
        observer_id: &cfg.observer_id,
        collected_at: chrono::Utc::now().to_rfc3339(),
        dna_b64: &cfg.dna_b64,
        self_health: PayloadSelfHealth {
            uptime_s,
            binary_version: BINARY_VERSION,
            last_cycle_at_iso: health
                .last_cycle_started_at_ms
                .and_then(ms_to_rfc3339),
            last_cycle_ms: health.last_cycle_duration_ms,
            consecutive_failed_cycles: health.consecutive_failed_cycles,
            reconnect_failures_total: health.reconnect_failures_total,
            reconnects_ok_total: health.reconnects_ok_total,
            pressure_active: health.pressure_active,
            pressure_consecutive: health.pressure_consecutive,
            stage_ejections_total: health.stage_ejections_total,
            is_stuck,
            last_error: health.last_error.clone(),
            last_error_at_iso: health
                .last_error_at_ms
                .and_then(ms_to_rfc3339),
        },
        backlog: PayloadBacklog {
            detected: stats.detected,
            queued: stats.queued,
            claimed: stats.claimed,
            in_flight: stats.in_flight,
            succeeded_total: stats.succeeded_total,
            failed_total: stats.failed_total,
            oldest_queued_age_s: stats.oldest_queued_age_s,
        },
        throughput: PayloadThroughput {
            succeeded_1h: stats.succeeded_1h,
            failed_1h: stats.failed_1h,
            succeeded_24h: stats.succeeded_24h,
            failed_24h: stats.failed_24h,
            avg_time_to_succeed_s_24h: stats.avg_time_to_succeed_s_24h,
        },
    };

    post_signed(cfg, client, &payload).await
}

async fn post_signed(
    cfg: &WatchtowerReporterConfig,
    client: &reqwest::Client,
    payload: &BridgePayload<'_>,
) -> Result<()> {
    let body = serde_json::to_vec(payload).context("serialize payload")?;
    let ts = chrono::Utc::now().to_rfc3339();
    let nonce = uuid::Uuid::new_v4().to_string();
    let digest = body_digest_hex(&body);
    let canonical = canonical_string(&cfg.observer_id, &ts, &nonce, &digest);
    let secret = hex::decode(&cfg.hmac_secret_hex)
        .context("reporter: WATCHTOWER_HMAC_SECRET_HEX is not valid hex")?;
    let sig = sign(&secret, &canonical)?;

    let resp = client
        .post(&cfg.ingest_url)
        .header("x-watchtower-schema", cfg.schema_version.to_string())
        .header("x-watchtower-observer", &cfg.observer_id)
        .header("x-watchtower-ts", ts)
        .header("x-watchtower-nonce", nonce)
        .header("x-watchtower-sig", sig)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .context("reporter: POST failed")?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("reporter: ingest rejected: {status}: {text}");
    }
    tracing::debug!(
        event = "watchtower_reporter.posted",
        status = %status,
        "posted bridge snapshot"
    );
    Ok(())
}

fn body_digest_hex(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    hex::encode(hasher.finalize())
}

fn canonical_string(observer_id: &str, ts: &str, nonce: &str, body_sha: &str) -> String {
    format!("{observer_id}\n{ts}\n{nonce}\n{body_sha}")
}

fn sign(secret: &[u8], canonical: &str) -> Result<String> {
    let mut mac = HmacSha256::new_from_slice(secret).context("reporter: invalid HMAC key length")?;
    mac.update(canonical.as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn ms_to_rfc3339(ms: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).map(|t| t.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_string_matches_watchtower_format() {
        let c = canonical_string("obs-1", "2026-04-20T00:00:00Z", "nonce-xyz", "deadbeef");
        assert_eq!(c, "obs-1\n2026-04-20T00:00:00Z\nnonce-xyz\ndeadbeef");
    }

    #[test]
    fn body_digest_hex_is_sha256() {
        let d = body_digest_hex(b"");
        // Known empty SHA-256 digest.
        assert_eq!(
            d,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sign_produces_deterministic_hex() {
        let sig_a = sign(b"s3cret", "canonical").unwrap();
        let sig_b = sign(b"s3cret", "canonical").unwrap();
        assert_eq!(sig_a, sig_b);
        assert_eq!(sig_a.len(), 64); // 32 bytes hex-encoded
    }

    #[tokio::test]
    async fn reporter_state_updates_and_snapshots() {
        let state = ReporterState::new();
        state.update(|h| {
            h.consecutive_failed_cycles = 2;
            h.pressure_active = true;
        });
        let snap = state.snapshot().await;
        assert_eq!(snap.consecutive_failed_cycles, 2);
        assert!(snap.pressure_active);
    }
}
