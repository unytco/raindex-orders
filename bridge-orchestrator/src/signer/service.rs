//! `bridge-orchestrator signer`, one Safe owner's signer service
//! (docs/signer-service.md).

mod cell;
mod checks;
mod config;
mod records;
#[cfg(test)]
pub(crate) mod tests;

use super::safe::Refused;
use crate::orchestrator::ham_config;
use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use cell::{Cell, HamCell};
use checks::Refusal;
use config::ServiceConfig;
use records::Records;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tracing::info;

const HEALTH_WINDOW: Duration = Duration::from_secs(5);

struct Service {
    config: ServiceConfig,
    cell: Arc<dyn Cell>,
    records: Records,
    health: Arc<Mutex<Option<Health>>>,
}

struct Health {
    checked: Instant,
    healthy: bool,
}

pub async fn run() -> Result<()> {
    let config = ServiceConfig::from_env()?;
    // Refuses a conductor without an external lair before the records file is
    // created or the listener binds.
    ham_config(&config.holochain)?;
    let records = Records::open(&config.db_path)?;
    let mut shutdown = ham::install_shutdown_handler();
    let cell = Arc::new(HamCell::new(config.holochain.clone()));
    let connection = tokio::spawn({
        let (cell, shutdown) = (cell.clone(), shutdown.clone());
        async move { cell.keep_connected(shutdown).await }
    });
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| {
            format!(
                "SIGNER_BIND_ADDR and SIGNER_BIND_PORT {} cannot be bound",
                config.bind
            )
        })?;
    let bound = listener
        .local_addr()
        .context("the signer's listener has no local address")?;
    info!(
        event = "signer.started",
        owner = %config.key.address(),
        safe = %config.safe,
        network = config.network.name(),
        "signing as owner {} of Safe {} on {}, at {}. Screening is {}",
        config.key.address(),
        config.safe,
        config.network.name(),
        bound,
        config
            .screener
            .as_ref()
            .map_or("off".to_string(), |screener| format!("on: {screener}"))
    );
    let service = Service {
        config,
        cell,
        records,
        health: Arc::default(),
    };
    axum::serve(listener, router(Arc::new(service)))
        .with_graceful_shutdown(async move {
            let _ = shutdown.changed().await;
        })
        .await?;
    connection.await?;
    Ok(())
}

fn router(service: Arc<Service>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/signer", get(identity))
        .route("/sign", post(sign))
        .with_state(service)
}

async fn healthz(State(service): State<Arc<Service>>) -> StatusCode {
    if service.healthy().await {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

impl Service {
    async fn healthy(self: Arc<Self>) -> bool {
        let mut last = self.health.clone().lock_owned().await;
        if let Some(health) = last
            .as_ref()
            .filter(|health| health.checked.elapsed() < HEALTH_WINDOW)
        {
            return health.healthy;
        }
        // Spawned so a check still finishes and is kept when its request disconnects.
        tokio::spawn(async move {
            let healthy = self.records.readable().is_ok() && self.cell.answers().await.is_ok();
            *last = Some(Health {
                checked: Instant::now(),
                healthy,
            });
            healthy
        })
        .await
        .unwrap_or(false)
    }
}

async fn identity(State(service): State<Arc<Service>>, headers: HeaderMap) -> Response {
    answered(service.identity(authorization(&headers)))
}

async fn sign(State(service): State<Arc<Service>>, headers: HeaderMap, body: Bytes) -> Response {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    answered(service.sign(authorization(&headers), &body, now).await)
}

fn authorization(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::AUTHORIZATION)?.to_str().ok()
}

fn answered(answer: Result<impl serde::Serialize, Refusal>) -> Response {
    match answer {
        Ok(body) => Json(body).into_response(),
        Err(refusal) => (
            refusal.code.status(),
            Json(Refused {
                code: refusal.code.name().to_string(),
                reason: refusal.reason,
            }),
        )
            .into_response(),
    }
}
