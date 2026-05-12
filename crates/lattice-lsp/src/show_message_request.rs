//! Server-initiated `window/showMessageRequest` plumbing
//! (4.4.b).
//!
//! Spec (LSP §3.16): the server emits a message at a severity
//! level (`Error` / `Warning` / `Info` / `Log`) accompanied by a
//! list of `MessageActionItem` action labels. The client
//! displays a modal picker; the user selects one (or dismisses);
//! the client replies with the selected `MessageActionItem`, or
//! `null` if the user dismissed without choosing.
//!
//! The user-side action set is server-defined -- e.g.
//! rust-analyzer: `[{ "title": "Reload Workspace" }]` after a
//! `Cargo.toml` edit. Picker UI uses the existing modal picker
//! infrastructure (P.1+) but with a synthetic source built from
//! the action list.
//!
//! Same bridge shape as `apply_edit`: mpsc Sender cloned per-
//! actor, per-request oneshot for the response.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

/// One server-initiated `window/showMessageRequest` request,
/// ferried from the LSP actor to the App's drain.
#[derive(Debug)]
pub struct InboundShowMessageRequest {
    /// Server that sent the request.
    pub server_id: Arc<str>,
    /// Severity. Used to colour the picker prompt + bias placement.
    pub level: lsp_types::MessageType,
    /// The message text -- displayed as the picker prompt.
    pub message: String,
    /// Action labels the user picks between. Empty when the
    /// server attached no actions (degenerate -- effectively
    /// `showMessage` with a forced acknowledgement). The App
    /// surfaces an `OK` entry so the user can still dismiss.
    pub actions: Vec<lsp_types::MessageActionItem>,
    /// Oneshot the App fills after the user picks (or
    /// dismisses).
    pub response: oneshot::Sender<ShowMessageRequestOutcome>,
}

/// Result the App reports back. `None` = user dismissed. `Some`
/// = user selected the carried action; the actor forwards this
/// verbatim to the server.
#[derive(Debug, Clone)]
pub struct ShowMessageRequestOutcome {
    pub selected: Option<lsp_types::MessageActionItem>,
}

/// Sender end of the show-message-request channel.
#[derive(Clone)]
pub struct ShowMessageRequestBus {
    tx: mpsc::UnboundedSender<InboundShowMessageRequest>,
}

impl ShowMessageRequestBus {
    pub fn new() -> (
        Self,
        mpsc::UnboundedReceiver<InboundShowMessageRequest>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    pub fn dispatch(
        &self,
        ev: InboundShowMessageRequest,
    ) -> Result<(), InboundShowMessageRequest> {
        self.tx.send(ev).map_err(|e| e.0)
    }
}

impl std::fmt::Debug for ShowMessageRequestBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShowMessageRequestBus").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_round_trips_to_receiver() {
        let (bus, mut rx) = ShowMessageRequestBus::new();
        let (tx, _resp_rx) = oneshot::channel();
        bus.dispatch(InboundShowMessageRequest {
            server_id: Arc::from("test"),
            level: lsp_types::MessageType::INFO,
            message: "Reload?".into(),
            actions: vec![lsp_types::MessageActionItem {
                title: "Yes".into(),
                properties: Default::default(),
            }],
            response: tx,
        })
        .expect("receiver alive");
        let got = rx.recv().await.expect("payload arrived");
        assert_eq!(got.server_id.as_ref(), "test");
        assert_eq!(got.actions.len(), 1);
    }
}
