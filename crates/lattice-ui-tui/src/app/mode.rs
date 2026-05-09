//! Modal-state transitions -- the buffer-level state machine
//! (Normal / Insert / Visual / Op-pending / Command /
//! Search) and the major-mode activation hooks that fire
//! on transition.
//!
//! Methods that live here:
//! - `modal_label` (status-line label of the current
//!   modal-state).
//!
//! Methods that move here in R.1 (deferred):
//! - `enter_normal`, `enter_insert`, `enter_replace`,
//!   `enter_op_pending`, `enter_command`, `enter_search`.
//! - The `ModeContext` builder used by major modes during
//!   `on_activate` -- including the `setup_buffer_locals`
//!   path that seeds `BufferLocals` from struct fields.
//! - Cursor-shape recompute on mode transitions (the
//!   App-side hook; renderer reads the resulting state).
//! - `set_active_mode_for_buffer` and the `Mode` trait
//!   dispatch that calls `on_activate` / `on_deactivate`.
//!
//! Insert-mode entry methods (`enter_insert_after`,
//! `enter_insert_line_start`, `enter_insert_line_end`,
//! `enter_insert_open_below`, `enter_insert_open_above`)
//! landed in `app/edit.rs` via R.1.47 because they pair
//! with the Insert / Replace edit primitives that already
//! lived there; they read as edit-flow rather than mode-
//! flow.
//!
//! What does NOT live here: the `Mode` trait itself
//! (lives in `crate::modes`), the per-major-mode impls
//! (`TextMode`, `RustMode`, ...) -- those stay in
//! `crate::modes`.

use lattice_grammar::ModalState;
use lattice_protocol::Event;

use super::App;

impl App {
    pub fn modal_label(&self) -> &'static str {
        match self.modal {
            ModalState::Normal => "NORMAL",
            ModalState::Insert => "INSERT",
            ModalState::Visual(_) => "VISUAL",
            ModalState::OperatorPending => "O-PEND",
            ModalState::Command => "CMD",
            ModalState::Search(_) => "SEARCH",
            ModalState::Replace => "REPLACE",
        }
    }

    pub(super) fn enter_mode(&mut self, state: ModalState) {
        let prior = self.modal;
        // Reset Replace's history every time we enter (or re-enter) Replace
        // so backspace-restore is bounded to the current `R` session.
        if matches!(state, ModalState::Replace) {
            self.replace_history.clear();
        }
        let was_insert_like = matches!(self.modal, ModalState::Insert | ModalState::Replace);
        let entering_insert_like = matches!(state, ModalState::Insert | ModalState::Replace);
        // Insert-replay capture:
        //   - Entering Insert/Replace from anything else: start recording.
        //   - Leaving Insert/Replace to anything else: promote into last_insert.
        if entering_insert_like && !was_insert_like {
            self.recording_insert = Some(String::new());
        }
        if was_insert_like
            && !entering_insert_like
            && let Some(rec) = self.recording_insert.take()
        {
            // Snapshot the recording before consuming the block-
            // insert spec; we need both to replicate.
            let block_spec = self.pending_block_insert.take();
            if !rec.is_empty() {
                self.last_insert = Some(rec.clone());
            }
            if let Some(spec) = block_spec
                && !rec.is_empty()
            {
                self.replicate_block_insert(spec, &rec);
            }
        } else if was_insert_like && !entering_insert_like {
            // Insert exited but recording_insert was already None
            // (shouldn't happen given enter_mode pairs them, but
            // belt-and-braces -- still clear any spec so a future
            // I/A starts clean).
            self.pending_block_insert = None;
        }
        self.modal = state;
        if matches!(state, ModalState::Normal) {
            // Vim's behavior: leaving Insert mode pulls the cursor back one
            // byte if it's not already at the start of the line, so the
            // cursor sits on the last inserted char rather than past it.
            if self.cursor.byte > 0 {
                self.cursor.byte -= 1;
            }
        }
        // Publish ModalModeChanged whenever the modal axis actually
        // moves. (DESIGN.md §5.10 catalog.) Re-entering the same
        // mode -- e.g. the dot-repeat path that calls enter_mode
        // for the side-effect of recording/replay accounting --
        // doesn't fire the event.
        if prior != state {
            self.event_bus.publish(Event::ModalModeChanged {
                from: format!("{prior:?}"),
                to: format!("{state:?}"),
            });
        }
    }
}
