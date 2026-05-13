//! Diagnostics routing: `textDocument/publishDiagnostics` → editor
//! subscribers (Phase 4.1.d.i).
//!
//! ## Why broadcast
//!
//! A document may have multiple panes, multiple decoration
//! consumers (gutter glyph provider + underline overlay + the
//! `:diagnostics` buffer view), and -- once §5.10's event bus
//! arrives -- arbitrary plugin subscribers. Each gets a clone
//! of every event. `tokio::sync::broadcast` is the right shape:
//!
//! - Sender: the actor.
//! - Receivers: each subscriber owns one; cheap to clone the
//!   underlying queue.
//! - Lagging consumers drop oldest first; for diagnostics this
//!   is correct -- the LATEST publish supersedes any in-flight
//!   older ones (per-version dropping is the editor's job).
//!
//! ## Why a typed event vs raw `PublishDiagnosticsParams`
//!
//! [`DiagnosticEvent`] flattens the LSP shape to what the editor
//! actually consumes (uri / version / diagnostics) and adds the
//! `server_id` so a multi-server setup (rust-analyzer + clippy
//! linter bridge) can be disambiguated by the consumer without
//! carrying server identity through every hop. Rest of the LSP
//! payload is preserved verbatim via `Vec<Diagnostic>`.

use std::sync::Arc;

use lsp_types::{Diagnostic, PublishDiagnosticsParams, Uri};
use tokio::sync::broadcast;

/// Capacity of the diagnostics broadcast channel. 256 events
/// per server fits a fast indexer's burst rate (rust-analyzer
/// at startup may publish ~once per crate file). Lagged
/// receivers drop oldest first; the editor reconciles by
/// reading the URI's latest event and ignoring earlier ones
/// for that URI.
pub const DIAGNOSTICS_CHANNEL_CAPACITY: usize = 256;

/// One diagnostics publish from the server.
///
/// `Arc<...>` on the heavier fields keeps the broadcast clone
/// cheap: the channel internally buffers the last N events, so
/// every subscriber that lags a beat ends up cloning the same
/// `Arc`s rather than the whole `Vec<Diagnostic>`.
#[derive(Debug, Clone)]
pub struct DiagnosticEvent {
    /// Server id (e.g. `"rust"`). Lets a multi-server setup
    /// disambiguate without carrying the identity through every
    /// hop.
    pub server_id: Arc<str>,
    /// URI the diagnostics apply to.
    pub uri: Uri,
    /// Doc version the server computed against. The editor
    /// compares this with its own `DocSync::version(uri)` and
    /// drops events older than the current version (avoids
    /// stale diagnostics overwriting fresher state when an
    /// edit raced with the publish).
    pub version: Option<i32>,
    /// Diagnostics list. Empty list means "the server cleared
    /// this URI's diagnostics" -- a real, meaningful event,
    /// don't filter it out.
    pub diagnostics: Arc<[Diagnostic]>,
}

impl DiagnosticEvent {
    /// Construct from the LSP `PublishDiagnosticsParams` shape.
    pub fn from_lsp(server_id: Arc<str>, params: PublishDiagnosticsParams) -> Self {
        Self {
            server_id,
            uri: params.uri,
            version: params.version,
            diagnostics: Arc::from(params.diagnostics.into_boxed_slice()),
        }
    }

    /// True iff this event clears (rather than reports) the
    /// URI's diagnostics. The editor uses this to drop the
    /// decoration overlay rather than rendering an empty list.
    pub fn is_clear(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

/// The actor's send-side diagnostics bus. One per actor; shared
/// across read-loop, supervisor, future telemetry hooks.
#[derive(Clone)]
pub struct DiagnosticsBus {
    tx: broadcast::Sender<DiagnosticEvent>,
}

impl DiagnosticsBus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(DIAGNOSTICS_CHANNEL_CAPACITY);
        Self { tx }
    }

    /// Subscribe to this bus. Returns a `Receiver` that yields
    /// every event published after the call. Late subscribers
    /// don't see older events.
    pub fn subscribe(&self) -> broadcast::Receiver<DiagnosticEvent> {
        self.tx.subscribe()
    }

    /// Publish an event to all subscribers. Drops silently if no
    /// subscriber is listening (broadcast::send returns Err in
    /// that case; we don't surface it -- "no listener" is a
    /// supported state, not an error).
    pub fn publish(&self, ev: DiagnosticEvent) {
        let _ = self.tx.send(ev);
    }

    /// Approximate count of active subscribers. Useful for
    /// telemetry / debug output; not load-bearing.
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for DiagnosticsBus {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for DiagnosticsBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiagnosticsBus")
            .field("subscribers", &self.receiver_count())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Diagnostic, DiagnosticSeverity, Position as LspPosition, Range as LspRange};
    use std::str::FromStr;

    fn sample_diagnostic() -> Diagnostic {
        Diagnostic {
            range: LspRange {
                start: LspPosition {
                    line: 0,
                    character: 0,
                },
                end: LspPosition {
                    line: 0,
                    character: 1,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            code: Some(lsp_types::NumberOrString::String("E0308".into())),
            code_description: None,
            source: Some("rustc".into()),
            message: "type mismatch".into(),
            related_information: None,
            tags: None,
            data: None,
        }
    }

    #[test]
    fn from_lsp_preserves_uri_version_and_diagnostics() {
        let uri = Uri::from_str("file:///x.rs").unwrap();
        let params = PublishDiagnosticsParams {
            uri: uri.clone(),
            diagnostics: vec![sample_diagnostic()],
            version: Some(7),
        };
        let ev = DiagnosticEvent::from_lsp(Arc::from("rust"), params);
        assert_eq!(ev.uri, uri);
        assert_eq!(ev.version, Some(7));
        assert_eq!(ev.diagnostics.len(), 1);
        assert!(!ev.is_clear());
    }

    #[test]
    fn empty_diagnostics_is_clear_event() {
        let uri = Uri::from_str("file:///y.rs").unwrap();
        let params = PublishDiagnosticsParams {
            uri,
            diagnostics: Vec::new(),
            version: Some(2),
        };
        let ev = DiagnosticEvent::from_lsp(Arc::from("rust"), params);
        assert!(ev.is_clear());
    }

    #[tokio::test]
    async fn bus_fans_out_to_multiple_subscribers() {
        let bus = DiagnosticsBus::new();
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        assert_eq!(bus.receiver_count(), 2);
        let uri = Uri::from_str("file:///z.rs").unwrap();
        bus.publish(DiagnosticEvent {
            server_id: Arc::from("rust"),
            uri: uri.clone(),
            version: Some(1),
            diagnostics: Arc::from(Vec::<Diagnostic>::new().into_boxed_slice()),
        });
        let got_a = a.recv().await.unwrap();
        let got_b = b.recv().await.unwrap();
        assert_eq!(got_a.uri, uri);
        assert_eq!(got_b.uri, uri);
    }

    #[tokio::test]
    async fn bus_publish_with_no_subscribers_is_silent() {
        let bus = DiagnosticsBus::new();
        let uri = Uri::from_str("file:///void.rs").unwrap();
        bus.publish(DiagnosticEvent {
            server_id: Arc::from("rust"),
            uri,
            version: None,
            diagnostics: Arc::from(Vec::<Diagnostic>::new().into_boxed_slice()),
        });
        // No assertion; just must not panic.
    }

    #[tokio::test]
    async fn late_subscriber_does_not_see_older_events() {
        let bus = DiagnosticsBus::new();
        let uri = Uri::from_str("file:///prior.rs").unwrap();
        bus.publish(DiagnosticEvent {
            server_id: Arc::from("rust"),
            uri,
            version: None,
            diagnostics: Arc::from(Vec::<Diagnostic>::new().into_boxed_slice()),
        });
        // Subscribe AFTER the publish.
        let mut rx = bus.subscribe();
        // Recv with a tight timeout: should be empty.
        let r = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await;
        assert!(r.is_err(), "subscriber should not see prior event");
    }
}
