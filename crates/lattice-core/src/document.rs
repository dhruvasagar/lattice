//! Document: a buffer plus the metadata needed for the rest of the editor.
//!
//! Phase 0 only wires the editing-relevant fields (buffer, version, undo
//! stack, selections, optional path). The full §5.1 metadata set --
//! `language`, `syntax`, `diagnostics`, `rendering_profile`, `encoding`,
//! `line_ending` -- is added when each subsystem comes online. Major /
//! minor modes (the `mode-architecture.md` mode system) live on `modes`,
//! starting empty and populated by the [`lattice_mode::ModeRegistry`]
//! when M.3 lands the per-buffer-kind major modes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_protocol::edit::{Edit, EditKind};
use lattice_protocol::ids::DocumentId;
use lattice_protocol::position::Range;
use lattice_protocol::selection::SelectionSet;

use crate::buffer::{AppliedEdit, Buffer};
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

    pub fn build(self) -> Document {
        let buffer = match self.initial_text {
            Some(t) => Buffer::from_text(&t),
            None => Buffer::empty(),
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
    /// `Event::DocumentChanged`).
    pub fn apply_edit(&mut self, edit: Edit) -> CoreResult<AppliedEdit> {
        let applied = self.buffer.apply_edit(&edit)?;
        let inverse = inverse_edit(&applied);
        let pre_push_depth = self.undo.undo_depth();
        self.undo.push(UndoEntry {
            inverse_edits: vec![inverse],
            label: String::new(),
        });
        self.invalidate_clean_if_lost(pre_push_depth);
        self.version += 1;
        self.text_version += 1;
        Ok(applied)
    }

    /// Apply a batch of edits as a single undoable unit. Edits are applied in
    /// order; undo reverts them all.
    pub fn apply_edit_batch(&mut self, edits: Vec<Edit>) -> CoreResult<Vec<AppliedEdit>> {
        let mut applied_set = Vec::with_capacity(edits.len());
        let mut inverses = Vec::with_capacity(edits.len());
        for edit in edits {
            let applied = self.buffer.apply_edit(&edit)?;
            inverses.push(inverse_edit(&applied));
            applied_set.push(applied);
        }
        // Inverses replay in reverse order during undo.
        inverses.reverse();
        let pre_push_depth = self.undo.undo_depth();
        self.undo.push(UndoEntry {
            inverse_edits: inverses,
            label: String::new(),
        });
        self.invalidate_clean_if_lost(pre_push_depth);
        self.version += 1;
        self.text_version += 1;
        Ok(applied_set)
    }

    pub fn undo(&mut self) -> CoreResult<Vec<AppliedEdit>> {
        let entry = self.undo.pop_for_undo().ok_or(CoreError::NothingToUndo)?;
        let mut applied = Vec::with_capacity(entry.inverse_edits.len());
        let mut redo_inverses = Vec::with_capacity(entry.inverse_edits.len());
        for edit in &entry.inverse_edits {
            let a = self.buffer.apply_edit(edit)?;
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
