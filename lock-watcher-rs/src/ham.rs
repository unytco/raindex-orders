//! Holochain Agent Manager (Ham) — manages a persistent connection to a Holochain conductor.
//!
//! Connects via AdminWebsocket, discovers or creates an app interface, issues an auth token,
//! connects via AppWebsocket, and authorizes signing credentials for zome calls.

use anyhow::{Context, Result};
use holochain_client::{
    AdminWebsocket, AppWebsocket, AuthorizeSigningCredentialsPayload, CellInfo,
    ClientAgentSigner, ExternIO, ZomeCallTarget,
};
use serde::de::DeserializeOwned;
use std::net::Ipv4Addr;
use tracing::{debug, info};

/// A Holochain Agent Manager
pub struct Ham {
    app_connection: AppWebsocket,
    _signer: ClientAgentSigner,
}

impl Ham {
    /// Connect to a running Holochain conductor's admin interface.
    pub async fn connect(admin_port: u16, app_port: u16, app_id: &str) -> Result<Self> {
        info!(
            "[ham] Connecting to Holochain admin on port {}",
            admin_port
        );
        let admin = AdminWebsocket::connect((Ipv4Addr::LOCALHOST, admin_port), None)
            .await
            .context("Failed to connect to admin interface")?;

        // Find an existing app interface; if none exists, attach one on app_port.
        let app_interfaces = admin
            .list_app_interfaces()
            .await
            .context("Failed to list app interfaces")?;

        let app_interface = app_interfaces
            .iter()
            .find(|ai| ai.installed_app_id.is_none());

        let port = if let Some(ai) = app_interface {
            info!("[ham] Using existing app interface on port {}", ai.port);
            ai.port
        } else {
            info!(
                "[ham] No existing app interface, attaching on port {}",
                app_port
            );
            admin
                .attach_app_interface(
                    app_port,
                    None,
                    holochain_client::AllowedOrigins::Any,
                    None,
                )
                .await
                .context("Failed to attach app interface")?
        };

        let issued_token = admin
            .issue_app_auth_token(app_id.to_string().into())
            .await
            .context("Failed to issue app auth token")?;

        let signer = ClientAgentSigner::default();
        let app_connection = AppWebsocket::connect(
            (Ipv4Addr::LOCALHOST, port),
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

        info!("[ham] Connected and authorized successfully");

        Ok(Self {
            app_connection,
            _signer: signer,
        })
    }

    /// Call a zome function through the app websocket.
    pub async fn zome_call<I, R>(
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
        debug!(
            "[ham] Calling zome: {}/{} (role: {})",
            zome_name, fn_name, role_name
        );
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
}
