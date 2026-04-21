use anyhow::{Context, Result};
use holochain_client::{
    AdminWebsocket, AppWebsocket, AuthorizeSigningCredentialsPayload, CellInfo, ClientAgentSigner,
    ExternIO, WebsocketConfig, ZomeCallTarget,
};
use serde::de::DeserializeOwned;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info};

pub struct Ham {
    app_connection: AppWebsocket,
    _signer: ClientAgentSigner,
}

impl Ham {
    pub async fn connect(
        admin_port: u16,
        app_port: u16,
        app_id: &str,
        request_timeout_secs: u64,
    ) -> Result<Self> {
        info!(
            event = "ham.connecting",
            admin_port, app_port, app_id, request_timeout_secs
        );
        let admin = AdminWebsocket::connect((Ipv4Addr::LOCALHOST, admin_port), None)
            .await
            .context("Failed to connect to admin interface")?;

        let app_interfaces = admin
            .list_app_interfaces()
            .await
            .context("Failed to list app interfaces")?;
        let app_interface = app_interfaces
            .iter()
            .find(|ai| ai.installed_app_id.is_none());
        let port = if let Some(ai) = app_interface {
            ai.port
        } else {
            admin
                .attach_app_interface(app_port, None, holochain_client::AllowedOrigins::Any, None)
                .await
                .context("Failed to attach app interface")?
        };

        let issued_token = admin
            .issue_app_auth_token(app_id.to_string().into())
            .await
            .context("Failed to issue app auth token")?;

        let mut ws_config = WebsocketConfig::CLIENT_DEFAULT;
        ws_config.default_request_timeout = Duration::from_secs(request_timeout_secs);
        let ws_config = Arc::new(ws_config);

        let signer = ClientAgentSigner::default();
        let app_connection = AppWebsocket::connect_with_config(
            (Ipv4Addr::LOCALHOST, port),
            ws_config,
            issued_token.token,
            signer.clone().into(),
            None,
        )
        .await
        .context("Failed to connect to app interface")?;

        let installed_app = app_connection
            .app_info()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get app info: {}", e))?
            .context("No app info found")?;
        let cells = installed_app
            .cell_info
            .into_values()
            .next()
            .context("No cells found in app")?;
        let cell_id = match cells.first().context("Empty cell list")? {
            CellInfo::Provisioned(c) => c.cell_id.clone(),
            _ => anyhow::bail!("Invalid cell type"),
        };

        let credentials = admin
            .authorize_signing_credentials(AuthorizeSigningCredentialsPayload {
                cell_id: cell_id.clone(),
                functions: None,
            })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to authorize signing credentials: {}", e))?;
        signer.add_credentials(cell_id, credentials);

        info!(event = "ham.connected");
        Ok(Self {
            app_connection,
            _signer: signer,
        })
    }

    pub async fn call_zome<I, R>(
        &self,
        role_name: &str,
        zome_name: &str,
        fn_name: &str,
        payload: I,
    ) -> Result<R>
    where
        I: serde::Serialize + std::fmt::Debug,
        R: DeserializeOwned,
    {
        debug!(event = "ham.call_zome", role_name, zome_name, fn_name);
        let response = self
            .app_connection
            .call_zome(
                ZomeCallTarget::RoleName(role_name.to_string()),
                zome_name.into(),
                fn_name.into(),
                ExternIO::encode(payload)?,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to call zome: {}", e))?;
        rmp_serde::from_slice(&response.0).context("Failed to deserialize response")
    }

    /// Round-trip probe that surfaces a dead websocket immediately. Uses
    /// `app_info` rather than `cached_app_info` so it actually hits the
    /// conductor.
    pub async fn ping(&self) -> Result<()> {
        self.app_connection
            .app_info()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to probe app_info: {}", e))?;
        Ok(())
    }
}

/// Classifies whether an `anyhow::Error` looks like a websocket / transport
/// failure that warrants rebuilding the `Ham` connection. Matches against the
/// rendered error chain so it handles both direct `holochain_client` failures
/// and wrapped context messages.
///
/// This is string-based because `holochain_client 0.8.x` surfaces websocket
/// failures as opaque strings inside `ConductorApiError::WebsocketError(_)`
/// and similar variants. The classifier is covered by unit tests to guard
/// against dependency upgrades silently changing the error text.
pub fn is_connection_error(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    const NEEDLES: &[&str] = &[
        "Websocket closed",
        "ConnectionClosed",
        "No connection",
        "Websocket error",
        "broken pipe",
        "connection reset",
        "IO error",
    ];
    NEEDLES.iter().any(|n| msg.contains(n))
}

#[cfg(test)]
mod tests {
    use super::is_connection_error;
    use anyhow::anyhow;

    fn wrap(base: &'static str) -> anyhow::Error {
        anyhow!(base).context("Failed to call zome")
    }

    #[test]
    fn classifies_websocket_closed() {
        let e = wrap("Websocket error: Websocket closed: ConnectionClosed");
        assert!(is_connection_error(&e));
    }

    #[test]
    fn classifies_no_connection() {
        let e = wrap("Websocket error: Websocket closed: No connection");
        assert!(is_connection_error(&e));
    }

    #[test]
    fn classifies_bare_websocket_error() {
        let e = wrap("Websocket error: some transport failure");
        assert!(is_connection_error(&e));
    }

    #[test]
    fn classifies_broken_pipe() {
        let e = wrap("io error: broken pipe");
        assert!(is_connection_error(&e));
    }

    #[test]
    fn classifies_connection_reset() {
        let e = wrap("io error: connection reset by peer");
        assert!(is_connection_error(&e));
    }

    #[test]
    fn classifies_generic_io_error() {
        let e = wrap("IO error: unexpected eof");
        assert!(is_connection_error(&e));
    }

    #[test]
    fn classifies_connection_closed_token() {
        let e = anyhow!("ConnectionClosed");
        assert!(is_connection_error(&e));
    }

    #[test]
    fn rejects_decode_error() {
        let e = wrap("Failed to deserialize response: invalid type");
        assert!(!is_connection_error(&e));
    }

    #[test]
    fn rejects_zome_logic_error() {
        let e = wrap("Failed to call zome: guest error: validation failed");
        assert!(!is_connection_error(&e));
    }

    #[test]
    fn rejects_empty_error() {
        let e = anyhow!("some unrelated problem");
        assert!(!is_connection_error(&e));
    }
}
