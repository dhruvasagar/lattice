//! `DocumentSnapshot` -- immutable view of a document at one
//! committed point in time (DESIGN.md §5.6.8).
//!
//! Renderers read every frame through one `arc_swap::ArcSwap::load`
//! per visible document. The returned `Arc<DocumentSnapshot>` lives
//! for the entire frame; all subsequent text / metadata reads go
//! through that `Arc` -- no actor round-trip, no lock contention.
//!
//! ## Snapshot fields
//!
//! v1 ships the subset the TUI renderer actually consumes. Future
//! commits will fold in:
//!
//! - `selections: Arc<SelectionSet>` -- when multi-cursor lands
//!   (today the App holds a single per-pane cursor).
//! - `syntax: Option<Arc<SyntaxSnapshot>>` -- when the syntax cache
//!   moves into the actor.
//! - `decorations: Arc<DecorationLayer>` -- when the decoration
//!   layer (§5.6.2) is built.
//! - `layout: Option<Arc<LayoutCacheSnapshot>>` -- shaped buffers
//!   only; not relevant for the TUI.
//!
//! All of these are `Arc`-cloned per snapshot. A snapshot's memory
//! cost is independent of buffer size: ~6 `Arc` words plus the
//! changed-fragment cost of the underlying immutable structures.
//!
//! ## Publish discipline
//!
//! The actor (and only the actor) writes to the published cell via
//! `PublishedSnapshot::store`. The runtime layer keeps that method
//! `pub(crate)` so external callers can't forge a snapshot.
//! Renderers / readers use [`PublishedSnapshot::load`].

use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use lattice_core::{Buffer, Document};
use lattice_protocol::ids::DocumentId;
use lattice_protocol::selection::SelectionSet;

/// One immutable snapshot of a document. Cheap to clone (every
/// non-trivial field is already `Arc`-shareable internally;
/// `Buffer` wraps a `ropey::Rope` whose interior is a B-tree of
/// `Arc`s, so cloning is `O(log n)` with no heap allocation in the
/// common case).
#[derive(Debug, Clone, Default)]
pub struct DocumentSnapshot {
    pub id: DocumentId,
    /// Bumps on any commit (edits, undo, redo, set_path, mark_clean).
    pub version: u64,
    /// Bumps only on text-mutating commits. Used by the syntax cache
    /// to decide whether to reparse.
    pub text_version: u64,
    pub buffer: Buffer,
    /// `Arc<PathBuf>` so cloning a snapshot doesn't clone the path
    /// string. `None` means an unsaved buffer (`*scratch*`-style).
    pub path: Option<Arc<PathBuf>>,
    /// True iff the buffer differs from its on-disk state. Tracked
    /// by the document's clean-position bookkeeping; the snapshot
    /// just reflects the current value.
    pub dirty: bool,
    /// `Arc<SelectionSet>` so cloning a snapshot doesn't clone the
    /// selection vector. v1 single-cursor: this is always one
    /// charwise selection; multi-cursor support folds in here when
    /// it lands.
    pub selections: Arc<SelectionSet>,
}

impl DocumentSnapshot {
    /// Bench-only constructor exposing [`Self::from_document`] to
    /// criterion benches (which live outside the runtime crate).
    /// Production callers must go through the actor's publish
    /// discipline; this is `#[doc(hidden)]` so it doesn't show in
    /// rustdoc and the name advertises the constraint.
    #[doc(hidden)]
    pub fn __bench_from_document(doc: &Document) -> Self {
        Self::from_document(doc)
    }

    /// Build a snapshot from a `Document`. Called by the actor on
    /// every commit. `pub(crate)` so external callers can't bypass
    /// the actor's publish discipline.
    pub(crate) fn from_document(doc: &Document) -> Self {
        Self {
            id: doc.id(),
            version: doc.version(),
            text_version: doc.text_version(),
            buffer: doc.buffer().clone(),
            path: doc.path().map(|p| Arc::new(p.to_path_buf())),
            dirty: doc.dirty(),
            selections: Arc::new(doc.selections().clone()),
        }
    }

    /// Convenience: borrow the path slice if set.
    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref().map(|p| p.as_path())
    }

    /// Convenience: render the buffer to a `String`. The renderer's
    /// hot path should prefer [`Buffer::as_string`] on
    /// `&self.buffer` directly; this is for tests / debug output.
    pub fn text(&self) -> String {
        self.buffer.as_string()
    }
}

/// Single-writer multi-reader cell holding the current
/// [`DocumentSnapshot`]. The actor stores; everyone else loads.
///
/// Backed by `arc_swap::ArcSwap`, an RCU-flavored primitive:
/// `load` is wait-free (~2ns), `store` is a single atomic
/// release-store; reclamation is by refcount drop on the last
/// `Arc<DocumentSnapshot>` reader. Per DESIGN.md §5.6.8 these
/// semantics are not fungible -- the renderer's correctness model
/// depends on them.
pub struct PublishedSnapshot {
    cell: Arc<ArcSwap<DocumentSnapshot>>,
}

impl PublishedSnapshot {
    /// M.2.b.1 (2026-05-31): promoted from `pub(crate)` so
    /// external `Document` impls (`MultibufferDocumentHandle` in
    /// `lattice-multibuffer`, future plugin-defined kinds) can
    /// publish their own composed snapshots. The publish
    /// **discipline** still applies — every impl is responsible
    /// for "publish-after-mutate" ordering against its own
    /// readers. The actor model in this crate enforces that
    /// discipline through the message loop; multibuffer enforces
    /// it through its own internal serialisation.
    pub fn new(initial: DocumentSnapshot) -> Self {
        Self {
            cell: Arc::new(ArcSwap::from_pointee(initial)),
        }
    }

    /// Borrowed handle to the shared `Arc<ArcSwap<...>>`. Used by
    /// [`SnapshotCache::new`] to share the underlying cell with a
    /// per-thread reader cache. Promoted from `pub(crate)` for
    /// the same reason as [`Self::new`] — external impls need to
    /// hand the cell to `SnapshotCache::new` for their own
    /// `snapshot_cache()` trait method.
    pub fn cell_arc(&self) -> Arc<ArcSwap<DocumentSnapshot>> {
        self.cell.clone()
    }

    /// Wait-free read. Returns an `Arc<DocumentSnapshot>` that lives
    /// as long as the caller needs it. Renderers call this once per
    /// visible document per frame.
    pub fn load(&self) -> Arc<DocumentSnapshot> {
        self.cell.load_full()
    }

    /// Replace the published snapshot with `next`. Atomic; any
    /// reader that observes the new pointer also observes all writes
    /// the publisher ordered before the store. Promoted from
    /// `pub(crate)` so external `Document` impls publish their
    /// own snapshots; the actor model in this crate is no longer
    /// the only writer.
    pub fn store(&self, next: DocumentSnapshot) {
        self.cell.store(Arc::new(next));
    }

    /// Bench-only `pub` wrapper around [`Self::store`]. See the
    /// note on [`DocumentSnapshot::__bench_from_document`].
    #[doc(hidden)]
    pub fn __bench_store(&self, next: DocumentSnapshot) {
        self.store(next);
    }

    /// Bench-only `pub` constructor matching [`Self::new`]. Lets
    /// benches stand up a `PublishedSnapshot` outside the actor's
    /// usual `spawn_document` path.
    #[doc(hidden)]
    pub fn __bench_new(initial: DocumentSnapshot) -> Self {
        Self::new(initial)
    }
}

/// Per-thread cached reader for a [`PublishedSnapshot`].
///
/// `arc_swap::Cache::load` is wait-free thread-local-cached: when
/// the underlying ArcSwap pointer is unchanged, the load is one
/// `Relaxed` atomic compare and returns the cached `Arc` reference
/// at no further cost. When the pointer changes (post-publish),
/// the next load reloads the new `Arc` and caches it.
///
/// **Per-thread state.** `Cache` is `Send` but `!Sync`. Each
/// reader thread owns its own `SnapshotCache`; the App's renderer
/// thread (the editor mainloop) is the canonical user. Multiple
/// threads reading the same document each construct their own
/// cache from clones of the underlying `Arc<PublishedSnapshot>`.
///
/// Backs the §5.6.8 commitment that the read path lives at the
/// hardware floor (~2ns per load) when the writer hasn't changed
/// the snapshot since the last frame -- the common case for a
/// renderer reading multiple times per frame between edits.
pub struct SnapshotCache {
    cache: arc_swap::Cache<Arc<ArcSwap<DocumentSnapshot>>, Arc<DocumentSnapshot>>,
}

/// Placeholder cache for `Editor::default()` headless / test
/// scaffolding. Backed by a fresh `ArcSwap<DocumentSnapshot>`
/// cell carrying a default snapshot. Real construction goes
/// through [`SnapshotCache::new`] from the document's
/// `PublishedSnapshot::cell_arc()`; `Editor::new(...)`
/// overwrites this before the first frame.
impl Default for SnapshotCache {
    fn default() -> Self {
        let cell = Arc::new(ArcSwap::from_pointee(DocumentSnapshot::default()));
        Self {
            cache: arc_swap::Cache::new(cell),
        }
    }
}

impl SnapshotCache {
    /// Build a fresh cache from a clone of the published-snapshot
    /// cell. The first `load()` call pulls the current snapshot;
    /// subsequent calls reuse the cached `Arc` until the writer
    /// publishes something new.
    pub fn new(cell: Arc<PublishedSnapshot>) -> Self {
        Self {
            cache: arc_swap::Cache::new(cell.cell_arc()),
        }
    }

    /// Wait-free per-thread-cached load. Returns a reference to
    /// the cached `Arc<DocumentSnapshot>`; clone if the caller
    /// needs an owned `Arc`. **Cheaper than cloning** -- the
    /// reference is valid for as long as the cache isn't loaded
    /// again.
    #[inline]
    pub fn load(&mut self) -> &Arc<DocumentSnapshot> {
        self.cache.load()
    }

    /// Owned-`Arc` variant for callers that genuinely need to
    /// store the snapshot beyond the cache's borrow lifetime.
    /// Costs one Arc bump on top of [`Self::load`]; use sparingly.
    #[inline]
    pub fn load_arc(&mut self) -> Arc<DocumentSnapshot> {
        Arc::clone(self.cache.load())
    }
}

impl std::fmt::Debug for SnapshotCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotCache").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for PublishedSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let snap = self.load();
        f.debug_struct("PublishedSnapshot")
            .field("version", &snap.version)
            .field("text_version", &snap.text_version)
            .field("id", &snap.id)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_protocol::edit::Edit;
    use lattice_protocol::position::Position;

    #[test]
    fn snapshot_from_fresh_document_carries_empty_text() {
        let doc = Document::from_text("");
        let snap = DocumentSnapshot::from_document(&doc);
        assert_eq!(snap.text(), "");
        assert!(!snap.dirty);
        assert_eq!(snap.version, 0);
    }

    #[test]
    fn snapshot_reflects_post_edit_state() {
        let mut doc = Document::from_text("hello");
        doc.apply_edit(Edit::insert(Position::new(0, 5), " world"))
            .unwrap();
        let snap = DocumentSnapshot::from_document(&doc);
        assert_eq!(snap.text(), "hello world");
        assert!(snap.dirty);
        assert!(snap.version > 0);
        assert!(snap.text_version > 0);
    }

    #[test]
    fn published_snapshot_store_replaces_load() {
        let doc = Document::from_text("a");
        let cell = PublishedSnapshot::new(DocumentSnapshot::from_document(&doc));
        assert_eq!(cell.load().text(), "a");

        let mut doc2 = Document::from_text("b");
        doc2.apply_edit(Edit::insert(Position::new(0, 1), "c"))
            .unwrap();
        cell.store(DocumentSnapshot::from_document(&doc2));
        assert_eq!(cell.load().text(), "bc");
    }

    #[test]
    fn loaded_arcs_outlive_subsequent_stores() {
        // The renderer's contract: an Arc<DocumentSnapshot> obtained
        // at frame start stays valid for the whole frame even if the
        // actor publishes a newer snapshot mid-frame.
        let doc = Document::from_text("v1");
        let cell = PublishedSnapshot::new(DocumentSnapshot::from_document(&doc));
        let pinned = cell.load();
        let mut doc2 = Document::from_text("v2");
        doc2.apply_edit(Edit::insert(Position::new(0, 2), "!"))
            .unwrap();
        cell.store(DocumentSnapshot::from_document(&doc2));
        // pinned still reflects the original.
        assert_eq!(pinned.text(), "v1");
        assert_eq!(cell.load().text(), "v2!");
    }

    #[test]
    fn snapshot_path_uses_arc_for_zero_copy_clone() {
        let mut doc = Document::from_text("");
        doc.save_as("/tmp/lattice-snapshot-test.txt").ok();
        let snap = DocumentSnapshot::from_document(&doc);
        let cloned = snap.clone();
        // Both clones point at the same Arc<PathBuf>.
        assert!(Arc::ptr_eq(
            snap.path.as_ref().unwrap(),
            cloned.path.as_ref().unwrap()
        ));
    }
}
