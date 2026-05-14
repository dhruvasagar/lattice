//! Server-initiated `window/showDocument` plumbing (4.4.b).
//!
//! Spec (LSP §3.16): the server asks the client to open a URI.
//! The URI can be:
//!
//! - A `file://` URI -- the editor opens it in a buffer.
//! - An `http://` / `https://` URI -- the editor delegates to
//!   the OS browser when `external == true`.
//! - Any other scheme -- best-effort; a server that asks the
//!   client to open a non-file, non-web URI without `external`
//!   gets `success: false`.
//!
//! Optional fields:
//!
//! - `external: bool` -- prefer the OS handler over an in-buffer
//!   open. Servers usually set this for `http*` URIs.
//! - `take_focus: bool` -- give the new buffer / window focus.
//! - `selection: Range` -- after opening, place the cursor.
//!
//! Same bridge shape as `apply_edit` + `configuration`: the
//! actor receives the request, packages it into
//! [`InboundShowDocument`] with a oneshot, and dispatches via
//! [`ShowDocumentBus`]. The App drains the receiver each frame,
//! performs the open (or browser-delegate), and writes
//! [`ShowDocumentOutcome`] back. The actor's response task
//! ferries the result to the wire.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

/// One server-initiated `window/showDocument` request, ferried
/// from the LSP actor to the App's drain.
#[derive(Debug)]
pub struct InboundShowDocument {
    /// Server that sent the request. Used by the App's echo /
    /// log so the user can tell which language server is
    /// asking. Cheap to clone (`Arc<str>`).
    pub server_id: Arc<str>,
    /// Workspace root the originating actor was spawned against
    /// (B'.2). Pairs with `server_id` to form the canonical
    /// `(server_id, workspace)` instance key so the App's log
    /// routes the show-document trail to the correct
    /// `*lsp:<server>:<workspace>*` ring.
    pub workspace: Arc<std::path::Path>,
    /// URI to open. The App inspects the scheme to decide
    /// between in-editor open vs. external-handler delegation.
    pub uri: lsp_types::Uri,
    /// True iff the server prefers an external handler (OS
    /// browser / shell). Spec defaults to false; we keep the
    /// server's wire value.
    pub external: bool,
    /// True iff the new buffer / external window should take
    /// focus. The App honours this for in-editor opens; the
    /// external path can't enforce focus.
    pub take_focus: bool,
    /// Optional selection range to place after opening (LSP
    /// positions; the App converts to byte offsets).
    pub selection: Option<lsp_types::Range>,
    /// Oneshot the App fills after performing the open. The
    /// actor task awaits this and converts the outcome into the
    /// LSP `Response`.
    pub response: oneshot::Sender<ShowDocumentOutcome>,
}

/// Result the App reports back to the actor's response task.
/// Mirrors `ShowDocumentResult`.
#[derive(Debug, Clone)]
pub struct ShowDocumentOutcome {
    pub success: bool,
}

/// Multiplexed sender end of the show-document channel. Every
/// LSP actor holds a clone; the App holds the matching
/// receiver. Dropping the receiver disables future dispatches
/// (the actor falls back to `success: false` so the server
/// doesn't hang).
#[derive(Clone)]
pub struct ShowDocumentBus {
    tx: mpsc::UnboundedSender<InboundShowDocument>,
}

impl ShowDocumentBus {
    /// Build a fresh bus + receiver pair.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<InboundShowDocument>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    /// Dispatch a request to the App's drain. Returns `Err`
    /// when the receiver has been dropped.
    pub fn dispatch(&self, ev: InboundShowDocument) -> Result<(), InboundShowDocument> {
        self.tx.send(ev).map_err(|e| e.0)
    }
}

impl std::fmt::Debug for ShowDocumentBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShowDocumentBus").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_round_trips_to_receiver() {
        let (bus, mut rx) = ShowDocumentBus::new();
        let (tx, _resp_rx) = oneshot::channel();
        use std::str::FromStr;
        let uri = lsp_types::Uri::from_str("file:///tmp/x.rs").unwrap();
        bus.dispatch(InboundShowDocument {
            server_id: Arc::from("test"),
            workspace: Arc::<std::path::Path>::from(std::path::Path::new("/tmp")),
            uri,
            external: false,
            take_focus: true,
            selection: None,
            response: tx,
        })
        .expect("receiver alive");
        let got = rx.recv().await.expect("payload arrived");
        assert_eq!(got.server_id.as_ref(), "test");
        assert!(got.take_focus);
    }
}
