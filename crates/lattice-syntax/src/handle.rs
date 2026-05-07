//! `SyntaxHandle` -- the async wrapper around per-document
//! [`Syntax`] that runs reparses off the UI thread.
//!
//! ## Why this exists
//!
//! The audit's C1 finding: `Syntax::parse` runs synchronously on
//! whatever thread calls it, and the App was calling it from
//! `App::apply` (every `Action`) and `App::refresh_highlights`
//! (per frame). On a multi-MB buffer that's a sub-millisecond
//! to multi-millisecond stall on the UI thread -- a direct
//! violation of paramount goal #1 ("UI thread does no … parsing").
//!
//! ## Architecture
//!
//! Mirrors the patterns we use for the document actor and the
//! LSP supervisor:
//!
//! - **Worker task.** Owns the `Syntax` instance + `Parser`.
//!   Receives `(text_version, text)` reparse requests on an
//!   unbounded mpsc channel. Each request runs the parse on
//!   `tokio::task::spawn_blocking` so the long-running
//!   tree-sitter call doesn't tie up a worker thread for the
//!   whole runtime; on completion the worker stores a fresh
//!   [`SyntaxSnapshot`] in the handle's `ArcSwap` cell.
//! - **Coalescing.** Newer requests supersede older ones. Before
//!   running a parse, the worker drains any queued newer
//!   `(text_version, text)` and uses only the latest. Bursts of
//!   keystrokes -> at most one parse per coalesce window.
//! - **Wait-free reads.** The App / renderer / fold provider
//!   reads the latest snapshot via `handle.snapshot()` (one
//!   `ArcSwap::load_full`). No mutex, no actor round-trip.
//!
//! ## Test path
//!
//! For sync tests that pre-build a parsed `Syntax` and want to
//! plug it into the handle directly, [`SyntaxHandle::seeded`]
//! constructs a handle whose snapshot starts populated and
//! whose worker task either runs (if a tokio runtime is
//! present) or is skipped (if not -- read-only handle).

use std::sync::Arc;

use arc_swap::ArcSwap;
use tokio::sync::mpsc;

use lattice_protocol::edit::EditDelta;

use crate::lang::Lang;
use crate::registry::LangRegistry;
use crate::syntax::{Syntax, SyntaxError, SyntaxSnapshot};

/// Cap on the per-request edit count. Bounds worst-case
/// `tree.edit()` cost in the worker before parse: at ~500ns per
/// edit, 256 edits = ~128µs of pre-parse work, comparable to the
/// parse itself. Beyond that, the bookkeeping cost exceeds the
/// incremental win, so we drop the edits and force a full
/// reparse. Pathological case: a 100k-char paste delivered as
/// per-character edits (shouldn't happen via Action paths, but
/// belt-and-suspenders).
pub(crate) const MAX_INCREMENTAL_EDITS_PER_REQUEST: usize = 256;

/// Editor-facing handle. Cheap to clone; cloning gives a
/// reference to the same underlying snapshot cell + the same
/// reparse-request channel.
#[derive(Clone)]
pub struct SyntaxHandle {
    snapshot: Arc<ArcSwap<SyntaxSnapshot>>,
    cmd_tx: mpsc::UnboundedSender<ReparseRequest>,
}

struct ReparseRequest {
    /// Version the worker's tree is expected to be at BEFORE
    /// applying `edits`. The worker compares this against its
    /// own `tree.text_version` and falls back to full reparse on
    /// mismatch (see [`Syntax::parse_at_with_edits`] guards).
    from_version: u64,
    /// Version the resulting snapshot should be stamped with
    /// AFTER the parse completes.
    text_version: u64,
    /// Full source text at `text_version`. Used both as the
    /// parser input and as the published snapshot's `source`.
    text: String,
    /// Edit deltas in apply-order, taking the buffer from
    /// `from_version` to `text_version`. Empty = "no deltas
    /// available, do full reparse" (file load, document replace).
    edits: Vec<EditDelta>,
}

impl SyntaxHandle {
    /// Build a handle for `lang` and start the worker task.
    /// Returns `Ok(None)` for `Lang::Plain` or any language not
    /// registered in the supplied registry -- the App treats
    /// `None` as "no syntax highlighting for this buffer".
    pub fn spawn(
        lang: Lang,
        registry: Arc<LangRegistry>,
    ) -> Result<Option<Self>, SyntaxError> {
        let Some(syntax) = Syntax::for_language_with_registry(lang, registry)? else {
            return Ok(None);
        };
        Ok(Some(Self::seeded(syntax)))
    }

    /// Wrap a pre-built `Syntax` (already parsed or not) in a
    /// handle. Used by tests that want to drive parses
    /// synchronously and then read the result, and by callers
    /// that already constructed a `Syntax` for other reasons
    /// (one-shot help-buffer markdown highlighting).
    ///
    /// If a tokio runtime is ambient, the worker task is
    /// spawned and subsequent `request_reparse` calls process
    /// asynchronously. If not, the handle stays read-only --
    /// the snapshot reflects the seeded state and reparse
    /// requests fail silently. (App's sync unit tests live here.)
    pub fn seeded(syntax: Syntax) -> Self {
        let snapshot = Arc::new(ArcSwap::from_pointee(syntax.snapshot_owned()));
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<ReparseRequest>();
        let snapshot_for_task = snapshot.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(worker_main(syntax, cmd_rx, snapshot_for_task));
            }
            Err(_) => {
                drop(cmd_rx);
                drop(syntax);
            }
        }
        Self { snapshot, cmd_tx }
    }

    /// Wait-free read of the latest parse snapshot. Hot path;
    /// renderer / fold provider / completion call this on every
    /// frame.
    pub fn snapshot(&self) -> Arc<SyntaxSnapshot> {
        self.snapshot.load_full()
    }

    /// Borrow the snapshot via `ArcSwap`'s short-lived guard.
    /// Cheaper than [`Self::snapshot`] when the caller only
    /// needs to read a single field (no `Arc::clone`).
    pub fn with_snapshot<R>(&self, f: impl FnOnce(&SyntaxSnapshot) -> R) -> R {
        let guard = self.snapshot.load();
        f(&guard)
    }

    /// Convenience: latest `Lang`. Wait-free.
    pub fn lang(&self) -> Lang {
        self.snapshot.load().lang()
    }

    /// Request that the worker reparse `text` and stamp the
    /// resulting snapshot with `text_version`. Fire-and-forget;
    /// the new snapshot is observable via [`Self::snapshot`]
    /// once the worker completes the parse.
    ///
    /// `from_version` is the version-baseline the worker's tree
    /// is expected to be at BEFORE applying `edits`. The worker
    /// uses it to detect mismatches (dropped requests, file
    /// load, document replace) and falls back to full reparse
    /// when the cached tree's version doesn't match.
    ///
    /// `edits` carries the tree-sitter-shaped deltas in apply
    /// order, taking the buffer from `from_version` to
    /// `text_version`. Empty `edits` = "do a full reparse"
    /// (file load / cold-start path).
    ///
    /// Coalesced: queued requests' edits are accumulated in
    /// order; the latest queued request's `text` and
    /// `text_version` win; the earliest queued request's
    /// `from_version` survives so the burst's baseline is
    /// preserved.
    pub fn request_reparse(
        &self,
        from_version: u64,
        text_version: u64,
        text: String,
        edits: Vec<EditDelta>,
    ) {
        let _ = self.cmd_tx.send(ReparseRequest {
            from_version,
            text_version,
            text,
            edits,
        });
    }
}

impl std::fmt::Debug for SyntaxHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let snap = self.snapshot.load();
        f.debug_struct("SyntaxHandle")
            .field("lang", &snap.lang())
            .field("text_version", &snap.text_version())
            .finish_non_exhaustive()
    }
}

/// Worker loop. Owns the `Syntax` exclusively; processes
/// reparse requests in FIFO order, coalescing newer requests on
/// top of older ones before running each parse on a blocking
/// pool thread. Exits when every handle is dropped (sender
/// closes the channel).
///
/// Coalescing semantics (slice B.2): when multiple requests
/// arrive while the worker is busy, edits accumulate in arrival
/// order while `text` and `text_version` snap to the latest, and
/// `from_version` snaps to the earliest. Result: a single parse
/// applies the union of edits in order, taking the cached tree
/// from the burst's baseline to the burst's tip in one step.
/// Dropping older requests in favour of the latest (the pre-B.2
/// behaviour) would lose their edits and silently corrupt the
/// cached tree's byte ranges -- the new shape preserves them.
///
/// Edit count is capped at [`MAX_INCREMENTAL_EDITS_PER_REQUEST`].
/// Beyond that, the worker drops the edits and lets the syntax's
/// full-reparse fallback fire (still publishes a correct tree,
/// just at full-reparse cost).
async fn worker_main(
    mut syntax: Syntax,
    mut cmd_rx: mpsc::UnboundedReceiver<ReparseRequest>,
    snapshot: Arc<ArcSwap<SyntaxSnapshot>>,
) {
    while let Some(mut req) = cmd_rx.recv().await {
        // Coalesce queued requests: accumulate edits in order,
        // take latest text/text_version, keep earliest
        // from_version. The mailbox is FIFO so this preserves
        // edit ordering across the burst.
        let mut acc_edits = std::mem::take(&mut req.edits);
        while let Ok(mut next) = cmd_rx.try_recv() {
            if next.text_version >= req.text_version {
                acc_edits.append(&mut next.edits);
                req.text = next.text;
                req.text_version = next.text_version;
            }
        }
        req.edits = acc_edits;

        // Pathological-burst guard: if the accumulated edit list
        // exceeds the cap, drop the edits and let the syntax's
        // empty-edits guard route us to a full reparse. Cheaper
        // than applying ~thousands of `tree.edit()` calls before
        // the parse.
        if req.edits.len() > MAX_INCREMENTAL_EDITS_PER_REQUEST {
            req.edits.clear();
        }

        // Run the parse on a blocking thread; tree-sitter
        // parses can take ~ms on large buffers and we don't
        // want to tie up a tokio worker thread. The closure
        // moves `syntax` in and back out so we keep ownership.
        let ReparseRequest {
            from_version,
            text_version,
            text,
            edits,
        } = req;
        let parsed = tokio::task::spawn_blocking(move || {
            // Dispatch: empty edits -> full reparse (file load,
            // cold start, coalesce-cap fallback). Non-empty ->
            // incremental, with parse_at_with_edits internally
            // handling version-baseline + byte-length guards
            // and falling back to full reparse if anything
            // looks inconsistent.
            if edits.is_empty() {
                syntax.parse_at(&text, text_version);
            } else {
                syntax.parse_at_with_edits(&text, text_version, from_version, &edits);
            }
            syntax
        })
        .await;
        let next = match parsed {
            Ok(s) => s,
            Err(_) => {
                // The blocking task panicked. The worker
                // exits; the snapshot stays at the last
                // successful parse; future requests fail (the
                // sender's future Sends queue but nobody drains
                // them -- they just leak until the handle is
                // dropped). The App treats stale snapshots
                // gracefully (highlights for the previous
                // version stay visible).
                return;
            }
        };
        snapshot.store(Arc::new(next.snapshot_owned()));
        syntax = next;
    }
}
