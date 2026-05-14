//! `DiagnosticsLayer` -- the editor's view of every server's
//! `publishDiagnostics` events, keyed for multi-server merging
//! and version-gated against stale publishes (Phase 4.1.d.ii).
//!
//! ## Why a separate layer instead of plumbing the bus directly
//!
//! The renderer and the `:diagnostics` buffer view both need:
//!
//! - **Per-URI lookup** of the latest set of diagnostics
//!   (multi-server-merged when relevant).
//! - **Per-line severity** -- what gutter glyph and what
//!   underline color to draw on a given line.
//! - **Workspace-wide enumeration** -- "show me every URI with
//!   any diagnostic, sorted by severity".
//!
//! Walking the bus on every render-frame pull is the wrong
//! shape; we'd need to replay the entire history. Instead the
//! layer maintains a `HashMap<(Uri, Arc<str>), DiagState>` that
//! the bus pump keeps up-to-date, and exposes O(n) lookups
//! against current state.
//!
//! ## Multi-server scenarios
//!
//! Two servers can attach to the same buffer (rust-analyzer +
//! a clippy linter bridge, c++ + a header-include linter).
//! Each publishes independently. The layer keys by
//! `(Uri, server_id)` so server A's clear doesn't drop
//! server B's diagnostics on the same file.
//!
//! `diagnostics_for(uri)` merges across servers; the consumer
//! sees one combined list.
//!
//! ## Version gating
//!
//! LSP servers may emit a `publishDiagnostics` for an older
//! document version after the client has sent a newer
//! `didChange`. Three rules:
//!
//! 1. If the incoming event's `version` is less than what we
//!    already have for `(uri, server_id)`, drop it.
//! 2. If `version` matches or is newer, accept.
//! 3. If `version` is `None`, accept (server doesn't track
//!    versions; we can't gate).
//!
//! Cross-version checking against the App's `DocSync.version()`
//! is the App's job -- the layer only enforces internal
//! monotonicity per `(uri, server_id)`.
//!
//! ## Empty-list = clear
//!
//! Per LSP spec, an empty `diagnostics` array means "the server
//! cleared this URI's diagnostics". We remove the entry; we
//! don't store an empty list (which would still appear in
//! `iter_uris()`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use lsp_types::{Diagnostic, DiagnosticSeverity, Uri};
use tokio::sync::broadcast;

use crate::diagnostics::DiagnosticEvent;
use crate::logging::{LogLevel, LogSource, LspLogger};

/// One server's latest published diagnostics for one URI.
#[derive(Debug, Clone)]
struct DiagState {
    version: Option<i32>,
    diagnostics: Arc<[Diagnostic]>,
}

/// Wait-free read view of all diagnostic state. Built by every
/// write into the layer; held in `DiagnosticsLayer.snapshot`
/// inside an `ArcSwap` cell. Renderers `load_full()` and read
/// per-URI without ever touching a mutex -- the audit's C3
/// finding (3000+ lock+clone+filter+collect per second on the
/// render thread) is gone.
#[derive(Debug, Clone, Default)]
struct DiagnosticsSnapshot {
    /// (uri, server_id) → state. Multi-server scenario: same
    /// URI can have entries from multiple servers; the
    /// `by_uri` field below pre-merges them so render-path
    /// readers do one O(1) lookup + an Arc clone.
    by_key: HashMap<(Uri, Arc<str>), DiagState>,
    /// uri → merged-and-sorted-by-(line, column) diagnostics.
    /// Recomputed for the affected URI on every write.
    by_uri: HashMap<Uri, Arc<[Diagnostic]>>,
}

/// Subsystem-wide diagnostics state.
///
/// **Reads are wait-free.** [`Self::diagnostics_for`],
/// [`Self::line_severity`], [`Self::diagnostics_on_line`], and
/// the count helpers all go through one `ArcSwap::load`. The
/// renderer's per-line `line_severity(uri, line)` call (50 lines
/// × 60 Hz = 3000/s on the render thread per the audit) no
/// longer takes a mutex.
///
/// **Writes serialise on a brief mutex.** [`Self::apply`] and the
/// `clear_*` methods take a small write-side lock just long
/// enough to clone the current snapshot, mutate it, recompute
/// the affected URI's `by_uri` entry, and `ArcSwap::store` it.
/// Writers don't block readers (ArcSwap is RCU-flavoured); two
/// concurrent writers serialise via the write lock.
///
/// Cloneable; every clone shares the same `ArcSwap` cell + the
/// same write lock (so the layer behaves as one logical state
/// across actor pumps).
#[derive(Clone)]
pub struct DiagnosticsLayer {
    /// Wait-free read cell. RCU-flavoured: `load` is ~2ns;
    /// `store` is one atomic release-store.
    snapshot: Arc<ArcSwap<DiagnosticsSnapshot>>,
    /// Serialises writers. Held only across the snapshot
    /// clone + mutation + store -- microseconds, never across
    /// I/O.
    write: Arc<Mutex<()>>,
    logger: LspLogger,
}

impl DiagnosticsLayer {
    /// Build an empty layer. The logger is used for "dropped
    /// stale" telemetry.
    pub fn new(logger: LspLogger) -> Self {
        Self {
            snapshot: Arc::new(ArcSwap::from(Arc::new(DiagnosticsSnapshot::default()))),
            write: Arc::new(Mutex::new(())),
            logger,
        }
    }

    /// Apply one [`DiagnosticEvent`]. Drops stale events; clears
    /// state on empty diagnostics; otherwise replaces the
    /// `(uri, server_id)` entry. Single-snapshot publish at the
    /// end: readers either see the prior state or the post-write
    /// state, never a torn intermediate.
    pub fn apply(&self, event: DiagnosticEvent) {
        let key = (event.uri.clone(), Arc::clone(&event.server_id));
        let _g = self.write.lock().expect("DiagnosticsLayer write lock");
        let current = self.snapshot.load_full();

        // Version gate: drop iff event.version < current and
        // both are Some. Equal versions are accepted (server
        // republishing is legal).
        if let Some(prev) = current.by_key.get(&key)
            && let (Some(p), Some(e)) = (prev.version, event.version)
            && e < p
        {
            // Drop the lock guard before logging (logger may
            // re-enter via Event::LspLogPushed on the bus).
            drop(_g);
            // B'.2: diagnostics layer doesn't carry workspace
            // today; the stale-version trace is subsystem-level
            // so route to the global ring rather than guessing
            // the instance. Future slice can plumb workspace
            // into `DiagnosticEvent` if per-instance routing is
            // wanted.
            self.logger.log(
                None,
                LogLevel::Debug,
                LogSource::Client,
                format!(
                    "dropping stale diagnostics for {} from {} version {} (have {})",
                    event.uri.as_str(),
                    event.server_id,
                    e,
                    p
                ),
            );
            return;
        }

        let mut next = (*current).clone();
        if event.diagnostics.is_empty() {
            next.by_key.remove(&key);
        } else {
            next.by_key.insert(
                key.clone(),
                DiagState {
                    version: event.version,
                    diagnostics: event.diagnostics,
                },
            );
        }
        rebuild_by_uri(&mut next, &key.0);
        self.snapshot.store(Arc::new(next));
    }

    /// All diagnostics for a URI, merged across every server
    /// that's attached to it. Returns an empty `Vec` when
    /// there are none.
    pub fn diagnostics_for(&self, uri: &Uri) -> Vec<Diagnostic> {
        match self.snapshot.load().by_uri.get(uri) {
            Some(arr) => arr.to_vec(),
            None => Vec::new(),
        }
    }

    /// Wait-free `Arc<[Diagnostic]>` borrow of the merged
    /// diagnostics for `uri`. Preferred on the hot path
    /// (renderer / picker fan-out) -- avoids the per-call
    /// `Vec` allocation that `diagnostics_for` does.
    pub fn diagnostics_arc(&self, uri: &Uri) -> Option<Arc<[Diagnostic]>> {
        self.snapshot.load().by_uri.get(uri).cloned()
    }

    /// Diagnostics that overlap `line`. Half-open at the end:
    /// a range ending on line N column 0 covers lines
    /// `start.line ..= end.line - 1` (LSP convention). To match
    /// the gutter glyph behaviour we still surface diagnostics
    /// whose end is exactly at line N column 0 if N is the
    /// current line -- callers prefer "show, don't hide" for
    /// edge cases.
    pub fn diagnostics_on_line(&self, uri: &Uri, line: u32) -> Vec<Diagnostic> {
        let snap = self.snapshot.load();
        match snap.by_uri.get(uri) {
            Some(arr) => arr
                .iter()
                .filter(|d| d.range.start.line <= line && line <= d.range.end.line)
                .cloned()
                .collect(),
            None => Vec::new(),
        }
    }

    /// Most severe severity present on `line`, or `None` if no
    /// diagnostic touches it. "Most severe" = lowest enum tag
    /// (Error == 1 < Warning == 2 < Information == 3 <
    /// Hint == 4). Used by the gutter glyph provider; runs once
    /// per visible line per frame, so this path stays
    /// allocation-free (filters the borrowed slice in place).
    pub fn line_severity(&self, uri: &Uri, line: u32) -> Option<DiagnosticSeverity> {
        let snap = self.snapshot.load();
        snap.by_uri
            .get(uri)?
            .iter()
            .filter(|d| d.range.start.line <= line && line <= d.range.end.line)
            .filter_map(|d| d.severity)
            .min_by_key(severity_rank)
    }

    /// Every URI with at least one stored diagnostic. Sorted
    /// alphabetically (stable for the `:diagnostics` buffer
    /// view).
    pub fn iter_uris(&self) -> Vec<Uri> {
        let snap = self.snapshot.load();
        let mut uris: Vec<Uri> = snap
            .by_uri
            .keys()
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        uris.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        uris
    }

    /// Snapshot of every (uri, merged-list) pair. Backs the
    /// `:diagnostics` buffer's body. URIs sorted; diagnostics
    /// within a URI ordered by (line, column).
    pub fn snapshot(&self) -> Vec<(Uri, Vec<Diagnostic>)> {
        let snap = self.snapshot.load();
        let mut uris: Vec<Uri> = snap.by_uri.keys().cloned().collect();
        uris.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        uris.into_iter()
            .map(|uri| {
                let diags = snap
                    .by_uri
                    .get(&uri)
                    .map(|arr| arr.to_vec())
                    .unwrap_or_default();
                (uri, diags)
            })
            .collect()
    }

    /// Total diagnostic count across every URI / server. Used
    /// by the modeline summary ("rust: 3 errors, 2 warnings").
    pub fn count(&self) -> usize {
        self.snapshot
            .load()
            .by_key
            .values()
            .map(|s| s.diagnostics.len())
            .sum()
    }

    /// Severity tally across every URI: (errors, warnings,
    /// info, hints). Modeline / status segment consumer.
    pub fn severity_counts(&self) -> SeverityCounts {
        let snap = self.snapshot.load();
        let mut counts = SeverityCounts::default();
        for state in snap.by_key.values() {
            for d in state.diagnostics.iter() {
                match d.severity {
                    Some(DiagnosticSeverity::ERROR) => counts.errors += 1,
                    Some(DiagnosticSeverity::WARNING) => counts.warnings += 1,
                    Some(DiagnosticSeverity::INFORMATION) => counts.info += 1,
                    Some(DiagnosticSeverity::HINT) => counts.hints += 1,
                    _ => counts.unknown += 1,
                }
            }
        }
        counts
    }

    /// Drop everything. Used by `:diag-clear` and tests.
    pub fn clear(&self) {
        let _g = self.write.lock().expect("DiagnosticsLayer write lock");
        self.snapshot
            .store(Arc::new(DiagnosticsSnapshot::default()));
    }

    /// Drop every entry for a URI (across all servers). Used
    /// when a buffer closes -- the diagnostics are no longer
    /// relevant since we won't render them.
    pub fn clear_uri(&self, uri: &Uri) {
        let _g = self.write.lock().expect("DiagnosticsLayer write lock");
        let current = self.snapshot.load_full();
        let mut next = (*current).clone();
        next.by_key.retain(|(u, _), _| u != uri);
        next.by_uri.remove(uri);
        self.snapshot.store(Arc::new(next));
    }

    /// Drop every entry for one server -- used when a server
    /// dies / is detached.
    pub fn clear_server(&self, server_id: &Arc<str>) {
        let _g = self.write.lock().expect("DiagnosticsLayer write lock");
        let current = self.snapshot.load_full();
        let mut next = (*current).clone();
        let affected_uris: Vec<Uri> = next
            .by_key
            .keys()
            .filter(|(_, s)| Arc::ptr_eq(s, server_id) || s.as_ref() == server_id.as_ref())
            .map(|(u, _)| u.clone())
            .collect();
        next.by_key
            .retain(|(_, s), _| !Arc::ptr_eq(s, server_id) && s.as_ref() != server_id.as_ref());
        for uri in affected_uris {
            rebuild_by_uri(&mut next, &uri);
        }
        self.snapshot.store(Arc::new(next));
    }
}

/// Recompute `next.by_uri[uri]` from `next.by_key`. Called
/// inside the write critical section after every mutation that
/// could have changed the affected URI's merged-list. Sort key
/// is `(line, character)` so the picker / `:diagnostics` view
/// gets stable ordering without sorting at read time.
fn rebuild_by_uri(next: &mut DiagnosticsSnapshot, uri: &Uri) {
    let mut merged: Vec<Diagnostic> = next
        .by_key
        .iter()
        .filter(|((u, _), _)| u == uri)
        .flat_map(|(_, s)| s.diagnostics.iter().cloned())
        .collect();
    if merged.is_empty() {
        next.by_uri.remove(uri);
        return;
    }
    merged.sort_by(|a, b| {
        a.range
            .start
            .line
            .cmp(&b.range.start.line)
            .then(a.range.start.character.cmp(&b.range.start.character))
    });
    next.by_uri.insert(uri.clone(), Arc::from(merged));
}

impl std::fmt::Debug for DiagnosticsLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.count();
        f.debug_struct("DiagnosticsLayer")
            .field("total_diagnostics", &n)
            .finish_non_exhaustive()
    }
}

/// Counts of each diagnostic severity in the layer.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SeverityCounts {
    pub errors: usize,
    pub warnings: usize,
    pub info: usize,
    pub hints: usize,
    /// Diagnostics with `severity = None` (servers may omit it
    /// for "general advisory" entries).
    pub unknown: usize,
}

impl SeverityCounts {
    /// Total across all severities.
    pub fn total(&self) -> usize {
        self.errors + self.warnings + self.info + self.hints + self.unknown
    }
}

/// Map LSP severity to a sort rank (lower = more severe).
fn severity_rank(s: &DiagnosticSeverity) -> u8 {
    // DiagnosticSeverity is a wrapper struct around i32; pull
    // out the int and compare. Spec: 1=Error, 2=Warning,
    // 3=Information, 4=Hint.
    match *s {
        DiagnosticSeverity::ERROR => 0,
        DiagnosticSeverity::WARNING => 1,
        DiagnosticSeverity::INFORMATION => 2,
        DiagnosticSeverity::HINT => 3,
        _ => 4,
    }
}

/// Drain a `DiagnosticEvent` broadcast receiver into the layer
/// in a tokio task. Returns when the bus closes (the server is
/// gone). Lagging consumers (`Lagged(n)`) are tolerated --
/// the next event in the queue still reflects the latest
/// state per URI, which is what callers care about.
///
/// Spawn pattern:
///
/// ```ignore
/// let layer = DiagnosticsLayer::new(logger.clone());
/// let rx = server_handle.subscribe_diagnostics();
/// tokio::spawn(pump_diagnostics(layer.clone(), rx));
/// ```
pub async fn pump_diagnostics(
    layer: DiagnosticsLayer,
    mut rx: broadcast::Receiver<DiagnosticEvent>,
) {
    loop {
        match rx.recv().await {
            Ok(event) => layer.apply(event),
            Err(broadcast::error::RecvError::Lagged(_)) => {
                // Drop the lag; latest state still arrives.
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Position, Range};
    use std::str::FromStr;

    fn diag(line: u32, severity: DiagnosticSeverity, msg: &str) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position { line, character: 0 },
                end: Position { line, character: 5 },
            },
            severity: Some(severity),
            code: None,
            code_description: None,
            source: None,
            message: msg.into(),
            related_information: None,
            tags: None,
            data: None,
        }
    }

    fn ev(
        server: &str,
        uri: &str,
        version: Option<i32>,
        diags: Vec<Diagnostic>,
    ) -> DiagnosticEvent {
        DiagnosticEvent {
            server_id: Arc::from(server),
            uri: Uri::from_str(uri).unwrap(),
            version,
            diagnostics: Arc::from(diags.into_boxed_slice()),
        }
    }

    fn layer() -> DiagnosticsLayer {
        DiagnosticsLayer::new(LspLogger::with_defaults())
    }

    #[test]
    fn apply_stores_diagnostics() {
        let l = layer();
        l.apply(ev(
            "rust",
            "file:///x.rs",
            Some(1),
            vec![diag(0, DiagnosticSeverity::ERROR, "boom")],
        ));
        let uri = Uri::from_str("file:///x.rs").unwrap();
        let d = l.diagnostics_for(&uri);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].message, "boom");
    }

    #[test]
    fn apply_with_empty_list_clears_entry() {
        let l = layer();
        l.apply(ev(
            "rust",
            "file:///x.rs",
            Some(1),
            vec![diag(0, DiagnosticSeverity::ERROR, "first")],
        ));
        l.apply(ev("rust", "file:///x.rs", Some(2), Vec::new()));
        let uri = Uri::from_str("file:///x.rs").unwrap();
        assert!(l.diagnostics_for(&uri).is_empty());
        assert!(l.iter_uris().is_empty(), "URI gone after clear");
    }

    #[test]
    fn stale_version_is_dropped() {
        let l = layer();
        l.apply(ev(
            "rust",
            "file:///x.rs",
            Some(5),
            vec![diag(0, DiagnosticSeverity::ERROR, "current")],
        ));
        // Stale: older version, should not overwrite.
        l.apply(ev(
            "rust",
            "file:///x.rs",
            Some(3),
            vec![diag(0, DiagnosticSeverity::ERROR, "stale")],
        ));
        let uri = Uri::from_str("file:///x.rs").unwrap();
        let d = l.diagnostics_for(&uri);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].message, "current");
    }

    #[test]
    fn equal_version_replaces_state() {
        let l = layer();
        l.apply(ev(
            "rust",
            "file:///x.rs",
            Some(5),
            vec![diag(0, DiagnosticSeverity::ERROR, "first")],
        ));
        l.apply(ev(
            "rust",
            "file:///x.rs",
            Some(5),
            vec![diag(0, DiagnosticSeverity::ERROR, "republish")],
        ));
        let uri = Uri::from_str("file:///x.rs").unwrap();
        let d = l.diagnostics_for(&uri);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].message, "republish");
    }

    #[test]
    fn version_none_always_accepted() {
        let l = layer();
        l.apply(ev(
            "rust",
            "file:///x.rs",
            None,
            vec![diag(0, DiagnosticSeverity::ERROR, "first")],
        ));
        l.apply(ev(
            "rust",
            "file:///x.rs",
            None,
            vec![diag(0, DiagnosticSeverity::WARNING, "second")],
        ));
        let uri = Uri::from_str("file:///x.rs").unwrap();
        let d = l.diagnostics_for(&uri);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].message, "second");
    }

    #[test]
    fn multi_server_diagnostics_merge_per_uri() {
        let l = layer();
        l.apply(ev(
            "rust",
            "file:///x.rs",
            Some(1),
            vec![diag(0, DiagnosticSeverity::ERROR, "rust err")],
        ));
        l.apply(ev(
            "clippy",
            "file:///x.rs",
            Some(1),
            vec![diag(2, DiagnosticSeverity::WARNING, "clippy warn")],
        ));
        let uri = Uri::from_str("file:///x.rs").unwrap();
        let d = l.diagnostics_for(&uri);
        assert_eq!(d.len(), 2);
        let messages: Vec<&str> = d.iter().map(|x| x.message.as_str()).collect();
        assert!(messages.contains(&"rust err"));
        assert!(messages.contains(&"clippy warn"));
    }

    #[test]
    fn one_servers_clear_does_not_drop_anothers_diagnostics() {
        let l = layer();
        l.apply(ev(
            "rust",
            "file:///x.rs",
            Some(1),
            vec![diag(0, DiagnosticSeverity::ERROR, "rust")],
        ));
        l.apply(ev(
            "clippy",
            "file:///x.rs",
            Some(1),
            vec![diag(2, DiagnosticSeverity::WARNING, "clippy")],
        ));
        // rust clears its diagnostics for the URI.
        l.apply(ev("rust", "file:///x.rs", Some(2), Vec::new()));
        let uri = Uri::from_str("file:///x.rs").unwrap();
        let d = l.diagnostics_for(&uri);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].message, "clippy");
    }

    #[test]
    fn line_severity_returns_most_severe() {
        let l = layer();
        l.apply(ev(
            "rust",
            "file:///x.rs",
            None,
            vec![
                diag(2, DiagnosticSeverity::WARNING, "warn"),
                diag(2, DiagnosticSeverity::ERROR, "err"),
                diag(2, DiagnosticSeverity::HINT, "hint"),
            ],
        ));
        let uri = Uri::from_str("file:///x.rs").unwrap();
        assert_eq!(l.line_severity(&uri, 2), Some(DiagnosticSeverity::ERROR));
    }

    #[test]
    fn line_severity_returns_none_for_clean_lines() {
        let l = layer();
        l.apply(ev(
            "rust",
            "file:///x.rs",
            None,
            vec![diag(0, DiagnosticSeverity::ERROR, "boom")],
        ));
        let uri = Uri::from_str("file:///x.rs").unwrap();
        assert_eq!(l.line_severity(&uri, 5), None);
    }

    #[test]
    fn diagnostics_on_line_filters_by_range() {
        let mut d_multi = diag(2, DiagnosticSeverity::ERROR, "spans");
        d_multi.range.end = Position {
            line: 4,
            character: 0,
        };
        let l = layer();
        l.apply(ev("rust", "file:///x.rs", None, vec![d_multi]));
        let uri = Uri::from_str("file:///x.rs").unwrap();
        // Lines 2..=4 are in range.
        for line in 2..=4 {
            assert_eq!(l.diagnostics_on_line(&uri, line).len(), 1, "line {line}");
        }
        // Line 5 is outside.
        assert!(l.diagnostics_on_line(&uri, 5).is_empty());
    }

    #[test]
    fn iter_uris_sorts_alphabetically() {
        let l = layer();
        l.apply(ev(
            "rust",
            "file:///b.rs",
            None,
            vec![diag(0, DiagnosticSeverity::ERROR, "x")],
        ));
        l.apply(ev(
            "rust",
            "file:///a.rs",
            None,
            vec![diag(0, DiagnosticSeverity::ERROR, "x")],
        ));
        let uris = l.iter_uris();
        assert_eq!(uris.len(), 2);
        assert_eq!(uris[0].as_str(), "file:///a.rs");
        assert_eq!(uris[1].as_str(), "file:///b.rs");
    }

    #[test]
    fn snapshot_returns_per_uri_sorted_by_line_then_column() {
        let l = layer();
        let mut d_a = diag(0, DiagnosticSeverity::ERROR, "first");
        d_a.range.start = Position {
            line: 5,
            character: 10,
        };
        let mut d_b = diag(0, DiagnosticSeverity::ERROR, "second");
        d_b.range.start = Position {
            line: 5,
            character: 3,
        };
        let mut d_c = diag(0, DiagnosticSeverity::ERROR, "third");
        d_c.range.start = Position {
            line: 1,
            character: 0,
        };
        l.apply(ev("rust", "file:///x.rs", None, vec![d_a, d_b, d_c]));
        let snap = l.snapshot();
        assert_eq!(snap.len(), 1);
        let (_, diags) = &snap[0];
        let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        // Expected order: line 1 "third", line 5 col 3 "second", line 5 col 10 "first".
        assert_eq!(messages, vec!["third", "second", "first"]);
    }

    #[test]
    fn severity_counts_tallies_across_uris() {
        let l = layer();
        l.apply(ev(
            "rust",
            "file:///a.rs",
            None,
            vec![
                diag(0, DiagnosticSeverity::ERROR, "e1"),
                diag(1, DiagnosticSeverity::WARNING, "w1"),
            ],
        ));
        l.apply(ev(
            "rust",
            "file:///b.rs",
            None,
            vec![
                diag(0, DiagnosticSeverity::ERROR, "e2"),
                diag(1, DiagnosticSeverity::HINT, "h1"),
                diag(2, DiagnosticSeverity::INFORMATION, "i1"),
            ],
        ));
        let c = l.severity_counts();
        assert_eq!(c.errors, 2);
        assert_eq!(c.warnings, 1);
        assert_eq!(c.info, 1);
        assert_eq!(c.hints, 1);
        assert_eq!(c.total(), 5);
    }

    #[test]
    fn count_returns_total_across_servers_and_uris() {
        let l = layer();
        l.apply(ev(
            "rust",
            "file:///a.rs",
            None,
            vec![diag(0, DiagnosticSeverity::ERROR, "x")],
        ));
        l.apply(ev(
            "clippy",
            "file:///a.rs",
            None,
            vec![diag(1, DiagnosticSeverity::WARNING, "y")],
        ));
        l.apply(ev(
            "rust",
            "file:///b.rs",
            None,
            vec![diag(0, DiagnosticSeverity::ERROR, "z")],
        ));
        assert_eq!(l.count(), 3);
    }

    #[test]
    fn clear_drops_everything() {
        let l = layer();
        l.apply(ev(
            "rust",
            "file:///x.rs",
            None,
            vec![diag(0, DiagnosticSeverity::ERROR, "x")],
        ));
        l.clear();
        assert_eq!(l.count(), 0);
    }

    #[test]
    fn clear_uri_drops_only_that_uri() {
        let l = layer();
        l.apply(ev(
            "rust",
            "file:///a.rs",
            None,
            vec![diag(0, DiagnosticSeverity::ERROR, "a")],
        ));
        l.apply(ev(
            "rust",
            "file:///b.rs",
            None,
            vec![diag(0, DiagnosticSeverity::ERROR, "b")],
        ));
        let a = Uri::from_str("file:///a.rs").unwrap();
        l.clear_uri(&a);
        assert!(l.diagnostics_for(&a).is_empty());
        let b = Uri::from_str("file:///b.rs").unwrap();
        assert_eq!(l.diagnostics_for(&b).len(), 1);
    }

    #[test]
    fn clear_server_drops_only_that_servers_entries() {
        let l = layer();
        l.apply(ev(
            "rust",
            "file:///x.rs",
            None,
            vec![diag(0, DiagnosticSeverity::ERROR, "rust")],
        ));
        l.apply(ev(
            "clippy",
            "file:///x.rs",
            None,
            vec![diag(0, DiagnosticSeverity::WARNING, "clippy")],
        ));
        let id: Arc<str> = Arc::from("rust");
        l.clear_server(&id);
        let uri = Uri::from_str("file:///x.rs").unwrap();
        let d = l.diagnostics_for(&uri);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].message, "clippy");
    }

    #[test]
    fn clone_shares_state() {
        let l = layer();
        let l2 = l.clone();
        l.apply(ev(
            "rust",
            "file:///x.rs",
            None,
            vec![diag(0, DiagnosticSeverity::ERROR, "x")],
        ));
        assert_eq!(l2.count(), 1);
    }
}
