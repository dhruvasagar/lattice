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
//!   Receives `(from_version, text_version, buffer, edits)`
//!   reparse requests on an unbounded mpsc channel. Each request
//!   runs the parse on `tokio::task::spawn_blocking` so the
//!   long-running tree-sitter call doesn't tie up a worker
//!   thread for the whole runtime; on completion the worker
//!   stores a fresh [`SyntaxSnapshot`] in the handle's `ArcSwap`
//!   cell.
//! - **Buffer-not-text** (slice B.5). The request carries a
//!   `Buffer` (O(1) Arc-bump clone via ropey's internal sharing)
//!   instead of a pre-materialized `String`. The worker calls
//!   `buffer.as_string()` on its `spawn_blocking` thread, so the
//!   O(n) source materialization stays off the input thread per
//!   paramount goal #1.
//! - **Incremental reparse with intermediate publish** (slices
//!   B.2 + C.2). Non-empty `edits` route to
//!   `Syntax::try_apply_intermediate` first -- this applies
//!   `tree.edit()` per delta and updates the cached source +
//!   `text_version`, but does NOT yet run `Parser::parse`. The
//!   worker then publishes an intermediate `ArcSwap::store` of
//!   this byte-shifted-but-pre-parse-shape snapshot, so renderers
//!   immediately see byte-aligned spans for unchanged content
//!   (only the changed region's tree shape is briefly stale).
//!   THEN `reparse_with_cached_tree` runs `Parser::parse(_,
//!   Some(&old_tree))` which reuses unchanged subtrees, and
//!   the worker publishes the final snapshot. Empty edits or
//!   any guard violation in `try_apply_intermediate` falls
//!   through to full reparse with a single publish.
//! - **Coalescing.** When multiple requests are queued, the
//!   worker accumulates `edits` in arrival order, takes the
//!   latest `buffer` and `text_version`, keeps the earliest
//!   `from_version`. Preserves edit ordering across the burst;
//!   the burst maps to a single `Parser::parse`. Coalesced edit
//!   count is capped at `MAX_INCREMENTAL_EDITS_PER_REQUEST`
//!   (256) to bound worst-case `tree.edit()` overhead;
//!   pathological bursts fall through to full reparse.
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
use tokio::sync::{Notify, mpsc};

use lattice_core::Buffer;
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
    /// Buffer at `text_version`. Slice B.5: carries the rope
    /// (cloning is O(1) via ropey's internal Arc) instead of a
    /// pre-materialized `String`. The worker materializes the
    /// rope to bytes via `buffer.as_string()` on the worker
    /// thread immediately before parse, moving the O(n) alloc
    /// off the input thread per paramount goal #1 ("UI thread
    /// does no I/O, no parsing"). Input-thread cost goes from
    /// O(n) String alloc + memcpy down to O(1) Arc bump (~ns).
    buffer: Buffer,
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
    pub fn spawn(lang: Lang, registry: Arc<LangRegistry>) -> Result<Option<Self>, SyntaxError> {
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
    /// **Production callers must use [`Self::seeded_with_runtime`]
    /// instead.** This method falls back to
    /// `Handle::try_current()` which silently fails when the
    /// caller hasn't entered a tokio context (notably: editor
    /// startup runs from a synchronous `main()`, before the
    /// runtime is set up). When that fails, the worker is never
    /// spawned and `request_reparse` calls go to a dropped
    /// channel -- the snapshot stays at the seeded state forever
    /// and no reparses ever run. LSP solved the same problem
    /// with an explicit runtime handle (see app.rs:96-100); this
    /// constructor remains for tests that already run inside a
    /// tokio context.
    pub fn seeded(syntax: Syntax) -> Self {
        let snapshot = Arc::new(ArcSwap::from_pointee(syntax.snapshot_owned()));
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<ReparseRequest>();
        let snapshot_for_task = snapshot.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(worker_main(syntax, cmd_rx, snapshot_for_task, None));
            }
            Err(_) => {
                drop(cmd_rx);
                drop(syntax);
            }
        }
        Self { snapshot, cmd_tx }
    }

    /// Wrap a pre-built `Syntax` and spawn the worker on the
    /// supplied tokio runtime handle. **This is the production
    /// constructor.** Mirrors what
    /// `lattice_lsp::supervisor::LspSupervisor::spawn` does: take
    /// an explicit handle so the worker actually starts even when
    /// the caller hasn't entered a tokio context yet (editor
    /// startup runs from synchronous `main()` and constructs the
    /// handle before the main loop enters tokio).
    ///
    /// Without this, `Handle::try_current()` in [`Self::seeded`]
    /// silently fails, the worker is never spawned, and Option B's
    /// entire incremental-reparse pipeline is dead -- `request_reparse`
    /// sends to a dropped channel, the snapshot stays at the seeded
    /// state, and no edit ever produces a fresh tree. The user-visible
    /// symptom is "syntax highlighting is stuck to byte positions and
    /// never tracks document edits" -- which was the bug originally
    /// reported against indent (`>>`) and backspace flows.
    /// `on_publish`, when `Some`, is fired (`notify_one`) after every
    /// snapshot publish (both the intermediate edit-shift and the final
    /// reparse). Production passes the host's `async_landed` Notify so
    /// the editor actor wakes and re-publishes render state when a
    /// reparse lands with no keystroke in flight — otherwise idle
    /// reparses (e.g. markdown, whose parse loses the race against the
    /// edit) never repaint until the next key. Tests pass `None`.
    pub fn seeded_with_runtime(
        syntax: Syntax,
        runtime: &tokio::runtime::Handle,
        on_publish: Option<Arc<Notify>>,
    ) -> Self {
        let snapshot = Arc::new(ArcSwap::from_pointee(syntax.snapshot_owned()));
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<ReparseRequest>();
        let snapshot_for_task = snapshot.clone();
        runtime.spawn(worker_main(syntax, cmd_rx, snapshot_for_task, on_publish));
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

    /// Request that the worker reparse `buffer` and stamp the
    /// resulting snapshot with `text_version`. Fire-and-forget;
    /// the new snapshot is observable via [`Self::snapshot`]
    /// once the worker completes the parse.
    ///
    /// Slice B.5: takes `Buffer` (clones in O(1) via ropey's
    /// internal Arc) rather than `String`. The full source
    /// materialization moves to the worker, off the input
    /// thread.
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
    /// order; the latest queued request's `buffer` and
    /// `text_version` win; the earliest queued request's
    /// `from_version` survives so the burst's baseline is
    /// preserved.
    pub fn request_reparse(
        &self,
        from_version: u64,
        text_version: u64,
        buffer: Buffer,
        edits: Vec<EditDelta>,
    ) {
        let _ = self.cmd_tx.send(ReparseRequest {
            from_version,
            text_version,
            buffer,
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
    on_publish: Option<Arc<Notify>>,
) {
    while let Some(mut req) = cmd_rx.recv().await {
        // Coalesce queued requests: accumulate edits in order,
        // take latest buffer/text_version, keep earliest
        // from_version. The mailbox is FIFO so this preserves
        // edit ordering across the burst.
        let mut acc_edits = std::mem::take(&mut req.edits);
        while let Ok(mut next) = cmd_rx.try_recv() {
            if next.text_version >= req.text_version {
                acc_edits.append(&mut next.edits);
                req.buffer = next.buffer;
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
            buffer,
            edits,
        } = req;
        let snapshot_for_intermediate = snapshot.clone();
        let parsed = tokio::task::spawn_blocking(move || {
            // Slice B.5: materialize the source bytes on the
            // worker thread, not the input thread. `as_string`
            // is O(n) for the rope but happens here on the
            // spawn_blocking pool.
            let text = buffer.as_string();
            // Slice C.2: two-stage parse with intermediate
            // publish.
            //
            // Stage 1 (fast, ~µs): try_apply_intermediate runs
            // tree.edit() per delta -- shifts every node's byte
            // range to track the edits -- and updates the
            // snapshot's source + text_version. The result is
            // byte-aligned (all node ranges match the new
            // source) but tree shape is pre-parse for the
            // changed regions.
            //
            // Publishing the intermediate now means the renderer
            // sees byte-aligned spans for the entire parse
            // window. Lines below a deleted line, lines after a
            // multi-byte insert, etc. all paint at correct
            // positions immediately -- no flicker for unchanged
            // content. Only the changed region's tree shape is
            // briefly stale (which usually doesn't affect
            // colour: shape-staleness within a string node, an
            // identifier node, etc. produces the same span as
            // the post-parse shape).
            //
            // Stage 2 (slow, ~50-300µs): reparse_with_cached_tree
            // runs Parser::parse with the (already edited) tree
            // as seed. tree-sitter reuses unchanged subtrees;
            // only the edited region gets re-parsed. The final
            // publish lands a moment later.
            //
            // On guard failure (no cached tree, version mismatch,
            // byte-length mismatch), Stage 1 returns Err, and we
            // fall through to a full reparse with a single
            // publish.
            let intermediate_ok = !edits.is_empty()
                && syntax
                    .try_apply_intermediate(&text, text_version, from_version, &edits)
                    .is_ok();
            if intermediate_ok {
                snapshot_for_intermediate.store(Arc::new(syntax.snapshot_owned()));
                syntax.reparse_with_cached_tree();
            } else {
                syntax.parse_at(&text, text_version);
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
        // 2026-06-03 (slice B.1): wake the host so the editor actor
        // re-publishes render state now that fresh syntax is
        // available — without this, an idle reparse (no keystroke in
        // flight) wouldn't repaint until the next key. Reached by both
        // the incremental and full-reparse paths (single fire point).
        // The intermediate publish above already advanced the snapshot
        // version, so one wake here suffices.
        if let Some(wake) = on_publish.as_ref() {
            wake.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Lang;
    use crate::syntax::Syntax;
    use std::time::Duration;

    /// Slice B.1 (2026-06-03): a reparse publish must fire the
    /// `on_publish` wake so the editor actor can re-publish render
    /// state on idle reparse completion. Without this, an idle reparse
    /// (no keystroke in flight) never repaints — the markdown
    /// "highlighting never comes back" symptom's idle half.
    #[tokio::test]
    async fn reparse_publish_fires_on_publish_wake() {
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse_at("fn main() {}\n", 1);
        let wake = Arc::new(Notify::new());
        let handle = SyntaxHandle::seeded_with_runtime(
            s,
            &tokio::runtime::Handle::current(),
            Some(wake.clone()),
        );

        // Full reparse (empty edits) to version 2.
        handle.request_reparse(
            1,
            2,
            Buffer::from_text("fn main() {}\n// new\n"),
            Vec::<EditDelta>::new(),
        );

        // The worker must fire the wake after publishing.
        tokio::time::timeout(Duration::from_secs(2), wake.notified())
            .await
            .expect("on_publish wake must fire after a reparse publish");
        assert_eq!(
            handle.snapshot().text_version(),
            2,
            "snapshot must reflect the reparsed version once the wake fires"
        );
    }
}
