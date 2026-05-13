//! Server-initiated `workspace/configuration` plumbing
//! (Phase 4.1 follow-up).
//!
//! When a language server sends `workspace/configuration` (most
//! commonly during its post-`initialize` startup to pull
//! per-server settings) the client must reply with one value
//! per requested item:
//!
//! ```json
//! {"items": [
//!   {"section": "rust-analyzer.cargo.features", "scopeUri": null},
//!   {"section": "rust-analyzer.checkOnSave",    "scopeUri": null}
//! ]}
//! ```
//!
//! Pre-this-module the actor responded with `[null, null]` for
//! every request -- spec-compliant but functionally useless.
//! With the §5.12 typed-options registry shipped + the TOML
//! loader from 4.2.g.5 (3a/3) caching the merged user + project
//! tree, the App can now look up each `section` against the
//! cached tree and surface the user's actual config.
//!
//! Same actor-to-App bridge shape as `apply_edit`: an mpsc
//! Sender cloned per-actor + a per-request oneshot for the
//! response. The actor receives the request, dispatches via
//! the bus, awaits the App's reply, and ferries the LSP
//! `Vec<Value>` back to the wire.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

/// One server-initiated `workspace/configuration` request,
/// ferried from the LSP actor to the App's drain. Carries the
/// requested section paths verbatim; the App walks each in its
/// cached TOML tree and writes the per-section values back via
/// the embedded oneshot in the same order.
#[derive(Debug)]
pub struct InboundConfigurationRequest {
    /// Server that sent the request -- recorded for the App's
    /// log entry. Cheap to clone (`Arc<str>`).
    pub server_id: Arc<str>,
    /// One section path per requested item. Spec lets `section`
    /// be `null`/missing (server wants all config); we coerce
    /// those to an empty string upstream so the app always sees
    /// a string.
    pub sections: Vec<String>,
    /// Oneshot the App fills with one `serde_json::Value` per
    /// section (in input order). Missing sections come back as
    /// `Value::Null`.
    pub response: oneshot::Sender<Vec<Value>>,
}

/// Shared sender for the configuration channel. Cloned into
/// every LSP actor at spawn; the App owns the matching
/// receiver. Dropping the receiver disables future dispatches
/// (the actor falls back to `[null, ...]` so the server doesn't
/// hang).
#[derive(Clone)]
pub struct ConfigurationBus {
    tx: mpsc::UnboundedSender<InboundConfigurationRequest>,
}

impl ConfigurationBus {
    /// Build a fresh bus + receiver pair. The App owns the
    /// receiver; the supervisor stores the bus and clones it
    /// into each actor it spawns.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<InboundConfigurationRequest>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    /// Dispatch a request to the App's drain. Returns `Err`
    /// (with the unsent payload) when the receiver has been
    /// dropped -- the actor's response task catches this and
    /// replies with `[null, ...]`.
    pub fn dispatch(
        &self,
        ev: InboundConfigurationRequest,
    ) -> Result<(), InboundConfigurationRequest> {
        self.tx.send(ev).map_err(|e| e.0)
    }
}

impl std::fmt::Debug for ConfigurationBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigurationBus").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_round_trips_to_receiver() {
        let (bus, mut rx) = ConfigurationBus::new();
        let (tx, _resp_rx) = oneshot::channel();
        bus.dispatch(InboundConfigurationRequest {
            server_id: Arc::from("test"),
            sections: vec!["rust-analyzer.cargo.features".into()],
            response: tx,
        })
        .expect("receiver alive");
        let got = rx.recv().await.expect("payload arrived");
        assert_eq!(&*got.server_id, "test");
        assert_eq!(got.sections.len(), 1);
        assert_eq!(got.sections[0], "rust-analyzer.cargo.features");
    }

    #[tokio::test]
    async fn dispatch_returns_err_when_receiver_dropped() {
        let (bus, rx) = ConfigurationBus::new();
        drop(rx);
        let (tx, _resp_rx) = oneshot::channel();
        let result = bus.dispatch(InboundConfigurationRequest {
            server_id: Arc::from("test"),
            sections: Vec::new(),
            response: tx,
        });
        assert!(result.is_err());
    }
}
