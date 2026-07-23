//! Small App-helper state types -- pure data, no renderer
//! coupling.
//!
//! Phase 5.2: extracted from `lattice-ui-tui::app` so the
//! eventual App migration carries fewer in-line type
//! definitions. Each struct here is a piece of state App holds
//! in a field (search line in progress, last search, unnamed
//! register, prev-pane snapshot). Renderer-agnostic by
//! construction.

use lattice_core::{BufferId, BufferKind, FoldMethod};

use lattice_grammar::{ModalState, SearchDirection, VisualKind, YankKind};

use lattice_protocol::CancellationToken;
use lattice_protocol::position::{Position, Range as ProtoRange};

use crate::action::{Action, FindKind};

/// In-progress `/` or `?` state. The cursor at entry is preserved
/// so Esc can restore it.
#[derive(Debug, Clone)]
pub struct SearchLine {
    pub direction: SearchDirection,
    pub pattern: String,
    pub origin: Position,
}

/// Last completed search -- consulted by `n` and `N`.
#[derive(Debug, Clone)]
pub struct LastSearch {
    pub pattern: String,
    pub direction: SearchDirection,
}

/// The unnamed register's payload. v1 uses a single global slot;
/// the full vim register zoo (`"a-z`, `"+`, `"*`, etc.) lands
/// later.
#[derive(Debug, Clone)]
pub struct UnnamedRegister {
    pub content: String,
    pub kind: YankKind,
}

/// Snapshot of the active pane's state captured just before help
/// took it over. Used by `dismiss_popup` to restore the user to
/// the buffer + cursor + scroll they came from. The same struct
/// serves both display modes (in-pane and popup-overlay).
#[derive(Debug, Clone, Copy)]
pub struct PrevPaneState {
    pub buffer: BufferKind,
    pub buffer_id: BufferId,
    pub cursor: Position,
    pub scroll: u32,
    /// PU-A.1b: the modal state at capture. A focus-stealing popup
    /// (`PopupFocus::Steal`) is a Normal-mode surface — its major mode
    /// receives keys — so open sets `ModalState::Normal` and dismiss
    /// restores this, returning a user who was mid-Insert to their
    /// prompt in Insert (popup-api.md §5). Passive floats never flip
    /// modal, so they never capture a `PrevPaneState`. In-pane help
    /// captures this but is torn down by `do_close_pane` (which ignores
    /// it), so the value is inert there.
    pub modal: ModalState,
}

/// MB.1 (rich minibuffer): the editing state suspended while the
/// `:` command line owns `self.document`. When the `:` line is
/// opened, [`Editor::focus_editing_buffer`] swaps `self.document` /
/// `document_buffer_id` / `active_buffer` to the synthetic
/// `*command-line*` buffer and stashes the prior *editing* focus here
/// **without** touching the pane tree (the active pane keeps rendering
/// its own buffer). [`Editor::restore_editing_buffer`] pops it back on
/// submit / cancel. `Some(_)` is the "command line is focused" flag
/// (`Editor::command_line_active`).
#[derive(Debug, Clone)]
pub struct CommandLineFocus {
    /// The document buffer that was focused for editing before the
    /// `:` line took over. Restored (re-fetched from the registry by
    /// id) when the command line closes.
    pub prior_buffer_id: BufferId,
    /// Kind of the prior active buffer (`Document` / `Messages` / …).
    pub prior_active_buffer: BufferKind,
    /// Cursor in the prior buffer at focus time.
    pub prior_cursor: Position,
    /// Scroll (first visible line) of the prior buffer at focus time.
    pub prior_scroll: u32,
    /// Horizontal scroll of the prior buffer at focus time.
    pub prior_leftcol: u32,
    /// Modal state at focus time, restored on close.
    pub prior_modal: ModalState,
    /// MB.2: whether the `:` line is **expanded** into the full-modal
    /// mini-buffer band (`<C-x><C-e>`). Tier 1 (`false`): the one-row
    /// readline `:` line under `ModalState::Command`. Tier 2 (`true`):
    /// the same `*command-line*` surface grown in place with the full vim
    /// grammar (real Normal / Insert / Visual). Collapsing returns the
    /// edited text to the one-row line for review.
    pub expanded: bool,
}

/// Hot-path option cache. Mirrors the typed-options registry's
/// resolved values for the active buffer; reads on this struct
/// fire on every render tick, so the cache exists to skip a
/// HashMap lookup per option. Repopulated by
/// `App::rebuild_option_cache` after every `:set`.
#[derive(Debug, Clone, Copy)]
pub struct OptionCache {
    pub show_line_numbers: bool,
    pub relative_line_numbers: bool,
    pub wrap_lines: bool,
    /// PU.1b-1a (`signcolumn`): whether the renderer reserves the
    /// gutter sign columns (diagnostics severity + diff sign). `true`
    /// (default) always reserves them so layout never shifts when a
    /// sign appears; `false` (help / synthetic buffers) renders
    /// gutterless. Resolved from the `SignColumnOption` typed option —
    /// the renderer reads only this flag, never the buffer kind.
    pub sign_column: bool,
    pub ignorecase: bool,
    pub tabstop: u32,
    pub foldenable: bool,
    pub foldmethod: FoldMethod,
    pub scrolloff: u32,
    /// Horizontal scroll step (`:set sidescroll`). `0` jump-scrolls
    /// the cursor to the window centre; positive scrolls N columns.
    pub sidescroll: u32,
    /// Horizontal scroll-off margin (`:set sidescrolloff`).
    pub sidescrolloff: u32,
    pub completion_auto_insert_single: bool,
    pub show_whitespace: bool,
    pub current_line_highlight: bool,
    pub whitespace_tab: Option<char>,
    pub whitespace_trailing: Option<char>,
    pub whitespace_leading: Option<char>,
    pub whitespace_space: Option<char>,
    pub whitespace_eol: Option<char>,
    /// Terminal-mode T2.b.0 (2026-05-25): cached
    /// `terminal.esc-exits`. Default `true` so test fixtures
    /// using `Editor::default()` (no `init_from_linkme`) get
    /// the production semantics without panicking on an
    /// unregistered option lookup.
    pub terminal_esc_exits: bool,
    /// D.0b: cached `scrollbind`. Triggers
    /// `rebuild_scrollbind_group` via `apply_option_cascade`
    /// when toggled.
    pub scrollbind: bool,
    /// DB.4: extra leading gutter cells to horizontally centre the active
    /// buffer's content (dashboard). `(viewport_width - content_block_width)/2`,
    /// recomputed on activation + resize. `0` = not centred (the default for
    /// every buffer). The renderer adds this to the gutter width so content +
    /// cursor shift right with no text mutation.
    pub content_left_pad: u32,
}

impl Default for OptionCache {
    fn default() -> Self {
        Self {
            show_line_numbers: true,
            relative_line_numbers: false,
            wrap_lines: false,
            sign_column: true,
            ignorecase: false,
            tabstop: 4,
            foldenable: true,
            foldmethod: FoldMethod::Manual,
            scrolloff: 0,
            sidescroll: 0,
            sidescrolloff: 0,
            completion_auto_insert_single: true,
            show_whitespace: false,
            current_line_highlight: false,
            whitespace_tab: Some('→'),
            whitespace_trailing: Some('·'),
            whitespace_leading: Some('·'),
            whitespace_space: None,
            whitespace_eol: None,
            terminal_esc_exits: true,
            scrollbind: false,
            content_left_pad: 0,
        }
    }
}

/// Capture of the most recent find/till for `;`/`,` repeat.
#[derive(Debug, Clone, Copy)]
pub struct LastFind {
    pub kind: FindKind,
    pub target: char,
}

/// In-progress macro recording. `q<reg>` starts; `q` again
/// stops and persists into the register table.
#[derive(Debug, Clone)]
pub struct MacroRecording {
    pub register: char,
    pub actions: Vec<Action>,
}

/// One entry on the vim-style tag stack. Pushed by `gd` (and
/// the goto-* family) at the pre-jump cursor; popped by `<C-t>`
/// to walk back. Distinct from the jump list because the user's
/// mental model for `<C-t>` is "undo the drill-down chain", not
/// "step through every cursor jump in chronological order".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagStackEntry {
    pub buffer: BufferKind,
    pub buffer_id: BufferId,
    pub position: Position,
    pub label: String,
}

/// One entry in the unified position history (DESIGN.md §5.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionEntry {
    pub position: Position,
    pub source: PositionSource,
    pub buffer: BufferKind,
    pub buffer_id: BufferId,
    /// T3.b.3 (2026-05-25): scrollback row the user was viewing
    /// when the entry was pushed. Only meaningful for
    /// `BufferKind::Terminal` entries — Document jumps ignore
    /// it. `0` = live edge. Restored by `<C-o>` / `<C-i>` when
    /// landing back on a Terminal so the user returns to the
    /// row they were studying.
    pub terminal_scroll_offset: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionSource {
    /// Pushed by "big motions" -- gg, G, search, *, #, %, mark jump.
    AutoJump,
    /// Reserved: `g<C-o>` style "I explicitly want to remember here"
    /// pushes (emacs `set-mark`). Not yet wired to a key.
    ExplicitMark,
    /// Reserved: pushed by plugins (LSP go-to-definition, fuzzy-finder
    /// hop, etc.). Treated like AutoJump for navigation.
    PluginPush,
    /// `mX` named mark. Walks via `g;` / `g,`.
    NamedMark(char),
}

impl PositionEntry {
    /// True for entries that the standard Ctrl-O / Ctrl-I jump-list
    /// walks consume.
    pub fn is_jump(&self) -> bool {
        matches!(
            self.source,
            PositionSource::AutoJump | PositionSource::PluginPush
        )
    }

    /// True for entries the `g;` / `g,` mark-history walks consume.
    pub fn is_named_mark(&self) -> bool {
        matches!(self.source, PositionSource::NamedMark(_))
    }
}

/// One replace-mode entry -- the byte that was at `at` before the
/// overwrite, so `<BS>` can restore it. `original = None` means
/// the overwrite extended the line (the position was past EOL);
/// `<BS>` deletes the inserted char rather than restoring a byte.
#[derive(Debug, Clone)]
pub struct ReplaceEntry {
    pub at: Position,
    pub original: Option<String>,
}

/// Cmdline completion popup state: candidates and selection.
/// Renderer-agnostic; the renderer reads this via &App.
#[derive(Debug, Clone)]
pub struct CompletionState {
    pub candidates: Vec<lattice_completion::RenderedCandidate>,
    pub selected: usize,
    /// Byte offset within the command line where the completed
    /// prefix starts.
    pub replace_start: usize,
    /// Snapshot of the command line at popup-open time.
    pub original_line: String,
}
/// Most-recently-completed visual selection. Used by `gv` to
/// reselect.
#[derive(Debug, Clone, Copy)]
pub struct LastVisual {
    pub anchor: Position,
    pub head: Position,
    pub kind: VisualKind,
}

/// Snapshot of an in-progress `:s/pat/repl/...` preview.
/// Refreshed on every cmdline keystroke while the input parses as
/// a substitute; consumed by the renderer to overlay match ranges
/// (and the typed replacement, when present) on the target buffer.
#[derive(Debug, Clone)]
pub struct SubstitutePreview {
    /// Match ranges in the target line(s).
    pub matches: Vec<ProtoRange>,
    /// The user-typed replacement template, once the second `/`
    /// has been entered. None while the user is still inside the
    /// pattern field.
    pub replacement: Option<String>,
    /// Whether the user has explicitly typed flags including 'g'.
    pub global: bool,
}

/// 5.5.G.23.cmdline: result of resolving a missing required first
/// arg on `:`-submit. Built by `Editor::try_resolve_missing_arg_prompt`
/// and consumed by `do_command_line_submit`.
#[derive(Debug, Clone)]
pub struct MissingArgPrompt {
    /// New value for `command_line`. Already contains the command
    /// word + bang + a trailing space; the cursor lands at end-of-
    /// line, in the first arg slot.
    pub prefill: String,
    /// Kind of the first arg. Drives whether the host arms the
    /// chord-capture overlay (kind == Chord) or just leaves the
    /// cmdline open for typed input.
    pub kind: lattice_grammar::ArgKind,
    /// Prompt text for the echo area, taken from the schema's
    /// `prompt` field (or `"<name>:"` when empty).
    pub prompt: String,
}

/// In-flight blockwise-visual insert (`I` or `A`).
///
/// When the user enters `I` from blockwise visual, the typed
/// prefix is replicated to every line in the block at the same
/// column on Esc. We capture the rectangle's lines and the
/// per-line insert column at entry time, then replay the
/// recorded text to all lines except the top one (the top row
/// was edited live during the Insert session).
#[derive(Debug, Clone, Copy)]
pub struct PendingBlockInsert {
    pub start_line: u32,
    pub end_line: u32,
    pub insert_col: u32,
    pub live_edits: u32,
}

/// In-flight async picker init. The future from
/// `PickerSourceGenerator::init` is spawned on the LSP
/// runtime; its resolved batch lands here via `rx`. The
/// `cancel` token lets a subsequent `:picker <source>` drop
/// the predecessor before it completes.
pub struct PendingPickerInit {
    pub source_id: String,
    pub generator: std::sync::Arc<dyn lattice_picker::PickerSourceGenerator>,
    pub rx: tokio::sync::mpsc::UnboundedReceiver<
        lattice_picker::SourceResult<lattice_picker::CandidateBatch>,
    >,
    pub cancel: CancellationToken,
}

impl std::fmt::Debug for PendingPickerInit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingPickerInit")
            .field("source_id", &self.source_id)
            .finish_non_exhaustive()
    }
}

/// In-flight async picker *accept*. The future from
/// `PickerSourceGenerator::accept_async` is spawned on the LSP
/// runtime (a plugin source's accept is an async guest call
/// bound to its actor task, so it must not block the actor
/// thread); its resolved outcome lands here via `rx` and is
/// applied by `drain_pending_picker_accept`. `target` is the
/// open-target override consumed at accept time and re-applied
/// when the outcome commits. Mirrors [`PendingPickerInit`]; the
/// `cancel` token drops a superseded accept (a rapid second
/// accept before this one drains).
pub struct PendingPickerAccept {
    pub source_id: String,
    pub target: lattice_picker::OpenTarget,
    pub rx: tokio::sync::mpsc::UnboundedReceiver<
        lattice_picker::SourceResult<lattice_picker::outcome::PickerAcceptOutcome>,
    >,
    pub cancel: CancellationToken,
}

impl std::fmt::Debug for PendingPickerAccept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingPickerAccept")
            .field("source_id", &self.source_id)
            .finish_non_exhaustive()
    }
}

/// Live-picker query state. Installed when `open_picker`
/// resolves a source whose `spec().live` is true; survives
/// until the picker is dismissed.
///
/// Two phases:
///
/// 1. **Debouncing.** `debounce_until = Some(deadline)` while
///    a keystroke is pending. Every fresh keystroke
///    reschedules the deadline forward. Once `Instant::now()
///    >= deadline`, the main-loop drain fires
///    `PickerSourceGenerator::on_query_changed` and clears
///    `debounce_until` to None.
/// 2. **In-flight.** When `on_query_changed` returns a
///    `Future` or a `Stream`, the spawned task lands its
///    result on `inflight.rx`. The drain seats new raw
///    candidates (if the result is still relevant) or drops
///    the result (if the user has typed past the launched
///    query).
pub struct LivePickerQueryState {
    pub source_id: String,
    pub generator: std::sync::Arc<dyn lattice_picker::PickerSourceGenerator>,
    pub debounce_until: Option<std::time::Instant>,
    pub inflight: Option<InFlightLiveQuery>,
    /// Set by `open_picker` from the first positional arg
    /// when the source is live; consumed (taken) by
    /// `seat_picker_from_pairs` on the first seat so the
    /// picker prompt opens pre-populated with the user's
    /// `:picker grep <pattern>` argument. None for live
    /// pickers opened without an initial pattern.
    pub initial_query: Option<String>,
}

impl std::fmt::Debug for LivePickerQueryState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LivePickerQueryState")
            .field("source_id", &self.source_id)
            .field("debounce_until", &self.debounce_until)
            .field("inflight", &self.inflight)
            .field("initial_query", &self.initial_query)
            .finish_non_exhaustive()
    }
}

/// Spawned `on_query_changed` future / stream paired with
/// the query it was launched against. The drain compares
/// `launched_for_query` against the picker's live query;
/// if they differ, the user has kept typing and a newer fire
/// is already in flight (or coming via debounce) -- discard
/// the stale result.
pub struct InFlightLiveQuery {
    pub cancel: CancellationToken,
    pub rx: tokio::sync::mpsc::UnboundedReceiver<
        lattice_picker::SourceResult<lattice_picker::PickerInitResult>,
    >,
    pub launched_for_query: String,
}

impl std::fmt::Debug for InFlightLiveQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InFlightLiveQuery")
            .field("launched_for_query", &self.launched_for_query)
            .finish_non_exhaustive()
    }
}

/// Debounce window before a live picker's query change
/// fires `on_query_changed`. Telescope uses ~150ms; chosen so
/// that burst keystrokes coalesce into one source call
/// without feeling laggy. Constant lives here (not in
/// `lattice-picker`) because debounce is host policy -- the
/// picker primitive is renderer- and timer-agnostic.
pub const LIVE_PICKER_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(150);

/// Sidecar metadata for snippet candidates in the active
/// insert-completion popup. Indexed by the candidate's
/// `CandidateData::Extension { payload }` (u32 LE) -- same
/// shape as the LSP source's sidecar. The host renders the
/// snippet body on accept and starts an `ActiveSnippet`; this
/// struct carries the parsed body plus the display fields the
/// popup row uses.
#[derive(Debug, Clone)]
pub struct SnippetCandidateMeta {
    pub name: String,
    pub prefix: String,
    pub description: Option<String>,
    pub body: lattice_snippet::SnippetBody,
}
