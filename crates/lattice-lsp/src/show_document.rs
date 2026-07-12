//! Server-initiated `window/showDocument` plumbing (4.4.b; BC.8c reshape).
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
//! **BC.8c (2026-06-24): reshaped onto the generic inbound primitive.**
//! The bespoke `ShowDocumentBus` (an mpsc sender with no wake, drained by a
//! host `Editor::drain_inbound_show_documents` method that itself ran
//! `do_edit`) is gone. The supervisor now holds an
//! `InboundBus<InboundShowDocument>` ([`lattice_mode::inbound`]) whose `send`
//! **wakes the editor** so a server-initiated request is answered
//! off-keystroke, and whose per-tick drain runs the **mode-owned**
//! [`make_handler`] below.
//!
//! Unlike the configuration handler (a pure read), this handler maps each
//! request to a host-applied open [`Effect`] and resolves the oneshot
//! optimistically (`success: true` once the request maps to a valid open;
//! `false` on a non-file / malformed URI). The open effects
//! ([`Effect::OpenExternalUri`], [`Effect::OpenBufferAtColumn`]) are
//! **host-applied** in `Editor::handle_effect` -- they MUST run host-side
//! because this bus drains off-keystroke through the generic inbound
//! tick-callback, where peer-applied effects (`OpenBuffer` / `OpenBufferAt`)
//! are not forwarded. The Effect-boundary layering holds: the handler emits
//! generic effects + resolves its own oneshot; no `lsp_types` crosses into
//! `lattice-grammar`.

use std::sync::Arc;

use lattice_grammar::Utf16Pos;
use lattice_grammar::effect::Effect;
use tokio::sync::oneshot;

use crate::logging::{InstanceKey, LogLevel, LogSource, LspLogger};

/// The bus the supervisor fans out to each actor -- the generic inbound
/// primitive specialised to the show-document payload. `send` wakes the
/// editor; the per-tick drain runs [`make_handler`]. (Was the bespoke
/// `ShowDocumentBus` struct before BC.8c.)
pub type ShowDocumentBus = lattice_mode::inbound::InboundBus<InboundShowDocument>;

/// One server-initiated `window/showDocument` request, ferried
/// from the LSP actor to the editor's per-tick drain.
#[derive(Debug)]
pub struct InboundShowDocument {
    /// Server that sent the request. Used by the handler's echo /
    /// log so the user can tell which language server is
    /// asking. Cheap to clone (`Arc<str>`).
    pub server_id: Arc<str>,
    /// Workspace root the originating actor was spawned against
    /// (B'.2). Pairs with `server_id` to form the canonical
    /// `(server_id, workspace)` instance key so the handler's log
    /// routes the show-document trail to the correct
    /// `*lsp:<server>:<workspace>*` ring.
    pub workspace: Arc<std::path::Path>,
    /// URI to open. The handler inspects the scheme to decide
    /// between in-editor open vs. external-handler delegation.
    pub uri: lsp_types::Uri,
    /// True iff the server prefers an external handler (OS
    /// browser / shell). Spec defaults to false; we keep the
    /// server's wire value.
    pub external: bool,
    /// True iff the new buffer / external window should take
    /// focus. Single-window today, so this is recorded but not
    /// yet acted on (parity with the retired drain).
    pub take_focus: bool,
    /// Optional selection range to place after opening (LSP
    /// positions; the host converts the UTF-16 column to a byte
    /// offset against the opened line).
    pub selection: Option<lsp_types::Range>,
    /// Oneshot the handler fills after mapping the open. The
    /// actor task awaits this and converts the outcome into the
    /// LSP `Response`.
    pub response: oneshot::Sender<ShowDocumentOutcome>,
}

/// Result the handler reports back to the actor's response task.
/// Mirrors `ShowDocumentResult`.
#[derive(Debug, Clone)]
pub struct ShowDocumentOutcome {
    pub success: bool,
}

/// BC.8c: the mode-owned handler for server-initiated
/// `window/showDocument`, registered via `boot.inbound::<…>()`.
///
/// Maps each request to zero-or-one host-applied open [`Effect`] and resolves
/// its oneshot (optimistic-ack):
///
/// - `external` → [`Effect::OpenExternalUri`], `success: true` (the host arm
///   spawns the OS handler; the spawn result can't be awaited here, so the
///   ack is optimistic). The trail is recorded on the per-instance ring.
/// - non-`file://` without `external` → log + `success: false`, no effect.
/// - `file://` → [`Effect::OpenBufferAtColumn`] (`column = Some` iff a
///   selection was given), `success: true`.
/// - malformed `file://` → log + `success: false`, no effect.
///
/// `logger` is captured so the reject / external-trail logs route to the
/// correct `(server_id, workspace)` ring (mode-owned: the LSP logger + the
/// instance key both live in `lattice-lsp`).
pub fn make_handler(
    logger: LspLogger,
) -> impl FnMut(InboundShowDocument) -> Vec<Effect> + Send + 'static {
    move |req| {
        let instance = InstanceKey::new(Arc::clone(&req.server_id), Arc::clone(&req.workspace));
        let uri_str = req.uri.as_str().to_string();
        let (effect, success) = if req.external {
            // Optimistic ack: we dispatch the OS-handler open and report
            // success; the host arm spawns + logs any failure (it can't be
            // awaited here). Record the trail on the per-instance ring.
            logger.log(
                Some(&instance),
                LogLevel::Info,
                LogSource::Client,
                format!("showDocument(external): {uri_str}"),
            );
            (Some(Effect::OpenExternalUri { uri: uri_str }), true)
        } else if !uri_str.starts_with("file://") {
            logger.log(
                Some(&instance),
                LogLevel::Warn,
                LogSource::Client,
                format!("showDocument: refusing non-file URI {uri_str:?} without `external`"),
            );
            (None, false)
        } else if let Some(path) = crate::actor::uri_to_path(&req.uri) {
            // `take_focus` is recorded but a no-op today (single window),
            // matching the retired host drain.
            let _take_focus = req.take_focus;
            // The selection's UTF-16 column travels unconverted; the host
            // resolves it to a byte offset against the opened line.
            let column = req.selection.map(|range| Utf16Pos {
                line: range.start.line,
                col: range.start.character,
            });
            (
                Some(Effect::OpenBufferAtColumn {
                    path: Some(path),
                    column,
                    force: false,
                }),
                true,
            )
        } else {
            logger.log(
                Some(&instance),
                LogLevel::Warn,
                LogSource::Client,
                format!("showDocument: malformed file URI {uri_str:?}"),
            );
            (None, false)
        };
        // A dropped response receiver (server gone) is fine — log-and-skip.
        let _ = req.response.send(ShowDocumentOutcome { success });
        effect.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_mode::inbound::make_inbound;
    use std::str::FromStr;
    use tokio::sync::Notify;

    fn req(
        uri: &str,
        external: bool,
        selection: Option<lsp_types::Range>,
        response: oneshot::Sender<ShowDocumentOutcome>,
    ) -> InboundShowDocument {
        InboundShowDocument {
            server_id: Arc::from("rust"),
            workspace: Arc::<std::path::Path>::from(std::path::Path::new("/tmp")),
            uri: lsp_types::Uri::from_str(uri).expect("valid uri"),
            external,
            take_focus: false,
            selection,
            response,
        }
    }

    /// A `file://` URI without a selection maps to a host-applied
    /// `OpenBufferAtColumn { column: None }` (open only) + replies success.
    #[test]
    fn file_uri_no_selection_opens_and_acks() {
        let mut handler = make_handler(LspLogger::with_defaults());
        let (tx, mut rx) = oneshot::channel();
        let effects = handler(req("file:///tmp/x.rs", false, None, tx));
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::OpenBufferAtColumn {
                path,
                column,
                force,
            } => {
                assert_eq!(path.as_deref(), Some(std::path::Path::new("/tmp/x.rs")));
                assert!(column.is_none(), "no selection → open only, no cursor move");
                assert!(!force);
            }
            other => panic!("expected OpenBufferAtColumn, got {other:?}"),
        }
        assert!(rx.try_recv().expect("reply landed").success);
    }

    /// A `file://` URI with a selection carries the UTF-16 column unconverted
    /// for the host to resolve post-open.
    #[test]
    fn file_uri_with_selection_carries_utf16_column() {
        let mut handler = make_handler(LspLogger::with_defaults());
        let (tx, mut rx) = oneshot::channel();
        let sel = lsp_types::Range {
            start: lsp_types::Position {
                line: 3,
                character: 9,
            },
            end: lsp_types::Position {
                line: 3,
                character: 9,
            },
        };
        let effects = handler(req("file:///tmp/x.rs", false, Some(sel), tx));
        match &effects[0] {
            Effect::OpenBufferAtColumn {
                column: Some(Utf16Pos { line, col }),
                ..
            } => {
                assert_eq!((*line, *col), (3, 9));
            }
            other => panic!("expected OpenBufferAtColumn with column, got {other:?}"),
        }
        assert!(rx.try_recv().expect("reply landed").success);
    }

    /// `external: true` maps to `OpenExternalUri` + an optimistic success ack.
    #[test]
    fn external_uri_maps_to_open_external_and_acks() {
        let mut handler = make_handler(LspLogger::with_defaults());
        let (tx, mut rx) = oneshot::channel();
        let effects = handler(req("https://example.com/x", true, None, tx));
        match &effects[0] {
            Effect::OpenExternalUri { uri } => assert_eq!(uri, "https://example.com/x"),
            other => panic!("expected OpenExternalUri, got {other:?}"),
        }
        assert!(rx.try_recv().expect("reply landed").success);
    }

    /// A non-file URI without `external` is refused: no effect, `success:false`.
    #[test]
    fn non_file_uri_without_external_is_refused() {
        let mut handler = make_handler(LspLogger::with_defaults());
        let (tx, mut rx) = oneshot::channel();
        let effects = handler(req("https://example.com/x", false, None, tx));
        assert!(effects.is_empty(), "refused → no effect");
        assert!(!rx.try_recv().expect("reply landed").success);
    }

    /// A dropped response receiver (server gone) does not panic the handler.
    #[test]
    fn tolerates_dropped_receiver() {
        let mut handler = make_handler(LspLogger::with_defaults());
        let (tx, drop_rx) = oneshot::channel();
        drop(drop_rx);
        let effects = handler(req("file:///tmp/x.rs", false, None, tx));
        assert_eq!(
            effects.len(),
            1,
            "effect still emitted; only the reply is dropped"
        );
    }

    /// `send` over the generic bus wakes the editor (the wake is baked into
    /// the primitive) and the drain runs the handler over each item.
    #[tokio::test]
    async fn send_wakes_and_drain_runs_handler() {
        let wake = Arc::new(Notify::new());
        let (bus, mut drain) =
            make_inbound(Arc::clone(&wake), make_handler(LspLogger::with_defaults()));
        let (tx, _rx) = oneshot::channel();
        bus.send(req("file:///tmp/x.rs", false, None, tx))
            .expect("receiver alive");
        let woke =
            tokio::time::timeout(std::time::Duration::from_millis(200), wake.notified()).await;
        assert!(woke.is_ok(), "send must wake the editor");
        assert_eq!(drain().len(), 1, "drain runs the handler → one open effect");
    }
}
