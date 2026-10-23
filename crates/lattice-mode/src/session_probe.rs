//! What a session-backed minor mode is shown, once per dispatch, to decide
//! whether its session is still live.
//!
//! Some minor modes live exactly as long as a mode-owned session does —
//! `active-snippet-mode` while a snippet is expanding. The host reconciles
//! each of them on the active buffer at the end of every dispatch
//! (`sync_keymap_overlays`) by calling the mode's predicate. The predicate
//! owns the policy: it answers "should my mode be active here?" and, when
//! the answer turns to no because the user left (Normal mode, the cursor
//! somewhere else), it ends its own session. The host only runs the loop
//! and hands over this probe, so it carries no knowledge of any one
//! session's rules.

use std::sync::Arc;

use lattice_core::BufferId;
use lattice_grammar::ModalState;
use lattice_protocol::position::Position;

/// The buffer being reconciled, as the host sees it after a dispatch.
pub struct SessionProbe<'a> {
    /// The buffer — always the active one.
    pub buffer: BufferId,
    /// The modal state the dispatch left it in.
    pub modal: ModalState,
    /// The cursor.
    pub cursor: Position,
    /// The buffer's document: its snapshot for positions, and the handle a
    /// session ending here sends its cleanup through (untracking ranges).
    pub document: &'a dyn lattice_runtime::Document,
}

/// A session-backed minor mode's predicate: `true` while its mode should be
/// active on the probed buffer. May end the mode's session as a side effect
/// when it answers `false` for a buffer it had one in.
pub type SessionPredicate = Arc<dyn Fn(&SessionProbe<'_>) -> bool + Send + Sync>;
