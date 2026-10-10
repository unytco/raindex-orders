//! Once a stop is signalled, no new request goes to Holochain or Ethereum.

use ham::ShutdownRx;

#[derive(Debug, thiserror::Error)]
#[error("stopping: a stop was signalled, so no new request is sent")]
pub struct Stopped;

pub fn ensure_running(stop: &ShutdownRx) -> anyhow::Result<()> {
    if *stop.borrow() {
        Err(Stopped.into())
    } else {
        Ok(())
    }
}

pub fn is_stopped(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Stopped>().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn a_stop_is_recognised_under_any_context() {
        let (stop, stopped) = tokio::sync::watch::channel(false);
        assert!(ensure_running(&stopped).is_ok());

        stop.send(true).unwrap();
        let e = ensure_running(&stopped)
            .context("reading the links")
            .context("reconcile")
            .unwrap_err();

        assert!(is_stopped(&e), "{e:#}");
        assert!(!is_stopped(&anyhow::anyhow!("Websocket closed")));
    }
}
