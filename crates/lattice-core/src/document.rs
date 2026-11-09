//! Document: a buffer plus the metadata needed for the rest of the editor.
//!
//! Phase 0 only wires the editing-relevant fields (buffer, version, undo
//! stack, selections, optional path). The full §5.1 metadata set --
//! `language`, `syntax`, `diagnostics`, `rendering_profile`, `encoding`,
//! `line_ending` -- is added when each subsystem comes online. Major /
//! minor modes (the `mode-architecture.md` mode system) live on `modes`,
//! starting empty and populated by the `lattice_mode::ModeRegistry`
//! when M.3 lands the per-buffer-kind major modes.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_protocol::edit::{Edit, EditKind};
use lattice_protocol::ids::DocumentId;
use lattice_protocol::position::{Position, Range};
use lattice_protocol::selection::{Selection, SelectionSet};

use crate::buffer::{AppliedEdit, Buffer, transform_position};
use crate::error::{CoreError, CoreResult};
use crate::tracked::{AppendOutcome, TrackedRangeId, TrackedRanges};
use crate::undo::{UndoEntry, UndoStack};

/// An editable document: a [`Buffer`] plus the bookkeeping every edit needs
/// — a process-unique [`DocumentId`], an optional file path, version
/// counters, the [`SelectionSet`], the [`UndoStack`] and dirty tracking.
///
/// All text mutation goes through [`Document::apply_edit`] /
/// [`Document::apply_edit_batch`] (or [`Document::undo`] /
/// [`Document::redo`]); each one records its inverse for undo, carries the
/// selections across the change, and bumps both [`Document::version`] and
/// [`Document::text_version`]. Positions are the protocol's
/// `(line, byte-within-line)` pairs, both 0-based — see [`Buffer`].
///
/// A `Document` has no interior mutability and no locking; the editor keeps
/// it behind a single writer (the core actor). It knows nothing about
/// syntax, modes, LSP or rendering.
///
/// # Examples
///
/// Edit, undo, redo and dirty tracking:
///
/// ```
/// use lattice_core::Document;
/// use lattice_core::protocol::edit::Edit;
/// use lattice_core::protocol::position::Position;
///
/// # fn main() -> lattice_core::CoreResult<()> {
/// let mut doc = Document::from_text("hello");
/// assert!(!doc.dirty());
///
/// doc.apply_edit(Edit::insert(Position::new(0, 5), " world"))?;
/// assert_eq!(doc.text(), "hello world");
/// assert!(doc.dirty());
///
/// doc.undo()?;
/// assert_eq!(doc.text(), "hello");
/// assert!(!doc.dirty()); // back at the loaded state
///
/// doc.redo()?;
/// assert_eq!(doc.text(), "hello world");
/// # Ok(())
/// # }
/// ```
///
/// Coalescing a whole insert session into one undo step:
///
/// ```
/// use lattice_core::Document;
/// use lattice_core::protocol::edit::Edit;
/// use lattice_core::protocol::position::Position;
///
/// # fn main() -> lattice_core::CoreResult<()> {
/// let mut doc = Document::empty();
/// doc.begin_undo_group(); // `i`
/// doc.apply_edit(Edit::insert(Position::new(0, 0), "a"))?;
/// doc.apply_edit(Edit::insert(Position::new(0, 1), "b"))?;
/// doc.apply_edit(Edit::insert(Position::new(0, 2), "c"))?;
/// doc.end_undo_group(); // `<Esc>`
///
/// doc.undo()?; // one `u` removes the whole session
/// assert_eq!(doc.text(), "");
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct Document {
    id: DocumentId,
    /// `Arc<PathBuf>` so every reader that needs to *carry* the path — a
    /// published `DocumentSnapshot`, a grammar dispatch context (OM.6b) —
    /// takes an Arc bump rather than a heap allocation. Motions dispatch on
    /// every `j`, so a `PathBuf` clone there would be an allocation on the
    /// keystroke path bought for one plugin feature (paramount #1).
    /// `path()` still hands out `Option<&Path>`, so callers are unaffected.
    path: Option<Arc<PathBuf>>,
    buffer: Buffer,
    version: u64,
    /// Bumps only on text-mutating operations (edits, undo, redo). Selection
    /// changes do not bump this. Used by callers (e.g., the syntax cache)
    /// to decide whether to reparse.
    text_version: u64,
    selections: SelectionSet,
    /// Ranges kept in place across edits ([`crate::tracked`]). Behind an
    /// `Arc` so a published snapshot shares them for the cost of a bump; an
    /// edit copies only when something is tracked.
    tracked: Arc<TrackedRanges>,
    /// The edits the last commit applied to linked tracked ranges on its
    /// own — snippet mirrors copying what was typed into one of them. The
    /// caller's edit is what `apply_edit` returns; these follow it, so the
    /// host reads them here (via the published snapshot) to keep language
    /// servers, syntax and the renderer in step. Cleared by every commit.
    linked_edits: Vec<AppliedEdit>,
    undo: UndoStack,
    /// Undo-stack depth at which the buffer last matched its on-disk state
    /// (or its initial-load state, for a fresh `open` / `from_text`). The
    /// document is dirty iff the current `undo.undo_depth()` differs from
    /// this. `None` means no clean state is reachable -- typically because
    /// an `apply_edit` cleared a redo entry that contained the saved state,
    /// so we can no longer undo back to disk parity.
    clean_position: Option<usize>,
    /// Undo-coalescing group state. While `coalescing` is true, an
    /// `apply_edit` / `apply_edit_batch` folds into the most recent undo
    /// entry (via [`UndoStack::amend_top`]) instead of pushing a fresh
    /// one, so a whole vim insert session (`i` .. `<Esc>`, backspaces
    /// included) is a single undo unit. `group_has_entry` tracks whether
    /// the open group has already pushed its initial entry: the first
    /// edit in the group `push`es (creating the entry to fold into), and
    /// every edit after it amends. Both reset on
    /// [`Self::begin_undo_group`] / [`Self::end_undo_group`].
    coalescing: bool,
    group_has_entry: bool,
    /// The stream whose undo entry is the top of the stack, while it is:
    /// its next [`Self::append_to_tracked`] amends that entry rather than
    /// pushing. Any other edit, an undo or a redo seals it (sets `None`), so
    /// one `u` removes a whole response when nothing interleaved and the
    /// latest segment otherwise. A buried entry is never amended: that
    /// would break linear undo (design `llm-layer.md` §9).
    stream_top: Option<TrackedRangeId>,
    /// `text_version` after the latest [`Self::append_to_tracked`]. The host
    /// adopts the caret an append moved when this is newer than its own
    /// last write, in any buffer, not only one with an editable tail.
    append_version: u64,
    /// LL.13a: the latest appends, each with the `text_version` it produced,
    /// so a write positioned against an older version can be rebased over
    /// them ([`Self::apply_edit_based_on`]). Appends only, newest last,
    /// bounded by [`APPEND_LOG_CAP`].
    append_log: VecDeque<(u64, AppliedEdit)>,
    /// LL.13a: bumped by every undo and redo; each stream remembers the value
    /// at its previous append (`stream_fences`), and a stream that sees it
    /// moved is refused with [`AppendOutcome::Undone`].
    undo_seq: u64,
    stream_fences: HashMap<TrackedRangeId, u64>,
    // M.4 follow-up: `modes: ActiveModes` removed. The canonical
    // active-modes map is `App.active_modes: HashMap<BufferId,
    // ActiveModes>` (M.2.1) -- per-buffer, not per-document, and
    // lives on the App because mode resolution requires
    // `lattice-config` access. Removing the field here breaks the
    // `lattice-core -> lattice-mode` dep, which lets `lattice-mode`
    // gain a dep on `lattice-config` for typed-option contributions
    // without forming a cycle. See `docs/dev/architecture/mode-architecture.md`.
}

/// Builder for a [`Document`] with a path and/or initial content.
///
/// Content precedence: a buffer from [`Self::with_buffer`] wins over text
/// from [`Self::with_text`]; with neither, the document is empty. Nothing
/// here touches the filesystem — [`Document::open`] reads, the builder only
/// assembles. Every built document gets a fresh [`DocumentId`], starts at
/// version 0, and is clean.
///
/// # Examples
///
/// ```
/// use lattice_core::DocumentBuilder;
///
/// let doc = DocumentBuilder::default()
///     .with_path("/tmp/notes.txt")
///     .with_text("draft\n")
///     .build();
/// assert_eq!(doc.text(), "draft\n");
/// assert_eq!(doc.path().and_then(|p| p.to_str()), Some("/tmp/notes.txt"));
/// assert!(!doc.dirty());
/// ```
#[derive(Debug, Default)]
pub struct DocumentBuilder {
    path: Option<Arc<PathBuf>>,
    initial_text: Option<String>,
    /// K.4.11.perf-fix (2026-06-02): pre-built Buffer for callers
    /// that already hold a Rope and want to skip the
    /// `String → Buffer::from_text` round-trip. Wins over
    /// `initial_text` when both are set (no caller sets both;
    /// the precedence is documented for clarity).
    prebuilt_buffer: Option<Buffer>,
}

impl DocumentBuilder {
    /// Set the file path the document will report and [`Document::save`]
    /// will write to. Not checked for existence.
    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(Arc::new(path.into()));
        self
    }

    /// Set the initial text. Ignored if [`Self::with_buffer`] is also set.
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.initial_text = Some(text.into());
        self
    }

    /// Build the Document around a
    /// pre-existing Buffer instead of going through a String.
    /// Used by callers that already hold a Buffer (typically from
    /// `DocumentSnapshot.buffer.clone()` — `Buffer::clone` is
    /// `Rope::clone` which is Arc-backed and O(1)) and want to
    /// avoid the `Buffer → as_string() → from_text(&str)` round-
    /// trip. K.4.11's `MultibufferDocumentHandle::dispatch_with_cancel`
    /// is the primary consumer: every multibuffer keystroke ran
    /// the round-trip, allocating O(composed_size) bytes on the
    /// App thread per motion. After this fix the per-keystroke
    /// cost is one Arc bump.
    ///
    /// Slice: K.4.11.perf-fix (2026-06-02).
    pub fn with_buffer(mut self, buffer: Buffer) -> Self {
        self.prebuilt_buffer = Some(buffer);
        self
    }

    /// Build the document: fresh [`DocumentId`], version 0, a single cursor
    /// at the origin, an empty undo stack, and clean (the initial content
    /// counts as the saved state).
    pub fn build(self) -> Document {
        let buffer = match (self.prebuilt_buffer, self.initial_text) {
            // K.4.11.perf-fix: prebuilt buffer wins. Callers go
            // through `with_buffer` when they already have a Rope
            // and want to skip the String round-trip.
            (Some(b), _) => b,
            (None, Some(t)) => Buffer::from_text(&t),
            (None, None) => Buffer::empty(),
        };
        Document {
            id: next_document_id(),
            path: self.path,
            buffer,
            version: 0,
            text_version: 0,
            selections: SelectionSet::default(),
            tracked: Arc::default(),
            linked_edits: Vec::new(),
            undo: UndoStack::new(),
            // A freshly built document is "clean" at undo depth 0 -- the
            // initial buffer (whether empty, from_text, or just-loaded
            // from disk) is by definition the saved state.
            clean_position: Some(0),
            // No undo group open at construction; the first edit pushes.
            coalescing: false,
            group_has_entry: false,
            stream_top: None,
            append_version: 0,
            append_log: VecDeque::new(),
            undo_seq: 0,
            stream_fences: HashMap::new(),
        }
    }
}

fn next_document_id() -> DocumentId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    DocumentId::new(NEXT.fetch_add(1, Ordering::Relaxed))
}

impl Document {
    /// An empty, pathless, clean document.
    pub fn empty() -> Self {
        DocumentBuilder::default().build()
    }

    /// A pathless document holding `text`. It starts clean: `text` is
    /// treated as the saved state.
    pub fn from_text(text: impl Into<String>) -> Self {
        DocumentBuilder::default().with_text(text).build()
    }

    /// Construct a Document around
    /// a pre-built Buffer. Sister of [`Self::from_text`] for
    /// callers that already hold a Rope-backed Buffer and want
    /// to avoid the `as_string() → from_text(&str)` round-trip.
    /// See [`DocumentBuilder::with_buffer`] for the rationale +
    /// the K.4.11 consumer.
    ///
    /// Slice: K.4.11.perf-fix (2026-06-02).
    pub fn from_buffer(buffer: Buffer) -> Self {
        DocumentBuilder::default().with_buffer(buffer).build()
    }

    /// Read `path` from disk into a new clean document whose path is set.
    ///
    /// Blocking I/O — never call it on the UI thread.
    ///
    /// # Errors
    ///
    /// [`CoreError::Io`] if the file cannot be read, including when it is
    /// not valid UTF-8. A missing file is an error here; use
    /// [`Self::open_or_new`] for vim's `:e newfile` behaviour.
    pub fn open(path: impl AsRef<Path>) -> CoreResult<Self> {
        let path = path.as_ref().to_path_buf();
        let text = std::fs::read_to_string(&path)?;
        Ok(DocumentBuilder::default()
            .with_path(path)
            .with_text(text)
            .build())
    }

    /// Open `path`, or — when nothing is there yet — start an empty,
    /// unmodified document that will create it on the first save.
    ///
    /// vim's `:e newfile` (checked in 9.2: an empty buffer, `&modified` 0, and
    /// `:w` creates the file). Only `NotFound` becomes a new document; any
    /// other read failure — permissions, a non-UTF-8 file — is still an
    /// error, because opening an empty buffer over an unreadable file would
    /// let the first save overwrite it.
    ///
    /// Returns whether the document is new, so a caller can tell a fresh file
    /// from an existing one without a second `stat`.
    pub fn open_or_new(path: impl AsRef<Path>) -> CoreResult<(Self, bool)> {
        let path = path.as_ref();
        match Self::open(path) {
            Ok(doc) => Ok((doc, false)),
            Err(crate::CoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok((
                DocumentBuilder::default()
                    .with_path(path.to_path_buf())
                    .build(),
                true,
            )),
            Err(e) => Err(e),
        }
    }

    /// The process-unique id assigned at construction. Never reused within
    /// a process; not stable across restarts. A document the host's buffer
    /// registry holds is re-stamped with its buffer's id as it is spawned
    /// (see [`Self::set_id`]).
    pub fn id(&self) -> DocumentId {
        self.id
    }

    /// Give the document the id its owner addresses it by.
    ///
    /// `lattice_runtime::spawn_document` calls this with the buffer's id, so
    /// a buffer and its document share one id: what `DocumentOpened`,
    /// `DocumentChanged` and `DocumentClosed` carry is the same number, and a
    /// subscriber can map it back to the buffer without a registry lookup
    /// (which a close cannot do, since the entry is already gone). Ids are
    /// unique because buffer ids are; call it before the document is shared.
    pub fn set_id(&mut self, id: DocumentId) {
        self.id = id;
    }

    /// The file this document is backed by, or `None` for a scratch /
    /// synthetic document that has never been saved.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref().map(PathBuf::as_path)
    }

    /// Adopt a path that is already shared. Does NOT touch the filesystem —
    /// [`Document::save_as`] is the one that writes.
    ///
    /// For a throwaway document assembled around an existing buffer, where the
    /// path came from the snapshot that buffer was cloned from (the host's
    /// plugin-action gate, OM.6b). An Arc bump, so it costs the same as not
    /// carrying the path at all.
    pub fn set_path_shared(&mut self, path: Arc<PathBuf>) {
        self.path = Some(path);
    }

    /// The path as a shared handle, for readers that must *carry* it out of
    /// the borrow — a published `DocumentSnapshot`, a grammar dispatch
    /// context. An Arc bump, never an allocation. Prefer [`Document::path`]
    /// when a borrow will do.
    pub fn path_shared(&self) -> Option<Arc<PathBuf>> {
        self.path.clone()
    }

    /// Monotonic change counter: bumps on every edit, batch, undo, redo
    /// **and** [`Self::set_selections`]. Use it to detect "anything changed";
    /// use [`Self::text_version`] when only text matters.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Monotonic counter that bumps only when the *text* changes (edits,
    /// batches, undo, redo) — not on selection changes or saves. The syntax
    /// cache keys reparses on it.
    pub fn text_version(&self) -> u64 {
        self.text_version
    }

    /// Whether the text differs from the last saved (or initially loaded)
    /// state, as judged by undo depth.
    ///
    /// Undoing back to the saved state makes the document clean again.
    /// Once an edit discards the redo entries that led back to the saved
    /// state, no clean state is reachable and the document stays dirty
    /// until the next [`Self::save`] / [`Self::save_as`].
    pub fn dirty(&self) -> bool {
        match self.clean_position {
            Some(k) => k != self.undo.undo_depth(),
            // No reachable clean state -- the saved depth was lost when
            // an apply_edit cleared the redo stack. Document is dirty.
            None => true,
        }
    }

    /// The underlying text. Read-only: mutation must go through the
    /// document so undo, versions and selections stay consistent.
    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// The current selections (a single cursor at the origin for a fresh
    /// document). Edits transform them automatically.
    pub fn selections(&self) -> &SelectionSet {
        &self.selections
    }

    /// The ranges this document keeps in place across its edits.
    pub fn tracked_ranges(&self) -> &Arc<TrackedRanges> {
        &self.tracked
    }

    /// Change the tracked ranges: start or stop tracking, or change a
    /// range's stickiness. Bumps [`Self::version`] (a published snapshot
    /// must show the change) but not [`Self::text_version`].
    pub fn update_tracked_ranges(&mut self, f: impl FnOnce(&mut TrackedRanges)) {
        f(Arc::make_mut(&mut self.tracked));
        self.version += 1;
    }

    /// Replace the selections. Bumps [`Self::version`] but not
    /// [`Self::text_version`]. Positions are not validated against the
    /// buffer.
    pub fn set_selections(&mut self, selections: SelectionSet) {
        self.selections = selections;
        self.version += 1;
    }

    /// The whole text as a `String` — an `O(n)` allocation; see
    /// [`Buffer::as_string`] for the cheaper alternatives.
    pub fn text(&self) -> String {
        self.buffer.as_string()
    }

    /// Apply an edit, push its inverse onto the undo stack, bump the version.
    /// Returns a structural description of what changed (suitable for
    /// `Event::DocumentChanged`). Transforms selections across the edit
    /// so the caret survives owner writes (§4 of owner-write-caret.md).
    ///
    /// Inside an open [`Self::begin_undo_group`] the inverse folds into the
    /// group's entry instead of pushing a new one. Pushing a new entry
    /// clears the redo stack.
    ///
    /// # Errors
    ///
    /// Those of [`Buffer::apply_edit`]; the document is unchanged on error.
    pub fn apply_edit(&mut self, edit: Edit) -> CoreResult<AppliedEdit> {
        self.linked_edits.clear();
        let applied = self.buffer.apply_edit(&edit)?;
        self.transform_for_edit(&applied);
        let mut inverses = vec![inverse_edit(&applied)];
        self.mirror_linked(&applied, &mut inverses)?;
        // Inverses replay in reverse order during undo.
        inverses.reverse();
        self.record_inverses(inverses);
        self.version += 1;
        self.text_version += 1;
        Ok(applied)
    }

    /// Apply a batch of edits as a single undoable unit. Edits are applied in
    /// order; undo reverts them all. Transforms selections across each edit
    /// so the caret survives owner writes (§4 of owner-write-caret.md).
    ///
    /// Each edit's range is in the coordinates produced by the edits before
    /// it, not the pre-batch text. Versions bump once for the whole batch.
    ///
    /// # Errors
    ///
    /// Those of [`Buffer::apply_edit`]. **Not atomic:** edits before the
    /// failing one stay applied, and — because the undo entry is recorded
    /// only after the loop — they are not on the undo stack and versions do
    /// not bump. Callers must hand in a batch that is valid in sequence.
    ///
    /// # Examples
    ///
    /// ```
    /// use lattice_core::Document;
    /// use lattice_core::protocol::edit::Edit;
    /// use lattice_core::protocol::position::{Position, Range};
    ///
    /// # fn main() -> lattice_core::CoreResult<()> {
    /// let mut doc = Document::from_text("one two");
    /// doc.apply_edit_batch(vec![
    ///     // Delete "one " …
    ///     Edit::delete(Range::new(Position::new(0, 0), Position::new(0, 4))),
    ///     // … so "two" now starts at byte 0.
    ///     Edit::insert(Position::new(0, 3), "!"),
    /// ])?;
    /// assert_eq!(doc.text(), "two!");
    ///
    /// doc.undo()?; // the batch is one undo unit
    /// assert_eq!(doc.text(), "one two");
    /// # Ok(())
    /// # }
    /// ```
    pub fn apply_edit_batch(&mut self, edits: Vec<Edit>) -> CoreResult<Vec<AppliedEdit>> {
        self.linked_edits.clear();
        let mut applied_set = Vec::with_capacity(edits.len());
        let mut inverses = Vec::with_capacity(edits.len());
        for edit in edits {
            let applied = self.buffer.apply_edit(&edit)?;
            self.transform_for_edit(&applied);
            inverses.push(inverse_edit(&applied));
            self.mirror_linked(&applied, &mut inverses)?;
            applied_set.push(applied);
        }
        // Inverses replay in reverse order during undo.
        inverses.reverse();
        self.record_inverses(inverses);
        self.version += 1;
        self.text_version += 1;
        Ok(applied_set)
    }

    /// Append `text` at the end of tracked range `id` and grow the range over
    /// it: streamed inline output (design `llm-layer.md` §9, LL.12).
    ///
    /// The anchor is resolved here, in the same turn as the write, so no
    /// user edit can land between reading the range and writing at it. The
    /// stream's range is normally `NeverGrows`: text the user types at its
    /// edges stays outside it, so the response stays one contiguous span and
    /// the user's text stays after it, pushed down as it grows. The caret
    /// goes with it: a caret at or after the insertion point shifts, as for
    /// any edit.
    ///
    /// Undo: the append amends its own entry while that entry is the top of
    /// the stack, and starts a new one otherwise. It never uses the
    /// coalescing group an Insert session holds, so the user's `<Esc>` does
    /// not close it and the stream does not absorb the user's typing; an
    /// append during an Insert session makes the session's next keystroke
    /// start a fresh entry above it. A stream write clears redo, as any
    /// edit does.
    ///
    /// Linked ranges are not mirrored: a stream writes text, the user does
    /// not type it.
    ///
    /// # Errors
    ///
    /// Those of [`Buffer::apply_edit`]; nothing is written on error.
    pub fn append_to_tracked(
        &mut self,
        id: TrackedRangeId,
        text: &str,
    ) -> CoreResult<AppendOutcome> {
        let Some(range) = self.tracked.get(id) else {
            return Ok(AppendOutcome::Unknown);
        };
        if self
            .stream_fences
            .get(&id)
            .is_some_and(|seen| *seen != self.undo_seq)
        {
            return Ok(AppendOutcome::Undone);
        }
        if range.collapsed {
            return Ok(AppendOutcome::Collapsed);
        }
        if text.is_empty() {
            return Ok(AppendOutcome::Appended);
        }
        // Fences of streams whose range is gone are dropped as we go, so the
        // map holds at most the streams still tracked.
        if self.stream_fences.len() > 8 {
            let tracked = &self.tracked;
            self.stream_fences.retain(|f, _| tracked.get(*f).is_some());
        }
        self.stream_fences.insert(id, self.undo_seq);
        self.linked_edits.clear();
        let at = self.buffer.byte_to_position(range.end)?;
        let applied = self.buffer.apply_edit(&Edit::insert(at, text))?;
        self.transform_for_edit(&applied);
        Arc::make_mut(&mut self.tracked).set_span(id, range.start, range.end + text.len());
        let inverse = vec![inverse_edit(&applied)];
        if self.stream_top == Some(id) {
            // Still the top: amend. Redo is already clear (the push that
            // made this the top cleared it, and an undo would have sealed
            // it). Depth is unchanged, so a save made since the last
            // append no longer describes the text: the document is dirty.
            if self.clean_position == Some(self.undo.undo_depth()) {
                self.clean_position = None;
            }
            self.undo.amend_top(inverse);
        } else {
            let pre_push_depth = self.undo.undo_depth();
            self.undo.push(UndoEntry {
                inverse_edits: inverse,
                label: String::new(),
            });
            self.invalidate_clean_if_lost(pre_push_depth);
            self.stream_top = Some(id);
            // An open Insert group's entry is now buried; its next edit
            // must push, not amend the stream's entry.
            self.group_has_entry = false;
        }
        self.version += 1;
        self.text_version += 1;
        self.append_version = self.text_version;
        if self.append_log.len() == APPEND_LOG_CAP {
            self.append_log.pop_front();
        }
        self.append_log.push_back((self.text_version, applied));
        Ok(AppendOutcome::Appended)
    }

    /// `text_version` after the latest [`Self::append_to_tracked`]; 0 if none.
    pub fn append_version(&self) -> u64 {
        self.append_version
    }

    /// The `text_version` before the current trailing run of appends:
    /// [`Self::text_version`] itself when the latest change was not an
    /// append. A writer that reads the version after its own edit uses this,
    /// not `text_version`, as the version it has seen, so an append that
    /// landed just after the edit still reads as unseen (LL.13a).
    pub fn pre_append_version(&self) -> u64 {
        let mut v = self.text_version;
        for (version, _) in self.append_log.iter().rev() {
            if *version != v {
                break;
            }
            v -= 1;
        }
        v
    }

    /// The appends applied after `text_version == based_on`, oldest first,
    /// or `None` when something other than an append changed the text since,
    /// or the log no longer reaches back that far.
    fn appends_since(&self, based_on: u64) -> Option<Vec<&AppliedEdit>> {
        if based_on >= self.text_version {
            return Some(Vec::new());
        }
        let needed = usize::try_from(self.text_version - based_on).ok()?;
        let start = self.append_log.len().checked_sub(needed)?;
        let mut out = Vec::with_capacity(needed);
        for (i, (version, applied)) in self.append_log.iter().skip(start).enumerate() {
            if *version != based_on + 1 + i as u64 {
                return None;
            }
            out.push(applied);
        }
        Some(out)
    }

    /// `edit`, positioned against `text_version == based_on`, moved over the
    /// appends since so it lands on the text it was aimed at (LL.13a).
    ///
    /// A stream writes from another thread, so an edit the host positioned
    /// from its caret can reach the actor after an append it has not seen.
    /// Rebasing here, in the turn that applies the edit, is what keeps a key
    /// typed during a stream at the caret. An insert at an append's point
    /// goes after it, where the caret visibly is; a deletion ending at that
    /// point does not take the appended text with it. When the text changed
    /// by anything other than an append since `based_on`, the edit is used
    /// as given, as before.
    pub fn rebase_edit(&self, mut edit: Edit, based_on: u64) -> Edit {
        let Some(appends) = self.appends_since(based_on) else {
            return edit;
        };
        let empty = edit.range.start == edit.range.end;
        for applied in appends {
            edit.range.start = transform_position(edit.range.start, applied);
            edit.range.end = if empty {
                edit.range.start
            } else {
                transform_end(edit.range.end, applied)
            };
        }
        edit
    }

    /// `edits`, a batch positioned against `text_version == based_on` (each
    /// edit in the coordinates the ones before it produce), carried over the
    /// appends since so the batch lands on the text it was aimed at. `Err`
    /// gives the batch back untouched when something other than an append
    /// changed the text since, or the append log no longer reaches back.
    ///
    /// Each edit moves over the appends as [`Self::rebase_edit`] moves one,
    /// and the appends then move over that edit (an append at the edit's
    /// start stays before it), so the next edit meets them where they now
    /// are. An append whose text an edit covers is gone after it and moves
    /// nothing further. Positions only: the buffer is never read.
    ///
    /// # Errors
    ///
    /// The batch itself, returned when it cannot be rebased.
    pub fn rebase_edit_batch(
        &self,
        edits: Vec<Edit>,
        based_on: u64,
    ) -> Result<Vec<Edit>, Vec<Edit>> {
        let Some(appends) = self.appends_since(based_on) else {
            return Err(edits);
        };
        if appends.is_empty() {
            return Ok(edits);
        }
        let mut regions: Vec<Shift> = appends
            .iter()
            .map(|a| Shift {
                original: a.original_range,
                inserted: a.inserted_range,
            })
            .collect();
        let mut out = Vec::with_capacity(edits.len());
        for mut edit in edits {
            let text = match &edit.kind {
                EditKind::Replace { text } => text.as_str(),
            };
            let mut next = Vec::with_capacity(regions.len());
            for region in regions {
                // `edit` and `region` are positioned against the same text.
                let before = edit.range;
                let empty = before.start == before.end;
                let start = shift_position(before.start, &region);
                let end = if empty {
                    start
                } else {
                    shift_end(before.end, &region)
                };
                edit.range = Range::new(start, end);
                let appended = region.inserted.start != region.inserted.end;
                let covered = appended
                    && !empty
                    && start <= region.inserted.start
                    && end >= region.inserted.end;
                if covered || !appended {
                    continue;
                }
                // The append, moved over `before` (pre-region coordinates).
                let at = region.original.start;
                let over = Shift {
                    original: before,
                    inserted: Range::new(before.start, end_after(before.start, text)),
                };
                let moved = if at <= before.start {
                    at
                } else {
                    shift_position(at, &over)
                };
                next.push(Shift {
                    original: Range::empty(moved),
                    inserted: Range::new(
                        moved,
                        offset(moved, region.original.start, region.inserted.end),
                    ),
                });
            }
            regions = next;
            out.push(edit);
        }
        Ok(out)
    }

    /// [`Self::apply_edit_batch`] for a batch positioned against
    /// `text_version == based_on`: rebased over the appends since
    /// ([`Self::rebase_edit_batch`]) in the turn that applies it. When the
    /// text changed by anything else, `on_mismatch` decides: apply the batch
    /// as given, or refuse it with [`CoreError::Stale`].
    ///
    /// # Errors
    ///
    /// [`CoreError::Stale`] when refused; otherwise those of
    /// [`Self::apply_edit_batch`].
    pub fn apply_edit_batch_based_on(
        &mut self,
        edits: Vec<Edit>,
        based_on: u64,
        on_mismatch: BaseMismatch,
    ) -> CoreResult<Vec<AppliedEdit>> {
        let edits = match (self.rebase_edit_batch(edits, based_on), on_mismatch) {
            (Ok(edits), _) | (Err(edits), BaseMismatch::UseAsGiven) => edits,
            (Err(_), BaseMismatch::Refuse) => return Err(CoreError::Stale),
        };
        self.apply_edit_batch(edits)
    }

    /// `pos`, read against `text_version == based_on`, moved over the appends
    /// since by the rule a caret the document holds is moved by. As given
    /// when something else changed the text since.
    pub fn rebase_position(&self, pos: Position, based_on: u64) -> Position {
        match self.appends_since(based_on) {
            Some(appends) => appends.into_iter().fold(pos, transform_position),
            None => pos,
        }
    }

    /// [`Self::apply_edit`] after [`Self::rebase_edit`].
    ///
    /// # Errors
    ///
    /// Those of [`Self::apply_edit`].
    pub fn apply_edit_based_on(&mut self, edit: Edit, based_on: u64) -> CoreResult<AppliedEdit> {
        let edit = self.rebase_edit(edit, based_on);
        self.apply_edit(edit)
    }

    /// [`Self::set_selections`] with every position moved over the appends
    /// since `text_version == based_on`, the same rule a caret the document
    /// already held was moved by (LL.13a). The host writes its caret through
    /// after each key; one positioned before an append it had not seen would
    /// otherwise overwrite the caret the append moved.
    pub fn set_selections_based_on(&mut self, selections: SelectionSet, based_on: u64) {
        let Some(appends) = self.appends_since(based_on) else {
            self.set_selections(selections);
            return;
        };
        let primary = selections.primary_index();
        let moved = selections
            .all()
            .iter()
            .map(|s| {
                let mut s = *s;
                for applied in &appends {
                    s.anchor = transform_position(s.anchor, applied);
                    s.head = transform_position(s.head, applied);
                }
                s
            })
            .collect();
        self.set_selections(SelectionSet::from_parts(moved, primary));
    }

    /// Open an undo-coalescing group: every edit applied until
    /// [`Self::end_undo_group`] folds into a single undo entry rather
    /// than pushing its own. This is how a vim insert session
    /// (`i`/`a`/`o`/`cw` .. `<Esc>`) becomes one `u` step -- typed
    /// characters, in-session backspaces, and completion inserts all
    /// collapse together. Re-opening an already-open group starts a
    /// fresh coalescing run (the next edit pushes a new entry).
    pub fn begin_undo_group(&mut self) {
        self.coalescing = true;
        self.group_has_entry = false;
    }

    /// Close the group opened by [`Self::begin_undo_group`]. Subsequent
    /// edits push their own entries again. Idempotent when no group is
    /// open.
    pub fn end_undo_group(&mut self) {
        self.coalescing = false;
        self.group_has_entry = false;
    }

    /// Move everything the document anchors to its text across one applied
    /// edit: the selections and the [tracked ranges](crate::tracked). Called
    /// for every edit — `apply_edit`, `apply_edit_batch`, `undo`, `redo` — so
    /// nothing anchored here can miss one.
    fn transform_for_edit(&mut self, applied: &AppliedEdit) {
        self.transform_selections(applied);
        if !self.tracked.is_empty() {
            Arc::make_mut(&mut self.tracked).transform(&applied.delta);
        }
    }

    /// The edits the last commit applied to linked ranges by itself — see
    /// [`crate::tracked`]. Empty unless the commit's edit landed in one.
    pub fn linked_edits(&self) -> &[AppliedEdit] {
        &self.linked_edits
    }

    /// Copy what `applied` left in a linked tracked range into the other
    /// ranges of its group (snippet mirrors), recording each copy in
    /// [`Self::linked_edits`] and its inverse in `inverses`, so the copies
    /// share the edit's undo entry. Copies are applied to the buffer
    /// directly: they do not mirror again. Undo and redo never mirror —
    /// they replay the copies recorded with the edit.
    fn mirror_linked(&mut self, applied: &AppliedEdit, inverses: &mut Vec<Edit>) -> CoreResult<()> {
        if self.tracked.is_empty() || !self.tracked.has_links() {
            return Ok(());
        }
        let Some((source, group)) = self.tracked.linked_source(&applied.delta) else {
            return Ok(());
        };
        let text = |doc: &Self, id| -> CoreResult<Option<String>> {
            let Some(r) = doc.tracked.get(id) else {
                return Ok(None);
            };
            let range = Range::new(
                doc.buffer.byte_to_position(r.start)?,
                doc.buffer.byte_to_position(r.end)?,
            );
            Ok(Some(doc.buffer.slice(range)?))
        };
        let Some(wanted) = text(self, source)? else {
            return Ok(());
        };
        for sibling in self.tracked.linked_siblings(group, source) {
            let Some(r) = self.tracked.get(sibling) else {
                continue;
            };
            if text(self, sibling)?.as_deref() == Some(wanted.as_str()) {
                continue;
            }
            let range = Range::new(
                self.buffer.byte_to_position(r.start)?,
                self.buffer.byte_to_position(r.end)?,
            );
            let copy = self
                .buffer
                .apply_edit(&Edit::replace(range, wanted.clone()))?;
            self.transform_for_edit(&copy);
            inverses.push(inverse_edit(&copy));
            self.linked_edits.push(copy);
        }
        Ok(())
    }

    /// Transform this document's selections across an applied edit.
    /// Every selection's anchor and head are independently transformed
    /// so the caret survives owner writes.
    fn transform_selections(&mut self, applied: &AppliedEdit) {
        let all: Vec<Selection> = self
            .selections
            .all()
            .iter()
            .map(|sel| Selection {
                anchor: transform_position(sel.anchor, applied),
                head: transform_position(sel.head, applied),
                visual: sel.visual,
            })
            .collect();
        let primary = self.selections.primary_index();
        self.selections = SelectionSet::from_parts(all, primary);
    }

    /// Route a just-applied operation's inverse edits onto the undo
    /// stack. Outside a coalescing group (the common case) this pushes a
    /// new entry -- one operation, one undo unit. Inside an open group,
    /// the first operation pushes (creating the entry to fold into) and
    /// every operation after it amends that entry, so the whole group is
    /// a single undo unit. `inverses` arrive in stored order
    /// (reverse-application), matching [`UndoStack::push`] /
    /// [`UndoStack::amend_top`].
    fn record_inverses(&mut self, inverses: Vec<Edit>) {
        // Any other edit seals the stream's entry.
        self.stream_top = None;
        if self.coalescing && self.group_has_entry {
            // Fold into the group's existing entry. Depth is unchanged
            // and redo stays cleared (the group's first push cleared it),
            // so clean-position tracking needs no adjustment here.
            self.undo.amend_top(inverses);
        } else {
            let pre_push_depth = self.undo.undo_depth();
            self.undo.push(UndoEntry {
                inverse_edits: inverses,
                label: String::new(),
            });
            self.invalidate_clean_if_lost(pre_push_depth);
            if self.coalescing {
                self.group_has_entry = true;
            }
        }
    }

    /// Revert the most recent undo entry and move it onto the redo stack.
    /// Returns the edits as applied to the buffer, in application order, so
    /// callers can emit change events. Bumps both versions.
    ///
    /// # Errors
    ///
    /// [`CoreError::NothingToUndo`] if the undo stack is empty.
    pub fn undo(&mut self) -> CoreResult<Vec<AppliedEdit>> {
        self.linked_edits.clear();
        self.stream_top = None;
        self.undo_seq += 1;
        let entry = self.undo.pop_for_undo().ok_or(CoreError::NothingToUndo)?;
        let mut applied = Vec::with_capacity(entry.inverse_edits.len());
        let mut redo_inverses = Vec::with_capacity(entry.inverse_edits.len());
        for edit in &entry.inverse_edits {
            let a = self.buffer.apply_edit(edit)?;
            self.transform_for_edit(&a);
            redo_inverses.push(inverse_edit(&a));
            applied.push(a);
        }
        redo_inverses.reverse();
        self.undo.record_redo(UndoEntry {
            inverse_edits: redo_inverses,
            label: entry.label,
        });
        self.version += 1;
        self.text_version += 1;
        Ok(applied)
    }

    /// Re-apply the most recently undone entry and move it back onto the
    /// undo stack. The mirror of [`Self::undo`].
    ///
    /// # Errors
    ///
    /// [`CoreError::NothingToRedo`] if nothing has been undone since the
    /// last new edit.
    pub fn redo(&mut self) -> CoreResult<Vec<AppliedEdit>> {
        self.linked_edits.clear();
        self.stream_top = None;
        self.undo_seq += 1;
        let entry = self.undo.pop_for_redo().ok_or(CoreError::NothingToRedo)?;
        let mut applied = Vec::with_capacity(entry.inverse_edits.len());
        let mut undo_inverses = Vec::with_capacity(entry.inverse_edits.len());
        for edit in &entry.inverse_edits {
            let a = self.buffer.apply_edit(edit)?;
            self.transform_for_edit(&a);
            undo_inverses.push(inverse_edit(&a));
            applied.push(a);
        }
        undo_inverses.reverse();
        self.undo.record_undo(UndoEntry {
            inverse_edits: undo_inverses,
            label: entry.label,
        });
        self.version += 1;
        self.text_version += 1;
        Ok(applied)
    }

    /// Persist to the document's path. Errors if no path is set.
    ///
    /// Writes the text verbatim (blocking I/O) and marks the current undo
    /// depth as clean. Returns the path written.
    ///
    /// # Errors
    ///
    /// [`CoreError::NoPath`] for a pathless document; [`CoreError::Io`] if
    /// the write fails (the document stays dirty).
    pub fn save(&mut self) -> CoreResult<&Path> {
        let path = self.path.clone().ok_or(CoreError::NoPath)?;
        std::fs::write(path.as_path(), self.buffer.as_string())?;
        self.clean_position = Some(self.undo.undo_depth());
        // path is set; dereference safely via the stored option.
        Ok(self
            .path
            .as_deref()
            .map(PathBuf::as_path)
            .expect("path set above"))
    }

    /// Write the text to `path`, then adopt `path` as the document's path
    /// and mark it clean. On a write error neither the path nor the clean
    /// state changes.
    ///
    /// # Errors
    ///
    /// [`CoreError::Io`] if the write fails.
    pub fn save_as(&mut self, path: impl Into<PathBuf>) -> CoreResult<()> {
        let path = path.into();
        std::fs::write(&path, self.buffer.as_string())?;
        self.path = Some(Arc::new(path));
        self.clean_position = Some(self.undo.undo_depth());
        Ok(())
    }

    /// Declare the current text the document's clean state, without
    /// writing it anywhere.
    ///
    /// GD.7d: for a buffer whose text is produced, not typed — a
    /// directory listing, a file tree, a help page. When its owner
    /// replaces the text, that text is the baseline: nothing in it is
    /// an unsaved modification. Without this such a buffer reads as
    /// modified from the moment it is filled, so its modified flag says
    /// nothing; with it, the flag means "the user changed this since
    /// the owner last wrote it", and undoing back to that point clears
    /// it, exactly as undoing back to a save does.
    pub fn mark_clean(&mut self) {
        self.clean_position = Some(self.undo.undo_depth());
    }

    /// Called after an `apply_edit` that just pushed a new undo entry
    /// (and cleared the redo stack as a side effect). If the saved-clean
    /// depth lived past `pre_push_depth`, it was reachable only via the
    /// just-cleared redo entries, so we drop it -- the document can no
    /// longer reach disk-parity through undo/redo alone.
    fn invalidate_clean_if_lost(&mut self, pre_push_depth: usize) {
        if let Some(k) = self.clean_position
            && k > pre_push_depth
        {
            self.clean_position = None;
        }
    }
}

/// How many appends a write positioned against an older version can be
/// rebased over ([`Document::rebase_edit`]). A stream writes at most once a
/// frame and a key's dispatch takes well under one, so one entry is the
/// common case; the cap only bounds memory.
const APPEND_LOG_CAP: usize = 64;

/// What [`Document::apply_edit_batch_based_on`] does when the text changed
/// after the batch's version by something other than an append, so the batch
/// cannot be carried to where it was aimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseMismatch {
    /// Apply it as given: what every write did before rebasing existed. Right
    /// for a key, which the user pressed and expects to take effect.
    UseAsGiven,
    /// Refuse with [`CoreError::Stale`]. Right for a write computed from text
    /// it must still match, such as a reviewed replacement: the caller reads
    /// again and decides again.
    Refuse,
}

/// An applied change, as positions only: what [`transform_position`] reads.
#[derive(Debug, Clone, Copy)]
struct Shift {
    original: Range,
    inserted: Range,
}

fn shift_position(pos: Position, shift: &Shift) -> Position {
    crate::buffer::transform_position_over(pos, shift.original, shift.inserted)
}

/// [`transform_end`] over a [`Shift`].
fn shift_end(end: Position, shift: &Shift) -> Position {
    if shift.original.start == shift.original.end && end <= shift.original.start {
        end
    } else {
        shift_position(end, shift)
    }
}

/// Where `text` ends when inserted at `start`.
fn end_after(start: Position, text: &str) -> Position {
    match text.rfind('\n') {
        Some(nl) => Position::new(
            start.line + u32::try_from(text.matches('\n').count()).unwrap_or(u32::MAX),
            u32::try_from(text.len() - nl - 1).unwrap_or(u32::MAX),
        ),
        None => Position::new(
            start.line,
            start.byte + u32::try_from(text.len()).unwrap_or(u32::MAX),
        ),
    }
}

/// `end`, an insertion's end measured from `from`, measured from `to` instead.
fn offset(to: Position, from: Position, end: Position) -> Position {
    if end.line == from.line {
        Position::new(to.line, to.byte + (end.byte - from.byte))
    } else {
        Position::new(to.line + (end.line - from.line), end.byte)
    }
}

/// The end of a non-empty range, moved over an insertion `applied`. Unlike
/// [`transform_position`], an end exactly at the insertion point stays: the
/// inserted text is after the range, not in it.
fn transform_end(
    end: lattice_protocol::position::Position,
    applied: &AppliedEdit,
) -> lattice_protocol::position::Position {
    if applied.original_range.start == applied.original_range.end
        && end <= applied.original_range.start
    {
        end
    } else {
        transform_position(end, applied)
    }
}

fn inverse_edit(applied: &AppliedEdit) -> Edit {
    Edit {
        range: applied.inserted_range,
        kind: EditKind::Replace {
            text: applied.replaced_text.clone(),
        },
    }
}

#[allow(dead_code)]
fn _empty_range_at_origin() -> Range {
    Range::new(
        lattice_protocol::position::Position::ZERO,
        lattice_protocol::position::Position::ZERO,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_protocol::position::Position;
    use lattice_protocol::selection::Selection;

    /// Tracked ranges follow every edit path — a single edit, a batch, undo
    /// and redo — because the document moves them itself.
    #[test]
    fn tracked_ranges_follow_edits_batches_undo_and_redo() {
        use crate::tracked::{Stickiness, TrackedRangeId};
        let mut d = Document::from_text("for idx in iter {}");
        let one = TrackedRangeId::next();
        let two = TrackedRangeId::next();
        d.update_tracked_ranges(|t| {
            t.insert(one, 4..7, Stickiness::GrowsAtEdges);
            t.insert(two, 11..15, Stickiness::NeverGrows);
        });
        let bytes = |d: &Document, id| d.tracked_ranges().get(id).map(|r| r.bytes());

        d.apply_edit(Edit::insert(Position::new(0, 7), "es"))
            .unwrap();
        assert_eq!(bytes(&d, one), Some(4..9), "typing at `$1`'s end grows it");
        assert_eq!(bytes(&d, two), Some(13..17), "and pushes `$2` along");

        d.apply_edit_batch(vec![Edit::insert(Position::new(0, 0), "  ")])
            .unwrap();
        assert_eq!(bytes(&d, two), Some(15..19), "a batch moves them too");

        d.undo().unwrap();
        d.undo().unwrap();
        assert_eq!(bytes(&d, one), Some(4..7), "undo moves them back");
        assert_eq!(bytes(&d, two), Some(11..15));

        d.redo().unwrap();
        assert_eq!(bytes(&d, one), Some(4..9), "and redo forward again");
    }

    /// An edit inside one linked range is copied into the others, the
    /// copies are reported as the commit's linked edits, and one undo takes
    /// back the edit and its copies together.
    #[test]
    fn an_edit_in_a_linked_range_is_copied_into_the_others() {
        use crate::tracked::{Stickiness, TrackedRangeChange, TrackedRangeId};
        // `$1 + $1`, both copies `x`.
        let mut d = Document::from_text("x + x");
        let (a, b) = (TrackedRangeId::next(), TrackedRangeId::next());
        d.update_tracked_ranges(|t| {
            t.insert(a, 0..1, Stickiness::GrowsAtEdges);
            t.insert(b, 4..5, Stickiness::GrowsAtEdges);
            t.apply(TrackedRangeChange::Link(vec![a, b]));
        });

        // Type `y` at the end of the second copy.
        let main = d
            .apply_edit(Edit::insert(Position::new(0, 5), "y"))
            .unwrap();
        assert_eq!(d.text(), "xy + xy");
        assert_eq!(
            main.inserted_text, "y",
            "the caller's edit is returned as is"
        );
        assert_eq!(d.linked_edits().len(), 1, "the copy into the first");
        assert_eq!(d.tracked_ranges().get(b).map(|r| r.bytes()), Some(5..7));

        // Undo replays the copies recorded with the edit; it never mirrors.
        d.undo().unwrap();
        assert_eq!(d.text(), "x + x", "one undo takes back both");
        assert!(d.linked_edits().is_empty());
    }

    /// With nothing tracked, an edit does not touch the shared set — a
    /// snapshot taken before still points at the same allocation.
    #[test]
    fn an_edit_with_nothing_tracked_leaves_the_set_shared() {
        let mut d = Document::from_text("abc");
        let before = Arc::clone(d.tracked_ranges());
        d.apply_edit(Edit::insert(Position::new(0, 1), "x"))
            .unwrap();
        assert!(Arc::ptr_eq(&before, d.tracked_ranges()));
    }

    #[test]
    fn empty_document_has_empty_buffer_and_no_path() {
        let d = Document::empty();
        assert_eq!(d.text(), "");
        assert!(d.path().is_none());
        assert_eq!(d.version(), 0);
        assert!(!d.dirty());
    }

    #[test]
    fn from_text_preserves_initial_content() {
        let d = Document::from_text("hi\n");
        assert_eq!(d.text(), "hi\n");
        assert_eq!(d.version(), 0);
        assert!(!d.dirty());
    }

    #[test]
    fn each_document_has_a_unique_id() {
        let a = Document::empty();
        let b = Document::empty();
        assert_ne!(a.id(), b.id());
    }

    #[test]
    fn apply_edit_increments_version_and_marks_dirty() {
        let mut d = Document::empty();
        let v0 = d.version();
        d.apply_edit(Edit::insert(Position::ZERO, "x")).unwrap();
        assert_eq!(d.text(), "x");
        assert!(d.dirty());
        assert_eq!(d.version(), v0 + 1);
    }

    #[test]
    fn apply_edit_returns_applied_metadata() {
        let mut d = Document::from_text("abc");
        let r = Range::new(Position::new(0, 1), Position::new(0, 2));
        let applied = d.apply_edit(Edit::replace(r, "X")).unwrap();
        assert_eq!(applied.replaced_text, "b");
        assert_eq!(applied.inserted_range.start, Position::new(0, 1));
        assert_eq!(applied.inserted_range.end, Position::new(0, 2));
        assert_eq!(d.text(), "aXc");
    }

    #[test]
    fn apply_edit_batch_applies_in_order_and_is_one_undo_unit() {
        let mut d = Document::empty();
        let edits = vec![
            Edit::insert(Position::ZERO, "a"),
            Edit::insert(Position::new(0, 1), "b"),
            Edit::insert(Position::new(0, 2), "c"),
        ];
        let applied = d.apply_edit_batch(edits).unwrap();
        assert_eq!(applied.len(), 3);
        assert_eq!(d.text(), "abc");
        // One batched edit is one undo step.
        d.undo().unwrap();
        assert_eq!(d.text(), "");
    }

    #[test]
    fn undo_reverts_last_edit() {
        let mut d = Document::from_text("hello");
        let r = Range::new(Position::new(0, 0), Position::new(0, 5));
        d.apply_edit(Edit::replace(r, "world")).unwrap();
        assert_eq!(d.text(), "world");
        d.undo().unwrap();
        assert_eq!(d.text(), "hello");
    }

    #[test]
    fn redo_replays_undone_edit() {
        let mut d = Document::from_text("hello");
        d.apply_edit(Edit::insert(Position::new(0, 5), "!"))
            .unwrap();
        assert_eq!(d.text(), "hello!");
        d.undo().unwrap();
        assert_eq!(d.text(), "hello");
        d.redo().unwrap();
        assert_eq!(d.text(), "hello!");
    }

    #[test]
    fn undo_without_history_is_an_error() {
        let mut d = Document::empty();
        assert!(matches!(d.undo(), Err(CoreError::NothingToUndo)));
    }

    #[test]
    fn redo_without_history_is_an_error() {
        let mut d = Document::empty();
        assert!(matches!(d.redo(), Err(CoreError::NothingToRedo)));
    }

    #[test]
    fn applying_a_new_edit_clears_pending_redo() {
        let mut d = Document::from_text("a");
        d.apply_edit(Edit::insert(Position::new(0, 1), "b"))
            .unwrap();
        d.undo().unwrap();
        assert_eq!(d.text(), "a");
        // Diverge: instead of redoing, apply a different edit.
        d.apply_edit(Edit::insert(Position::new(0, 1), "c"))
            .unwrap();
        assert_eq!(d.text(), "ac");
        // The previous redo path is gone.
        assert!(matches!(d.redo(), Err(CoreError::NothingToRedo)));
    }

    #[test]
    fn multiple_undo_walks_back_to_origin() {
        let mut d = Document::empty();
        d.apply_edit(Edit::insert(Position::ZERO, "a")).unwrap();
        d.apply_edit(Edit::insert(Position::new(0, 1), "b"))
            .unwrap();
        d.apply_edit(Edit::insert(Position::new(0, 2), "c"))
            .unwrap();
        assert_eq!(d.text(), "abc");
        d.undo().unwrap();
        d.undo().unwrap();
        d.undo().unwrap();
        assert_eq!(d.text(), "");
    }

    #[test]
    fn batch_undo_inverses_replay_in_reverse_order() {
        // Verifies that an edit batch which mutates overlapping regions can be
        // undone correctly because inverses replay in reverse.
        let mut d = Document::from_text("xxxxx");
        let r1 = Range::new(Position::new(0, 0), Position::new(0, 2));
        let r2 = Range::new(Position::new(0, 0), Position::new(0, 2));
        d.apply_edit_batch(vec![Edit::replace(r1, "AB"), Edit::replace(r2, "CD")])
            .unwrap();
        assert_eq!(d.text(), "CDxxx");
        d.undo().unwrap();
        assert_eq!(d.text(), "xxxxx");
    }

    #[test]
    fn undo_group_collapses_edits_into_one_unit() {
        // The reported bug: per-character inserts must undo as one batch.
        let mut d = Document::empty();
        d.begin_undo_group();
        for (i, ch) in "hello".chars().enumerate() {
            d.apply_edit(Edit::insert(Position::new(0, i as u32), ch.to_string()))
                .unwrap();
        }
        d.end_undo_group();
        assert_eq!(d.text(), "hello");
        // A single undo reverts the whole session, not one char.
        d.undo().unwrap();
        assert_eq!(d.text(), "");
        // And a single redo replays it whole.
        d.redo().unwrap();
        assert_eq!(d.text(), "hello");
    }

    #[test]
    fn edits_outside_a_group_stay_separate_units() {
        // Guard against over-coalescing: normal-mode edits are individual.
        let mut d = Document::empty();
        d.apply_edit(Edit::insert(Position::ZERO, "a")).unwrap();
        d.apply_edit(Edit::insert(Position::new(0, 1), "b"))
            .unwrap();
        d.undo().unwrap();
        assert_eq!(d.text(), "a", "only the last edit reverts");
    }

    #[test]
    fn two_groups_are_two_undo_units() {
        // Two insert sessions with no intervening edit must NOT merge.
        let mut d = Document::empty();
        d.begin_undo_group();
        d.apply_edit(Edit::insert(Position::ZERO, "ab")).unwrap();
        d.apply_edit(Edit::insert(Position::new(0, 2), "cd"))
            .unwrap();
        d.end_undo_group();
        d.begin_undo_group();
        d.apply_edit(Edit::insert(Position::new(0, 4), "ef"))
            .unwrap();
        d.apply_edit(Edit::insert(Position::new(0, 6), "gh"))
            .unwrap();
        d.end_undo_group();
        assert_eq!(d.text(), "abcdefgh");
        d.undo().unwrap();
        assert_eq!(d.text(), "abcd", "second group reverts as one unit");
        d.undo().unwrap();
        assert_eq!(d.text(), "", "first group reverts as one unit");
    }

    #[test]
    fn group_coalesces_inserts_and_in_session_deletes() {
        // Backspaces typed during an insert session are part of the same
        // undo unit (vim). Mixes push + amend across edit kinds.
        let mut d = Document::empty();
        d.begin_undo_group();
        d.apply_edit(Edit::insert(Position::ZERO, "a")).unwrap();
        d.apply_edit(Edit::insert(Position::new(0, 1), "b"))
            .unwrap();
        d.apply_edit(Edit::insert(Position::new(0, 2), "c"))
            .unwrap();
        // Backspace the 'c'.
        d.apply_edit(Edit::replace(
            Range::new(Position::new(0, 2), Position::new(0, 3)),
            "",
        ))
        .unwrap();
        d.end_undo_group();
        assert_eq!(d.text(), "ab");
        d.undo().unwrap();
        assert_eq!(d.text(), "", "insert+delete session reverts as one unit");
    }

    #[test]
    fn group_coalescing_preserves_dirty_tracking() {
        let mut d = Document::empty();
        assert!(!d.dirty());
        d.begin_undo_group();
        d.apply_edit(Edit::insert(Position::ZERO, "x")).unwrap();
        d.apply_edit(Edit::insert(Position::new(0, 1), "y"))
            .unwrap();
        d.end_undo_group();
        assert!(d.dirty());
        d.undo().unwrap();
        assert!(!d.dirty(), "undo of the whole group returns to clean");
    }

    #[test]
    fn empty_group_pushes_nothing() {
        // Entering and leaving insert without typing adds no undo history.
        let mut d = Document::empty();
        d.begin_undo_group();
        d.end_undo_group();
        assert!(matches!(d.undo(), Err(CoreError::NothingToUndo)));
    }

    #[test]
    fn batch_inside_a_group_folds_into_the_session() {
        // A batched edit (e.g. completion accept) mid-session joins the
        // insert unit rather than splitting it.
        let mut d = Document::empty();
        d.begin_undo_group();
        d.apply_edit(Edit::insert(Position::ZERO, "a")).unwrap();
        d.apply_edit_batch(vec![
            Edit::insert(Position::new(0, 1), "b"),
            Edit::insert(Position::new(0, 2), "c"),
        ])
        .unwrap();
        d.apply_edit(Edit::insert(Position::new(0, 3), "d"))
            .unwrap();
        d.end_undo_group();
        assert_eq!(d.text(), "abcd");
        d.undo().unwrap();
        assert_eq!(d.text(), "", "single + batch edits collapse together");
    }

    #[test]
    fn set_selections_replaces_and_bumps_version() {
        let mut d = Document::empty();
        let v0 = d.version();
        let new_set = SelectionSet::single(Selection::cursor(Position::new(0, 4)));
        d.set_selections(new_set.clone());
        assert_eq!(d.selections(), &new_set);
        assert!(d.version() > v0);
    }

    #[test]
    fn text_version_bumps_on_text_mutations_only() {
        let mut d = Document::from_text("hello");
        let tv0 = d.text_version();
        let v0 = d.version();

        // Selection change bumps version but NOT text_version.
        d.set_selections(SelectionSet::single(Selection::cursor(Position::new(0, 2))));
        assert!(d.version() > v0);
        assert_eq!(d.text_version(), tv0);

        // Edit bumps both.
        d.apply_edit(Edit::insert(Position::new(0, 5), "!"))
            .unwrap();
        assert!(d.text_version() > tv0);

        // Undo also bumps text_version.
        let tv_after_edit = d.text_version();
        d.undo().unwrap();
        assert!(d.text_version() > tv_after_edit);

        // Redo also.
        let tv_after_undo = d.text_version();
        d.redo().unwrap();
        assert!(d.text_version() > tv_after_undo);
    }

    /// GD.7d: an owner-written baseline is clean, an edit on top of it
    /// is not, and undoing that edit is clean again.
    #[test]
    fn mark_clean_makes_the_current_text_the_baseline() {
        let mut d = Document::empty();
        d.apply_edit(Edit::insert(Position::ZERO, "listing"))
            .unwrap();
        assert!(d.dirty(), "filled by its owner, it reads as modified");
        d.mark_clean();
        assert!(!d.dirty());

        d.apply_edit(Edit::insert(Position::ZERO, "x")).unwrap();
        assert!(d.dirty());
        d.undo().unwrap();
        assert!(!d.dirty(), "back at the baseline");
    }

    #[test]
    fn save_without_path_errors() {
        let mut d = Document::empty();
        d.apply_edit(Edit::insert(Position::ZERO, "x")).unwrap();
        assert!(matches!(d.save(), Err(CoreError::NoPath)));
    }

    #[test]
    fn save_as_writes_file_and_clears_dirty() {
        let dir = tempdir();
        let path = dir.join("a.txt");
        let mut d = Document::from_text("alpha");
        // dirty starts false; making an edit sets it.
        d.apply_edit(Edit::insert(Position::new(0, 5), "!"))
            .unwrap();
        assert!(d.dirty());
        d.save_as(&path).unwrap();
        assert!(!d.dirty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha!");
        assert_eq!(d.path(), Some(path.as_path()));
        cleanup(&dir);
    }

    #[test]
    fn save_after_save_as_writes_to_remembered_path() {
        let dir = tempdir();
        let path = dir.join("b.txt");
        let mut d = Document::from_text("first");
        d.save_as(&path).unwrap();
        d.apply_edit(Edit::insert(Position::new(0, 5), "!"))
            .unwrap();
        d.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first!");
        cleanup(&dir);
    }

    #[test]
    fn open_reads_file_and_remembers_path() {
        let dir = tempdir();
        let path = dir.join("c.txt");
        std::fs::write(&path, "loaded").unwrap();
        let d = Document::open(&path).unwrap();
        assert_eq!(d.text(), "loaded");
        assert_eq!(d.path(), Some(path.as_path()));
        assert!(!d.dirty());
        cleanup(&dir);
    }

    #[test]
    fn open_or_new_starts_an_unmodified_empty_document_for_a_missing_file() {
        let dir = tempdir();
        let path = dir.join("fresh.org");
        let (d, new) = Document::open_or_new(&path).unwrap();
        assert!(new);
        assert_eq!(d.text(), "");
        assert_eq!(d.path(), Some(path.as_path()));
        assert!(!d.dirty(), "vim: `&modified` is 0 on a new file");
        assert!(!path.exists(), "opening creates nothing");
        cleanup(&dir);
    }

    #[test]
    fn open_or_new_reads_an_existing_file() {
        let dir = tempdir();
        let path = dir.join("there.txt");
        std::fs::write(&path, "loaded").unwrap();
        let (d, new) = Document::open_or_new(&path).unwrap();
        assert!(!new);
        assert_eq!(d.text(), "loaded");
        cleanup(&dir);
    }

    /// Only a missing file is new. An unreadable one stays an error, or its
    /// first save would overwrite it with an empty buffer.
    #[test]
    fn open_or_new_does_not_mask_other_read_errors() {
        let dir = tempdir();
        let path = dir.join("binary.bin");
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert!(Document::open_or_new(&path).is_err());
        cleanup(&dir);
    }

    #[test]
    fn open_missing_file_is_an_error() {
        let dir = tempdir();
        let path = dir.join("nope.txt");
        assert!(Document::open(&path).is_err());
        cleanup(&dir);
    }

    #[test]
    fn undo_back_to_initial_state_clears_dirty() {
        let mut d = Document::from_text("hi");
        assert!(!d.dirty());
        d.apply_edit(Edit::insert(Position::new(0, 2), "!"))
            .unwrap();
        assert!(d.dirty());
        d.undo().unwrap();
        assert_eq!(d.text(), "hi");
        assert!(!d.dirty(), "undo back to initial should be clean");
    }

    #[test]
    fn redoing_back_to_initial_state_keeps_dirty() {
        // Initial -> edit -> undo (clean) -> redo (dirty again).
        let mut d = Document::from_text("hi");
        d.apply_edit(Edit::insert(Position::new(0, 2), "!"))
            .unwrap();
        d.undo().unwrap();
        assert!(!d.dirty());
        d.redo().unwrap();
        assert!(d.dirty(), "redoing past clean point makes it dirty again");
    }

    #[test]
    fn undo_back_to_saved_state_clears_dirty() {
        let dir = tempdir();
        let path = dir.join("u.txt");
        let mut d = Document::from_text("alpha");
        d.save_as(&path).unwrap();
        assert!(!d.dirty());
        d.apply_edit(Edit::insert(Position::new(0, 5), "!"))
            .unwrap();
        assert!(d.dirty());
        d.undo().unwrap();
        assert_eq!(d.text(), "alpha");
        assert!(!d.dirty(), "undo back to saved state should clear dirty");
        cleanup(&dir);
    }

    #[test]
    fn save_after_edits_then_undo_back_to_save_clears_dirty() {
        // Common workflow: edit, edit, save, edit, undo. Should be clean
        // (the undo returned to saved state).
        let dir = tempdir();
        let path = dir.join("v.txt");
        let mut d = Document::from_text("a");
        d.apply_edit(Edit::insert(Position::new(0, 1), "b"))
            .unwrap();
        d.apply_edit(Edit::insert(Position::new(0, 2), "c"))
            .unwrap();
        d.save_as(&path).unwrap();
        assert!(!d.dirty());
        d.apply_edit(Edit::insert(Position::new(0, 3), "d"))
            .unwrap();
        assert!(d.dirty());
        d.undo().unwrap();
        assert_eq!(d.text(), "abc");
        assert!(!d.dirty());
        cleanup(&dir);
    }

    #[test]
    fn new_edit_destroying_redo_path_to_clean_makes_clean_unreachable() {
        // Edit (depth=1, clean at 1), undo to depth=0 (dirty - clean was
        // at 1), apply new edit which clears the redo stack. The redo
        // entry that would have led back to clean is gone.
        let dir = tempdir();
        let path = dir.join("w.txt");
        let mut d = Document::from_text("x");
        d.apply_edit(Edit::insert(Position::new(0, 1), "y"))
            .unwrap();
        d.save_as(&path).unwrap();
        assert!(!d.dirty());
        d.undo().unwrap();
        assert_eq!(d.text(), "x");
        assert!(d.dirty());
        // Apply a new edit that clears the redo entry containing clean.
        d.apply_edit(Edit::insert(Position::new(0, 1), "z"))
            .unwrap();
        // Now we're at depth=1 again, but the buffer is "xz", not the
        // saved "xy". We can't reach clean by any undo/redo.
        assert!(d.dirty());
        d.undo().unwrap();
        assert!(d.dirty(), "previous saved state is no longer reachable");
        d.redo().unwrap();
        assert!(d.dirty());
    }

    #[test]
    fn empty_document_starts_clean() {
        // Even an empty document is "clean" relative to its initial state.
        // This matches vim/emacs: an unmodified scratch buffer is not dirty.
        let d = Document::empty();
        assert!(!d.dirty());
    }

    #[test]
    fn save_resets_clean_position_to_current_depth() {
        let dir = tempdir();
        let path = dir.join("s.txt");
        let mut d = Document::from_text("a");
        d.apply_edit(Edit::insert(Position::new(0, 1), "b"))
            .unwrap();
        d.save_as(&path).unwrap();
        // After save, dirty=false. Undo should now go past clean.
        d.undo().unwrap();
        assert!(d.dirty(), "undoing past saved state is dirty");
        d.redo().unwrap();
        assert!(!d.dirty(), "redoing back to saved state is clean");
        cleanup(&dir);
    }

    fn tempdir() -> std::path::PathBuf {
        // Per-test unique directory under the OS temp area. The counter is
        // what makes it unique: parallel tests can read the same nanosecond,
        // share a directory, and one's `cleanup` deletes the other's files.
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir();
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = base.join(format!("lattice-core-test-{}-{id}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &std::path::Path) {
        let _ = std::fs::remove_dir_all(dir);
    }
}
