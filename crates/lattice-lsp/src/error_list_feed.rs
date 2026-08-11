//! EP.3 (2026-08-10): the language server as a **second producer** of
//! the core error list.
//!
//! Design: `docs/dev/architecture/error-list.md` §3.2–§3.3. Slice plan:
//! `docs/dev/operations/slice-plans/error-list-producers.md`.
//!
//! `*problems*`, the `:error-list` picker and the whole `:next-error`
//! family already exist and are producer-agnostic. Feeding them from
//! diagnostics is what turns "the errors from my last compile" into
//! "everything currently wrong", without a second surface to learn.
//!
//! ## Coalescing is not an optimisation
//!
//! `publishDiagnostics` arrives per-URI at edit-debounce rate. Pushing
//! a rebuilt `Vec<ErrorEntry>` per notification would drive allocation
//! and a cross-thread send on every keystroke burst — the background
//! churn paramount goal #1 forbids. Instead the feed watches
//! [`DiagnosticsLayer::snapshot_revision`] and rebuilds at most once
//! per quiet period.
//!
//! ## Scope, stated honestly
//!
//! This surfaces what servers have **published**, which is not a
//! workspace scan. rust-analyzer publishes workspace-wide after a
//! check; other servers publish only for open files. Callers echo the
//! entry count so an empty result is not misread as a clean tree.

use std::sync::Arc;
use std::time::Duration;

use lsp_types::{Diagnostic, DiagnosticSeverity, Uri};

use lattice_protocol::error_list::{ErrorEntry, ErrorSeverity};

use crate::actor::uri_to_path;
use crate::diagnostics_layer::DiagnosticsLayer;

/// How long the feed waits for quiet before rebuilding.
///
/// Long enough that a burst of keystrokes produces one push rather than
/// one per publish; short enough that the list feels live. Tuned by the
/// same reasoning as other edit-debounces in the editor, not measured —
/// if the bench in the slice plan says otherwise, this is the knob.
pub const COALESCE_INTERVAL: Duration = Duration::from_millis(250);

/// Map LSP's severity onto the error list's.
///
/// `ErrorSeverity`'s own doc-comment anticipated this: *"producers map
/// their own severity onto this small set."* A diagnostic with no
/// severity is treated as an error — servers omit it rarely, and
/// under-reporting a real error is worse than over-reporting a hint.
pub fn map_severity(severity: Option<DiagnosticSeverity>) -> ErrorSeverity {
    match severity {
        Some(DiagnosticSeverity::WARNING) => ErrorSeverity::Warning,
        Some(DiagnosticSeverity::INFORMATION) => ErrorSeverity::Info,
        Some(DiagnosticSeverity::HINT) => ErrorSeverity::Note,
        // ERROR, or an unknown/absent severity.
        _ => ErrorSeverity::Error,
    }
}

/// Convert one snapshot of the diagnostics layer into error entries.
///
/// URIs that do not resolve to a filesystem path are **skipped, not
/// failed**: a server may publish against `untitled:` or a custom
/// scheme, and one unmappable URI must not cost the user every other
/// entry. Ordering follows the snapshot (URIs sorted, diagnostics by
/// line then column), which gives a stable list across rebuilds — the
/// property EP.2's re-anchoring depends on.
pub fn entries_from_snapshot(snapshot: &[(Uri, Vec<Diagnostic>)]) -> Vec<ErrorEntry> {
    let mut entries = Vec::new();
    for (uri, diagnostics) in snapshot {
        let Some(path) = uri_to_path(uri) else {
            tracing::debug!(uri = %uri.as_str(), "diagnostics: URI has no file path; skipping");
            continue;
        };
        for d in diagnostics {
            entries.push(ErrorEntry {
                path: path.clone(),
                // LSP positions are already 0-based, which is the
                // convention `jump_to_file_line_col` expects.
                line: d.range.start.line,
                col: d.range.start.character,
                severity: map_severity(d.severity),
                message: d.message.clone(),
            });
        }
    }
    entries
}

/// Build the current entry set from the layer. The pull half of the
/// feed — used by `:lsp-diagnostics-to-error-list` and by the
/// option's `false → true` transition, so a snapshot and a live tick
/// produce identical results.
pub fn current_entries(layer: &DiagnosticsLayer) -> Vec<ErrorEntry> {
    entries_from_snapshot(&layer.snapshot())
}

/// Drive the live feed: rebuild whenever the layer's revision moves and
/// the caller says the option is on, at most once per
/// [`COALESCE_INTERVAL`].
///
/// Returns the revision it last published, so the caller can thread it
/// into the next tick. `None` means nothing was published this tick —
/// either the revision was unchanged or the feed is disabled.
///
/// Split out from any spawning so it is testable without a runtime:
/// the coalescing rule is the part worth pinning, not tokio's timer.
pub fn poll_feed(
    layer: &DiagnosticsLayer,
    last_published: &mut usize,
    enabled: bool,
) -> Option<Vec<ErrorEntry>> {
    if !enabled {
        return None;
    }
    let revision = layer.snapshot_revision();
    if revision == *last_published {
        return None;
    }
    *last_published = revision;
    Some(current_entries(layer))
}

/// Handle to the running feed. Dropping it stops the task.
pub struct ErrorListFeed {
    task: tokio::task::JoinHandle<()>,
}

impl ErrorListFeed {
    /// Spawn the coalescing loop.
    ///
    /// `emit` receives each rebuilt entry set. In production that is an
    /// `InboundBus<Vec<ErrorEntry>>::send`, whose `send` bakes in the
    /// `async_landed` wake — so a republish reaches the screen without
    /// waiting for a keystroke. A bare `TickCallback` here would
    /// reproduce the "it only updates when I press something" bug class
    /// `boot-composition.md` §3 exists to design out.
    ///
    /// `enabled` is read every tick rather than captured, so toggling
    /// `lsp.diagnostics-to-error-list` takes effect without a respawn.
    pub fn spawn<E, F>(layer: DiagnosticsLayer, enabled: F, emit: E) -> Self
    where
        E: Fn(Vec<ErrorEntry>) + Send + 'static,
        F: Fn() -> bool + Send + 'static,
    {
        let task = tokio::spawn(async move {
            let mut last_published = 0usize;
            let mut ticker = tokio::time::interval(COALESCE_INTERVAL);
            // A missed tick means the editor was busy; skipping is
            // right — we want the latest state, not a backlog of
            // rebuilds.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                if let Some(entries) = poll_feed(&layer, &mut last_published, enabled()) {
                    emit(entries);
                }
            }
        });
        Self { task }
    }
}

impl Drop for ErrorListFeed {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Shared handle so boot can register the feed and the ex-command can
/// reach the layer for a manual pull.
pub type ErrorListFeedHandle = Arc<ErrorListFeed>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lsp_types::{Position, Range};

    fn diag(line: u32, col: u32, severity: Option<DiagnosticSeverity>, msg: &str) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position {
                    line,
                    character: col,
                },
                end: Position {
                    line,
                    character: col + 1,
                },
            },
            severity,
            message: msg.to_string(),
            ..Default::default()
        }
    }

    fn uri(s: &str) -> Uri {
        s.parse().unwrap()
    }

    #[test]
    fn severity_maps_every_variant() {
        assert_eq!(
            map_severity(Some(DiagnosticSeverity::ERROR)),
            ErrorSeverity::Error
        );
        assert_eq!(
            map_severity(Some(DiagnosticSeverity::WARNING)),
            ErrorSeverity::Warning
        );
        assert_eq!(
            map_severity(Some(DiagnosticSeverity::INFORMATION)),
            ErrorSeverity::Info
        );
        assert_eq!(
            map_severity(Some(DiagnosticSeverity::HINT)),
            ErrorSeverity::Note
        );
    }

    /// A server that omits severity must not have its diagnostic
    /// silently demoted — under-reporting a real error is the worse
    /// failure.
    #[test]
    fn absent_severity_is_treated_as_an_error() {
        assert_eq!(map_severity(None), ErrorSeverity::Error);
    }

    #[test]
    fn entries_carry_zero_based_line_and_column() {
        let snap = vec![(
            uri("file:///tmp/a.rs"),
            vec![diag(4, 9, Some(DiagnosticSeverity::ERROR), "boom")],
        )];
        let entries = entries_from_snapshot(&snap);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].line, 4);
        assert_eq!(entries[0].col, 9);
        assert_eq!(entries[0].message, "boom");
        assert_eq!(entries[0].severity, ErrorSeverity::Error);
    }

    /// One unmappable URI must not cost the user every other entry.
    #[test]
    fn a_non_file_uri_is_skipped_not_fatal() {
        let snap = vec![
            (
                uri("untitled:Untitled-1"),
                vec![diag(0, 0, Some(DiagnosticSeverity::ERROR), "scratch")],
            ),
            (
                uri("file:///tmp/real.rs"),
                vec![diag(1, 0, Some(DiagnosticSeverity::WARNING), "real")],
            ),
        ];
        let entries = entries_from_snapshot(&snap);
        assert_eq!(entries.len(), 1, "the file:// entry survives");
        assert_eq!(entries[0].message, "real");
    }

    #[test]
    fn an_empty_snapshot_yields_no_entries() {
        assert!(entries_from_snapshot(&[]).is_empty());
    }

    /// The coalescing rule: an unchanged revision publishes nothing, so
    /// a quiet editor does no work at all.
    #[test]
    fn poll_publishes_only_when_the_revision_moves() {
        let layer = DiagnosticsLayer::default();
        let mut last = layer.snapshot_revision();

        assert!(
            poll_feed(&layer, &mut last, true).is_none(),
            "unchanged revision must not republish"
        );
    }

    #[test]
    fn poll_publishes_nothing_while_disabled() {
        let layer = DiagnosticsLayer::default();
        // A revision the feed has never seen.
        let mut last = usize::MAX;
        assert!(
            poll_feed(&layer, &mut last, false).is_none(),
            "the option gates the feed, not just the echo"
        );
    }
}
