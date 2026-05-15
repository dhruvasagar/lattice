//! The renderer-agnostic editor state.
//!
//! Phase 5.B.3 introduces [`Editor`] as the destination for
//! the per-cluster field migration from
//! `lattice-ui-tui::App`. See
//! [`docs/dev/architecture/phase-5b-app-design.md`] for the
//! Option-D → Option-E pivot that this struct realises:
//!
//! - The host owns the editor's state and logic in `Editor`.
//! - Each renderer crate composes `Editor` into its own
//!   concrete `App` wrapper alongside its renderer-specific
//!   caches (`theme`, `pane_render_registry`, ...).
//!
//! Subsequent slices (5.B.4 onwards) relocate field clusters
//! one at a time from `App` into `Editor`, moving the methods
//! that touch only those fields into `impl Editor` here. Each
//! per-cluster commit ships green: methods that still live in
//! `impl App` access migrated fields via `self.editor.foo`;
//! methods that have moved access them via `self.foo` (now an
//! inherent method on `Editor`).
//!
//! The empty-now/grows-later shape is intentional: it lets
//! the wrapper field `editor: Editor` get added to `App`
//! before any field actually moves, giving every subsequent
//! migration a target that already exists in the type
//! system.

use std::collections::HashMap;

use lattice_grammar::Register;
use lattice_protocol::position::Position;

use crate::action::Action;
use crate::state::{MacroRecording, UnnamedRegister};

/// Renderer-agnostic editor state.
///
/// The renderer-agnostic half of every editor App. Each
/// renderer's `App` struct composes one of these alongside
/// its renderer-specific caches. Host-level code (mode
/// lifecycle, dispatch, picker sources, LSP supervisor, ...)
/// takes `&mut Editor` directly; renderer-side code takes
/// `&mut App` and reaches the editor via `app.editor`.
///
/// **Field set grows per-cluster.** Each 5.B.x slice
/// migrates a logical cluster of fields here from
/// `lattice-ui-tui::App`. As clusters land, this struct
/// accumulates state; in parallel, `App`'s direct field set
/// shrinks. When the migration completes, every renderer-
/// agnostic field on App lives here, every renderer-agnostic
/// method on App lives in this crate's `impl Editor` blocks,
/// and App becomes a thin wrapper holding `editor: Editor`
/// plus renderer-specific caches only.
///
/// **Clusters landed so far:**
/// - 5.B.4 -- macro recording state (`macros`,
///   `macro_recording`, `last_played_macro`).
/// - 5.B.5 -- marks + registers (`marks`, `registers`,
///   `pending_register`, `unnamed_register`).
#[derive(Debug, Default)]
pub struct Editor {
    /// Completed macro recordings keyed by register name.
    /// Replays go through the dispatch layer's `PlayMacro`
    /// action handler. v1 records `Action` streams; insert-
    /// mode keystrokes ARE captured (every Action::Insert is
    /// recorded), but dot-repeat-style replay of insert content
    /// from `c`/`i`/`a` remains a §15 follow-up.
    pub macros: HashMap<char, Vec<Action>>,
    /// In-flight macro recording. `Some` while between
    /// `q<reg>` start and the matching `q` stop; pushed
    /// Actions append to `actions`.
    pub macro_recording: Option<MacroRecording>,
    /// The most recently played macro register, for `@@`
    /// repeat.
    pub last_played_macro: Option<char>,
    /// Unnamed register -- destination of `y` / `d` / `c`,
    /// source of `p` / `P`. `None` until something has been
    /// yanked.
    pub unnamed_register: Option<UnnamedRegister>,
    /// User-set marks. v1 stores them flat by name (a-z,
    /// A-Z, 0-9); uppercase / numbered global marks treat
    /// all marks as buffer-local since the v1 TUI runs
    /// against a single document.
    pub marks: HashMap<char, Position>,
    /// Named registers `"a-z`, `"A-Z`, numbered `"0-"9`,
    /// etc. Stores content + kind. `""` (the unnamed
    /// register) is [`Self::unnamed_register`]; this map
    /// covers everything else.
    pub registers: HashMap<Register, UnnamedRegister>,
    /// Register selected for the next operator / paste
    /// (`"a` prefix). Consumed-and-cleared by `run_invocation`
    /// (operators) and `do_paste` (paste). `None` means use
    /// unnamed.
    pub pending_register: Option<Register>,
}
