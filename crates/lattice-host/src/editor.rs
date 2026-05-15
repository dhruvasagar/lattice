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
use std::path::PathBuf;

use lattice_grammar::Register;
use lattice_protocol::position::{Position, Range as ProtoRange};

use crate::action::Action;
use crate::state::{
    LastSearch, MacroRecording, PositionEntry, SearchLine, SubstitutePreview, TagStackEntry,
    UnnamedRegister,
};

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
/// - 5.B.6 -- position history + tag stack
///   (`position_history`, `position_history_cursor`,
///   `recent_files`, `tag_stack`, `pending_tag_origin`).
/// - 5.B.7 -- search state (`search_line`, `last_search`,
///   `current_match`, `all_matches`, `substitute_preview`).
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
    /// Unified position-history ring (DESIGN.md §5.1.1).
    /// Every entry is tagged by source so different keybindings
    /// walk filtered views of the same data (`Ctrl-O` / `Ctrl-I`
    /// walk `AutoJump` + `PluginPush`; `g;` / `g,` walk
    /// `NamedMark`).
    pub position_history: Vec<PositionEntry>,
    /// Cursor into [`Self::position_history`] -- the next entry
    /// the navigation action would visit.
    pub position_history_cursor: usize,
    /// MRU list of canonical paths the user has opened via
    /// `:edit` (or any path flowing through `do_edit`). Newest
    /// first; deduplicated; capped at `MAX_RECENT_FILES`. Source
    /// for the `:recent` picker.
    pub recent_files: Vec<PathBuf>,
    /// Vim-style tag stack (DESIGN.md §5.1.1 follow-up).
    /// Distinct from the jump list: each "drill-down" navigation
    /// (`gd` / `gD` / `gy` / `gI` and their multi-result picker
    /// accept variants) pushes one entry; `<C-t>` pops the most
    /// recent entry. `<C-o>` walks all jumps chronologically;
    /// `<C-t>` pops only the LIFO tag-style drill-downs.
    pub tag_stack: Vec<TagStackEntry>,
    /// Pre-jump origin captured when an LSP nav request fires;
    /// transferred to [`Self::tag_stack`] on the actual jump
    /// (single-result drain or multi-result picker accept).
    /// Cleared on picker dismiss / nav cancellation / drain
    /// with no results.
    pub pending_tag_origin: Option<TagStackEntry>,
    /// In-progress `/` or `?` search. `Some` only while
    /// `modal == ModalState::Search(_)`.
    pub search_line: Option<SearchLine>,
    /// Most recent submitted search; consulted by `n` / `N`.
    pub last_search: Option<LastSearch>,
    /// Range of the most recent search match, used to draw
    /// the primary highlight in the buffer view. Cleared on
    /// Esc and on cursor motion.
    pub current_match: Option<ProtoRange>,
    /// Every occurrence of the most recent search pattern,
    /// used to draw the secondary "hlsearch" overlay.
    /// Cleared on Esc; persists after submit until the next
    /// search.
    pub all_matches: Vec<ProtoRange>,
    /// In-progress substitute preview. Populated as the user
    /// types `:s/pat...`; the renderer overlays match ranges
    /// (and the typed replacement once the second `/` has
    /// been entered) so the user sees the substitution before
    /// pressing Enter. Cleared when the cmdline closes or the
    /// input no longer parses as a substitute (DESIGN.md
    /// §5.9.10).
    pub substitute_preview: Option<SubstitutePreview>,
}
