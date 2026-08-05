//! Document: a buffer plus the metadata needed for the rest of the editor.
//!
//! Phase 0 only wires the editing-relevant fields (buffer, version, undo
//! stack, selections, optional path). The full §5.1 metadata set --
//! `language`, `syntax`, `diagnostics`, `rendering_profile`, `encoding`,
//! `line_ending` -- is added when each subsystem comes online. Major /
//! minor modes (the `mode-architecture.md` mode system) live on `modes`,
//! starting empty and populated by the `lattice_mode::ModeRegistry`
//! when M.3 lands the per-buffer-kind major modes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_protocol::edit::{Edit, EditKind};
use lattice_protocol::ids::DocumentId;
use lattice_protocol::position::Range;
use lattice_protocol::selection::{Selection, SelectionSet};

use crate::buffer::{AppliedEdit, Buffer, transform_position};
use crate::error::{CoreError, CoreResult};
use crate::undo::{UndoEntry, UndoStack};

#[derive(Debug)]
pub struct Document {
    id: DocumentId,
    path: Option<PathBuf>,
    buffer: Buffer,
    version: u64,
    /// Bumps only on text-mutating operations (edits, undo, redo). Selection
    /// changes do not bump this. Used by callers (e.g., the syntax cache)
    /// to decide whether to reparse.
    text_version: u64,
    selections: SelectionSet,
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
    // M.4 follow-up: `modes: ActiveModes` removed. The canonical
    // active-modes map is `App.active_modes: HashMap<BufferId,
    // ActiveModes>` (M.2.1) -- per-buffer, not per-document, and
    // lives on the App because mode resolution requires
    // `lattice-config` access. Removing the field here breaks the
    // `lattice-core -> lattice-mode` dep, which lets `lattice-mode`
    // gain a dep on `lattice-config` for typed-option contributions
    // without forming a cycle. See `docs/dev/architecture/mode-architecture.md`.
}

#[derive(Debug, Default)]
pub struct DocumentBuilder {
    path: Option<PathBuf>,
    initial_text: Option<String>,
    /// K.4.11.perf-fix (2026-06-02): pre-built Buffer for callers
    /// that already hold a Rope and want to skip the
    /// `String → Buffer::from_text` round-trip. Wins over
    /// `initial_text` when both are set (no caller sets both;
    /// the precedence is documented for clarity).
    prebuilt_buffer: Option<Buffer>,
}

impl DocumentBuilder {
    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.initial_text = Some(text.into());
        self
    }

    /// K.4.11.perf-fix (2026-06-02): build the Document around a
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
    pub fn with_buffer(mut self, buffer: Buffer) -> Self {
        self.prebuilt_buffer = Some(buffer);
        self
    }

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
            undo: UndoStack::new(),
            // A freshly built document is "clean" at undo depth 0 -- the
            // initial buffer (whether empty, from_text, or just-loaded
            // from disk) is by definition the saved state.
            clean_position: Some(0),
            // No undo group open at construction; the first edit pushes.
            coalescing: false,
            group_has_entry: false,
        }
    }
}

fn next_document_id() -> DocumentId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    DocumentId::new(NEXT.fetch_add(1, Ordering::Relaxed))
}

impl Document {
    pub fn empty() -> Self {
        DocumentBuilder::default().build()
    }

    pub fn from_text(text: impl Into<String>) -> Self {
        DocumentBuilder::default().with_text(text).build()
    }

    /// K.4.11.perf-fix (2026-06-02): construct a Document around
    /// a pre-built Buffer. Sister of [`Self::from_text`] for
    /// callers that already hold a Rope-backed Buffer and want
    /// to avoid the `as_string() → from_text(&str)` round-trip.
    /// See [`DocumentBuilder::with_buffer`] for the rationale +
    /// the K.4.11 consumer.
    pub fn from_buffer(buffer: Buffer) -> Self {
        DocumentBuilder::default().with_buffer(buffer).build()
    }

    pub fn open(path: impl AsRef<Path>) -> CoreResult<Self> {
        let path = path.as_ref().to_path_buf();
        let text = std::fs::read_to_string(&path)?;
        Ok(DocumentBuilder::default()
            .with_path(path)
            .with_text(text)
            .build())
    }

    pub fn id(&self) -> DocumentId {
        self.id
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn text_version(&self) -> u64 {
        self.text_version
    }

    pub fn dirty(&self) -> bool {
        match self.clean_position {
            Some(k) => k != self.undo.undo_depth(),
            // No reachable clean state -- the saved depth was lost when
            // an apply_edit cleared the redo stack. Document is dirty.
            None => true,
        }
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    pub fn selections(&self) -> &SelectionSet {
        &self.selections
    }

    pub fn set_selections(&mut self, selections: SelectionSet) {
        self.selections = selections;
        self.version += 1;
    }

    pub fn text(&self) -> String {
        self.buffer.as_string()
    }

    /// Apply an edit, push its inverse onto the undo stack, bump the version.
    /// Returns a structural description of what changed (suitable for
    /// `Event::DocumentChanged`). Transforms selections across the edit
    /// so the caret survives owner writes (§4 of owner-write-caret.md).
    pub fn apply_edit(&mut self, edit: Edit) -> CoreResult<AppliedEdit> {
        let applied = self.buffer.apply_edit(&edit)?;
        self.transform_selections(&applied);
        let inverse = inverse_edit(&applied);
        self.record_inverses(vec![inverse]);
        self.version += 1;
        self.text_version += 1;
        Ok(applied)
    }

    /// Apply a batch of edits as a single undoable unit. Edits are applied in
    /// order; undo reverts them all. Transforms selections across each edit
    /// so the caret survives owner writes (§4 of owner-write-caret.md).
    pub fn apply_edit_batch(&mut self, edits: Vec<Edit>) -> CoreResult<Vec<AppliedEdit>> {
        let mut applied_set = Vec::with_capacity(edits.len());
        let mut inverses = Vec::with_capacity(edits.len());
        for edit in edits {
            let applied = self.buffer.apply_edit(&edit)?;
            self.transform_selections(&applied);
            inverses.push(inverse_edit(&applied));
            applied_set.push(applied);
        }
        // Inverses replay in reverse order during undo.
        inverses.reverse();
        self.record_inverses(inverses);
        self.version += 1;
        self.text_version += 1;
        Ok(applied_set)
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

    /// Transform this document's selections across an applied edit.
    /// Every selection's anchor and head are independently transformed
    /// so the caret survives owner writes. Called from `apply_edit`
    /// and `apply_edit_batch`.
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

    pub fn undo(&mut self) -> CoreResult<Vec<AppliedEdit>> {
        let entry = self.undo.pop_for_undo().ok_or(CoreError::NothingToUndo)?;
        let mut applied = Vec::with_capacity(entry.inverse_edits.len());
        let mut redo_inverses = Vec::with_capacity(entry.inverse_edits.len());
        for edit in &entry.inverse_edits {
            let a = self.buffer.apply_edit(edit)?;
            self.transform_selections(&a);
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

    pub fn redo(&mut self) -> CoreResult<Vec<AppliedEdit>> {
        let entry = self.undo.pop_for_redo().ok_or(CoreError::NothingToRedo)?;
        let mut applied = Vec::with_capacity(entry.inverse_edits.len());
        let mut undo_inverses = Vec::with_capacity(entry.inverse_edits.len());
        for edit in &entry.inverse_edits {
            let a = self.buffer.apply_edit(edit)?;
            self.transform_selections(&a);
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
    pub fn save(&mut self) -> CoreResult<&Path> {
        let path = self.path.clone().ok_or(CoreError::NoPath)?;
        std::fs::write(&path, self.buffer.as_string())?;
        self.clean_position = Some(self.undo.undo_depth());
        // path is set; dereference safely via the stored option.
        Ok(self.path.as_deref().expect("path set above"))
    }

    pub fn save_as(&mut self, path: impl Into<PathBuf>) -> CoreResult<()> {
        let path = path.into();
        std::fs::write(&path, self.buffer.as_string())?;
        self.path = Some(path);
        self.clean_position = Some(self.undo.undo_depth());
        Ok(())
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
        // Per-test unique directory under the OS temp area.
        let base = std::env::temp_dir();
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = base.join(format!("lattice-core-test-{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &std::path::Path) {
        let _ = std::fs::remove_dir_all(dir);
    }
}
