use crate::config::Holochain;
use crate::orchestrator::{backoff_config, bridging_agreement, connect_ham};
use anyhow::{Context, Result};
use ham::{connect_with_backoff, is_connection_error, Ham, ShutdownRx};
use holo_hash::{ActionHash, ActionHashB64, AgentPubKey, AgentPubKeyB64};
use rave_engine::types::ParkedLinkStanding;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

pub type Reading<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub trait Cell: Send + Sync {
    fn answers(&self) -> Reading<'_, ()>;
    fn standing<'a>(&'a self, link: &'a ActionHash) -> Reading<'a, ParkedLinkStanding>;
    fn bridging_agreement<'a>(
        &'a self,
        bridging_agent: &'a AgentPubKeyB64,
        hot_unit_index: u32,
    ) -> Reading<'a, ActionHashB64>;
}

/// The cell the local conductor runs, through a connection `keep_connected`
/// holds open.
pub struct HamCell {
    holochain: Holochain,
    ham: Mutex<Option<Arc<Ham>>>,
    lost: Notify,
}

impl HamCell {
    pub fn new(holochain: Holochain) -> Self {
        Self {
            holochain,
            ham: Mutex::default(),
            lost: Notify::new(),
        }
    }

    pub async fn keep_connected(&self, mut shutdown: ShutdownRx) {
        let backoff = backoff_config(&self.holochain);
        while let Some(ham) =
            connect_with_backoff(|| connect_ham(&self.holochain), &backoff, &mut shutdown).await
        {
            *self.ham.lock().expect("ham mutex poisoned") = Some(Arc::new(ham));
            tokio::select! {
                () = self.lost.notified() => {}
                _ = shutdown.changed() => return,
            }
        }
    }

    fn connected(&self) -> Result<Arc<Ham>> {
        self.ham
            .lock()
            .expect("ham mutex poisoned")
            .clone()
            .context("the conductor is not connected")
    }

    /// A connection error drops `ham` and wakes `keep_connected`, but never a
    /// newer connection a reconnect already holds.
    fn checked<T>(&self, ham: &Arc<Ham>, read: Result<T>) -> Result<T> {
        if read.as_ref().is_err_and(is_connection_error) {
            let mut held = self.ham.lock().expect("ham mutex poisoned");
            if held
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, ham))
            {
                *held = None;
                self.lost.notify_one();
            }
        }
        read
    }
}

impl Cell for HamCell {
    fn answers(&self) -> Reading<'_, ()> {
        Box::pin(async move {
            let ham = self.connected()?;
            let read = async {
                ham.ping().await?;
                ham.call_zome::<_, AgentPubKey>(
                    &self.holochain.role_name,
                    "transactor",
                    "whoami",
                    (),
                )
                .await
                .map(drop)
            }
            .await;
            self.checked(&ham, read)
        })
    }

    fn standing<'a>(&'a self, link: &'a ActionHash) -> Reading<'a, ParkedLinkStanding> {
        Box::pin(async move {
            let ham = self.connected()?;
            let read = ham
                .call_zome(
                    &self.holochain.role_name,
                    "transactor",
                    "get_parked_link_standing",
                    link,
                )
                .await;
            self.checked(&ham, read)
        })
    }

    fn bridging_agreement<'a>(
        &'a self,
        bridging_agent: &'a AgentPubKeyB64,
        hot_unit_index: u32,
    ) -> Reading<'a, ActionHashB64> {
        Box::pin(async move {
            let ham = self.connected()?;
            let read = bridging_agreement(
                &ham,
                &self.holochain.role_name,
                bridging_agent,
                hot_unit_index,
            )
            .await;
            self.checked(&ham, read)
        })
    }
}
