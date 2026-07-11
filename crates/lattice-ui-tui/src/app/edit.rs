//! Insert / Replace edits, paste, register store + read,
//! ex-command edit bodies (`:d` / `:g` / `:v`), and the
//! block-visual replicator. The cohesive home for any App
//! method that mutates rope state through `apply_edit_*` or
//! that reads / writes the register store.
//!
//! Methods that live here:
//! - `do_join_lines` (`J` / `gJ`).
//! - `do_toggle_case_at_cursor` (`~`).
//! - `do_paste_text`, `do_paste`, `do_paste_blockwise`
//!   (vim's `p` / `P` family + blockwise paste).
//! - `do_enter_append`, `do_enter_block_visual_insert`,
//!   `do_open_line_below`, `do_open_line_above` (Insert-
//!   mode entries that adjust cursor before mode change).
//! - `do_delete_line` (`:d`), `do_global` (`:g` / `:v`).
//! - `do_overwrite_char`, `do_replace_undo_last`,
//!   `do_insert_text`, `do_delete_char_backward` (Insert /
//!   Replace primitives).
//! - `store_yank`, `read_register` (register store
//!   read/write; the register map itself lives on `App`).
//! - `replicate_block_insert` (commit a block-visual
//!   `I` / `A` session as one undo unit).
//!
//! Stays in app.rs (deferred):
//! - `do_repeat_last_change` (`.`) -- entangled with the
//!   dispatch / dot-record path.
//! - `apply_text_edit` (the LSP-edit applier reused by
//!   substitute, formatting, code actions).
//! - `apply_edit_blocking` / `apply_edit_batch_blocking` /
//!   `undo_blocking` / `redo_blocking` -- synchronous
//!   wrappers over `Document` that pair with the App
//!   Action dispatch path; will move with the document-
//!   mutation slice.
//!
//! What does NOT live here: the rope itself (ropey,
//! wrapped by `Document`), the undo tree, the register
//! store -- those are owned by `crate::document` /
//! `crate::registers`.

use lattice_core::buffer::AppliedEdit;
use lattice_grammar::CommandInvocation;
use lattice_protocol::edit::Edit;
#[cfg(test)]
use lattice_protocol::position::Position;
use lattice_runtime::RuntimeError;

use super::App;

#[cfg(test)]
use lattice_grammar::YankKind;

impl App {
    // ---- Blocking bridges to the document actor ----
    //
    // Per DESIGN.md §5.2.1 every mutating call returns a
    // `Pending<T>`. The TUI input loop runs on a blocking thread
    // (crossterm's poll model) so it forwards each Pending to
    // [`lattice_runtime::block_on`]. These helpers concentrate the
    // bridging in one place; the rest of `App` reads as if it
    // owned `Document` directly.
    //
    // Returns are pre-flattened: callers that only care about
    // success use `.ok()`; callers that need to inspect the error
    // can match on `RuntimeError::Core(_)` for invalid edits vs.
    // `Busy` / `ActorGone` for actor-protocol failures.

    /// Block_on `apply_edit` and return the `AppliedEdit` (or
    /// `RuntimeError`). Snapshot republishes inside the actor
    /// before this returns. On success, publishes a
    /// [`Event::DocumentChanged`] to the App's event bus and
    /// records the edit with the LSP supervisor (Phase
    /// 4.1.i.2) so attached servers see `didChange`.
    ///
    /// Oil-buffer routing: when `active_buffer == Oil` the edit
    /// lands on `oil.content` (the in-memory rope owned by the
    /// `OilBuffer`) instead of the document actor's rope. The
    /// document actor is the wrong destination for oil edits --
    /// oil's content is intentionally separate so `:w` can diff
    /// against a snapshot and translate into filesystem
    /// operations. LSP `didChange` is intentionally not fired
    /// for oil edits (oil isn't an LSP-tracked buffer).
    /// 5.5.E.7.3: see [`lattice_host::dispatch::Editor::apply_edit_blocking`].
    pub(super) fn apply_edit_blocking(&mut self, edit: Edit) -> Result<AppliedEdit, RuntimeError> {
        // Slice 3c.final.E.2: route through `mutate_editor_with`.
        self.mutate_editor_with(move |e| e.apply_edit_blocking(edit))
    }

    /// 5.5.E.7.3: see [`lattice_host::dispatch::Editor::apply_edit_batch_blocking`].
    pub(super) fn apply_edit_batch_blocking(
        &mut self,
        edits: Vec<Edit>,
    ) -> Result<Vec<AppliedEdit>, RuntimeError> {
        // Slice 3c.final.E.2: route through `mutate_editor_with`.
        self.mutate_editor_with(move |e| e.apply_edit_batch_blocking(edits))
    }

    // 5.5.E.7.3: `apply_edit_to_oil` relocated to
    // [`lattice_host::dispatch::Editor::apply_edit_to_oil`] (private
    // host helper called only by the apply_edit_* methods); App-side
    // deleted entirely.

    /// 5.5.E.7.3: see [`lattice_host::dispatch::Editor::undo_blocking`].
    /// 5.5.H: kept as `#[allow(dead_code)]` -- no prod caller
    /// (Action::Undo routes through `Editor::dispatch` host-side),
    /// but several test modules in `app/edit.rs`, `app/motions.rs`,
    /// etc. construct an App, mutate it, and call `a.undo_blocking()`
    /// directly for tighter assertions than `Action::Undo` permits.
    #[allow(dead_code)]
    pub(super) fn undo_blocking(&mut self) -> Result<Vec<AppliedEdit>, RuntimeError> {
        // Slice 3c.final.E.2: route through `mutate_editor_with`.
        self.mutate_editor_with(|e| e.undo_blocking())
    }

    /// 5.5.E.7.3: see [`lattice_host::dispatch::Editor::redo_blocking`].
    /// 5.5.G.3 dropped App's `Action::Redo` caller; kept as
    /// `#[allow(dead_code)]` for symmetry with `undo_blocking`
    /// until tests / scripts that call it directly migrate.
    #[allow(dead_code)]
    pub(super) fn redo_blocking(&mut self) -> Result<Vec<AppliedEdit>, RuntimeError> {
        // Slice 3c.final.E.2: route through `mutate_editor_with`.
        self.mutate_editor_with(|e| e.redo_blocking())
    }

    /// Vim's `J` / `gJ`: join the current line with the next. With
    /// `with_space = true` (J), the joining newline becomes one space
    /// (and any leading whitespace on the next line is trimmed). With
    /// `with_space = false` (gJ), no replacement -- pure concat.
    // 5.5.G.3: `do_join_lines` + `do_toggle_case_at_cursor`
    // migrated to [`lattice_host::dispatch::Editor`].

    // 5.5.G.9: `do_paste_text`, `do_paste`, `do_paste_blockwise`,
    // `read_register` all migrated to
    // [`lattice_host::dispatch::Editor`]. `do_paste` retained as a
    // 1-line delegate below because `app/picker.rs:235` (paste-
    // picker accept) still calls it; `do_paste_text` retired in
    // 5.5.H -- bracketed-paste routes via `Action::PasteText`
    // through `Editor::dispatch`, no direct App caller remains.
    pub(super) fn do_paste(&mut self, before: bool) {
        // Slice 3c.final.E.2: route through `mutate_editor`.
        self.mutate_editor(move |e| e.do_paste(before));
    }

    // 5.5.H: `do_paste_text`, `do_enter_block_visual_insert`,
    // `do_delete_line` delegates retired (zero callers; host
    // copies live in `lattice_host::dispatch::Editor`).

    /// Vim's :g / :v -- execute `body` on every line matching (or NOT
    /// matching, when inverted) the literal pattern. Operates bottom-up
    /// so deletions don't shift the upcoming target lines. v1: `body`
    /// is parsed as a single ex-command.
    ///
    /// 5.5.E.7.6: planner moved to
    /// [`lattice_host::dispatch::Editor::build_global_targets`]; the
    /// body-replay loop stays here until the `Effect` router fully
    /// migrates host-side (a not-yet-migrated body effect would be a
    /// silent no-op via `Editor::handle_effect` alone; `apply_effect`
    /// still owns the full router today).
    pub(super) fn do_global(&mut self, pattern: &str, inverted: bool, body: &CommandInvocation) {
        // 5.8.AF.3: planning + body-replay live on `Editor::do_global`.
        // The host applies each body effect inline (so cursor-
        // positional effects land on the right line) and emits the
        // effect on `out.effects` for the renderer's App-side arms.
        // We call `apply_effect_app_arms` directly (NOT `apply_effect`)
        // because the host already called `handle_effect` host-side;
        // routing through `apply_effect` would double-apply.
        //
        // Slice 3c.final.E.5i: clone the borrowed args to owned so
        // the closure captures `Send + 'static`; build the
        // `DispatchOutcome` inside the closure and return it from
        // `mutate_editor_with` — same pattern slice E.3 documented
        // for `&mut out` results.
        let pattern = pattern.to_string();
        let body = body.clone();
        let mut out = self.mutate_editor_with(move |e| {
            let mut out = lattice_host::dispatch::DispatchOutcome::default();
            e.do_global(&pattern, inverted, &body, &mut out);
            out
        });
        for eff in std::mem::take(&mut out.effects) {
            self.apply_effect_app_arms(eff);
        }
        for signal in std::mem::take(&mut out.renderer_signals) {
            self.handle_renderer_signal(signal);
        }
    }

    /// Overstrike one char at the cursor: if the cursor is mid-line,
    /// replace `[cursor, cursor+1)` with `c`; if past EOL, just insert
    /// (vim's R extends the line). Either way the cursor advances by
    /// one byte. The original byte (or `None` if past EOL) is pushed
    /// onto `replace_history` so backspace can restore it.
    // 5.5.G.3: `do_overwrite_char` + `do_replace_undo_last`
    // migrated to [`lattice_host::dispatch::Editor`].

    /// Splice `s` at the cursor as the canonical Insert-mode insertion
    /// path: applies the edit, advances the cursor, captures the text
    /// for dot-repeat (when an Insert recording is in flight), bumps
    /// the block-visual `I` / `A` live-edit counter, refilters any
    /// open completion popup, and fires the LSP signature-help /
    /// on-type-formatting trigger autopilot when a single-char insert
    /// matches a server-advertised trigger char.
    /// 5.5.G.23.insert: body migrated to
    /// [`lattice_host::dispatch::Editor::do_insert_text`]. Retained as
    /// a thin App-side wrapper because internal App callers (the
    /// `RepeatLastChange` replay path, snippet expansion
    /// `expand_snippet`, the `do_completion_accept_then_insert` tail,
    /// the `paste-mode` guard, multi-char insert paths) still drive
    /// it directly. The wrapper drives a private `DispatchOutcome`,
    /// applies any host-emitted effects + signals, and drains
    /// `next_actions` via `self.apply` so LSP autopilot follow-ups
    /// (`LspOnTypeFormattingRequest`, `LspInsertCompletionRequest`)
    /// fire from the same dispatch loop.
    pub(super) fn do_insert_text(&mut self, s: &str) {
        // Slice 3c.final.E.5i: clone the borrowed `s` to owned, build
        // the `DispatchOutcome` inside the closure, return it from
        // `mutate_editor_with`. Same pattern as `do_global`.
        let s = s.to_string();
        let mut out = self.mutate_editor_with(move |e| {
            let mut out = lattice_host::dispatch::DispatchOutcome::default();
            e.do_insert_text(&s, &mut out);
            out
        });
        for effect in std::mem::take(&mut out.effects) {
            self.apply_effect_app_arms(effect);
        }
        for signal in std::mem::take(&mut out.renderer_signals) {
            self.handle_renderer_signal(signal);
        }
        for follow_up in std::mem::take(&mut out.next_actions) {
            self.apply(follow_up);
        }
    }

    // 5.5.G.3: `do_delete_char_backward` migrated to
    // [`lattice_host::dispatch::Editor`]. Vim `<BS>` in
    // Insert / Replace -- deletes the byte before the
    // cursor (Unicode-aware) and bumps the block-visual
    // `I` / `A` live-edit counter so the Esc replay
    // accounts for the deletion.

    // 5.5.E.3: `store_yank` moved to
    // [`lattice_host::dispatch::Editor::store_yank`] alongside the
    // [`Effect::Yank`] arm. Renderer-side call sites (the oil-buffer
    // narrow re-implementation of `apply_effect`) now invoke
    // `self.editor.store_yank(...)` directly.

    // 5.5.G.9: `read_register` migrated to
    // [`lattice_host::dispatch::Editor::read_register`] (zero
    // remaining App callers).

    // 5.5.G.17: `replicate_block_insert` migrated to
    // [`lattice_host::dispatch::Editor`]. The only caller was
    // `enter_mode`'s Insert-exit branch, which also moved
    // host-side.
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::app::test_helpers::{
        app_in_command_mode, app_with, attach_test_syntax, invoke_motion,
    };
    use crate::app::*;
    use lattice_grammar::VisualKind;
    use lattice_protocol::edit::Edit;
    use lattice_protocol::selection::VisualMode;

    #[test]
    fn undo_redo_accumulate_inverse_deltas() {
        // Forward edit + undo + redo each push a delta. The
        // undo's delta is the inverse of the forward (start_byte
        // unchanged; old_end / new_end swapped).
        let mut a = app_with("a", 5);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.apply_edit_blocking(Edit::insert(Position::new(0, 1), "b"))
            .unwrap();
        assert_eq!(a.editor.pending_syntax_edits.len(), 1);
        let forward = a.editor.pending_syntax_edits[0];
        a.undo_blocking().unwrap();
        assert_eq!(a.editor.pending_syntax_edits.len(), 2);
        let undo_delta = a.editor.pending_syntax_edits[1];
        // Undo's old_end/new_end are swapped relative to
        // forward.
        assert_eq!(undo_delta.start_byte, forward.start_byte);
        assert_eq!(undo_delta.old_end_byte, forward.new_end_byte);
        assert_eq!(undo_delta.new_end_byte, forward.old_end_byte);
        a.redo_blocking().unwrap();
        assert_eq!(a.editor.pending_syntax_edits.len(), 3);
    }

    #[test]
    fn insert_mode_inserts_text_and_advances_cursor() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("h".into()));
        a.apply(Action::Insert("i".into()));
        assert_eq!(a.editor.document.text(), "hi");
        assert_eq!(a.editor.cursor, Position::new(0, 2));
    }

    #[test]
    fn insert_then_normal_pulls_cursor_back_one() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("hi".into()));
        assert_eq!(a.editor.cursor, Position::new(0, 2));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }

    #[test]
    fn backspace_deletes_char_before_cursor_in_insert() {
        let mut a = app_with("hi", 10);
        a.editor.cursor.byte = 2;
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::DeleteCharBackward);
        assert_eq!(a.editor.document.text(), "h");
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }

    #[test]
    fn backspace_at_origin_is_a_no_op() {
        let mut a = app_with("hi", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::DeleteCharBackward);
        assert_eq!(a.editor.document.text(), "hi");
        assert_eq!(a.editor.cursor, Position::ZERO);
    }

    #[test]
    fn backspace_across_line_boundary_joins_lines() {
        let mut a = app_with("a\nb", 10);
        a.editor.cursor = Position::new(1, 0);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::DeleteCharBackward);
        assert_eq!(a.editor.document.text(), "ab");
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }

    #[test]
    fn enter_append_advances_cursor_one_byte_then_inserts() {
        let mut a = app_with("ab", 10);
        a.apply(Action::EnterAppend);
        assert_eq!(a.editor.modal, ModalState::Insert);
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }

    #[test]
    fn open_line_below_creates_new_line_and_drops_cursor_to_it() {
        let mut a = app_with("first", 10);
        a.apply(Action::OpenLineBelow);
        assert_eq!(a.editor.modal, ModalState::Insert);
        assert_eq!(a.editor.document.text(), "first\n");
        assert_eq!(a.editor.cursor, Position::new(1, 0));
    }

    #[test]
    fn open_line_above_creates_new_line_above() {
        let mut a = app_with("second", 10);
        a.apply(Action::OpenLineAbove);
        assert_eq!(a.editor.modal, ModalState::Insert);
        assert_eq!(a.editor.document.text(), "\nsecond");
        assert_eq!(a.editor.cursor, Position::new(0, 0));
    }

    #[test]
    fn delete_with_word_forward_target_dw_in_app() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.editor.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(
                a.editor.builtins.word_forward,
                lattice_grammar::Args::None,
            ),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "world");
        assert_eq!(a.editor.cursor, Position::ZERO);
    }

    #[test]
    fn delete_char_under_cursor_x_in_app() {
        let mut a = app_with("abc", 10);
        let inv = CommandInvocation::of(a.editor.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(
                a.editor.builtins.char_right,
                lattice_grammar::Args::None,
            ),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "bc");
        assert_eq!(a.editor.cursor, Position::ZERO);
    }

    #[test]
    fn undo_after_insert_restores_buffer() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("hi".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.document.text(), "hi");
        a.apply(Action::Undo);
        assert_eq!(a.editor.document.text(), "");
    }

    #[test]
    fn redo_replays_undone_edit() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("hi".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        a.apply(Action::Undo);
        a.apply(Action::Redo);
        assert_eq!(a.editor.document.text(), "hi");
    }

    #[test]
    fn per_character_insert_session_is_one_undo_unit() {
        // The reported bug: each keystroke arrives as its own
        // `Action::Insert`, but the whole insert session must undo as a
        // single batch, not one character at a time.
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        for ch in ["h", "e", "l", "l", "o"] {
            a.apply(Action::Insert(ch.into()));
        }
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.document.text(), "hello");
        a.apply(Action::Undo);
        assert_eq!(a.editor.document.text(), "", "one undo reverts the session");
        // Redo replays the whole session too.
        a.apply(Action::Redo);
        assert_eq!(a.editor.document.text(), "hello");
    }

    #[test]
    fn two_insert_sessions_are_two_undo_units() {
        // Separate `i` .. `<Esc>` sessions must not merge, even with no
        // edit in between -- proves the group closes on `<Esc>` and
        // re-opens on the next Insert.
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("a".into()));
        a.apply(Action::Insert("b".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        // Park the cursor at end-of-line so the second session appends.
        a.editor.cursor = Position::new(0, 2);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("c".into()));
        a.apply(Action::Insert("d".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.document.text(), "abcd");
        a.apply(Action::Undo);
        assert_eq!(
            a.editor.document.text(),
            "ab",
            "second session reverts alone"
        );
        a.apply(Action::Undo);
        assert_eq!(a.editor.document.text(), "", "first session reverts alone");
    }

    #[test]
    fn backspace_within_insert_is_part_of_the_same_undo_unit() {
        // Typing then backspacing inside one session is one undo unit.
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        for ch in ["a", "b", "c"] {
            a.apply(Action::Insert(ch.into()));
        }
        a.apply(Action::DeleteCharBackward); // drop the 'c'
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.document.text(), "ab");
        a.apply(Action::Undo);
        assert_eq!(a.editor.document.text(), "");
    }

    /// The insert-entry commands that bypass `enter_mode`
    /// (`a` / `A` / `I` / `o` / `O`) must open the undo group too, so a
    /// whole session collapses to one `u` -- not just `i`. Each case types
    /// several characters, leaves Insert, and asserts a single undo reverts
    /// the entire session.
    fn assert_entry_is_one_undo_unit(
        start: &str,
        cursor: Position,
        entry: Action,
        typed: &[&str],
        after_typing: &str,
    ) {
        let mut a = app_with(start, 10);
        a.editor.cursor = cursor;
        a.apply(entry);
        for ch in typed {
            a.apply(Action::Insert((*ch).into()));
        }
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.document.text(), after_typing);
        a.apply(Action::Undo);
        assert_eq!(
            a.editor.document.text(),
            start,
            "one undo should revert the whole insert session"
        );
    }

    #[test]
    fn append_session_is_one_undo_unit() {
        // `a`: append after the cursor.
        assert_entry_is_one_undo_unit(
            "x",
            Position::new(0, 0),
            Action::EnterAppend,
            &["a", "b", "c"],
            "xabc",
        );
    }

    #[test]
    fn append_eol_session_is_one_undo_unit() {
        // `A`: append at end of line.
        assert_entry_is_one_undo_unit(
            "hi",
            Position::new(0, 0),
            Action::EnterAppendEndOfLine,
            &["!", "?"],
            "hi!?",
        );
    }

    #[test]
    fn insert_first_non_blank_session_is_one_undo_unit() {
        // `I`: insert at first non-blank column.
        assert_entry_is_one_undo_unit(
            "  x",
            Position::new(0, 3),
            Action::EnterInsertFirstNonBlank,
            &["a", "b"],
            "  abx",
        );
    }

    #[test]
    fn open_line_below_session_is_one_undo_unit() {
        // `o`: open a new line below.
        assert_entry_is_one_undo_unit(
            "top",
            Position::new(0, 0),
            Action::OpenLineBelow,
            &["n", "e", "w"],
            "top\nnew",
        );
    }

    #[test]
    fn open_line_above_session_is_one_undo_unit() {
        // `O`: open a new line above.
        assert_entry_is_one_undo_unit(
            "bot",
            Position::new(0, 0),
            Action::OpenLineAbove,
            &["n", "e", "w"],
            "new\nbot",
        );
    }

    #[test]
    fn cw_deletes_word_and_enters_insert_mode() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.editor.builtins.change.0).with_target(
            lattice_grammar::Target::Motion(
                a.editor.builtins.word_forward,
                lattice_grammar::Args::None,
            ),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "world");
        assert_eq!(a.editor.modal, ModalState::Insert);
        assert_eq!(a.editor.cursor, Position::ZERO);
    }

    #[test]
    fn cc_clears_current_line_and_enters_insert_mode() {
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.editor.cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(a.editor.builtins.change.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "aaa\n\nccc");
        assert_eq!(a.editor.modal, ModalState::Insert);
    }

    #[test]
    fn join_lines_with_space_combines_two_lines_with_one_space() {
        let mut a = app_with("hello\nworld", 10);
        a.apply(Action::JoinLines { with_space: true });
        assert_eq!(a.editor.document.text(), "hello world");
        // Cursor lands at the join point (end of original first line).
        assert_eq!(a.editor.cursor, Position::new(0, 5));
    }

    #[test]
    fn join_lines_without_space_concatenates_directly() {
        let mut a = app_with("hello\nworld", 10);
        a.apply(Action::JoinLines { with_space: false });
        assert_eq!(a.editor.document.text(), "helloworld");
    }

    #[test]
    fn join_lines_trims_leading_whitespace_on_next_line() {
        let mut a = app_with("hello\n   world", 10);
        a.apply(Action::JoinLines { with_space: true });
        assert_eq!(a.editor.document.text(), "hello world");
    }

    #[test]
    fn join_lines_at_last_line_is_no_op() {
        let mut a = app_with("only", 10);
        a.apply(Action::JoinLines { with_space: true });
        assert_eq!(a.editor.document.text(), "only");
    }

    #[test]
    fn paste_from_named_register_uses_named_content() {
        let mut a = app_with("hello", 10);
        // Manually populate "a with custom content.
        a.editor.registers.insert(
            Register::Named('a'),
            UnnamedRegister {
                content: "X".into(),
                kind: YankKind::Charwise,
            },
        );
        a.apply(Action::SelectRegister(Register::Named('a')));
        a.apply(Action::PasteAfter);
        assert_eq!(a.editor.document.text(), "hXello");
    }

    #[test]
    fn delete_into_black_hole_does_not_overwrite_unnamed() {
        let mut a = app_with("hello world", 10);
        // First yank into unnamed.
        let yank = CommandInvocation::of(a.editor.builtins.yank.0).with_target(
            lattice_grammar::Target::Motion(
                a.editor.builtins.word_forward,
                lattice_grammar::Args::None,
            ),
        );
        a.apply(Action::Invoke(yank));
        let pre_delete_unnamed = a.editor.unnamed_register.as_ref().unwrap().content.clone();
        // Now delete into black hole; unnamed should be untouched.
        a.apply(Action::SelectRegister(Register::BlackHole));
        let inv = CommandInvocation::of(a.editor.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(
                a.editor.builtins.word_forward,
                lattice_grammar::Args::None,
            ),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(
            a.editor.unnamed_register.as_ref().unwrap().content,
            pre_delete_unnamed
        );
    }

    #[test]
    fn delete_does_not_populate_zero_register() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.editor.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(
                a.editor.builtins.word_forward,
                lattice_grammar::Args::None,
            ),
        );
        a.apply(Action::Invoke(inv));
        // Delete populates unnamed but NOT "0.
        assert!(!a.editor.registers.contains_key(&Register::Numbered(0)));
        assert!(a.editor.unnamed_register.is_some());
    }

    #[test]
    fn paste_from_unset_named_register_falls_back_to_unnamed() {
        let mut a = app_with("hello", 10);
        a.editor.unnamed_register = Some(UnnamedRegister {
            content: "X".into(),
            kind: YankKind::Charwise,
        });
        a.apply(Action::SelectRegister(Register::Named('z')));
        a.apply(Action::PasteAfter);
        // 'z' is empty -> fall back to unnamed.
        assert_eq!(a.editor.document.text(), "hXello");
    }

    #[test]
    fn toggle_case_at_cursor_inverts_letter_and_advances() {
        let mut a = app_with("hello", 10);
        a.apply(Action::ToggleCaseAtCursor);
        assert_eq!(a.editor.document.text(), "Hello");
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }

    #[test]
    fn toggle_case_advances_through_non_letters() {
        let mut a = app_with("a 1 b", 10);
        a.apply(Action::ToggleCaseAtCursor);
        assert_eq!(a.editor.document.text(), "A 1 b");
        a.apply(Action::ToggleCaseAtCursor);
        // Space at byte 1 -> unchanged but cursor advances.
        assert_eq!(a.editor.document.text(), "A 1 b");
        assert_eq!(a.editor.cursor, Position::new(0, 2));
    }

    #[test]
    fn toggle_case_at_eol_is_no_op() {
        let mut a = app_with("hi", 10);
        a.editor.cursor = Position::new(0, 2);
        a.apply(Action::ToggleCaseAtCursor);
        assert_eq!(a.editor.document.text(), "hi");
        assert_eq!(a.editor.cursor, Position::new(0, 2));
    }

    #[test]
    fn overwrite_char_replaces_byte_at_cursor() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::OverwriteChar('H'));
        assert_eq!(a.editor.document.text(), "Hello");
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }

    #[test]
    fn overwrite_chain_replaces_consecutively() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        for c in "WORL".chars() {
            a.apply(Action::OverwriteChar(c));
        }
        assert_eq!(a.editor.document.text(), "WORLo");
        assert_eq!(a.editor.cursor, Position::new(0, 4));
    }

    #[test]
    fn overwrite_at_eol_extends_line() {
        let mut a = app_with("hi", 10);
        a.editor.cursor = Position::new(0, 2);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::OverwriteChar('!'));
        assert_eq!(a.editor.document.text(), "hi!");
        assert_eq!(a.editor.cursor, Position::new(0, 3));
    }

    #[test]
    fn replace_undo_last_restores_overwritten_char() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::OverwriteChar('H'));
        assert_eq!(a.editor.document.text(), "Hello");
        assert_eq!(a.editor.cursor, Position::new(0, 1));
        // Backspace: should restore 'h' and step cursor back.
        a.apply(Action::ReplaceUndoLast);
        assert_eq!(a.editor.document.text(), "hello");
        assert_eq!(a.editor.cursor, Position::ZERO);
    }

    #[test]
    fn replace_undo_after_eol_extension_deletes_extension() {
        let mut a = app_with("hi", 10);
        a.editor.cursor = Position::new(0, 2);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::OverwriteChar('!'));
        assert_eq!(a.editor.document.text(), "hi!");
        a.apply(Action::ReplaceUndoLast);
        assert_eq!(a.editor.document.text(), "hi");
        assert_eq!(a.editor.cursor, Position::new(0, 2));
    }

    #[test]
    fn replace_undo_with_empty_history_is_no_op() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::ReplaceUndoLast);
        assert_eq!(a.editor.document.text(), "hello");
        assert_eq!(a.editor.cursor, Position::ZERO);
    }

    #[test]
    fn replace_undo_chain_restores_in_reverse_order() {
        let mut a = app_with("abcde", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::OverwriteChar('A'));
        a.apply(Action::OverwriteChar('B'));
        a.apply(Action::OverwriteChar('C'));
        assert_eq!(a.editor.document.text(), "ABCde");
        a.apply(Action::ReplaceUndoLast);
        assert_eq!(a.editor.document.text(), "ABcde");
        a.apply(Action::ReplaceUndoLast);
        assert_eq!(a.editor.document.text(), "Abcde");
        a.apply(Action::ReplaceUndoLast);
        assert_eq!(a.editor.document.text(), "abcde");
    }

    #[test]
    fn delete_records_last_change_and_dot_replays_it() {
        let mut a = app_with("foo bar foo bar", 10);
        let inv = CommandInvocation::of(a.editor.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(
                a.editor.builtins.word_forward,
                lattice_grammar::Args::None,
            ),
        );
        a.apply(Action::Invoke(inv));
        // After dw: "bar foo bar".
        assert_eq!(a.editor.document.text(), "bar foo bar");
        assert!(a.editor.last_change.is_some());
        // `.` replays the same dw at the new cursor position.
        a.apply(Action::RepeatLastChange);
        assert_eq!(a.editor.document.text(), "foo bar");
    }

    #[test]
    fn insert_session_captures_typed_text_into_last_insert() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::Insert("Y".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.last_insert.as_deref(), Some("XY"));
    }

    #[test]
    fn paste_after_charwise_inserts_after_cursor() {
        let mut a = app_with("hello", 10);
        a.editor.unnamed_register = Some(UnnamedRegister {
            content: "X".into(),
            kind: YankKind::Charwise,
        });
        a.editor.cursor = Position::new(0, 0); // on 'h'
        a.apply(Action::PasteAfter);
        assert_eq!(a.editor.document.text(), "hXello");
        // Cursor lands on the last char of the pasted text (still 'X').
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }

    #[test]
    fn paste_before_charwise_inserts_at_cursor() {
        let mut a = app_with("hello", 10);
        a.editor.unnamed_register = Some(UnnamedRegister {
            content: "X".into(),
            kind: YankKind::Charwise,
        });
        a.editor.cursor = Position::new(0, 2); // on 'l'
        a.apply(Action::PasteBefore);
        assert_eq!(a.editor.document.text(), "heXllo");
        assert_eq!(a.editor.cursor, Position::new(0, 2));
    }

    #[test]
    fn paste_after_linewise_inserts_below_current_line() {
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.editor.unnamed_register = Some(UnnamedRegister {
            content: "XXX\n".into(),
            kind: YankKind::Linewise,
        });
        a.editor.cursor = Position::new(1, 0); // on 'B' line
        a.apply(Action::PasteAfter);
        assert_eq!(a.editor.document.text(), "aaa\nBBB\nXXX\nccc");
        assert_eq!(a.editor.cursor, Position::new(2, 0));
    }

    #[test]
    fn paste_before_linewise_inserts_above_current_line() {
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.editor.unnamed_register = Some(UnnamedRegister {
            content: "XXX\n".into(),
            kind: YankKind::Linewise,
        });
        a.editor.cursor = Position::new(1, 0);
        a.apply(Action::PasteBefore);
        assert_eq!(a.editor.document.text(), "aaa\nXXX\nBBB\nccc");
        assert_eq!(a.editor.cursor, Position::new(1, 0));
    }

    #[test]
    fn paste_with_empty_register_emits_error_message() {
        let mut a = app_with("hello", 10);
        assert!(a.editor.unnamed_register.is_none());
        a.apply(Action::PasteAfter);
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert_eq!(a.editor.document.text(), "hello");
    }

    #[test]
    fn paste_text_in_normal_inserts_at_cursor_one_undo_unit() {
        let mut a = app_with("hello", 10);
        a.editor.cursor = Position::new(0, 5);
        a.apply(Action::PasteText(" world".into()));
        assert_eq!(a.editor.document.text(), "hello world");
        assert_eq!(a.editor.cursor, Position::new(0, 11));
        // One bracketed-paste = one undo unit.
        a.apply(Action::Undo);
        assert_eq!(a.editor.document.text(), "hello");
    }

    #[test]
    fn paste_text_in_insert_inserts_and_records_for_dot_repeat() {
        let mut a = app_with("a", 10);
        a.editor.cursor = Position::new(0, 1);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::PasteText("bcd".into()));
        assert_eq!(a.editor.document.text(), "abcd");
        assert_eq!(a.editor.cursor, Position::new(0, 4));
        assert!(matches!(a.editor.modal, ModalState::Insert));
        // Dot-repeat insert recording captured the pasted text.
        let rec = a.editor.recording_insert.as_ref().unwrap();
        assert_eq!(rec, "bcd");
    }

    #[test]
    fn paste_text_in_command_appends_to_command_line() {
        let mut a = app_with("xx", 10);
        a.apply(Action::EnterMode(ModalState::Command));
        a.editor.command_line = "w ".into();
        a.apply(Action::PasteText("foo.rs".into()));
        assert_eq!(a.editor.command_line, "w foo.rs");
        // Document untouched.
        assert_eq!(a.editor.document.text(), "xx");
    }

    #[test]
    fn paste_text_in_search_appends_to_search_pattern() {
        let mut a = app_with("xx", 10);
        a.apply(Action::EnterSearch(
            lattice_grammar::SearchDirection::Forward,
        ));
        a.apply(Action::SearchAppend('a'));
        a.apply(Action::PasteText("bcd".into()));
        let line = a.editor.search_line.as_ref().unwrap();
        assert_eq!(line.pattern, "abcd");
    }

    #[test]
    fn paste_text_empty_is_a_noop() {
        let mut a = app_with("hello", 10);
        let before = a.editor.document.text();
        a.apply(Action::PasteText(String::new()));
        assert_eq!(a.editor.document.text(), before);
    }

    #[test]
    fn paste_text_with_newlines_lands_as_single_edit() {
        let mut a = app_with("a", 10);
        a.editor.cursor = Position::new(0, 1);
        a.apply(Action::PasteText("\nb\nc".into()));
        assert_eq!(a.editor.document.text(), "a\nb\nc");
        assert_eq!(a.editor.cursor, Position::new(2, 1));
    }

    fn enter_block_visual(text: &str, anchor: Position, head: Position) -> App {
        let mut a = app_with(text, 10);
        a.editor.cursor = anchor;
        a.apply(Action::EnterVisual(VisualKind::Blockwise));
        a.editor.cursor = head;
        a.editor.visual_anchor = Some(anchor);
        let sel = Selection {
            anchor,
            head,
            visual: Some(VisualMode::Blockwise),
        };
        a.editor.set_selections_blocking(SelectionSet::single(sel));
        a
    }

    #[test]
    fn block_delete_removes_each_rows_column_slice() {
        // Three rows, columns 1..=2 deleted from each.
        // Initial:    "abcd\n1234\nWXYZ"
        // After d :   "ad\n14\nWZ"
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv = CommandInvocation::of(a.editor.builtins.delete.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "ad\n14\nWZ");
    }

    #[test]
    fn block_delete_lands_cursor_at_top_left_of_block() {
        // Vim's behavior: after a rectangle delete, the cursor sits
        // at the block's top-left column, not at column 0.
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv = CommandInvocation::of(a.editor.builtins.delete.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        // Top-left of block was (0, 1); after the delete column 1
        // is the new content's start on the top row.
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }

    #[test]
    fn block_delete_lands_as_single_undo_unit() {
        // The whole rectangle delete must collapse into one undo
        // entry -- the dispatcher coalesces the per-row AppliedEdits
        // by snapshotting pre/post and replaying as one Edit::replace.
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv = CommandInvocation::of(a.editor.builtins.delete.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "ad\n14\nWZ");
        let _ = a.undo_blocking();
        assert_eq!(a.editor.document.text(), "abcd\n1234\nWXYZ");
    }

    #[test]
    fn block_change_lands_as_single_undo_unit() {
        // Block-visual `c` deletes each row's column slice and enters
        // Insert. The deletion piece must be one undo unit; future
        // typed text would be batched separately by the I/A path.
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv = CommandInvocation::of(a.editor.builtins.change.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "ad\n14\nWZ");
        assert!(matches!(a.editor.modal, ModalState::Insert));
        // Exit Insert without typing anything to isolate the deletion.
        a.apply(Action::EnterMode(ModalState::Normal));
        let _ = a.undo_blocking();
        assert_eq!(a.editor.document.text(), "abcd\n1234\nWXYZ");
    }

    #[test]
    fn block_yank_stores_blockwise_content_in_unnamed_register() {
        // Yank a 3x2 rectangle: cols 1..=2 across three rows of "abcd\n1234\nWXYZ".
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv = CommandInvocation::of(a.editor.builtins.yank.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        // Document untouched.
        assert_eq!(a.editor.document.text(), "abcd\n1234\nWXYZ");
        // Unnamed register has the 3 column slices joined by newline,
        // tagged Blockwise.
        let reg = a.editor.unnamed_register.as_ref().expect("yank stored");
        assert_eq!(reg.content, "bc\n23\nXY");
        assert_eq!(reg.kind, YankKind::Blockwise);
    }

    #[test]
    fn block_yank_clamps_short_rows_to_intersection() {
        // Middle row "12" partially overlaps the rectangle: cols 1..=2,
        // line len 2, intersection is `[1, 2)` = "2".
        let mut a = enter_block_visual("abcd\n12\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv = CommandInvocation::of(a.editor.builtins.yank.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        let reg = a.editor.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.content, "bc\n2\nXY");
        assert_eq!(reg.kind, YankKind::Blockwise);
    }

    #[test]
    fn block_yank_with_row_entirely_left_of_rectangle_yields_empty_slice() {
        // Middle row is "" (empty). Visual cols 1..=2 fully outside;
        // intersection is empty.
        let mut a = enter_block_visual("abcd\n\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv = CommandInvocation::of(a.editor.builtins.yank.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        let reg = a.editor.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.content, "bc\n\nXY");
        assert_eq!(reg.kind, YankKind::Blockwise);
    }

    #[test]
    fn block_visual_indent_right_indents_each_row_in_block() {
        // Indent operates on lines covered by the block. The
        // insertion goes at column 0 of each line (vim's behavior),
        // not at the block's left column. Whole change must be one
        // undo unit (operator opts out of per-row blockwise dispatch
        // via blockwise_per_row=false; the indent operator's
        // apply_edit_batch makes the multi-line indent atomic).
        let mut a = enter_block_visual("abc\n123\nWXY", Position::new(0, 1), Position::new(2, 1));
        let inv = CommandInvocation::of(a.editor.builtins.indent_right.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "    abc\n    123\n    WXY");
        let _ = a.undo_blocking();
        assert_eq!(a.editor.document.text(), "abc\n123\nWXY");
    }

    #[test]
    fn block_visual_capital_i_via_real_motions_not_explicit_selection() {
        // Reproduces the path the actual user takes: Ctrl-V to enter
        // blockwise, motions to extend the selection, capital I.
        // No manual set_selections_blocking -- selections must be
        // maintained by the SelectionChange effect from motions.
        let mut a = app_with("abcd\n1234\nWXYZ", 10);
        a.editor.cursor = Position::new(0, 1);
        a.apply(Action::EnterVisual(VisualKind::Blockwise));
        // Move down 2 rows + right 1 column via motions.
        a.apply(invoke_motion(a.editor.builtins.line_down));
        a.apply(invoke_motion(a.editor.builtins.line_down));
        a.apply(invoke_motion(a.editor.builtins.char_right));
        // Cursor should now be at (2, 2). visual_anchor was (0, 1).
        assert_eq!(a.editor.cursor, Position::new(2, 2));
        assert_eq!(a.editor.visual_anchor, Some(Position::new(0, 1)));

        a.apply(Action::EnterBlockVisualInsert);
        assert!(matches!(a.editor.modal, ModalState::Insert));
        // I should land at column 1 (block's left col) on the top row.
        assert_eq!(a.editor.cursor, Position::new(0, 1));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.document.text(), "aXbcd\n1X234\nWXXYZ");
    }

    #[test]
    fn block_visual_capital_i_inserts_at_block_left_column_on_each_row() {
        // 3 rows, block at column 1. `I` enters Insert at (top_row, 1).
        // Type "X", Esc -> "X" lands at column 1 on every row.
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        a.apply(Action::EnterBlockVisualInsert);
        assert!(matches!(a.editor.modal, ModalState::Insert));
        assert_eq!(a.editor.cursor, Position::new(0, 1));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.document.text(), "aXbcd\n1X234\nWXXYZ");
    }

    #[test]
    fn block_visual_capital_a_appends_after_block_right_column() {
        // Block cols 1..=2 across 3 rows; `A` lands at col 3 on each row.
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        a.apply(Action::EnterBlockVisualAppend);
        assert!(matches!(a.editor.modal, ModalState::Insert));
        assert_eq!(a.editor.cursor, Position::new(0, 3));
        a.apply(Action::Insert("@".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.document.text(), "abc@d\n123@4\nWXY@Z");
    }

    #[test]
    fn block_visual_capital_i_lands_as_single_undo_unit() {
        // Type 3 chars during the I session, replicate to 2 other rows,
        // then `u` once -- the buffer should fully revert. Without the
        // batched-commit fix, undo would only roll back the last char
        // on one row.
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        a.apply(Action::EnterBlockVisualInsert);
        a.apply(Action::Insert("X".into()));
        a.apply(Action::Insert("Y".into()));
        a.apply(Action::Insert("Z".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.document.text(), "aXYZbcd\n1XYZ234\nWXYZXYZ");

        // One undo should restore the original buffer.
        let _ = a.undo_blocking();
        assert_eq!(a.editor.document.text(), "abcd\n1234\nWXYZ");
    }

    #[test]
    fn block_visual_capital_a_lands_as_single_undo_unit() {
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        a.apply(Action::EnterBlockVisualAppend);
        a.apply(Action::Insert("@".into()));
        a.apply(Action::Insert("@".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.document.text(), "abc@@d\n123@@4\nWXY@@Z");
        let _ = a.undo_blocking();
        assert_eq!(a.editor.document.text(), "abcd\n1234\nWXYZ");
    }

    #[test]
    fn block_visual_capital_i_skips_lines_shorter_than_insert_col() {
        // Middle row "12" is too short for col 3 (insert_col). Vim skips it.
        let mut a = enter_block_visual("abcd\n12\nWXYZ", Position::new(0, 3), Position::new(2, 3));
        a.apply(Action::EnterBlockVisualInsert);
        a.apply(Action::Insert("Q".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        // Top row gets the live edit; bottom row replays at col 3;
        // middle row is too short and is left untouched.
        assert_eq!(a.editor.document.text(), "abcQd\n12\nWXYQZ");
    }

    #[test]
    fn block_visual_indent_left_dedents_each_row_in_block() {
        let mut a = enter_block_visual(
            "    abc\n    123\n    WXY",
            Position::new(0, 0),
            Position::new(2, 0),
        );
        let inv = CommandInvocation::of(a.editor.builtins.indent_left.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "abc\n123\nWXY");
    }

    #[test]
    fn block_change_deletes_rectangle_and_enters_insert() {
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv = CommandInvocation::of(a.editor.builtins.change.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "ad\n14\nWZ");
        assert!(matches!(a.editor.modal, ModalState::Insert));
    }

    #[test]
    fn block_paste_after_replays_rectangle_on_consecutive_lines() {
        // Yank a 2x2 rectangle from the top, paste it at column 0 of
        // line 2. Each row of the yanked content lands on a successive
        // line at the paste column.
        let mut a = enter_block_visual(
            "abcd\n1234\nWXYZ\n----",
            Position::new(0, 1),
            Position::new(1, 2),
        );
        let yank = CommandInvocation::of(a.editor.builtins.yank.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(yank));
        // Exit visual and move to a fresh paste site.
        a.apply(Action::ExitVisual);
        a.editor.cursor = Position::new(2, 0);
        // `p` (after-cursor) -> insert at col 1 on line 2 and line 3.
        a.apply(Action::PasteAfter);
        // Line 2: "WXYZ" -> "WbcXYZ"; Line 3: "----" -> "-23---"
        assert_eq!(a.editor.document.text(), "abcd\n1234\nWbcXYZ\n-23---");
    }

    #[test]
    fn backspace_after_popup_open_live_refilters() {
        let mut a = app_in_command_mode("describ");
        a.apply(Action::CommandLineCompleteOrAdvance);
        // Phase 5.8.AF.6 / issue-3: opening the popup with multiple
        // matches LCP-extends the cmdline (here from "describ" to
        // "describe-"; every `describe-*` command shares that
        // prefix). Backspace removes one byte off the EXTENDED
        // cmdline -> "describe", not the pre-LCP "descri".
        assert_eq!(a.editor.command_line, "describe-");
        let narrow_count = a.editor.completion_state.as_ref().unwrap().candidates.len();
        a.apply(Action::CommandLineBackspace);
        assert!(a.editor.completion_state.is_some());
        assert_eq!(a.editor.command_line, "describe");
        // Shorter prefix -> at least as many candidates.
        let widened = a.editor.completion_state.as_ref().unwrap().candidates.len();
        assert!(widened >= narrow_count);
    }

    #[test]
    fn delete_word_backward_with_open_popup_refilters() {
        let mut a = app_in_command_mode("describ");
        a.apply(Action::CommandLineCompleteOrAdvance);
        a.apply(Action::CommandLineDeleteWordBackward);
        // Word-delete leaves us with an empty cmdline -> Empty slot
        // -> all commands; popup stays open.
        assert!(a.editor.completion_state.is_some());
        assert_eq!(a.editor.command_line, "");
    }

    #[test]
    fn delete_chord_pops_one_full_token() {
        let mut a = app_in_command_mode("describe-key <C-c>");
        a.apply(Action::CommandLineDeleteChord);
        // The whole `<C-c>` token (5 bytes) gets removed in one
        // delete -- not a single byte.
        assert_eq!(a.editor.command_line, "describe-key ");
    }

    #[test]
    fn delete_chord_on_plain_char_pops_one_char() {
        let mut a = app_in_command_mode("describe-key gg");
        a.apply(Action::CommandLineDeleteChord);
        assert_eq!(a.editor.command_line, "describe-key g");
    }

    #[test]
    fn delete_chord_on_empty_cmdline_exits_command_mode() {
        let mut a = app_with("xx", 10);
        a.editor.modal = ModalState::Command;
        a.editor.command_line = String::new();
        a.apply(Action::CommandLineDeleteChord);
        assert!(matches!(a.editor.modal, ModalState::Normal));
    }

    #[test]
    fn insert_completion_trigger_outside_insert_is_noop() {
        let mut a = app_with("foo bar baz", 10);
        // Normal mode by default -- trigger should no-op.
        a.do_completion_trigger();
        assert!(a.editor.insert_completion.is_none());
    }

    #[test]
    fn insert_completion_trigger_with_no_matches_echoes_no_completions() {
        let mut a = app_with("hello world hello\nfoo bar baz qux", 10);
        a.editor.modal = ModalState::Insert;
        // Cursor at end of `hello` on line 0 -- prefix "hello".
        // BufferWordsSource skips the cursor's own word, and
        // none of the remaining buffer words fuzzy-match
        // "hello", so the popup auto-closes with the
        // "no completions" echo.
        a.editor.cursor = Position::new(0, 5);
        a.do_completion_trigger();
        assert!(a.editor.insert_completion.is_none());
        let msg = a.editor.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no completions"));
    }

    #[test]
    fn insert_completion_open_with_matching_query_keeps_popup() {
        let mut a = app_with("hello world helper helmet hi", 10);
        a.editor.modal = ModalState::Insert;
        // Cursor right after `hel` -- prefix "hel". Buffer words:
        // "hello", "world", "helper", "helmet", "hi".
        a.editor.cursor = Position::new(0, 3);
        // Place hel at the cursor: rewrite content via cursor
        // positioning (the buffer already has "hel" as part of
        // hello). For the test, just place cursor on a different
        // line.
        let _ = a.apply_edit_blocking(Edit::insert(Position::new(0, 28), "\nhel"));
        a.editor.cursor = Position::new(1, 3);
        a.do_completion_trigger();
        let state = a.editor.insert_completion.as_ref().expect("popup opened");
        assert_eq!(state.query, "hel");
        // hello / helper / helmet all start with "hel" -- prefix
        // tier (score 800) matches. Order may vary by stable
        // sort over insertion order.
        let labels: Vec<String> = state.rendered.iter().map(|c| c.raw.text.clone()).collect();
        assert!(labels.contains(&"hello".to_string()));
        assert!(labels.contains(&"helper".to_string()));
        assert!(labels.contains(&"helmet".to_string()));
        // "hi" doesn't fuzzy-match "hel", "world" doesn't either.
        assert!(!labels.contains(&"hi".to_string()));
        assert!(!labels.contains(&"world".to_string()));
    }

    #[test]
    fn insert_completion_next_prev_navigates_with_wrap() {
        let mut a = app_with("alpha alphabet alligator", 10);
        a.editor.modal = ModalState::Insert;
        let _ = a.apply_edit_blocking(Edit::insert(Position::new(0, 24), "\nal"));
        a.editor.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        let total = a
            .editor
            .insert_completion
            .as_ref()
            .expect("popup")
            .rendered
            .len();
        assert!(total >= 2, "need ≥ 2 candidates for wrap test");
        assert_eq!(a.editor.insert_completion.as_ref().unwrap().selected, 0);
        a.do_completion_next();
        assert_eq!(a.editor.insert_completion.as_ref().unwrap().selected, 1);
        // Wrap to last via prev from 1 -> 0 -> total-1.
        a.do_completion_prev();
        a.do_completion_prev();
        assert_eq!(
            a.editor.insert_completion.as_ref().unwrap().selected,
            total - 1
        );
    }

    #[test]
    fn insert_completion_accept_replaces_prefix_and_closes() {
        let mut a = app_with("alphabet alligator\nal", 10);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        // Pick the first candidate.
        let first_text = a
            .editor
            .insert_completion
            .as_ref()
            .and_then(|s| s.selected_candidate())
            .map(|c| c.raw.text.clone())
            .expect("at least one candidate");
        a.do_completion_accept();
        assert!(a.editor.insert_completion.is_none());
        // Buffer line 1 should now be the chosen word.
        let snap = a.editor.document.snapshot();
        let line1 = snap.buffer.line(1).unwrap_or_default();
        assert_eq!(line1.trim_end(), first_text);
    }

    #[test]
    fn insert_completion_cancel_drops_popup() {
        let mut a = app_with("alpha alphabet\nal", 10);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        assert!(a.editor.insert_completion.is_some());
        a.do_completion_cancel();
        assert!(a.editor.insert_completion.is_none());
        // Modal stays Insert.
        assert!(matches!(a.editor.modal, ModalState::Insert));
    }

    #[test]
    fn insert_completion_cancel_and_exit_insert_drops_popup_and_exits() {
        let mut a = app_with("alpha alphabet\nal", 10);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        assert!(a.editor.insert_completion.is_some());
        a.apply(Action::CompletionCancelAndExitInsert);
        assert!(a.editor.insert_completion.is_none());
        assert!(matches!(a.editor.modal, ModalState::Normal));
    }

    #[test]
    fn insert_completion_toggle_docs_flips_state() {
        let mut a = app_with("alpha alphabet\nal", 10);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        assert!(
            a.editor
                .insert_completion
                .as_ref()
                .map(|s| s.doc_popup.is_none())
                .unwrap_or(false)
        );
        a.do_completion_toggle_docs();
        assert!(
            a.editor
                .insert_completion
                .as_ref()
                .unwrap()
                .doc_popup
                .is_some()
        );
        a.do_completion_toggle_docs();
        assert!(
            a.editor
                .insert_completion
                .as_ref()
                .unwrap()
                .doc_popup
                .is_none()
        );
    }

    #[test]
    fn insert_completion_refilters_on_keystroke() {
        let mut a = app_with("alpha alphabet alligator\nal", 10);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        let pre_count = a
            .editor
            .insert_completion
            .as_ref()
            .expect("popup")
            .rendered
            .len();
        // Type 'p' -- query becomes "alp"; only "alpha" /
        // "alphabet" survive (alligator drops out).
        a.apply(Action::Insert("p".into()));
        let state = a
            .editor
            .insert_completion
            .as_ref()
            .expect("popup still open");
        assert_eq!(state.query, "alp");
        let labels: Vec<String> = state.rendered.iter().map(|c| c.raw.text.clone()).collect();
        assert!(labels.contains(&"alpha".to_string()));
        assert!(labels.contains(&"alphabet".to_string()));
        assert!(!labels.contains(&"alligator".to_string()));
        assert!(state.rendered.len() < pre_count);
    }

    #[test]
    fn insert_completion_closes_when_query_leaves_word_boundary() {
        let mut a = app_with("alpha alphabet\nal", 10);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        assert!(a.editor.insert_completion.is_some());
        // Type a space -- pushes the cursor past the word.
        a.apply(Action::Insert(" ".into()));
        assert!(a.editor.insert_completion.is_none());
    }

    #[test]
    fn block_paste_extends_buffer_when_below_eof() {
        // Yank 2 rows then paste at the bottom -- the missing row is
        // appended as a fresh line.
        let mut a = enter_block_visual("abcd\n1234", Position::new(0, 1), Position::new(1, 2));
        let yank = CommandInvocation::of(a.editor.builtins.yank.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(yank));
        a.apply(Action::ExitVisual);
        // Move to last line and paste with `P` (before-cursor) at col 0.
        a.editor.cursor = Position::new(1, 0);
        a.apply(Action::PasteBefore);
        // Line 1 becomes "bc1234"; new line 2 holds "23".
        assert_eq!(a.editor.document.text(), "abcd\nbc1234\n23");
    }

    #[test]
    fn delete_then_paste_after_emulates_xp_swap() {
        // Vim trick: cursor on 'a' of "abc"; `xp` swaps 'a' and 'b' -> "bac".
        let mut a = app_with("abc", 10);
        a.editor.cursor = Position::ZERO;
        // x: delete char-right
        let inv = CommandInvocation::of(a.editor.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(
                a.editor.builtins.char_right,
                lattice_grammar::Args::None,
            ),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "bc");
        // p: paste after cursor (cursor at 0 on 'b'; paste after -> "bac").
        a.apply(Action::PasteAfter);
        assert_eq!(a.editor.document.text(), "bac");
    }
}
