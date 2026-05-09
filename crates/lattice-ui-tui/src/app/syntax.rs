//! Syntax / tree-sitter App surface -- the (re)parse
//! request trigger that keeps `self.syntax`'s worker in
//! lockstep with the document's `text_version`.
//!
//! Methods that live here:
//! - `maybe_reparse_syntax` (DESIGN.md §B.2: hands edit
//!   deltas to the syntax worker on text_version bump,
//!   then triggers `recompute_folds` so foldmethod=indent
//!   stays in sync). Idempotent and cheap when nothing
//!   changed.
//!
//! Stays in app.rs (deferred):
//! - `refresh_highlights` (per-frame render-side cache;
//!   moves with a render-coupled slice -- it touches
//!   `VisibleHighlightsKey` + the visible-line extent
//!   walker that's also a render concern).
//! - `refresh_pane_highlights` and inactive-pane parse
//!   coordination (render-coupled).
//!
//! What does NOT live here: tree-sitter parser cache
//! (`crate::syntax`), grammar registration -- those are
//! content-shape concerns owned by `lattice-syntax`.

use super::App;

impl App {
    /// Request a reparse if the document's text has changed
    /// since the last request. Idempotent and cheap when nothing
    /// changed; the actual parse runs on the syntax handle's
    /// worker task off the UI thread (audit slice 3 / paramount
    /// goal #1: "UI thread does no … parsing").
    pub(super) fn maybe_reparse_syntax(&mut self) {
        let tv = self.document.text_version();
        if tv == self.last_parsed_text_version {
            return;
        }
        if let Some(syntax) = self.syntax.as_ref() {
            // Slice B.2 part 2: ship the accumulated EditDeltas
            // to the worker. Worker applies them via tree.edit()
            // before running incremental Parser::parse, falling
            // back to full reparse on any inconsistency
            // (from_version mismatch, byte-length mismatch, no
            // cached tree, empty edits).
            //
            // Slice B.5: pass the Buffer (clones in O(1) via
            // ropey's internal Arc) instead of materializing the
            // full text here. Worker calls buffer.as_string() on
            // its thread, so the O(n) alloc + memcpy stays off
            // the input thread.
            let edits = std::mem::take(&mut self.pending_syntax_edits);
            let buffer = self.document.snapshot().buffer.clone();
            syntax.request_reparse(
                self.last_synced_syntax_version,
                tv,
                buffer,
                edits,
            );
        }
        self.last_parsed_text_version = tv;
        // Worker WILL be at this version after the request
        // completes. If a request gets dropped (worker panicked),
        // the next request's from_version mismatch triggers a
        // full reparse and self-corrects.
        self.last_synced_syntax_version = tv;
        // Recompute computed folds in lockstep with the syntax
        // reparse request so `foldmethod=indent` stays in sync.
        // Manual foldmethod skips the recompute (the user's `zf`
        // ranges are authoritative). Folds read the latest
        // available snapshot; if the worker is mid-parse the
        // computed folds reflect the prior text version --
        // self-corrects on the next refresh once the new
        // snapshot publishes.
        self.recompute_folds();
    }
}
