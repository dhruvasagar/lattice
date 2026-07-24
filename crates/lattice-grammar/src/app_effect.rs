//! `AppEffect` -- the typed App-side effects produced by free-form
//! `CommandKind::Action` registrations.
//!
//! Background: the keymap-bound `Action` enum in `lattice-ui-tui`
//! historically encoded the App's response to non-grammar chords
//! (`<Esc>`, `o`, `<C-w>v`, ...). Slice 8.i (see
//! [`docs/dev/notes/8i-approach.md`](../../../docs/dev/notes/8i-approach.md)) retires
//! the per-binding `Action` bridge and routes those chords through
//! the unified dispatcher's `CommandKind::Action` branch instead.
//! Action-kind registry entries return `Effect::AppAction(AppEffect)`;
//! the App's `apply_effect` then matches on the inner `AppEffect`
//! variant.
//!
//! Why a sibling type to `Effect` rather than a flat extension:
//!
//! - `Effect`'s existing variants describe *core / dispatcher-native*
//!   work (`Edits`, `SelectionChange`, `Yank`, `EnterMode`,
//!   `Substitute`, ...) and *ex-command-emitted* App work
//!   (`SaveBuffer`, `OpenBuffer`, `LspRestart`, ...). Both are
//!   produced by code paths that *resolve* a typed grammar concern
//!   into a typed effect.
//! - `AppEffect` is for chord bindings that historically had no
//!   grammar concept attached -- `<Esc>` exits Visual, `<C-w>v`
//!   splits a pane, `o` opens a line below. Treating those as
//!   first-class "free-form actions" keeps the dispatcher contract
//!   ("everything returns Effect") honest without fusing two
//!   conceptually different surfaces into one giant enum.
//!
//! 8.i.0 ships the carrier with `Quit` only -- the smallest
//! variant that proves the dispatcher path. Slices 8.i.1-3 grow
//! `AppEffect` as the per-mode bindings migrate; slice 8.i.4
//! retires the `bind_legacy` bridge entirely.

use serde::{Deserialize, Serialize};

use crate::modal::{ModalState, SearchDirection, VisualKind};
use crate::register::Register;
use crate::registry::OperatorId;

// M.4 follow-up: `PaneDirection` moved to `lattice-core::ui::pane`
// (the same crate as the pane geometry). lattice-grammar
// re-exports it so `AppEffect::NavigatePane(PaneDirection)`
// remains ergonomic.
pub use lattice_core::ui::pane::PaneDirection;

/// Vim's `H` / `M` / `L` target positions: where in the visible
/// viewport the cursor lands. App-side concept hosted here so
/// `AppEffect::JumpViewport(ViewportPos)` can carry the typed
/// payload without `lattice-ui-tui` having to dance through a
/// dependency cycle. Slice 8.i.2.c hoist; the App's previous
/// `crate::app::ViewportPos` becomes a `pub use` re-export of
/// this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewportPos {
    /// `H` -- first visible line of the viewport.
    Top,
    /// `M` -- middle visible line of the viewport.
    Middle,
    /// `L` -- last visible line of the viewport.
    Bottom,
}

/// Vim's `zz` / `zt` / `zb` target positions: where in the
/// viewport the cursor's current line should sit after the
/// scroll. App-side concept hosted alongside [`ViewportPos`] for
/// the same dependency reason. Slice 8.i.2.c hoist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrollPos {
    /// `zt` -- cursor's line lands at the top of the viewport.
    Top,
    /// `zz` -- cursor's line lands at the vertical centre.
    Center,
    /// `zb` -- cursor's line lands at the bottom of the viewport.
    Bottom,
}

/// Manual horizontal-scroll commands (HS.2), the vim `z{l,h,L,H,s,e}`
/// family. Carried by [`AppEffect::HorizontalScroll`]; the host
/// handler mutates `leftcol` and keeps the cursor inside the new
/// window. `wrap`-off only (no-op under wrap, like the cursor-follow
/// clamp). See `docs/dev/architecture/horizontal-scroll.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HScroll {
    /// `zl` / `zh`: scroll `count` columns right / left.
    Columns { right: bool },
    /// `zL` / `zH`: scroll half the body width right / left.
    HalfScreen { right: bool },
    /// `zs` (cursor to left edge) / `ze` (cursor to right edge).
    CursorToEdge { end: bool },
}

/// CM.2 (2026-07-22): which error entry a [`AppEffect::ErrorNav`]
/// should resolve to. Carried in the AppEffect so the whole
/// `:cnext` / `:cprev` / `:cc` / `:cfirst` / `:clast` / `]q` / `[q`
/// family shares a single host handler (`Editor::do_error_nav`).
/// The error list is core substrate (like the jump ring), so a
/// host AppEffect variant is the right carrier — not a
/// provider-specific one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorTarget {
    /// `:cnext` / `]q` — next entry (wraps to first past the end).
    Next,
    /// `:cprev` / `[q` — previous entry (wraps to last past the start).
    Prev,
    /// `:cc [N]` — jump to the Nth entry (1-based). `None` (bare
    /// `:cc`) re-visits the current entry.
    Jump(Option<usize>),
    /// `:cfirst` / `[Q` — jump to the first entry.
    First,
    /// `:clast` / `]Q` — jump to the last entry.
    Last,
    /// `:cnextfile` / `]qf` — first entry of the next file (wraps).
    NextFile,
    /// `:cprevfile` / `[qf` — first entry of the previous file (wraps).
    PrevFile,
}

/// App-side typed effect produced by a `CommandKind::Action`
/// dispatch (DESIGN.md §5.2.1, see also `docs/dev/notes/8i-approach.md`).
///
/// Insert-mode line-editing operations — the general readline/vim chords
/// available in **every** buffer (part of the built-in grammar keymap, not a
/// mode). Distinct from Normal-mode motions/operators because Insert cursor
/// semantics differ (the caret sits *between* bytes and may rest past the last
/// char), so `<C-e>` lands after the last byte where `$` would land on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InsertLineEdit {
    /// `<C-a>`: caret to the start of the line (byte 0).
    CursorLineStart,
    /// `<C-e>`: caret to the end of the line (past the last byte).
    CursorLineEnd,
    /// `<C-b>`: caret one byte left (stops at line start).
    CursorCharLeft,
    /// `<C-f>`: caret one byte right (stops past the last byte).
    CursorCharRight,
    /// `<C-w>`: delete the word before the caret.
    DeleteWordBackward,
    /// `<C-u>`: delete from the line start to the caret.
    DeleteToLineStart,
    /// `<C-k>`: delete from the caret to the line end.
    KillToLineEnd,
    /// `<C-t>`: indent the current line by one shiftwidth.
    IndentLine,
    /// `<C-d>`: dedent the current line by one shiftwidth.
    DedentLine,
}

/// Variants are added incrementally during slice 8.i as each
/// historical `Action` variant is promoted from the legacy
/// `bind_legacy` bridge to a typed `CommandInvocation`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppEffect {
    /// Graceful editor shutdown. The App's `apply_effect` arm
    /// publishes `Event::BeforeQuit` and sets the `should_quit`
    /// flag the runtime polls between frames; matches today's
    /// `Action::Quit` semantics exactly. (8.i.0 smoke variant;
    /// the `<C-c>` binding lives in `input.rs` so the legacy
    /// site doesn't migrate until the input-layer rewrite.)
    Quit,
    /// Vim's `%`. Jumps to the bracket / brace / paren matching
    /// the one at-or-after the cursor on the current line.
    /// Promoted from `Action::MatchBracket` in slice 8.i.1.a.
    MatchBracket,
    /// Vim's `~`. Toggles the case of the char at the cursor and
    /// advances by one byte. Promoted from
    /// `Action::ToggleCaseAtCursor` in slice 8.i.1.a.
    ToggleCaseAtCursor,
    /// Vim's `o`. Opens a new line below the current line and
    /// enters Insert. Promoted from `Action::OpenLineBelow` in
    /// slice 8.i.1.a.
    OpenLineBelow,
    /// Vim's `O`. Opens a new line above the current line and
    /// enters Insert. Promoted from `Action::OpenLineAbove` in
    /// slice 8.i.1.a.
    OpenLineAbove,
    // L7: `AppEffect::LspHoverRequest` removed — `K` is mode-owned now
    // (`lsp-mode`'s `action_handlers()` emits `Effect::Lsp(LspRequest::Hover)`).
    /// Vim's `n`. Re-runs the last search forward. Promoted from
    /// `Action::SearchNext` in slice 8.i.1.b.
    SearchNext,
    /// Vim's `N`. Re-runs the last search in the reverse
    /// direction. Promoted from `Action::SearchPrevious` in
    /// slice 8.i.1.b.
    SearchPrevious,
    /// Vim's `<C-o>`. Walk one step backward through the position
    /// history (DESIGN.md §5.1.1). Promoted from
    /// `Action::JumpHistoryBack` in slice 8.i.1.b.
    JumpHistoryBack,
    /// Vim's `<C-i>` / `<Tab>`. Walk one step forward through the
    /// position history. Promoted from `Action::JumpHistoryForward`
    /// in slice 8.i.1.b.
    JumpHistoryForward,
    /// Vim's `g;`. Walk one step backward through the
    /// mark history (oldest -> newest cursor positions in this
    /// buffer). Promoted from `Action::WalkMarkHistoryBack` in
    /// slice 8.i.1.b.
    WalkMarkHistoryBack,
    /// Vim's `g,`. Walk one step forward through the mark
    /// history. Promoted from `Action::WalkMarkHistoryForward` in
    /// slice 8.i.1.b.
    WalkMarkHistoryForward,
    /// Vim's `<C-t>`. Pop one entry off the tag stack and jump
    /// back to where the previous `gd` / `<C-]>` originated.
    /// Promoted from `Action::TagStackPop` in slice 8.i.1.b.
    TagStackPop,
    /// Vim's `zo`. Open the fold containing the cursor.
    /// Promoted from `Action::OpenFoldAtCursor` in slice 8.i.1.c.
    OpenFoldAtCursor,
    /// Vim's `zc`. Close the fold containing the cursor.
    /// Promoted from `Action::CloseFoldAtCursor` in slice 8.i.1.c.
    CloseFoldAtCursor,
    /// Vim's `za`. Toggle the fold containing the cursor.
    /// Promoted from `Action::ToggleFoldAtCursor` in slice 8.i.1.c.
    ToggleFoldAtCursor,
    /// Vim's `zR`. Open every fold in the buffer.
    /// Promoted from `Action::OpenAllFolds` in slice 8.i.1.c.
    OpenAllFolds,
    /// Vim's `zM`. Close every fold in the buffer.
    /// Promoted from `Action::CloseAllFolds` in slice 8.i.1.c.
    CloseAllFolds,
    /// org-cycle `z<Space>` / `:fold-cycle`. Cycle the heading/fold under
    /// the cursor through emacs org-mode's local states
    /// FOLDED → CHILDREN → SUBTREE.
    CycleFoldAtCursor,
    /// org-cycle `z<Tab>` / `:fold-cycle-global`. Cycle the WHOLE buffer
    /// through OVERVIEW → CONTENTS → SHOW-ALL.
    CycleFoldsGlobal,
    /// `zp` / `:fold-goto-parent`. Move the cursor to the parent heading
    /// (one level up the fold hierarchy) — emacs `outline-up-heading`.
    GotoParentFold,
    /// Vim's `zd`. Delete the fold containing the cursor (drop
    /// it from the manual fold table; structure-driven folds
    /// reappear on the next reparse). Promoted from
    /// `Action::DeleteFoldAtCursor` in slice 8.i.1.c.
    DeleteFoldAtCursor,
    /// Vim's `zj`. Move cursor to the start of the next fold.
    /// Promoted from `Action::GotoNextFold` in slice 8.i.1.c.
    GotoNextFold,
    /// Vim's `zk`. Move cursor to the end of the previous fold.
    /// Promoted from `Action::GotoPrevFold` in slice 8.i.1.c.
    GotoPrevFold,
    /// Vim's `zi`. Toggle the `foldenable` option (when off,
    /// every line renders flat regardless of any closed flag).
    /// Promoted from `Action::ToggleFoldEnable` in slice 8.i.1.c.
    ToggleFoldEnable,
    /// Vim's `u`. Undo the last buffer change. Promoted from
    /// `Action::Undo` in slice 8.i.1.d.
    Undo,
    /// Vim's `<C-r>`. Redo the last undone change. Promoted from
    /// `Action::Redo` in slice 8.i.1.d.
    Redo,
    /// Vim's `.`. Repeat the last change (operator + motion +
    /// register + count). Promoted from `Action::RepeatLastChange`
    /// in slice 8.i.1.d.
    RepeatLastChange,
    /// Vim's `<C-f>`. Page-down: scroll the viewport down one
    /// page. Promoted from `Action::PageDown` in slice 8.i.1.d.
    PageDown,
    /// Vim's `<C-b>`. Page-up: scroll the viewport up one page.
    /// Promoted from `Action::PageUp` in slice 8.i.1.d.
    PageUp,
    /// Vim's `<C-y>`. Scroll viewport up one line (cursor
    /// stays at the same screen position when possible).
    /// Promoted from `Action::ScrollLineUp` in slice 8.i.1.d.
    ScrollLineUp,
    /// Vim's `<C-e>`. Scroll viewport down one line. Promoted
    /// from `Action::ScrollLineDown` in slice 8.i.1.d.
    ScrollLineDown,
    /// Vim's `<C-l>`. Force a full screen redraw. Promoted from
    /// `Action::RedrawScreen` in slice 8.i.1.e.
    RedrawScreen,
    /// Vim's `:` / Emacs' `M-x`. Open the command picker over
    /// all registered ex-commands. If the chosen command has a
    /// required first argument the picker arms the cmdline so
    /// the user can supply it; otherwise executes immediately.
    OpenCommandPicker,
    /// MB.3: Vim's `q:`. Open the command-line *history* picker
    /// over `command_history`. `<CR>` loads the chosen command
    /// into the `:` line WITHOUT executing (the user tweaks /
    /// `<C-x><C-e>` expands, then `<CR>`s). Fired from an ordinary
    /// buffer's Normal mode or the expanded tier-2 band's Normal
    /// mode (where it seeds the picker filter with the in-progress
    /// command-line text).
    OpenHistoryPicker,
    /// Vim's `:`. Enter the command-line minibuffer. Promoted
    /// from `Action::EnterCommandLine` in slice 8.i.1.e.
    EnterCommandLine,
    /// MB.1: `<CR>` in `command-line-mode` — submit (or accept the
    /// open completion candidate). Drives the rewired
    /// `Editor::do_command_line_submit`.
    CommandLineSubmit,
    /// MB.1: `<Esc>` / `<C-c>` in `command-line-mode` — cancel (or
    /// dismiss the open completion popup first).
    CommandLineCancel,
    /// MB.1: `<C-p>` / `<Up>` in `command-line-mode` — walk history
    /// backward (or previous completion candidate when the popup is open).
    CommandLineHistoryPrev,
    /// MB.1: `<C-n>` / `<Down>` in `command-line-mode` — walk history
    /// forward (or next completion candidate when the popup is open).
    CommandLineHistoryNext,
    /// MB.1: `<Tab>` in `command-line-mode` — open the completion popup
    /// or advance the selection.
    CommandLineComplete,
    /// MB.1: `<S-Tab>` in `command-line-mode` — previous completion
    /// candidate.
    CommandLineCompletePrev,
    /// MB.1: `<C-h>` in `command-line-mode` — describe the command / arg
    /// under the cursor.
    CommandLineDescribeUnderCursor,
    /// MB.2: `<C-x><C-e>` in `command-line-mode` — toggle the `:` line's
    /// **expanded** tier-2 mini-buffer band (full modal editing in place),
    /// or collapse it back to the one-row readline line for review.
    CommandLineToggleExpand,
    /// MB.5a: `<CR>` on the `/`·`?` search line — submit the search
    /// pattern. Resolved from `search-line-mode`'s Insert keymap.
    SearchLineSubmit,
    /// MB.5a: `<Esc>` / `<C-c>` on the `/`·`?` search line — cancel
    /// the search and restore the prior editing buffer.
    SearchLineCancel,
    /// MB.5b: `<C-p>` / `<Up>` on the `/`·`?` search line — walk to an
    /// older entry in `search_history`.
    SearchLineHistoryPrev,
    /// MB.5b: `<C-n>` / `<Down>` on the `/`·`?` search line — walk to a
    /// newer entry in `search_history`.
    SearchLineHistoryNext,
    /// MB.5c: `<C-x><C-e>` on the `/`·`?` search line — toggle the
    /// expanded tier-2 mini-buffer band.
    SearchLineToggleExpand,
    /// Lattice's `-`. Open / step up in the oil-style directory
    /// view (DESIGN.md §5.9.4). Promoted from
    /// `Action::OilNavigateUp` in slice 8.i.1.e.
    OilNavigateUp,
    /// Vim's `gv`. Reselect the last Visual selection (same
    /// kind, anchor, head). Promoted from
    /// `Action::ReselectLastVisual` in slice 8.i.1.e.
    ReselectLastVisual,
    /// Vim's `o` in Visual mode -- swap the cursor (head) to the
    /// other end of the selection (and back). Anchor and head trade
    /// places so subsequent motions / text objects grow or shrink the
    /// selection at the end the cursor now sits on.
    SwapVisualEnds,
    /// Vim's `p`. Paste the unnamed register's contents after
    /// the cursor. Promoted from `Action::PasteAfter` in slice
    /// 8.i.1.e.
    PasteAfter,
    /// Vim's `P`. Paste the unnamed register's contents before
    /// the cursor. Promoted from `Action::PasteBefore` in slice
    /// 8.i.1.e.
    PasteBefore,
    // L7: the 6 nav `AppEffect::Lsp*Request` variants (`gd` / `gD` / `gy`
    // / `gI` / `gr` / `gx`) removed — they are mode-owned now. `lsp-mode`'s
    // `action_handlers()` closures emit `Effect::Lsp(LspRequest::{Definition,
    // Declaration, TypeDefinition, Implementation, References, FollowLink})`,
    // dispatched host-side by `editor.lsp_request`.
    /// Vim's `a`. Move cursor one byte right (clamped) and enter
    /// Insert. Promoted from `Action::EnterAppend` in slice
    /// 8.i.1.g.
    EnterAppend,
    /// Vim's `I`: move cursor to first non-blank column of the
    /// current line and enter Insert.
    EnterInsertFirstNonBlank,
    /// Vim's `A`: move cursor to end of the current line and enter
    /// Insert.
    EnterAppendEndOfLine,
    /// Vim's `gj`: move down one display line (wrap segment).
    /// Degrades to `j` when wrapping is off.
    DisplayLineDown,
    /// Vim's `gk`: move up one display line (wrap segment).
    /// Degrades to `k` when wrapping is off.
    DisplayLineUp,
    /// Vim's `g0`: move to the first byte of the current display segment.
    /// Degrades to `0` when wrapping is off.
    DisplayLineStart,
    /// Vim's `g$`: move to the last byte of the current display segment.
    /// Degrades to `$` when wrapping is off.
    DisplayLineEnd,
    /// Vim's `zf`. Create a fold from the most recent Visual
    /// selection. Promoted from `Action::CreateFoldFromVisual`
    /// in slice 8.i.1.g.
    CreateFoldFromVisual,
    /// Insert mode's `<BS>`. Delete the byte before the cursor.
    /// Promoted from `Action::DeleteCharBackward` in slice
    /// 8.i.1.g.
    DeleteCharBackward,
    /// Insert-mode line editing — the readline/vim chords (`<C-a>`, `<C-e>`,
    /// `<C-w>`, `<C-u>`, `<C-k>`, `<C-t>`, `<C-d>`, …) available in every
    /// buffer. One grouped effect keyed by [`InsertLineEdit`] so the whole
    /// family shares a single host handler.
    InsertLineEdit(InsertLineEdit),
    /// Insert mode's `<C-Space>` and `<C-x><C-o>`. Trigger the
    /// completion popup (omni-completion alias). Promoted from
    /// `Action::CompletionTrigger` in slice 8.i.1.g. The
    /// completion-popup minor-mode layer's `<C-Space>` binding
    /// keeps its legacy `bind_action` registration until that
    /// helper picks up `CommandInvocation` (separate scope).
    CompletionTrigger,
    // SN.3c.1 (2026-06-14): `AppEffect::SnippetExpand` removed.
    // `<C-x><C-s>` is now mode-owned (`snippet-mode`'s `keymap()` +
    // `action_handlers()`): the handler scans the word prefix and
    // emits `Effect::ExpandSnippet { replace_range }`, which the host
    // resolves + expands. No host `Action` / `AppEffect` round-trip.
    /// Visual mode's `<Esc>` / `v` / `V`. Exit Visual to Normal,
    /// collapsing the selection. Promoted from `Action::ExitVisual`
    /// in slice 8.i.1.h.
    ExitVisual,
    /// Replace mode's `<BS>`. Undo the last overwritten char.
    /// Promoted from `Action::ReplaceUndoLast` in slice 8.i.1.h.
    ReplaceUndoLast,
    /// Vim's `i` / `R` / `<Esc>` (from Insert / Replace) -- enter
    /// the named modal state. Promoted from
    /// `Action::EnterMode(_)` in slice 8.i.2.a. Each chord in the
    /// keymap binds a *distinct* `CommandId` whose `ActionSpec`
    /// returns the right `AppEffect::EnterMode(state)` constant
    /// -- the `ModalState` rides in the AppEffect rather than in
    /// `CommandInvocation::args` so the App's `apply_app_effect`
    /// matches a single `EnterMode(state)` arm instead of N
    /// param-flat variants.
    EnterMode(ModalState),
    /// Vim's `v` / `V` / `<C-v>` -- enter Visual with the named
    /// kind anchored at the current cursor. Promoted from
    /// `Action::EnterVisual(_)` in slice 8.i.2.a. Same encoding
    /// as [`Self::EnterMode`]: distinct `CommandId` per kind,
    /// payload rides in the AppEffect.
    EnterVisual(VisualKind),
    /// Vim's `gh` / `gH` / `g<C-h>` -- enter Select mode (SN.3d) with
    /// the named kind, anchored at the current cursor. Same encoding as
    /// [`Self::EnterVisual`]: distinct `CommandId` per kind, payload
    /// rides in the AppEffect. The host handler (`do_enter_select`)
    /// anchors a zero-width selection like `do_enter_visual` does —
    /// typing then overtypes it. Programmatic entry (snippets) instead
    /// uses `EnterMode(Select(k))` with an explicit selection already
    /// set; see `docs/dev/architecture/select-mode.md` §3.
    EnterSelect(VisualKind),
    /// Vim's `/` / `?` -- enter the Search minibuffer in the
    /// named direction. Promoted from `Action::EnterSearch(_)`
    /// in slice 8.i.2.b. Same encoding as [`Self::EnterMode`]:
    /// distinct `CommandId` per direction.
    EnterSearch(SearchDirection),
    /// Vim's `*` / `#` -- search for the word under the cursor
    /// in the named direction. Promoted from
    /// `Action::SearchWordUnderCursor(_)` in slice 8.i.2.b.
    SearchWordUnderCursor(SearchDirection),
    /// Vim's `H` / `M` / `L` -- jump cursor to the named
    /// position within the visible viewport. Promoted from
    /// `Action::JumpViewport(_)` in slice 8.i.2.c.
    JumpViewport(ViewportPos),
    /// Vim's `zz` / `zt` / `zb` -- scroll the viewport so the
    /// cursor's current line sits at the named position.
    /// Promoted from `Action::ScrollCursorTo(_)` in slice 8.i.2.c.
    ScrollCursorTo(ScrollPos),
    /// HS.2: vim `z{l,h,L,H,s,e}` manual horizontal scroll.
    HorizontalScroll(HScroll),
    /// Vim's `J` (with-space) / `gJ` (no-space). Joins the
    /// current line with the next, replacing the joining
    /// newline with a single space (`with_space: true`) or
    /// nothing (`false`). Promoted from `Action::JoinLines` in
    /// slice 8.i.2.d. Bool payload rides in the AppEffect:
    /// distinct `CommandId` per binding (`J` -> with-space=true,
    /// `gJ` -> with-space=false).
    JoinLines { with_space: bool },
    /// Vim's `;` (forward) / `,` (reverse). Repeat the most
    /// recent `f` / `F` / `t` / `T` find on the current line
    /// in the originally-typed direction (`reverse: false`) or
    /// the opposite direction (`reverse: true`). Promoted from
    /// `Action::FindRepeat` in slice 8.i.2.d.
    FindRepeat { reverse: bool },
    /// Insert / Replace mode's `<CR>`. Inserts a literal newline
    /// at the cursor. Promoted from
    /// `Action::Insert("\n".into())` in slice 8.i.2.e. Distinct
    /// flat variant rather than a `String`-payload `Insert(_)`
    /// because the keymap-bound forms always pin a fixed
    /// literal; the wildcard "type any printable char" path
    /// stays on `Action::Insert(c.to_string())` since it isn't
    /// keymap-bound.
    InsertNewline,
    /// Insert mode's `<Tab>`. Inserts a literal tab at the
    /// cursor. Promoted from `Action::Insert("\t".into())` in
    /// slice 8.i.2.e.
    InsertTab,
    /// Replace mode's bare-printable wildcard. Overwrites the
    /// byte at the cursor with the captured char. Promoted from
    /// `Action::OverwriteChar(c)` in slice 8.i.3.
    OverwriteChar(char),
    /// Vim's `m<X>`. Sets mark `<X>` at the cursor's current
    /// position. Promoted from `Action::SetMark(c)` in slice
    /// 8.i.3. The bound `ActionSpec` validates that `<X>` is
    /// `[a-zA-Z0-9]` -- invalid chars dispatch to `Effect::None`
    /// (effectively a no-op; `App::apply` clears the pending
    /// state on every Invoke).
    SetMark(char),
    /// Vim's `'<X>`. Jumps the cursor to the line of mark `<X>`.
    /// Promoted from `Action::JumpToMarkLine(c)` in slice 8.i.3.
    JumpToMarkLine(char),
    /// Vim's `` `<X> ``. Jumps the cursor to the exact position
    /// (line + byte) of mark `<X>`. Promoted from
    /// `Action::JumpToMarkExact(c)` in slice 8.i.3.
    JumpToMarkExact(char),
    /// Vim's `"<X>`. Selects the named register for the next
    /// yank / paste / delete. Promoted from
    /// `Action::SelectRegister(_)` in slice 8.i.3. The bound
    /// `ActionSpec` validates the captured char via
    /// [`Register::from_input_char`]; chars that don't name a
    /// register dispatch to `Effect::None`.
    SelectRegister(Register),
    /// Vim's `q<X>`. Starts recording a macro into register
    /// `<X>`. Promoted from `Action::StartMacroRecord(c)` in
    /// slice 8.i.3.
    StartMacroRecord(char),
    /// Vim's `@<X>` for `<X>` alphanumeric. Plays the macro
    /// stored in register `<X>`. Promoted from
    /// `Action::PlayMacro(c)` in slice 8.i.3. The `@@` chord is
    /// dispatched to [`Self::PlayLastMacro`] from the same
    /// `play-macro` action (the spec branches on the captured
    /// char).
    PlayMacro(char),
    /// Vim's `@@`. Replays the most recently played macro.
    /// Promoted from `Action::PlayLastMacro` in slice 8.i.3.
    /// Shares its bind site (`@<CharLiteral>`) with
    /// [`Self::PlayMacro`]; the `play-macro` action's apply
    /// closure picks one or the other based on the captured
    /// char.
    PlayLastMacro,
    /// Slice 8.i.4.c: arm an operator-pending state via the
    /// `partial_chord` mechanism. The App handler does two
    /// things atomically:
    ///
    /// 1. Latch `pending_count` into `op_count` (vim's
    ///    `<count>op<motion>` count multiplication; without
    ///    this `2dd` would get a count of 1).
    /// 2. Push the operator's chord prefix into
    ///    `App::partial_chord` so the next keystroke routes
    ///    through `lookup_normal_with_prefix` and resolves
    ///    `[op, motion]` / `[op, i/a, text-object]` /
    ///    `[op, f/F/t/T, char]` to the bound `Invoke`.
    ///
    /// Replaces the `Action::SetPending(Pending::AfterOperator(_))`
    /// flow that did the same two things split across the
    /// keymap (which fired `SetPending`) and `App::apply`
    /// (which latched `op_count` inside the SetPending arm).
    /// The 8 operator prefixes -- `d`, `c`, `y`, `>`, `<`,
    /// `gU`, `gu`, `g~` -- bind to typed actions whose
    /// `ApplySpec` returns this variant.
    AbsorbOperatorPrefix(OperatorId),
    /// Vim's `<C-w>s`: split the active pane horizontally.
    /// Promoted from `Action::SplitPaneHorizontal` in slice 8.i.4.d.
    SplitPaneHorizontal,
    /// Vim's `<C-w>v`: split the active pane vertically.
    /// Promoted from `Action::SplitPaneVertical` in slice 8.i.4.d.
    SplitPaneVertical,
    /// Vim's `<C-w>c` / `<C-w>q`: close the active pane.
    /// Promoted from `Action::ClosePane` in slice 8.i.4.d.
    ClosePane,
    /// Vim's `<C-w>o` / `:only` / emacs `C-x 1`: close every pane
    /// except the active one (collapse the tree to the active leaf).
    /// No-op when only one pane is open. S3b (2026-06-22).
    OnlyPane,
    /// Vim's `<C-w>h/j/k/l` (and arrow / `<BS>` aliases): move
    /// focus to the pane in the named direction. Promoted from
    /// `Action::NavigatePane(_)` in slice 8.i.4.d.
    NavigatePane(PaneDirection),
    /// Vim's `<C-w>w` / `<C-w><Tab>`: cycle focus to the next
    /// pane. Promoted from `Action::NextPane` in slice 8.i.4.d.
    NextPane,
    /// Vim's `<C-w>W` / `<C-w><S-Tab>`: cycle focus to the
    /// previous pane. Promoted from `Action::PrevPane` in
    /// slice 8.i.4.d.
    PrevPane,
    /// Issue #29 (2026-05-22): vim's `gt` — next tab.
    NextTab,
    /// Vim's `gT` — previous tab.
    PrevTab,
    /// Vim's `{N}gt` — switch to tab N (1-indexed; clamped).
    GoToTab(u32),
    /// `:tabnew` — new empty tab.
    NewTab,
    /// `:tabnew <path>` — new tab opening `path`.
    NewTabAt(String),
    /// Issue #40 / Terminal-mode T1 (2026-05-22):
    /// `:terminal [cmd]` — spawn a PTY-backed shell.
    TerminalSpawn(Option<String>),
    /// T4 (2026-05-25): `:tabterminal [cmd]` — open a fresh
    /// tab and spawn a PTY-backed shell in it. Handler does
    /// `do_new_tab` then `do_terminal_spawn(cmd)`.
    TerminalSpawnInNewTab(Option<String>),
    /// T4 (2026-05-25): `<C-w>T` — move the active pane to a
    /// fresh tab. Handler in `Editor::do_move_pane_to_new_tab`.
    MovePaneToNewTab,
    /// `:tabclose` — close active tab.
    CloseTab,
    /// `:tabonly` — close every tab except the active one.
    OnlyTab,
    /// `:tabmove [N]` — move active tab to position N (1-indexed).
    MoveTab(u32),
    /// Issue #32 (2026-05-22): picker open-target overrides.
    /// `<C-s>` — accept selected candidate in horizontal split.
    PickerAcceptInSplit,
    /// `<C-v>` — accept selected candidate in vertical split.
    PickerAcceptInVSplit,
    /// `<C-t>` — accept selected candidate in new tab.
    PickerAcceptInTab,
    /// Issue #28 (2026-05-22): `<C-w>=` — reset every split's
    /// ratio to 0.5.
    EqualizePanes,
    /// `<C-w>+` — grow the active pane vertically.
    GrowPaneHeight,
    /// `<C-w>-` — shrink the active pane vertically.
    ShrinkPaneHeight,
    /// `<C-w>>` — grow the active pane horizontally.
    GrowPaneWidth,
    /// `<C-w><` — shrink the active pane horizontally.
    ShrinkPaneWidth,
    /// Completion-popup overlay: focus the next entry. Promoted
    /// from `Action::CompletionNext` in slice 8.i.4.e.
    CompletionNext,
    /// Completion-popup overlay: focus the previous entry.
    /// Promoted from `Action::CompletionPrev` in slice 8.i.4.e.
    CompletionPrev,
    /// Completion-popup overlay: accept the focused candidate.
    /// Promoted from `Action::CompletionAccept` in slice
    /// 8.i.4.e.
    CompletionAccept,
    /// Completion-popup overlay: cancel the popup, stay in
    /// Insert. Promoted from `Action::CompletionCancel` in
    /// slice 8.i.4.e.
    CompletionCancel,
    /// Completion-popup overlay: cancel the popup and exit
    /// Insert. Promoted from
    /// `Action::CompletionCancelAndExitInsert` in slice 8.i.4.e.
    CompletionCancelAndExitInsert,
    /// Completion-popup overlay: toggle the doc-popup
    /// (`<C-d>`). Promoted from `Action::CompletionToggleDocs`
    /// in slice 8.i.4.e.
    CompletionToggleDocs,
    /// Completion-popup overlay: scroll the doc-popup down
    /// (`<C-f>`). Promoted from
    /// `Action::CompletionDocsScrollDown` in slice 8.i.4.e.
    CompletionDocsScrollDown,
    /// Completion-popup overlay: scroll the doc-popup up
    /// (`<C-b>`). Promoted from
    /// `Action::CompletionDocsScrollUp` in slice 8.i.4.e.
    CompletionDocsScrollUp,
    /// Completion-popup overlay: bare-printable wildcard.
    /// Accept the focused candidate, then insert the captured
    /// char (so the user can finish typing through a
    /// confirmed prefix). Promoted from
    /// `Action::CompletionAcceptThenInsert(c)` in slice
    /// 8.i.4.e.
    CompletionAcceptThenInsert(char),
    /// Active-snippet overlay: jump to the next placeholder
    /// (`<Tab>`). Promoted from
    /// `Action::SnippetNextPlaceholder` in slice 8.i.4.e.
    SnippetNextPlaceholder,
    /// Active-snippet overlay: jump to the previous placeholder
    /// (`<S-Tab>`). Promoted from
    /// `Action::SnippetPrevPlaceholder` in slice 8.i.4.e.
    SnippetPrevPlaceholder,
    // SN.3c.2 (2026-06-14): `AppEffect::SnippetLeave` removed.
    // `<Esc>` is mode-owned now (`active-snippet-mode`'s
    // `keymap()` binds it + a per-buffer closure in `on_activate`
    // clears the session + returns `Effect::EnterMode(Normal)`);
    // no host `Action` / `AppEffect` round-trip. (Unlike the nav
    // placeholders, which keep their `register_simple`-produced
    // AppEffect variants as no-ops, leave switched to
    // `register_action`, so this variant had no producer left.)
    /// Completion-popup overlay: restrict candidates to a single
    /// source. The string is the `SourceId` (e.g.
    /// `"gen:buffer-words"`, `"gen:lsp-completion"`). Bound to
    /// the popup-mode filter chords (`<C-b>`, `<C-o>`, `<C-f>`,
    /// `<C-t>`, ...) introduced in CSM.K2.
    CompletionFilterToSource(String),
    /// Completion-popup overlay: clear the active source filter
    /// (`<C-Space>`). Restores the mixed merged candidate list.
    CompletionFilterClear,
    /// D.5.b (2026-05-30): diff-mode `do` (diff-get) operator.
    /// CR.1 (2026-06-24): `do` is now mode-owned
    /// (`DiffMode::action_handlers()` → `Effect::ApplyEdit`); this
    /// `AppEffect` is retained only as the FALLBACK the `action:diff-get`
    /// CommandSpec emits when no handler is registered. The host's
    /// `apply_app_effect` arm is emptied to a silent no-op (the
    /// `Action::DiffGet` variant it used to push is deleted).
    DiffGet,
    /// D.5.c (2026-05-30): diff-mode `dp` (diff-put) operator.
    /// CR.1: mirror of [`Self::DiffGet`] — mode-owned now; retained as
    /// the emptied `action:diff-put` fallback shell.
    DiffPut,
    /// T.3: tutor-mode `<CR>` / `:tutor-next`. Advance to the
    /// next exercise; advance to the next lesson when the
    /// current one is complete. No-op when tutor-mode is not
    /// active on the current buffer.
    TutorAdvance,
    /// T.3: tutor-mode `:tutor-prev`. Retreat to the previous
    /// exercise. No-op at exercise 0.
    TutorRetreat,
    /// M.5 (2026-06-01): `:multibuffer-expand [n]` /
    /// `:multibuffer-contract [n]` ex-commands. `delta` is the
    /// signed row count (positive expands, negative contracts).
    /// Routed to the active multibuffer view's `expand_excerpt_at`
    /// from the dispatch path. No-op when the active buffer
    /// isn't a multibuffer.
    MultibufferExpand { delta: i32 },
    /// N.1.1 (2026-06-10): `:narrow [{range}]` ex-command. The host
    /// arm resolves `range` against the active document to a line
    /// span, fetches the active buffer's `Arc<dyn Document>`, and
    /// calls `lattice_multibuffer::providers::narrow::create_narrow_view`
    /// — opening a one-excerpt multibuffer focused on that region.
    /// `range == None` (bare `:narrow`) narrows the current line in
    /// N.1.1 (N.1.2 widens this to the current paragraph / Visual
    /// selection).
    NarrowTrigger { range: Option<crate::range::Range> },
    /// N.1.1 (2026-06-10): `:widen` ex-command. The host arm closes
    /// the active narrow view (an editable one-excerpt multibuffer),
    /// restoring the full source buffer. No-op + echo when the
    /// active buffer isn't a narrow view.
    NarrowWiden,
    /// N.1.3 (2026-06-10): the `zn` narrow operator emits this once
    /// the operator-pending machinery has resolved its motion / text
    /// object to a line span. Carries pre-resolved inclusive 0-based
    /// `[start_line, end_line]` (unlike `NarrowTrigger`, which carries
    /// an unresolved `Range`); the host arm narrows the active buffer
    /// to that span via the same `create_narrow_view` sink.
    NarrowLines { start_line: u32, end_line: u32 },
    /// M.6 (2026-06-01): `:search <query>` ex-command. M.10.6
    /// (2026-06-03) inlined the work into the host's
    /// apply_effect arm — it calls
    /// `lattice_multibuffer::providers::search::project_search`
    /// against the active editor as the activator. No longer
    /// trampolines through `Action::SearchTrigger` /
    /// `Editor::do_search` (both deleted).
    SearchTrigger { query: String },
    /// M.6.1 (2026-06-01): `<CR>` chord in project-search-multibuffer-mode.
    /// Resolves the excerpt under cursor → source path → opens
    /// the file at the matched row. M.10.3 (2026-06-03) made
    /// this mode-owned: the search mode's `on_activate`
    /// M.6.1 (2026-06-01): `gr` chord in project-search-multibuffer-mode.
    /// Re-runs the scan with the view's current query. M.10.5
    /// (2026-06-03) made this mode-owned: the search mode's
    /// `on_activate` registers a closure that intercepts via
    /// `ActionHandlerRegistry` before this AppEffect arm runs.
    /// The arm is a no-op marker; the work happens in the
    /// mode's closure (reading state, clearing excerpts,
    /// spawning a fresh scan task).
    SearchRefresh,
    /// CM.1 (2026-07-21): `:compile <cmd>` / `:recompile` / `:make`.
    /// The host arm creates the read-only synthetic `*compilation*`
    /// buffer host-side (`Editor::ensure_named_synthetic_document`,
    /// activating `compilation-mode`) and kicks off the pipe-captured
    /// off-thread run via the `CompilationServiceHandle`, whose output
    /// streams into that buffer. Same
    /// shape as `SearchTrigger`: the substrate crate owns the work;
    /// the host arm is generic apply-effect routing. `cmdline`:
    /// `Some(cmd)` for `:compile`; `None` for `:recompile` / bare
    /// `:make` (reuse the last command).
    CompileRun { cmdline: Option<String> },
    /// CM.3b (2026-07-22): `<CR>` on a location line in the
    /// `*compilation*` buffer. The `compilation-mode` action handler
    /// parses the cursor line's text (`parse_location_line`) into a
    /// source location and emits this; the host arm calls
    /// `Editor::jump_to_file_line_col(&path, line, col)` (records the
    /// hop in position history) and syncs the error list index to the
    /// matching entry. `line` / `col` are 0-based. The location rides
    /// in the AppEffect because the jump target is computed off the
    /// buffer text in the mode's closure, but the error list index is
    /// core/host state — so the host owns the apply.
    CompileJumpToLocation {
        path: std::path::PathBuf,
        line: u32,
        col: u32,
    },
    /// CM.2 (2026-07-22): `:cnext`/`:cprev`/`:cc [N]`/`:cfirst`/
    /// `:clast` and the Builtin `]q`/`[q` chords. The host arm calls
    /// `Editor::do_error_nav`, which walks the core error list
    /// (recording each hop in position history via
    /// `jump_to_file_line_col`). On an empty list `Next`/`Prev` fall
    /// back to today's active-buffer diagnostic hopping; `Jump`/
    /// `First`/`Last` echo "no error list".
    ErrorNav { target: ErrorTarget },
    /// CM.3a (2026-07-22): parsed error entries from the compilation
    /// stderr reader — the off-thread → host-state seam. The reader
    /// accumulates entries and sends the FULL list through the
    /// compilation inbound bus; this handler maps each send here, and
    /// the host arm calls `Editor::set_error_list(entries)`
    /// (replace-semantics — the growing list stays visible). An empty
    /// vec (sent on a new run / `:recompile`) clears the stale list.
    /// The parser (below-host `lattice-compilation`) and this payload
    /// share the `lattice_protocol::error_list::ErrorEntry` type.
    SetErrorList {
        entries: Vec<lattice_protocol::error_list::ErrorEntry>,
    },
    /// CM.3c (2026-07-22): the per-buffer severity gutter index for the
    /// `*compilation*` buffer — the off-thread → host-state seam for
    /// in-buffer severity marks (twin of `SetErrorList`, which feeds the
    /// cross-file error list). The compilation drain scans each
    /// streamed line for a severity keyword, accumulates the FULL
    /// per-buffer index of `(line, severity)`, and sends it through the
    /// compilation inbound bus; this handler maps each send here, and the
    /// host arm converts the severities to `GutterSeverityLevel` and writes
    /// the `render_state` compilation-severity slot for `buffer`. The
    /// renderer reads that slot and injects it into the mode's
    /// `gutter_decorations` via `CompilationSeverityData`. An empty vec
    /// (sent on `Reset` / a new run) clears the buffer's marks.
    ///
    /// `buffer` is the raw `BufferId.0` (`BufferId` is process-local and
    /// not `Serialize`, so the wire form is the `u32`; the host arm
    /// reconstructs `BufferId`). Severity rides as the parser-native
    /// `ErrorSeverity` (already shared with `SetErrorList`) — the single
    /// map to the renderer-facing `GutterSeverityLevel` happens host-side,
    /// avoiding a lossy `GutterSeverityLevel`↔`ErrorSeverity` round-trip
    /// and keeping `lattice-grammar` free of a `lattice-mode` dependency.
    CompilationGutterSet {
        buffer: u32,
        entries: Vec<(u32, lattice_protocol::error_list::ErrorSeverity)>,
    },
    /// CM.3c (2026-07-22): per-buffer compilation location-line
    /// index for theme-based highlighting of navigable lines in
    /// the `*compilation*` buffer. Twin of `CompilationGutterSet`:
    /// the off-thread compilation drain scans each chunk for
    /// location-bearing lines (via `parse_location_line`) and
    /// ships the full list through an inbound bus; this effect
    /// writes the `render_state` compilation-location slot for
    /// `buffer`. An empty vec (sent on `Reset` / a new run)
    /// clears the buffer's location-line set.
    CompilationLocationLines {
        buffer: u32,
        /// (line, path_byte_start, path_byte_end) for each location line.
        /// byte_start/end are the byte offsets of the file-path portion
        /// within the line text, for link-like fg highlighting.
        lines: Vec<(u32, u32, u32)>,
    },
    /// CM.3d (2026-07-22): resolved compilation location theme colours
    /// — published by the mode during activation so the renderer
    /// reads `compilation.location` bg/fg from the theme rather than
    /// hardcoding RGB values.
    CompilationThemeColors {
        bg: u32,
        fg: u32,
    },
    /// CM.3d (2026-07-22): kill the running compilation child
    /// process. The host arm calls `CompilationService::kill()`.
    CompilationKill,
    /// CM.4 (2026-07-22): `:copen`. The host arm reads the core
    /// error list and calls
    /// `lattice_multibuffer::providers::problems::create_problems_view`,
    /// opening the `*problems*` multibuffer — the error entries
    /// grouped as editable source excerpts by file. Same shape as
    /// `SearchTrigger`: the substrate crate owns view-creation; the
    /// host arm is generic apply-effect routing. Echoes "no error
    /// list" when the list is empty.
    ProblemsOpen,
    /// CM.4 (2026-07-22): `:cclose`. The host arm closes the active
    /// `*problems*` view (an editable multibuffer with
    /// `ProblemsMinorMode`), leaving the source buffers open — the
    /// `NarrowWiden` close shape, guarded to problems views. No-op +
    /// echo when the active buffer isn't a problems view.
    ProblemsClose,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn quit_round_trips_through_serde() {
        let q = AppEffect::Quit;
        let s = serde_json::to_string(&q).unwrap();
        let back: AppEffect = serde_json::from_str(&s).unwrap();
        assert_eq!(q, back);
    }
}
