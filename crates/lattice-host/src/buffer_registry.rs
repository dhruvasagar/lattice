//! Unified buffer registry (DESIGN.md §5.9).
//!
//! Every concrete buffer the App can route input through -- a code
//! [`Document`] today, a [`FileTreeBuffer`] tomorrow -- lives in
//! [`BufferRegistry`] keyed by [`BufferId`]. A single registry
//! gives `:bn` / `:bp` / `:ls` / `:bd` a consistent surface across
//! buffer kinds; multiple file trees coexist with multiple
//! documents under the same shape.
//!
//! ## Threading model (B'.1b)
//!
//! `BufferRegistry` uses interior mutability via
//! `Arc<Mutex<BufferRegistryInner>>`. Every method takes `&self`
//! and locks briefly. The registry is `Clone` (cheap atomic
//! bump) so the App's `BufferStore` service impl can hold the
//! same state as the App's `buffers` field — modes call into
//! the shared store from any thread; the App accesses the same
//! data through its direct field.
//!
//! Methods that previously returned `Option<&BufferEntry>` (and
//! kind-specific equivalents like `document(id)`) are replaced
//! with two flavours:
//!
//! - **Owned-return convenience methods** for common patterns
//!   (`document_handle`, `name_of`, `kind_of`, `flags_of`,
//!   `document_dirty`, `document_path`, `entry_summary`).
//! - **Callback methods** (`with_entry`, `with_document`, etc.)
//!   for one-off access. The callback runs while the lock is
//!   held; callers MUST NOT re-enter the registry from inside
//!   (it would deadlock on the same `Mutex`).
//!
//! Help buffers stay overlay-rendered for v1 (transient popup),
//! so they're not in the registry yet -- moving them in is a
//! follow-up that doesn't require structural change.
//!
//! Hot-path access: the *active* document's actor handle, syntax
//! state, and last-parsed-version live on [`crate::app::App`]
//! directly so motion / dispatch code stays unchanged. Switching
//! the active document snapshots those fields back into the
//! matching registry entry and loads from the destination's.
//!
//! [`Document`]: lattice_core::Document
//! [`FileTreeBuffer`]: crate::file_tree::FileTreeBuffer

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};


use crate::buffers::{BufferFlags, BufferId, BufferKind};
use crate::file_tree::FileTreeBuffer;
use crate::help::HelpBuffer;
use crate::oil::OilBuffer;
use lattice_terminal::buffer::TerminalBuffer;

/// Per-document registry payload. Each entry carries the actor
/// handle plus per-document tree-sitter `Syntax` state, fold
/// list, and any other "lives with this buffer until it
/// closes" state.
///
/// **Active vs inactive split.** The currently-active buffer's
/// `syntax` / `folds` slots are conventionally `None` / empty
/// because the live state lives on `App.syntax` / `App.folds`
/// for hot-path access. Switching buffers via
/// `App::activate_document` snapshots the old buffer's live
/// state into its entry, then loads the destination's state
/// from its entry into the App's hot-path fields. The
/// `App::activate_buffer_state` hook then refreshes anything
/// that needs recomputing for the newly-active buffer (e.g.
/// fold recompute when switching into a buffer for the first
/// time).
#[derive(Debug)]
pub struct DocumentEntry {
    pub id: BufferId,
    /// M.0 (2026-05-31): typed as `Arc<dyn Document>` so the
    /// registry can hold either a regular `RopeDocumentHandle`
    /// (today) or a `MultibufferDocumentHandle` (M.1) without
    /// kind-branching at retrieval. Consumers read through
    /// the `Document` trait directly; concrete-type access
    /// (e.g., `RopeDocumentHandle::replace`-equivalent operations
    /// that don't apply to multibuffer) is gone — slot
    /// replacement / membership APIs handle those cases.
    pub handle: std::sync::Arc<dyn lattice_runtime::Document>,
    // M.3.2.c.5: `syntax`, `last_parsed_text_version`,
    // `last_synced_syntax_version`, and `folds` retired off the
    // entry. They live in `App.buffer_locals[id]` as
    // [`crate::modes::DocumentSyntax`] /
    // [`crate::modes::DocumentLastParsedTextVersion`] /
    // [`crate::modes::DocumentLastSyncedSyntaxVersion`] /
    // [`crate::modes::DocumentFolds`]. Reads route through
    // `App::document_syntax_for` and friends; writes go through
    // `App::seed_document_entry_locals` /
    // `App::seed_active_document_locals` at the activation
    // boundary helpers in `app/lifecycle.rs`.
}

/// One slot in the registry. The kind-specific data lives in
/// [`BufferData`]; flags + id + name apply uniformly.
///
/// `name` is the buffer's synthetic display label when there is
/// no physical file backing it. For path-backed Documents it
/// stays `None` and the status line / picker fall back to the
/// path. For synthetic buffers like `*lsp*`, `*messages*`, or
/// `*lsp:rust-analyzer:lattice*`, the owning subsystem sets the
/// name at construction time; the status line + `:ls` + buffer
/// picker all surface that label uniformly. Help buffers carry
/// their own `title` field (used by help-mode link / anchor
/// machinery) and don't use `name`.
#[derive(Debug)]
pub struct BufferEntry {
    pub id: BufferId,
    pub flags: BufferFlags,
    pub data: BufferData,
    pub name: Option<String>,
}

impl BufferEntry {
    pub fn kind(&self) -> BufferKind {
        match &self.data {
            BufferData::Document(_) => BufferKind::Document,
            BufferData::FileTree(_) => BufferKind::FileTree,
            BufferData::Help(_) => BufferKind::Help,
            BufferData::Oil(_) => BufferKind::Oil,
            BufferData::Terminal(_) => BufferKind::Terminal,
            BufferData::Messages(_) => BufferKind::Messages,
        }
    }

    /// The Document storage for any rope-backed-doc kind.
    /// Returns `Some` for both [`BufferData::Document`] and
    /// [`BufferData::Messages`] (their storage is identical;
    /// only the kind tag differs); `None` for other kinds.
    /// Code that needs to differentiate Messages from a user
    /// Document branches on [`Self::kind`].
    pub fn document(&self) -> Option<&DocumentEntry> {
        match &self.data {
            BufferData::Document(d) | BufferData::Messages(d) => Some(d),
            _ => None,
        }
    }

    pub fn document_mut(&mut self) -> Option<&mut DocumentEntry> {
        match &mut self.data {
            BufferData::Document(d) | BufferData::Messages(d) => Some(d),
            _ => None,
        }
    }

    pub fn file_tree(&self) -> Option<&FileTreeBuffer> {
        match &self.data {
            BufferData::FileTree(t) => Some(t),
            _ => None,
        }
    }

    pub fn file_tree_mut(&mut self) -> Option<&mut FileTreeBuffer> {
        match &mut self.data {
            BufferData::FileTree(t) => Some(t),
            _ => None,
        }
    }

    pub fn help(&self) -> Option<&HelpBuffer> {
        match &self.data {
            BufferData::Help(h) => Some(h),
            _ => None,
        }
    }

    pub fn help_mut(&mut self) -> Option<&mut HelpBuffer> {
        match &mut self.data {
            BufferData::Help(h) => Some(h),
            _ => None,
        }
    }

    pub fn oil(&self) -> Option<&OilBuffer> {
        match &self.data {
            BufferData::Oil(o) => Some(o),
            _ => None,
        }
    }

    pub fn oil_mut(&mut self) -> Option<&mut OilBuffer> {
        match &mut self.data {
            BufferData::Oil(o) => Some(o),
            _ => None,
        }
    }

    pub fn terminal(&self) -> Option<&TerminalBuffer> {
        match &self.data {
            BufferData::Terminal(t) => Some(t),
            _ => None,
        }
    }

    pub fn terminal_mut(&mut self) -> Option<&mut TerminalBuffer> {
        match &mut self.data {
            BufferData::Terminal(t) => Some(t),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum BufferData {
    Document(DocumentEntry),
    FileTree(FileTreeBuffer),
    /// Help / log / picker-listing buffers placed into a pane
    /// (DESIGN.md §5.9, §5.11). The transient overlay path
    /// (`App.popup_buffer`) remains for popup-style displays
    /// (hover, doc lookups, error toasts); persistent help views
    /// (`:lsp-log`, `:lsp-server-log`, `:lsp-trace-log`,
    /// `:describe-*`, `:diagnostics`) route here so they live in
    /// a real pane, can be split, switched, listed via `:ls`,
    /// and updated live when their backing source emits events.
    Help(HelpBuffer),
    /// Flat editable directory listing (oil.nvim-style).
    Oil(OilBuffer),
    Terminal(TerminalBuffer),
    /// The editor's `*messages*` audit transcript. Storage is
    /// identical to [`BufferData::Document`] (same
    /// [`DocumentEntry`]); the discriminator exists so `:ls`,
    /// modeline, and introspection paths can tell the transcript
    /// apart from a user-edited file. The subsystem owns content;
    /// `messages-mode` contributes `ReadOnly` + `NoFile`.
    Messages(DocumentEntry),
}

#[derive(Debug, Default)]
struct BufferRegistryInner {
    by_id: HashMap<BufferId, BufferEntry>,
}

/// The App's buffer registry. Methods take `&self` and lock
/// internally; the registry is `Clone` so the App's
/// `BufferStore` service impl can hold a clone for cross-thread
/// access.
///
/// Perf plan B.4.b: carries an `AtomicU64` version counter
/// alongside the inner. Every mutating method (`insert`, `remove`,
/// `set_flags`, `set_name`, and every `with_*_mut` closure
/// accessor) bumps it. `Versioned<T>`'s `DerefMut` discipline can't
/// fire here because the registry uses interior mutability —
/// callers take `&self` rather than `&mut self`, so autoref through
/// `DerefMut` is impossible. The atomic is shared via the same
/// `Arc<...>` clone as `inner` so every registry handle sees the
/// same counter; `Clone` is one Arc bump for the pair.
///
/// `version()` is the read API. The `BuffersRenderState` /
/// `TabsRenderState` caches on `Editor::publish_cache` compare
/// against the prior captured value to decide cache reuse vs.
/// rebuild.
#[derive(Clone, Debug, Default)]
pub struct BufferRegistry {
    inner: Arc<Mutex<BufferRegistryInner>>,
    /// Perf plan B.4.b: monotonic version. Bumped by every
    /// mutating method on `BufferRegistry`. `AtomicU64` is
    /// `Default` (=0) so this slot composes with the existing
    /// `#[derive(Default)]` without an explicit `Default` impl.
    version: Arc<std::sync::atomic::AtomicU64>,
}

impl BufferRegistry {
    /// Perf plan B.4.b: monotonic mutation counter. Read by the
    /// publish cache (see [`crate::render_state::PublishCache`])
    /// to decide whether the cached `buffers` / `tabs` Arcs can be
    /// reused across publishes. `Relaxed` ordering is fine because
    /// the publish path runs on the actor thread; we only need the
    /// counter to advance after a mutation, not to synchronise
    /// memory order with other writers.
    pub fn version(&self) -> u64 {
        self.version.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[inline]
    fn bump_version(&self) {
        self.version
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

fn lock_inner(inner: &Arc<Mutex<BufferRegistryInner>>) -> MutexGuard<'_, BufferRegistryInner> {
    inner.lock().expect("BufferRegistry mutex poisoned")
}

impl BufferRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- Mutation ----------------------------------------

    pub fn insert(&self, entry: BufferEntry) {
        lock_inner(&self.inner).by_id.insert(entry.id, entry);
        // Perf plan B.4.b: bump after the mutation so the publish
        // cache sees the new state on its next sub-state check.
        self.bump_version();
    }

    pub fn remove(&self, id: BufferId) -> Option<BufferEntry> {
        let removed = lock_inner(&self.inner).by_id.remove(&id);
        // Only bump on an actual removal so a no-op `remove` of a
        // non-existent id doesn't invalidate the cache.
        if removed.is_some() {
            self.bump_version();
        }
        removed
    }

    // ---- Owned-return reads ------------------------------

    pub fn contains(&self, id: BufferId) -> bool {
        lock_inner(&self.inner).by_id.contains_key(&id)
    }

    pub fn len(&self) -> usize {
        lock_inner(&self.inner).by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        lock_inner(&self.inner).by_id.is_empty()
    }

    /// Kind of the entry at `id`, or `None` if absent.
    pub fn kind_of(&self, id: BufferId) -> Option<BufferKind> {
        lock_inner(&self.inner).by_id.get(&id).map(|e| e.kind())
    }

    /// Synthetic name of the entry at `id`, or `None` if absent
    /// or if the entry has no name set.
    pub fn name_of(&self, id: BufferId) -> Option<String> {
        lock_inner(&self.inner)
            .by_id
            .get(&id)
            .and_then(|e| e.name.clone())
    }

    /// Per-buffer flags. `BufferFlags` is `Copy` so this returns
    /// owned without locking issues.
    pub fn flags_of(&self, id: BufferId) -> Option<BufferFlags> {
        lock_inner(&self.inner).by_id.get(&id).map(|e| e.flags)
    }

    /// Mutate the per-buffer flags via callback (e.g. flip
    /// `listed` on `:setlocal nobuflisted` once that lands).
    pub fn set_flags(&self, id: BufferId, flags: BufferFlags) -> bool {
        let mut inner = lock_inner(&self.inner);
        let updated = match inner.by_id.get_mut(&id) {
            Some(e) => {
                e.flags = flags;
                true
            }
            None => false,
        };
        // Drop the lock before bumping the atomic — the version is
        // a separate Arc and doesn't need the inner lock.
        drop(inner);
        if updated {
            self.bump_version();
        }
        updated
    }

    /// Rename the entry's synthetic `name` slot. Used by the
    /// supervisor when an actor exits and the per-instance buffer
    /// gets renamed `*lsp:rust:/path*` → `*lsp:rust:/path (exited)*`.
    pub fn set_name(&self, id: BufferId, name: Option<String>) -> bool {
        let mut inner = lock_inner(&self.inner);
        let updated = match inner.by_id.get_mut(&id) {
            Some(e) => {
                e.name = name;
                true
            }
            None => false,
        };
        drop(inner);
        if updated {
            self.bump_version();
        }
        updated
    }

    /// Kind-specific convenience: clone the `Arc<dyn Document>`
    /// for `id`. The handle is `Send + Sync` and can be held
    /// across thread boundaries. M.0: returns the polymorphic
    /// shape (was `RopeDocumentHandle` pre-M.0) so the registry
    /// serves multibuffer handles (M.1) through the same path.
    pub fn document_handle(
        &self,
        id: BufferId,
    ) -> Option<std::sync::Arc<dyn lattice_runtime::Document>> {
        lock_inner(&self.inner)
            .by_id
            .get(&id)
            .and_then(|e| e.document().map(|d| d.handle.clone()))
    }

    /// D.3.a.1 (2026-05-29): reverse lookup —
    /// `DocumentId` → `BufferId`. Used by the
    /// diff subsystem's `DocumentBufferResolver` impl: bus
    /// events carry `DocumentId`, host-side state is keyed by
    /// `BufferId`. Scans the registry's document entries —
    /// O(N_documents) per call, acceptable at v1 buffer counts
    /// (~tens; the LSP fan-in and the keymap registry do
    /// similar walks). Future inverse index lives behind the
    /// same method signature.
    pub fn buffer_id_for_document(
        &self,
        document_id: lattice_protocol::ids::DocumentId,
    ) -> Option<BufferId> {
        let inner = lock_inner(&self.inner);
        for (buffer_id, entry) in inner.by_id.iter() {
            if let Some(d) = entry.document() {
                if d.handle.id() == document_id {
                    return Some(*buffer_id);
                }
            }
        }
        None
    }

    /// Kind-specific convenience: path of the document at `id`.
    pub fn document_path(&self, id: BufferId) -> Option<std::path::PathBuf> {
        lock_inner(&self.inner)
            .by_id
            .get(&id)
            .and_then(|e| e.document().and_then(|d| d.handle.path()))
    }

    /// Kind-specific convenience: dirty flag for the document
    /// at `id`. Returns `false` for absent / non-document.
    pub fn document_dirty(&self, id: BufferId) -> bool {
        lock_inner(&self.inner)
            .by_id
            .get(&id)
            .and_then(|e| e.document().map(|d| d.handle.dirty()))
            .unwrap_or(false)
    }

    pub fn contains_document(&self, id: BufferId) -> bool {
        lock_inner(&self.inner)
            .by_id
            .get(&id)
            .map(|e| matches!(e.data, BufferData::Document(_) | BufferData::Messages(_)))
            .unwrap_or(false)
    }

    /// True iff the entry at `id` is the `*messages*` transcript.
    /// Symmetric with [`Self::contains_help`] /
    /// [`Self::contains_oil`] / [`Self::contains_file_tree`] —
    /// callers that need to differentiate the Messages identity
    /// from a regular Document branch on this.
    pub fn contains_messages(&self, id: BufferId) -> bool {
        lock_inner(&self.inner)
            .by_id
            .get(&id)
            .map(|e| matches!(e.data, BufferData::Messages(_)))
            .unwrap_or(false)
    }

    pub fn contains_file_tree(&self, id: BufferId) -> bool {
        lock_inner(&self.inner)
            .by_id
            .get(&id)
            .map(|e| matches!(e.data, BufferData::FileTree(_)))
            .unwrap_or(false)
    }

    pub fn contains_help(&self, id: BufferId) -> bool {
        lock_inner(&self.inner)
            .by_id
            .get(&id)
            .map(|e| matches!(e.data, BufferData::Help(_)))
            .unwrap_or(false)
    }

    pub fn contains_oil(&self, id: BufferId) -> bool {
        lock_inner(&self.inner)
            .by_id
            .get(&id)
            .map(|e| matches!(e.data, BufferData::Oil(_)))
            .unwrap_or(false)
    }

    /// Compact snapshot of an entry: `(id, kind, flags, name,
    /// is_document_path_set)`. Used by `:ls` and the buffer picker
    /// to render rows without holding the lock through complex
    /// display logic. Returns `None` if the entry is absent.
    pub fn entry_summary(
        &self,
        id: BufferId,
    ) -> Option<(BufferId, BufferKind, BufferFlags, Option<String>)> {
        let inner = lock_inner(&self.inner);
        inner
            .by_id
            .get(&id)
            .map(|e| (e.id, e.kind(), e.flags, e.name.clone()))
    }

    /// All ids in ascending order. Used by `:bn` / `:bp` for
    /// deterministic cycling order independent of HashMap
    /// hash-randomization.
    pub fn sorted_ids(&self) -> Vec<BufferId> {
        let inner = lock_inner(&self.inner);
        let mut ids: Vec<BufferId> = inner.by_id.keys().copied().collect();
        ids.sort();
        ids
    }

    /// All listed ids in ascending order. `:bn` / `:bp` skip
    /// unlisted buffers (vim semantics); `:ls` shows them under a
    /// separate header (post-v1 polish).
    pub fn listed_ids_sorted(&self) -> Vec<BufferId> {
        let inner = lock_inner(&self.inner);
        let mut ids: Vec<BufferId> = inner
            .by_id
            .iter()
            .filter(|(_, e)| e.flags.listed)
            .map(|(id, _)| *id)
            .collect();
        ids.sort();
        ids
    }

    /// Document buffers only, sorted by id. The `*messages*`
    /// transcript stores as [`BufferData::Messages`] (see
    /// [`Self::messages_ids_sorted`]) so it is **excluded** from
    /// this list even though storage is identical; callers that
    /// want every rope-backed-doc kind should walk both.
    pub fn document_ids_sorted(&self) -> Vec<BufferId> {
        let inner = lock_inner(&self.inner);
        let mut ids: Vec<BufferId> = inner
            .by_id
            .iter()
            .filter(|(_, e)| matches!(e.data, BufferData::Document(_)))
            .map(|(id, _)| *id)
            .collect();
        ids.sort();
        ids
    }

    /// `*messages*` (and any future Messages-kind) buffers,
    /// sorted by id. Symmetric with [`Self::document_ids_sorted`].
    pub fn messages_ids_sorted(&self) -> Vec<BufferId> {
        let inner = lock_inner(&self.inner);
        let mut ids: Vec<BufferId> = inner
            .by_id
            .iter()
            .filter(|(_, e)| matches!(e.data, BufferData::Messages(_)))
            .map(|(id, _)| *id)
            .collect();
        ids.sort();
        ids
    }

    /// File-tree buffers only, sorted by id.
    pub fn file_tree_ids_sorted(&self) -> Vec<BufferId> {
        let inner = lock_inner(&self.inner);
        let mut ids: Vec<BufferId> = inner
            .by_id
            .iter()
            .filter(|(_, e)| matches!(e.data, BufferData::FileTree(_)))
            .map(|(id, _)| *id)
            .collect();
        ids.sort();
        ids
    }

    /// Help buffers only, sorted by id.
    pub fn help_ids_sorted(&self) -> Vec<BufferId> {
        let inner = lock_inner(&self.inner);
        let mut ids: Vec<BufferId> = inner
            .by_id
            .iter()
            .filter(|(_, e)| matches!(e.data, BufferData::Help(_)))
            .map(|(id, _)| *id)
            .collect();
        ids.sort();
        ids
    }

    pub fn oil_ids_sorted(&self) -> Vec<BufferId> {
        let inner = lock_inner(&self.inner);
        let mut ids: Vec<BufferId> = inner
            .by_id
            .iter()
            .filter(|(_, e)| matches!(e.data, BufferData::Oil(_)))
            .map(|(id, _)| *id)
            .collect();
        ids.sort();
        ids
    }

    /// IDs of every registered file-tree buffer, in arbitrary
    /// order. The App-side `file_tree_with_root` walks these +
    /// probes each one's `FileTreeRoot` buffer-local for the
    /// dedup lookup; the registry can't do that walk because it
    /// doesn't own buffer-locals.
    pub fn file_tree_ids(&self) -> Vec<BufferId> {
        lock_inner(&self.inner)
            .by_id
            .values()
            .filter_map(|entry| match &entry.data {
                BufferData::FileTree(_) => Some(entry.id),
                _ => None,
            })
            .collect()
    }

    /// IDs of every registered oil buffer, in arbitrary order.
    pub fn oil_ids(&self) -> Vec<BufferId> {
        lock_inner(&self.inner)
            .by_id
            .values()
            .filter_map(|entry| match &entry.data {
                BufferData::Oil(_) => Some(entry.id),
                _ => None,
            })
            .collect()
    }

    /// First buffer whose `name` matches exactly. Used by
    /// subsystem-owned synthetic buffers (`*lsp*`, `*messages*`,
    /// per-instance LSP log buffers) so re-running the
    /// owner's create-or-activate path surfaces the existing
    /// entry rather than allocating a duplicate.
    pub fn by_name(&self, name: &str) -> Option<BufferId> {
        let inner = lock_inner(&self.inner);
        for entry in inner.by_id.values() {
            if entry.name.as_deref() == Some(name) {
                return Some(entry.id);
            }
        }
        None
    }

    /// First help buffer with the given title, if any. Used by the
    /// `:lsp-log` / `:lsp-trace-log` openers so re-running the
    /// command surfaces the existing buffer rather than allocating
    /// a duplicate.
    pub fn help_with_title(&self, title: &str) -> Option<BufferId> {
        let inner = lock_inner(&self.inner);
        for entry in inner.by_id.values() {
            if let BufferData::Help(h) = &entry.data
                && h.title == title
            {
                return Some(entry.id);
            }
        }
        None
    }

    /// First document buffer with the given path, if any. Used by
    /// `:e FILE` to detect "already open".
    pub fn document_with_path(&self, path: &std::path::Path) -> Option<BufferId> {
        let inner = lock_inner(&self.inner);
        for entry in inner.by_id.values() {
            if let BufferData::Document(d) = &entry.data
                && d.handle.path() == Some(path.to_path_buf())
            {
                return Some(entry.id);
            }
        }
        None
    }

    // ---- Callback access ---------------------------------
    //
    // Each `with_*` method locks, looks up the entry, runs the
    // callback while the lock is held, releases. Callers MUST
    // NOT re-enter the registry from inside the callback (it
    // would deadlock on the same `Mutex`). Use the owned-return
    // helpers above to extract data when re-entry would
    // otherwise be needed.

    /// Run `f` against the `BufferEntry` at `id` while holding
    /// the registry lock. Returns `None` if the entry is absent.
    pub fn with_entry<R>(&self, id: BufferId, f: impl FnOnce(&BufferEntry) -> R) -> Option<R> {
        let inner = lock_inner(&self.inner);
        inner.by_id.get(&id).map(f)
    }

    /// Mutable variant of [`Self::with_entry`].
    ///
    /// Perf plan B.4.b: conservatively bumps the version after the
    /// closure runs, even if the closure didn't actually mutate.
    /// Over-bumping causes a one-time cache miss on the next
    /// publish — safe and bounded — versus the alternative of
    /// missing a real mutation, which would leave stale Arcs
    /// visible to renderers indefinitely.
    pub fn with_entry_mut<R>(
        &self,
        id: BufferId,
        f: impl FnOnce(&mut BufferEntry) -> R,
    ) -> Option<R> {
        let result = {
            let mut inner = lock_inner(&self.inner);
            inner.by_id.get_mut(&id).map(f)
        };
        if result.is_some() {
            self.bump_version();
        }
        result
    }

    pub fn with_document<R>(&self, id: BufferId, f: impl FnOnce(&DocumentEntry) -> R) -> Option<R> {
        let inner = lock_inner(&self.inner);
        inner.by_id.get(&id).and_then(|e| e.document()).map(f)
    }

    pub fn with_document_mut<R>(
        &self,
        id: BufferId,
        f: impl FnOnce(&mut DocumentEntry) -> R,
    ) -> Option<R> {
        // Perf plan B.4.b: document-entry mutations don't affect
        // the published `BuffersRenderState` fields directly (kind /
        // name / flags). They do invalidate the registry-derived
        // tabs label only if a name changes — which goes through
        // the dedicated `set_name`, not through `with_document_mut`.
        // We still bump because plugin code could conceivably edit
        // the wrong field through a closure; conservatism is cheaper
        // than a hard-to-find staleness bug.
        let result = {
            let mut inner = lock_inner(&self.inner);
            inner
                .by_id
                .get_mut(&id)
                .and_then(|e| e.document_mut())
                .map(f)
        };
        if result.is_some() {
            self.bump_version();
        }
        result
    }

    pub fn with_help<R>(&self, id: BufferId, f: impl FnOnce(&HelpBuffer) -> R) -> Option<R> {
        let inner = lock_inner(&self.inner);
        inner.by_id.get(&id).and_then(|e| e.help()).map(f)
    }

    pub fn with_help_mut<R>(
        &self,
        id: BufferId,
        f: impl FnOnce(&mut HelpBuffer) -> R,
    ) -> Option<R> {
        let result = {
            let mut inner = lock_inner(&self.inner);
            inner.by_id.get_mut(&id).and_then(|e| e.help_mut()).map(f)
        };
        if result.is_some() {
            self.bump_version();
        }
        result
    }

    pub fn with_file_tree<R>(
        &self,
        id: BufferId,
        f: impl FnOnce(&FileTreeBuffer) -> R,
    ) -> Option<R> {
        let inner = lock_inner(&self.inner);
        inner.by_id.get(&id).and_then(|e| e.file_tree()).map(f)
    }

    pub fn with_file_tree_mut<R>(
        &self,
        id: BufferId,
        f: impl FnOnce(&mut FileTreeBuffer) -> R,
    ) -> Option<R> {
        let result = {
            let mut inner = lock_inner(&self.inner);
            inner
                .by_id
                .get_mut(&id)
                .and_then(|e| e.file_tree_mut())
                .map(f)
        };
        if result.is_some() {
            self.bump_version();
        }
        result
    }

    pub fn with_oil<R>(&self, id: BufferId, f: impl FnOnce(&OilBuffer) -> R) -> Option<R> {
        let inner = lock_inner(&self.inner);
        inner.by_id.get(&id).and_then(|e| e.oil()).map(f)
    }

    pub fn with_oil_mut<R>(&self, id: BufferId, f: impl FnOnce(&mut OilBuffer) -> R) -> Option<R> {
        let result = {
            let mut inner = lock_inner(&self.inner);
            inner.by_id.get_mut(&id).and_then(|e| e.oil_mut()).map(f)
        };
        if result.is_some() {
            self.bump_version();
        }
        result
    }

    pub fn with_terminal<R>(&self, id: BufferId, f: impl FnOnce(&TerminalBuffer) -> R) -> Option<R> {
        let inner = lock_inner(&self.inner);
        inner.by_id.get(&id).and_then(|e| e.terminal()).map(f)
    }

    pub fn with_terminal_mut<R>(&self, id: BufferId, f: impl FnOnce(&mut TerminalBuffer) -> R) -> Option<R> {
        let result = {
            let mut inner = lock_inner(&self.inner);
            inner.by_id.get_mut(&id).and_then(|e| e.terminal_mut()).map(f)
        };
        if result.is_some() {
            self.bump_version();
        }
        result
    }

    /// Run `f` against every entry under the registry lock.
    /// Callers must not re-enter the registry from inside.
    pub fn for_each<F: FnMut(&BufferEntry)>(&self, mut f: F) {
        let inner = lock_inner(&self.inner);
        for entry in inner.by_id.values() {
            f(entry);
        }
    }
}

// ---------------------------------------------------------------
// BufferStore impl (B'.3)
// ---------------------------------------------------------------
//
// Wraps a clone of `BufferRegistry` so modes can find synthetic
// buffers by name and pull `RopeDocumentHandle`s from any tokio task.
// Registered into the App's `ServiceRegistry` at boot; modes pull
// it via `ctx.service::<lattice_mode::BufferStoreHandle>()`.
//
// `ensure_named_document` is partially implemented: it returns
// the existing id when the name is registered. Creation +
// major-mode activation is App-driven (B'.3 keeps that path on
// the App side); a future slice can wire the create path back
// through here when modes need to provision their own buffers.

impl lattice_mode::BufferStore for BufferRegistry {
    fn find_by_name(&self, name: &str) -> Option<lattice_core::BufferId> {
        self.by_name(name)
    }

    fn ensure_named_document(
        &self,
        name: &str,
        _major: lattice_mode::ModeId,
        _flags: lattice_core::BufferFlags,
    ) -> lattice_core::BufferId {
        // Return the existing id when registered (B'.3: every
        // synthetic LSP / messages buffer is created App-side
        // before its mode activates, so callers only need the
        // find half of "find-or-create" today). When `name` is
        // unknown the panic surfaces a programmer error in CI
        // rather than silently routing writes to a fresh-but-
        // un-activated buffer.
        match self.by_name(name) {
            Some(id) => id,
            None => panic!(
                "BufferStore::ensure_named_document: no buffer named {name:?}; \
                 App-side creation path not yet routed through BufferStore"
            ),
        }
    }

    fn handle_for(
        &self,
        id: lattice_core::BufferId,
    ) -> Option<std::sync::Arc<dyn lattice_runtime::Document>> {
        self.document_handle(id)
    }

    fn name_for(&self, id: lattice_core::BufferId) -> Option<String> {
        self.name_of(id)
    }
}

// ---------------------------------------------------------------
// TerminalStore impl (T-mode-1)
// ---------------------------------------------------------------
//
// Wraps `BufferRegistry` so `TerminalNormalMode`'s on-activate /
// Guard-Drop can install / clear the SyntheticDoc on a
// `TerminalBuffer` from inside the mode lifecycle. Registered at
// boot via `editor_boot.rs` and pulled by the mode via
// `ctx.service::<lattice_terminal::TerminalStoreHandle>()`.

impl lattice_terminal::TerminalStore for BufferRegistry {
    fn install_synthetic(&self, id: BufferId) -> bool {
        self.with_terminal_mut(id, |t| {
            let doc = t.term.build_normal_snapshot();
            t.synthetic = Some(std::sync::Arc::new(doc));
        })
        .is_some()
    }

    fn clear_synthetic(&self, id: BufferId) -> bool {
        self.with_terminal_mut(id, |t| {
            t.synthetic = None;
        })
        .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ft_entry(id: BufferId, listed: bool, name: Option<String>) -> BufferEntry {
        BufferEntry {
            id,
            flags: BufferFlags {
                listed,
                hidden: false,
            },
            data: BufferData::FileTree(FileTreeBuffer {
                id,
                content: lattice_core::Buffer::empty(),
                cursor: lattice_protocol::position::Position::ZERO,
                scroll: 0,
            }),
            name,
        }
    }

    #[test]
    fn fresh_registry_is_empty() {
        let r = BufferRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn sorted_ids_returns_ascending_order() {
        let r = BufferRegistry::new();
        let id_a = BufferId::next();
        let id_b = BufferId::next();
        let id_c = BufferId::next();
        r.insert(ft_entry(id_c, true, None));
        r.insert(ft_entry(id_a, true, None));
        r.insert(ft_entry(id_b, true, None));
        assert_eq!(r.sorted_ids(), vec![id_a, id_b, id_c]);
    }

    #[test]
    fn unlisted_buffers_skip_listed_ids() {
        let r = BufferRegistry::new();
        let id_a = BufferId::next();
        let id_b = BufferId::next();
        r.insert(ft_entry(id_a, true, None));
        r.insert(ft_entry(id_b, false, None));
        assert_eq!(r.listed_ids_sorted(), vec![id_a]);
        assert_eq!(r.sorted_ids(), vec![id_a, id_b]);
    }

    #[test]
    fn by_name_finds_entry_with_matching_synthetic_name() {
        let r = BufferRegistry::new();
        let id_lsp = BufferId::next();
        let id_other = BufferId::next();
        r.insert(ft_entry(id_lsp, true, Some("*lsp*".to_string())));
        r.insert(ft_entry(id_other, true, None));
        assert_eq!(r.by_name("*lsp*"), Some(id_lsp));
        assert_eq!(r.by_name("nope"), None);
    }

    #[test]
    fn file_tree_ids_lists_registered_trees() {
        let r = BufferRegistry::new();
        let id = BufferId::next();
        r.insert(ft_entry(id, true, None));
        assert_eq!(r.file_tree_ids(), vec![id]);
    }

    #[test]
    fn clone_shares_state() {
        let r1 = BufferRegistry::new();
        let r2 = r1.clone();
        let id = BufferId::next();
        r1.insert(ft_entry(id, true, None));
        assert!(r2.contains(id));
        assert_eq!(r2.len(), 1);
    }

    #[test]
    fn with_entry_runs_callback_under_lock() {
        let r = BufferRegistry::new();
        let id = BufferId::next();
        r.insert(ft_entry(id, true, Some("name".to_string())));
        let kind = r.with_entry(id, |e| e.kind());
        assert_eq!(kind, Some(BufferKind::FileTree));
    }

    #[test]
    fn set_name_updates_entry() {
        let r = BufferRegistry::new();
        let id = BufferId::next();
        r.insert(ft_entry(id, true, Some("old".to_string())));
        assert!(r.set_name(id, Some("new".to_string())));
        assert_eq!(r.name_of(id), Some("new".to_string()));
        assert_eq!(r.by_name("new"), Some(id));
        assert_eq!(r.by_name("old"), None);
    }
}
