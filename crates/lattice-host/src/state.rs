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

/// In-progress `/` or `?` search state (MB.5a). The pattern text is
/// NOT stored here — it lives in the focused `*search-line*` buffer
/// (single source of truth, read via `Editor::search_pattern`), the
/// same way the `:` line reads `*command-line*`. This struct is the
/// "search is active" marker + the metadata the buffer can't carry:
/// the direction (`/` vs `?`) and the `origin` cursor preserved so
/// `<Esc>` restores it and incremental preview anchors its search.
#[derive(Debug, Clone)]
pub struct SearchLine {
    pub direction: SearchDirection,
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

/// MB.1 / MB.5a (rich minibuffer): the editing state suspended while a
/// **minibuffer prompt** (`:` command line or `/`·`?` search line) owns
/// `self.document`. When a prompt opens,
/// [`Editor::focus_editing_buffer`] swaps `self.document` /
/// `document_buffer_id` / `active_buffer` to the synthetic
/// `*command-line*` / `*search-line*` buffer and stashes the prior
/// *editing* focus here **without** touching the pane tree (the active
/// pane keeps rendering its own buffer). [`Editor::restore_editing_buffer`]
/// pops it back on submit / cancel. `Some(_)` is the "a prompt is
/// focused" flag; whether it's the command line vs the search line is
/// decided by `Editor::search_line` (`command_line_active` /
/// `search_line_active`). Prompt-agnostic by design so `git-commit-line`
/// / `repl-input` reuse it unchanged (design §6).
#[derive(Debug, Clone)]
pub struct MinibufferFocus {
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
    /// IG.3: `display.indent-guides.char`, the glyph the TUI substitutes
    /// into a guide column. `None` (empty string) ⇒ the TUI draws no
    /// guides. The GPU peer paints a rule and ignores this.
    pub indent_guide_char: Option<char>,
    /// IG.3: `display.indent-guides.active` — draw the block enclosing
    /// the cursor in its own style. Whether guides exist at all is
    /// `display.indent-guides`, which is resolved by the worker (it
    /// changes what gets built, not how it is painted).
    pub indent_guide_active: bool,
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
            indent_guide_char: Some('\u{2502}'),
            indent_guide_active: true,
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

/// TR.2: in-flight async **transient build**. A guest-backed
/// menu's builder answers a `TransientBuildFuture` rather than a
/// spec; it is spawned on the LSP runtime and its resolved spec
/// lands here via `rx`, seated by
/// `drain_pending_transient_build` on the async-landed wake.
///
/// `source` is kept for the error echo: a build that fails must
/// name the menu that failed, or the user sees a chord that did
/// nothing.
///
/// Single-slot like [`PendingPickerInit`], and for the same
/// reason — a second `Effect::OpenTransient` before the first
/// lands supersedes it, so the older future is cancelled rather
/// than racing to seat a menu the user has moved on from.
pub struct PendingTransientBuild {
    pub source: String,
    pub rx: tokio::sync::mpsc::UnboundedReceiver<Result<lattice_picker::TransientSpec, String>>,
    pub cancel: CancellationToken,
}

impl std::fmt::Debug for PendingTransientBuild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingTransientBuild")
            .field("source", &self.source)
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

// ─────────────────────────────────────────────────────────────
// YR.1 — the yank ring
// ─────────────────────────────────────────────────────────────

/// A bounded history of everything that has been yanked or deleted.
///
/// Beside [`UnnamedRegister`] rather than in `lattice-grammar` (where the
/// slice plan proposed it) because that is where the entry type it holds
/// already lives; the grammar does not read the ring.
///
/// **Deletes push too, and the system clipboard still does not take
/// them.** That looks like a contradiction of `clipboard.md` §5, which
/// keeps deletes out of the clipboard on purpose — vim's `unnamedplus`
/// wart, where an incidental `x` clobbers what you copied from a browser.
/// The two stores have different blast radii. The clipboard is shared
/// with every other application, so a stray write there destroys
/// something the editor never owned; the ring is internal, bounded and
/// additive, so an `x` landing in it costs one slot and destroys
/// nothing. "Get back the line I just deleted" is also among the most
/// common reasons to open the picker at all, and a ring holding only
/// yanks would decline the question users most want to ask it.
#[derive(Debug, Clone, Default)]
pub struct YankRing {
    /// Newest first. Front is the most recent entry, which is what the
    /// `"0`–`"9` projection (YR.2) and the picker both read from.
    entries: std::collections::VecDeque<RingEntry>,
}

/// YR.2: a ring slot — the register plus how it got here.
///
/// The yank/delete flag lives HERE rather than on [`UnnamedRegister`]
/// because the ring is its only consumer: `"0` projects the newest
/// *yank*, `"1`–`"9` the newest *deletes*. Putting it on the register
/// would add a field to every named register and the unnamed one, none
/// of which can answer a question about provenance — and it would have
/// to be kept truthful at every construction site rather than at the
/// one seam (`store_yank`) that actually knows.
#[derive(Debug, Clone)]
pub struct RingEntry {
    pub register: UnnamedRegister,
    /// True when this came from an explicit yank, false from a delete.
    pub yanked: bool,
}

impl YankRing {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of entries currently held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries newest-first. The picker shows yanks and deletes alike,
    /// so this does not filter.
    pub fn iter(&self) -> impl Iterator<Item = &UnnamedRegister> {
        self.entries.iter().map(|e| &e.register)
    }

    /// The `n`th newest entry, 0-based. `nth(0)` is the most recent.
    pub fn nth(&self, n: usize) -> Option<&UnnamedRegister> {
        self.entries.get(n).map(|e| &e.register)
    }

    /// YR.2: `"0` — the newest **yank**.
    ///
    /// Not `nth(0)`: an intervening delete must not shadow the last
    /// thing you deliberately copied, which is the whole reason vim
    /// keeps `"0` distinct from `""`.
    pub fn newest_yank(&self) -> Option<&UnnamedRegister> {
        self.entries.iter().find(|e| e.yanked).map(|e| &e.register)
    }

    /// YR.2: `"1`–`"9` — the `n`th newest **delete**, 0-based.
    ///
    /// `nth_delete(0)` is `"1`. Yanks are skipped rather than counted,
    /// so `"1` is always "the last thing I deleted" no matter how many
    /// yanks happened in between.
    pub fn nth_delete(&self, n: usize) -> Option<&UnnamedRegister> {
        self.entries
            .iter()
            .filter(|e| !e.yanked)
            .nth(n)
            .map(|e| &e.register)
    }

    /// Record a yank or a delete, trimming to `capacity`.
    ///
    /// Two duplicate rules, and they are deliberately different:
    ///
    /// - **A consecutive repeat collapses.** `yy` pressed twice, or a
    ///   re-yank of an unchanged line, otherwise produces two identical
    ///   rows the picker cannot help you tell apart. The existing front
    ///   entry is left in place rather than removed and re-pushed, so
    ///   the ring does not churn on a held key.
    /// - **A non-consecutive repeat is promoted.** Re-yanking something
    ///   from an hour ago is a real event, and moving it to the top is
    ///   the useful answer — it is what you are about to paste. Adding a
    ///   second row for it would not be.
    ///
    /// Eviction is oldest-first, which is what lets YR.2's `"0`–`"9`
    /// projection be sound: the numbered registers read the newest
    /// entries, so dropping from the back can never change what `"9`
    /// means.
    ///
    /// `capacity` is passed in rather than held on the ring because it
    /// is a live option (`yank.ring.size`) — reading it at push time is
    /// what makes lowering it take effect on the next yank instead of at
    /// the next restart. A capacity of 0 disables the ring.
    pub fn push(&mut self, entry: UnnamedRegister, yanked: bool, capacity: usize) {
        if capacity == 0 {
            self.entries.clear();
            return;
        }
        match self.entries.front() {
            // Consecutive duplicate: already at the top, nothing to do.
            Some(front)
                if front.register.content == entry.content && front.register.kind == entry.kind =>
            {
                return;
            }
            _ => {}
        }
        // Non-consecutive repeat: promote rather than duplicate.
        //
        // YR.2: the promoted slot takes the NEW provenance. Deleting
        // something you yanked an hour ago makes it the newest delete,
        // and `"1` should find it — keeping the stale `yanked` flag
        // would leave it addressable only as `"0`.
        if let Some(pos) = self
            .entries
            .iter()
            .position(|e| e.register.content == entry.content && e.register.kind == entry.kind)
        {
            self.entries.remove(pos);
        }
        self.entries.push_front(RingEntry {
            register: entry,
            yanked,
        });
        while self.entries.len() > capacity {
            self.entries.pop_back();
        }
    }
}

#[cfg(test)]
mod yr2_projection_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn reg(s: &str) -> UnnamedRegister {
        UnnamedRegister {
            content: s.to_string(),
            kind: YankKind::Charwise,
        }
    }

    /// `"0` is the newest YANK, not the newest entry. An intervening
    /// delete must not shadow the last thing you deliberately copied —
    /// that is the whole reason vim keeps `"0` distinct from `""`.
    #[test]
    fn register_zero_skips_deletes() {
        let mut ring = YankRing::new();
        ring.push(reg("yanked"), true, 10);
        ring.push(reg("deleted"), false, 10);

        assert_eq!(ring.nth(0).unwrap().content, "deleted", "newest overall");
        assert_eq!(
            ring.newest_yank().unwrap().content,
            "yanked",
            "`\"0` must survive an intervening delete"
        );
    }

    /// `"1`–`"9` count deletes only. Yanks in between are skipped rather
    /// than counted, so `"1` is always "the last thing I deleted".
    #[test]
    fn numbered_registers_count_deletes_only() {
        let mut ring = YankRing::new();
        ring.push(reg("d3"), false, 10);
        ring.push(reg("y"), true, 10);
        ring.push(reg("d2"), false, 10);
        ring.push(reg("d1"), false, 10);

        assert_eq!(ring.nth_delete(0).unwrap().content, "d1", "\"1");
        assert_eq!(ring.nth_delete(1).unwrap().content, "d2", "\"2");
        assert_eq!(
            ring.nth_delete(2).unwrap().content,
            "d3",
            "\"3 — the yank between d2 and d3 is skipped, not counted"
        );
        assert!(ring.nth_delete(3).is_none());
    }

    /// An empty ring projects to nothing rather than to a wrong answer.
    #[test]
    fn an_empty_ring_projects_to_none() {
        let ring = YankRing::new();
        assert!(ring.newest_yank().is_none());
        assert!(ring.nth_delete(0).is_none());
    }

    /// A promoted repeat takes the NEW provenance. Deleting something you
    /// yanked earlier makes it the newest delete, and `"1` must find it;
    /// keeping the stale flag would leave it addressable only as `"0`.
    #[test]
    fn a_promoted_entry_takes_its_new_provenance() {
        let mut ring = YankRing::new();
        ring.push(reg("shared"), true, 10);
        ring.push(reg("other"), false, 10);
        // Same content, now arriving as a delete → promoted to front.
        ring.push(reg("shared"), false, 10);

        assert_eq!(ring.nth_delete(0).unwrap().content, "shared");
        assert!(
            ring.newest_yank().is_none(),
            "the only yank was re-classified when it was promoted"
        );
    }

    /// The picker sees everything; only the numbered projection filters.
    #[test]
    fn iteration_is_unfiltered() {
        let mut ring = YankRing::new();
        ring.push(reg("y"), true, 10);
        ring.push(reg("d"), false, 10);
        let all: Vec<_> = ring.iter().map(|r| r.content.as_str()).collect();
        assert_eq!(all, vec!["d", "y"]);
    }
}
