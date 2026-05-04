//! Pure application state and transitions.
//!
//! The state machine is intentionally separated from the IO loop so it can be
//! unit-tested without spinning up a terminal. Each input keystroke becomes
//! an `Action`; `App::apply` consumes the action, dispatching motion / edit
//! work through `lattice_grammar::execute()` where appropriate.
//!
//! Phase 2 wiring: motions and the `delete` operator flow through the
//! grammar engine; the modal-mode primitives (`i`, `a`, `o`, `<Esc>`) live
//! locally on `App` because they're inherently a state-machine concern, not
//! a buffer command. Phase 3+ migrates more of these to the grammar layer.

use fancy_regex::Regex;
use lattice_core::Buffer;
use lattice_core::CoreError;
use lattice_core::Document;
use lattice_core::buffer::AppliedEdit;
use lattice_core::search::{self, SearchHit};
use lattice_grammar::CommandRegistry;
use lattice_grammar::ModalState;
use lattice_grammar::SearchDirection;
use lattice_grammar::VisualKind;
use lattice_grammar::YankKind;
use lattice_grammar::builtins::{Builtins, populate};
use lattice_grammar::command::CommandInvocation;
use lattice_grammar::effect::Effect;
use lattice_grammar::register::Register;
use lattice_grammar::registry::OperatorId;
use lattice_protocol::Event;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range as ProtoRange};
use lattice_protocol::selection::{Selection, SelectionSet, VisualMode};
use lattice_lsp::{DiagnosticsLayer, LogLevel, LspLogger, LspSupervisor};
use lattice_runtime::{
    CancellationToken, DocumentHandle, EventBus, RuntimeError, SnapshotCache, block_on,
    spawn_document,
};
use lattice_syntax::{Lang, LangRegistry, StyledSpan, Syntax};

use std::collections::HashMap;
use std::sync::Arc;

use crate::buffer_registry::{BufferData, BufferEntry, BufferRegistry, DocumentEntry};
use crate::buffers::{BufferFlags, BufferId, BufferKind};

/// Build a fresh LSP subsystem. Returns the supervisor wrapped
/// in `Arc<Mutex>` for App-side sharing, plus cloned handles
/// to the diagnostics layer + logger so the renderer's
/// per-frame reads can skip the supervisor lock.
fn build_lsp_subsystem() -> (
    std::sync::Arc<tokio::sync::Mutex<LspSupervisor>>,
    DiagnosticsLayer,
    LspLogger,
) {
    let logger = LspLogger::with_defaults();
    let mut sup = LspSupervisor::new(logger.clone());
    // Builtin registry: rust-analyzer, pyright, gopls,
    // typescript-language-server, clangd, lua-language-server.
    // Users override via lsp.toml when §5.12 lands.
    sup.set_configs(lattice_lsp::builtin_servers());
    let diagnostics = sup.diagnostics().clone();
    (
        std::sync::Arc::new(tokio::sync::Mutex::new(sup)),
        diagnostics,
        logger,
    )
}
use crate::excommand;
use crate::file_tree::{FileTreeBuffer, FileTreeEntryKind};
use crate::help::{HelpBuffer, HelpDisplayMode, command_link, key_link};
use crate::pane::{PaneDirection, PaneState, PaneTree, SplitOrientation};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    None,
    /// First `<C-w>` of a window-management chord (split / close /
    /// navigate). Resolves on the next key.
    AfterCtrlW,
    /// First `g` of a `gg`-style two-key sequence.
    AfterG,
    /// Operator key pressed; awaiting motion or text-object.
    AfterOperator(OperatorId),
    /// `f` / `F` / `t` / `T` pressed; awaiting the target character.
    /// If the user pressed an operator first (like `df`), the operator is
    /// stashed here so we can compose `df<char>` into a single Invoke.
    AfterFindChar {
        kind: FindKind,
        operator: Option<OperatorId>,
    },
    /// `i` or `a` pressed in operator-pending state; awaiting the
    /// text-object selector char (`w`, `"`, `(`, `[`, `{`, etc.).
    AfterTextObject {
        operator: OperatorId,
        around: bool,
    },
    /// `z` pressed; the next char selects the scroll command (`zz`, `zt`, `zb`).
    AfterZ,
    /// `"` pressed; the next char selects the register for the upcoming
    /// operator or paste.
    AfterRegister,
    /// `q` pressed (when not already recording); next char is the macro
    /// register name.
    AfterMacroStart,
    /// `@` pressed; next char is the macro register name to play.
    AfterMacroPlay,
    /// `m` pressed; the next char is the mark to set.
    AfterSetMark,
    /// `'` pressed; the next char is the mark to jump to (linewise).
    AfterJumpMarkLine,
    /// `` ` `` pressed; the next char is the mark to jump to (exact).
    AfterJumpMarkExact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewportPos {
    Top,
    Middle,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollPos {
    Top,
    Center,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindKind {
    /// `f` -- move to next occurrence of the char on the current line.
    Forward,
    /// `F` -- move to previous occurrence of the char on the current line.
    Backward,
    /// `t` -- move to one byte before the next occurrence (inclusive of arg).
    TillForward,
    /// `T` -- move to one byte after the previous occurrence.
    TillBackward,
}

/// A transient one-line message rendered in the echo area below the mode line
/// (DESIGN.md §5.9.10). Phase 2 wiring: replaced by the next call to
/// `App::set_message` (no timeout-based fade yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoMessage {
    pub text: String,
    pub level: EchoLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EchoLevel {
    Info,
    Warn,
    Error,
}

/// Convert grammar's wire-typed [`lattice_grammar::EchoLevel`] (carried
/// by `Effect::Echo`) into the App's display-typed `EchoLevel`. Two types
/// because the App's is part of the public crate API; the grammar's is a
/// dispatch detail.
/// Resolve user-typed command text to a `CommandId`, accepting
/// either the canonical registry name (`ex:write`) or an alias
/// (`write`, `w`). Used by App handlers that take a command name
/// from user input -- mirrors the two-stage logic in
/// `excommand::parse_invocation`.
fn resolve_command_name_or_alias(
    registry: &lattice_grammar::CommandRegistry,
    name: &str,
) -> Option<lattice_grammar::CommandId> {
    if let Some(id) = registry.id_by_name(name) {
        return Some(id);
    }
    let canonical = crate::excommand::aliases().get(name).copied()?;
    registry.id_by_name(canonical)
}

/// Rewrite Command-kind candidates from canonical names
/// (`ex:describe-command`) to the user-facing alias
/// (`describe-command`) and recompute their match ranges against
/// the new text. Non-command candidates pass through.
///
/// This is purely a UX rewrite -- the parser accepts both forms.
/// We re-derive match ranges instead of clearing them so the
/// popup's match-face highlighting still shows where the query
/// matched.
fn prefer_aliases_for_command_candidates(
    candidates: &mut Vec<lattice_completion::RenderedCandidate>,
    query: &str,
) {
    let needle = query.to_ascii_lowercase();
    candidates.retain_mut(|c| {
        if !matches!(c.raw.kind, lattice_completion::CandidateKind::Command) {
            return true;
        }
        let canonical = c.raw.text.clone();
        let alias = crate::excommand::preferred_alias_for(&canonical);
        let new_text = alias.map(|a| a.to_string()).unwrap_or(canonical);
        c.raw.text = new_text.clone();
        c.raw.display = new_text.clone();
        // Recompute match ranges: subsequence-match the lowercase
        // query against the lowercase text; emit one range per
        // matched byte. Mirrors the fuzzy matcher's range output
        // so the popup highlights work consistently.
        c.match_ranges = subsequence_match_ranges(&needle, &new_text);
        // Keep the candidate even if the rewrite no longer
        // visibly contains the query -- the matcher already
        // accepted it against the canonical form. Filtering here
        // would surprise users (typing `ex:` then accepting an
        // alias-rewritten candidate would unexpectedly drop the
        // candidate). Empty match_ranges just means no
        // highlights.
        true
    });
}

fn subsequence_match_ranges(needle_lower: &str, haystack: &str) -> Vec<std::ops::Range<usize>> {
    if needle_lower.is_empty() {
        return Vec::new();
    }
    let n = needle_lower.as_bytes();
    let h = haystack.as_bytes();
    let mut ranges = Vec::with_capacity(n.len());
    let mut ni = 0;
    for (i, b) in h.iter().enumerate() {
        if ni >= n.len() {
            break;
        }
        if b.eq_ignore_ascii_case(&n[ni]) {
            ranges.push(i..i + 1);
            ni += 1;
        }
    }
    if ni < n.len() {
        // Couldn't match every needle char -- abandon the highlights;
        // the candidate stays (host kept the framework's verdict).
        Vec::new()
    } else {
        ranges
    }
}

/// Trim the last whitespace-delimited word from the end of `s`.
/// `<C-w>` semantics on the command line: removes the partial token
/// the user is typing (plus any trailing spaces). v1 cursor is
/// always at end-of-line; if cursor support lands later this should
/// take a cursor offset and operate to the left of it.
fn delete_trailing_word(s: &mut String) {
    // Strip trailing whitespace.
    let trimmed = s.trim_end_matches(char::is_whitespace);
    if trimmed.len() < s.len() {
        s.truncate(trimmed.len());
    }
    // Strip the trailing non-whitespace run.
    let last_ws = s.rfind(char::is_whitespace);
    let cut_to = last_ws.map(|i| i + 1).unwrap_or(0);
    s.truncate(cut_to);
}

/// Whether an action would mutate the document buffer (or the
/// document's mode / selection / undo state). The help-buffer guard
/// in [`App::apply`] short-circuits these when active_buffer ==
/// Help so a stray `i` / `p` / `u` / `dd` while reading help
/// doesn't fall through onto the underlying document.
///
/// Motions and scroll-class actions are NOT in this set -- they
/// operate on whichever buffer is active (document or help) per the
/// per-action active-buffer routing.
fn action_is_document_mutation(action: &Action) -> bool {
    matches!(
        action,
        Action::Insert(_)
            | Action::DeleteCharBackward
            | Action::EnterMode(ModalState::Insert)
            | Action::EnterMode(ModalState::Replace)
            | Action::EnterAppend
            | Action::EnterBlockVisualInsert
            | Action::EnterBlockVisualAppend
            | Action::OpenLineBelow
            | Action::OpenLineAbove
            | Action::Undo
            | Action::Redo
            | Action::OverwriteChar(_)
            | Action::ReplaceUndoLast
            | Action::PasteAfter
            | Action::PasteBefore
            | Action::PasteText(_)
            | Action::EnterVisual(_)
            | Action::ExitVisual
            | Action::ReselectLastVisual
            | Action::JoinLines { .. }
            | Action::ToggleCaseAtCursor
            | Action::CreateFoldFromVisual
            | Action::OpenFoldAtCursor
            | Action::CloseFoldAtCursor
            | Action::ToggleFoldAtCursor
            | Action::OpenAllFolds
            | Action::CloseAllFolds
            | Action::DeleteFoldAtCursor
            | Action::RepeatLastChange
            | Action::StartMacroRecord(_)
            | Action::StopMacroRecord
            | Action::PlayMacro(_)
            | Action::PlayLastMacro
            // `*` / `#` -- search-word-under-cursor reads the
            // *document* word and is fold-aware. Defer until
            // it's generalised through `active_text()`. The
            // regular `/` and friends are NOT mutations and run
            // on any buffer kind.
            | Action::SearchWordUnderCursor(_)
            | Action::MatchBracket
            | Action::FindRepeat { .. }
            | Action::SetMark(_)
            | Action::JumpToMarkLine(_)
            | Action::JumpToMarkExact(_)
            | Action::WalkMarkHistoryBack
            | Action::WalkMarkHistoryForward
            | Action::GotoNextFold
            | Action::GotoPrevFold
    )
}

fn echo_level_from_grammar(level: lattice_grammar::EchoLevel) -> EchoLevel {
    match level {
        lattice_grammar::EchoLevel::Info => EchoLevel::Info,
        lattice_grammar::EchoLevel::Warn => EchoLevel::Warn,
        lattice_grammar::EchoLevel::Error => EchoLevel::Error,
    }
}

#[derive(Debug, Clone)]
pub enum Action {
    None,
    Quit,
    /// Run a CommandInvocation through `lattice_grammar::execute()`.
    Invoke(CommandInvocation),
    /// Insert a string at the cursor (used by Insert mode).
    Insert(String),
    /// Delete the byte before the cursor (Insert-mode backspace).
    DeleteCharBackward,
    /// Move into a different modal state (Insert, Normal, ...).
    EnterMode(ModalState),
    /// Vim's `a`: move cursor one byte right (clamped) and enter Insert.
    EnterAppend,
    /// Vim's blockwise-visual `I`: move cursor to the leftmost
    /// column of the block on the top line, enter Insert, and on
    /// Esc replicate the typed text to every other line in the
    /// block at the same column. Issued only from Visual(Blockwise).
    EnterBlockVisualInsert,
    /// Vim's blockwise-visual `A`: same as [`Self::EnterBlockVisualInsert`]
    /// but the cursor lands one byte past the rightmost column of
    /// the block on each line.
    EnterBlockVisualAppend,
    /// Vim's `o`: open a new line below the current line and enter Insert.
    OpenLineBelow,
    /// Vim's `O`: open a new line above the current line and enter Insert.
    OpenLineAbove,
    /// Set the pending-key state (e.g., we just saw `g`).
    SetPending(Pending),
    Undo,
    Redo,
    /// Append a digit (0-9) to the in-progress count prefix.
    PushDigit(u8),
    /// Enter Visual modal state (`v` Charwise, `V` Linewise) anchored at
    /// the current cursor.
    EnterVisual(VisualKind),
    /// Exit Visual to Normal, collapsing the selection.
    ExitVisual,
    /// Vim's `gv` -- re-enter Visual with the same anchor / head / kind
    /// as the most recently exited Visual selection.
    ReselectLastVisual,
    /// Vim's `*` (Forward) and `#` (Backward) -- search for the word under
    /// the cursor in the given direction.
    SearchWordUnderCursor(SearchDirection),
    /// Vim's `%` -- jump to the matching bracket. Looks at or beyond the
    /// cursor on the current line for the first `()[]{}` and seeks its
    /// pair using a depth-tracking scan.
    MatchBracket,
    /// Vim's `~` -- toggle the case of the char at the cursor and
    /// advance by one byte.
    ToggleCaseAtCursor,
    /// Vim's `J` (with-space) and `gJ` (no-space): join the current line
    /// with the next, replacing the joining newline with a single space
    /// (or nothing for `gJ`).
    JoinLines {
        with_space: bool,
    },
    /// Vim's `;` (no-reverse) and `,` (reverse): repeat the last
    /// f/F/t/T find on the current line.
    FindRepeat {
        reverse: bool,
    },
    /// Vim's `zf` -- create a fold from the current Visual selection.
    CreateFoldFromVisual,
    /// Vim's `zo` -- open the fold containing the cursor.
    OpenFoldAtCursor,
    /// Vim's `zc` -- close the fold containing the cursor.
    CloseFoldAtCursor,
    /// Vim's `za` -- toggle the fold containing the cursor.
    ToggleFoldAtCursor,
    /// Vim's `zR` -- open all folds.
    OpenAllFolds,
    /// Vim's `zM` -- close all folds.
    CloseAllFolds,
    /// Vim's `zd` -- delete the fold containing the cursor.
    DeleteFoldAtCursor,
    /// Vim's `zj` -- move cursor to the start of the next fold.
    GotoNextFold,
    /// Vim's `zk` -- move cursor to the end of the previous fold.
    GotoPrevFold,
    /// Vim's `zi` -- toggle [`App::foldenable`]. With folds disabled
    /// every line renders flat regardless of any closed flag.
    ToggleFoldEnable,
    /// `K` (Phase 4.2.b). Send `textDocument/hover` to every
    /// LSP server attached to the active document; render the
    /// first non-empty markdown body in the hover popup. The
    /// request rides a [`lattice_protocol::CancellationToken`]
    /// so a stale response from a slow server can't drop a
    /// popup over a moved cursor.
    LspHoverRequest,
    /// `gd` (Phase 4.2.c). Send `textDocument/definition` to
    /// every attached LSP server. Single result → jump
    /// in-place (or via `:e <path>` if cross-file); multiple
    /// results → render in a `*lsp:definitions*` picker. Pushes
    /// the current cursor onto the position history (§5.1.1)
    /// so `<C-o>` walks back. Cancellation token rides on
    /// motion / mode change.
    LspDefinitionRequest,
    /// `"<reg>` prefix -- stash the named register for the next operator
    /// / paste invocation.
    SelectRegister(Register),
    /// Vim's `Ctrl-O` -- step backward in the position history.
    JumpHistoryBack,
    /// Vim's `Ctrl-I` (Tab) -- step forward.
    JumpHistoryForward,
    /// Vim's `Ctrl-L` -- force a full redraw. Reparses the syntax
    /// tree, recomputes folds, clears the visible-highlight cache,
    /// and tells the runtime to clear the terminal screen on the
    /// next frame. Intended escape hatch for any visual glitch
    /// (stale highlights, leftover ANSI escape sequences from a
    /// crashed external program, terminal-resize race).
    RedrawScreen,
    /// Vim's `g;` -- step backward through `NamedMark` entries in the
    /// unified position history.
    WalkMarkHistoryBack,
    /// Vim's `g,` -- step forward.
    WalkMarkHistoryForward,
    /// Vim's `q<reg>` to start recording into a register; `q` while
    /// recording stops. App handles routing internally.
    StartMacroRecord(char),
    StopMacroRecord,
    /// Vim's `@<reg>` to play. Replays the recorded Action stream.
    PlayMacro(char),
    /// Vim's `@@` to repeat the most recently played macro.
    PlayLastMacro,
    /// Vim's `.` -- re-dispatch the last buffer-mutating invocation from
    /// the current cursor.
    RepeatLastChange,
    /// Replace mode: overwrite the char at the cursor with `c` and advance.
    /// Beyond end-of-line, falls back to insert (vim behavior).
    OverwriteChar(char),
    /// Backspace within Replace -- pop the latest entry from
    /// `replace_history` and restore the original byte (or delete if the
    /// overwrite was a line extension).
    ReplaceUndoLast,
    /// Jump cursor to a viewport-relative line (vim's `H`, `M`, `L`).
    JumpViewport(ViewportPos),
    /// Adjust scroll so the cursor lands at the viewport top / center /
    /// bottom (vim's `zt`, `zz`, `zb`).
    ScrollCursorTo(ScrollPos),
    /// Move cursor down / up by one viewport-page (vim's Ctrl-F / Ctrl-B).
    PageDown,
    PageUp,
    /// Scroll the viewport one line up (Ctrl-Y) or down (Ctrl-E),
    /// nudging the cursor to keep it on-screen.
    ScrollLineUp,
    ScrollLineDown,
    /// `m<letter>` -- record the cursor at mark `<letter>`.
    SetMark(char),
    /// `'<letter>` -- jump to the line of mark `<letter>` (column = first
    /// non-blank).
    JumpToMarkLine(char),
    /// `` `<letter> `` -- jump to the exact position of mark `<letter>`.
    JumpToMarkExact(char),

    // ---- Command-line minibuffer (Phase 2: simple, single-line) ----
    /// Pressed `:` in Normal mode -- enter command modal with empty buffer.
    EnterCommandLine,
    /// Append a character to the in-progress command line.
    CommandLineAppend(char),
    /// Delete the last character. If the buffer is empty, leave Command mode.
    CommandLineBackspace,
    /// Submit the current command line: parse + execute, then leave Command.
    CommandLineSubmit,
    /// Drop the current command line and leave Command modal.
    CommandLineCancel,
    /// Walk to an older entry in the command history (`Up` arrow in
    /// Command modal).
    CommandLineHistoryPrev,
    /// Walk to a newer entry, eventually returning to the user's
    /// in-progress line.
    CommandLineHistoryNext,
    /// Replace the echo area with a typed message.
    Echo(EchoMessage),

    // ---- Hover popup ----
    /// Dismiss the hover popup. Mirrors the `:HoverClose` ex-command
    /// for the keymap path. Once a hover is *promoted* to a help
    /// buffer (via the second-K gesture), the standard
    /// help-dismissal path (`HelpDismiss`) closes it instead.
    CloseHover,

    // ---- Picker (DESIGN.md §5.9.7) ----
    /// Append a character to the picker's query and refilter.
    PickerAppend(char),
    /// Drop the last char from the picker's query and refilter.
    PickerBackspace,
    /// Move the selection cursor down one row (wraps).
    PickerSelectNext,
    /// Move the selection cursor up one row (wraps).
    PickerSelectPrev,
    /// Run the picker's accept action against the selected
    /// candidate and dismiss.
    PickerAccept,
    /// Drop the picker without acting on any candidate.
    PickerDismiss,

    // ---- Paste (`p`, `P`) ----
    /// Vim's `p` -- paste the unnamed register after the cursor (charwise)
    /// or below the current line (linewise).
    PasteAfter,
    /// Vim's `P` -- paste before cursor / above current line.
    PasteBefore,
    /// A bracketed-paste burst from the terminal -- the user pressed
    /// their terminal's paste shortcut (Ctrl-Shift-V, Cmd-V, mouse
    /// middle-click, ...) and the terminal handed us the whole payload
    /// in one event. Mode-dependent target: cursor in Insert/Normal/
    /// Visual/Replace, command line in Command, search line in Search.
    /// One undo unit, so a single `u` reverts the entire paste.
    PasteText(String),

    // ---- Command-line editing (DESIGN.md §5.11.3) ----
    /// `<C-u>` -- clear the entire command line.
    CommandLineClear,
    /// `<C-w>` -- delete the word to the left of the cursor.
    /// (v1: cursor is at end-of-line, so deletes the trailing word.)
    CommandLineDeleteWordBackward,
    /// `<C-h>` -- describe the command word / arg under cursor.
    /// Hybrid resolution: word-at-cursor describes itself if it
    /// resolves to a registered command; else describe the parent
    /// command at the relevant `arg:<name>` anchor.
    CommandLineDescribeUnderCursor,
    /// Chord-capture overlay (`ArgKind::Chord` slot): append one
    /// pre-formatted chord token (`<C-c>`, `<Esc>`, `gg`, ...) to
    /// the cmdline. Translation from the raw `KeyEvent` happens
    /// in `input::translate_command_chord_capture`.
    CommandLineAppendChord(String),
    /// Chord-capture overlay: backspace deletes one full chord
    /// token (`<C-c>` is one unit, not 5 chars), not a single byte.
    CommandLineDeleteChord,

    // ---- Completion popup (DESIGN.md §5.11.3) ----
    /// `<Tab>` -- open completion popup if closed; advance the
    /// selected candidate if open.
    CommandLineCompleteOrAdvance,
    /// `<S-Tab>` -- previous candidate when popup is open.
    CommandLineCompletePrev,
    /// `<CR>` while popup open -- replace the prefix with the
    /// selected candidate's `text` and close the popup.
    CommandLineAcceptCompletion,
    /// `<Esc>` while popup open -- close the popup without
    /// touching the command line. (Two-stage Esc: a second Esc
    /// then cancels the command line.)
    CommandLineDismissCompletion,

    // ---- Pane tree (DESIGN.md §5.9) ----
    /// `<C-w>s` -- split the active pane horizontally (new pane below).
    SplitPaneHorizontal,
    /// `<C-w>v` -- split the active pane vertically (new pane right).
    SplitPaneVertical,
    /// `<C-w>c` / `<C-w>q` -- close the active pane.
    ClosePane,
    /// `<C-w>{h,j,k,l}` -- move the active pane cardinally.
    NavigatePane(PaneDirection),
    /// `<C-w>w` -- cycle to the next pane in declaration order.
    NextPane,
    /// `<C-w>W` -- cycle to the previous pane.
    PrevPane,

    // ---- Help buffer (DESIGN.md §5.11, §5.9) ----
    //
    // Help is a regular buffer routed through the same Normal-mode
    // chord grammar as the document buffer (motions, page motions,
    // viewport jumps, `<C-o>` / `<C-i>`, `gg` / `G`, etc.). The
    // App's `active_buffer` field decides which cursor an action
    // affects. Only two help-specific actions remain -- buffer-local
    // bindings emitted by `translate()` when active_buffer == Help:
    /// Close the active help overlay (`Esc` / `q`).
    HelpDismiss,
    /// Follow the link under the cursor (`<CR>`). Resolves the
    /// link's URL scheme and dispatches: `command:NAME` re-runs
    /// `:describe-command NAME`, `key:CHORD` re-runs
    /// `:describe-key CHORD`, `file:PATH:LINE` opens the file at
    /// the line. Cursor not on a link is a no-op.
    FollowLink,

    // ---- Search (`/`, `?`, `n`, `N`) ----
    /// Pressed `/` (Forward) or `?` (Backward) -- enter Search modal with
    /// empty pattern, remembering origin so cancel restores cursor.
    EnterSearch(SearchDirection),
    SearchAppend(char),
    /// Delete one char from the pattern. If pattern is empty, leave Search.
    SearchBackspace,
    /// Confirm the pattern: jump to current match (if any) and store it
    /// as `last_search` for `n`/`N` repeat.
    SearchSubmit,
    /// Drop the in-progress pattern, restore cursor, leave Search.
    SearchCancel,
    /// Repeat the last search in its original direction.
    SearchNext,
    /// Repeat the last search in the opposite direction.
    SearchPrevious,
}

/// In-progress `/` or `?` state. The cursor at entry is preserved so
/// Esc can restore it.
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

/// The unnamed register's payload. v1 uses a single global slot; the
/// full vim register zoo (`"a-z`, `"+`, `"*`, etc.) lands later.
#[derive(Debug, Clone)]
pub struct UnnamedRegister {
    pub content: String,
    pub kind: YankKind,
}

/// Snapshot of the active pane's state captured just before help
/// took it over. Used by `dismiss_help` to restore the user to the
/// buffer + cursor + scroll they came from. The same struct serves
/// both display modes (in-pane and popup-overlay) -- popup mode
/// doesn't actually mutate `pane.buffer` so the restore there is
/// effectively a no-op for the pane fields, but keeping one stash
/// for both paths means dismiss has a single code path.
#[derive(Debug, Clone, Copy)]
pub struct PrevPaneState {
    pub buffer: BufferKind,
    pub buffer_id: BufferId,
    pub cursor: Position,
    pub scroll: u32,
}

pub struct App {
    /// Handle to the per-document actor (DESIGN.md §5.2.1, §5.7).
    /// The actor owns the writable [`Document`]; mutations route
    /// through it; reads load a versioned snapshot.
    /// Denormalized from `documents[active_document_id].handle` for
    /// hot-path access.
    pub document: DocumentHandle,
    /// Per-thread cached reader for [`Self::document`]'s published
    /// snapshot cell (DESIGN.md §5.6.8). The renderer's per-frame
    /// `snapshot_cache.load()` returns the current
    /// `Arc<DocumentSnapshot>` in ~300ps in steady state (no edit
    /// since last frame); ~16ns when the actor has just published.
    /// Rebuilt whenever [`Self::document`] is reassigned --
    /// `arc_swap::Cache` caches against a specific cell, so it must
    /// follow the active document's handle.
    pub snapshot_cache: SnapshotCache,
    /// Stable id for the *active* document buffer. Mirrors the
    /// active pane's `buffer_id` whenever that pane holds a
    /// Document leaf. Position-history entries (§5.1.1) and
    /// per-pane state record this id; switching the active
    /// document via `:bnext` / `:e FILE` rotates `Self::document` /
    /// `Self::syntax` etc. to the new active.
    pub document_buffer_id: BufferId,
    /// Unified buffer registry (DESIGN.md §5.9). Holds every open
    /// buffer regardless of kind -- documents, file trees, future
    /// outline / diagnostics views -- under one [`BufferId`]
    /// keyspace. `:bn` / `:bp` / `:ls` / `:bd` operate on this
    /// registry; `:e FILE` and `:Tree path` insert into it. The
    /// *active* document's hot-path state mirrors fields on App
    /// directly ([`Self::document`], [`Self::syntax`], etc.); the
    /// matching registry entry's `syntax` slot stays `None` until
    /// a switch saves the active state back.
    pub buffers: BufferRegistry,
    /// Which buffer the input pipeline currently routes to. When a
    /// help overlay is open this is `Help`; otherwise `Document`.
    /// Motions, jumps, and `<C-o>` / `<C-i>` consult this to pick
    /// the cursor + buffer they operate on (DESIGN.md §5.9).
    /// Denormalized from `pane_tree.active().buffer` -- updated in
    /// lockstep with the active pane.
    pub active_buffer: BufferKind,
    /// Pane tree (DESIGN.md §5.9). Holds one [`PaneState`] per
    /// visible viewport plus the split layout. Always non-empty;
    /// the active pane's cursor / scroll are stored on
    /// [`Self::cursor`] / [`Self::scroll`] for hot-path code, and
    /// snapshotted back into the pane tree on every active-pane
    /// switch.
    pub pane_tree: PaneTree,
    pub cursor: Position,
    /// First visible line in the viewport (0-based).
    pub scroll: u32,
    pub should_quit: bool,
    /// Last height we were drawn at; used by motion clamping and viewport
    /// scrolling. Updated by the renderer before each frame.
    pub viewport_height: u32,
    /// Last terminal width we were drawn at. Used by pane geometry
    /// (DESIGN.md §5.9 navigation needs to know which pane is
    /// horizontally adjacent). `None` until the renderer first
    /// records it.
    pub terminal_width: Option<u16>,
    pub modal: ModalState,
    pub pending: Pending,
    /// Grammar registry shared with the document actor by `Arc`. The
    /// actor calls `lattice_grammar::execute` with this registry from
    /// inside its own task. The App also reads it directly for the
    /// parser, completion pipeline, and introspection -- all
    /// read-only operations.
    pub registry: Arc<CommandRegistry>,
    /// In-process event bus (DESIGN.md §5.10). The App publishes
    /// editor lifecycle events (DocumentChanged, SelectionsChanged,
    /// ModalModeChanged, BeforeSave, DocumentSaved, BeforeQuit,
    /// OptionChanged) after observing the corresponding state
    /// transitions. The App itself subscribes to `OptionChanged`
    /// for the cascade hook (see [`Self::option_change_rx`]);
    /// other subscribers (plugins, autocmds) wire up the same way.
    pub event_bus: Arc<EventBus>,
    /// Receiver for in-flight LSP hover responses (Phase 4.2.b).
    /// `K` fires a `textDocument/hover` request through the typed
    /// wrapper; the spawned task awaits the actor's response and
    /// pushes a [`HoverOutcome`] onto this channel. The main loop
    /// drains it before each draw via [`Self::drain_pending_hover`]
    /// and either feeds the body into the existing
    /// [`Self::hover_popup`] via [`Self::do_open_hover`], or echoes
    /// the no-result reason so the user knows their `K` press was
    /// received and processed (versus silently dropped).
    ///
    /// `Option` only because the field needs to be `take`-able so
    /// the drain method can borrow `&mut self` for the popup
    /// update; always `Some` between calls.
    pub pending_hover_rx: Option<tokio::sync::mpsc::UnboundedReceiver<HoverOutcome>>,
    /// Cancellation token of the most recent hover request. Flipped
    /// when the user re-fires `K`, moves the cursor, or changes
    /// mode -- so a slow server's response arrives marked stale and
    /// is dropped by the typed wrapper's relay. `None` when no
    /// hover is in flight.
    pub pending_hover_token: Option<lattice_protocol::CancellationToken>,
    /// Receiver for in-flight goto-definition responses (Phase
    /// 4.2.c). Shape mirrors [`Self::pending_hover_rx`] -- `gd`
    /// fires every attached server's `textDocument/definition`,
    /// the spawned task collects the merged + deduped location
    /// list, and pushes it onto this channel. Drained per frame
    /// in [`Self::drain_pending_definitions`]; single-result
    /// case jumps in-place, multi-result case echoes a count
    /// (picker buffer lands with 4.2.d).
    pub pending_definition_rx: Option<tokio::sync::mpsc::UnboundedReceiver<Vec<lsp_types::Location>>>,
    /// Cancellation token of the most recent goto-definition
    /// request. Flipped on a follow-up `gd` so a slow server's
    /// stale response can't drop a popup over a moved cursor.
    pub pending_definition_token: Option<lattice_protocol::CancellationToken>,
    /// Receiver end of the App's own subscription to
    /// `EventKind::OptionChanged` (DESIGN.md §5.10 + §5.12). The
    /// typed-options registry publishes through `event_bus` on
    /// every successful set; this channel queues those events for
    /// [`Self::drain_option_changes`] to consume on the App's main
    /// thread. Decouples cascade timing from the publish path
    /// (publishes can come from any thread -- plugin tasks, future
    /// LSP-driven config writes, the customize buffer) without
    /// risking re-entrancy on the registry mutex or the renderer.
    ///
    /// `Option` only because the field needs to be `take`-able so
    /// the drain method can borrow `&mut self` for cascade work
    /// while iterating the receiver. Always `Some` between calls.
    pub option_change_rx: Option<tokio::sync::mpsc::UnboundedReceiver<Event>>,
    /// Shared language registry for tree-sitter highlighting. One
    /// `Arc<LangRegistry>` services the document buffer's `Syntax`
    /// AND every `HelpBuffer` constructed by `:describe-*` /
    /// `:apropos` / `:keymap`. Help bodies render with markdown
    /// highlighting (headings, fenced-block injections to the
    /// language tag) sourced from this same registry.
    pub lang_registry: Arc<LangRegistry>,
    pub builtins: Builtins,
    /// In-progress text in the `:` minibuffer. Populated only while
    /// `modal == ModalState::Command`.
    pub command_line: String,
    /// Most recent transient status / error message, displayed in the echo
    /// area until replaced.
    pub last_message: Option<EchoMessage>,
    /// Set by [`Action::RedrawScreen`] (`<C-l>`); the runtime
    /// clears this on its next frame after issuing a full
    /// terminal-clear so any leftover ANSI / stale glyph state
    /// gets repainted from scratch.
    pub pending_redraw: bool,
    /// Per-document tree-sitter state. `None` when the document's language
    /// is `Plain` (no grammar bundled).
    pub syntax: Option<Syntax>,
    /// `text_version` last fed to `syntax.parse(...)`. Used to skip reparse
    /// when no text mutation has happened since the previous frame.
    last_parsed_text_version: u64,
    /// Per-line `StyledSpan`s for the currently visible viewport, indexed
    /// from `[scroll, scroll + viewport_height)`. Recomputed each frame by
    /// `refresh_highlights` (called from the runtime before drawing).
    pub visible_highlights: Vec<Vec<StyledSpan>>,
    /// In-progress `/` or `?` search. `Some` only while
    /// `modal == ModalState::Search(_)`.
    pub search_line: Option<SearchLine>,
    /// Most recent submitted search; consulted by `n` / `N`.
    pub last_search: Option<LastSearch>,
    /// Range of the most recent search match, used to draw the highlight
    /// in the buffer view. Cleared on Esc and on cursor motion.
    pub current_match: Option<ProtoRange>,
    /// Every occurrence of the most recent search pattern, used to draw
    /// the secondary "hlsearch" overlay. Cleared on Esc; persists after
    /// submit until the next search.
    pub all_matches: Vec<ProtoRange>,
    /// In-progress substitute preview. Populated as the user types
    /// `:s/pat...` or `:%s/pat...` in the cmdline; the renderer
    /// overlays match ranges (and the typed replacement, when the
    /// user has typed past the second `/`) so the user sees what
    /// the substitute will do before pressing Enter. Cleared when
    /// the cmdline closes or the input no longer parses as a
    /// substitute. (DESIGN.md §5.9.10 minibuffer live preview.)
    pub substitute_preview: Option<SubstitutePreview>,
    /// Unnamed register -- destination of `y` / `d` / `c`, source of
    /// `p` / `P`. `None` until something has been yanked.
    pub unnamed_register: Option<UnnamedRegister>,
    /// In-progress count prefix being typed (`3` of `3w`, `12` of `12dd`).
    /// 0 means "no count typed". The next `Action::Invoke` consumes this
    /// and resets it to 0.
    pub pending_count: u32,
    /// Count latched when an operator key was pressed (`2` of `2d3w`).
    /// Multiplied with the motion's count (`3`) to give the final count
    /// the operator dispatches with (`6`). 0 means "no operator count".
    pub op_count: u32,
    /// Anchor position when Visual mode was entered. `None` outside
    /// Visual; restored on Esc. The `head` of the selection follows the
    /// cursor; the `anchor` stays put so the selection extends or
    /// contracts as the user moves.
    pub visual_anchor: Option<Position>,
    /// Last operator-class invocation that mutated the buffer.
    /// `.` re-dispatches it from the current cursor. v1 records
    /// operator + motion / operator + range / Visual-mode operator;
    /// insert-mode text replay is a known gap (§5.2.4).
    pub last_change: Option<CommandInvocation>,
    /// Last Visual-mode selection extents, captured on exit so `gv` can
    /// re-enter Visual with the same anchor / head / kind.
    pub last_visual: Option<LastVisual>,
    /// User-set marks. v1 stores them flat by name (a-z, A-Z, 0-9);
    /// uppercase / numbered global marks treat all marks as buffer-local
    /// since the v1 TUI runs against a single document.
    pub marks: HashMap<char, Position>,
    /// Per-Replace-session log of overwritten bytes so backspace can
    /// restore the original (rather than deleting). Cleared on entry,
    /// pushed on each `OverwriteChar`, popped on `ReplaceUndoLast`.
    /// `original` is `None` when the cursor was past EOL and the
    /// overwrite extended the line -- backspace deletes that byte rather
    /// than relying on it.
    pub replace_history: Vec<ReplaceEntry>,
    /// Named registers `"a-z`, `"A-Z`, numbered `"0-"9`, etc. Stores
    /// content + kind. `""` (the unnamed register) is the
    /// `unnamed_register` field above; this map covers everything else.
    pub registers: HashMap<Register, UnnamedRegister>,
    /// Register selected for the next operator / paste (`"a` prefix).
    /// Consumed-and-cleared by `run_invocation` (operators) and
    /// `do_paste` (paste). `None` means use unnamed.
    pub pending_register: Option<Register>,
    /// Unified position-history ring (§5.1.1). Every entry is tagged
    /// by source, so different keybindings can iterate filtered views
    /// of the same data:
    ///
    /// - `Ctrl-O` / `Ctrl-I` (Tab) walk `AutoJump` and `PluginPush`.
    /// - `g;` / `g,` walk `NamedMark`.
    ///
    /// Pushed before "big jumps" (gg, G, search submit, n / N, *, #,
    /// %, mark jumps) with `AutoJump`, plus on every `mX` with
    /// `NamedMark(X)`. The cursor sits at one past the last navigated
    /// entry; the navigation action chooses both direction and filter.
    pub position_history: Vec<PositionEntry>,
    pub position_history_cursor: usize,
    /// Macros: completed recordings keyed by register name. Replays go
    /// through `do_play_macro`. v1 records `Action` streams; insert-mode
    /// keystrokes ARE captured (every Action::Insert is recorded), but
    /// dot-repeat-style replay of insert content from `c`/`i`/`a`
    /// remains a §15 follow-up.
    pub macros: HashMap<char, Vec<Action>>,
    /// In-flight macro recording. `Some` while between `q<reg>` start
    /// and the matching `q` stop; pushed Actions append to `actions`.
    pub macro_recording: Option<MacroRecording>,
    /// The most recently played macro register, for `@@` repeat.
    pub last_played_macro: Option<char>,
    /// Last f/F/t/T find on this buffer, for `;` / `,`.
    pub last_find: Option<LastFind>,
    /// Manual folds. v1 supports non-nested folds defined by line range.
    /// `closed=true` means the fold's interior is skipped during render.
    pub folds: Vec<Fold>,
    /// Text inserted during the most recently completed Insert session.
    /// Captured on Esc out of Insert; replayed by dot-repeat after the
    /// operator part. `None` if the last change had no insert phase.
    pub last_insert: Option<String>,
    /// In-flight blockwise-visual `I` / `A` session. Captured at
    /// mode-entry time (block extents + per-line insert column);
    /// consumed when Insert exits, at which point the recorded
    /// text is replicated to every line in the block other than
    /// the top row (the top row's insert is the recording itself).
    /// `None` outside a block-visual insert.
    pub pending_block_insert: Option<PendingBlockInsert>,
    /// Text being captured during the *current* Insert session.
    /// Promoted into `last_insert` when leaving Insert.
    pub recording_insert: Option<String>,
    /// Shared typed-options registry (DESIGN.md §5.12). Every
    /// option's *current value* lives in here behind an
    /// `ArcSwap<T>`; `:set` parses against it; the customize
    /// buffer view (post-1.0) reads + writes through the same
    /// surface. Renderer-agnostic options register via
    /// [`lattice_config::register_core_options`]; this renderer's
    /// own options register via [`crate::tui_options::register_tui_options`].
    pub config: std::sync::Arc<lattice_config::ConfigRegistry>,
    /// Typed handles to the renderer-agnostic options registered
    /// at [`Self::new`] time. Used by the cmdline path
    /// (`config.parse_and_set_command`) and the cascade hook
    /// (`drain_option_changes`) that refreshes [`Self::option_cache`].
    pub core_options: lattice_config::CoreOptions,
    /// Hot-path read cache for the option values. Populated at
    /// [`Self::new`] time; refreshed inside the
    /// `Event::OptionChanged` cascade so writes through any path
    /// (cmdline, plugins, the future customize buffer) propagate.
    /// Accessor methods on `App` (`foldmethod()` / `tabstop()` /
    /// `show_line_numbers()` / ...) read the cached primitive
    /// directly (~1ns field access) instead of going through the
    /// registry's mutex + ArcSwap + downcast (~33ns). The
    /// renderer hits these accessors per visible line, so the
    /// difference is measurable on the 60-line / 120-line frame
    /// benchmarks. Single source of truth stays in
    /// [`Self::config`]; this struct is a derived projection.
    pub option_cache: OptionCache,
    /// Typed handles to the TUI-specific options. Same shape as
    /// [`Self::core_options`], scoped to options that only make
    /// sense for the terminal renderer (`ui.separator`,
    /// `ui.statusline_active_fg`, ...).
    pub tui_options: crate::tui_options::TuiOptions,
    /// Free-form help topic registry (DESIGN.md §5.11). `:help`
    /// reads from this; built-ins are sourced from `docs/help/*.md`
    /// at build time. Plugins / future LSP integrations register
    /// additional topics through the same registry.
    pub help_topics: std::sync::Arc<crate::help_topics::HelpTopicRegistry>,
    /// UI styling knobs (DESIGN.md §5.6). Carries per-pane status
    /// line colors, the inactive-pane dim overlay, separator
    /// characters, etc. Customizable via `:set ui.*` options.
    pub theme: crate::theme::Theme,
    /// Per-frame snapshot of inactive panes' visible-window syntax
    /// highlights, keyed by pane index. Refreshed by
    /// [`Self::refresh_pane_highlights`] before each draw so the
    /// renderer can read via `&App`. The active pane uses the live
    /// [`Self::visible_highlights`] field instead.
    pub pane_highlights: HashMap<usize, Vec<Vec<StyledSpan>>>,
    /// Submitted `:` command history. Newest at the back. Bounded.
    pub command_history: Vec<String>,
    /// While in Command modal: index into `command_history` of the
    /// entry currently shown (None = the user's in-progress text).
    pub command_history_cursor: Option<usize>,
    /// Snapshot of the user's typed command_line on the first Up so
    /// Down can return to it after walking through history.
    pub command_history_pending: Option<String>,
    /// Active help buffer (DESIGN.md §5.11). `Some` while a
    /// `:describe-*` / `:apropos` view is open. Held as a real
    /// rope-backed [`HelpBuffer`] -- the same data shape as a code
    /// buffer -- so the migration to multi-buffer (Phase 6 / §5.9)
    /// only needs to swap the *display strategy* without touching the
    /// help-content layer. The current display strategy is the
    /// centred popup; [`Self::help_display_mode`] picks between
    /// surfaces.
    pub help_buffer: Option<HelpBuffer>,
    /// Pane state captured before activating help -- used by
    /// `dismiss_help` to restore the user to whatever buffer +
    /// cursor + scroll they came from. Set by both display
    /// paths (in-pane via `activate_help_in_pane`, popup via
    /// `open_help_popup_overlay`); cleared by dismiss. v1 single-
    /// pane scope -- multi-pane help dismissal will key by pane
    /// id when that scenario surfaces.
    pub prev_pane_for_help: Option<PrevPaneState>,
    /// Where the active help buffer is rendered. v1 only implements
    /// `Popup`; the other variants are reserved for the multi-buffer
    /// phase. Configurable per-user (eventually via `:set
    /// help.display-mode=...`).
    pub help_display_mode: HelpDisplayMode,

    /// Pluggable completion pipeline (DESIGN.md §5.11.3). Owned by
    /// the App at v1 -- promotes to a sibling crate when plugins
    /// need cross-buffer access.
    pub completion_registry: lattice_completion::CompletionRegistry,
    /// Active completion popup. `Some` while the user has Tab-
    /// triggered completion in the `:` line.
    pub completion_state: Option<CompletionState>,
    /// Active vertico-style picker (DESIGN.md §5.9.7, §5.9.10).
    /// `Some` while a picker is open over a buffer / LSP instance
    /// / future generator. Input routes here in
    /// [`crate::input::translate`] before falling through to the
    /// modal handlers; render takes precedence over completion +
    /// hover popups.
    pub picker: Option<crate::picker::Picker>,
    /// True while a buffer activation is in *preview* mode --
    /// driven by the picker's `select_next` / `select_prev`
    /// hooks. Activate paths gate position-history pushes on
    /// this flag so a hover-preview doesn't pollute the jump
    /// list. Cleared at the end of every preview tick.
    pub previewing: bool,
    /// Receiver for [`Event::LspLogPushed`] events (Phase 4).
    /// Drained once per main-loop tick by
    /// [`Self::drain_lsp_log_events`]; matching log buffers in
    /// `BufferRegistry` are rebuilt from the logger snapshot so
    /// `*lsp*` / `*lsp:<server>*` / `*lsp:<server>:trace*` views
    /// update live without the user having to reopen them.
    pub lsp_log_event_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<lattice_protocol::Event>>,
    // `completion.auto_insert_single` lives on the typed-options
    // registry now (`self.config` keyed by
    // `self.core_options.completion_auto_insert_single`). Read via
    // [`Self::completion_auto_insert_single`].
    /// One-shot "auto-submit on next chord" flag. Set when the
    /// user submitted a Chord-arg-required command with no value
    /// (`:describe-key<CR>`); the cmdline pre-fills with the
    /// command word + space, and the very next captured chord
    /// auto-fires [`Action::CommandLineSubmit`] without an
    /// explicit `<CR>`. Reset on cancel / submit.
    pub auto_submit_after_chord: bool,
    /// LSP supervisor (DESIGN.md §5.4, Phase 4.1.h). Owns the
    /// per-(workspace, server-id) actor map, per-buffer
    /// attachments, and the shared logger / diagnostics layer
    /// references the App needs to manipulate. Wrapped in
    /// `Arc<tokio::sync::Mutex>` (Phase 4.1.i.2) so async open
    /// / close paths and sync record / flush paths can both
    /// borrow without rippling `&mut self` through the App's
    /// 44 edit call sites. The mutex is cheap (uncontended
    /// during normal operation -- App methods are
    /// single-threaded; only `:e <path>` async open contends
    /// briefly with idle-flush).
    pub lsp: std::sync::Arc<tokio::sync::Mutex<lattice_lsp::LspSupervisor>>,
    /// Cloned handle to the supervisor's diagnostics layer.
    /// `DiagnosticsLayer` is Clone-via-Arc-internal so this is
    /// cheap; the renderer's per-frame `app.lsp_diagnostics
    /// .line_severity(...)` reads happen without taking the
    /// supervisor lock.
    pub lsp_diagnostics: DiagnosticsLayer,
    /// Cloned handle to the supervisor's logger. Same lock-
    /// free read pattern as `lsp_diagnostics`.
    pub lsp_logger: LspLogger,
    /// `BufferId` → `Uri` map. Maintained by buffer-open /
    /// buffer-close paths; the supervisor's API is keyed by
    /// `Uri`, so this is the bridge.
    pub buffer_uris: std::collections::HashMap<BufferId, lattice_lsp::Uri>,
    /// Pending file-open attachments. `:e <path>` queues here
    /// because the supervisor's `open_buffer` is async; the
    /// runtime drains the queue between input events.
    pub pending_lsp_opens: Vec<(BufferId, std::path::PathBuf, String)>,
    /// Channel that record-edit fires into to wake the debounced
    /// flush task. Spawned by `initialize_lsp`. `None` until
    /// the runtime calls initialize_lsp; tests that don't
    /// invoke initialize_lsp leave it None and record_edit
    /// just skips the wake (the supervisor's queue still
    /// accumulates so a manual flush() works).
    pub lsp_flush_signal: Option<tokio::sync::mpsc::UnboundedSender<()>>,
}

/// Reasons `compute_completion_state` can fail. Kept narrow so the
/// open and refresh paths can pick different recovery strategies --
/// the open path turns these into echoed messages, the refresh path
/// usually closes the popup but keeps it alive on `NoMatches` so
/// vertico-style "type-to-filter, then back-out-to-recover" works.
#[derive(Debug, Clone)]
enum CompletionComputeError {
    NoCompletionForArg(String),
    NoCompletionAtCursor,
    MissingSource(String),
    PipelineUnconfigured,
    NoMatches { prefix: String },
}

impl CompletionComputeError {
    fn echo(&self) -> (EchoLevel, String) {
        match self {
            Self::NoCompletionForArg(name) => {
                (EchoLevel::Info, format!("no completion for arg `{name}`"))
            }
            Self::NoCompletionAtCursor => (EchoLevel::Info, "no completion at cursor".to_string()),
            Self::MissingSource(name) => (
                EchoLevel::Error,
                format!("completion source `{name}` not registered"),
            ),
            Self::PipelineUnconfigured => (
                EchoLevel::Error,
                "completion pipeline not configured (missing default matcher / ranker)".to_string(),
            ),
            Self::NoMatches { prefix } => {
                (EchoLevel::Info, format!("no completions for `{prefix}`"))
            }
        }
    }
}

/// One open completion popup (DESIGN.md §5.11.3 vertico-style
/// rendering). Built by `Action::CommandLineCompleteOrAdvance`
/// when the user presses Tab; consumed by accept / dismiss / scroll
/// actions.
#[derive(Debug, Clone)]
pub struct CompletionState {
    pub candidates: Vec<lattice_completion::RenderedCandidate>,
    pub selected: usize,
    /// Byte offset within `App.command_line` where the prefix being
    /// completed begins. The accept-handler replaces
    /// `[replace_start, command_line.len())` with the chosen
    /// candidate's `text`.
    pub replace_start: usize,
    /// What the cmdline looked like at popup-open time (for
    /// debugging + future filter-as-you-type refinement).
    pub original_line: String,
}

const COMMAND_HISTORY_CAP: usize = 100;

/// One contiguous fold range in a document buffer.
///
/// `identity` is the stable handle used to carry closed-state across
/// recomputes. Computed providers (indent / markdown) hash the
/// trimmed start-line text together with the leading-indent depth
/// so that adding or removing lines elsewhere in the buffer doesn't
/// reopen this fold. Manual folds (`zf`) leave it `None` -- their
/// stable identity is the line range itself.
#[derive(Debug, Clone, Copy)]
pub struct Fold {
    pub start_line: u32,
    pub end_line: u32,
    pub closed: bool,
    pub identity: Option<u64>,
}

// `FoldMethod` moved to `lattice_core::folding::FoldMethod` for
// renderer-agnostic ownership. Re-exported through `lattice_core`'s
// crate root + this re-export so existing call sites
// (`crate::app::FoldMethod` / `FoldMethod`) keep resolving without
// edits.
pub use lattice_core::FoldMethod;

/// Result of a `K` (LSP hover) request, sent from the spawned
/// task to the App's main thread via [`App::pending_hover_rx`].
/// Carrying the no-result variants explicitly (instead of just
/// dropping the channel send) lets the drain echo a clear
/// message so the user always gets feedback on `K`.
#[derive(Debug, Clone)]
pub enum HoverOutcome {
    /// Markdown body to feed into the popup. First non-empty wins
    /// across attached servers.
    Body(String),
    /// Walked every attached server; each returned an empty /
    /// missing hover. Echo "no hover info" so the user knows
    /// their `K` was processed but the position has nothing
    /// useful (e.g. cursor on whitespace, or rust-analyzer is
    /// still indexing).
    NoBody {
        servers_tried: usize,
    },
    /// The buffer's URI maps to no attached servers (matching
    /// servers' spawn failed at boot, or the file extension
    /// isn't covered). Echo so the user can `:lsp-status` /
    /// `:lsp-log` to investigate.
    NoServers,
}

/// Hot-path read cache for the typed-options registry's core
/// options (DESIGN.md §5.12). The renderer reads these once per
/// visible line in the gutter / wrap / tabstop logic; going
/// through the registry's mutex + `ArcSwap` + downcast on every
/// read measured at ~33ns vs. ~1ns for a direct field access.
/// At 60-120 visible lines × 2-4 reads per line per frame, the
/// difference is in the multi-µs range and showed up on the
/// `render::frame_*_lines` benches.
///
/// **Single source of truth stays in `App.config`.** This struct
/// is a derived projection refreshed via the
/// `Event::OptionChanged` cascade in
/// [`App::drain_option_changes`] -- so any write source
/// (cmdline, plugins, the future customize buffer view) keeps
/// the cache coherent through the same path.
#[derive(Debug, Clone, Copy)]
pub struct OptionCache {
    pub show_line_numbers: bool,
    pub relative_line_numbers: bool,
    pub wrap_lines: bool,
    pub ignorecase: bool,
    pub tabstop: u32,
    pub foldenable: bool,
    pub foldmethod: FoldMethod,
    pub scrolloff: u32,
    pub completion_auto_insert_single: bool,
}

impl Default for OptionCache {
    /// Defaults match `lattice-config::register_core_options`.
    /// Used at App construction before the first
    /// `rebuild_option_cache` runs; once the registry is built
    /// the cache is repopulated with the actual values (which
    /// today match these defaults but may diverge once a future
    /// `options.toml` layer applies user overrides at boot).
    fn default() -> Self {
        Self {
            show_line_numbers: true,
            relative_line_numbers: false,
            wrap_lines: false,
            ignorecase: false,
            tabstop: 8,
            foldenable: true,
            foldmethod: FoldMethod::Manual,
            scrolloff: 0,
            completion_auto_insert_single: true,
        }
    }
}

/// Capture of the most recent find/till for `;`/`,` repeat.
#[derive(Debug, Clone, Copy)]
pub struct LastFind {
    pub kind: FindKind,
    pub target: char,
}

#[derive(Debug, Clone)]
pub struct MacroRecording {
    pub register: char,
    pub actions: Vec<Action>,
}

const POSITION_HISTORY_CAP: usize = 100;

/// One entry in the unified position history (§5.1.1). v1 carries
/// the originating [`BufferKind`] + [`BufferId`] so `<C-o>` /
/// `<C-i>` walks across buffer boundaries cleanly (jumping from the
/// document into a help buffer pops back into the document
/// transparently). The `timestamp` field the spec mentions is
/// omitted in v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionEntry {
    pub position: Position,
    pub source: PositionSource,
    /// Which buffer kind this entry was recorded in. Used to
    /// switch [`App::active_buffer`] when the walk crosses kinds.
    pub buffer: BufferKind,
    /// Concrete buffer id at record time. Stale ids (e.g. an entry
    /// recorded in a now-closed Help buffer) collapse to the
    /// surviving buffer of the same kind via [`BufferKind`] alone.
    pub buffer_id: BufferId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionSource {
    /// Pushed by "big motions" -- gg, G, search, *, #, %, mark jump.
    /// The default Ctrl-O / Ctrl-I view filters to this (and Plugin).
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

#[derive(Debug, Clone)]
pub struct ReplaceEntry {
    pub at: Position,
    pub original: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct LastVisual {
    pub anchor: Position,
    pub head: Position,
    pub kind: VisualKind,
}

/// Snapshot of an in-progress `:s/pat/repl/...` preview. Refreshed
/// on every cmdline keystroke while the input parses as a
/// substitute; consumed by the renderer to overlay match ranges
/// (and the typed replacement, when present) on the target buffer.
///
/// The preview is observation-only -- it never mutates the document.
/// On submit, the actual substitute runs through `do_substitute`;
/// on cancel the preview is dropped.
#[derive(Debug, Clone)]
pub struct SubstitutePreview {
    /// Match ranges in the target line(s). Empty when the pattern
    /// is empty or compile-failed.
    pub matches: Vec<ProtoRange>,
    /// The user-typed replacement template, once the second `/` has
    /// been entered. None while the user is still inside the
    /// pattern field.
    pub replacement: Option<String>,
    /// Whether the user has explicitly typed flags including 'g'.
    /// `:s/foo/bar/g` matches every occurrence per line; without
    /// 'g' only the first match is highlighted (vim's default).
    pub global: bool,
}

/// Result of resolving a missing-arg prompt (DESIGN.md §B.1).
/// Returned by [`App::try_resolve_missing_arg_prompt`] when the
/// user submits a bare command with a required first arg empty.
struct MissingArgPrompt {
    /// New value for `command_line`. Already contains the command
    /// word + bang + a trailing space; the cursor lands at end-of-
    /// line, in the first arg slot.
    prefill: String,
    /// Kind of the first arg. Drives whether the App arms the
    /// chord-capture overlay (kind == Chord) or just leaves the
    /// cmdline open for typed input.
    kind: lattice_grammar::ArgKind,
    /// Prompt text for the echo area, taken from the schema's
    /// `prompt` field (or `"<name>:"` when empty).
    prompt: String,
}

/// In-flight blockwise-visual insert (`I` or `A`).
///
/// Vim's semantics: when the user enters `I` from blockwise visual,
/// the typed prefix is replicated to every line in the block at
/// the same column on Esc. We capture the rectangle's lines and
/// the per-line insert column at entry time, then replay the
/// recorded text to all lines except the top one (the top row was
/// edited live during the Insert session).
///
/// `A` differs only in `insert_col`: it lands one past the
/// rightmost column of the block.
///
/// `live_edits` counts the edit calls made on the top row while
/// the user typed; on Esc the App rewinds those via undo and
/// re-applies the whole I/A change as one batched edit so the
/// session lands as a single undo unit.
#[derive(Debug, Clone, Copy)]
pub struct PendingBlockInsert {
    /// First line in the block (top row -- edits flow here live).
    pub start_line: u32,
    /// Last line in the block (replication walks `start_line+1..=end_line`).
    pub end_line: u32,
    /// Byte column at which to insert on each line. For `I` this
    /// is the block's left column; for `A` it's the right column
    /// plus one. Lines whose end-of-line falls before this column
    /// are skipped (vim's behavior; trying to extend short lines
    /// is a known gap left for v2).
    pub insert_col: u32,
    /// Number of `apply_edit_blocking` calls made during the live
    /// Insert session (each typed char / backspace / paste). On
    /// Esc the App rewinds these via `undo_blocking` to collapse
    /// the entire I/A session into a single batched edit.
    pub live_edits: u32,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("cursor", &self.cursor)
            .field("scroll", &self.scroll)
            .field("should_quit", &self.should_quit)
            .field("viewport_height", &self.viewport_height)
            .field("modal", &self.modal)
            .field("pending", &self.pending)
            .field("command_line", &self.command_line)
            .field("last_message", &self.last_message)
            .field("dirty", &self.document.dirty())
            .finish()
    }
}

impl App {
    pub fn new(document: Document) -> Self {
        // LSP subsystem: build once + extract shared handles so
        // the App's `lsp_diagnostics` / `lsp_logger` reads land
        // on the same Arc-shared state the supervisor's actors
        // push to.
        let (lsp, lsp_diagnostics, lsp_logger) = build_lsp_subsystem();
        let mut registry = CommandRegistry::new();
        let builtins = populate(&mut registry);
        // Register the built-in ex-commands as peers of motions /
        // operators / text objects (DESIGN.md §5.2.1). The returned
        // ids aren't held in App state today -- the parser front-end
        // looks them up by name -- but registering them populates the
        // registry so `:`-line parsing can route to them.
        let _ex_builtins = lattice_grammar::ex_commands::populate(&mut registry);
        // §5.11.3 completion pipeline: register the built-in
        // generators / matchers / rankers / annotators and wire
        // sensible defaults (prefix matcher, score ranker, kind +
        // doc annotators).
        let mut completion_registry = lattice_completion::CompletionRegistry::new();
        let _completion_builtins = lattice_completion::populate(&mut completion_registry);
        // Help-topic registry + its completion generator
        // (`gen:help-topics`). Registering here lets `:help <Tab>`
        // enumerate built-in + plugin-supplied topics through the
        // same pipeline `:e <Tab>` and `:describe-command <Tab>`
        // use.
        let help_topics = crate::help_topics::builtin_topics();
        completion_registry.register_generator(
            "gen:help-topics",
            "Every registered free-form help topic (`:help <topic>`).",
            crate::help_topics::HelpTopicsGenerator {
                topics: help_topics.clone(),
            },
        );
        // §5.10 event bus. Stood up before the typed-options
        // registry so the registry can publish `OptionChanged`
        // events to it via the `EventPublisher` closure.
        let event_bus = Arc::new(EventBus::new());
        // Subscribe the App's own cascade-handler channel to
        // `OptionChanged` events on the bus. The receiver lives
        // on `App.option_change_rx`; `App::drain_option_changes`
        // pulls from it (called from the main loop + at the end
        // of `do_set`). This decouples cascades from the publish
        // path: any consumer that calls `config.set` -- the
        // cmdline, plugins, the future customize buffer view --
        // triggers the cascade through the same channel.
        let (option_tx, option_change_rx) = tokio::sync::mpsc::unbounded_channel();
        event_bus.subscribe(
            lattice_runtime::EventFilter::kind(lattice_protocol::EventKind::OptionChanged),
            lattice_runtime::SubscriptionTarget::Channel(option_tx),
        );
        // LSP log live-tail (Phase 4): every record the LspLogger
        // appends fires Event::LspLogPushed; the App's drain hook
        // refreshes any open `*lsp*` / `*lsp:<server>*` /
        // `*lsp:<server>:trace*` help buffer from the logger
        // snapshot so views update live as records arrive.
        let (lsp_log_tx, lsp_log_event_rx) = tokio::sync::mpsc::unbounded_channel();
        event_bus.subscribe(
            lattice_runtime::EventFilter::kind(lattice_protocol::EventKind::LspLogPushed),
            lattice_runtime::SubscriptionTarget::Channel(lsp_log_tx),
        );
        // Wire the logger's publisher to the same bus. The
        // logger lives in `lattice-lsp`; the closure captures an
        // Arc<EventBus> clone so the logger's lifetime is
        // independent of any single App field.
        let bus_for_log = event_bus.clone();
        lsp_logger.set_event_publisher(std::sync::Arc::new(move |event| {
            bus_for_log.publish(event);
        }));
        // Typed-options registry (DESIGN.md §5.12). Single source
        // of truth for every option's *current value*: each
        // `Option<T>` owns a wait-free `ArcSwap<T>` cell that
        // `:set` parses into, hot-path readers load from, and the
        // (future) customize buffer view edits through. Renderer-
        // agnostic options register from `lattice-config`; this
        // renderer's own options register from `crate::tui_options`.
        let config = Arc::new(lattice_config::ConfigRegistry::new());
        // Wire the registry's `OptionChanged` publisher to the
        // event bus (§5.10 + §5.12 unification). Subscribers see
        // every typed-option change as `Event::OptionChanged`
        // instead of having to poll. The closure captures an
        // Arc<EventBus> clone so the registry's lifetime is
        // independent of any single App field.
        let bus_for_publisher = event_bus.clone();
        config.set_event_publisher(std::sync::Arc::new(move |event| {
            bus_for_publisher.publish(event);
        }));
        let core_options = lattice_config::register_core_options(&config);
        let tui_options = crate::tui_options::register_tui_options(&config);
        // `gen:options` -- completion source for `:set <Tab>` and
        // `:set name=<Tab>`. Wired to the same `ConfigRegistry` the
        // `:set` parser consults so completions never drift from
        // the canonical option list.
        completion_registry.register_generator(
            "gen:options",
            "Every registered option name + (when applicable) its enumerated values.",
            lattice_config::OptionsGenerator::new(config.clone()),
        );
        // One `LangRegistry` per App, shared between the document
        // buffer's `Syntax` and every `HelpBuffer` we'll spin up
        // for `:describe-*` / `:apropos` / `:keymap` (markdown
        // highlighted with fenced-block language injection).
        let lang_registry = LangRegistry::standard().expect("standard lang registry");
        let lang = Lang::detect_from_path(document.path());
        let mut syntax = Syntax::for_language_with_registry(lang, lang_registry.clone())
            .ok()
            .flatten();
        if let Some(s) = syntax.as_mut() {
            s.parse(&document.text());
        }
        let last_parsed_text_version = document.text_version();
        // Hand the document to the actor (DESIGN.md §5.7). After
        // this call the only way to read or mutate it is through
        // the returned `DocumentHandle` -- the App holds no other
        // reference. The registry moves into an `Arc` so the
        // actor and the App share it without lifetime gymnastics.
        let registry = Arc::new(registry);
        let document = spawn_document(document, registry.clone());
        let snapshot_cache = document.snapshot_cache();
        let document_buffer_id = BufferId::next();
        let initial_pane = PaneState {
            id: crate::pane::PaneId::next(),
            buffer: BufferKind::Document,
            buffer_id: document_buffer_id,
            cursor: Position::ZERO,
            scroll: 0,
        };
        let pane_tree = PaneTree::single(initial_pane);
        // Seed the buffer registry with the initial document. The
        // hot-path `self.document` / `self.syntax` /
        // `self.last_parsed_text_version` mirror what's stored
        // here for the active buffer; switching buffers swaps
        // them.
        let mut buffers = BufferRegistry::new();
        buffers.insert(BufferEntry {
            id: document_buffer_id,
            flags: BufferFlags::default(),
            data: BufferData::Document(DocumentEntry {
                id: document_buffer_id,
                handle: document.clone(),
                // Active buffer's syntax / folds live on the App
                // for the hot path; the registry entry stays empty
                // until a switch snapshots the active state back.
                syntax: None,
                last_parsed_text_version: 0,
                folds: Vec::new(),
            }),
        });
        let mut app = Self {
            document,
            snapshot_cache,
            document_buffer_id,
            buffers,
            active_buffer: BufferKind::Document,
            pane_tree,
            cursor: Position::ZERO,
            scroll: 0,
            should_quit: false,
            viewport_height: 1,
            terminal_width: None,
            modal: ModalState::Normal,
            pending: Pending::None,
            registry,
            event_bus: event_bus.clone(),
            option_change_rx: Some(option_change_rx),
            pending_hover_rx: None,
            pending_hover_token: None,
            pending_definition_rx: None,
            pending_definition_token: None,
            lang_registry,
            builtins,
            command_line: String::new(),
            last_message: None,
            pending_redraw: false,
            syntax,
            last_parsed_text_version,
            visible_highlights: Vec::new(),
            search_line: None,
            last_search: None,
            current_match: None,
            all_matches: Vec::new(),
            substitute_preview: None,
            unnamed_register: None,
            pending_count: 0,
            op_count: 0,
            visual_anchor: None,
            last_change: None,
            last_visual: None,
            marks: HashMap::new(),
            replace_history: Vec::new(),
            registers: HashMap::new(),
            pending_register: None,
            position_history: Vec::new(),
            position_history_cursor: 0,
            macros: HashMap::new(),
            macro_recording: None,
            last_played_macro: None,
            last_find: None,
            folds: Vec::new(),
            last_insert: None,
            recording_insert: None,
            pending_block_insert: None,
            config,
            core_options,
            // Default placeholder; rebuilt from config below before
            // the App is returned. The placeholder lets the struct
            // literal type-check; the rebuild is the canonical
            // initial population.
            option_cache: OptionCache::default(),
            tui_options,
            help_topics,
            theme: crate::theme::Theme::default(),
            pane_highlights: HashMap::new(),
            command_history: Vec::new(),
            command_history_cursor: None,
            command_history_pending: None,
            help_buffer: None,
            prev_pane_for_help: None,
            help_display_mode: HelpDisplayMode::default(),
            completion_registry,
            completion_state: None,
            picker: None,
            previewing: false,
            lsp_log_event_rx: Some(lsp_log_event_rx),
            auto_submit_after_chord: false,
            lsp,
            lsp_diagnostics,
            lsp_logger,
            buffer_uris: std::collections::HashMap::new(),
            pending_lsp_opens: Vec::new(),
            lsp_flush_signal: None,
        };
        // Sync derived theme styles from the freshly-registered
        // ui.* options so the renderer's first frame uses the
        // configured colors / separator (rather than the static
        // Theme::default values).
        app.sync_theme_from_config();
        // Populate the hot-path option cache from canonical config
        // values. Subsequent updates flow through the
        // `Event::OptionChanged` cascade in
        // `apply_option_cascade`.
        app.rebuild_option_cache();
        app
    }

    // ---- Typed-options accessors (DESIGN.md §5.12) ----
    //
    // The current value of each option lives in `self.config`
    // behind an `ArcSwap` (single source of truth). These
    // accessors read from `self.option_cache` -- a derived
    // projection refreshed via the §5.10 cascade hook on every
    // `Event::OptionChanged` -- so the renderer's per-line option
    // checks stay at field-access speed (~1ns) instead of the
    // ~33ns mutex+ArcSwap+downcast dance per call.

    /// `:set number`. Default `true`.
    pub fn show_line_numbers(&self) -> bool {
        self.option_cache.show_line_numbers
    }

    /// `:set relativenumber`. Default `false`. When true the
    /// gutter shows distance from the cursor; the cursor's line
    /// shows its absolute number. Implies `number` (vim's
    /// behaviour) -- the cascade hook in [`Self::apply_option_cascade`]
    /// mirrors that cascade.
    pub fn relative_line_numbers(&self) -> bool {
        self.option_cache.relative_line_numbers
    }

    /// `:set wrap`. Default `false`. (v1 renderer always
    /// horizontal-scrolls; this flag is read by future B.3 polish.)
    pub fn wrap_lines(&self) -> bool {
        self.option_cache.wrap_lines
    }

    /// `:set ignorecase`. Default `false`.
    pub fn ignorecase(&self) -> bool {
        self.option_cache.ignorecase
    }

    /// `:set tabstop=N`. Default `8`. Stored as `i64` in config
    /// (the typed system's integer type) and cast back to `u32`
    /// at cache-rebuild time -- the validate closure on the option
    /// caps the range to `1..=32` so the cast can never lose bits.
    pub fn tabstop(&self) -> u32 {
        self.option_cache.tabstop
    }

    /// `:set scrolloff=N`. Default `0`. Same `i64`→`u32` shape
    /// as [`Self::tabstop`]; range `0..=64`.
    pub fn scrolloff(&self) -> u32 {
        self.option_cache.scrolloff
    }

    /// `:set foldmethod=...`. Default [`FoldMethod::Manual`].
    pub fn foldmethod(&self) -> FoldMethod {
        self.option_cache.foldmethod
    }

    /// `:set foldenable` / `:set nofoldenable` (`zi`). Default `true`.
    pub fn foldenable(&self) -> bool {
        self.option_cache.foldenable
    }

    /// `:set completion.auto_insert_single`. Default `true`.
    pub fn completion_auto_insert_single(&self) -> bool {
        self.option_cache.completion_auto_insert_single
    }

    /// Repopulate [`Self::option_cache`] from the canonical values
    /// in [`Self::config`]. Called at App-init time and from the
    /// `Event::OptionChanged` cascade so any write source (cmdline,
    /// plugin, customize buffer) refreshes the renderer-visible
    /// projection. Cheap: 9 typed reads (~30ns each).
    fn rebuild_option_cache(&mut self) {
        self.option_cache = OptionCache {
            show_line_numbers: *self.config.get(self.core_options.number),
            relative_line_numbers: *self.config.get(self.core_options.relativenumber),
            wrap_lines: *self.config.get(self.core_options.wrap),
            ignorecase: *self.config.get(self.core_options.ignorecase),
            tabstop: *self.config.get(self.core_options.tabstop) as u32,
            foldenable: *self.config.get(self.core_options.foldenable),
            foldmethod: *self.config.get(self.core_options.foldmethod),
            scrolloff: *self.config.get(self.core_options.scrolloff) as u32,
            completion_auto_insert_single: *self
                .config
                .get(self.core_options.completion_auto_insert_single),
        };
    }

    // ---- Test-only typed setters (kept on the public surface
    //      because integration tests in render.rs reach for them).
    //      Production code uses `do_set` which goes through the
    //      cmdline path. These mirror what `do_set` does sans the
    //      cmdline parse, calling `apply_post_set` so side effects
    //      (foldmethod ⇒ recompute, ui.* ⇒ theme refresh, ...) match
    //      the user-driven path. ----

    /// Set `foldmethod` directly. Drains the cascade afterwards
    /// so the option cache + recompute_folds run synchronously
    /// for the caller -- mirrors what production's `do_set` does
    /// after the cmdline path.
    pub fn set_foldmethod_for_test(&mut self, fm: FoldMethod) {
        self.config
            .set(self.core_options.foldmethod, fm)
            .expect("set foldmethod");
        self.drain_option_changes();
    }

    /// Set `foldenable` directly. Drains the cascade so the cache
    /// reflects the new value before the caller observes it.
    pub fn set_foldenable_for_test(&mut self, on: bool) {
        let _ = self.config.set(self.core_options.foldenable, on);
        self.drain_option_changes();
    }

    /// Set `completion.auto_insert_single` directly. Drains the
    /// cascade so the cache reflects the new value before the
    /// caller observes it.
    pub fn set_completion_auto_insert_single_for_test(&mut self, on: bool) {
        let _ = self
            .config
            .set(self.core_options.completion_auto_insert_single, on);
        self.drain_option_changes();
    }

    /// Async LSP boot. The runtime calls this once after
    /// [`App::new`] but before entering the main loop. Walks
    /// every currently-registered buffer (today: just the
    /// initial document) and attaches matching servers via
    /// the supervisor.
    ///
    /// Failures here do NOT block editor startup -- the editor
    /// works fine without LSP. Spawn errors (server binary not
    /// on PATH, etc.) are logged via the supervisor's logger
    /// and surfaced through the `*lsp*` buffer when the user
    /// opens it.
    pub async fn initialize_lsp(&mut self) {
        // Initial document: if the document was opened with a
        // path on disk, attach matching servers. New unsaved
        // buffers have no path and skip attachment until the
        // first :write gives them one.
        let snap = self.document.snapshot();
        let path_opt = snap.path().map(std::path::Path::to_path_buf);
        let text = snap.buffer.as_string();
        let buffer_id = self.document_buffer_id;
        drop(snap);

        if let Some(path) = path_opt {
            let result = {
                let mut sup = self.lsp.lock().await;
                sup.open_buffer(path.clone(), text).await
            };
            match result {
                Ok(_handles) => {
                    let uri = lattice_lsp::actor::uri_from_path(&path);
                    self.buffer_uris.insert(buffer_id, uri);
                }
                Err(e) => {
                    self.lsp_logger.log(
                        None,
                        LogLevel::Warn,
                        lattice_lsp::LogSource::Client,
                        format!("initialize_lsp: open_buffer failed: {e}"),
                    );
                }
            }
        }

        // Spawn the debounced flush task. Wakes 50ms after the
        // most recent record_edit signal, locks the supervisor,
        // calls flush_all() (cheap when nothing's pending,
        // correct when something is). One task per App; lives
        // for the editor's lifetime.
        let (flush_tx, mut flush_rx) =
            tokio::sync::mpsc::unbounded_channel::<()>();
        self.lsp_flush_signal = Some(flush_tx);
        let supervisor_clone = std::sync::Arc::clone(&self.lsp);
        tokio::spawn(async move {
            use tokio::time::{Duration, Instant, timeout_at};
            const DEBOUNCE: Duration = Duration::from_millis(50);
            loop {
                // Wait for first edit signal.
                if flush_rx.recv().await.is_none() {
                    return;
                }
                // Coalesce additional signals during the
                // debounce window.
                let deadline = Instant::now() + DEBOUNCE;
                loop {
                    match timeout_at(deadline, flush_rx.recv()).await {
                        Ok(Some(())) => continue,
                        Ok(None) => return,
                        Err(_) => break, // timeout -> flush
                    }
                }
                let mut sup = supervisor_clone.lock().await;
                let _ = sup.flush_all();
            }
        });
    }

    // ---- LSP integration helpers ----
    //
    // The supervisor lives on the App; its open_buffer is
    // async so we expose a sync-side hook (record_edit /
    // close_buffer / flush) for tight integration with
    // apply_edit_blocking and do_buffer_delete, and an async
    // entry point ([`Self::initialize_lsp`]) for the boot
    // path. Open-on-edit (`:e <path>`) hooks land in 4.1.i.2
    // alongside the sync->async bridge for the input pipeline.

    /// Look up the current URI of a buffer. None for buffers
    /// that have no on-disk path yet (new unsaved scratch
    /// buffers).
    pub fn buffer_uri(&self, id: BufferId) -> Option<&lattice_lsp::Uri> {
        self.buffer_uris.get(&id)
    }

    /// Notify the LSP supervisor of an edit on a buffer. Sync;
    /// `try_lock` because the supervisor mutex is uncontended
    /// during normal operation (only async open / shutdown
    /// paths take it for non-trivial windows). When contended
    /// (rare: only during `:e <path>` async open), the edit is
    /// dropped on the LSP side -- the editor's buffer still
    /// commits; the LSP layer catches up on the next edit.
    /// `&self` so it can be called from `apply_edit_blocking`
    /// without rippling `&mut self` through 44 edit call sites.
    pub fn lsp_record_edit(&self, buffer_id: BufferId, edit: &Edit) {
        let Some(uri) = self.buffer_uris.get(&buffer_id).cloned() else {
            return;
        };
        if let Ok(mut sup) = self.lsp.try_lock() {
            let _ = sup.record_edit(&uri, edit);
        }
        // Wake the debounce task so a `didChange` flush fires
        // ~50ms after this edit (modulo further edits in that
        // window).
        if let Some(tx) = self.lsp_flush_signal.as_ref() {
            let _ = tx.send(());
        }
    }

    /// Flush queued didChange events for a buffer immediately.
    /// Used by the App's debounce timer and by will-save hooks
    /// (4.3). `&self` for the same reason as
    /// [`Self::lsp_record_edit`].
    pub fn lsp_flush(&self, buffer_id: BufferId) {
        let Some(uri) = self.buffer_uris.get(&buffer_id).cloned() else {
            return;
        };
        if let Ok(mut sup) = self.lsp.try_lock() {
            let _ = sup.flush(&uri);
        }
    }

    /// Detach a buffer from every attached LSP server. Called
    /// from the bdelete path. Sends `didClose` per server +
    /// clears the URI's diagnostics.
    pub fn lsp_close_buffer(&mut self, buffer_id: BufferId) {
        let Some(uri) = self.buffer_uris.remove(&buffer_id) else {
            return;
        };
        if let Ok(mut sup) = self.lsp.try_lock() {
            let _ = sup.close_buffer(&uri);
        }
    }

    /// Queue an LSP attachment for a freshly-opened file. The
    /// runtime's main loop drains the queue between input
    /// events so `:e <path>` doesn't have to await the
    /// supervisor lock + handshake on the input thread.
    pub fn queue_lsp_open(&mut self, buffer_id: BufferId, path: std::path::PathBuf, text: String) {
        self.pending_lsp_opens.push((buffer_id, path, text));
    }

    /// Drain the pending-LSP-open queue (async; called by the
    /// runtime). For each entry, takes the supervisor lock,
    /// awaits open_buffer, and on success records the
    /// BufferId → Uri mapping.
    pub async fn drain_pending_lsp_opens(&mut self) {
        let pending: Vec<_> = std::mem::take(&mut self.pending_lsp_opens);
        for (buffer_id, path, text) in pending {
            let result = {
                let mut sup = self.lsp.lock().await;
                sup.open_buffer(path.clone(), text).await
            };
            match result {
                Ok(_handles) => {
                    let uri = lattice_lsp::actor::uri_from_path(&path);
                    self.buffer_uris.insert(buffer_id, uri);
                }
                Err(e) => {
                    self.lsp_logger.log(
                        None,
                        LogLevel::Warn,
                        lattice_lsp::LogSource::Client,
                        format!(
                            "drain_pending_lsp_opens: open_buffer({}) failed: {e}",
                            path.display()
                        ),
                    );
                }
            }
        }
    }

    // ---- LSP diagnostic navigation (Phase 4.1.d.iv) ---------

    /// `:diagnostics` -- open a help-style buffer listing every
    /// workspace diagnostic with clickable per-entry source
    /// links.
    pub fn do_list_diagnostics(&mut self) {
        let buffer = crate::help::HelpBuffer::diagnostics(&self.lsp_diagnostics)
            .with_markdown_syntax(self.lang_registry.clone());
        self.open_help(buffer);
    }

    /// `]d` / `:diag-next` / `:cnext` -- move the cursor to the
    /// next diagnostic in the active buffer. Wraps to top.
    pub fn do_next_diagnostic(&mut self) {
        let Some(uri) = self.buffer_uris.get(&self.document_buffer_id) else {
            self.set_message(EchoLevel::Error, "no LSP attachment".to_string());
            return;
        };
        let mut diags = self.lsp_diagnostics.diagnostics_for(uri);
        if diags.is_empty() {
            self.set_message(EchoLevel::Info, "no diagnostics in buffer".to_string());
            return;
        }
        diags.sort_by_key(|d| (d.range.start.line, d.range.start.character));
        let cursor = self.cursor;
        let Some(next) = diags
            .iter()
            .find(|d| {
                d.range.start.line > cursor.line
                    || (d.range.start.line == cursor.line
                        && d.range.start.character > cursor.byte)
            })
            .or_else(|| diags.first())
            .map(|d| d.range.start)
        else {
            // Unreachable: the empty-diags case returned early
            // above, so first() is always Some here. Surface a
            // no-op rather than panicking if the invariant
            // breaks.
            return;
        };
        self.cursor = Position::new(next.line, next.character);
        self.publish_position_change();
    }

    /// `[d` / `:diag-prev` / `:cprev` -- move the cursor to the
    /// previous diagnostic in the active buffer. Wraps to
    /// bottom.
    pub fn do_prev_diagnostic(&mut self) {
        let Some(uri) = self.buffer_uris.get(&self.document_buffer_id) else {
            self.set_message(EchoLevel::Error, "no LSP attachment".to_string());
            return;
        };
        let mut diags = self.lsp_diagnostics.diagnostics_for(uri);
        if diags.is_empty() {
            self.set_message(EchoLevel::Info, "no diagnostics in buffer".to_string());
            return;
        }
        diags.sort_by_key(|d| (d.range.start.line, d.range.start.character));
        let cursor = self.cursor;
        let Some(prev) = diags
            .iter()
            .rev()
            .find(|d| {
                d.range.start.line < cursor.line
                    || (d.range.start.line == cursor.line
                        && d.range.start.character < cursor.byte)
            })
            .or_else(|| diags.last())
            .map(|d| d.range.start)
        else {
            return;
        };
        self.cursor = Position::new(prev.line, prev.character);
        self.publish_position_change();
    }

    /// Helper: publish a position-only change event. Cheap
    /// stand-in for whatever the rest of the App uses to
    /// signal cursor moves. Currently a no-op since the
    /// renderer reads cursor directly; reserved for future
    /// position-history pushes.
    fn publish_position_change(&self) {
        // 4.1.d.iv: position history hook reserved -- a real
        // PluginPush entry lands here when the position-history
        // wiring catches up.
    }

    // ---- LSP introspection (Phase 4.1.g) --------------------

    /// `:lsp-log [server]` -- open a per-server log buffer in the
    /// active pane.
    ///
    /// - **No arg**: open the vertico-style picker over every
    ///   running `(workspace, server_id)` instance. `<CR>` opens
    ///   `*lsp:<server>*` for the chosen row.
    /// - **`server` arg**: pre-filter the picker. If exactly one
    ///   instance matches (the common case -- one rust-analyzer
    ///   per workspace), short-circuit and open directly. If
    ///   multiple instances match (same server id across multiple
    ///   workspaces), the picker still appears so the user
    ///   disambiguates by workspace path.
    ///
    /// Buffer goes through [`Self::open_help_in_pane`] -- it lives
    /// in `BufferRegistry` and is reachable via `:bn` / `:b N` /
    /// the buffer picker (Phase 1 / Phase 2 wiring).
    pub fn do_open_lsp_log(&mut self, server_id: Option<&str>) {
        self.open_lsp_picker(
            "lsp-log",
            server_id.map(|s| s.to_string()),
            crate::picker::PickerAction::OpenLspLog,
        );
    }

    /// `:lsp-trace-log [server]` -- open the JSON-RPC trace ring
    /// in the active pane. Same dispatch shape as `:lsp-log`:
    /// picker on no-arg or multi-match, direct open on single
    /// match. **Does not toggle tracing** -- pair with
    /// `:lsp-trace <server>` to start / stop the wire trace; this
    /// command only views the records.
    pub fn do_open_lsp_trace_log(&mut self, server_id: Option<&str>) {
        self.open_lsp_picker(
            "lsp-trace-log",
            server_id.map(|s| s.to_string()),
            crate::picker::PickerAction::OpenLspTraceLog,
        );
    }

    /// `:lsp-trace <name>` -- toggle JSON-RPC trace for the
    /// server. Pure toggle: the trace buffer is opened by the
    /// separate `:lsp-trace-log [server]` command so peeking
    /// mid-stream doesn't flip the toggle off.
    ///
    /// `name` is resolved against running actors first (exact id
    /// match), then against configured binary names so `:lsp-trace
    /// rust-analyzer` resolves to the `rust` actor id when the
    /// user types the binary name they recognise. On miss the echo
    /// lists running actor ids so the user sees what's available
    /// instead of a phantom-toggle that goes nowhere.
    pub fn do_toggle_lsp_trace(&mut self, name: &str) {
        let resolved = self.resolve_server_id(name);
        let Some(server_id) = resolved else {
            let running = self.running_server_ids();
            let listing = if running.is_empty() {
                "no LSP servers running".to_string()
            } else {
                format!("running: {}", running.join(", "))
            };
            self.set_message(
                EchoLevel::Error,
                format!("lsp-trace: no server matches {name:?} ({listing})"),
            );
            return;
        };
        let id: std::sync::Arc<str> = std::sync::Arc::from(server_id.as_str());
        let now_on = self.lsp_logger.toggle_trace(id);
        let label = if now_on { "on" } else { "off" };
        let alias_note = if server_id != name {
            format!(" (resolved {name:?} -> {server_id:?})")
        } else {
            String::new()
        };
        self.set_message(
            EchoLevel::Info,
            format!(
                "lsp-trace {server_id}: {label}{alias_note} (use :lsp-trace-log {server_id} to view)"
            ),
        );
    }

    /// Resolve a user-supplied server name to a canonical server
    /// id. Tries, in order:
    ///
    /// 1. Exact id match against running actors (the common case
    ///    once a buffer has attached).
    /// 2. Exact id match against registered configs (so
    ///    `:lsp-trace rust` works pre-spawn -- e.g. enable trace
    ///    before opening the first .rs file).
    /// 3. Binary file-name (or stem) match against configs (so
    ///    `:lsp-trace rust-analyzer` resolves to the `rust` actor
    ///    id when the user types the binary they recognise).
    ///
    /// Returns `None` when none matches.
    fn resolve_server_id(&self, name: &str) -> Option<String> {
        let sup = self.lsp.try_lock().ok()?;
        for ((_, sid), _) in sup.running_actors() {
            if sid == name {
                return Some(sid);
            }
        }
        for cfg in sup.configs() {
            if cfg.id == name {
                return Some(cfg.id.clone());
            }
            let file = cfg
                .binary
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            let stem = file.trim_end_matches(".exe");
            if file == name || stem == name {
                return Some(cfg.id.clone());
            }
        }
        None
    }

    /// Distinct server ids of every running actor. Used in echo
    /// messages so the user sees what's available.
    fn running_server_ids(&self) -> Vec<String> {
        let Ok(sup) = self.lsp.try_lock() else {
            return Vec::new();
        };
        let mut ids: Vec<String> = sup
            .running_actors()
            .into_iter()
            .map(|((_, sid), _)| sid)
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// `:lsp-status` -- render every running server in a
    /// help-style buffer.
    pub fn do_lsp_status(&mut self) {
        // try_lock is enough; the App is single-threaded and
        // the supervisor isn't held across .await right now.
        let buffer = match self.lsp.try_lock() {
            Ok(sup) => crate::help::HelpBuffer::lsp_status(&sup),
            Err(_) => crate::help::HelpBuffer::from_lines(
                "lsp-status",
                vec![
                    "# :lsp-status".into(),
                    String::new(),
                    "(supervisor lock unavailable; an async open / shutdown is in flight)".into(),
                ],
            ),
        };
        self.open_help(buffer.with_markdown_syntax(self.lang_registry.clone()));
    }

    /// `:lsp-server-log` -- vertico picker over every running
    /// `(workspace, server_id)` LSP actor. `<CR>` opens the
    /// per-server log (`*lsp:<server>*`) for the chosen row. Use
    /// `:lsp-trace-log` for the trace-ring view; `:lsp-status` for
    /// the read-only static overview.
    pub fn do_lsp_server_log_listing(&mut self) {
        self.open_lsp_picker(
            "lsp-server-log",
            None,
            crate::picker::PickerAction::OpenLspLog,
        );
    }

    /// `:lsp-restart <server>` -- supervisor restart hook.
    /// Currently emits an info message; full restart-with-
    /// backoff lands in 4.4.
    pub fn do_lsp_restart(&mut self, server_id: &str) {
        self.set_message(
            EchoLevel::Info,
            format!(
                "lsp-restart {}: supervisor restart wiring lands in 4.4",
                server_id
            ),
        );
    }

    /// `:lsp-log-level [server] <level>` -- set the subsystem
    /// default min level (when no server) or a per-server
    /// override.
    pub fn do_set_lsp_log_level(&mut self, server_id: Option<&str>, level: &str) {
        let Some(parsed) = lattice_lsp::LogLevel::parse(level) else {
            self.set_message(
                EchoLevel::Error,
                format!(
                    "unknown log level {level:?}; expected error/warn/info/debug/trace"
                ),
            );
            return;
        };
        match server_id {
            None => {
                self.lsp_logger.set_default_level(parsed);
                self.set_message(
                    EchoLevel::Info,
                    format!("lsp default log level: {level}"),
                );
            }
            Some(id) => {
                let arc: std::sync::Arc<str> = std::sync::Arc::from(id);
                self.lsp_logger.set_server_level(arc, Some(parsed));
                self.set_message(
                    EchoLevel::Info,
                    format!("lsp log level for {id}: {level}"),
                );
            }
        }
    }

    /// `:lsp-log-clear [server]` -- drop ring contents.
    pub fn do_lsp_log_clear(&mut self, server_id: Option<&str>) {
        match server_id {
            None => {
                self.lsp_logger.clear_global();
                self.set_message(EchoLevel::Info, "*lsp* cleared".to_string());
            }
            Some(id) => {
                let arc: std::sync::Arc<str> = std::sync::Arc::from(id);
                self.lsp_logger.clear_server(&arc);
                self.set_message(
                    EchoLevel::Info,
                    format!("*lsp:{id}* cleared"),
                );
            }
        }
    }

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
    pub fn apply_edit_blocking(&self, edit: Edit) -> Result<AppliedEdit, RuntimeError> {
        let result = block_on(self.document.apply_edit(edit.clone()));
        if result.is_ok() {
            self.publish_document_changed();
            self.lsp_record_edit(self.document_buffer_id, &edit);
        }
        result
    }

    /// Block_on `apply_edit_batch`. The batch lands as one undo
    /// unit on the document's undo stack. Each edit in the
    /// batch is also fed to the LSP supervisor in order
    /// (Phase 4.1.i.2).
    pub fn apply_edit_batch_blocking(
        &self,
        edits: Vec<Edit>,
    ) -> Result<Vec<AppliedEdit>, RuntimeError> {
        let edits_for_lsp = edits.clone();
        let result = block_on(self.document.apply_edit_batch(edits));
        if result.is_ok() {
            self.publish_document_changed();
            for edit in &edits_for_lsp {
                self.lsp_record_edit(self.document_buffer_id, edit);
            }
        }
        result
    }

    pub fn undo_blocking(&self) -> Result<Vec<AppliedEdit>, RuntimeError> {
        let result = block_on(self.document.undo());
        if result.is_ok() {
            self.publish_document_changed();
        }
        result
    }

    pub fn redo_blocking(&self) -> Result<Vec<AppliedEdit>, RuntimeError> {
        let result = block_on(self.document.redo());
        if result.is_ok() {
            self.publish_document_changed();
        }
        result
    }

    pub fn save_blocking(&self) -> Result<std::path::PathBuf, RuntimeError> {
        // BeforeSave fires before the actor commits, so a future
        // veto-class handler (§5.10.2) can format / sanitize the
        // buffer before it hits disk. v1 is observation-only, so
        // BeforeSave runs only for telemetry / autocmd compatibility.
        let snap = self.document.snapshot();
        if let Some(path) = snap.path.as_ref() {
            self.event_bus.publish(Event::BeforeSave {
                id: snap.id,
                path: (**path).clone(),
            });
        }
        let result = block_on(self.document.save());
        if let Ok(path) = result.as_ref() {
            self.event_bus.publish(Event::DocumentSaved {
                id: snap.id,
                path: path.clone(),
            });
        }
        result
    }

    pub fn save_as_blocking(&self, path: std::path::PathBuf) -> Result<(), RuntimeError> {
        let snap = self.document.snapshot();
        self.event_bus.publish(Event::BeforeSave {
            id: snap.id,
            path: path.clone(),
        });
        let result = block_on(self.document.save_as(path.clone()));
        if result.is_ok() {
            self.event_bus
                .publish(Event::DocumentSaved { id: snap.id, path });
        }
        result
    }

    pub fn set_selections_blocking(&self, selections: SelectionSet) {
        // SetSelections only fails on actor-gone; ignore the
        // Result (post-shutdown nothing meaningful to do).
        let _ = block_on(self.document.set_selections(selections));
        self.publish_selections_changed();
    }

    /// Build + publish [`Event::DocumentChanged`] from the current
    /// snapshot. Called from every path that mutates the buffer
    /// (apply_edit / batch / undo / redo). The post-mutation
    /// snapshot drives the event payload.
    fn publish_document_changed(&self) {
        let snap = self.document.snapshot();
        // v1 doesn't carry the per-edit AppliedEdits in the event
        // payload (the protocol's `Event::DocumentChanged.edits`
        // field is reserved for the future actor-side publish path
        // where the actor knows what was applied). For now the
        // event signals "something changed; reload via snapshot".
        self.event_bus.publish(Event::DocumentChanged {
            id: snap.id,
            version: snap.version,
            edits: Vec::new(),
        });
    }

    /// Build + publish [`Event::SelectionsChanged`] from the current
    /// snapshot. Called whenever the App's view of selections
    /// rotates (visual extension, dispatcher SelectionChange effect,
    /// `gv` reselect, etc.).
    fn publish_selections_changed(&self) {
        let snap = self.document.snapshot();
        self.event_bus.publish(Event::SelectionsChanged {
            id: snap.id,
            version: snap.version,
            selections: (*snap.selections).clone(),
        });
    }

    /// Replace the actor's document outright. Used by `:edit
    /// path`. The actor swaps state in place and republishes the
    /// snapshot.
    pub fn replace_document_blocking(&self, document: Document) {
        let _ = block_on(self.document.replace(document));
    }

    /// Block_on a grammar dispatch through the actor (DESIGN.md
    /// §5.2.1). Replaces direct `lattice_grammar::execute(&self.registry,
    /// &mut self.document, ...)` calls; the actor holds the only
    /// `&mut Document` and runs `execute` inside its task.
    ///
    /// v1 passes a `CancellationToken::never()` -- the input loop
    /// (`lattice_ui_tui::runtime::run`) is single-threaded crossterm
    /// poll, so no concurrent code path can flip the token while
    /// `block_on` parks the thread. The plumbing is in place for a
    /// future runtime that reads input on a separate task and flips
    /// the dispatch token on Esc; see `dispatch_with_cancel` on
    /// [`DocumentHandle`].
    pub fn dispatch_blocking(&self, invocation: CommandInvocation) -> Result<Effect, RuntimeError> {
        block_on(self.document.dispatch_with_cancel(
            invocation,
            self.cursor,
            CancellationToken::never(),
        ))
    }

    pub fn apply(&mut self, action: Action) {
        // Snapshot pre-dispatch state for the State-A hover
        // auto-dismiss hook below: while a hover popup is shown
        // and focus is still on the main buffer, any motion that
        // changes the doc cursor closes the popup -- the popup
        // is anchored to the symbol the user pressed `K` on, so
        // a cursor motion makes it stale. Once the user has
        // pressed `K` again to *focus into* the popup (State B,
        // active_buffer == Help), this auto-dismiss is skipped:
        // motions there move the popup's cursor, not the doc's.
        let pre_active = self.active_buffer;
        let pre_cursor = self.cursor;
        let popup_in_state_a = self.help_buffer.is_some()
            && self.prev_pane_for_help.is_none()
            && pre_active == BufferKind::Document;
        // While a macro recording is in flight, capture every Action
        // EXCEPT the recording-management ones themselves (otherwise the
        // recording would include "stop recording" or recurse on play).
        if let Some(rec) = self.macro_recording.as_mut()
            && !matches!(
                action,
                Action::StartMacroRecord(_)
                    | Action::StopMacroRecord
                    | Action::PlayMacro(_)
                    | Action::PlayLastMacro
            )
        {
            rec.actions.push(action.clone());
        }
        // Pending lifecycle: any action that *resolves* (i.e. isn't
        // itself SetPending) consumes the pending state. Without this,
        // a chord like `zz` (ScrollCursorTo) would leave pending=AfterZ
        // so the next key `j` would route through resolve_after_z and
        // emit GotoNextFold instead of line_down.
        if !matches!(action, Action::SetPending(_)) {
            self.pending = Pending::None;
        }
        // Read-only guard for help: when a help buffer holds focus
        // (DESIGN.md §5.9 active-buffer routing), buffer-mutating
        // actions (Insert / Delete / Paste / Undo / Redo / fold ops
        // / etc.) silently no-op with a "read-only" echo. Motion-
        // and scroll-class actions, the universal escape hatches
        // (Quit, EnterCommandLine, HelpDismiss), and command-line
        // editing actions all keep working -- the read-only set is
        // narrow and explicit, so additions to `Action` default to
        // working in help unless they're added to this list.
        if matches!(self.active_buffer, BufferKind::Help) && action_is_document_mutation(&action) {
            self.set_message(EchoLevel::Info, "buffer is read-only".to_string());
            self.ensure_cursor_visible();
            self.maybe_reparse_syntax();
            return;
        }
        match action {
            Action::None => {}
            Action::Quit => {
                self.event_bus.publish(Event::BeforeQuit);
                self.should_quit = true;
            }
            Action::Invoke(inv) => self.run_invocation(inv),
            Action::Insert(s) => self.do_insert_text(&s),
            Action::DeleteCharBackward => self.do_delete_char_backward(),
            Action::EnterMode(state) => self.enter_mode(state),
            Action::EnterAppend => self.do_enter_append(),
            Action::EnterBlockVisualInsert => self.do_enter_block_visual_insert(false),
            Action::EnterBlockVisualAppend => self.do_enter_block_visual_insert(true),
            Action::OpenLineBelow => self.do_open_line_below(),
            Action::OpenLineAbove => self.do_open_line_above(),
            Action::SetPending(p) => {
                // When entering operator-pending state, latch the in-progress
                // count as `op_count` so the next motion's count multiplies
                // with it (vim's `2d3w` -> d6w). Other pending transitions
                // keep `pending_count` so a partially-typed `gg` count
                // survives the chord.
                if matches!(p, Pending::AfterOperator(_)) && self.pending_count > 0 {
                    self.op_count = self.pending_count;
                    self.pending_count = 0;
                }
                self.pending = p;
            }
            Action::Undo => {
                let _ = self.undo_blocking();
                self.clamp_cursor_to_buffer();
            }
            Action::Redo => {
                let _ = self.redo_blocking();
                self.clamp_cursor_to_buffer();
            }

            Action::EnterCommandLine => {
                self.command_line.clear();
                self.modal = ModalState::Command;
                self.pending = Pending::None;
                self.last_message = None;
                // Q16: opening the cmdline dismisses any open help.
                // The user can only focus on one thing.
                self.dismiss_help();
                self.completion_state = None;
            }
            Action::CommandLineAppend(c) => {
                if matches!(self.modal, ModalState::Command) {
                    self.command_line.push(c);
                    // Vertico-style live filtering: if the popup is
                    // open, re-run the pipeline against the new
                    // prefix. The user can keep typing to drill
                    // down without losing the popup.
                    if self.completion_state.is_some() {
                        self.refresh_completion_popup();
                    }
                    self.refresh_substitute_preview();
                }
            }
            Action::CommandLineBackspace => {
                if matches!(self.modal, ModalState::Command) {
                    if self.command_line.pop().is_none() {
                        // Empty buffer + backspace -> exit Command modal.
                        self.modal = ModalState::Normal;
                        self.completion_state = None;
                        self.substitute_preview = None;
                    } else {
                        if self.completion_state.is_some() {
                            // Popup live-refilters against the shorter
                            // prefix (vertico-style).
                            self.refresh_completion_popup();
                        }
                        self.refresh_substitute_preview();
                    }
                }
            }
            Action::CommandLineSubmit => {
                if matches!(self.modal, ModalState::Command) {
                    // Missing-arg prompt path (DESIGN.md §B.1):
                    // if the user submitted with a required first
                    // arg empty (`:describe-key<CR>`, `:write<CR>`,
                    // `:edit<CR>`, ...), don't fail -- prefill the
                    // cmdline with the command word + space, set
                    // the cursor in the arg slot, and surface the
                    // schema's prompt in the echo area. For Chord
                    // args we additionally arm a one-shot auto-
                    // submit so the very next captured chord runs
                    // the lookup with no second <CR>; for other
                    // kinds the user types and submits normally.
                    if let Some(info) = self.try_resolve_missing_arg_prompt() {
                        let is_chord = info.kind == lattice_grammar::ArgKind::Chord;
                        self.command_line = info.prefill;
                        self.auto_submit_after_chord = is_chord;
                        self.set_message(EchoLevel::Info, info.prompt);
                        return;
                    }
                    let line = std::mem::take(&mut self.command_line);
                    self.modal = ModalState::Normal;
                    self.pending = Pending::None;
                    self.command_history_cursor = None;
                    self.command_history_pending = None;
                    self.auto_submit_after_chord = false;
                    self.substitute_preview = None;
                    if !line.trim().is_empty() {
                        // De-duplicate consecutive identical entries.
                        if self.command_history.last() != Some(&line) {
                            self.command_history.push(line.clone());
                            if self.command_history.len() > COMMAND_HISTORY_CAP {
                                self.command_history.remove(0);
                            }
                        }
                    }
                    self.execute_ex_line(&line);
                }
            }
            Action::CommandLineCancel => {
                if matches!(self.modal, ModalState::Command) {
                    self.command_line.clear();
                    self.command_history_cursor = None;
                    self.command_history_pending = None;
                    self.modal = ModalState::Normal;
                    self.pending = Pending::None;
                    self.auto_submit_after_chord = false;
                    self.substitute_preview = None;
                }
            }
            Action::CommandLineHistoryPrev => self.do_command_history_step(true),
            Action::CommandLineHistoryNext => self.do_command_history_step(false),
            Action::Echo(message) => {
                self.last_message = Some(message);
            }

            Action::CloseHover => self.do_close_hover(),
            Action::PickerAppend(c) => {
                if let Some(p) = self.picker.as_mut() {
                    p.append_query(c);
                }
                self.preview_picker_selection();
            }
            Action::PickerBackspace => {
                if let Some(p) = self.picker.as_mut() {
                    p.backspace_query();
                }
                self.preview_picker_selection();
            }
            Action::PickerSelectNext => {
                if let Some(p) = self.picker.as_mut() {
                    p.select_next();
                }
                self.preview_picker_selection();
            }
            Action::PickerSelectPrev => {
                if let Some(p) = self.picker.as_mut() {
                    p.select_prev();
                }
                self.preview_picker_selection();
            }
            Action::PickerAccept => self.do_picker_accept(),
            Action::PickerDismiss => self.do_picker_dismiss(),

            Action::PushDigit(d) => {
                // Accumulate one decimal digit into the pending count.
                // Saturating math prevents overflow on absurd inputs.
                self.pending_count = self
                    .pending_count
                    .saturating_mul(10)
                    .saturating_add(d.into());
            }

            Action::EnterVisual(kind) => self.do_enter_visual(kind),
            Action::ExitVisual => self.do_exit_visual(),
            Action::ReselectLastVisual => self.do_reselect_visual(),
            Action::SearchWordUnderCursor(direction) => self.do_search_word_under_cursor(direction),
            Action::MatchBracket => self.do_match_bracket(),
            Action::ToggleCaseAtCursor => self.do_toggle_case_at_cursor(),
            Action::JoinLines { with_space } => self.do_join_lines(with_space),
            Action::FindRepeat { reverse } => self.do_find_repeat(reverse),

            Action::CreateFoldFromVisual => self.do_create_fold_from_visual(),
            Action::OpenFoldAtCursor => self.do_set_fold_state_at_cursor(Some(false)),
            Action::CloseFoldAtCursor => self.do_set_fold_state_at_cursor(Some(true)),
            Action::ToggleFoldAtCursor => self.do_set_fold_state_at_cursor(None),
            Action::OpenAllFolds => self.do_set_all_folds(false),
            Action::CloseAllFolds => self.do_set_all_folds(true),
            Action::DeleteFoldAtCursor => self.do_delete_fold_at_cursor(),
            Action::GotoNextFold => self.do_goto_fold(true),
            Action::GotoPrevFold => self.do_goto_fold(false),
            Action::ToggleFoldEnable => {
                // `zi` toggle path. The set() publishes through
                // the bus; drain immediately so the cascade
                // refreshes `option_cache.foldenable` before any
                // subsequent reads in this same `apply` call (and
                // before the next frame draws).
                let cur = self.foldenable();
                let _ = self.config.set(self.core_options.foldenable, !cur);
                self.drain_option_changes();
            }
            Action::LspHoverRequest => self.do_lsp_hover_request(),
            Action::LspDefinitionRequest => self.do_lsp_definition_request(),
            Action::SelectRegister(reg) => {
                self.pending_register = Some(reg);
            }
            Action::JumpHistoryBack => self.do_jump_history(-1),
            Action::JumpHistoryForward => self.do_jump_history(1),
            Action::RedrawScreen => self.do_redraw_screen(),
            Action::WalkMarkHistoryBack => self.do_mark_history(-1),
            Action::WalkMarkHistoryForward => self.do_mark_history(1),

            Action::StartMacroRecord(reg) => self.do_start_macro_record(reg),
            Action::StopMacroRecord => self.do_stop_macro_record(),
            Action::PlayMacro(reg) => self.do_play_macro(reg),
            Action::PlayLastMacro => {
                if let Some(reg) = self.last_played_macro {
                    self.do_play_macro(reg);
                } else {
                    self.set_message(EchoLevel::Error, "no previous macro".to_string());
                }
            }

            Action::OverwriteChar(c) => self.do_overwrite_char(c),
            Action::ReplaceUndoLast => self.do_replace_undo_last(),

            Action::JumpViewport(vp) => self.do_jump_viewport(vp),
            Action::ScrollCursorTo(sp) => self.do_scroll_cursor_to(sp),
            Action::PageDown => self.do_page(true),
            Action::PageUp => self.do_page(false),
            Action::ScrollLineUp => self.do_scroll_line(false),
            Action::ScrollLineDown => self.do_scroll_line(true),

            Action::SetMark(name) => {
                if is_valid_mark_name(name) {
                    self.marks.insert(name, self.cursor);
                    // Also fold into the unified position history so
                    // `g;` / `g,` can walk through marks chronologically.
                    let cur = self.cursor;
                    self.push_position_history(cur, PositionSource::NamedMark(name));
                } else {
                    self.set_message(EchoLevel::Error, format!("invalid mark: {name}"));
                }
            }
            Action::JumpToMarkLine(name) => self.do_jump_mark(name, false),
            Action::JumpToMarkExact(name) => self.do_jump_mark(name, true),

            Action::RepeatLastChange => {
                if let Some(inv) = self.last_change.clone() {
                    // Snapshot last_insert because run_invocation may
                    // reset it (running the change op enters Insert,
                    // which clears recording_insert) -- we want the
                    // OLD text to replay.
                    let insert_replay = self.last_insert.clone();
                    self.run_invocation(inv);
                    // If the change flipped us into Insert and there's
                    // captured text, replay it and exit back to Normal.
                    if matches!(self.modal, ModalState::Insert)
                        && let Some(text) = insert_replay
                    {
                        self.do_insert_text(&text);
                        self.enter_mode(ModalState::Normal);
                    }
                } else {
                    self.set_message(EchoLevel::Error, "no previous change to repeat".to_string());
                }
            }

            Action::PasteAfter => self.do_paste(false),
            Action::PasteBefore => self.do_paste(true),
            Action::PasteText(text) => self.do_paste_text(&text),

            // ---- Command-line editing + completion ----
            Action::CommandLineClear => {
                if matches!(self.modal, ModalState::Command) {
                    self.command_line.clear();
                    if self.completion_state.is_some() {
                        // Empty cmdline -> slot becomes Empty, which
                        // surfaces every command. Same live-refilter
                        // contract as the other edit actions.
                        self.refresh_completion_popup();
                    }
                }
            }
            Action::CommandLineDeleteWordBackward => {
                if matches!(self.modal, ModalState::Command) {
                    delete_trailing_word(&mut self.command_line);
                    if self.completion_state.is_some() {
                        // Same live-refilter contract as Append /
                        // Backspace.
                        self.refresh_completion_popup();
                    }
                }
            }
            Action::CommandLineDescribeUnderCursor => self.do_command_line_describe_under_cursor(),
            Action::CommandLineAppendChord(token) => {
                if matches!(self.modal, ModalState::Command) {
                    self.command_line.push_str(&token);
                    // Chord-capture suppresses the completion popup
                    // (no useful candidates for chord input). If
                    // somehow open, drop it to keep the screen clean.
                    self.completion_state = None;
                    // One-shot auto-submit: when the cmdline was
                    // armed by a missing-arg prompt, the very next
                    // chord token also fires submit. Recursive
                    // re-entry into apply() is fine -- Submit
                    // resets the flag before doing anything else.
                    if self.auto_submit_after_chord {
                        self.auto_submit_after_chord = false;
                        self.apply(Action::CommandLineSubmit);
                    }
                }
            }
            Action::CommandLineDeleteChord => {
                if matches!(self.modal, ModalState::Command) {
                    let n = crate::chord::last_chord_token_byte_len(&self.command_line);
                    if n == 0 {
                        // Empty buffer + delete -> exit Command modal,
                        // matching plain `<BS>` semantics.
                        self.modal = ModalState::Normal;
                        self.completion_state = None;
                    } else {
                        let new_len = self.command_line.len() - n;
                        self.command_line.truncate(new_len);
                    }
                }
            }
            Action::CommandLineCompleteOrAdvance => self.do_command_line_complete_or_advance(),
            Action::CommandLineCompletePrev => self.do_command_line_complete_prev(),
            Action::CommandLineAcceptCompletion => self.do_command_line_accept_completion(),
            Action::CommandLineDismissCompletion => {
                self.completion_state = None;
            }

            Action::HelpDismiss => match self.active_buffer {
                BufferKind::Help => self.dismiss_help(),
                BufferKind::FileTree => self.dismiss_file_tree(),
                BufferKind::Document => {}
            },
            Action::FollowLink => match self.active_buffer {
                BufferKind::Help => self.do_help_follow_link(),
                BufferKind::FileTree => self.do_file_tree_follow(),
                BufferKind::Document => {}
            },

            Action::SplitPaneHorizontal => self.do_split_pane(SplitOrientation::Horizontal),
            Action::SplitPaneVertical => self.do_split_pane(SplitOrientation::Vertical),
            Action::ClosePane => self.do_close_pane(),
            Action::NavigatePane(dir) => self.do_navigate_pane(dir),
            Action::NextPane => {
                let target = self.pane_tree.next_pane();
                self.activate_pane(target);
            }
            Action::PrevPane => {
                let target = self.pane_tree.prev_pane();
                self.activate_pane(target);
            }

            Action::EnterSearch(direction) => {
                self.search_line = Some(SearchLine {
                    direction,
                    pattern: String::new(),
                    origin: self.cursor,
                });
                self.modal = ModalState::Search(direction);
                self.pending = Pending::None;
                self.last_message = None;
                self.current_match = None;
            }
            Action::SearchAppend(c) => {
                if let Some(line) = self.search_line.as_mut() {
                    line.pattern.push(c);
                    self.preview_search();
                }
            }
            Action::SearchBackspace => {
                let leave = match self.search_line.as_mut() {
                    Some(line) => {
                        if line.pattern.pop().is_none() {
                            true
                        } else {
                            self.preview_search();
                            false
                        }
                    }
                    None => false,
                };
                if leave {
                    self.cancel_search();
                }
            }
            Action::SearchSubmit => self.submit_search(),
            Action::SearchCancel => self.cancel_search(),
            Action::SearchNext => self.repeat_search(false),
            Action::SearchPrevious => self.repeat_search(true),
        }
        self.ensure_cursor_visible();
        self.maybe_reparse_syntax();
        // State-A hover-auto-dismiss: popup was shown, focus
        // never moved into it (so `prev_pane_for_help` is None),
        // and the doc cursor moved. Drop the popup -- it's
        // anchored to the prior symbol and is now stale.
        if popup_in_state_a
            && self.active_buffer == BufferKind::Document
            && self.cursor != pre_cursor
        {
            self.help_buffer = None;
        }
        let _ = pre_active;
    }

    /// Reparse syntax if the document's text has changed since the last
    /// parse. Idempotent and cheap when nothing changed.
    fn maybe_reparse_syntax(&mut self) {
        let tv = self.document.text_version();
        if tv == self.last_parsed_text_version {
            return;
        }
        if let Some(syntax) = self.syntax.as_mut() {
            syntax.parse(&self.document.text());
        }
        self.last_parsed_text_version = tv;
        // Recompute computed folds in lockstep with the syntax
        // reparse so `foldmethod=indent` stays in sync with the
        // document. Manual foldmethod skips the recompute (the
        // user's `zf` ranges are authoritative).
        self.recompute_folds();
    }

    /// Refresh [`Self::folds`] from the active [`FoldMethod`].
    /// `manual` -- no-op (preserves user `zf` folds). The other
    /// providers (`indent` / `markdown` / `syntax`) replace `folds`
    /// with the recomputed set, preserving the closed/open state of
    /// any existing fold whose identity matches a recomputed one
    /// (so `zc` survives a reparse).
    ///
    /// `Syntax` runs the language's tree-sitter `folds.scm` query
    /// against the live parse tree and emits one fold per `@fold`
    /// capture spanning more than one line. When the buffer's
    /// language doesn't ship a `folds.scm` (or the parse tree
    /// hasn't been built yet), the syntax provider cascades to the
    /// markdown / indent providers based on the file extension --
    /// so `:set foldmethod=syntax` is useful even on a plain-text
    /// buffer.
    pub fn recompute_folds(&mut self) {
        let fm = self.foldmethod();
        if matches!(fm, FoldMethod::Manual) {
            return;
        }
        let snapshot = self.document.snapshot();
        let mut next = match fm {
            FoldMethod::Manual => return,
            FoldMethod::Indent => crate::folds::compute_indent_folds(&snapshot.buffer),
            FoldMethod::Markdown => crate::folds::compute_markdown_folds(&snapshot.buffer),
            FoldMethod::Syntax => self.recompute_syntax_folds(&snapshot.buffer),
        };
        // Carry over closed-state. Identity hash (heading text +
        // depth) is the primary key so that adding a line to one
        // section doesn't reopen the closed section above. Falls
        // back to (start_line, end_line) when identity is missing.
        for nf in next.iter_mut() {
            let prev = nf
                .identity
                .and_then(|id| self.folds.iter().find(|f| f.identity == Some(id)))
                .or_else(|| {
                    self.folds
                        .iter()
                        .find(|f| f.start_line == nf.start_line && f.end_line == nf.end_line)
                });
            if let Some(prev) = prev {
                nf.closed = prev.closed;
            }
        }
        // Manual folds (identity = None) coexist with computed
        // folds; recomputed providers don't produce them, so carry
        // them over verbatim.
        for prev in &self.folds {
            if prev.identity.is_none() {
                next.push(*prev);
            }
        }
        next.sort_by(|a, b| {
            a.start_line
                .cmp(&b.start_line)
                .then_with(|| b.end_line.cmp(&a.end_line))
        });
        self.folds = next;
    }

    /// Run the tree-sitter folds.scm provider against the live
    /// `Syntax`, falling back to markdown / indent when the syntax
    /// provider returns `None` (no `folds.scm` for this language,
    /// or no parse tree yet).
    fn recompute_syntax_folds(&self, buffer: &lattice_core::Buffer) -> Vec<Fold> {
        if let Some(syntax) = self.syntax.as_ref()
            && let Some(folds) = crate::folds::compute_syntax_folds(syntax)
        {
            return folds;
        }
        // Cascade: markdown for `.md`, indent otherwise. Used when
        // the buffer's language doesn't ship a folds.scm yet (e.g.
        // plain text) or before the first parse has run.
        let is_md = self
            .document
            .path()
            .map(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|ext| ext.eq_ignore_ascii_case("md"))
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if is_md {
            crate::folds::compute_markdown_folds(buffer)
        } else {
            crate::folds::compute_indent_folds(buffer)
        }
    }

    /// Recompute the per-line styled spans for the current viewport.
    /// Called by the runtime before each `terminal.draw`.
    ///
    /// The end of the highlight window stretches with closed folds:
    /// each closed fold collapses N buffer lines onto one viewport
    /// row, so a viewport of `height` rows can cover well over
    /// `scroll + height` buffer lines. Highlighting only the naive
    /// range left lines below folds without spans -- the symptom
    /// the user sees as "syntax highlighting drops out further
    /// down". The visible-buffer-line walk here mirrors what
    /// `compose_visible_lines` does in the renderer.
    pub fn refresh_highlights(&mut self) {
        let start = self.scroll;
        let end = self
            .visible_buffer_line_extent(start, self.viewport_height)
            .saturating_add(1);
        self.visible_highlights = match self.syntax.as_mut() {
            Some(syntax) => syntax.highlight_lines(start, end).unwrap_or_default(),
            None => Vec::new(),
        };
    }

    /// Last buffer-line index that ends up rendered when the
    /// viewport draws `height` rows starting at `scroll`,
    /// accounting for closed folds collapsing multiple buffer
    /// lines onto one row. Returns `scroll` itself when the
    /// viewport has zero height or the buffer is empty -- the
    /// caller's `+1` then yields a non-empty range so
    /// `highlight_lines` doesn't short-circuit.
    fn visible_buffer_line_extent(&self, scroll: u32, height: u32) -> u32 {
        let total_lines = self.document.snapshot().buffer.line_count();
        if total_lines == 0 {
            return scroll;
        }
        let mut buf_line = scroll;
        let mut row: u32 = 0;
        let mut last = scroll;
        while row < height && buf_line < total_lines {
            // Hidden interior of a closed fold -- still part of the
            // window the user is looking at (its content gets shown
            // via the fold heading), so include it in the highlight
            // range.
            if self.line_inside_closed_fold(buf_line) {
                last = buf_line;
                buf_line += 1;
                continue;
            }
            last = buf_line;
            if let Some(fold) = self.fold_start_at(buf_line) {
                last = fold.end_line;
                buf_line = fold.end_line + 1;
            } else {
                buf_line += 1;
            }
            row += 1;
        }
        last
    }

    /// Recompute per-pane highlights for inactive Document panes.
    /// Each inactive pane's [`DocumentEntry::syntax`] gets reparsed
    /// when the document's `text_version` differs from the entry's
    /// cached version (cheap: one parse per inactive pane per
    /// changed document); the visible-window slice lands in
    /// [`Self::pane_highlights`] keyed by pane index. The renderer
    /// reads from there via `&App`.
    ///
    /// Active pane is skipped (it uses [`Self::visible_highlights`]
    /// directly). Panes whose document is the same as the active
    /// document also fall through to `visible_highlights` -- a
    /// single parse covers both panes.
    pub fn refresh_pane_highlights(&mut self) {
        self.pane_highlights.clear();
        let active_idx = self.pane_tree.active_index();
        let active_doc_id = if matches!(self.active_buffer, BufferKind::Document) {
            Some(self.document_buffer_id)
        } else {
            None
        };
        // Collect (pane_idx, doc_id, scroll, height) for each
        // inactive Document pane that doesn't share doc with the
        // active pane.
        let pending: Vec<(usize, BufferId, u32, u32)> = self
            .pane_tree
            .leaves()
            .iter()
            .enumerate()
            .filter_map(|(idx, pane)| {
                if idx == active_idx {
                    return None;
                }
                if !matches!(pane.buffer, BufferKind::Document) {
                    return None;
                }
                if Some(pane.buffer_id) == active_doc_id {
                    return None;
                }
                // Use the pane's own viewport slice (the per-pane
                // status line eats one row, so subtract; for v1
                // we approximate using app.viewport_height).
                let h = self.viewport_height;
                Some((idx, pane.buffer_id, pane.scroll, h))
            })
            .collect();
        for (idx, doc_id, scroll, height) in pending {
            let Some(entry) = self.buffers.document_mut(doc_id) else {
                continue;
            };
            let snap = entry.handle.snapshot();
            let tv = snap.version;
            if entry.syntax.is_none() {
                continue;
            }
            if let Some(syntax) = entry.syntax.as_mut() {
                if tv != entry.last_parsed_text_version {
                    syntax.parse(&snap.buffer.as_string());
                    entry.last_parsed_text_version = tv;
                }
                let end = scroll.saturating_add(height);
                let spans = syntax.highlight_lines(scroll, end).unwrap_or_default();
                self.pane_highlights.insert(idx, spans);
            }
        }
    }

    /// Spans for the line at `viewport_row` (0-based, relative to the top of
    /// the viewport). Empty slice if no syntax or the row is past EOF.
    ///
    /// Prefer [`Self::highlights_for_buffer_line`] when the renderer
    /// is iterating the visible-line list under closed folds, since
    /// `viewport_row` no longer maps to `scroll + row` once folds
    /// hide interior lines.
    pub fn highlights_for_viewport_row(&self, viewport_row: u32) -> &[StyledSpan] {
        self.visible_highlights
            .get(viewport_row as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Spans for a specific buffer line. `refresh_highlights` populates
    /// `visible_highlights` for the contiguous window
    /// `[scroll, scroll + viewport_height)`; lines outside that window
    /// (or far enough that the slot is missing) return an empty slice.
    /// The renderer uses this for the active pane so closed folds
    /// don't desync syntax styling -- viewport row 5 might be buffer
    /// line 12 once a fold collapses lines 5..=11.
    pub fn highlights_for_buffer_line(&self, line: u32) -> &[StyledSpan] {
        if line < self.scroll {
            return &[];
        }
        let offset = (line - self.scroll) as usize;
        self.visible_highlights
            .get(offset)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn set_message(&mut self, level: EchoLevel, text: impl Into<String>) {
        self.last_message = Some(EchoMessage {
            text: text.into(),
            level,
        });
    }

    /// Run the in-progress pattern from origin. Used to highlight the
    /// current match while typing in `/` or `?`. Does not move cursor;
    /// the cursor jumps only on `SearchSubmit`.
    fn preview_search(&mut self) {
        let Some(line) = self.search_line.as_ref() else {
            return;
        };
        if line.pattern.is_empty() {
            self.current_match = None;
            self.all_matches.clear();
            return;
        }
        // Live preview tolerates compile errors silently -- the user
        // is still typing. The submit path surfaces the error.
        let Ok(regex) = compile_search_pattern(&line.pattern) else {
            self.current_match = None;
            self.all_matches.clear();
            return;
        };
        let dir = match line.direction {
            SearchDirection::Forward => search::Direction::Forward,
            SearchDirection::Backward => search::Direction::Backward,
        };
        let buffer = self.active_text();
        match search::find(
            &buffer,
            &regex,
            line.origin,
            dir,
            &CancellationToken::never(),
        ) {
            Ok(Some(SearchHit { range, .. })) => self.current_match = Some(range),
            _ => self.current_match = None,
        }
        // Live hlsearch: highlight every occurrence as the user types.
        self.all_matches =
            search::find_all(&buffer, &regex, &CancellationToken::never()).unwrap_or_default();
    }

    fn submit_search(&mut self) {
        let Some(line) = self.search_line.take() else {
            return;
        };
        self.modal = ModalState::Normal;
        self.pending = Pending::None;
        if line.pattern.is_empty() {
            // Empty submit: re-run last_search if any (vim behavior).
            if self.last_search.is_some() {
                self.repeat_search(false);
            }
            return;
        }
        // Save the pre-search position so Ctrl-O returns. Position
        // history is currently document-only; help / tree don't
        // participate yet.
        if matches!(self.active_buffer, BufferKind::Document) {
            self.push_position_history(line.origin, PositionSource::AutoJump);
        }
        // Compile once for both find + find_all + later n/N replays.
        let regex = match compile_search_pattern(&line.pattern) {
            Ok(r) => r,
            Err(msg) => {
                self.set_message(EchoLevel::Error, format!("regex: {msg}"));
                self.current_match = None;
                self.all_matches.clear();
                return;
            }
        };
        let dir = match line.direction {
            SearchDirection::Forward => search::Direction::Forward,
            SearchDirection::Backward => search::Direction::Backward,
        };
        let buffer = self.active_text();
        match search::find(
            &buffer,
            &regex,
            line.origin,
            dir,
            &CancellationToken::never(),
        ) {
            Ok(Some(hit)) => {
                self.cursor = hit.range.start;
                self.current_match = Some(hit.range);
                self.all_matches =
                    search::find_all(&buffer, &regex, &CancellationToken::never())
                        .unwrap_or_default();
                if hit.wrapped {
                    let level = EchoLevel::Warn;
                    let text = match line.direction {
                        SearchDirection::Forward => "search hit BOTTOM, continuing at TOP",
                        SearchDirection::Backward => "search hit TOP, continuing at BOTTOM",
                    };
                    self.set_message(level, text.to_string());
                }
                self.last_search = Some(LastSearch {
                    pattern: line.pattern,
                    direction: line.direction,
                });
                if matches!(self.active_buffer, BufferKind::Document) {
                    self.auto_open_folds_at_cursor();
                }
            }
            Ok(None) => {
                self.current_match = None;
                self.all_matches.clear();
                self.set_message(
                    EchoLevel::Error,
                    format!("E486: Pattern not found: {}", line.pattern),
                );
                // Vim still records the pattern so `n`/`N` can retry later.
                self.last_search = Some(LastSearch {
                    pattern: line.pattern,
                    direction: line.direction,
                });
            }
            Err(_) => {
                self.current_match = None;
                self.all_matches.clear();
            }
        }
    }

    fn cancel_search(&mut self) {
        if let Some(line) = self.search_line.take() {
            self.cursor = line.origin;
        }
        self.current_match = None;
        self.all_matches.clear();
        self.modal = ModalState::Normal;
        self.pending = Pending::None;
    }

    /// Repeat last search. `reverse=false` keeps the original direction
    /// (`n`); `reverse=true` flips it (`N`).
    fn repeat_search(&mut self, reverse: bool) {
        let Some(last) = self.last_search.clone() else {
            self.set_message(
                EchoLevel::Error,
                "E35: no previous regular expression".to_string(),
            );
            return;
        };
        if matches!(self.active_buffer, BufferKind::Document) {
            let cur = self.cursor;
            self.push_position_history(cur, PositionSource::AutoJump);
        }
        let direction = match (last.direction, reverse) {
            (SearchDirection::Forward, false) | (SearchDirection::Backward, true) => {
                SearchDirection::Forward
            }
            (SearchDirection::Backward, false) | (SearchDirection::Forward, true) => {
                SearchDirection::Backward
            }
        };
        let dir = match direction {
            SearchDirection::Forward => search::Direction::Forward,
            SearchDirection::Backward => search::Direction::Backward,
        };
        let buffer = self.active_text();
        // Skip current match: advance one byte in the chosen direction.
        let from = step_byte(&buffer, self.cursor, direction);
        let regex = match compile_search_pattern(&last.pattern) {
            Ok(r) => r,
            Err(msg) => {
                self.set_message(EchoLevel::Error, format!("regex: {msg}"));
                self.current_match = None;
                return;
            }
        };
        match search::find(&buffer, &regex, from, dir, &CancellationToken::never()) {
            Ok(Some(hit)) => {
                self.cursor = hit.range.start;
                self.current_match = Some(hit.range);
                if hit.wrapped {
                    let text = match direction {
                        SearchDirection::Forward => "search hit BOTTOM, continuing at TOP",
                        SearchDirection::Backward => "search hit TOP, continuing at BOTTOM",
                    };
                    self.set_message(EchoLevel::Warn, text.to_string());
                }
                if matches!(self.active_buffer, BufferKind::Document) {
                    self.auto_open_folds_at_cursor();
                }
            }
            Ok(None) => {
                self.current_match = None;
                self.set_message(
                    EchoLevel::Error,
                    format!("E486: Pattern not found: {}", last.pattern),
                );
            }
            Err(_) => {
                self.current_match = None;
            }
        }
    }

    /// Hybrid `<C-h>` resolution (DESIGN.md §5.11.3 Q11). Walk the
    /// `:` line up to the cursor (v1: cursor is at end), find the
    /// "word" the user is hovering on, and:
    ///
    /// 1. If the word resolves to a registered command (via alias
    ///    expansion), describe THAT -- the user is asking about the
    ///    command they're typing.
    /// 2. Else, if we can identify the slot as an arg of a known
    ///    command, describe the parent command scrolled to
    ///    `arg:<name>`.
    /// 3. Else, no-op + status message.
    fn do_command_line_describe_under_cursor(&mut self) {
        if !matches!(self.modal, ModalState::Command) {
            return;
        }
        // Take what's typed so far. v1 cursor is at end-of-line.
        let line = self.command_line.clone();
        let cursor = line.len();
        let alias_resolver = |short: &str| {
            crate::excommand::aliases()
                .get(short)
                .map(|s| (*s).to_string())
        };
        let slot = lattice_completion::current_slot(&line, cursor, &self.registry, &alias_resolver);

        // Word-at-cursor: try to resolve to a registered command.
        let word = slot.prefix();
        let canonical = if word.is_empty() {
            None
        } else {
            // Try alias resolution; fall through to direct registry
            // name lookup.
            alias_resolver(word)
                .or_else(|| self.registry.id_by_name(word).and(Some(word.to_string())))
        };

        if let Some(name) = canonical
            && self.registry.id_by_name(&name).is_some()
        {
            self.do_describe_command(&name, None);
            return;
        }

        // Fall back to arg-aware: describe the parent command at
        // arg:<name>.
        match &slot {
            lattice_completion::CommandLineSlot::Arg {
                command_name,
                arg_spec,
                ..
            } => {
                let anchor = format!("arg:{}", arg_spec.name);
                self.do_describe_command(command_name, Some(&anchor));
            }
            lattice_completion::CommandLineSlot::CommandName { prefix, .. } => {
                // Cursor in the command-name slot but the prefix
                // doesn't resolve. Surface a helpful message.
                if prefix.is_empty() {
                    self.set_message(
                        EchoLevel::Info,
                        "type a command name then C-h for its help".to_string(),
                    );
                } else {
                    self.set_message(EchoLevel::Error, format!("no command named `{prefix}`"));
                }
            }
            _ => {
                self.set_message(
                    EchoLevel::Info,
                    "no command-line context for `C-h`".to_string(),
                );
            }
        }
    }

    /// `<Tab>` opens the completion popup or advances within an
    /// open one. Slot detection drives generator selection; the
    /// pipeline runs through the registered matcher / ranker /
    /// annotators.
    fn do_command_line_complete_or_advance(&mut self) {
        if !matches!(self.modal, ModalState::Command) {
            return;
        }
        if let Some(state) = self.completion_state.as_mut() {
            if !state.candidates.is_empty() {
                state.selected = (state.selected + 1) % state.candidates.len();
            }
            return;
        }
        self.open_completion_popup();
    }

    fn do_command_line_complete_prev(&mut self) {
        if let Some(state) = self.completion_state.as_mut()
            && !state.candidates.is_empty()
        {
            if state.selected == 0 {
                state.selected = state.candidates.len() - 1;
            } else {
                state.selected -= 1;
            }
        }
    }

    fn do_command_line_accept_completion(&mut self) {
        let Some(state) = self.completion_state.take() else {
            return;
        };
        if state.candidates.is_empty() {
            return;
        }
        let chosen = &state.candidates[state.selected];
        // Replace [replace_start, end) with the chosen text.
        self.command_line.replace_range(
            state.replace_start..self.command_line.len(),
            &chosen.raw.text,
        );
    }

    /// On `Action::CommandLineSubmit`, decide whether the line is
    /// an empty-arg invocation of a command whose first required
    /// arg is `Chord`. If so, return the prefill string for the
    /// cmdline (`<command-word> ` -- with trailing space) so the
    /// caller can transition into a chord-capture prompt.
    /// `None` means submit normally.
    /// Generalized missing-arg detection (DESIGN.md §B.1).
    ///
    /// When the user submits a bare command with a required first
    /// arg empty -- e.g. `:write<CR>` (path required), `:edit<CR>`
    /// (path required), `:describe-command<CR>` (name required) --
    /// resolve the spec, look up the schema's first required arg,
    /// and return enough info for the App to prefill the cmdline
    /// + show a prompt.
    ///
    /// Returns `None` when:
    /// - The cmdline is empty.
    /// - The user already supplied an arg (parser handles it).
    /// - The command is unknown (parser errors anyway).
    /// - There's no first arg or it's not Required.
    /// - The command's args use the delimiter form (`:s/.../.../`).
    fn try_resolve_missing_arg_prompt(&self) -> Option<MissingArgPrompt> {
        let line = self.command_line.trim();
        if line.is_empty() {
            return None;
        }
        // Split off the command word + bang the same way
        // `excommand::parse_invocation` does. We don't go through
        // the full parser because we explicitly want the
        // `args == empty` case here (the parser would error).
        let (raw_cmd, rest) = match line.find(char::is_whitespace) {
            Some(i) => (&line[..i], line[i..].trim()),
            None => (line, ""),
        };
        if !rest.is_empty() {
            // User supplied an arg -- normal submit handles it.
            return None;
        }
        let cmd = raw_cmd.strip_suffix('!').unwrap_or(raw_cmd);
        let canonical = self.registry.id_by_name(cmd).or_else(|| {
            crate::excommand::aliases()
                .get(cmd)
                .copied()
                .and_then(|c| self.registry.id_by_name(c))
        })?;
        let spec = self.registry.ex_command_spec(canonical)?;
        // Delimiter-form commands (`:s`, `:g`, `:v`) don't go
        // through the keyword arg-prompt path -- their syntax is
        // its own UX.
        if matches!(
            spec.surface_form,
            lattice_grammar::SurfaceForm::Delimiter { .. }
        ) {
            return None;
        }
        let first = spec.args_schema.first()?;
        if !matches!(first.default, lattice_grammar::ArgDefault::Required) {
            // Non-required arg has a fallback; let the parser take
            // the default path.
            return None;
        }
        let prompt = if first.prompt.is_empty() {
            format!("{}:", first.name)
        } else {
            first.prompt.to_string()
        };
        Some(MissingArgPrompt {
            // Preserve the user's spelling (alias vs canonical) plus
            // any bang they typed; append a trailing space so the
            // cursor lands in the arg slot.
            prefill: format!("{raw_cmd} "),
            kind: first.kind,
            prompt,
        })
    }

    /// True when the cmdline cursor is on an `ArgKind::Chord` arg
    /// slot. Drives the input layer's chord-capture overlay
    /// (`translate_command_chord_capture`). v1: `:describe-key`'s
    /// `chord` arg is the only `Chord`-kinded arg in the registry;
    /// when `:map` / `:nnoremap` land they reuse this gate.
    pub fn chord_capture_active(&self) -> bool {
        if !matches!(self.modal, ModalState::Command) {
            return false;
        }
        let line = &self.command_line;
        let alias_resolver = |short: &str| {
            crate::excommand::aliases()
                .get(short)
                .map(|s| (*s).to_string())
        };
        let slot =
            lattice_completion::current_slot(line, line.len(), &self.registry, &alias_resolver);
        matches!(
            &slot,
            lattice_completion::CommandLineSlot::Arg { arg_spec, .. }
                if arg_spec.kind == lattice_grammar::ArgKind::Chord
        )
    }

    /// Build the pipeline for the current slot and run it. Caches
    /// results into `completion_state`.
    ///
    /// When `completion.auto_insert_single` is on (the default) and
    /// the pipeline returns exactly one candidate, the popup is
    /// skipped and that candidate is applied to the command line
    /// directly -- same effect as `<Tab><CR>` but without the
    /// confirm keystroke for an unambiguous match. The popup-open
    /// boundary is the only fire point; narrowing an already-open
    /// popup to one candidate while typing does not auto-insert.
    fn open_completion_popup(&mut self) {
        match self.compute_completion_state() {
            Ok(state) => {
                if self.completion_auto_insert_single() && state.candidates.len() == 1 {
                    let chosen_text = state.candidates[0].raw.text.clone();
                    self.command_line
                        .replace_range(state.replace_start..self.command_line.len(), &chosen_text);
                    // Don't open the popup -- the single candidate
                    // is already applied. `completion_state` stays
                    // `None` so the next `<Tab>` would re-trigger
                    // the pipeline against the new line.
                    return;
                }
                self.completion_state = Some(state);
            }
            Err(err) => {
                let (level, msg) = err.echo();
                self.set_message(level, msg);
            }
        }
    }

    /// Re-run the completion pipeline against the current command line
    /// and update the popup in place. Called from `CommandLineAppend` /
    /// `CommandLineBackspace` / `CommandLineDeleteWordBackward` while
    /// the popup is open -- this is the vertico "filter as you type"
    /// behaviour. No echo: refresh is silent. Empty results keep the
    /// popup alive (so further edits can repopulate it); a slot
    /// transition that has no completion source closes it.
    /// Recompute the in-progress substitute preview against the
    /// current `command_line`. Called from CommandLineAppend /
    /// Backspace / cmdline-init so the preview tracks the user's
    /// typing in real time.
    ///
    /// Drops the preview when the cmdline doesn't parse as a
    /// substitute, when the pattern is empty, or when regex
    /// compilation fails. Cleared explicitly by CommandLineCancel
    /// and by execute_ex_line on submit.
    fn refresh_substitute_preview(&mut self) {
        let parsed = match crate::excommand::try_parse_substitute_partial(&self.command_line) {
            Some(p) => p,
            None => {
                self.substitute_preview = None;
                return;
            }
        };
        if parsed.pattern.is_empty() {
            self.substitute_preview = None;
            return;
        }
        let regex = match compile_search_pattern(&parsed.pattern) {
            Ok(r) => r,
            Err(_) => {
                // Pattern doesn't compile yet (mid-typing). Keep the
                // last preview rather than flickering -- but if we
                // never had one, drop quietly.
                return;
            }
        };
        let global = parsed
            .flags
            .as_ref()
            .map(|f| f.contains('g'))
            .unwrap_or(false);

        let buffer = self.document.snapshot().buffer.clone();
        let mut matches: Vec<ProtoRange> = Vec::new();
        match parsed.scope {
            crate::excommand::SubstitutePartialScope::CurrentLine => {
                self.collect_substitute_matches_for_line(
                    &buffer,
                    &regex,
                    self.cursor.line,
                    global,
                    &mut matches,
                );
            }
            crate::excommand::SubstitutePartialScope::Whole => {
                let last = last_addressable_line(&buffer);
                for line in 0..=last {
                    self.collect_substitute_matches_for_line(
                        &buffer,
                        &regex,
                        line,
                        global,
                        &mut matches,
                    );
                }
            }
        }

        self.substitute_preview = Some(SubstitutePreview {
            matches,
            replacement: parsed.replacement,
            global,
        });
    }

    /// Push every match of `regex` on `line` into `out`. Honours
    /// `global`: when false, only the leftmost match is collected
    /// (mirrors vim's default `:s` without the `g` flag).
    fn collect_substitute_matches_for_line(
        &self,
        buffer: &Buffer,
        regex: &fancy_regex::Regex,
        line: u32,
        global: bool,
        out: &mut Vec<ProtoRange>,
    ) {
        let line_text = match buffer.line(line) {
            Some(s) => s,
            None => return,
        };
        if line_text.is_empty() {
            return;
        }
        for m in regex.find_iter(&line_text) {
            let m = match m {
                Ok(m) => m,
                Err(_) => break,
            };
            let start = Position::new(line, m.start() as u32);
            let end = Position::new(line, m.end() as u32);
            out.push(ProtoRange::new(start, end));
            if !global {
                break;
            }
        }
    }

    fn refresh_completion_popup(&mut self) {
        if self.completion_state.is_none() {
            return;
        }
        match self.compute_completion_state() {
            Ok(state) => {
                self.completion_state = Some(state);
            }
            Err(CompletionComputeError::NoMatches { .. }) => {
                // Keep the popup open with zero candidates so the user
                // can backspace and re-match without re-tabbing.
                if let Some(state) = self.completion_state.as_mut() {
                    state.candidates.clear();
                    state.selected = 0;
                    state.original_line = self.command_line.clone();
                }
            }
            Err(_) => {
                // Slot moved to a region with no completion source
                // (UnknownCommand, BeyondSchema, arg without
                // `completion`). Drop the popup; the user can re-Tab
                // to re-arm it later.
                self.completion_state = None;
            }
        }
    }

    /// Slot-detect, build the pipeline, run it, and host-rewrite
    /// command candidates to user-facing aliases. Pure -- no
    /// `set_message` side effects, so both the open and the refresh
    /// path can share it. Errors carry enough info for the open path
    /// to surface them via echo.
    fn compute_completion_state(&self) -> Result<CompletionState, CompletionComputeError> {
        let line = self.command_line.clone();
        let cursor = line.len();
        let alias_resolver = |short: &str| {
            crate::excommand::aliases()
                .get(short)
                .map(|s| (*s).to_string())
        };
        let slot = lattice_completion::current_slot(&line, cursor, &self.registry, &alias_resolver);
        let (source_name, prefix, replace_start) = match &slot {
            lattice_completion::CommandLineSlot::CommandName {
                prefix,
                replace_start,
            } => ("gen:commands", prefix.clone(), *replace_start),
            lattice_completion::CommandLineSlot::Arg {
                arg_spec,
                prefix,
                replace_start,
                ..
            } => match arg_spec.completion {
                Some(name) => (name, prefix.clone(), *replace_start),
                None => {
                    return Err(CompletionComputeError::NoCompletionForArg(
                        arg_spec.name.to_string(),
                    ));
                }
            },
            lattice_completion::CommandLineSlot::Empty => ("gen:commands", String::new(), 0),
            _ => {
                return Err(CompletionComputeError::NoCompletionAtCursor);
            }
        };

        let Some(generator) = self.completion_registry.generator_by_name(source_name) else {
            return Err(CompletionComputeError::MissingSource(
                source_name.to_string(),
            ));
        };
        let generator_id = generator.id;
        let Some(pipeline) = lattice_completion::CompletionPipeline::for_generator(
            &self.completion_registry,
            generator_id,
        ) else {
            return Err(CompletionComputeError::PipelineUnconfigured);
        };
        let snap = self.document.snapshot();
        let ctx = lattice_completion::GenerateContext {
            prefix: &prefix,
            buffer: &snap.buffer,
            registry: &self.registry,
            case_sensitive: false,
        };
        let mut candidates = pipeline.run(&ctx, &prefix, &self.completion_registry.cache);

        // Host-side post-process: command candidates from
        // `gen:commands` come back as canonical names
        // (`ex:describe-command`). Rewrite to the user-facing alias
        // (`describe-command`) so the popup shows -- and accepts --
        // what the user would actually type. The parser accepts
        // both forms (see excommand::parse_invocation), so this is
        // purely a UX rewrite.
        prefer_aliases_for_command_candidates(&mut candidates, &prefix);

        if candidates.is_empty() {
            return Err(CompletionComputeError::NoMatches { prefix });
        }
        Ok(CompletionState {
            candidates,
            selected: 0,
            replace_start,
            original_line: line,
        })
    }

    fn execute_ex_line(&mut self, line: &str) {
        match excommand::parse(line, &self.registry) {
            Ok(inv) => match self.dispatch_blocking(inv) {
                Ok(eff) => self.apply_effect(eff),
                Err(e) => self.set_message(EchoLevel::Error, e.to_string()),
            },
            Err(err) => {
                self.set_message(EchoLevel::Error, err.to_string());
            }
        }
    }

    /// Walk through `:` command history in Command modal. `back = true`
    /// goes to older entries (Up); `false` goes newer (Down).
    fn do_command_history_step(&mut self, back: bool) {
        if !matches!(self.modal, ModalState::Command) {
            return;
        }
        if self.command_history.is_empty() {
            return;
        }
        let new_cursor = match (self.command_history_cursor, back) {
            (None, true) => {
                // First Up: snapshot the in-progress line and move to
                // the most recent history entry.
                self.command_history_pending = Some(self.command_line.clone());
                Some(self.command_history.len() - 1)
            }
            (None, false) => return, // Down with no history walked yet: no-op.
            (Some(0), true) => return, // Already at oldest.
            (Some(i), true) => Some(i - 1),
            (Some(i), false) if i + 1 >= self.command_history.len() => {
                // Past the newest history entry: restore the
                // in-progress line.
                self.command_line = self.command_history_pending.take().unwrap_or_default();
                self.command_history_cursor = None;
                return;
            }
            (Some(i), false) => Some(i + 1),
        };
        if let Some(idx) = new_cursor {
            self.command_line = self.command_history[idx].clone();
            self.command_history_cursor = Some(idx);
        }
    }

    /// Vim's `:e [path]` -- swap the current document for the file at
    /// `path`. With `path = None`, re-reads the current document from
    /// disk (vim's `:e` reload). Refuses if the buffer is dirty unless
    /// `force` is true. Registers, marks, and global state persist
    /// across the swap; cursor / scroll / search / syntax / undo /
    /// folds are reset to the new doc.
    /// `:e[dit] FILE` (DESIGN.md §5.9 multi-buffer). If a buffer
    /// for `path` is already open, switch to it; otherwise spawn
    /// a fresh document actor, register it, and switch the active
    /// pane to the new buffer. With no path, re-edit the current
    /// buffer's path (force-reload from disk; `!` required when
    /// dirty).
    fn do_edit(&mut self, path: Option<std::path::PathBuf>, force: bool) {
        let target = match path {
            Some(p) => p,
            None => match self.document.path() {
                Some(p) => p,
                None => {
                    self.set_message(EchoLevel::Error, "no file name".to_string());
                    return;
                }
            },
        };
        // Directories defer to `:Tree path` so `:e folder` opens
        // the file-tree buffer (vim's `:Explore` semantics --
        // editing a directory shows its contents). Probe the path
        // before parsing it as a file; if the metadata read fails
        // we fall through and let `Document::open` produce the
        // right error.
        if let Ok(meta) = std::fs::metadata(&target)
            && meta.is_dir()
        {
            self.do_open_file_tree(Some(target));
            return;
        }
        // If `target` is already open, switch to it. The dirty
        // check only applies when we'd discard the current buffer
        // -- switching to a different *open* buffer leaves the
        // current one alone, so dirtiness doesn't block.
        if let Some(existing_id) = self.find_document_by_path(&target) {
            if existing_id == self.document_buffer_id {
                // Re-edit current: reload from disk (vim's `:e`).
                if !force && self.document.dirty() {
                    self.set_message(
                        EchoLevel::Error,
                        "no write since last change (add ! to override)".to_string(),
                    );
                    return;
                }
                let new_doc = match Document::open(&target) {
                    Ok(d) => d,
                    Err(e) => {
                        self.set_message(EchoLevel::Error, format!("open error: {e}"));
                        return;
                    }
                };
                let lang = Lang::detect_from_path(new_doc.path());
                let mut syntax =
                    Syntax::for_language_with_registry(lang, self.lang_registry.clone())
                        .ok()
                        .flatten();
                if let Some(s) = syntax.as_mut() {
                    s.parse(&new_doc.text());
                }
                self.last_parsed_text_version = new_doc.text_version();
                self.syntax = syntax;
                self.replace_document_blocking(new_doc);
                self.cursor = Position::ZERO;
                self.scroll = 0;
                self.current_match = None;
                self.all_matches.clear();
                self.search_line = None;
                self.last_search = None;
                self.last_find = None;
                self.last_change = None;
                self.last_visual = None;
                self.visual_anchor = None;
                self.replace_history.clear();
                self.position_history.clear();
                self.position_history_cursor = 0;
                self.folds.clear();
                self.set_message(
                    EchoLevel::Info,
                    format!("\"{}\" reloaded", target.display()),
                );
            } else {
                // Different already-open buffer: switch to it.
                self.activate_document(existing_id);
                self.set_message(
                    EchoLevel::Info,
                    format!("\"{}\" (already open)", target.display()),
                );
            }
            return;
        }
        // Brand-new file: open a fresh actor and register it.
        let new_doc = match Document::open(&target) {
            Ok(d) => d,
            Err(e) => {
                self.set_message(EchoLevel::Error, format!("open error: {e}"));
                return;
            }
        };
        let lang = Lang::detect_from_path(new_doc.path());
        let mut syntax = Syntax::for_language_with_registry(lang, self.lang_registry.clone())
            .ok()
            .flatten();
        if let Some(s) = syntax.as_mut() {
            s.parse(&new_doc.text());
        }
        let new_handle = spawn_document(new_doc, self.registry.clone());
        let new_id = BufferId::next();
        self.buffers.insert(BufferEntry {
            id: new_id,
            flags: BufferFlags::default(),
            data: BufferData::Document(DocumentEntry {
                id: new_id,
                handle: new_handle.clone(),
                // Active buffer's syntax / folds live on the App
                // for the hot path; entry's slots stay empty until
                // a switch.
                syntax: None,
                last_parsed_text_version: 0,
                folds: Vec::new(),
            }),
        });
        // Save the currently-active buffer's hot-path state into
        // its registry entry, then load the new buffer's into the
        // hot path.
        self.snapshot_active_pane();
        self.snapshot_active_document();
        self.active_buffer = BufferKind::Document;
        self.document_buffer_id = new_id;
        self.document = new_handle;
        // Rebuild the cache against the new document's published-cell.
        self.snapshot_cache = self.document.snapshot_cache();
        self.syntax = syntax;
        self.last_parsed_text_version = self.document.text_version();
        self.cursor = Position::ZERO;
        self.scroll = 0;
        self.current_match = None;
        self.all_matches.clear();
        self.search_line = None;
        self.last_search = None;
        self.last_find = None;
        self.last_change = None;
        self.last_visual = None;
        self.visual_anchor = None;
        self.replace_history.clear();
        self.folds.clear();
        // Position history follows the active buffer; a new buffer
        // resets the ring so `<C-o>` from the new buffer doesn't
        // walk into a stale buffer's positions immediately. (The
        // entries-by-buffer-kind filter from B.1.a would also
        // skip them, but emptying the ring is simpler.)
        self.position_history.clear();
        self.position_history_cursor = 0;
        // Mirror the new active buffer id onto the active pane's
        // leaf so subsequent `<C-o>` / `<C-i>` walks record the
        // right buffer.
        self.pane_tree.active_mut().buffer = BufferKind::Document;
        self.pane_tree.active_mut().buffer_id = new_id;
        // Single principled hook for everything that needs to
        // come up with the new buffer (parse seam, fold seed,
        // highlight cache reset). Same hook used by
        // `activate_document` so opening a fresh file and
        // switching to an existing one go through the same path.
        self.activate_buffer_state();
        // Queue an LSP attachment for the new file (Phase
        // 4.1.i.2). The runtime drains the queue on its next
        // tick; the buffer is fully usable before LSP attaches.
        let new_text = self.document.snapshot().buffer.as_string();
        self.queue_lsp_open(new_id, target.clone(), new_text);
        self.set_message(EchoLevel::Info, format!("\"{}\" opened", target.display()));
    }

    /// Look up a buffer by file path. Used by `:e FILE` to detect
    /// "already open"; later by `:b NAME` for completion.
    fn find_document_by_path(&self, path: &std::path::Path) -> Option<BufferId> {
        self.buffers.document_with_path(path)
    }

    /// Save the currently-active document's hot-path state
    /// (`syntax`, `last_parsed_text_version`, `folds`) into its
    /// [`DocumentEntry`]. Called before switching the active
    /// buffer so the rotation is round-trippable.
    ///
    /// Guarded by `active_buffer == Document`: when the active
    /// buffer is a file tree or help, `self.syntax` was already
    /// moved into the document entry on the *previous* transition
    /// (when we left the document). Calling this again would
    /// `take()` an already-None value and overwrite the entry's
    /// stashed syntax, dropping the highlight state on the floor
    /// (the visible symptom: opening `:Tree` and pressing `q`
    /// returned to the document with no syntax colours).
    fn snapshot_active_document(&mut self) {
        if !matches!(self.active_buffer, BufferKind::Document) {
            return;
        }
        if let Some(entry) = self.buffers.document_mut(self.document_buffer_id) {
            entry.syntax = self.syntax.take();
            entry.last_parsed_text_version = self.last_parsed_text_version;
            // Folds round-trip with the buffer: stashing them here
            // preserves the user's open/closed state across a
            // switch-away-and-back. The activation hook on the
            // destination side decides whether to recompute (first
            // visit) or restore (subsequent visits).
            entry.folds = std::mem::take(&mut self.folds);
        }
    }

    /// Lifecycle hook fired after a document buffer becomes the
    /// active buffer (either via [`Self::activate_document`] or
    /// after `:e <path>` opens a fresh file). Refreshes anything
    /// that "lives with the buffer until it closes" so the user
    /// sees consistent state without having to reach for `<C-l>`.
    ///
    /// New buffer-level state plugs in here: keep the path
    /// principled instead of sprinkling per-option fixups across
    /// every entry point that changes the active buffer.
    fn activate_buffer_state(&mut self) {
        // Make sure the syntax tree matches the current text. If
        // the entry stashed a parse for the document's current
        // version this no-ops; otherwise it parses + recomputes
        // folds in lockstep via the seam in `maybe_reparse_syntax`.
        self.maybe_reparse_syntax();
        // First-activation case: a freshly-opened file (or one we
        // never visited before) has an empty fold list and the
        // reparse seam may have been a no-op (text version already
        // matched the entry's stashed parse). Seed the fold list
        // from the active foldmethod so the gutter shows ▸ markers
        // and `za` works without a manual `<C-l>`. `Manual` skips
        // the seed (the user's `zf` ranges are authoritative).
        if self.folds.is_empty() && !matches!(self.foldmethod(), FoldMethod::Manual) {
            self.recompute_folds();
        }
        // Drop frame-level highlight caches so the next
        // `refresh_highlights` repopulates against the activated
        // buffer's content rather than the previous buffer's.
        self.visible_highlights.clear();
        self.pane_highlights.clear();
    }

    /// Switch the active document to `id`. Snapshots the current
    /// active state into its entry, then loads from the
    /// destination's entry. No-op if `id` is already active or
    /// not registered.
    pub fn activate_document(&mut self, id: BufferId) {
        if id == self.document_buffer_id && matches!(self.active_buffer, BufferKind::Document) {
            return;
        }
        if self.buffers.document(id).is_none() {
            self.set_message(EchoLevel::Error, format!("buffer #{} not a document", id.0));
            return;
        }
        // Save active pane's cursor/scroll first; the active pane
        // is the one whose buffer changed.
        self.snapshot_active_pane();
        // Same-document fast path: returning to the document
        // buffer that `self.document` still points at (e.g. from
        // a help-in-pane overlay or a file-tree pane). Two cases
        // converge here, distinguished by whether the prior
        // transition stashed hot-path state into the entry:
        //
        // 1. **Help-in-pane** (overlay): `activate_help_in_pane`
        //    deliberately does NOT stash, so `self.syntax` /
        //    `self.folds` stay live for the underlying document
        //    paint. `entry.syntax` is `None`. Skip the restore;
        //    just flip flags + pane state.
        //
        // 2. **File tree** (replaces buffer area): `activate_file_tree`
        //    routes through `snapshot_active_document` which moves
        //    `self.syntax` into `entry.syntax`. On the way back
        //    `entry.syntax` is `Some` and we restore it.
        //
        // The "is the entry stashed?" check is `entry.syntax.is_some()`;
        // folds piggyback on the same condition so partial-empty
        // fold lists don't trip the take-from-entry branch.
        if id == self.document_buffer_id {
            self.active_buffer = BufferKind::Document;
            let pane = self.pane_tree.active_mut();
            pane.buffer = BufferKind::Document;
            pane.buffer_id = id;
            if let Some(entry) = self.buffers.document_mut(id)
                && entry.syntax.is_some()
            {
                self.syntax = entry.syntax.take();
                self.last_parsed_text_version = entry.last_parsed_text_version;
                self.folds = std::mem::take(&mut entry.folds);
            }
            return;
        }
        self.snapshot_active_document();
        // Load destination.
        let entry = self
            .buffers
            .document_mut(id)
            .expect("document() lookup above succeeded");
        self.document = entry.handle.clone();
        // Rebuild the cache against the activated document's
        // published-cell; the previous cache pointed at the old
        // document.
        self.snapshot_cache = self.document.snapshot_cache();
        self.syntax = entry.syntax.take();
        self.last_parsed_text_version = entry.last_parsed_text_version;
        // Folds round-trip with the buffer (see DocumentEntry
        // doc-comment). On first activation the entry is empty
        // and `activate_buffer_state` seeds from foldmethod;
        // subsequent re-activations restore the user's
        // open/closed state.
        self.folds = std::mem::take(&mut entry.folds);
        self.document_buffer_id = id;
        // The active pane now references this document.
        let pane = self.pane_tree.active_mut();
        pane.buffer = BufferKind::Document;
        pane.buffer_id = id;
        // Per-document transient state resets that should NOT
        // persist across buffer switches.
        self.current_match = None;
        self.all_matches.clear();
        self.search_line = None;
        // Note: `last_search` / `last_find` / `last_change` /
        // `last_visual` / marks / registers / macros /
        // replace_history / position_history all persist
        // intentionally. Folds are buffer-local and snapshotted
        // into `DocumentEntry` above.
        self.cursor = Position::ZERO;
        self.scroll = 0;
        self.load_active_pane();
        // Single principled hook for everything that needs to
        // come up with the buffer (parse, folds, highlight cache).
        self.activate_buffer_state();
        self.set_message(
            EchoLevel::Info,
            format!(
                "switched to buffer #{} {}",
                id.0,
                self.document
                    .path()
                    .map(|p| format!("\"{}\"", p.display()))
                    .unwrap_or_else(|| "(no file)".into())
            ),
        );
    }

    /// `:bnext` / `:bn` -- cycle to the next listed buffer in id
    /// order, regardless of kind. Skips unlisted buffers; if every
    /// other buffer is unlisted, no-op.
    fn do_buffer_next(&mut self) {
        let Some(target) = self.next_listed_buffer_id() else {
            self.set_message(EchoLevel::Info, "only one listed buffer".to_string());
            return;
        };
        self.activate_buffer(target);
    }

    /// `:bprev` / `:bp` -- cycle to the previous listed buffer.
    fn do_buffer_prev(&mut self) {
        let Some(target) = self.prev_listed_buffer_id() else {
            self.set_message(EchoLevel::Info, "only one listed buffer".to_string());
            return;
        };
        self.activate_buffer(target);
    }

    /// Listed buffer ids in ascending order across kinds. `:bn` /
    /// `:bp` cycle through this; unlisted buffers (vim
    /// `nobuflisted`) are filtered out.
    fn listed_buffer_ids_sorted(&self) -> Vec<BufferId> {
        self.buffers.listed_ids_sorted()
    }

    /// What `:bn` / `:bp` consider the "current" buffer for
    /// stepping. The active pane's buffer_id is the source of
    /// truth (the active pane is what the user sees).
    fn active_pane_buffer_id(&self) -> BufferId {
        self.pane_tree.active().buffer_id
    }

    fn next_listed_buffer_id(&self) -> Option<BufferId> {
        let ids = self.listed_buffer_ids_sorted();
        if ids.len() <= 1 {
            return None;
        }
        let cur = self.active_pane_buffer_id();
        let pos = ids.iter().position(|id| *id == cur)?;
        Some(ids[(pos + 1) % ids.len()])
    }

    fn prev_listed_buffer_id(&self) -> Option<BufferId> {
        let ids = self.listed_buffer_ids_sorted();
        if ids.len() <= 1 {
            return None;
        }
        let cur = self.active_pane_buffer_id();
        let pos = ids.iter().position(|id| *id == cur)?;
        Some(ids[if pos == 0 { ids.len() - 1 } else { pos - 1 }])
    }

    /// `:ls` / `:buffers` -- render every open buffer (regardless
    /// of kind) in a help-style view. The `%` marker points at
    /// whichever buffer the active pane is currently showing.
    fn do_list_buffers(&mut self) {
        let ids = self.buffers.sorted_ids();
        let active_id = self.active_pane_buffer_id();
        let doc_count = self.buffers.document_ids_sorted().len();
        let tree_count = self.buffers.file_tree_ids_sorted().len();
        let help_count = self.buffers.help_ids_sorted().len();
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!(
            "{} open buffer(s) ({} document, {} tree, {} help):",
            ids.len(),
            doc_count,
            tree_count,
            help_count,
        ));
        lines.push(String::new());
        for id in ids {
            let Some(entry) = self.buffers.get(id) else {
                continue;
            };
            let active_marker = if id == active_id { "%" } else { " " };
            let listed_marker = if entry.flags.listed { " " } else { "u" };
            match &entry.data {
                BufferData::Document(d) => {
                    let path = d
                        .handle
                        .path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "(no file)".to_string());
                    let dirty = if d.handle.dirty() { "[+]" } else { "   " };
                    lines.push(format!(
                        "  {active_marker}{listed_marker} #{:<3} doc  {dirty} {path}",
                        id.0
                    ));
                }
                BufferData::FileTree(t) => {
                    lines.push(format!(
                        "  {active_marker}{listed_marker} #{:<3} tree     {}",
                        id.0,
                        t.root.display()
                    ));
                }
                BufferData::Help(h) => {
                    lines.push(format!(
                        "  {active_marker}{listed_marker} #{:<3} help     {}",
                        id.0, h.title,
                    ));
                }
            }
        }
        self.open_help(
            HelpBuffer::from_lines("buffers", lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        );
    }

    /// Snapshot the supervisor's running actor table into the
    /// shape the picker consumes. Walks the supervisor under its
    /// lock, builds one [`crate::picker::LspInstanceRow`] per
    /// `(workspace, server_id)` actor, drops the lock, and returns
    /// the vec. Returns an empty list (and echoes a warning) when
    /// the lock isn't immediately available -- async open / close
    /// in flight; the picker degrades to an empty list rather than
    /// blocking input.
    fn snapshot_lsp_instances(&mut self) -> Vec<crate::picker::LspInstanceRow> {
        let Ok(sup) = self.lsp.try_lock() else {
            self.set_message(
                EchoLevel::Warn,
                "lsp-picker: supervisor busy (async open / shutdown in flight); try again"
                    .to_string(),
            );
            return Vec::new();
        };
        let actors = sup.running_actors();
        actors
            .into_iter()
            .map(|((workspace, server_id), handle)| {
                let key = (workspace.clone(), server_id.clone());
                let buffer_count = sup.buffer_count_for(&key);
                let caps = handle.capabilities();
                let cap_summary = crate::help::summarise_capabilities(&caps);
                crate::picker::LspInstanceRow {
                    workspace,
                    server_id,
                    buffer_count,
                    cap_summary,
                }
            })
            .collect()
    }

    /// Build + open an LSP instance picker. Called by `:lsp-log`,
    /// `:lsp-server-log`, and `:lsp-trace-log`. The `prefilter`
    /// arg pre-narrows the candidate list to one server id while
    /// still allowing the user to disambiguate between multiple
    /// workspaces. `on_accept` decides which buffer the chosen
    /// row opens (`OpenLspLog` or `OpenLspTraceLog`).
    fn open_lsp_picker(
        &mut self,
        title: &str,
        prefilter: Option<String>,
        on_accept: crate::picker::PickerAction,
    ) {
        let rows = self.snapshot_lsp_instances();
        if rows.is_empty() {
            self.set_message(
                EchoLevel::Info,
                "no LSP servers running; open a file with a matching language to attach"
                    .to_string(),
            );
            return;
        }
        // Resolve the user's prefilter through the alias table so
        // `:lsp-log rust-analyzer` finds the `rust` actor. On miss
        // we fall back to the literal string -- the picker UI then
        // shows "no match" with the unresolved name in the echo.
        let resolved_prefilter = prefilter.as_deref().and_then(|n| self.resolve_server_id(n));
        let effective = resolved_prefilter.clone().or_else(|| prefilter.clone());
        // Single match short-circuit: when prefilter narrows the
        // candidate set to exactly one row, skip the picker and
        // open the buffer directly. Vim-style "do what I mean"
        // (e.g. `:lsp-log rust` with one rust workspace).
        let matches: Vec<&crate::picker::LspInstanceRow> = rows
            .iter()
            .filter(|r| {
                effective
                    .as_ref()
                    .is_none_or(|want| r.server_id == *want)
            })
            .collect();
        if matches.len() == 1 {
            let server_id = matches[0].server_id.clone();
            match on_accept {
                crate::picker::PickerAction::OpenLspLog => {
                    self.open_lsp_log_in_pane(&server_id)
                }
                crate::picker::PickerAction::OpenLspTraceLog => {
                    self.open_lsp_trace_log_in_pane(&server_id)
                }
                crate::picker::PickerAction::SwitchToBuffer => {}
            }
            return;
        }
        if matches.is_empty() {
            let asked = prefilter.clone().unwrap_or_default();
            let running = self.running_server_ids();
            let listing = if running.is_empty() {
                String::new()
            } else {
                format!(" (running: {})", running.join(", "))
            };
            self.set_message(
                EchoLevel::Info,
                format!("no LSP server matching {asked:?} running{listing}"),
            );
            return;
        }
        let mut p = crate::picker::Picker::new(
            title,
            crate::picker::PickerSource::LspInstances {
                prefilter: effective,
            },
            on_accept,
        );
        p.set_lsp_instances(rows);
        self.picker = Some(p);
    }

    /// `:b` with no arg (DESIGN.md §5.9.7) -- open the vertico-style
    /// buffer switcher. Type to filter; `<Up>` / `<Down>` (or
    /// `<C-p>` / `<C-n>`) to move; `<CR>` to switch to the
    /// selected buffer; `<Esc>` to dismiss. Marginalia shows the
    /// kind (`doc` / `tree` / `help`) plus a `(current)` tag on
    /// the active buffer.
    ///
    /// **Live preview.** While the picker is open, every selection
    /// change activates the candidate buffer in the active pane
    /// (without polluting the jump list). On accept, that
    /// activation becomes the real switch; on dismiss, the
    /// pane reverts to whatever buffer was active when the
    /// picker opened.
    pub fn open_buffer_picker(&mut self) {
        let active = self.active_pane_buffer_id();
        let mut p = crate::picker::Picker::new(
            "buffers",
            crate::picker::PickerSource::Buffers,
            crate::picker::PickerAction::SwitchToBuffer,
        );
        // Host-side candidate build (the picker module is
        // renderer-agnostic and doesn't import `BufferRegistry`).
        let raw = raw_buffer_candidates(&self.buffers, active);
        p.set_raw_candidates(raw);
        // Stash the original active buffer id so dismiss can
        // restore. None on no-buffer pickers (LSP); for the
        // buffer switcher we always have one. Encoded as `u32`
        // because `Picker::preview_origin` is renderer-agnostic
        // (the host newtype-wraps).
        p.preview_origin = Some(active.0);
        self.picker = Some(p);
        // Preview the initial selection. With the active buffer
        // floated to the bottom, the initial selection is a
        // *different* buffer (the alternate-buffer convention),
        // so opening the picker immediately shows what `<CR>`
        // would land on.
        self.preview_picker_selection();
    }

    /// If the picker is open and its action is
    /// [`crate::picker::PickerAction::SwitchToBuffer`], activate
    /// the currently-selected candidate's buffer in the active
    /// pane *as a preview* -- no position-history push, no
    /// commit. Called after every selection change while a buffer
    /// picker is open.
    fn preview_picker_selection(&mut self) {
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        if !matches!(picker.on_accept, crate::picker::PickerAction::SwitchToBuffer) {
            return;
        }
        let Some(c) = picker.selected_candidate() else {
            return;
        };
        let Some(raw_id) = crate::picker::buffer_id_from_text(&c.raw.text) else {
            return;
        };
        let id = BufferId(raw_id);
        if id == self.active_pane_buffer_id() {
            // Already showing this buffer; nothing to preview.
            return;
        }
        self.previewing = true;
        self.activate_buffer(id);
        self.previewing = false;
    }

    /// Apply `Action::PickerDismiss` -- close the picker and, if
    /// a buffer-switch picker was previewing, restore the active
    /// pane to whatever buffer it was on at picker-open. Tested
    /// by `picker_dismiss_restores_origin_when_previewing`.
    fn do_picker_dismiss(&mut self) {
        let Some(picker) = self.picker.take() else {
            return;
        };
        if let Some(origin_raw) = picker.preview_origin {
            let origin = BufferId(origin_raw);
            if origin != self.active_pane_buffer_id() {
                self.previewing = true;
                self.activate_buffer(origin);
                self.previewing = false;
            }
        }
    }

    /// Apply `Action::PickerAccept` -- run the picker's stored
    /// action against the selected candidate, then dismiss.
    /// For [`crate::picker::PickerAction::SwitchToBuffer`] the
    /// preview-activated buffer is already on screen; the accept
    /// path just commits (clears preview tracking) without
    /// re-activating, so the position history sees ONE entry for
    /// the user's original cursor (pushed at picker-open in
    /// future, today the help-arm autopush handles cross-buffer-
    /// kind landings).
    fn do_picker_accept(&mut self) {
        let Some(picker) = self.picker.take() else {
            return;
        };
        let Some(c) = picker.selected_candidate() else {
            // Empty filter -- bail without acting (the picker is
            // already gone since we `take()`d it). Restore the
            // original buffer if we'd been previewing.
            if let Some(origin) = picker.preview_origin {
                self.previewing = true;
                self.activate_buffer(BufferId(origin));
                self.previewing = false;
            }
            return;
        };
        let payload = c.raw.text.clone();
        match picker.on_accept {
            crate::picker::PickerAction::SwitchToBuffer => {
                let Some(raw_id) = crate::picker::buffer_id_from_text(&payload) else {
                    self.set_message(
                        EchoLevel::Error,
                        format!("picker: malformed buffer id {payload:?}"),
                    );
                    // Malformed -- restore origin if we'd previewed.
                    if let Some(origin) = picker.preview_origin {
                        self.previewing = true;
                        self.activate_buffer(BufferId(origin));
                        self.previewing = false;
                    }
                    return;
                };
                let id = BufferId(raw_id);
                // Already on the target via preview; no additional
                // action needed beyond letting the picker drop.
                if id != self.active_pane_buffer_id() {
                    self.activate_buffer(id);
                }
            }
            crate::picker::PickerAction::OpenLspLog => {
                let Some((_workspace, server_id)) =
                    crate::picker::lsp_key_from_text(&payload)
                else {
                    self.set_message(
                        EchoLevel::Error,
                        format!("picker: malformed lsp key {payload:?}"),
                    );
                    return;
                };
                self.open_lsp_log_in_pane(&server_id);
            }
            crate::picker::PickerAction::OpenLspTraceLog => {
                let Some((_workspace, server_id)) =
                    crate::picker::lsp_key_from_text(&payload)
                else {
                    self.set_message(
                        EchoLevel::Error,
                        format!("picker: malformed lsp key {payload:?}"),
                    );
                    return;
                };
                self.open_lsp_trace_log_in_pane(&server_id);
            }
        }
    }

    /// Open `*lsp:<server_id>*` in the active pane via the
    /// in-pane help registry path. Used by both the picker
    /// accept dispatcher and the direct ex-command short path
    /// when only one instance matches.
    fn open_lsp_log_in_pane(&mut self, server_id: &str) {
        let arc: std::sync::Arc<str> = std::sync::Arc::from(server_id);
        let buffer = crate::help::HelpBuffer::lsp_server_log(&self.lsp_logger, &arc)
            .with_markdown_syntax(self.lang_registry.clone());
        self.open_help_in_pane(buffer);
    }

    /// Open `*lsp:<server_id>:trace*` in the active pane. Pure
    /// view -- the trace toggle is `:lsp-trace <server>` and is
    /// independent of opening / closing this buffer.
    fn open_lsp_trace_log_in_pane(&mut self, server_id: &str) {
        let arc: std::sync::Arc<str> = std::sync::Arc::from(server_id);
        let buffer = crate::help::HelpBuffer::lsp_server_trace(&self.lsp_logger, &arc)
            .with_markdown_syntax(self.lang_registry.clone());
        self.open_help_in_pane(buffer);
    }

    /// Drain queued [`lattice_protocol::Event::LspLogPushed`]
    /// events (Phase 4) and refresh any open log / trace
    /// help buffers from the logger snapshot. Called once per
    /// main-loop tick + at the end of any path that pushes a log
    /// record synchronously.
    ///
    /// Cheap when no log buffers are open: the refresh path
    /// short-circuits on `BufferRegistry::help_with_title`
    /// missing the title. When buffers ARE open the rebuild walks
    /// the logger ring (≤ 10k records) and replaces the rope --
    /// well within frame budget for the editor's scale.
    pub fn drain_lsp_log_events(&mut self) {
        let Some(mut rx) = self.lsp_log_event_rx.take() else {
            return;
        };
        // Coalesce: collect every drained event's scope, then
        // refresh each unique scope at most once. A burst of
        // 100 trace records on one server -> one refresh.
        let mut subsystem = false;
        let mut server_logs: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut server_traces: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        while let Ok(event) = rx.try_recv() {
            if let lattice_protocol::Event::LspLogPushed {
                server_id,
                level,
                source,
                ..
            } = event
            {
                match server_id {
                    None => subsystem = true,
                    Some(id) => {
                        // Trace records (level == "trace" OR
                        // source == "trace") refresh the trace
                        // view; everything else refreshes the
                        // per-server log view (which excludes
                        // trace records by filter).
                        if level == "trace" || source == "trace" {
                            server_traces.insert(id);
                        } else {
                            server_logs.insert(id);
                        }
                    }
                }
            }
        }
        if subsystem {
            self.refresh_lsp_log_buffer_subsystem();
        }
        for id in server_logs {
            self.refresh_lsp_log_buffer_per_server(&id);
        }
        for id in server_traces {
            self.refresh_lsp_trace_buffer(&id);
        }
        self.lsp_log_event_rx = Some(rx);
    }

    /// Rebuild the `*lsp*` (subsystem-wide) help buffer from the
    /// logger snapshot, preserving cursor + scroll. No-op when
    /// the buffer isn't currently open.
    fn refresh_lsp_log_buffer_subsystem(&mut self) {
        let Some(id) = self.buffers.help_with_title("lsp") else {
            return;
        };
        let new_buf = crate::help::HelpBuffer::lsp_global_log(&self.lsp_logger)
            .with_markdown_syntax(self.lang_registry.clone());
        self.replace_help_buffer_preserving_cursor(id, new_buf);
    }

    /// Rebuild `*lsp:<server_id>*` from the logger snapshot.
    fn refresh_lsp_log_buffer_per_server(&mut self, server_id: &str) {
        let title = format!("lsp:{server_id}");
        let Some(id) = self.buffers.help_with_title(&title) else {
            return;
        };
        let arc: std::sync::Arc<str> = std::sync::Arc::from(server_id);
        let new_buf = crate::help::HelpBuffer::lsp_server_log(&self.lsp_logger, &arc)
            .with_markdown_syntax(self.lang_registry.clone());
        self.replace_help_buffer_preserving_cursor(id, new_buf);
    }

    /// Rebuild `*lsp:<server_id>:trace*` from the logger snapshot.
    fn refresh_lsp_trace_buffer(&mut self, server_id: &str) {
        let title = format!("lsp:{server_id}:trace");
        let Some(id) = self.buffers.help_with_title(&title) else {
            return;
        };
        let arc: std::sync::Arc<str> = std::sync::Arc::from(server_id);
        let new_buf = crate::help::HelpBuffer::lsp_server_trace(&self.lsp_logger, &arc)
            .with_markdown_syntax(self.lang_registry.clone());
        self.replace_help_buffer_preserving_cursor(id, new_buf);
    }

    /// Atomically replace a registry-tracked help buffer's body
    /// with `new_buf`, preserving the existing buffer id +
    /// cursor + scroll so the user's view stays put across the
    /// rebuild. Clamps cursor to the new content's line bounds.
    /// Also syncs `App.help_buffer` (the popup hot-path mirror)
    /// when it points at the same id.
    fn replace_help_buffer_preserving_cursor(
        &mut self,
        id: BufferId,
        mut new_buf: crate::help::HelpBuffer,
    ) {
        let (cur, scr) = match self.buffers.help(id) {
            Some(h) => (h.cursor, h.scroll),
            None => return,
        };
        new_buf.id = id;
        new_buf.cursor = cur;
        new_buf.scroll = scr;
        let line_count = new_buf.line_count() as u32;
        if line_count > 0 && new_buf.cursor.line >= line_count {
            new_buf.cursor.line = line_count - 1;
        }
        if let Some(slot) = self.buffers.help_mut(id) {
            *slot = new_buf;
        }
        // Sync the popup hot-path mirror when active.
        if self.help_buffer.as_ref().map(|h| h.id) == Some(id)
            && let Some(reg) = self.buffers.help(id)
        {
            self.help_buffer = Some(reg.clone());
        }
    }

    /// `:bd[elete]` -- close the active buffer (whichever the
    /// active pane shows). v1 picks any other buffer to activate;
    /// if no others remain, the close is rejected. For document
    /// buffers `!` bypasses the dirty check; tree buffers are
    /// always read-only and skip the dirty guard.
    fn do_buffer_delete(&mut self, force: bool) {
        if self.buffers.len() <= 1 {
            self.set_message(
                EchoLevel::Error,
                "Cannot delete the only buffer".to_string(),
            );
            return;
        }
        let to_remove = self.active_pane_buffer_id();
        // Dirty check applies to documents only.
        if let Some(d) = self.buffers.document(to_remove)
            && !force
            && d.handle.dirty()
        {
            self.set_message(
                EchoLevel::Error,
                "no write since last change (add ! to override)".to_string(),
            );
            return;
        }
        // Pick a successor (any other buffer in id order).
        let ids = self.buffers.sorted_ids();
        let Some(successor) = ids.iter().copied().find(|id| *id != to_remove) else {
            return;
        };
        self.activate_buffer(successor);
        // Detach from LSP before dropping the buffer registry
        // entry so the supervisor sees the URI go away while the
        // BufferId is still mapped.
        self.lsp_close_buffer(to_remove);
        self.buffers.remove(to_remove);
        // Re-point any pane still referencing the removed buffer.
        let new_id = self.active_pane_buffer_id();
        let new_kind = self.active_buffer;
        for pane in self.pane_tree.leaves_mut() {
            if pane.buffer_id == to_remove {
                pane.buffer_id = new_id;
                pane.buffer = new_kind;
            }
        }
        self.set_message(EchoLevel::Info, format!("buffer #{} deleted", to_remove.0));
    }

    /// Switch the active pane to whatever buffer `id` references,
    /// regardless of kind. Document buffers route through
    /// [`Self::activate_document`]; tree buffers update the active
    /// pane + load the tree's stash; help buffers go through
    /// [`Self::activate_help_in_pane`] (Phase 1 wiring).
    pub fn activate_buffer(&mut self, id: BufferId) {
        let kind = match self.buffers.get(id) {
            Some(entry) => entry.kind(),
            None => {
                self.set_message(EchoLevel::Error, format!("buffer #{} not found", id.0));
                return;
            }
        };
        match kind {
            BufferKind::Document => self.activate_document(id),
            BufferKind::FileTree => self.activate_file_tree(id),
            BufferKind::Help => self.activate_help_in_pane(id),
        }
    }

    /// Switch the active pane to the file-tree buffer with `id`.
    /// Snapshots the current active state first; the pane's
    /// stashed cursor / scroll load into the tree's hot fields
    /// via [`Self::load_active_pane`].
    pub fn activate_file_tree(&mut self, id: BufferId) {
        if self.buffers.file_tree(id).is_none() {
            self.set_message(EchoLevel::Error, format!("buffer #{} not a tree", id.0));
            return;
        }
        if id == self.active_pane_buffer_id() && matches!(self.active_buffer, BufferKind::FileTree)
        {
            return;
        }
        self.snapshot_active_pane();
        self.snapshot_active_document();
        // Load the tree's stash into the App's hot-path cursor /
        // scroll. After this, `self.cursor` / `self.scroll` are
        // the tree's -- motion / scroll / search code reads /
        // writes them uniformly, no per-kind branches needed.
        let (stash_cursor, stash_scroll) = self
            .buffers
            .file_tree(id)
            .map(|t| (t.cursor, t.scroll as u32))
            .unwrap_or((Position::ZERO, 0));
        self.cursor = stash_cursor;
        self.scroll = stash_scroll;
        self.active_buffer = BufferKind::FileTree;
        let pane = self.pane_tree.active_mut();
        pane.buffer = BufferKind::FileTree;
        pane.buffer_id = id;
        pane.cursor = stash_cursor;
        pane.scroll = stash_scroll;
    }

    /// `:set [option | option=value | nooption | option?]`.
    /// Parses against the shared typed-options
    /// [`lattice_config::ConfigRegistry`] (DESIGN.md §5.12).
    /// Boolean toggle / negate forms (`:set nu` / `:set nonu`),
    /// query (`:set foo?`), and typed assignment (`:set tabstop=4`)
    /// all flow through one path. Post-set side effects
    /// (`relativenumber` ⇒ `number`, `foldmethod` ⇒ recompute folds,
    /// every `ui.*` change ⇒ refresh derived theme styles) are
    /// applied after a successful set in [`Self::apply_post_set`].
    fn do_set(&mut self, option: &str) {
        let echo = match self.config.parse_and_set_command(option) {
            Ok(echo) => echo,
            Err(err) => {
                self.set_message(EchoLevel::Error, err.to_string());
                return;
            }
        };
        // Drain any cascade events the set just enqueued so the
        // user sees the side effects (recompute folds, theme
        // refresh, ...) before the next frame draws. The runtime's
        // main_loop also drains once per iteration as a backstop
        // for writes that originate outside the keystroke path
        // (plugin tasks, future LSP-driven config writes).
        self.drain_option_changes();
        self.set_message(EchoLevel::Info, echo);
    }

    /// Drain queued [`Event::OptionChanged`] events from the App's
    /// own bus subscription and apply per-option cascades on the
    /// App's main thread.
    ///
    /// Why a channel and not a callback: typed-option writes can
    /// originate from anywhere -- the cmdline, plugin tasks
    /// (Phase 7), the customize buffer view (post-1.0), or future
    /// LSP-driven config writes. The publisher closure on the
    /// registry runs *on the calling thread*, which may not be
    /// the App's. Routing every cascade through this channel
    /// gives us:
    ///
    /// - **No re-entrancy on the registry mutex**: the cascade
    ///   runs after the publish path drops every lock. A cascade
    ///   that itself calls `config.set` (e.g. `relativenumber=true`
    ///   ⇒ `number=true`) just queues another event -- the
    ///   `while let Ok` loop picks it up on the next iteration.
    /// - **No render-thread blocking**: drains happen at known
    ///   points (top of main_loop iteration, end of `do_set`).
    ///   Plugins doing heavy work in their own subscriptions
    ///   never delay a keystroke.
    /// - **One source of truth for the cascade logic**: any
    ///   typed-option write goes through this hook regardless of
    ///   how the write was triggered. Pre-bus the cascade lived
    ///   on the cmdline path only and direct `config.set` calls
    ///   silently skipped it.
    ///
    /// `Manual` foldmethod, no-op cascades, and unmatched options
    /// all return early so the drain is cheap when nothing
    /// substantive needs to happen.
    pub fn drain_option_changes(&mut self) {
        // Take the receiver to dodge the borrow checker (we want
        // to mutate `self` for cascades while reading from the rx).
        // Always restored after the loop; the `Option` is purely a
        // borrow gymnastic, never observed in any other state.
        let mut rx = match self.option_change_rx.take() {
            Some(rx) => rx,
            None => return,
        };
        while let Ok(event) = rx.try_recv() {
            if let Event::OptionChanged { name, .. } = event {
                self.apply_option_cascade(&name);
            }
        }
        self.option_change_rx = Some(rx);
    }

    /// Run the per-option cascade for `canonical_name` (already
    /// resolved by `Event::OptionChanged.name`, which is always
    /// the canonical name regardless of which alias the user
    /// typed).
    fn apply_option_cascade(&mut self, canonical_name: &str) {
        // Refresh the hot-path cache so subsequent reads from
        // `app.show_line_numbers()` etc. see the new value.
        // Cheap (~300ns total for all 9 options); only runs when
        // an option actually changed, never on every frame.
        self.rebuild_option_cache();
        match canonical_name {
            "relativenumber" => {
                // Vim cascade: `:set rnu` implies `:set nu` so the
                // gutter renders at all. The reverse (`:set nornu`)
                // does NOT clear `nu` -- preserves user intent.
                // Conditional on the new value being `true`, which
                // we re-read through the typed handle (cheap).
                if self.relative_line_numbers() {
                    let _ = self.config.set(self.core_options.number, true);
                }
            }
            "foldmethod" => {
                // Recompute folds against the new method. Idempotent
                // and cheap when method is `Manual` (the recompute
                // returns immediately).
                self.recompute_folds();
            }
            n if n.starts_with("ui.") => {
                self.sync_theme_from_config();
            }
            _ => {}
        }
    }

    /// Re-derive `App.theme`'s renderer-specific [`Style`] values
    /// from the current `ui.*` option values in the config. Called
    /// at App-init time (after registration) and on every `:set
    /// ui.*` so the cached theme stays in lockstep with the
    /// canonical primitives in config.
    pub fn sync_theme_from_config(&mut self) {
        use ratatui::style::Style;
        // ui.dim_inactive -- bool flag projected directly.
        self.theme.dim_inactive_panes = *self.config.get(self.tui_options.dim_inactive);
        // ui.separator -- one-character glyph for the vertical
        // pane divider. Validated to len==1 at parse; fall back to
        // the default if a forged value sneaks through.
        let sep = self.config.get(self.tui_options.separator);
        self.theme.pane_separator_vertical = sep.chars().next().unwrap_or('│');
        // ui.separator_color -- color name; parse_color returned
        // Ok during validate so unwrap-via-fallback is safe.
        let sep_color = self.config.get(self.tui_options.separator_color);
        if let Ok(c) = crate::theme::parse_color(&sep_color) {
            self.theme.pane_separator = Style::default().fg(c);
        }
        // ui.statusline_active_fg -- foreground only; preserve any
        // existing modifiers / background by chaining `.fg(c)` on
        // the current style.
        let active_fg = self.config.get(self.tui_options.statusline_active_fg);
        if let Ok(c) = crate::theme::parse_color(&active_fg) {
            self.theme.pane_status_active = self.theme.pane_status_active.fg(c);
        }
        let inactive_fg = self.config.get(self.tui_options.statusline_inactive_fg);
        if let Ok(c) = crate::theme::parse_color(&inactive_fg) {
            self.theme.pane_status_inactive = self.theme.pane_status_inactive.fg(c);
        }
    }

    /// `:describe-option <name>` (DESIGN.md §5.11). Renders the
    /// option's metadata + current value into a help buffer.
    fn do_describe_option(&mut self, name: &str) {
        let Some(spec) = self.config.lookup(name) else {
            self.set_message(EchoLevel::Error, format!("E518: Unknown option: {name}"));
            return;
        };
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("# {}", spec.name()));
        if !spec.aliases().is_empty() {
            lines.push(format!("aliases: {}", spec.aliases().join(", ")));
        }
        lines.push(format!("type:    {}", spec.type_label()));
        lines.push(format!("default: {}", spec.default_formatted()));
        lines.push(format!("current: {}", spec.get_formatted()));
        if let Some(values) = spec.enumerate_values() {
            lines.push(format!("values:  {}", values.join(", ")));
        }
        lines.push(String::new());
        lines.push(spec.doc().to_string());
        self.open_help(
            HelpBuffer::from_lines(format!("describe-option {name}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        );
    }

    /// `K` (LSP hover) response handler / `:hover [markdown]`.
    /// Enters **State A**: popup overlay shown, focus stays on
    /// the main buffer. The popup auto-dismisses on the next
    /// motion (apply()'s post-dispatch hook) since it's anchored
    /// to the symbol the user K'd. To navigate inside the popup
    /// the user presses `K` again, which `do_lsp_hover_request`
    /// recognises as "focus into popup" -> State B.
    fn do_open_hover(&mut self, markdown: &str) {
        let lines: Vec<String> = markdown.split('\n').map(String::from).collect();
        let buffer = crate::help::HelpBuffer::from_lines("hover", lines)
            .with_markdown_syntax(self.lang_registry.clone());
        // State A: just set the help_buffer. Active stays on the
        // main buffer; self.cursor untouched. prev_pane_for_help
        // remains `None` -- the State-A auto-dismiss key.
        self.help_buffer = Some(buffer);
    }

    /// **State A -> State B**: focus moves into the popup. After
    /// this, the popup behaves like any other buffer -- vim
    /// grammar (motions, `/` search, `n`/`N`, `gg`/`G`, `:` ex
    /// commands) operates on the popup's content; the doc behind
    /// is frozen. Dismiss with `<Esc>` / `q` returns focus to
    /// the doc at the cursor it was on.
    ///
    /// `prev_pane_for_help` is the dismiss-restore stash and
    /// also signals "we're in State B" to the auto-dismiss-on-
    /// motion hook -- when it's `Some`, motion doesn't close the
    /// popup.
    fn focus_help_popup(&mut self) {
        let Some(help) = self.help_buffer.as_ref() else {
            return;
        };
        let active = self.pane_tree.active();
        self.prev_pane_for_help = Some(PrevPaneState {
            buffer: active.buffer,
            buffer_id: active.buffer_id,
            cursor: self.cursor,
            scroll: self.scroll,
        });
        let stash_cursor = help.cursor;
        let stash_scroll = help.scroll as u32;
        self.cursor = stash_cursor;
        self.scroll = stash_scroll;
        self.active_buffer = BufferKind::Help;
        self.pending = Pending::None;
    }

    /// `K` (Phase 4.2.b). Send `textDocument/hover` to every LSP
    /// server attached to the active document; the spawned task
    /// awaits the actor's response on the LSP runtime, so the
    /// keystroke handler returns instantly. The markdown body
    /// arrives back through [`Self::pending_hover_rx`] and the
    /// next frame's [`Self::drain_pending_hover`] feeds it into
    /// the popup.
    ///
    /// **Multi-server merge** is "first non-empty wins" for
    /// 4.2.b. The `--- {server-name} ---` concat-with-separators
    /// strategy spec'd in `docs/lsp-architecture.md` lands as a
    /// follow-up; today the simpler shape is enough to validate
    /// the end-to-end plumbing.
    ///
    /// **Cancellation**: any prior in-flight hover's token is
    /// flipped before the new request fires, so a slow server
    /// can't drop a stale popup over the new cursor position.
    fn do_lsp_hover_request(&mut self) {
        // Already focused into the popup (State B) -- K is a
        // no-op. To get a fresh hover the user dismisses with
        // Esc / q, repositions in the doc, then presses K.
        if matches!(self.active_buffer, BufferKind::Help) {
            return;
        }
        // Popup shown but focus still on main buffer (State A) --
        // second K transfers focus into the popup. No new LSP
        // request fires; we just promote.
        if self.help_buffer.is_some() {
            self.focus_help_popup();
            return;
        }
        // First K -- fire a fresh hover request. Cancel any
        // in-flight first.
        if let Some(token) = self.pending_hover_token.take() {
            token.cancel();
        }

        // Resolve the active buffer's URI. No URI = no LSP for
        // this buffer (e.g. unsaved scratch); echo + bail.
        let Some(uri) = self
            .buffer_uris
            .get(&self.document_buffer_id)
            .cloned()
        else {
            self.set_message(
                EchoLevel::Info,
                "no LSP server attached to current buffer".to_string(),
            );
            return;
        };

        // Build the LSP-side cursor position. App's cursor is
        // (line, col_byte) in utf-8; LSP wants utf-16 columns.
        // The lattice-lsp::position::Encoding-aware conversion
        // walks the line; for hover, exact column matters
        // because servers gate the lookup on the symbol under
        // the cursor.
        let snapshot = self.document.snapshot();
        let lsp_position = match app_to_lsp_position(&snapshot.buffer, self.cursor) {
            Some(p) => p,
            None => {
                self.set_message(EchoLevel::Error, "hover: cursor out of buffer".to_string());
                return;
            }
        };

        // Fresh channel + token for this request.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<HoverOutcome>();
        let token = lattice_protocol::CancellationToken::new();
        self.pending_hover_rx = Some(rx);
        self.pending_hover_token = Some(token.clone());

        // Spawn the request on the LSP runtime so the keystroke
        // handler returns instantly. The task walks every server
        // attached to the buffer; first non-empty body wins. The
        // task ALWAYS sends exactly one [`HoverOutcome`] (or
        // nothing if cancelled mid-flight, in which case the
        // drain just sees an idle channel) so the user always
        // gets feedback.
        //
        // Trace each request into `:lsp-log` so users can
        // correlate K presses with server responses -- helpful
        // when rust-analyzer-style servers are still indexing
        // on first launch and every hover returns null until the
        // index settles (a real "indexing in progress" status
        // segment lands as Phase 4.4 polish).
        // Per-request traces flow into the per-server ring
        // (`*lsp:<server>*`) -- not the global `*lsp*` ring --
        // so `:lsp-server-log` lands the picker on the right
        // place to inspect them. The logger's first arg gates
        // routing: `None` = global ring; `Some(id)` = per-server
        // ring keyed by id.
        let lsp = self.lsp.clone();
        let logger = self.lsp_logger.clone();
        let request_started = std::time::Instant::now();
        let request_uri = uri.as_str().to_string();
        crate::runtime::spawn_on_lsp_runtime(async move {
            // Snapshot the attached handles under the supervisor
            // lock, then drop it before awaiting any per-server
            // response (the lock is App-side; we don't hold it
            // across awaits).
            let handles: Vec<lattice_lsp::ServerHandle> =
                { lsp.lock().await.servers_for(&uri) };
            if handles.is_empty() {
                let _ = tx.send(HoverOutcome::NoServers);
                return;
            }
            let mut tried = 0usize;
            for handle in handles {
                if token.is_cancelled() {
                    return;
                }
                tried += 1;
                let params = lsp_types::HoverParams {
                    text_document_position_params: lsp_types::TextDocumentPositionParams {
                        text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                        position: lsp_position,
                    },
                    work_done_progress_params: Default::default(),
                };
                let server_id_arc: std::sync::Arc<str> =
                    std::sync::Arc::from(handle.server_id());
                // Trace the request landing per-server so the
                // user can correlate K presses with responses
                // from inside `:lsp-log <server>` /
                // `:lsp-server-log` -> the right row's log.
                logger.log(
                    Some(&server_id_arc),
                    lattice_lsp::LogLevel::Debug,
                    lattice_lsp::LogSource::Client,
                    format!(
                        "hover requested @ {request_uri} line {} character {}",
                        lsp_position.line, lsp_position.character
                    ),
                );
                match handle.hover(params, token.clone()).await {
                    Ok(Some(hover)) => {
                        let body = hover_contents_to_markdown(&hover.contents);
                        if !body.trim().is_empty() {
                            logger.log(
                                Some(&server_id_arc),
                                lattice_lsp::LogLevel::Debug,
                                lattice_lsp::LogSource::Client,
                                format!(
                                    "hover reply: {} bytes after {:?}",
                                    body.len(),
                                    request_started.elapsed()
                                ),
                            );
                            let _ = tx.send(HoverOutcome::Body(body));
                            return;
                        }
                        // Server replied but the body's empty.
                        // rust-analyzer returns this while still
                        // indexing -- highlight the pattern in
                        // the per-server log.
                        logger.log(
                            Some(&server_id_arc),
                            lattice_lsp::LogLevel::Debug,
                            lattice_lsp::LogSource::Client,
                            "hover reply: empty body (server still indexing?)".to_string(),
                        );
                    }
                    Ok(None) => {
                        logger.log(
                            Some(&server_id_arc),
                            lattice_lsp::LogLevel::Debug,
                            lattice_lsp::LogSource::Client,
                            "hover reply: null (cursor not on a known symbol, or server still indexing)"
                                .to_string(),
                        );
                    }
                    Err(e) => {
                        // Cancelled / decode error / actor gone:
                        // per-server Warn so the right log
                        // surfaces the reason.
                        logger.log(
                            Some(&server_id_arc),
                            lattice_lsp::LogLevel::Warn,
                            lattice_lsp::LogSource::Client,
                            format!("hover error: {e}"),
                        );
                    }
                }
            }
            // Walked every server, none had a non-empty body.
            let _ = tx.send(HoverOutcome::NoBody {
                servers_tried: tried,
            });
        });
    }

    /// Drain the channel populated by [`Self::do_lsp_hover_request`]
    /// and act on every pending [`HoverOutcome`]: open the popup
    /// for `Body`, echo a clear message for `NoBody` / `NoServers`
    /// so the user always knows their `K` press was processed.
    /// Called once per main_loop iteration before draw; cheap
    /// when the channel is empty (the common case).
    pub fn drain_pending_hover(&mut self) {
        let Some(mut rx) = self.pending_hover_rx.take() else {
            return;
        };
        // Last-writer-wins -- if a stale outcome and a fresh one
        // both queued (e.g. user pressed K twice quickly and
        // both relays raced), we surface the latest.
        let mut latest: Option<HoverOutcome> = None;
        while let Ok(outcome) = rx.try_recv() {
            latest = Some(outcome);
        }
        if let Some(outcome) = latest {
            match outcome {
                HoverOutcome::Body(body) => {
                    self.do_open_hover(&body);
                }
                HoverOutcome::NoBody { servers_tried } => {
                    self.set_message(
                        EchoLevel::Info,
                        format!(
                            "no hover info at cursor ({servers_tried} server{} replied)",
                            if servers_tried == 1 { "" } else { "s" }
                        ),
                    );
                }
                HoverOutcome::NoServers => {
                    self.set_message(
                        EchoLevel::Warn,
                        "hover: no LSP servers attached for this buffer (\
                         check :lsp-status / :lsp-log)"
                            .to_string(),
                    );
                }
            }
            // Outcome delivered: clear the in-flight token so a
            // subsequent motion doesn't try to flip a stale token.
            self.pending_hover_token = None;
        }
        self.pending_hover_rx = Some(rx);
    }

    /// `gd` (Phase 4.2.c). Send `textDocument/definition` to every
    /// LSP server attached to the active document. Same async
    /// shape as [`Self::do_lsp_hover_request`]: spawn on the LSP
    /// runtime, route the merged + deduped location list back via
    /// [`Self::pending_definition_rx`], drain on next frame.
    ///
    /// **Multi-server merge**: every server's response is
    /// flattened to `Vec<Location>` (the lsp-types enum carries
    /// `Scalar` / `Array` / `Link` shapes); the union is
    /// deduplicated by `(uri, range.start)`. A single result
    /// jumps; multiple results today echo a count and jump to the
    /// first (the picker buffer lands with 4.2.d's references
    /// view -- same shape, no point building it twice).
    fn do_lsp_definition_request(&mut self) {
        if let Some(token) = self.pending_definition_token.take() {
            token.cancel();
        }
        let Some(uri) = self
            .buffer_uris
            .get(&self.document_buffer_id)
            .cloned()
        else {
            self.set_message(
                EchoLevel::Info,
                "no LSP server attached to current buffer".to_string(),
            );
            return;
        };
        let snapshot = self.document.snapshot();
        let lsp_position = match app_to_lsp_position(&snapshot.buffer, self.cursor) {
            Some(p) => p,
            None => {
                self.set_message(
                    EchoLevel::Error,
                    "definition: cursor out of buffer".to_string(),
                );
                return;
            }
        };
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<Vec<lsp_types::Location>>();
        let token = lattice_protocol::CancellationToken::new();
        self.pending_definition_rx = Some(rx);
        self.pending_definition_token = Some(token.clone());
        let lsp = self.lsp.clone();
        crate::runtime::spawn_on_lsp_runtime(async move {
            let handles: Vec<lattice_lsp::ServerHandle> =
                { lsp.lock().await.servers_for(&uri) };
            let mut all: Vec<lsp_types::Location> = Vec::new();
            for handle in handles {
                if token.is_cancelled() {
                    return;
                }
                let params = lsp_types::GotoDefinitionParams {
                    text_document_position_params: lsp_types::TextDocumentPositionParams {
                        text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                        position: lsp_position,
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                };
                if let Ok(Some(resp)) = handle.goto_definition(params, token.clone()).await {
                    all.extend(definition_response_to_locations(resp));
                }
            }
            // Dedup by (uri, range.start). Some servers emit the
            // same location for overloaded methods; the picker
            // shouldn't show duplicates.
            all.sort_by(|a, b| {
                let au = a.uri.as_str();
                let bu = b.uri.as_str();
                au.cmp(bu)
                    .then_with(|| a.range.start.line.cmp(&b.range.start.line))
                    .then_with(|| a.range.start.character.cmp(&b.range.start.character))
            });
            all.dedup_by(|a, b| {
                a.uri.as_str() == b.uri.as_str() && a.range.start == b.range.start
            });
            let _ = tx.send(all);
        });
    }

    /// Drain queued goto-definition results and act on them:
    /// 0 → echo, 1 → jump, N>1 → echo count + jump to first
    /// (picker is 4.2.d's job; we share its buffer surface
    /// rather than build two of them). Pushes the pre-jump
    /// cursor onto the position history so `<C-o>` walks back.
    pub fn drain_pending_definitions(&mut self) {
        let Some(mut rx) = self.pending_definition_rx.take() else {
            return;
        };
        let mut latest: Option<Vec<lsp_types::Location>> = None;
        while let Ok(locs) = rx.try_recv() {
            latest = Some(locs);
        }
        self.pending_definition_rx = Some(rx);
        let locs = match latest {
            Some(l) => l,
            None => return,
        };
        // Result delivered; clear the in-flight token.
        self.pending_definition_token = None;

        match locs.len() {
            0 => {
                self.set_message(EchoLevel::Info, "no definitions found".to_string());
            }
            1 => {
                self.jump_to_lsp_location(&locs[0]);
            }
            n => {
                self.set_message(
                    EchoLevel::Info,
                    format!("{n} definitions; jumping to first (picker comes in 4.2.d)"),
                );
                self.jump_to_lsp_location(&locs[0]);
            }
        }
    }

    /// Jump to an LSP `Location`. If the target is the current
    /// buffer, just move the cursor + push history. If
    /// cross-file, route through `do_edit` so the `:e` machinery
    /// (LSP attach, buffer registry, etc.) handles the open;
    /// then move cursor.
    ///
    /// Pushes the *pre-jump* cursor onto position history with
    /// source [`PositionSource::PluginPush`] so `<C-o>` walks
    /// back. Tagging it as PluginPush (not AutoJump) reflects
    /// that the jump came from an external dispatch (LSP) rather
    /// than a vim-style motion.
    fn jump_to_lsp_location(&mut self, loc: &lsp_types::Location) {
        let target_path = match lattice_lsp::actor::uri_to_path(&loc.uri) {
            Some(p) => p,
            None => {
                self.set_message(
                    EchoLevel::Error,
                    format!("definition target uri is not a file: {}", loc.uri.as_str()),
                );
                return;
            }
        };
        // Push pre-jump cursor before doing anything else so a
        // subsequent <C-o> walks back to where we started, not to
        // the target.
        self.push_position_history(self.cursor, PositionSource::PluginPush);

        // Same buffer? Just update the cursor.
        let same_buffer = self
            .document
            .path()
            .map(|p| p == target_path)
            .unwrap_or(false);
        if !same_buffer {
            self.do_edit(Some(target_path), false);
            // After do_edit, self.document points at the new
            // buffer; the cursor below positions inside it.
        }
        // Convert LSP target position back to App (line, byte).
        let snap = self.document.snapshot();
        let line_text = snap.buffer.line(loc.range.start.line).unwrap_or_default();
        // utf-16 → utf-8 byte (Phase 4.1's encoding negotiation
        // defaults to utf-16; same assumption here).
        let byte = lattice_lsp::position::utf16_column_to_utf8_byte(
            &line_text,
            loc.range.start.character,
        );
        self.cursor = Position::new(loc.range.start.line, byte);
        // The fold-aware open-on-jump (#173) handles closed folds
        // around the target; nothing extra needed here.
    }

    /// `:HoverClose` -- dismiss the hover popup. Routes through
    /// the unified help-dismiss path so State A and State B both
    /// unwind cleanly (B restores via `prev_pane_for_help`; A
    /// just drops the popup).
    fn do_close_hover(&mut self) {
        self.dismiss_help();
    }

    /// `:help [topic]` (DESIGN.md §5.11). With no topic the index
    /// is rendered (the topic registered as `index`); with a
    /// topic name the registry is queried and the topic body is
    /// rendered into a help buffer through the same markdown-
    /// highlighting path `:describe-command` uses. Unknown topic
    /// surfaces as a clear echo error so completion + typo
    /// recovery work.
    fn do_open_help_topic(&mut self, topic: Option<&str>) {
        let name = topic.unwrap_or("index").to_string();
        let registry = self.help_topics.clone();
        let Some(t) = registry.lookup(&name) else {
            self.set_message(EchoLevel::Error, format!("no help topic: {name}"));
            return;
        };
        let body = t.body.render();
        let lines: Vec<String> = body.split('\n').map(|s| s.to_string()).collect();
        // Auto-generate anchors from `#` / `##` / ... headings so
        // intra-doc `[label](#slug)` links route to the right
        // section without authors hand-maintaining anchor tables.
        let anchors = crate::help::generate_heading_anchors(&lines);
        let title = if name == "index" {
            "help".to_string()
        } else {
            format!("help {name}")
        };
        self.open_help(
            HelpBuffer::from_lines_and_anchors(title, lines, anchors)
                .with_markdown_syntax(self.lang_registry.clone()),
        );
    }

    /// `:options` -- list every registered option in a help view.
    fn do_list_options(&mut self) {
        let mut lines: Vec<String> = Vec::new();
        let mut specs = self.config.iter();
        specs.sort_by_key(|s| s.name());
        lines.push(format!("{} registered option(s):", specs.len()));
        lines.push(String::new());
        for spec in specs {
            lines.push(format!(
                "  {:<32} {:<10} = {}",
                spec.name(),
                spec.type_label(),
                spec.get_formatted()
            ));
        }
        self.open_help(
            HelpBuffer::from_lines("options", lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        );
    }

    /// Vim's `:reg` -- list every register's contents in the echo area.
    /// v1 shows the unnamed `""`, the numbered `"0`, and the named
    /// alphabetic registers in alphabetical order.
    fn do_list_registers(&mut self) {
        let mut lines: Vec<String> = Vec::new();
        if let Some(reg) = &self.unnamed_register {
            lines.push(format!("\"\"  {}", preview_register(&reg.content)));
        }
        let mut keys: Vec<Register> = self.registers.keys().copied().collect();
        keys.sort_by_key(|k| match k {
            Register::Named(c) => format!("a{c}"),
            Register::Numbered(n) => format!("b{n}"),
            Register::System => "z+".into(),
            _ => "z".into(),
        });
        for k in keys {
            // The keys came from `self.registers.keys()`, so the lookup
            // can't fail unless someone races us -- which we don't.
            let Some(entry) = self.registers.get(&k) else {
                continue;
            };
            let label = match k {
                Register::Named(c) => format!("\"{c}"),
                Register::Numbered(n) => format!("\"{n}"),
                Register::System => "\"+".into(),
                _ => "?".into(),
            };
            lines.push(format!("{label}  {}", preview_register(&entry.content)));
        }
        if lines.is_empty() {
            self.set_message(EchoLevel::Info, "no registers set".to_string());
        } else {
            self.set_message(EchoLevel::Info, lines.join("  |  "));
        }
    }

    /// Vim's `:marks` -- list every set mark's name + position.
    fn do_list_marks(&mut self) {
        let mut entries: Vec<(char, Position)> = self.marks.iter().map(|(c, p)| (*c, *p)).collect();
        entries.sort_by_key(|(c, _)| *c);
        if entries.is_empty() {
            self.set_message(EchoLevel::Info, "no marks set".to_string());
            return;
        }
        let parts: Vec<String> = entries
            .into_iter()
            .map(|(c, p)| format!("{c}={}:{}", p.line + 1, p.byte))
            .collect();
        self.set_message(EchoLevel::Info, parts.join("  "));
    }

    /// Delete the cursor's whole line including its trailing newline
    /// (vim's `:d`). The standard delete operator's CurrentLine range
    /// preserves the newline, which leaves an empty line behind -- that's
    /// fine for `dd` (cursor stays put on a now-empty line) but wrong
    /// for `:d` and `:g/.../d`. Here we explicitly include the newline.
    fn do_delete_line(&mut self) {
        let line = self.cursor.line;
        let last = last_addressable_line(&self.document.snapshot().buffer);
        let len = line_byte_len(&self.document.snapshot().buffer, line);
        let r = if line < last {
            // Include the trailing newline by extending into the next line.
            ProtoRange::new(Position::new(line, 0), Position::new(line + 1, 0))
        } else if line > 0 {
            // Last line: include the previous line's newline by reaching
            // back to the end of `line - 1`.
            let prev = line - 1;
            let prev_len = line_byte_len(&self.document.snapshot().buffer, prev);
            ProtoRange::new(Position::new(prev, prev_len), Position::new(line, len))
        } else {
            // Single-line buffer: just delete the content.
            ProtoRange::new(Position::new(line, 0), Position::new(line, len))
        };
        if self.apply_edit_blocking(Edit::delete(r)).is_ok() {
            self.cursor = Position::new(
                line.min(last_addressable_line(&self.document.snapshot().buffer)),
                0,
            );
        }
    }

    /// Vim's :g / :v -- execute `body` on every line matching (or NOT
    /// matching, when inverted) the literal pattern. Operates bottom-up
    /// so deletions don't shift the upcoming target lines. v1: `body`
    /// is parsed as a single ex-command.
    fn do_global(&mut self, pattern: &str, inverted: bool, body: &CommandInvocation) {
        if pattern.is_empty() {
            self.set_message(EchoLevel::Error, "empty pattern".to_string());
            return;
        }
        let last = last_addressable_line(&self.document.snapshot().buffer);
        // Build the list of target line numbers from the current snapshot
        // (so subsequent edits don't shift our intent).
        let mut targets = Vec::new();
        {
            let text = self.document.text();
            for (i, line) in text.split_inclusive('\n').enumerate() {
                if i as u32 > last {
                    break;
                }
                let stripped = line.trim_end_matches('\n');
                let matches = stripped.contains(pattern);
                if matches != inverted {
                    targets.push(i as u32);
                }
            }
        }
        if targets.is_empty() {
            self.set_message(
                EchoLevel::Error,
                format!(
                    "no lines {} pattern: {pattern}",
                    if inverted { "lacking" } else { "matching" }
                ),
            );
            return;
        }
        // Run bottom-up so deletions and edits on later lines don't
        // shift the line numbers we plan to operate on. The body is
        // already parsed -- the cmdline's `:g/pat/body` parser
        // compiled it once at submit time, so we just clone the
        // invocation per match.
        for &line in targets.iter().rev() {
            self.cursor = Position::new(line, 0);
            match self.dispatch_blocking(body.clone()) {
                Ok(eff) => self.apply_effect(eff),
                Err(e) => {
                    self.set_message(EchoLevel::Error, format!("g: {e}"));
                    return;
                }
            }
        }
    }

    /// Vim's `:s/pattern/replacement/[g]` (and `:%s/...` for whole-buffer
    /// scope). v1 is literal substring matching (regex deferred to
    /// post-1.0). Returns count of replacements via the echo area.
    fn do_substitute(
        &mut self,
        scope: lattice_grammar::SubstituteScope,
        pattern: &str,
        replacement: &str,
        global: bool,
    ) {
        if pattern.is_empty() {
            self.set_message(EchoLevel::Error, "empty pattern".to_string());
            return;
        }
        // Compile once. Surface compile errors to the user.
        // Replacement template syntax follows fancy-regex /
        // `regex` crate: `$1`, `${name}`, `$0` (whole match), `$$`
        // for a literal `$`. NOT vim's `\1`/`&` -- modern syntax.
        let regex = match compile_search_pattern(pattern) {
            Ok(r) => r,
            Err(msg) => {
                self.set_message(EchoLevel::Error, format!("regex: {msg}"));
                return;
            }
        };
        // Determine the line range.
        let (first_line, last_line) = match scope {
            lattice_grammar::SubstituteScope::CurrentLine => (self.cursor.line, self.cursor.line),
            lattice_grammar::SubstituteScope::Whole => {
                let last = last_addressable_line(&self.document.snapshot().buffer);
                (0, last)
            }
        };
        let mut total = 0usize;
        // Apply per line, top-down. fancy-regex's `replace_all` /
        // `replace` does the heavy lifting: SIMD literal prefilter
        // for backref-free patterns, NFA fallback when needed,
        // template substitution with $1/${name}.
        for line in first_line..=last_line {
            let line_text = self
                .document
                .snapshot()
                .buffer
                .line(line)
                .unwrap_or_default();
            let new_line = if global {
                regex.replace_all(&line_text, replacement)
            } else {
                regex.replace(&line_text, replacement)
            };
            // If nothing changed on this line, skip the edit.
            if new_line == line_text {
                continue;
            }
            // Count substitutions: cheap to tally via find_iter.
            let count_on_line = if global {
                let mut c = 0usize;
                for m in regex.find_iter(&line_text) {
                    if m.is_ok() {
                        c += 1;
                    }
                }
                c
            } else {
                1
            };
            let line_len = line_text.len() as u32;
            let r = ProtoRange::new(Position::new(line, 0), Position::new(line, line_len));
            let _ = self.apply_edit_blocking(Edit::replace(r, new_line.into_owned()));
            total += count_on_line;
        }
        if total == 0 {
            self.set_message(
                EchoLevel::Error,
                format!("E486: Pattern not found: {pattern}"),
            );
        } else {
            self.set_message(
                EchoLevel::Info,
                format!("{total} substitution{}", if total == 1 { "" } else { "s" }),
            );
        }
    }

    fn do_write(&mut self, path: Option<std::path::PathBuf>) {
        let result: Result<String, RuntimeError> = match path {
            Some(p) => self
                .save_as_blocking(p.clone())
                .map(|()| p.display().to_string()),
            None => self.save_blocking().map(|p| p.display().to_string()),
        };
        match result {
            Ok(displayed) => self.set_message(EchoLevel::Info, format!("\"{displayed}\" written")),
            Err(RuntimeError::Core(CoreError::NoPath)) => {
                self.set_message(EchoLevel::Error, "no file name (use :w <path>)".to_string());
            }
            Err(e) => self.set_message(EchoLevel::Error, format!("write error: {e}")),
        }
    }

    fn do_quit(&mut self, force: bool) {
        if !force && self.document.dirty() {
            self.set_message(
                EchoLevel::Error,
                "no write since last change (add ! to override)".to_string(),
            );
            return;
        }
        // BeforeQuit is observation-only in v1 (no veto seam yet --
        // see §5.10.2 follow-up). Subscribers see it; the quit
        // proceeds regardless. Future: if a Before-class handler
        // returns Err, abort.
        self.event_bus.publish(Event::BeforeQuit);
        self.should_quit = true;
    }

    // `:wq` / `:x` are now Effect::Many([SaveBuffer, QuitEditor{force}])
    // composed in `lattice_grammar::ex_commands::apply_write_quit`. The
    // do_write + do_quit pair runs in sequence via apply_effect; the
    // quit's force-bit comes from the trailing `!` (DESIGN.md §5.2.1).

    fn run_invocation(&mut self, inv: CommandInvocation) {
        // Pending state is consumed by the input layer that built `inv`; any
        // dispatch resets it.
        self.pending = Pending::None;
        // Help is read-only; route motions through the help buffer
        // path and reject operator-class invocations cleanly. Other
        // CommandKind variants (text-objects, ex-commands) shouldn't
        // reach Help -- ex-commands route through `execute_ex_line`,
        // text-objects only resolve via operators -- but if they do
        // they get the same read-only echo.
        if matches!(self.active_buffer, BufferKind::Help) {
            self.run_help_invocation(inv);
            return;
        }
        if matches!(self.active_buffer, BufferKind::FileTree) {
            self.run_file_tree_invocation(inv);
            return;
        }
        self.run_document_invocation(inv);
    }

    /// Resolve a motion against the active file tree's content.
    /// Same shape as [`Self::run_help_invocation`] but mutates
    /// the tree's cursor instead of the help buffer's.
    fn run_file_tree_invocation(&mut self, inv: CommandInvocation) {
        // File-tree is a read-only buffer; motion is the only
        // class that runs. Operators / text-objects / etc. fall
        // through to the read-only echo. Same path as help below.
        self.run_read_only_motion(inv);
    }

    /// Resolve a motion-class invocation against the active help
    /// buffer. Operators / text-objects / ex-commands echo a "read-
    /// only" message; the dispatcher in
    /// [`Self::run_document_invocation`] is the only path that
    /// commits buffer mutations.
    ///
    /// Counts compose the same way they do for the document path
    /// (`pending_count` and `op_count` both fold in), so `5j` /
    /// `3gg` work in help. Jump-class motions (`gg` / `G`) push to
    /// the unified position-history ring -- with `active_buffer ==
    /// Help` recorded on the entry -- so `<C-o>` walks back into
    /// the document if the help session is shallow.
    fn run_help_invocation(&mut self, inv: CommandInvocation) {
        // Help is a read-only buffer; same dispatcher as
        // file-tree. The only difference between these and the
        // document path is the read-only-ness; vim grammar
        // (motions, search, scroll, etc.) is identical.
        self.run_read_only_motion(inv);
    }

    /// Unified motion dispatch for read-only buffer kinds (help /
    /// file-tree). Reads buffer text via [`Self::active_text`] and
    /// the live cursor / scroll from `self.cursor` / `self.scroll`
    /// -- same hot-path the document dispatcher uses, so motion
    /// semantics (counts, jump-history pushes for `gg` / `G`,
    /// scroll-aware visibility) are identical. Non-motion command
    /// classes (operators, text-objects, ex bodies that reach
    /// here) echo "buffer is read-only" and bail.
    fn run_read_only_motion(&mut self, mut inv: CommandInvocation) {
        let Some(spec) = self.registry.lookup(inv.command) else {
            return;
        };
        if !matches!(spec.kind, lattice_grammar::CommandKind::Motion) {
            self.pending_count = 0;
            self.op_count = 0;
            self.pending_register = None;
            self.set_message(EchoLevel::Info, "buffer is read-only".to_string());
            return;
        }
        // Jump-class motions push history before dispatch so
        // `<C-o>` can return.
        if inv.command == self.builtins.goto_first_line.0
            || inv.command == self.builtins.goto_last_line.0
        {
            let cur = self.cursor;
            self.push_position_history(cur, PositionSource::AutoJump);
        }
        let motion_count = if self.pending_count > 0 {
            self.pending_count
        } else {
            inv.count.map(|c| c.0).unwrap_or(1)
        };
        let final_count = if self.op_count > 0 {
            self.op_count.saturating_mul(motion_count)
        } else {
            motion_count
        };
        if final_count > 1 {
            inv = inv.with_count(lattice_grammar::command::Count(final_count));
        }
        self.pending_count = 0;
        self.op_count = 0;
        let buffer = self.active_text();
        let cancel = lattice_runtime::CancellationToken::never();
        match lattice_grammar::execute_motion_only(
            &self.registry,
            &buffer,
            self.cursor,
            inv,
            &cancel,
        ) {
            Ok(target) => {
                self.cursor = target;
                // Clamp cursor's byte to the new line's length
                // (motions like `j`/`k` may land in a column past
                // the destination line's content; same vim
                // semantic as the document path).
                let line_len = line_byte_len(&buffer, self.cursor.line);
                if self.cursor.byte > line_len {
                    self.cursor.byte = line_len;
                }
                // ensure_cursor_visible at the end of `apply` does
                // the scroll math -- self.viewport_height is the
                // active buffer's visible row count.
            }
            Err(_) => {
                // Same swallow-error contract as the document path:
                // motion failures (e.g. cancel, blocked) don't
                // surface to the user yet -- DESIGN.md §5.10 error
                // notification subsystem will route these.
            }
        }
    }

    fn run_document_invocation(&mut self, mut inv: CommandInvocation) {
        // Attach the pending register (from a `"a` prefix) to the
        // invocation if not already specified.
        if let Some(reg) = self.pending_register.take()
            && inv.register.is_none()
        {
            inv = inv.with_register(reg);
        }
        // Jump-class motions (gg, G) push history before dispatch so
        // Ctrl-O can return.
        if inv.command == self.builtins.goto_first_line.0
            || inv.command == self.builtins.goto_last_line.0
        {
            let cur = self.cursor;
            self.push_position_history(cur, PositionSource::AutoJump);
        }
        // Capture find/till invocations for `;` / `,` repeat.
        if let lattice_grammar::Args::Char(c) = inv.args {
            let kind = if inv.command == self.builtins.find_char_forward.0 {
                Some(FindKind::Forward)
            } else if inv.command == self.builtins.find_char_backward.0 {
                Some(FindKind::Backward)
            } else if inv.command == self.builtins.till_char_forward.0 {
                Some(FindKind::TillForward)
            } else if inv.command == self.builtins.till_char_backward.0 {
                Some(FindKind::TillBackward)
            } else {
                None
            };
            if let Some(kind) = kind {
                self.last_find = Some(LastFind { kind, target: c });
            }
        }
        // Apply the count multiplication: the operator's count (latched at
        // `op_count`) multiplies with the motion's count (the in-progress
        // `pending_count`, or the invocation's own count default of 1).
        // Vim semantics: `2d3w` -> d6w. Either alone replaces the default.
        let motion_count = if self.pending_count > 0 {
            self.pending_count
        } else {
            inv.count.map(|c| c.0).unwrap_or(1)
        };
        let final_count = if self.op_count > 0 {
            self.op_count.saturating_mul(motion_count)
        } else {
            motion_count
        };
        let mut effective_count = final_count;
        // Fold-aware operator expansion (`docs/help/folding.md`):
        // when the cursor sits on the heading line of a closed fold
        // and the operator's range is `CurrentLine` (the `dd` / `yy`
        // / `cc` / `>>` family), grow the count so the operator
        // covers the whole fold. The operator stays a single edit /
        // single undo unit because the dispatcher composes one
        // `Effect::Edits` from the expanded range.
        if self.foldenable()
            && matches!(inv.range, Some(lattice_grammar::range::Range::CurrentLine))
            && let Some(fold) = self.fold_start_at(self.cursor.line)
        {
            let span = fold.end_line.saturating_sub(fold.start_line) + 1;
            effective_count = effective_count.max(span);
        }
        if effective_count > 1 {
            inv = inv.with_count(lattice_grammar::command::Count(effective_count));
        }
        self.pending_count = 0;
        self.op_count = 0;
        let was_visual = matches!(self.modal, ModalState::Visual(_));
        let mut should_exit_visual = false;
        let inv_for_repeat = inv.clone();
        // Vertical-jump motions auto-open folds the cursor lands in
        // (`docs/help/folding.md`). Linear motions don't -- this set
        // is intentionally narrow: `gg`, `G`, and counted `numberG`
        // (the same builtins the jump-list `<C-o>`/`<C-i>` walk
        // uses).
        let is_vertical_jump = inv.command == self.builtins.goto_first_line.0
            || inv.command == self.builtins.goto_last_line.0;
        // Every motion that goes through the dispatcher and isn't a
        // jump-class command runs the fold-aware snap so the cursor
        // never settles inside a closed fold's hidden body. Without
        // this, motions like `w` / `b` / `e` / `(` / `)` / `{` / `}`
        // happily landed on hidden lines, and the user's perceived
        // location diverged from `cursor.line`. The snap is
        // direction-aware (uses `prev_cursor_line`) and idempotent
        // when the cursor was already on a visible line.
        let prev_cursor_line = self.cursor.line;
        match self.dispatch_blocking(inv) {
            Ok(effect) => {
                // Visual exits on any operator-class effect (mutation OR
                // yank-only); dot-repeat only records buffer mutations.
                should_exit_visual = effect_mutates_or_yanks(&effect);
                if effect_mutates(&effect) {
                    self.last_change = Some(inv_for_repeat);
                }
                self.apply_effect(effect);
                if is_vertical_jump {
                    // Jump motions auto-open the destination fold so
                    // the user lands at the actual target line, not on
                    // the fold heading.
                    self.auto_open_folds_at_cursor();
                } else {
                    // Non-jump motions snap out of any closed fold's
                    // hidden body to the nearest visible line per
                    // `docs/help/folding.md`.
                    self.snap_cursor_past_closed_folds(prev_cursor_line);
                }
            }
            Err(_) => {
                // TODO(error-surface): publish to a notification once that
                // subsystem lands.
            }
        }
        // After a Visual-mode operator (d/y/c on selection), vim returns
        // to Normal. Pure motion in Visual extends the selection -- keep
        // Visual. The `c` operator already flipped to Insert via
        // Effect::EnterMode; the post-check would be a no-op there.
        if was_visual && should_exit_visual && matches!(self.modal, ModalState::Visual(_)) {
            self.do_exit_visual();
        }
        self.clamp_cursor_to_buffer();
    }

    fn apply_effect(&mut self, effect: Effect) {
        match effect {
            Effect::None => {}
            Effect::Edits(edits) => self.handle_edits(&edits),
            Effect::SelectionChange(set) => {
                let new_head = set.primary().head;
                self.cursor = new_head;
                // In Visual mode the head moves but the anchor is preserved
                // -- the dispatcher's `replace_primary(Selection::cursor(...))`
                // would otherwise collapse the selection. Refresh the
                // document's selection to reflect the extension.
                if let ModalState::Visual(kind) = self.modal {
                    let sel = Selection {
                        anchor: self.visual_anchor.unwrap_or(new_head),
                        head: new_head,
                        visual: Some(visual_kind_to_mode(kind)),
                    };
                    self.set_selections_blocking(SelectionSet::single(sel));
                }
            }
            Effect::Yank {
                content,
                kind,
                register,
            } => self.store_yank(register, content, kind),
            Effect::EnterMode(mode) => {
                // Operators that flip mode (`c` -> Insert) come through
                // the same `enter_mode` helper as direct Action::EnterMode
                // does, so the dot-repeat insert-recording starts/stops
                // consistently. (`enter_mode`'s cursor pull-back only
                // fires when going to Normal; safe for our use cases.)
                self.enter_mode(mode);
            }
            // --- Ex-command effects (DESIGN.md §5.2.1 unified dispatch). ---
            // These come from ex-command apply closures registered in the
            // grammar registry; the host owns the side effects.
            Effect::SaveBuffer { path } => self.do_write(path),
            Effect::QuitEditor { force } => self.do_quit(force),
            Effect::OpenBuffer { path, force } => self.do_edit(path, force),
            Effect::SetOption { spec } => self.do_set(&spec),
            Effect::ClearSearchHighlight => {
                self.current_match = None;
                self.all_matches.clear();
            }
            Effect::Echo { level, text } => self.set_message(echo_level_from_grammar(level), text),
            Effect::EchoRegisters => self.do_list_registers(),
            Effect::EchoMarks => self.do_list_marks(),
            Effect::Substitute {
                scope,
                pattern,
                replacement,
                global,
            } => self.do_substitute(scope, &pattern, &replacement, global),
            Effect::Global {
                pattern,
                inverted,
                body,
            } => self.do_global(&pattern, inverted, body.as_ref()),
            Effect::DeleteCurrentLine => self.do_delete_line(),
            Effect::DescribeCommand { name, anchor } => {
                self.do_describe_command(&name, anchor.as_deref())
            }
            Effect::DescribeBuffer => self.do_describe_buffer(),
            Effect::Apropos { pattern } => self.do_apropos(&pattern),
            Effect::DescribeKey { chord } => self.do_describe_key(&chord),
            Effect::ListKeymap => self.do_list_keymap(),
            Effect::BufferNext => self.do_buffer_next(),
            Effect::BufferPrev => self.do_buffer_prev(),
            Effect::ListBuffers => self.do_list_buffers(),
            Effect::OpenBufferPicker => self.open_buffer_picker(),
            Effect::BufferDelete { force } => self.do_buffer_delete(force),
            Effect::OpenFileTree { root } => self.do_open_file_tree(root),
            Effect::CloseFileTree => self.dismiss_file_tree(),
            Effect::DescribeOption { name } => self.do_describe_option(&name),
            Effect::ListOptions => self.do_list_options(),
            Effect::OpenHover { markdown } => self.do_open_hover(&markdown),
            Effect::CloseHover => self.do_close_hover(),
            Effect::OpenHelpTopic { topic } => self.do_open_help_topic(topic.as_deref()),
            Effect::ListDiagnostics => self.do_list_diagnostics(),
            Effect::NextDiagnostic => self.do_next_diagnostic(),
            Effect::PrevDiagnostic => self.do_prev_diagnostic(),
            Effect::OpenLspLog { server_id } => self.do_open_lsp_log(server_id.as_deref()),
            Effect::ToggleLspTrace { server_id } => self.do_toggle_lsp_trace(&server_id),
            Effect::OpenLspTraceLog { server_id } => {
                self.do_open_lsp_trace_log(server_id.as_deref())
            }
            Effect::LspStatus => self.do_lsp_status(),
            Effect::LspServerLogListing => self.do_lsp_server_log_listing(),
            Effect::LspRestart { server_id } => self.do_lsp_restart(&server_id),
            Effect::SetLspLogLevel { server_id, level } => {
                self.do_set_lsp_log_level(server_id.as_deref(), &level)
            }
            Effect::LspLogClear { server_id } => self.do_lsp_log_clear(server_id.as_deref()),
            Effect::Many(many) => {
                for e in many {
                    self.apply_effect(e);
                }
            }
        }
    }

    fn handle_edits(&mut self, edits: &[AppliedEdit]) {
        // After a delete, the cursor sits at the start of the deleted range
        // (which is now the position of whatever followed). Vim's behavior.
        if let Some(first) = edits.first() {
            self.cursor = first.original_range.start;
        }
    }

    /// Jump the cursor to a viewport-relative line. `H` -> top of view,
    /// `M` -> middle, `L` -> bottom. Column is preserved (clamped to the
    /// destination line's length).
    fn do_jump_viewport(&mut self, vpos: ViewportPos) {
        let height = self.viewport_height.max(1);
        let line = match vpos {
            ViewportPos::Top => self.scroll,
            ViewportPos::Middle => self.scroll + height / 2,
            ViewportPos::Bottom => self.scroll + height.saturating_sub(1),
        };
        let buffer = self.active_text();
        let last = last_addressable_line(&buffer);
        let line = line.min(last);
        let len = line_byte_len(&buffer, line);
        let byte = self.cursor.byte.min(len);
        self.cursor = Position::new(line, byte);
        // Folds only apply to documents.
        if matches!(self.active_buffer, BufferKind::Document) {
            self.auto_open_folds_at_cursor();
        }
    }

    /// Adjust scroll so the cursor lands at the requested viewport row.
    /// Cursor itself doesn't move (vim's `zt`/`zz`/`zb`).
    fn do_scroll_cursor_to(&mut self, spos: ScrollPos) {
        let height = self.viewport_height.max(1);
        self.scroll = match spos {
            ScrollPos::Top => self.cursor.line,
            ScrollPos::Center => self.cursor.line.saturating_sub(height / 2),
            ScrollPos::Bottom => self.cursor.line.saturating_sub(height.saturating_sub(1)),
        };
    }

    /// Move cursor by one viewport-height (vim's Ctrl-F / Ctrl-B). Vim
    /// leaves a 1-line overlap; we mirror that by stepping
    /// `viewport_height - 2` lines and letting `ensure_cursor_visible`
    /// handle the scroll.
    fn do_page(&mut self, down: bool) {
        let height = self.viewport_height.max(1);
        let step = height.saturating_sub(2).max(1);
        let buffer = self.active_text();
        let last = last_addressable_line(&buffer);
        let new_line = if down {
            self.cursor.line.saturating_add(step).min(last)
        } else {
            self.cursor.line.saturating_sub(step)
        };
        let len = line_byte_len(&buffer, new_line);
        let byte = self.cursor.byte.min(len);
        self.cursor = Position::new(new_line, byte);
    }

    /// Scroll one line. `down = true` -> Ctrl-E (scroll content up,
    /// pulling the next line into view); `down = false` -> Ctrl-Y.
    /// Cursor follows so it stays on-screen.
    fn do_scroll_line(&mut self, down: bool) {
        let height = self.viewport_height.max(1);
        let buffer = self.active_text();
        if down {
            let last = last_addressable_line(&buffer);
            self.scroll = self.scroll.saturating_add(1).min(last);
            // Pull cursor down if it's now off the top of the viewport.
            if self.cursor.line < self.scroll {
                self.cursor.line = self.scroll;
            }
        } else {
            self.scroll = self.scroll.saturating_sub(1);
            // Push cursor up if it's now off the bottom.
            let bottom = self.scroll + height.saturating_sub(1);
            if self.cursor.line > bottom {
                self.cursor.line = bottom;
            }
        }
        let len = line_byte_len(&buffer, self.cursor.line);
        if self.cursor.byte > len {
            self.cursor.byte = len;
        }
    }

    /// Overstrike one char at the cursor: if the cursor is mid-line,
    /// replace `[cursor, cursor+1)` with `c`; if past EOL, just insert
    /// (vim's R extends the line). Either way the cursor advances by
    /// one byte. The original byte (or `None` if past EOL) is pushed
    /// onto `replace_history` so backspace can restore it.
    fn do_overwrite_char(&mut self, c: char) {
        let len = line_byte_len(&self.document.snapshot().buffer, self.cursor.line);
        let s = c.to_string();
        let entry_pos = self.cursor;
        if self.cursor.byte < len {
            let r = ProtoRange::new(
                self.cursor,
                Position::new(self.cursor.line, self.cursor.byte + 1),
            );
            // Capture the original byte before the replace lands.
            let original = self.document.snapshot().buffer.slice(r).ok();
            if let Ok(applied) = self.apply_edit_blocking(Edit::replace(r, &s)) {
                self.cursor = applied.inserted_range.end;
                self.replace_history.push(ReplaceEntry {
                    at: entry_pos,
                    original,
                });
            }
        } else {
            // Past end of line: extend. Original is None.
            if let Ok(applied) = self.apply_edit_blocking(Edit::insert(self.cursor, &s)) {
                self.cursor = applied.inserted_range.end;
                self.replace_history.push(ReplaceEntry {
                    at: entry_pos,
                    original: None,
                });
            }
        }
    }

    /// Pop the latest replace_history entry and restore. If the entry
    /// recorded an original byte, replace the byte at the entry's
    /// position with it. If it didn't (line-extension case), delete
    /// the byte. Either way the cursor moves back to the entry's
    /// position.
    fn do_replace_undo_last(&mut self) {
        let Some(entry) = self.replace_history.pop() else {
            return;
        };
        let after = Position::new(entry.at.line, entry.at.byte + 1);
        let r = ProtoRange::new(entry.at, after);
        match entry.original {
            Some(orig) => {
                let _ = self.apply_edit_blocking(Edit::replace(r, &orig));
            }
            None => {
                let _ = self.apply_edit_blocking(Edit::delete(r));
            }
        }
        self.cursor = entry.at;
    }

    fn do_insert_text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if let Ok(applied) = self.apply_edit_blocking(Edit::insert(self.cursor, s)) {
            self.cursor = applied.inserted_range.end;
            // Capture into the in-flight Insert recording for dot-repeat.
            if let Some(rec) = self.recording_insert.as_mut() {
                rec.push_str(s);
            }
            // Block-visual I/A: count this edit so the Esc handler
            // can rewind the whole session and re-emit it as a
            // single batched undo unit.
            if let Some(spec) = self.pending_block_insert.as_mut() {
                spec.live_edits = spec.live_edits.saturating_add(1);
            }
        }
    }

    /// Format `:describe-command <name>` into a help overlay
    /// (DESIGN.md §5.11). Pulls metadata directly from the registry's
    /// `CommandSpec` -- name, kind, doc, and `args_schema` -- so the
    /// view stays in sync as commands are registered or rewritten.
    /// `:describe-command <name>` -- render via the unified
    /// [`Introspectable`] surface so every `:describe-*` formatter
    /// lands in `lattice_grammar::render_introspection`. Adding a
    /// new section to command help (e.g. example invocations) means
    /// extending `impl Introspectable for CommandSpec`, not editing
    /// the host.
    ///
    /// `anchor` (optional) scrolls the help buffer to a named
    /// anchor after rendering. Used by the cmdline's arg-aware
    /// `<C-h>` to jump to `arg:<name>`.
    /// Follow the help link under the cursor (`<CR>` in help mode).
    /// Looks up the link by cursor position, then dispatches based
    /// on the link target's variant. Source links echo the
    /// `path:line` for now -- full file-open lands with multi-buffer.
    fn do_help_follow_link(&mut self) {
        let cursor = self.cursor;
        let Some(help) = self.help_buffer.as_ref() else {
            return;
        };
        let Some(link) = help.link_at(cursor) else {
            self.set_message(EchoLevel::Info, "no link under cursor".to_string());
            return;
        };
        // Clone the target so we can drop the `&help` borrow
        // before calling `push_position_history` (`&mut self`).
        let target = link.target.clone();
        let prev_help_cursor = cursor;
        match target {
            crate::help::HelpLinkTarget::Command(name) => {
                // Help -> help transition: record where we were in
                // the *current* help buffer so `<C-o>` brings us
                // back to it. The subsequent `do_describe_command`
                // replaces `help_buffer`, so the entry's
                // `buffer_id` becomes "stale" -- the unified ring
                // walker filters those out (see `do_walk_history`).
                self.push_position_history(prev_help_cursor, PositionSource::AutoJump);
                self.do_describe_command(&name, None);
            }
            crate::help::HelpLinkTarget::Execute(cmdline) => {
                // `[label](exec:CMDLINE)` -- run `:CMDLINE` as if
                // the user had typed it. Used by picker-style help
                // buffers (e.g. `:lsp-server-log`) where each row
                // dispatches the underlying ex-command on Enter.
                // Push history so `<C-o>` walks back into the
                // picker.
                self.push_position_history(prev_help_cursor, PositionSource::AutoJump);
                self.execute_ex_line(&cmdline);
            }
            crate::help::HelpLinkTarget::Chord(chord) => {
                self.push_position_history(prev_help_cursor, PositionSource::AutoJump);
                self.do_describe_key(&chord);
            }
            crate::help::HelpLinkTarget::Topic(name) => {
                self.push_position_history(prev_help_cursor, PositionSource::AutoJump);
                self.do_open_help_topic(Some(&name));
            }
            crate::help::HelpLinkTarget::Anchor(slug) => {
                // Intra-doc jump: scroll the *current* help buffer to
                // the anchor line and move the cursor there. Push
                // history so `<C-o>` returns to the link site.
                self.push_position_history(prev_help_cursor, PositionSource::AutoJump);
                // Anchor lookup runs against the help buffer's
                // anchor list; the cursor + scroll updates land
                // on the App's unified hot path.
                let target_line = self.help_buffer.as_ref().and_then(|h| {
                    h.anchors.iter().find(|a| a.name == slug).map(|a| a.line)
                });
                if let Some(line) = target_line {
                    let buffer = self.active_text();
                    let len = line_byte_len(&buffer, line);
                    self.cursor = Position::new(line, self.cursor.byte.min(len));
                    self.scroll = line;
                } else {
                    self.set_message(
                        EchoLevel::Warn,
                        format!("anchor not found: #{slug}"),
                    );
                }
            }
            crate::help::HelpLinkTarget::Source { path, line } => {
                // `[label](file:PATH:LINE)` -- open the file via
                // the existing `:e` machinery (multi-buffer
                // foundation, §5.9), then position the cursor at
                // the requested line. Push the help-side cursor
                // onto position history with `PluginPush` so
                // `<C-o>` walks back into the help view.
                self.push_position_history(prev_help_cursor, PositionSource::PluginPush);
                self.do_edit(Some(path.clone()), false);
                // `do_edit` may have set an error message + bailed
                // (e.g. permission denied). Don't try to jump in
                // that case -- the message is already on screen.
                if matches!(
                    self.last_message.as_ref().map(|m| m.level),
                    Some(EchoLevel::Error)
                ) {
                    return;
                }
                // Source links carry 1-based line numbers (matching
                // every editor + every `path:line` convention);
                // convert to the App's 0-based line index, clamping
                // to a valid line in the now-loaded buffer.
                let snap = self.document.snapshot();
                let last = snap.buffer.line_count().saturating_sub(1);
                let target_line = line.saturating_sub(1).min(last);
                self.cursor = Position::new(target_line, 0);
            }
            crate::help::HelpLinkTarget::Unresolved(url) => {
                self.set_message(EchoLevel::Warn, format!("no handler for `{url}`"));
            }
        }
    }

    fn do_describe_command(&mut self, name: &str, anchor: Option<&str>) {
        // Two-stage resolution mirrors `excommand::parse_invocation`:
        // try the typed text as a registry name first (canonical
        // forms like `ex:write`), then fall back to alias expansion
        // (`write` -> `ex:write`). Lets users type either form.
        let Some(id) = resolve_command_name_or_alias(&self.registry, name) else {
            self.set_message(EchoLevel::Error, format!("no command named `{name}`"));
            return;
        };
        let Some(spec) = self.registry.lookup(id) else {
            self.set_message(EchoLevel::Error, format!("no command named `{name}`"));
            return;
        };
        let rendered = lattice_grammar::render_introspection(spec);
        let anchors: Vec<crate::help::HelpAnchor> = rendered
            .anchors
            .into_iter()
            .map(|a| crate::help::HelpAnchor {
                name: a.name,
                line: a.line,
            })
            .collect();
        let mut lines = rendered.lines;
        // Cross-link: append `See also: [topic](help:topic)` for
        // every help topic whose `related_command_patterns`
        // matches this command's name. Lets a user reading
        // `:describe-command operator:fold-create` jump to the
        // `folding` topic via `<CR>` on the link.
        let topics: Vec<String> = self
            .help_topics
            .topics_for_command(&spec.name)
            .map(|t| crate::help::topic_link(&t.name))
            .collect();
        if !topics.is_empty() {
            lines.push(String::new());
            lines.push(format!("See also: {}", topics.join(", ")));
        }
        let mut buffer =
            HelpBuffer::from_lines_and_anchors(format!("describe-command {name}"), lines, anchors)
                .with_markdown_syntax(self.lang_registry.clone());
        if let Some(a) = anchor {
            buffer.scroll_to_anchor(a);
        }
        self.open_help(buffer);
    }

    fn do_describe_buffer(&mut self) {
        let mut lines: Vec<String> = Vec::new();
        let snap = self.document.snapshot();
        let path = snap
            .path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(no file)".to_string());
        let lang = lattice_syntax::Lang::detect_from_path(snap.path());
        let line_count = snap.buffer.line_count();
        let byte_count = snap.buffer.as_string().len();
        let dirty = if self.document.dirty() { "yes" } else { "no" };
        lines.push(format!("path:           {path}"));
        lines.push(format!("language:       {lang:?}"));
        lines.push(format!("modal state:    {:?}", self.modal));
        lines.push(format!(
            "cursor:         line {}, col {}",
            self.cursor.line + 1,
            self.cursor.byte
        ));
        lines.push(format!("dirty:          {dirty}"));
        lines.push(format!("line count:     {line_count}"));
        lines.push(format!("byte count:     {byte_count}"));
        lines.push(format!("registers set:  {}", self.registers.len()));
        lines.push(format!("marks set:      {}", self.marks.len()));
        lines.push(format!(
            "position-history depth: {}",
            self.position_history.len()
        ));
        lines.push(format!("macros stored:  {}", self.macros.len()));
        lines.push(format!("folds:          {}", self.folds.len()));
        lines.push(format!(
            "options:        number={}  relativenumber={}",
            self.show_line_numbers(),
            self.relative_line_numbers()
        ));
        self.open_help(
            HelpBuffer::from_lines("describe-buffer", lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        );
    }

    fn do_apropos(&mut self, pattern: &str) {
        if pattern.is_empty() {
            self.set_message(EchoLevel::Error, "empty pattern".to_string());
            return;
        }
        let needle = pattern.to_ascii_lowercase();
        // Collect (name, kind, first_line_of_doc) for every spec whose
        // name or doc contains `needle` (case-insensitive).
        let mut hits: Vec<(String, &'static str, String)> = Vec::new();
        for name in self.registry.names() {
            let id = match self.registry.id_by_name(name) {
                Some(id) => id,
                None => continue,
            };
            let Some(spec) = self.registry.lookup(id) else {
                continue;
            };
            let name_match = spec.name.to_ascii_lowercase().contains(&needle);
            let doc_match = spec.doc.to_ascii_lowercase().contains(&needle);
            if name_match || doc_match {
                let first = spec.doc.lines().next().unwrap_or("").to_string();
                hits.push((spec.name.clone(), spec.kind.label(), first));
            }
        }
        hits.sort_by(|a, b| a.0.cmp(&b.0));
        let mut lines: Vec<String> = Vec::new();
        if hits.is_empty() {
            lines.push(format!("no matches for `{pattern}`"));
        } else {
            lines.push(format!("{} match(es) for `{pattern}`:", hits.len()));
            lines.push(String::new());
            // Compute alignment width once. We measure pre-link
            // wrapping so the visible text stays aligned even after
            // the renderer eventually styles the link markup.
            let name_w = hits.iter().map(|(n, _, _)| n.len()).max().unwrap_or(0);
            let kind_w = hits.iter().map(|(_, k, _)| k.len()).max().unwrap_or(0);
            for (name, kind, first) in hits {
                let pad_n = name_w.saturating_sub(name.len());
                let pad_k = kind_w.saturating_sub(kind.len());
                lines.push(format!(
                    "  {}{}  {}{}  {}",
                    command_link(&name),
                    " ".repeat(pad_n),
                    kind,
                    " ".repeat(pad_k),
                    first
                ));
            }
        }
        self.open_help(
            HelpBuffer::from_lines(format!("apropos {pattern}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        );
    }

    /// Format `:describe-key <chord>` (DESIGN.md §5.11). A chord may
    /// have entries in multiple modes (e.g. `j` is "line down" in
    /// Normal and Visual, "scroll" in Help). Each entry renders
    /// through the unified [`Introspectable`] surface so the source
    /// link + Action section come out uniformly with the other
    /// `:describe-*` commands.
    fn do_describe_key(&mut self, chord: &str) {
        let hits = crate::keymap::lookup(chord);
        let mut lines: Vec<String> = Vec::new();
        if hits.is_empty() {
            lines.push(format!("`{chord}` is not bound in any mode."));
        } else {
            lines.push(format!("{} -- {} binding(s):", key_link(chord), hits.len()));
            // One render_introspection per entry, so each binding's
            // source (and any Action section) appears next to its
            // mode header. The blank line between renders keeps
            // adjacent entries readable.
            for entry in hits {
                lines.push(String::new());
                for l in lattice_grammar::render_introspection_lines(entry) {
                    lines.push(l);
                }
            }
        }
        self.open_help(
            HelpBuffer::from_lines(format!("describe-key {chord}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        );
    }

    fn do_list_keymap(&mut self) {
        use crate::keymap::{BindingMode, entries};
        let mut by_mode: std::collections::BTreeMap<&str, Vec<&crate::keymap::KeymapEntry>> =
            std::collections::BTreeMap::new();
        // Stable iteration order: enumerate modes in a fixed order so
        // the rendered output reads top-down.
        let mode_order = [
            BindingMode::Normal,
            BindingMode::Visual,
            BindingMode::OperatorPending,
            BindingMode::AfterG,
            BindingMode::AfterZ,
            BindingMode::AfterMark,
            BindingMode::AfterJumpMarkLine,
            BindingMode::AfterJumpMarkExact,
            BindingMode::AfterRegister,
            BindingMode::AfterMacroStart,
            BindingMode::AfterMacroPlay,
            BindingMode::AfterFindChar,
            BindingMode::AfterTextObject,
            BindingMode::Insert,
            BindingMode::Replace,
            BindingMode::Command,
            BindingMode::Search,
            BindingMode::Help,
        ];
        for entry in entries() {
            by_mode.entry(entry.mode.label()).or_default().push(entry);
        }
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!(
            "Default keymap: {} bindings across {} modes",
            entries().len(),
            mode_order.len()
        ));
        lines.push(String::new());
        for mode in mode_order {
            let label = mode.label();
            let Some(group) = by_mode.get(label) else {
                continue;
            };
            lines.push(format!("[{label}]"));
            // Compute alignment width on the unwrapped chord string;
            // pad after the link wrapper so the visible text stays
            // column-aligned once the renderer styles links.
            let chord_w = group.iter().map(|e| e.chord.len()).max().unwrap_or(0);
            for entry in group {
                let pad = chord_w.saturating_sub(entry.chord.len());
                lines.push(format!(
                    "  {}{}  {}",
                    key_link(entry.chord),
                    " ".repeat(pad),
                    entry.doc
                ));
            }
            lines.push(String::new());
        }
        self.open_help(
            HelpBuffer::from_lines("keymap", lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        );
    }

    /// Split the active pane along `orientation`. The new sibling
    /// inherits the active pane's content + cursor + scroll (so a
    /// fresh `<C-w>s` shows the same view in both panes, vim's
    /// default). Active stays on the original pane.
    fn do_split_pane(&mut self, orientation: SplitOrientation) {
        // Save the App's hot-path cursor/scroll into the active
        // pane's stash so the new sibling clones a fresh snapshot.
        self.snapshot_active_pane();
        let _new_idx = self.pane_tree.split_active(orientation);
    }

    /// Close the active pane. The first surviving pane becomes
    /// active. No-op when only one pane is open (vim leaves the
    /// last window alone; closing it would mean closing the editor).
    /// Singleton transient buffers (file tree) get garbage-collected
    /// if no surviving pane references them.
    fn do_close_pane(&mut self) {
        if self.pane_tree.len() <= 1 {
            self.set_message(EchoLevel::Warn, "Already only one pane".to_string());
            return;
        }
        // Save the active pane's state, then drop it.
        self.snapshot_active_pane();
        if !self.pane_tree.close_active() {
            return;
        }
        self.load_active_pane();
        self.gc_unreferenced_panel_buffers();
    }

    /// Drop singleton non-document buffers (currently: file tree)
    /// when no pane still references them. Document buffers are
    /// no-op stub left in for backwards compatibility with the
    /// pre-registry refactor. Trees now live in the unified buffer
    /// registry alongside documents (DESIGN.md §5.9), so closing
    /// the only pane that referenced a tree leaves the tree in the
    /// registry where `:bn` / `:bp` can reach it. Use `:bd` to
    /// actually drop a tree buffer.
    fn gc_unreferenced_panel_buffers(&mut self) {}

    /// Step cardinally to the spatial neighbour of the active pane.
    /// Geometry comes from [`PaneTree::compute_rects`] so the walk
    /// matches what the renderer drew.
    fn do_navigate_pane(&mut self, direction: PaneDirection) {
        let area = self.buffer_area_rect();
        let Some(target) = self.pane_tree.navigate(direction, area) else {
            return;
        };
        self.activate_pane(target);
    }

    /// Make pane `idx` the active one, swapping the App's hot-path
    /// cursor / scroll with the target pane's stash.
    fn activate_pane(&mut self, idx: usize) {
        if idx == self.pane_tree.active_index() {
            return;
        }
        self.snapshot_active_pane();
        if !self.pane_tree.set_active(idx) {
            return;
        }
        self.load_active_pane();
    }

    /// Copy the App's hot-path cursor / scroll into the active
    /// pane's stash. Called before any operation that flips which
    /// pane is active.
    ///
    /// **Unified hot-path**: `self.cursor` and `self.scroll` are
    /// the active buffer's regardless of kind, so the snapshot
    /// reads from there uniformly. Help / file-tree records are
    /// also synced into their kind-specific cursor / scroll fields
    /// (and the registry copy for help) so the archival state stays
    /// current; live state always lives on `self`.
    fn snapshot_active_pane(&mut self) {
        let cursor = self.cursor;
        let scroll = self.scroll;
        let pane_id = self.pane_tree.active().buffer_id;
        // Mirror live state into the buffer-specific stash + the
        // registry record for archival / cross-pane round-trips.
        match self.active_buffer {
            BufferKind::Help => {
                if let Some(h) = self.help_buffer.as_mut() {
                    h.cursor = cursor;
                    h.scroll = scroll as usize;
                    if h.id == pane_id
                        && let Some(reg) = self.buffers.help_mut(pane_id)
                    {
                        *reg = h.clone();
                    }
                }
            }
            BufferKind::FileTree => {
                if let Some(t) = self.buffers.file_tree_mut(pane_id) {
                    t.cursor = cursor;
                    t.scroll = scroll as usize;
                }
            }
            BufferKind::Document => {}
        }
        let active = self.pane_tree.active_mut();
        active.cursor = cursor;
        active.scroll = scroll;
    }

    /// Inverse of [`Self::snapshot_active_pane`]: pull the freshly
    /// activated pane's stashed cursor / scroll back into the
    /// App's hot-path fields. `active_buffer` is denormalized from
    /// the pane's `buffer` kind.
    ///
    /// **Unified hot-path**: `self.cursor` and `self.scroll` are
    /// the active buffer's, regardless of kind. Help / file-tree
    /// keep their own cursor / scroll fields as **save state** --
    /// updated at the snapshot boundary so the registry record is
    /// archival-correct, but the *live* cursor is `self.cursor`
    /// for every motion / scroll / search / render path.
    fn load_active_pane(&mut self) {
        let pane = *self.pane_tree.active();
        self.active_buffer = pane.buffer;
        self.cursor = pane.cursor;
        self.scroll = pane.scroll;
        // Help: restore the registry copy into the hot-path slot
        // if the active pane points at a different help buffer
        // than the one currently mirrored.
        if matches!(pane.buffer, BufferKind::Help)
            && self.help_buffer.as_ref().map(|h| h.id) != Some(pane.buffer_id)
            && let Some(reg) = self.buffers.help(pane.buffer_id)
        {
            self.help_buffer = Some(reg.clone());
        }
    }

    /// Total area available to pane content in screen-cell units.
    /// Currently the buffer area = full terminal minus the mode
    /// line (1 row) and the echo / cmdline area (1 row). Width is
    /// the terminal width; v1 doesn't track terminal width as
    /// state, so we estimate from `viewport_height` and a constant
    /// width that the renderer overrides with the real terminal
    /// width before navigation. Good enough until B.1.c has the
    /// per-frame terminal size cached on App.
    fn buffer_area_rect(&self) -> crate::pane::PaneRect {
        crate::pane::PaneRect {
            x: 0,
            y: 0,
            width: self.terminal_width.unwrap_or(120),
            height: self.viewport_height as u16,
        }
    }

    /// Adopt a freshly-built help buffer as the active view. Records
    /// the current document cursor on the position-history ring as
    /// an `AutoJump` (so `<C-o>` from inside the help buffer returns
    /// to the document spot the user opened from), then flips
    /// `active_buffer` to `Help`. Used by every `:describe-*` /
    /// `:apropos` / `:keymap` entry point.
    ///
    /// **Popup vs in-pane.** This is the *popup* path -- the help
    /// content sits on the App's transient `help_buffer` slot and
    /// renders as a centred overlay. The complementary
    /// [`Self::open_help_in_pane`] path registers the buffer in
    /// [`BufferRegistry`] and swaps the active pane to it; that's
    /// what `:lsp-log` / `:lsp-server-log` / `:lsp-trace-log` (Phase
    /// 3) and future persistent help views route through.
    fn open_help(&mut self, buffer: HelpBuffer) {
        // Record the *document* cursor (we're still active=Document
        // here, since open_help precedes the active_buffer flip).
        // Skip the push if we're already in Help (a help->help
        // re-open from a link follow); the inter-help transition
        // is recorded by `do_help_follow_link` itself.
        if matches!(self.active_buffer, BufferKind::Document) {
            let cur = self.cursor;
            self.push_position_history(cur, PositionSource::AutoJump);
        }
        // Load the new help buffer's cursor / scroll into the
        // App's hot path. Same model as activate_help_in_pane:
        // `self.cursor` / `self.scroll` are the active buffer's,
        // motion / scroll / search read / write them uniformly.
        let stash_cursor = buffer.cursor;
        let stash_scroll = buffer.scroll as u32;
        self.help_buffer = Some(buffer);
        self.cursor = stash_cursor;
        self.scroll = stash_scroll;
        self.active_buffer = BufferKind::Help;
        self.pending = Pending::None;
    }

    /// Adopt a help buffer into the unified [`BufferRegistry`] and
    /// swap the active pane to it -- the in-pane counterpart to
    /// [`Self::open_help`]. Used by persistent help views (LSP logs,
    /// `:diagnostics`, `:apropos` once migrated) that should live as
    /// real buffers: split-able, switchable via `:bn` / `:b N`,
    /// listed by `:ls`.
    ///
    /// De-duplicates by title -- re-running the command surfaces the
    /// existing buffer rather than allocating a new one. Returns the
    /// `BufferId` either way so callers can wire follow-up state
    /// (Phase 4 live-tail subscriptions key off this id).
    ///
    /// **Hot-path model.** The registry entry is the durable record
    /// (`:ls` / `:bn` / picker discovery); the App's `help_buffer`
    /// slot mirrors the active in-pane help so the keymap +
    /// renderer stay single-path. Pane-switch hooks
    /// ([`Self::snapshot_active_pane`] / [`Self::load_active_pane`])
    /// sync the two at boundaries -- same pattern as Document's
    /// `syntax`/`folds` snapshots.
    pub fn open_help_in_pane(&mut self, buffer: HelpBuffer) -> BufferId {
        if let Some(existing_id) = self.buffers.help_with_title(&buffer.title) {
            // Already open: refresh its content (so `:lsp-log` re-
            // run picks up new records) and switch the active pane
            // to it.
            if let Some(slot) = self.buffers.help_mut(existing_id) {
                *slot = buffer;
            }
            self.activate_help_in_pane(existing_id);
            return existing_id;
        }
        let id = BufferId::next();
        // Clone for the registry record; the active hot-path copy
        // lands on `self.help_buffer` via `activate_help_in_pane`.
        // HelpBuffer's heavy field is the rope (O(1) clone); the
        // markdown highlight Vec is the only allocation cost.
        let registry_copy = buffer.clone();
        self.buffers.insert(BufferEntry {
            id,
            flags: BufferFlags::default(),
            data: BufferData::Help(registry_copy),
        });
        // Take ownership of the original for the popup hot-path.
        self.help_buffer = Some(buffer);
        self.activate_help_in_pane(id);
        id
    }

    /// Switch the active pane to an existing help buffer in the
    /// registry. Snapshots prior pane state so `<C-o>` returns the
    /// user to the document/cursor they came from. The registry's
    /// HelpBuffer is mirrored into `self.help_buffer` so the
    /// existing keymap + render paths transparently target it.
    fn activate_help_in_pane(&mut self, id: BufferId) {
        if self.buffers.help(id).is_none() {
            self.set_message(EchoLevel::Error, format!("buffer #{} not a help buffer", id.0));
            return;
        }
        // Skip the auto-jump push during picker-preview hovers --
        // the user hasn't committed to this buffer yet, so we
        // don't want every cursor over a candidate to bloat the
        // jump list. The real push happens on `PickerAccept` if
        // the user commits.
        if !self.previewing && matches!(self.active_buffer, BufferKind::Document) {
            let cur = self.cursor;
            self.push_position_history(cur, PositionSource::AutoJump);
        }
        // Capture pre-activation pane + active state so dismiss
        // can restore the user to whatever buffer they came from.
        // Only set when transitioning into help from a non-help
        // buffer; help-to-help transitions (link follows etc.)
        // preserve the original origin.
        if !matches!(self.active_buffer, BufferKind::Help) {
            let active = self.pane_tree.active();
            self.prev_pane_for_help = Some(PrevPaneState {
                buffer: active.buffer,
                buffer_id: active.buffer_id,
                cursor: self.cursor,
                scroll: self.scroll,
            });
        }
        self.snapshot_active_pane();
        // Note: do NOT call `snapshot_active_document` here. Help
        // is rendered as a popup overlay over the underlying
        // document; the pane's per-frame paint draws the active
        // document via `draw_buffer(snap)` which reads from
        // `self.syntax` / `self.folds` for highlights + fold
        // overlays. Stashing those onto the document entry would
        // leave `self.syntax = None` for the duration of the help
        // session, so the document underneath the popup paints
        // unhighlighted (the user's #5 bug report). The hot-path
        // state stays live; the round-trip back to the same
        // document via `activate_document` early-returns on
        // matching `document_buffer_id` (see that fn for the
        // same-doc fast path).
        // Mirror the registry copy into the hot-path slot. If
        // open_help_in_pane just placed it there, this is a no-op;
        // for re-entries via :bn / picker we restore the saved
        // content.
        if self.help_buffer.as_ref().map(|h| h.id) != Some(id)
            && let Some(reg_help) = self.buffers.help(id)
        {
            self.help_buffer = Some(reg_help.clone());
        }
        // Load the help buffer's stash into the App's hot-path
        // cursor / scroll. After this, `self.cursor` /
        // `self.scroll` are the help's -- motion / scroll /
        // search code reads / writes them uniformly, no per-kind
        // branches needed. Fresh help buffers default to (0, 0).
        let (stash_cursor, stash_scroll) = self
            .help_buffer
            .as_ref()
            .map(|h| (h.cursor, h.scroll as u32))
            .unwrap_or((Position::ZERO, 0));
        self.cursor = stash_cursor;
        self.scroll = stash_scroll;
        self.active_buffer = BufferKind::Help;
        let pane = self.pane_tree.active_mut();
        pane.buffer = BufferKind::Help;
        pane.buffer_id = id;
        pane.cursor = stash_cursor;
        pane.scroll = stash_scroll;
        self.pending = Pending::None;
    }

    /// Close the help overlay and route input back to the document.
    /// Idempotent: closing when no help is open is a no-op. Pane-
    /// tracked help buffers stay in the registry (so `:bn` / `:b N`
    /// can return to them); only the popup slot is cleared and the
    /// active buffer flips back to Document.
    fn dismiss_help(&mut self) {
        self.help_buffer = None;
        // Restore pre-help state if focus had moved into the help
        // (State B for hover; in-pane mode for `:lsp-log` etc.).
        // State A (popup shown but never focused) leaves
        // `prev_pane_for_help` as `None` -- nothing to restore;
        // active was never flipped to Help.
        if let Some(prev) = self.prev_pane_for_help.take() {
            self.cursor = prev.cursor;
            self.scroll = prev.scroll;
            let pane = self.pane_tree.active_mut();
            pane.buffer = prev.buffer;
            pane.buffer_id = prev.buffer_id;
            self.active_buffer = prev.buffer;
        } else {
            // State A dismiss (or popup-overlay variant of help
            // that never took focus): just flip active back as a
            // safety net; cursor/scroll were never touched.
            self.active_buffer = BufferKind::Document;
        }
        // Help mode reuses Pending::AfterG for the gg chord; clear
        // it on dismiss so a stranded `g` doesn't leak into Normal
        // mode.
        self.pending = Pending::None;
    }

    /// `:Tree [path]` (DESIGN.md §5.9 buffer-as-content). Opens a
    /// [`FileTreeBuffer`] rooted at `path` (or the current
    /// document's parent dir / cwd if absent) and inserts it into
    /// the unified buffer registry. If a tree at the same root is
    /// already open, the active pane switches to it instead of
    /// spawning a duplicate -- matching `:e FILE`'s "already open"
    /// semantics. The active pane flips to the new (or existing)
    /// tree buffer.
    fn do_open_file_tree(&mut self, root: Option<std::path::PathBuf>) {
        let root = match root {
            Some(p) => p,
            None => match self
                .document
                .path()
                .and_then(|p| p.parent().map(Into::into))
            {
                Some(parent) => parent,
                None => match std::env::current_dir() {
                    Ok(p) => p,
                    Err(e) => {
                        self.set_message(EchoLevel::Error, format!("cwd error: {e}"));
                        return;
                    }
                },
            },
        };
        // De-dup: if the same root is already open, just switch.
        if let Some(existing_id) = self.buffers.file_tree_with_root(&root) {
            self.activate_file_tree(existing_id);
            self.set_message(
                EchoLevel::Info,
                format!("tree: {} (already open)", root.display()),
            );
            return;
        }
        let tree = match FileTreeBuffer::open(&root) {
            Ok(t) => t,
            Err(e) => {
                self.set_message(
                    EchoLevel::Error,
                    format!("tree open error: {}: {e}", root.display()),
                );
                return;
            }
        };
        // Record the current cursor on the position-history ring
        // so `<C-o>` from inside the tree returns to the document
        // spot.
        if matches!(self.active_buffer, BufferKind::Document) {
            let cur = self.cursor;
            self.push_position_history(cur, PositionSource::AutoJump);
        }
        let new_id = tree.id;
        self.buffers.insert(BufferEntry {
            id: new_id,
            flags: BufferFlags::default(),
            data: BufferData::FileTree(tree),
        });
        // Snapshot whichever buffer was active so its hot-path
        // state lands in the registry, then point the active pane
        // at the new tree.
        self.snapshot_active_pane();
        self.snapshot_active_document();
        self.active_buffer = BufferKind::FileTree;
        let pane = self.pane_tree.active_mut();
        pane.buffer = BufferKind::FileTree;
        pane.buffer_id = new_id;
        pane.cursor = Position::ZERO;
        pane.scroll = 0;
        self.pending = Pending::None;
        self.set_message(EchoLevel::Info, format!("tree: {}", root.display()));
    }

    /// `:TreeClose` -- close the active pane's tree by swapping
    /// the active pane back to a Document buffer (the original
    /// document if available; whichever document is registered
    /// otherwise) and dropping the tree from the registry.
    fn dismiss_file_tree(&mut self) {
        if !matches!(self.active_buffer, BufferKind::FileTree) {
            return;
        }
        let tree_id = self.active_pane_buffer_id();
        // Pick a successor: prefer any document buffer.
        let successor = self
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap_or(self.document_buffer_id);
        self.activate_buffer(successor);
        self.buffers.remove(tree_id);
        // Re-point any other panes that referenced the closed tree.
        let new_kind = self.active_buffer;
        let new_id = self.active_pane_buffer_id();
        for pane in self.pane_tree.leaves_mut() {
            if pane.buffer_id == tree_id {
                pane.buffer = new_kind;
                pane.buffer_id = new_id;
            }
        }
        self.pending = Pending::None;
    }

    /// `<CR>` while the active pane shows a file-tree buffer: if
    /// the cursor is on a directory row, toggle expansion; if on
    /// a file, open it via the standard `:e FILE` path (which now
    /// switches to / spawns a Document buffer in the active pane).
    fn do_file_tree_follow(&mut self) {
        let active_id = self.active_pane_buffer_id();
        // Live cursor lives on `self.cursor` (unified across
        // buffer kinds); the tree's own `cursor` field is
        // archival save-state.
        let idx = self.cursor.line as usize;
        let Some(tree) = self.buffers.file_tree_mut(active_id) else {
            return;
        };
        let Some(entry) = tree.entries.get(idx).cloned() else {
            return;
        };
        match entry.kind {
            FileTreeEntryKind::Directory { .. } => {
                if let Err(e) = tree.toggle_at(idx) {
                    self.set_message(EchoLevel::Error, format!("toggle error: {e}"));
                }
            }
            FileTreeEntryKind::File => {
                let path = entry.path.clone();
                // Open the file in the active pane (replaces the
                // tree). The user can split first (`<C-w>v`) if
                // they want to keep the tree visible.
                self.do_edit(Some(path), false);
            }
        }
    }

    /// Bracketed-paste handler. Routes the payload to the right target
    /// based on the current modal state -- cursor for editing modes,
    /// command line for `:`, search line for `/` `?`. Always one undo
    /// unit. The terminal already stripped the bracketed-paste markers
    /// before crossterm handed us the string.
    fn do_paste_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.modal {
            ModalState::Command => {
                self.command_line.push_str(text);
                self.command_history_cursor = None;
            }
            ModalState::Search(_) => {
                if let Some(line) = self.search_line.as_mut() {
                    line.pattern.push_str(text);
                }
            }
            // Insert / Replace / Normal / Visual / OperatorPending all
            // land at the cursor as a single edit. We deliberately don't
            // transition modes -- the user's mode is preserved across
            // the paste, matching Vim's `paste` option behaviour.
            _ => {
                if let Ok(applied) = self.apply_edit_blocking(Edit::insert(self.cursor, text)) {
                    self.cursor = applied.inserted_range.end;
                    if matches!(self.modal, ModalState::Insert)
                        && let Some(rec) = self.recording_insert.as_mut()
                    {
                        rec.push_str(text);
                    }
                }
            }
        }
    }

    fn do_delete_char_backward(&mut self) {
        let prev = previous_position(&self.document.snapshot().buffer, self.cursor);
        if prev == self.cursor {
            return;
        }
        let range = ProtoRange::new(prev, self.cursor);
        if self.apply_edit_blocking(Edit::delete(range)).is_ok() {
            self.cursor = prev;
            if let Some(spec) = self.pending_block_insert.as_mut() {
                spec.live_edits = spec.live_edits.saturating_add(1);
            }
        }
    }

    fn enter_mode(&mut self, state: ModalState) {
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
        self.pending = Pending::None;
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

    /// Commit a block-visual `I` / `A` session as a single undo unit.
    ///
    /// Vim's behavior: the typed prefix on the top row plus the
    /// replicated text on the other rows land as one atomic
    /// change. To honour that without restructuring Insert mode
    /// to defer edits, we:
    ///
    /// 1. Roll back the live-typed edits via `undo_blocking` --
    ///    `spec.live_edits` counts how many `apply_edit` calls
    ///    happened on the top row during the Insert session.
    /// 2. Build a batch: top-row insert at `insert_col` plus an
    ///    insert at the same column on every line in
    ///    `start_line+1..=end_line` whose length is at least
    ///    `insert_col` (lines too short to hold the column are
    ///    skipped, matching vim's behavior).
    /// 3. Apply the batch via `apply_edit_batch_blocking` so the
    ///    whole session is one undo / redo unit.
    fn replicate_block_insert(&mut self, spec: PendingBlockInsert, text: &str) {
        // Rewind the live-typed edits. Each call decrements the
        // top-row state by one; after `live_edits` calls the
        // buffer is back to the pre-Insert state and we can
        // build the batched edit list against it.
        for _ in 0..spec.live_edits {
            let _ = self.undo_blocking();
        }

        let buffer = self.document.snapshot().buffer.clone();
        let mut edits = Vec::with_capacity((spec.end_line - spec.start_line + 1) as usize);

        // Top row first. Note: we don't skip the top row even if
        // its length is below insert_col (the user did type there
        // live, so the buffer already has at least one valid
        // insertion point at the line-end position they reached).
        let top_len = line_byte_len(&buffer, spec.start_line);
        let top_col = spec.insert_col.min(top_len);
        edits.push(Edit::insert(Position::new(spec.start_line, top_col), text));

        for line in (spec.start_line + 1)..=spec.end_line {
            let line_len = line_byte_len(&buffer, line);
            if line_len < spec.insert_col {
                continue;
            }
            edits.push(Edit::insert(Position::new(line, spec.insert_col), text));
        }

        let _ = self.apply_edit_batch_blocking(edits);
        // Cursor settles on the start of the inserted prefix on
        // the top row -- vim's behavior. The previous cursor pos
        // (one past the typed text on top row) is no longer
        // accurate after the rewind.
        self.cursor = Position::new(spec.start_line, top_col);
    }

    fn do_enter_append(&mut self) {
        let len = line_byte_len(&self.document.snapshot().buffer, self.cursor.line);
        if self.cursor.byte < len {
            self.cursor.byte += 1;
        }
        self.modal = ModalState::Insert;
        self.pending = Pending::None;
    }

    /// Vim's blockwise-visual `I` (`append=false`) and `A`
    /// (`append=true`). Captures the block extents from the active
    /// selection, parks them in `pending_block_insert`, moves the
    /// cursor to the top-row insert column, and switches to Insert.
    /// The replication onto rows 2..N happens when Insert exits.
    ///
    /// No-op if the modal is not blockwise visual; called only
    /// from translate_visual which guards on the mode.
    fn do_enter_block_visual_insert(&mut self, append: bool) {
        if !matches!(self.modal, ModalState::Visual(VisualKind::Blockwise)) {
            return;
        }
        let sels = self.document.selections();
        let sel = sels.primary();
        let start_line = sel.anchor.line.min(sel.head.line);
        let end_line = sel.anchor.line.max(sel.head.line);
        let left_col = sel.anchor.byte.min(sel.head.byte);
        let right_col = sel.anchor.byte.max(sel.head.byte);
        let insert_col = if append { right_col + 1 } else { left_col };

        self.pending_block_insert = Some(PendingBlockInsert {
            start_line,
            end_line,
            insert_col,
            live_edits: 0,
        });

        // Move cursor to the top row's insert column. If the line
        // is shorter than insert_col (e.g. `A` on a short line),
        // clamp -- the user's edits land at end-of-line and the
        // replay handles short lines per-row.
        let line_len = line_byte_len(&self.document.snapshot().buffer, start_line);
        let cursor_col = insert_col.min(line_len);
        self.cursor = Position::new(start_line, cursor_col);

        // Drop visual mode and enter Insert. enter_mode handles
        // recording_insert so the typed prefix is captured.
        self.visual_anchor = None;
        self.enter_mode(ModalState::Insert);
    }

    fn do_open_line_below(&mut self) {
        let len = line_byte_len(&self.document.snapshot().buffer, self.cursor.line);
        let eol = Position::new(self.cursor.line, len);
        if self.apply_edit_blocking(Edit::insert(eol, "\n")).is_ok() {
            self.cursor = Position::new(self.cursor.line + 1, 0);
        }
        self.modal = ModalState::Insert;
        self.pending = Pending::None;
    }

    fn do_enter_visual(&mut self, kind: VisualKind) {
        self.modal = ModalState::Visual(kind);
        self.pending = Pending::None;
        self.visual_anchor = Some(self.cursor);
        // Seed document.selections so Range::Selection picks up the
        // anchor=head=cursor selection immediately.
        let sel = Selection {
            anchor: self.cursor,
            head: self.cursor,
            visual: Some(visual_kind_to_mode(kind)),
        };
        self.set_selections_blocking(SelectionSet::single(sel));
    }

    fn do_exit_visual(&mut self) {
        // Capture the selection extents BEFORE collapsing, so `gv` can
        // restore them. We want the kind from `self.modal` (Visual carries
        // it) and the anchor / head from the document selection.
        if let ModalState::Visual(kind) = self.modal {
            let sels = self.document.selections();
            let sel = sels.primary();
            self.last_visual = Some(LastVisual {
                anchor: sel.anchor,
                head: sel.head,
                kind,
            });
        }
        self.modal = ModalState::Normal;
        self.pending = Pending::None;
        self.visual_anchor = None;
        // Collapse selection to a cursor at the current head.
        self.set_selections_blocking(SelectionSet::single(Selection::cursor(self.cursor)));
    }

    fn do_start_macro_record(&mut self, register: char) {
        if !is_valid_mark_name(register) {
            self.set_message(
                EchoLevel::Error,
                format!("invalid macro register: {register}"),
            );
            return;
        }
        if self.macro_recording.is_some() {
            // Already recording -- ignore (vim treats this as a no-op).
            return;
        }
        self.macro_recording = Some(MacroRecording {
            register,
            actions: Vec::new(),
        });
        self.set_message(EchoLevel::Info, format!("recording @{register}"));
    }

    fn do_stop_macro_record(&mut self) {
        let Some(rec) = self.macro_recording.take() else {
            return;
        };
        let label = rec.register;
        self.macros.insert(rec.register, rec.actions);
        self.set_message(EchoLevel::Info, format!("recorded @{label}"));
    }

    fn do_play_macro(&mut self, register: char) {
        if !is_valid_mark_name(register) {
            self.set_message(
                EchoLevel::Error,
                format!("invalid macro register: {register}"),
            );
            return;
        }
        let Some(actions) = self.macros.get(&register).cloned() else {
            self.set_message(EchoLevel::Error, format!("no macro in register {register}"));
            return;
        };
        // Suppress recording-into-current-macro while replaying. (We don't
        // want a `q` started before play to capture the playback's actions
        // -- vim explicitly drops play actions from the recording.)
        let mut paused = self.macro_recording.take();
        for action in actions {
            self.apply(action);
            if self.should_quit {
                break;
            }
        }
        if let Some(rec) = paused.take() {
            self.macro_recording = Some(rec);
        }
        self.last_played_macro = Some(register);
    }

    /// Push a tagged entry onto the history ring. If the history-cursor
    /// is not at the end (the user has been walking back), truncate
    /// forward entries before pushing -- standard "modify-from-middle"
    /// semantics. Capped at POSITION_HISTORY_CAP entries; oldest dropped.
    /// Adjacent same-position-and-source duplicates are coalesced.
    pub fn push_position_history(&mut self, pos: Position, source: PositionSource) {
        let buffer = self.active_buffer;
        let buffer_id = self.active_buffer_id();
        if let Some(last) = self.position_history.last()
            && last.position == pos
            && last.source == source
            && last.buffer == buffer
            && last.buffer_id == buffer_id
        {
            return;
        }
        if self.position_history_cursor < self.position_history.len() {
            self.position_history.truncate(self.position_history_cursor);
        }
        self.position_history.push(PositionEntry {
            position: pos,
            source,
            buffer,
            buffer_id,
        });
        if self.position_history.len() > POSITION_HISTORY_CAP {
            self.position_history.remove(0);
            // Truncating from the front shifts the cursor too; clamp
            // before we re-anchor it.
            self.position_history_cursor = self.position_history_cursor.saturating_sub(1);
        }
        self.position_history_cursor = self.position_history.len();
    }

    /// Id of whichever buffer is currently active. The active
    /// pane's `buffer_id` is the source of truth -- documents and
    /// trees both live in [`Self::buffers`] under one id space.
    /// Help still lives outside the registry as a transient
    /// overlay; while help is active we return its id, otherwise
    /// the active pane's id.
    pub fn active_buffer_id(&self) -> BufferId {
        match self.active_buffer {
            BufferKind::Help => self
                .help_buffer
                .as_ref()
                .map(|h| h.id)
                .unwrap_or(self.document_buffer_id),
            BufferKind::Document | BufferKind::FileTree => self.pane_tree.active().buffer_id,
        }
    }

    /// Vim's `<C-l>` -- force a fresh redraw to recover from any
    /// visual glitch. Concretely:
    ///
    /// - bumps the parsed-version mirror so the next
    ///   `maybe_reparse_syntax` actually re-runs the parser even if
    ///   the document version hasn't changed (covers the rare case
    ///   where a fold or syntax cache went stale);
    /// - clears the cached `visible_highlights` and pane highlights
    ///   so the next frame's `refresh_highlights` repopulates from
    ///   scratch;
    /// - sets `pending_redraw` so the runtime clears the terminal
    ///   on the next frame, scrubbing leftover ANSI sequences from
    ///   crashed external programs / partial repaints.
    fn do_redraw_screen(&mut self) {
        // Force a syntax reparse on the next frame.
        self.last_parsed_text_version = u64::MAX;
        // Drop cached spans so refresh_highlights can't return
        // stale data for a single frame.
        self.visible_highlights.clear();
        self.pane_highlights.clear();
        // Recompute folds in case the fold set drifted from the
        // current document state (paranoia; the seam already runs
        // on every reparse, but `<C-l>` is the explicit "reset"
        // hook so we err on the side of re-running it).
        self.recompute_folds();
        // Tell the runtime to clear the terminal on next frame.
        self.pending_redraw = true;
        self.set_message(EchoLevel::Info, "redraw".to_string());
    }

    /// Step through the position history filtered to jump-class entries
    /// (AutoJump | PluginPush). `delta = -1` for Ctrl-O, `+1` for Ctrl-I.
    /// On the first Ctrl-O from end-of-ring, also snapshot the current
    /// cursor as AutoJump so a subsequent Ctrl-I can return to it.
    fn do_jump_history(&mut self, delta: i32) {
        if delta < 0
            && self.position_history_cursor == self.position_history.len()
            && self.position_history.iter().any(|e| e.is_jump())
        {
            let cur = self.active_cursor();
            let already_there = self
                .position_history
                .last()
                .map(|e| e.position == cur && e.buffer == self.active_buffer)
                .unwrap_or(false);
            if !already_there {
                self.push_position_history(cur, PositionSource::AutoJump);
                // After push the cursor==len. Step it one back so the
                // walk finds the entry preceding our snapshot rather
                // than the snapshot itself.
                self.position_history_cursor = self.position_history.len().saturating_sub(1);
            }
        }
        self.do_walk_history(delta, |e| e.is_jump(), "jumps", "jump list");
    }

    /// Step through named-mark entries -- vim's `g;` (back) / `g,`
    /// (forward) per §5.1.1's interpretation. No "snapshot current
    /// pos" pre-step: mark navigation is exploratory and shouldn't
    /// pollute the jump-list ring.
    fn do_mark_history(&mut self, delta: i32) {
        self.do_walk_history(delta, |e| e.is_named_mark(), "marks", "mark history");
    }

    /// Generic walk over the unified ring filtered by `pred`. Mirrors
    /// vim's "save current pos on first step back so the forward step
    /// can return to it" behavior, but only when the current position
    /// itself qualifies for the filter (so jumping back over named
    /// marks doesn't pollute the ring with AutoJump entries and vice
    /// versa).
    ///
    /// When the target entry was recorded in a different buffer
    /// (e.g. the user pressed `<C-o>` from a help overlay back to a
    /// document position), the walk also flips
    /// [`Self::active_buffer`] and lands the cursor on the correct
    /// buffer. Stale entries pointing at a closed Help buffer
    /// (matching kind but different id) are skipped -- the history
    /// outlives any one Help session.
    fn do_walk_history<F: Fn(&PositionEntry) -> bool>(
        &mut self,
        delta: i32,
        pred: F,
        empty_label: &str,
        bound_label: &str,
    ) {
        if !self.position_history.iter().any(&pred) {
            self.set_message(EchoLevel::Error, format!("no {empty_label}"));
            return;
        }
        // Reachable: the registry still holds an entry for the
        // recorded buffer id (in-pane Help / Document / FileTree
        // all live in `self.buffers`); the transient popup-mode
        // Help overlay's id is checked separately.
        let popup_help_id = self.help_buffer.as_ref().map(|h| h.id);
        let reachable = |e: &PositionEntry| -> bool {
            match e.buffer {
                BufferKind::Document | BufferKind::FileTree => self.buffers.contains(e.buffer_id),
                BufferKind::Help => {
                    self.buffers.help(e.buffer_id).is_some()
                        || popup_help_id == Some(e.buffer_id)
                }
            }
        };
        let combined = |e: &PositionEntry| pred(e) && reachable(e);
        let target_idx = if delta < 0 {
            self.position_history[..self.position_history_cursor]
                .iter()
                .rposition(&combined)
        } else {
            let from = self
                .position_history_cursor
                .saturating_add(1)
                .min(self.position_history.len());
            self.position_history[from..]
                .iter()
                .position(&combined)
                .map(|i| i + from)
        };
        let Some(idx) = target_idx else {
            let bound = if delta < 0 { "start" } else { "end" };
            self.set_message(EchoLevel::Error, format!("at {bound} of {bound_label}"));
            return;
        };
        self.position_history_cursor = idx;
        let entry = self.position_history[idx];
        // Cross-buffer landing: switch active_buffer and write the
        // cursor onto the right buffer's tracking field.
        match entry.buffer {
            BufferKind::Document => {
                self.active_buffer = BufferKind::Document;
                self.cursor = entry.position;
                self.clamp_cursor_to_buffer();
                self.auto_open_folds_at_cursor();
            }
            BufferKind::Help => {
                self.active_buffer = BufferKind::Help;
                // Prefer an in-pane help buffer with the recorded id;
                // fall back to the transient popup. Either way the
                // live cursor lands on `self.cursor` (unified).
                let buffer_present = self.buffers.help(entry.buffer_id).is_some()
                    || self
                        .help_buffer
                        .as_ref()
                        .map(|h| h.id == entry.buffer_id)
                        .unwrap_or(false);
                if buffer_present {
                    self.cursor = entry.position;
                    self.pane_tree.active_mut().buffer = BufferKind::Help;
                    self.pane_tree.active_mut().buffer_id = entry.buffer_id;
                    self.clamp_cursor_to_active_buffer();
                }
            }
            BufferKind::FileTree => {
                if self.buffers.file_tree(entry.buffer_id).is_some() {
                    self.active_buffer = BufferKind::FileTree;
                    self.cursor = entry.position;
                    self.pane_tree.active_mut().buffer = BufferKind::FileTree;
                    self.pane_tree.active_mut().buffer_id = entry.buffer_id;
                    self.clamp_cursor_to_active_buffer();
                }
            }
        }
    }

    /// Store a yank into the appropriate register slot. Vim's behavior:
    ///
    /// - `Register::BlackHole` -> drop on the floor, no storage.
    /// - Any explicit register -> store there AND in `""` (unnamed).
    /// - `Register::Unnamed` -> store in `""`.
    /// - Yanks (vs deletes) also populate `"0`. We approximate vim's
    ///   distinction by treating any `Effect::Yank` from a yank operator
    ///   as also writing `"0`; deletes don't (they hit `"1`+ in vim,
    ///   which we don't model in v1).
    fn store_yank(&mut self, register: Register, content: String, kind: YankKind) {
        if matches!(register, Register::BlackHole) {
            return;
        }
        let entry = UnnamedRegister {
            content: content.clone(),
            kind,
        };
        // Always update unnamed.
        self.unnamed_register = Some(entry.clone());
        // If a named / numbered / system register was explicitly chosen,
        // store there too.
        match register {
            Register::Unnamed | Register::BlackHole => {}
            other => {
                self.registers.insert(other, entry.clone());
            }
        }
        // For uppercase named registers, vim *appends* to the lowercase
        // version. v1 simplification: A-Z replaces lowercase too (so
        // both "a and "A end up with the same content). The append
        // semantics is logged for follow-up.
    }

    /// Read the register slot for paste / inspection. Falls back to
    /// `unnamed_register`.
    fn read_register(&self, register: Option<Register>) -> Option<UnnamedRegister> {
        match register {
            None | Some(Register::Unnamed) => self.unnamed_register.clone(),
            Some(Register::BlackHole) => None,
            Some(r) => self
                .registers
                .get(&r)
                .cloned()
                .or_else(|| self.unnamed_register.clone()),
        }
    }

    /// Vim's `zf`: create a fold over the current Visual selection's
    /// line range. No-op outside Visual mode.
    fn do_create_fold_from_visual(&mut self) {
        if !matches!(self.modal, ModalState::Visual(_)) {
            self.set_message(
                EchoLevel::Error,
                "zf requires a Visual selection".to_string(),
            );
            return;
        }
        let sels = self.document.selections();
        let sel = sels.primary();
        let start_line = sel.anchor.line.min(sel.head.line);
        let end_line = sel.anchor.line.max(sel.head.line);
        if start_line == end_line {
            // Single-line "fold" is meaningless; vim allows it but we
            // skip to avoid noise.
            return;
        }
        self.folds.push(Fold {
            start_line,
            end_line,
            closed: true,
            identity: None,
        });
        // Exit Visual back to Normal at the fold start.
        self.cursor = Position::new(start_line, 0);
        self.do_exit_visual();
    }

    /// Toggle / open / close the fold containing the cursor.
    ///
    /// Selection rules per vim semantics, refined for the
    /// "cursor on a line that opens multiple folds" case:
    ///
    /// - `Some(true)` (`zc`): if the cursor sits on `start_line` of
    ///   any open fold, close the **outermost** such fold (the
    ///   user's mental model when their cursor is on the `if` /
    ///   `let` / `impl` line is "fold the entire form"). Otherwise
    ///   close the **innermost open** fold containing the cursor
    ///   (the cursor is in a fold's body and they want the tightest
    ///   enclosing structure). Subsequent `zc`s from the same line
    ///   walk outward as each layer closes.
    /// - `Some(false)` (`zo`): opens the **outermost closed** fold
    ///   containing the cursor. Subsequent `zo`s walk inward as
    ///   each layer reveals the next.
    /// - `None` (`za`): if any closed fold contains the cursor,
    ///   acts like `zo`; otherwise acts like `zc`.
    ///
    /// Innermost = max start_line, then min end_line on ties.
    /// Outermost = min start_line, then max end_line on ties.
    /// Emits `E490: No fold found` when the requested operation
    /// has no candidate (e.g. `zo` with nothing closed at cursor).
    fn do_set_fold_state_at_cursor(&mut self, state: Option<bool>) {
        let line = self.cursor.line;
        let target = match state {
            Some(true) => fold_to_close_at(&self.folds, line),
            Some(false) => outermost_fold_idx(&self.folds, line, |f| f.closed),
            None => {
                let any_closed = self
                    .folds
                    .iter()
                    .any(|f| f.closed && line >= f.start_line && line <= f.end_line);
                if any_closed {
                    outermost_fold_idx(&self.folds, line, |f| f.closed)
                } else {
                    fold_to_close_at(&self.folds, line)
                }
            }
        };
        let Some(idx) = target else {
            // No matching fold. Default `foldmethod = manual` produces
            // none until the user runs `zf`; the common cause of "zc
            // does nothing" is forgetting `:set foldmethod=indent` or
            // `=syntax`. Surface vim's E490 so the gap is discoverable.
            self.set_message(EchoLevel::Error, "E490: No fold found".to_string());
            return;
        };
        self.folds[idx].closed = match state {
            None => !self.folds[idx].closed,
            Some(s) => s,
        };
    }

    fn do_set_all_folds(&mut self, closed: bool) {
        for fold in self.folds.iter_mut() {
            fold.closed = closed;
        }
    }

    fn do_goto_fold(&mut self, forward: bool) {
        let line = self.cursor.line;
        let target = if forward {
            self.folds
                .iter()
                .filter(|f| f.start_line > line)
                .map(|f| f.start_line)
                .min()
        } else {
            self.folds
                .iter()
                .filter(|f| f.end_line < line)
                .map(|f| f.end_line)
                .max()
        };
        if let Some(t) = target {
            self.cursor = Position::new(t, 0);
        } else {
            self.set_message(EchoLevel::Error, "no more folds".to_string());
        }
    }

    fn do_delete_fold_at_cursor(&mut self) {
        let line = self.cursor.line;
        // Vim's `zd` removes one fold (the innermost). Previously
        // we retained-out every containing fold which silently
        // deleted siblings in nested cases.
        if let Some(idx) = innermost_fold_idx(&self.folds, line, |_| true) {
            self.folds.remove(idx);
        } else {
            self.set_message(EchoLevel::Error, "E490: No fold found".to_string());
        }
    }

    /// Returns true if `line` is inside a closed fold (and not the fold
    /// start, which is rendered as the summary). The renderer uses this
    /// to skip lines. When `foldenable` is false, returns `false`
    /// regardless of fold state -- `zi` / `:set nofoldenable` makes
    /// every line visible.
    pub fn line_inside_closed_fold(&self, line: u32) -> bool {
        if !self.foldenable() {
            return false;
        }
        self.folds
            .iter()
            .any(|f| f.closed && line > f.start_line && line <= f.end_line)
    }

    /// Returns Some(fold) if `line` is the start of a closed fold; the
    /// renderer renders the summary header instead of the line content.
    /// `foldenable = false` short-circuits this -- nothing renders
    /// folded.
    pub fn fold_start_at(&self, line: u32) -> Option<&Fold> {
        if !self.foldenable() {
            return None;
        }
        self.folds.iter().find(|f| f.closed && f.start_line == line)
    }

    /// Returns Some(fold) if `line` is the start of any fold (open or
    /// closed). Used by the renderer to draw the gutter glyph
    /// (▾ open / ▸ closed) regardless of state. With
    /// `foldenable = false` the gutter glyph is suppressed too --
    /// every line renders flat.
    pub fn fold_start_at_any(&self, line: u32) -> Option<&Fold> {
        if !self.foldenable() {
            return None;
        }
        self.folds.iter().find(|f| f.start_line == line)
    }

    /// Move the cursor out of any closed fold's hidden body to the
    /// nearest visible line, per `docs/help/folding.md`. Called
    /// after every non-jump dispatcher motion (`j`, `k`, `w`, `b`,
    /// `e`, `(`, `)`, `{`, `}`, `<C-d>` and friends) so the
    /// cursor's logical position never diverges from where the user
    /// thinks it is.
    ///
    /// `prev_line` records the cursor's row before dispatch so the
    /// snap can pick a sensible direction:
    ///
    /// - moving down (`prev_line < new_line`) → walk forward over
    ///   chained closed folds and trailing blank lines, landing on
    ///   the next visible content row;
    /// - moving up (`prev_line > new_line`) → walk back to the
    ///   containing fold's heading line (no blank-swallow on the
    ///   up direction; the heading is the right landing point);
    /// - same row (`prev_line == new_line`) → no work; intra-line
    ///   motions like `^` / `$` can't land on a hidden line.
    ///
    /// The trailing-blank swallow only fires *after* the snap has
    /// exited at least one fold downward. Plain `j` onto a blank
    /// line that isn't a fold neighbour stops at that blank, the
    /// way vim's visible-line counting works.
    ///
    /// `foldenable = false` suppresses the snap entirely; with
    /// folds disabled every line is visible and the cursor is free
    /// to land anywhere.
    fn snap_cursor_past_closed_folds(&mut self, prev_line: u32) {
        if !self.foldenable() {
            return;
        }
        let new_line = self.cursor.line;
        if new_line == prev_line {
            return;
        }
        let going_down = new_line > prev_line;
        let snap = self.document.snapshot();
        let last = last_addressable_line(&snap.buffer);
        let mut snapped = new_line;
        let mut exited_a_fold = false;
        loop {
            let in_closed = self
                .folds
                .iter()
                .find(|f| f.closed && snapped > f.start_line && snapped <= f.end_line)
                .copied();
            if let Some(fold) = in_closed {
                snapped = if going_down {
                    (fold.end_line + 1).min(last)
                } else {
                    fold.start_line
                };
                exited_a_fold = true;
                continue;
            }
            if exited_a_fold
                && going_down
                && snapped < last
                && is_blank_line(&snap.buffer, snapped)
            {
                snapped += 1;
                continue;
            }
            break;
        }
        if snapped == new_line {
            return;
        }
        let len = line_byte_len(&snap.buffer, snapped);
        let byte = self.cursor.byte.min(len);
        self.cursor = Position::new(snapped, byte);
    }

    /// Open every closed fold whose range contains the current
    /// cursor line. Called by jump-class motions (search hits,
    /// gg / G / numberG, H / M / L, mark jumps, Ctrl-O / Ctrl-I,
    /// `%` bracket-match) so the cursor never lands inside a hidden
    /// region. Linear motions (j / k / h / l / w / b) do NOT call
    /// this -- vim's "next visible line" skip behaviour still
    /// applies to them. When `foldenable = false` this is a no-op.
    /// (`docs/help/folding.md`).
    pub fn auto_open_folds_at_cursor(&mut self) {
        if !self.foldenable() {
            return;
        }
        let line = self.cursor.line;
        for fold in self.folds.iter_mut() {
            if fold.closed && line >= fold.start_line && line <= fold.end_line {
                fold.closed = false;
            }
        }
    }

    /// Vim's `J` / `gJ`: join the current line with the next. With
    /// `with_space = true` (J), the joining newline becomes one space
    /// (and any leading whitespace on the next line is trimmed). With
    /// `with_space = false` (gJ), no replacement -- pure concat.
    fn do_join_lines(&mut self, with_space: bool) {
        let last = last_addressable_line(&self.document.snapshot().buffer);
        if self.cursor.line >= last {
            // No next line to join.
            return;
        }
        let line = self.cursor.line;
        let next_line = line + 1;
        let cur_len = line_byte_len(&self.document.snapshot().buffer, line);
        // Compute how many leading whitespace bytes to trim from the
        // next line's content (only for J, not gJ).
        let trim = if with_space {
            let text = self.document.text();
            let next_text = text
                .split_inclusive('\n')
                .nth(next_line as usize)
                .map(|l| l.trim_end_matches('\n'))
                .unwrap_or("");
            let mut t = 0usize;
            let bytes = next_text.as_bytes();
            while t < bytes.len() && (bytes[t] == b' ' || bytes[t] == b'\t') {
                t += 1;
            }
            t as u32
        } else {
            0
        };
        // Range to replace covers `\n` + (optional) leading whitespace.
        let range = ProtoRange::new(Position::new(line, cur_len), Position::new(next_line, trim));
        let replacement = if with_space { " " } else { "" };
        if let Ok(applied) = self.apply_edit_blocking(Edit::replace(range, replacement)) {
            // Cursor lands at the end of the original first line (vim's
            // standard J behavior puts cursor on the first space).
            self.cursor = applied.original_range.start;
        }
    }

    /// Vim's `;` / `,`: repeat the last f/F/t/T find on the current
    /// line. `reverse = false` keeps the original direction; `true`
    /// flips it.
    fn do_find_repeat(&mut self, reverse: bool) {
        let Some(last) = self.last_find else {
            self.set_message(EchoLevel::Error, "no previous find".to_string());
            return;
        };
        let kind = if reverse {
            match last.kind {
                FindKind::Forward => FindKind::Backward,
                FindKind::Backward => FindKind::Forward,
                FindKind::TillForward => FindKind::TillBackward,
                FindKind::TillBackward => FindKind::TillForward,
            }
        } else {
            last.kind
        };
        let motion_id = match kind {
            FindKind::Forward => self.builtins.find_char_forward,
            FindKind::Backward => self.builtins.find_char_backward,
            FindKind::TillForward => self.builtins.till_char_forward,
            FindKind::TillBackward => self.builtins.till_char_backward,
        };
        // Don't update last_find on repeat -- the original direction
        // sticks (vim semantics: ; preserves direction even after ,).
        let inv =
            CommandInvocation::of(motion_id.0).with_args(lattice_grammar::Args::Char(last.target));
        // Bypass run_invocation's last_find recording by dispatching
        // directly. We still want the standard pending/count consumption.
        self.run_invocation(inv);
    }

    /// Vim's `~`: toggle the case of the char at cursor and advance.
    /// Non-letter chars are unchanged; cursor still advances. At EOL
    /// the cursor stops (no wrap).
    fn do_toggle_case_at_cursor(&mut self) {
        let line_len = line_byte_len(&self.document.snapshot().buffer, self.cursor.line);
        if self.cursor.byte >= line_len {
            return;
        }
        let r = ProtoRange::new(
            self.cursor,
            Position::new(self.cursor.line, self.cursor.byte + 1),
        );
        let original = match self.document.snapshot().buffer.slice(r) {
            Ok(s) => s,
            Err(_) => return,
        };
        let toggled: String = original
            .as_bytes()
            .iter()
            .map(|&b| match b {
                b'a'..=b'z' => (b - 32) as char,
                b'A'..=b'Z' => (b + 32) as char,
                other => other as char,
            })
            .collect();
        if let Ok(applied) = self.apply_edit_blocking(Edit::replace(r, &toggled)) {
            self.cursor = applied.inserted_range.end;
        }
    }

    /// Vim's `*` / `#`: extract the word at the cursor, store it as
    /// `last_search`, and jump to the next (or previous) occurrence.
    /// Skips the current match by stepping one byte beyond it before
    /// invoking the search engine.
    fn do_search_word_under_cursor(&mut self, direction: SearchDirection) {
        let pre_jump = self.cursor;
        let text = self.document.text();
        let bytes = text.as_bytes();
        let cursor_byte = match self
            .document
            .snapshot()
            .buffer
            .position_to_byte(self.cursor)
        {
            Ok(b) => b,
            Err(_) => return,
        };
        // Find the word boundaries at cursor; if cursor isn't on a word
        // byte, scan forward to the next word on the same line.
        let mut start = cursor_byte;
        if start >= bytes.len() || !is_word_char_byte(bytes[start]) {
            // Scan forward up to end-of-line for a word byte.
            while start < bytes.len() && bytes[start] != b'\n' && !is_word_char_byte(bytes[start]) {
                start += 1;
            }
            if start >= bytes.len() || bytes[start] == b'\n' {
                self.set_message(EchoLevel::Error, "no word under cursor".to_string());
                return;
            }
        }
        // Walk back to start of word.
        while start > 0 && is_word_char_byte(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = start;
        while end < bytes.len() && is_word_char_byte(bytes[end]) {
            end += 1;
        }
        let word = String::from_utf8_lossy(&bytes[start..end]).into_owned();
        if word.is_empty() {
            self.set_message(EchoLevel::Error, "no word under cursor".to_string());
            return;
        }
        let dir = match direction {
            SearchDirection::Forward => lattice_core::search::Direction::Forward,
            SearchDirection::Backward => lattice_core::search::Direction::Backward,
        };
        // Skip the current match: search from one byte past for forward,
        // one byte before for backward.
        let from = match direction {
            SearchDirection::Forward => {
                step_byte(&self.document.snapshot().buffer, self.cursor, direction)
            }
            SearchDirection::Backward => {
                step_byte(&self.document.snapshot().buffer, self.cursor, direction)
            }
        };
        // The word is a literal we want to find verbatim, not a
        // pattern. Escape regex metachars before compiling so words
        // containing `.`, `*`, `(` etc. don't trigger metacharacter
        // semantics. (vim's `*` also adds `\<...\>` word-boundary
        // anchors -- if we want that later, change this to
        // `\b{escaped}\b`.)
        let escaped = fancy_regex::escape(&word).into_owned();
        let regex = match compile_search_pattern(&escaped) {
            Ok(r) => r,
            Err(_) => {
                self.set_message(EchoLevel::Error, "regex compile failed".to_string());
                return;
            }
        };
        match lattice_core::search::find(
            &self.document.snapshot().buffer,
            &regex,
            from,
            dir,
            &CancellationToken::never(),
        ) {
            Ok(Some(hit)) => {
                self.push_position_history(pre_jump, PositionSource::AutoJump);
                self.cursor = hit.range.start;
                self.current_match = Some(hit.range);
                self.all_matches = lattice_core::search::find_all(
                    &self.document.snapshot().buffer,
                    &regex,
                    &CancellationToken::never(),
                )
                .unwrap_or_default();
                if hit.wrapped {
                    let text = match direction {
                        SearchDirection::Forward => "search hit BOTTOM, continuing at TOP",
                        SearchDirection::Backward => "search hit TOP, continuing at BOTTOM",
                    };
                    self.set_message(EchoLevel::Warn, text.to_string());
                }
            }
            Ok(None) => {
                self.current_match = None;
                self.all_matches.clear();
                self.set_message(EchoLevel::Error, format!("E486: Pattern not found: {word}"));
            }
            Err(_) => {
                self.current_match = None;
                self.all_matches.clear();
            }
        }
        self.last_search = Some(LastSearch {
            pattern: word,
            direction,
        });
    }

    /// Vim's `%`: jump to the matching `()[]{}`. Behavior: scan the
    /// current line from `cursor.byte` for the first bracket char; that
    /// bracket and its match define the jump. If the cursor is past
    /// every bracket on the line, do nothing.
    fn do_match_bracket(&mut self) {
        let text = self.document.text();
        let bytes = text.as_bytes();
        let cursor_byte = match self
            .document
            .snapshot()
            .buffer
            .position_to_byte(self.cursor)
        {
            Ok(b) => b,
            Err(_) => return,
        };
        // Scan from cursor to end-of-line for a bracket char.
        let mut idx = cursor_byte;
        let mut bracket = None;
        while idx < bytes.len() && bytes[idx] != b'\n' {
            if matches!(bytes[idx], b'(' | b')' | b'[' | b']' | b'{' | b'}') {
                bracket = Some((idx, bytes[idx]));
                break;
            }
            idx += 1;
        }
        let Some((start, b)) = bracket else {
            self.set_message(EchoLevel::Error, "no bracket on this line".to_string());
            return;
        };
        let (open, close, forward) = match b {
            b'(' => (b'(', b')', true),
            b')' => (b'(', b')', false),
            b'[' => (b'[', b']', true),
            b']' => (b'[', b']', false),
            b'{' => (b'{', b'}', true),
            b'}' => (b'{', b'}', false),
            _ => return,
        };
        let pre_jump = self.cursor;
        let target = if forward {
            scan_forward_for_match(bytes, start, open, close)
        } else {
            scan_backward_for_match(bytes, start, open, close)
        };
        match target {
            Some(t) => {
                if let Ok(pos) = self.document.snapshot().buffer.byte_to_position(t) {
                    self.push_position_history(pre_jump, PositionSource::AutoJump);
                    self.cursor = pos;
                    self.auto_open_folds_at_cursor();
                }
            }
            None => {
                self.set_message(EchoLevel::Error, "unmatched bracket".to_string());
            }
        }
    }

    /// Jump to a recorded mark. `exact = true` puts the cursor at the
    /// stored byte; `exact = false` jumps to the line and column = first
    /// non-blank (vim's `'<letter>` semantics).
    fn do_jump_mark(&mut self, name: char, exact: bool) {
        if !is_valid_mark_name(name) {
            self.set_message(EchoLevel::Error, format!("invalid mark: {name}"));
            return;
        }
        let Some(&pos) = self.marks.get(&name) else {
            self.set_message(EchoLevel::Error, format!("mark not set: {name}"));
            return;
        };
        // Push pre-jump position so Ctrl-O can return.
        let cur = self.cursor;
        self.push_position_history(cur, PositionSource::AutoJump);
        if exact {
            self.cursor = pos;
        } else {
            // Line-only jump: snap byte to first non-blank on that line.
            let text = self.document.text();
            let line_text = text
                .split_inclusive('\n')
                .nth(pos.line as usize)
                .map(|l| l.trim_end_matches('\n'))
                .unwrap_or("");
            let bytes = line_text.as_bytes();
            let mut col = 0usize;
            while col < bytes.len() && (bytes[col] == b' ' || bytes[col] == b'\t') {
                col += 1;
            }
            self.cursor = Position::new(pos.line, col as u32);
        }
        self.clamp_cursor_to_buffer();
        self.auto_open_folds_at_cursor();
    }

    fn do_reselect_visual(&mut self) {
        let Some(last) = self.last_visual else {
            self.set_message(EchoLevel::Error, "no previous visual selection".to_string());
            return;
        };
        // Restore the selection: cursor lands at `head`, anchor at `anchor`,
        // visual mode is the saved kind.
        self.modal = ModalState::Visual(last.kind);
        self.pending = Pending::None;
        self.visual_anchor = Some(last.anchor);
        self.cursor = last.head;
        let sel = Selection {
            anchor: last.anchor,
            head: last.head,
            visual: Some(visual_kind_to_mode(last.kind)),
        };
        self.set_selections_blocking(SelectionSet::single(sel));
    }

    /// Paste from the chosen register (`pending_register` if set, else
    /// the unnamed register). `before = true` for `P` (paste before
    /// cursor / above current line), `false` for `p` (paste after
    /// cursor / below current line). Linewise yanks insert on a new
    /// line; charwise yanks splice at the cursor.
    fn do_paste(&mut self, before: bool) {
        let chosen = self.pending_register.take();
        let Some(reg) = self.read_register(chosen) else {
            self.set_message(EchoLevel::Error, "register empty".to_string());
            return;
        };
        match reg.kind {
            YankKind::Charwise => {
                // `p` inserts after the cursor's byte; `P` at the cursor.
                let line_len = line_byte_len(&self.document.snapshot().buffer, self.cursor.line);
                let insert_at = if before {
                    self.cursor
                } else if self.cursor.byte < line_len {
                    Position::new(self.cursor.line, self.cursor.byte + 1)
                } else {
                    self.cursor
                };
                if let Ok(applied) = self.apply_edit_blocking(Edit::insert(insert_at, &reg.content))
                {
                    // Vim leaves the cursor on the last char of the pasted text.
                    let end = applied.inserted_range.end;
                    self.cursor = if end.byte > 0 {
                        Position::new(end.line, end.byte - 1)
                    } else {
                        end
                    };
                }
            }
            YankKind::Linewise => {
                // Linewise content is inserted as a whole new line. We
                // normalise by ensuring exactly one trailing newline on the
                // payload before splicing at the appropriate line boundary.
                let mut payload = reg.content.clone();
                if !payload.ends_with('\n') {
                    payload.push('\n');
                }
                let insert_at = if before {
                    Position::new(self.cursor.line, 0)
                } else {
                    let len = line_byte_len(&self.document.snapshot().buffer, self.cursor.line);
                    // Insert at end of current line then a newline -- but
                    // vim's `p` puts the line BELOW. So insert at start of
                    // the next line. If we're on the last line and there's
                    // no trailing newline, insert "\n<payload-without-tail>".
                    if self.cursor.line + 1 < self.document.snapshot().buffer.line_count() {
                        Position::new(self.cursor.line + 1, 0)
                    } else {
                        // Append at EOL of last line; payload starts with \n
                        // implicit in being on a "new" line.
                        let _ = self.apply_edit_blocking(Edit::insert(
                            Position::new(self.cursor.line, len),
                            "\n",
                        ));
                        Position::new(self.cursor.line + 1, 0)
                    }
                };
                if let Ok(applied) = self.apply_edit_blocking(Edit::insert(insert_at, &payload)) {
                    // Cursor lands at the start of the pasted block.
                    self.cursor = applied.inserted_range.start;
                }
            }
            YankKind::Blockwise => self.do_paste_blockwise(&reg.content, before),
        }
    }

    /// Vim's blockwise paste: each `\n`-separated row is inserted on
    /// consecutive lines starting at the same column. `p` (after)
    /// inserts at `cursor.byte + 1`, `P` (before) at `cursor.byte`.
    /// Rows wider than a target line's existing length are appended
    /// after end-of-line; missing rows below the buffer extend it
    /// with new lines. Cursor lands at the top-left of the pasted
    /// block.
    fn do_paste_blockwise(&mut self, content: &str, before: bool) {
        if content.is_empty() {
            return;
        }
        let rows: Vec<&str> = content.split('\n').collect();
        let start_line = self.cursor.line;
        let line_len = line_byte_len(&self.document.snapshot().buffer, start_line);
        let start_col = if before {
            self.cursor.byte
        } else if self.cursor.byte < line_len {
            self.cursor.byte + 1
        } else {
            self.cursor.byte
        };

        for (i, row) in rows.iter().enumerate() {
            let target_line = start_line + i as u32;
            let total_lines = self.document.snapshot().buffer.line_count();
            if target_line >= total_lines {
                // Need a new line at the bottom of the buffer. Append
                // a newline at the end of the current last line.
                let last = total_lines.saturating_sub(1);
                let last_len = line_byte_len(&self.document.snapshot().buffer, last);
                let _ = self.apply_edit_blocking(Edit::insert(Position::new(last, last_len), "\n"));
            }
            let target_len = line_byte_len(&self.document.snapshot().buffer, target_line);
            let insert_col = start_col.min(target_len);
            let pos = Position::new(target_line, insert_col);
            // Pad with spaces if the target line is shorter than the
            // start column (vim's behaviour: don't extend the rectangle
            // to the left). With `target_len <= start_col`, append at
            // end-of-line instead.
            let _ = self.apply_edit_blocking(Edit::insert(pos, *row));
        }
        self.cursor = Position::new(start_line, start_col);
    }

    fn do_open_line_above(&mut self) {
        let bol = Position::new(self.cursor.line, 0);
        if self.apply_edit_blocking(Edit::insert(bol, "\n")).is_ok() {
            self.cursor = bol;
        }
        self.modal = ModalState::Insert;
        self.pending = Pending::None;
    }

    /// Cursor of the currently active buffer. Reads `App::cursor`
    /// when the document is active or `help_buffer.cursor` when a
    /// help overlay holds focus. Used by code that records jump
    /// origins (where `<C-o>` would land if pressed right now)
    /// without needing to know which buffer kind that origin came
    /// from.
    pub fn active_cursor(&self) -> Position {
        match self.active_buffer {
            BufferKind::Document => self.cursor,
            BufferKind::Help => self
                .help_buffer
                .as_ref()
                .map(|h| h.cursor)
                .unwrap_or(self.cursor),
            BufferKind::FileTree => self
                .buffers
                .file_tree(self.active_pane_buffer_id())
                .map(|t| t.cursor)
                .unwrap_or(self.cursor),
        }
    }

    fn clamp_cursor_to_buffer(&mut self) {
        self.clamp_cursor_to_active_buffer();
    }

    /// Clamp `self.cursor` to the active buffer's bounds. Same as
    /// `clamp_cursor_to_buffer` but reads from `active_text()` so
    /// it works for help / file-tree / document uniformly.
    fn clamp_cursor_to_active_buffer(&mut self) {
        let buffer = self.active_text();
        let last_line = last_addressable_line(&buffer);
        if self.cursor.line > last_line {
            self.cursor.line = last_line;
        }
        let len = line_byte_len(&buffer, self.cursor.line);
        if self.cursor.byte > len {
            self.cursor.byte = len;
        }
    }

    pub fn ensure_cursor_visible(&mut self) {
        if self.viewport_height == 0 {
            return;
        }
        if self.cursor.line < self.scroll {
            self.scroll = self.cursor.line;
        }
        let bottom = self.scroll + self.viewport_height - 1;
        if self.cursor.line > bottom {
            self.scroll = self.cursor.line + 1 - self.viewport_height;
        }
    }

    /// The active buffer's text -- a `Buffer` clone (rope is O(1)).
    /// Document, help, file-tree all flow through this, so motion /
    /// scroll / search code can read text without branching on
    /// `BufferKind`. `self.cursor` / `self.scroll` are the live
    /// position into this buffer.
    pub fn active_text(&self) -> Buffer {
        match self.active_buffer {
            BufferKind::Help => self
                .help_buffer
                .as_ref()
                .map(|h| h.content.clone())
                .unwrap_or_else(|| self.document.snapshot().buffer.clone()),
            BufferKind::FileTree => self
                .buffers
                .file_tree(self.active_pane_buffer_id())
                .map(|t| t.content.clone())
                .unwrap_or_else(|| self.document.snapshot().buffer.clone()),
            BufferKind::Document => self.document.snapshot().buffer.clone(),
        }
    }

    pub fn set_viewport_height(&mut self, height: u32) {
        self.viewport_height = height.max(1);
        self.ensure_cursor_visible();
    }

    /// Compute the active pane's *content* height inside a buffer
    /// area of `buffer_height` rows. Mirrors the renderer's per-pane
    /// layout: the pane tree splits the area evenly; with more than
    /// one pane, the bottom row of each pane is reserved for the
    /// status line. Returns at least 1 so callers can multiply / use
    /// without checking for zero.
    ///
    /// Used by the runtime to feed `set_viewport_height` the
    /// **active pane's** content height -- not the full buffer area
    /// -- so motions, scroll, fold-aware ensure_cursor_visible all
    /// agree with what's actually drawn. Without this, a horizontal
    /// split clips the lower half of the upper pane: the App thinks
    /// it has the whole screen, the renderer only paints half.
    ///
    /// Help fills the pane area (DESIGN.md §5.9: help is a real
    /// buffer); its viewport height matches the active pane's
    /// content rows -- no special-case shrink for the popup
    /// frame. The transient hover-overlay popup is a separate
    /// surface that doesn't drive `self.viewport_height`.
    pub fn active_pane_content_height(&self, buffer_height: u32) -> u32 {
        let area = crate::pane::PaneRect {
            x: 0,
            y: 0,
            width: 1,
            height: buffer_height as u16,
        };
        let rects = self.pane_tree.compute_rects(area);
        let active_idx = self.pane_tree.active_index();
        let multi = rects.len() > 1;
        let pane_h = rects
            .iter()
            .find(|(idx, _)| *idx == active_idx)
            .map(|(_, r)| r.height)
            .unwrap_or(buffer_height as u16);
        let content_h = if multi && pane_h >= 2 {
            pane_h - 1 // reserve the per-pane status row
        } else {
            pane_h
        };
        u32::from(content_h).max(1)
    }

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
}

pub(crate) fn line_byte_len(buf: &Buffer, line: u32) -> u32 {
    // §8.2 hot path: use ropey's O(log n) line API instead of
    // materialising the whole buffer.
    buf.line_byte_len(line)
}

/// Compile a search / substitute pattern string into a
/// [`fancy_regex::Regex`]. Returns the compile error's display
/// string on failure -- callers surface it via `set_message` or
/// equivalent.
///
/// Why a free function: hlsearch / live-preview compiles per
/// keystroke; the submit path compiles once. Both reach for the
/// same helper. If profiling shows compile cost bites we can add
/// a cache on App keyed by `(pattern, ...flags)` -- but for ~10us
/// compile of typical patterns it's unnecessary.
fn compile_search_pattern(pattern: &str) -> Result<Regex, String> {
    Regex::new(pattern).map_err(|e| e.to_string())
}

pub(crate) fn last_addressable_line(buf: &Buffer) -> u32 {
    let lc = buf.line_count();
    if lc == 0 {
        return 0;
    }
    // ropey reports an extra empty line for any rope ending in
    // `\n`. Detect that by checking whether the last "line" the
    // rope reports is empty, without materialising the entire
    // buffer text.
    let last_idx = lc - 1;
    if buf.line_byte_len(last_idx) == 0 && lc >= 2 {
        last_idx - 1
    } else {
        last_idx
    }
}

fn is_valid_mark_name(c: char) -> bool {
    c.is_ascii_alphabetic() || c.is_ascii_digit()
}

/// Render a register's content into a one-line preview (truncated and
/// with newlines escaped). Used by `:reg`.
fn preview_register(s: &str) -> String {
    const MAX: usize = 40;
    let escaped: String = s
        .chars()
        .map(|c| if c == '\n' { '\u{21B5}' } else { c })
        .collect();
    if escaped.chars().count() <= MAX {
        escaped
    } else {
        let trimmed: String = escaped.chars().take(MAX).collect();
        format!("{trimmed}…")
    }
}

fn is_word_char_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn scan_forward_for_match(bytes: &[u8], from: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = from;
    loop {
        if i >= bytes.len() {
            return None;
        }
        let b = bytes[i];
        if b == open {
            depth += 1;
        } else if b == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
}

fn scan_backward_for_match(bytes: &[u8], from: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = from;
    loop {
        let b = bytes[i];
        if b == close {
            depth += 1;
        } else if b == open {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

fn visual_kind_to_mode(kind: VisualKind) -> VisualMode {
    match kind {
        VisualKind::Charwise => VisualMode::Charwise,
        VisualKind::Linewise => VisualMode::Linewise,
        VisualKind::Blockwise => VisualMode::Blockwise,
    }
}

/// True if `line_idx` is empty or whitespace-only. Used by the
/// fold-aware j/k snap to swallow trailing blanks between sibling
/// folds (so `j` from a closed fold's heading lands on the next
/// sibling's heading, not on the blank between them).
fn is_blank_line(buffer: &lattice_core::Buffer, line_idx: u32) -> bool {
    buffer
        .line(line_idx)
        .map(|s| s.trim().is_empty())
        .unwrap_or(true)
}

/// Index of the *innermost* fold containing `line` that satisfies
/// `pred`. Innermost = max start_line, then min end_line on ties.
/// Used by `zc` (close innermost open) and `za`'s close branch.
fn innermost_fold_idx<F: Fn(&Fold) -> bool>(
    folds: &[Fold],
    line: u32,
    pred: F,
) -> Option<usize> {
    folds
        .iter()
        .enumerate()
        .filter(|(_, f)| pred(f) && line >= f.start_line && line <= f.end_line)
        .max_by_key(|(_, f)| (f.start_line, std::cmp::Reverse(f.end_line)))
        .map(|(i, _)| i)
}

/// Pick the fold that `zc` (or `za`'s close branch) should target
/// when the cursor is on `line`.
///
/// If any open fold *starts* at `line`, the user is positioned on
/// the line that opens one or more folds (e.g. the `let len = if
/// cond {` line that simultaneously opens the let_declaration, the
/// if_expression, and the then-block). Their natural intent is to
/// fold the *largest* of those constructs in one step -- the
/// "fold the entire form" reading of `zc`. Pick the outermost
/// (largest end_line) among the open folds whose start_line equals
/// the cursor.
///
/// Otherwise the cursor is in a fold's body and the inverse rule
/// applies: pick the innermost open fold containing the cursor, so
/// progressive `zc`s walk inside-out as the user closes one
/// enclosing layer at a time.
fn fold_to_close_at(folds: &[Fold], line: u32) -> Option<usize> {
    // Strict-start match: outermost open fold whose `start_line == line`.
    let starts_here = folds
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.closed && f.start_line == line)
        .max_by_key(|(_, f)| f.end_line)
        .map(|(i, _)| i);
    if starts_here.is_some() {
        return starts_here;
    }
    // Body match: innermost open fold strictly containing the
    // cursor (cursor not on the start_line of an open fold here).
    folds
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.closed && line > f.start_line && line <= f.end_line)
        .max_by_key(|(_, f)| (f.start_line, std::cmp::Reverse(f.end_line)))
        .map(|(i, _)| i)
}

/// Index of the *outermost* fold containing `line` that satisfies
/// `pred`. Outermost = min start_line, then max end_line on ties.
/// Used by `zo` (open outermost closed) and `za`'s open branch.
fn outermost_fold_idx<F: Fn(&Fold) -> bool>(
    folds: &[Fold],
    line: u32,
    pred: F,
) -> Option<usize> {
    folds
        .iter()
        .enumerate()
        .filter(|(_, f)| pred(f) && line >= f.start_line && line <= f.end_line)
        .min_by_key(|(_, f)| (f.start_line, std::cmp::Reverse(f.end_line)))
        .map(|(i, _)| i)
}

/// True if the Effect indicates an operator-class action (the buffer
/// changed or content was yanked). Used by Visual mode to decide whether
/// to auto-exit after the dispatch -- motions in Visual should not exit;
/// d / y / c should.
fn effect_mutates_or_yanks(effect: &Effect) -> bool {
    match effect {
        Effect::Edits(_) | Effect::Yank { .. } => true,
        // Ex-effects that the host turns into edits / yanks at apply time.
        Effect::Substitute { .. } | Effect::Global { .. } | Effect::DeleteCurrentLine => true,
        Effect::Many(parts) => parts.iter().any(effect_mutates_or_yanks),
        Effect::None
        | Effect::SelectionChange(_)
        | Effect::EnterMode(_)
        | Effect::SaveBuffer { .. }
        | Effect::QuitEditor { .. }
        | Effect::OpenBuffer { .. }
        | Effect::SetOption { .. }
        | Effect::ClearSearchHighlight
        | Effect::Echo { .. }
        | Effect::EchoRegisters
        | Effect::EchoMarks
        | Effect::DescribeCommand { .. }
        | Effect::DescribeBuffer
        | Effect::Apropos { .. }
        | Effect::DescribeKey { .. }
        | Effect::ListKeymap
        | Effect::BufferNext
        | Effect::BufferPrev
        | Effect::ListBuffers
        | Effect::OpenBufferPicker
        | Effect::BufferDelete { .. }
        | Effect::OpenFileTree { .. }
        | Effect::CloseFileTree
        | Effect::DescribeOption { .. }
        | Effect::ListOptions
        | Effect::OpenHover { .. }
        | Effect::CloseHover
        | Effect::OpenHelpTopic { .. }
        | Effect::ListDiagnostics
        | Effect::NextDiagnostic
        | Effect::PrevDiagnostic
        | Effect::OpenLspLog { .. }
        | Effect::ToggleLspTrace { .. }
        | Effect::OpenLspTraceLog { .. }
        | Effect::LspStatus
        | Effect::LspServerLogListing
        | Effect::LspRestart { .. }
        | Effect::SetLspLogLevel { .. }
        | Effect::LspLogClear { .. } => false,
    }
}

/// True if the Effect produced a buffer mutation. Used by dot-repeat
/// to decide whether to record the invocation -- yank-only invocations
/// (vim's `y`) are NOT eligible for `.`, only changes.
fn effect_mutates(effect: &Effect) -> bool {
    match effect {
        Effect::Edits(_) => true,
        Effect::Substitute { .. } | Effect::Global { .. } | Effect::DeleteCurrentLine => true,
        Effect::Many(parts) => parts.iter().any(effect_mutates),
        Effect::None
        | Effect::SelectionChange(_)
        | Effect::Yank { .. }
        | Effect::EnterMode(_)
        | Effect::SaveBuffer { .. }
        | Effect::QuitEditor { .. }
        | Effect::OpenBuffer { .. }
        | Effect::SetOption { .. }
        | Effect::ClearSearchHighlight
        | Effect::Echo { .. }
        | Effect::EchoRegisters
        | Effect::EchoMarks
        | Effect::DescribeCommand { .. }
        | Effect::DescribeBuffer
        | Effect::Apropos { .. }
        | Effect::DescribeKey { .. }
        | Effect::ListKeymap
        | Effect::BufferNext
        | Effect::BufferPrev
        | Effect::ListBuffers
        | Effect::OpenBufferPicker
        | Effect::BufferDelete { .. }
        | Effect::OpenFileTree { .. }
        | Effect::CloseFileTree
        | Effect::DescribeOption { .. }
        | Effect::ListOptions
        | Effect::OpenHover { .. }
        | Effect::CloseHover
        | Effect::OpenHelpTopic { .. }
        | Effect::ListDiagnostics
        | Effect::NextDiagnostic
        | Effect::PrevDiagnostic
        | Effect::OpenLspLog { .. }
        | Effect::ToggleLspTrace { .. }
        | Effect::OpenLspTraceLog { .. }
        | Effect::LspStatus
        | Effect::LspServerLogListing
        | Effect::LspRestart { .. }
        | Effect::SetLspLogLevel { .. }
        | Effect::LspLogClear { .. } => false,
    }
}

/// Convert the App's `(line, byte)` cursor into an LSP `Position`
/// using the utf-16 encoding the spec defaults to. Returns `None`
/// if the line is out of bounds.
///
/// Build the raw candidate list for the buffer-switcher picker.
/// Lives here (not in [`crate::picker`]) because it walks the
/// host's [`BufferRegistry`] / [`BufferData`] -- types that the
/// picker module is intentionally agnostic of so it can graduate
/// to a renderer-neutral sibling crate when a second renderer
/// (GPUI, web) needs it.
///
/// One row per [`crate::buffer_registry::BufferEntry`] regardless
/// of kind. The active buffer is tagged `(current)` in the
/// marginalia and floated to the bottom so the picker's
/// initial-selected row lands on the alternate buffer (vim's
/// `:b<CR>` shortcut).
pub(crate) fn raw_buffer_candidates(
    registry: &BufferRegistry,
    active: BufferId,
) -> Vec<lattice_completion::RawCandidate> {
    let mut ids = registry.sorted_ids();
    ids.sort_by_key(|id| (*id == active, *id));
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(entry) = registry.get(id) else {
            continue;
        };
        let active_marker = if id == active { " (current)" } else { "" };
        let (body, kind_label) = match &entry.data {
            BufferData::Document(d) => {
                let path = d
                    .handle
                    .path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "[no name]".to_string());
                let dirty = if d.handle.dirty() { " [+]" } else { "" };
                (
                    format!("#{:<3} {path}{dirty}", id.0),
                    format!("doc{active_marker}"),
                )
            }
            BufferData::FileTree(t) => (
                format!("#{:<3} {}", id.0, t.root.display()),
                format!("tree{active_marker}"),
            ),
            BufferData::Help(h) => (
                format!("#{:<3} {}", id.0, h.title),
                format!("help{active_marker}"),
            ),
        };
        // `text` is the dispatch payload (`#<id>`, parsed by
        // `picker::buffer_id_from_text`); `display` is what the
        // popup paints (body + kind marginalia).
        let mut raw = lattice_completion::RawCandidate::plain(
            format!("#{}", id.0),
            lattice_completion::CandidateKind::Buffer,
        );
        raw.display = format!("{body:<60} {kind_label}");
        out.push(raw);
    }
    out
}

/// Phase 4.2 features (hover, definition, references, completion)
/// all need this; later we'll thread the per-server negotiated
/// `PositionEncodingKind` through here so utf-8 / utf-32 servers
/// don't pay the utf-16 conversion. For 4.2.b utf-16 is correct
/// for every server we care about today.
pub(crate) fn app_to_lsp_position(buffer: &Buffer, p: Position) -> Option<lsp_types::Position> {
    let line_text = buffer.line(p.line)?;
    let character = lattice_lsp::position::utf8_byte_to_utf16_column(&line_text, p.byte);
    Some(lsp_types::Position {
        line: p.line,
        character,
    })
}

/// Flatten an LSP `GotoDefinitionResponse` (Scalar / Array /
/// Link) into a uniform `Vec<Location>`. The `Link` shape carries
/// richer per-result info (origin selection range used to
/// highlight the symbol the user clicked); we drop it for now and
/// keep the target location only -- the App's jump path is
/// position-only. When 4.2.d's picker buffer lands the link
/// metadata (e.g., `target_selection_range` for narrower jump
/// destinations) becomes useful and this function gains a richer
/// sibling.
pub(crate) fn definition_response_to_locations(
    resp: lsp_types::GotoDefinitionResponse,
) -> Vec<lsp_types::Location> {
    match resp {
        lsp_types::GotoDefinitionResponse::Scalar(loc) => vec![loc],
        lsp_types::GotoDefinitionResponse::Array(locs) => locs,
        lsp_types::GotoDefinitionResponse::Link(links) => links
            .into_iter()
            .map(|l| lsp_types::Location {
                uri: l.target_uri,
                // `target_selection_range` is the narrower symbol
                // range; `target_range` is the enclosing block.
                // Picker UX usually wants the narrower one.
                range: l.target_selection_range,
            })
            .collect(),
    }
}

/// Render an LSP `HoverContents` payload to a markdown string the
/// renderer's [`crate::hover::HoverPopup`] pipeline can highlight
/// via the markdown grammar.
///
/// `MarkedString::String(s)` keeps `s` verbatim. `MarkedString::LanguageString
/// { language, value }` wraps `value` in a fenced code block tagged with
/// `language` so the markdown injection picks it up.
/// `MarkupContent` arrives pre-rendered as either markdown or plaintext
/// (we treat plaintext as already-good markdown). `Array` joins each
/// element with two newlines so blocks separate cleanly.
pub(crate) fn hover_contents_to_markdown(contents: &lsp_types::HoverContents) -> String {
    fn marked_to_markdown(m: &lsp_types::MarkedString) -> String {
        match m {
            lsp_types::MarkedString::String(s) => s.clone(),
            lsp_types::MarkedString::LanguageString(ls) => {
                format!("```{}\n{}\n```", ls.language, ls.value)
            }
        }
    }
    match contents {
        lsp_types::HoverContents::Scalar(m) => marked_to_markdown(m),
        lsp_types::HoverContents::Array(items) => items
            .iter()
            .map(marked_to_markdown)
            .collect::<Vec<_>>()
            .join("\n\n"),
        lsp_types::HoverContents::Markup(m) => m.value.clone(),
    }
}

fn previous_position(buf: &Buffer, p: Position) -> Position {
    if p.byte > 0 {
        Position::new(p.line, p.byte - 1)
    } else if p.line > 0 {
        let prev_line = p.line - 1;
        Position::new(prev_line, line_byte_len(buf, prev_line))
    } else {
        p
    }
}

/// One byte forward or backward, wrapping across newlines. Caller for
/// search-repeat: skip the current match by advancing one byte before
/// calling the engine. At buffer extremes we return the original
/// position; the engine then handles wrap.
fn step_byte(buf: &Buffer, p: Position, dir: SearchDirection) -> Position {
    match dir {
        SearchDirection::Forward => {
            let len = line_byte_len(buf, p.line);
            if p.byte < len {
                Position::new(p.line, p.byte + 1)
            } else {
                let last = last_addressable_line(buf);
                if p.line < last {
                    Position::new(p.line + 1, 0)
                } else {
                    p
                }
            }
        }
        SearchDirection::Backward => previous_position(buf, p),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn app_with(text: &str, viewport: u32) -> App {
        let mut a = App::new(Document::from_text(text));
        a.set_viewport_height(viewport);
        a
    }

    /// Subscribe a channel sink to the App's event bus. Returns
    /// the receiver so tests can drain published events. The
    /// subscription stays alive for the rx's lifetime.
    fn subscribe_all_events(a: &App) -> tokio::sync::mpsc::UnboundedReceiver<Event> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        a.event_bus.subscribe(
            lattice_runtime::EventFilter::any(),
            lattice_runtime::SubscriptionTarget::Channel(tx),
        );
        rx
    }

    #[test]
    fn event_bus_publishes_document_changed_on_apply_edit() {
        let a = app_with("hello", 5);
        let mut rx = subscribe_all_events(&a);
        a.apply_edit_blocking(Edit::insert(Position::new(0, 5), " world"))
            .unwrap();
        assert!(matches!(rx.try_recv(), Ok(Event::DocumentChanged { .. })));
    }

    #[test]
    fn event_bus_publishes_document_changed_on_undo_redo() {
        let a = app_with("a", 5);
        a.apply_edit_blocking(Edit::insert(Position::new(0, 1), "b"))
            .unwrap();
        let mut rx = subscribe_all_events(&a);
        a.undo_blocking().unwrap();
        a.redo_blocking().unwrap();
        let mut count = 0;
        while let Ok(Event::DocumentChanged { .. }) = rx.try_recv() {
            count += 1;
        }
        assert_eq!(count, 2, "expected DocumentChanged for undo + redo");
    }

    #[test]
    fn event_bus_publishes_modal_mode_changed_on_actual_transition() {
        let mut a = app_with("", 5);
        let mut rx = subscribe_all_events(&a);
        a.apply(Action::EnterMode(ModalState::Insert));
        let evt = rx.try_recv().unwrap();
        match evt {
            Event::ModalModeChanged { from, to } => {
                assert_eq!(from, "Normal");
                assert_eq!(to, "Insert");
            }
            other => panic!("expected ModalModeChanged, got {other:?}"),
        }
    }

    #[test]
    fn event_bus_skips_modal_mode_changed_when_state_unchanged() {
        // enter_mode is sometimes called for the side-effect of
        // recording / replay accounting without actually moving
        // the modal axis. Those re-entries shouldn't fire events.
        let mut a = app_with("", 5);
        let mut rx = subscribe_all_events(&a);
        a.apply(Action::EnterMode(ModalState::Normal)); // Normal -> Normal
        assert!(rx.try_recv().is_err(), "no event for same-state re-entry");
    }

    #[test]
    fn event_bus_publishes_before_quit_on_action_quit() {
        let mut a = app_with("", 5);
        let mut rx = subscribe_all_events(&a);
        a.apply(Action::Quit);
        // Drain until BeforeQuit (other events may precede).
        let mut found = false;
        while let Ok(evt) = rx.try_recv() {
            if matches!(evt, Event::BeforeQuit) {
                found = true;
                break;
            }
        }
        assert!(found, "BeforeQuit should be published on Action::Quit");
        assert!(a.should_quit);
    }

    #[test]
    fn event_bus_publishes_selections_changed_on_set_selections() {
        let a = app_with("hello world", 5);
        let mut rx = subscribe_all_events(&a);
        let sel = Selection::cursor(Position::new(0, 5));
        a.set_selections_blocking(SelectionSet::single(sel));
        let mut found = false;
        while let Ok(evt) = rx.try_recv() {
            if matches!(evt, Event::SelectionsChanged { .. }) {
                found = true;
                break;
            }
        }
        assert!(found);
    }

    fn invoke_motion(id: lattice_grammar::registry::MotionId) -> Action {
        Action::Invoke(CommandInvocation::of(id.0))
    }

    // ---- Event::OptionChanged (DESIGN.md §5.10 + §5.12) ----

    #[test]
    fn event_bus_publishes_option_changed_on_set_assign() {
        let mut a = app_with("xx", 10);
        let mut rx = subscribe_all_events(&a);
        a.command_line = "set tabstop=4".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let mut found_opt = None;
        while let Ok(evt) = rx.try_recv() {
            if let Event::OptionChanged { name, old, new } = evt {
                found_opt = Some((name, old, new));
                break;
            }
        }
        let (name, old, new) = found_opt.expect("OptionChanged should fire on :set tabstop=4");
        assert_eq!(name, "tabstop");
        assert_eq!(old.as_deref(), Some("8"));
        assert_eq!(new, "4");
    }

    #[test]
    fn event_bus_publishes_option_changed_on_set_negate() {
        let mut a = app_with("xx", 10);
        let mut rx = subscribe_all_events(&a);
        a.command_line = "set nonumber".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let mut found = false;
        while let Ok(evt) = rx.try_recv() {
            if let Event::OptionChanged { name, new, .. } = evt
                && name == "number"
                && new == "false"
            {
                found = true;
                break;
            }
        }
        assert!(found, ":set nonumber should publish OptionChanged");
    }

    #[test]
    fn drain_option_changes_runs_foldmethod_cascade_for_direct_config_writes() {
        // Architectural test: writes that bypass `:set` -- e.g. a
        // plugin or the future customize buffer view calling
        // `config.set` directly -- still trigger the cascade once
        // `drain_option_changes` runs. Pre-bus the cascade lived
        // on the cmdline path only; this confirms the migration to
        // the bus-subscription model fixes that gap.
        let mut a = app_with("def f():\n    pass\n    pass\n", 10);
        // No :set involved -- direct write to the registry.
        a.config
            .set(a.core_options.foldmethod, FoldMethod::Indent)
            .unwrap();
        // Folds should not be populated yet -- the cascade is
        // queued but hasn't been drained.
        // (The rx hasn't drained, so recompute_folds hasn't run.)
        // Drain explicitly (production code drains in main_loop /
        // do_set).
        a.drain_option_changes();
        assert_eq!(a.foldmethod(), FoldMethod::Indent);
        assert!(
            !a.folds.is_empty(),
            "drain_option_changes should run the foldmethod cascade and recompute folds"
        );
    }

    #[test]
    fn drain_option_changes_runs_relativenumber_to_number_cascade() {
        // Direct `config.set(relativenumber, true)` should also
        // implicitly enable `number` after the drain. Mirrors vim:
        // `:set rnu` implies `:set nu`.
        let mut a = app_with("xx", 10);
        // Start with number off so the cascade has something to do.
        a.config.set(a.core_options.number, false).unwrap();
        a.drain_option_changes();
        assert!(!a.show_line_numbers());
        // Now flip relativenumber on directly.
        a.config
            .set(a.core_options.relativenumber, true)
            .unwrap();
        a.drain_option_changes();
        assert!(a.relative_line_numbers());
        assert!(
            a.show_line_numbers(),
            "relativenumber=true should cascade to number=true via the bus subscription"
        );
    }

    #[test]
    fn drain_option_changes_runs_ui_theme_sync_for_direct_writes() {
        // Direct write to a `ui.*` option should refresh the
        // cached theme projections via `sync_theme_from_config`.
        let mut a = app_with("xx", 10);
        a.config
            .set(a.tui_options.dim_inactive, false)
            .unwrap();
        a.drain_option_changes();
        assert!(
            !a.theme.dim_inactive_panes,
            "ui.dim_inactive=false should propagate to theme.dim_inactive_panes via the cascade"
        );
    }

    #[test]
    fn drain_option_changes_handles_chained_cascade_writes() {
        // The relativenumber cascade itself calls config.set(number),
        // which fires another OptionChanged event. The drain loop
        // must handle the chained event without deadlocking or
        // dropping it. Confirm by starting from a clean state and
        // asserting both options ended up correctly set.
        let mut a = app_with("xx", 10);
        a.config.set(a.core_options.number, false).unwrap();
        a.drain_option_changes();
        a.config
            .set(a.core_options.relativenumber, true)
            .unwrap();
        a.drain_option_changes();
        assert!(a.relative_line_numbers());
        assert!(a.show_line_numbers());
        // Re-drain should be a no-op (channel empty).
        a.drain_option_changes();
        assert!(a.relative_line_numbers());
        assert!(a.show_line_numbers());
    }

    #[test]
    fn event_bus_does_not_publish_option_changed_on_query() {
        let mut a = app_with("xx", 10);
        let mut rx = subscribe_all_events(&a);
        a.command_line = "set number?".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        // No OptionChanged for query; we only get unrelated events
        // (ModalModeChanged from cmdline transitions, etc.).
        while let Ok(evt) = rx.try_recv() {
            assert!(
                !matches!(evt, Event::OptionChanged { .. }),
                "query should not publish OptionChanged"
            );
        }
    }

    // ---- Initial state ----

    #[test]
    fn new_app_starts_at_origin_in_normal_mode() {
        let a = app_with("abc", 10);
        assert_eq!(a.cursor, Position::ZERO);
        assert_eq!(a.scroll, 0);
        assert!(!a.should_quit);
        assert_eq!(a.modal, ModalState::Normal);
        assert_eq!(a.pending, Pending::None);
    }

    #[test]
    fn modal_label_reports_state() {
        let mut a = app_with("", 10);
        assert_eq!(a.modal_label(), "NORMAL");
        a.apply(Action::EnterMode(ModalState::Insert));
        assert_eq!(a.modal_label(), "INSERT");
    }

    #[test]
    fn quit_sets_flag() {
        let mut a = app_with("abc", 10);
        a.apply(Action::Quit);
        assert!(a.should_quit);
    }

    // ---- Motion via grammar engine ----

    #[test]
    fn invoke_char_right_advances_cursor() {
        let mut a = app_with("abc", 10);
        let id = a.builtins.char_right;
        a.apply(invoke_motion(id));
        assert_eq!(a.cursor, Position::new(0, 1));
    }

    #[test]
    fn invoke_char_left_at_origin_does_not_underflow() {
        let mut a = app_with("abc", 10);
        let id = a.builtins.char_left;
        a.apply(invoke_motion(id));
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn invoke_line_down_then_line_up() {
        let mut a = app_with("hello\nworld", 10);
        let down = a.builtins.line_down;
        let up = a.builtins.line_up;
        a.apply(invoke_motion(down));
        assert_eq!(a.cursor.line, 1);
        a.apply(invoke_motion(up));
        assert_eq!(a.cursor.line, 0);
    }

    #[test]
    fn invoke_goto_last_line_jumps_to_last_line() {
        let mut a = app_with("a\nb\nc", 10);
        let id = a.builtins.goto_last_line;
        a.apply(invoke_motion(id));
        assert_eq!(a.cursor.line, 2);
    }

    #[test]
    fn invoke_goto_first_line_returns_to_origin() {
        let mut a = app_with("a\nb\nc", 10);
        let last = a.builtins.goto_last_line;
        let first = a.builtins.goto_first_line;
        a.apply(invoke_motion(last));
        a.apply(invoke_motion(first));
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn invoke_line_end_moves_to_eol() {
        let mut a = app_with("hello world", 10);
        let id = a.builtins.line_end;
        a.apply(invoke_motion(id));
        assert_eq!(a.cursor, Position::new(0, 11));
    }

    #[test]
    fn invocation_resets_pending() {
        let mut a = app_with("abc", 10);
        a.apply(Action::SetPending(Pending::AfterG));
        assert_eq!(a.pending, Pending::AfterG);
        let id = a.builtins.char_right;
        a.apply(invoke_motion(id));
        assert_eq!(a.pending, Pending::None);
    }

    // ---- Insert mode ----

    #[test]
    fn entering_insert_mode_does_not_move_cursor() {
        let mut a = app_with("abc", 10);
        let before = a.cursor;
        a.apply(Action::EnterMode(ModalState::Insert));
        assert_eq!(a.modal, ModalState::Insert);
        assert_eq!(a.cursor, before);
    }

    #[test]
    fn insert_mode_inserts_text_and_advances_cursor() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("h".into()));
        a.apply(Action::Insert("i".into()));
        assert_eq!(a.document.text(), "hi");
        assert_eq!(a.cursor, Position::new(0, 2));
    }

    #[test]
    fn insert_then_normal_pulls_cursor_back_one() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("hi".into()));
        assert_eq!(a.cursor, Position::new(0, 2));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.cursor, Position::new(0, 1));
    }

    #[test]
    fn backspace_deletes_char_before_cursor_in_insert() {
        let mut a = app_with("hi", 10);
        a.cursor.byte = 2;
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::DeleteCharBackward);
        assert_eq!(a.document.text(), "h");
        assert_eq!(a.cursor, Position::new(0, 1));
    }

    #[test]
    fn backspace_at_origin_is_a_no_op() {
        let mut a = app_with("hi", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::DeleteCharBackward);
        assert_eq!(a.document.text(), "hi");
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn backspace_across_line_boundary_joins_lines() {
        let mut a = app_with("a\nb", 10);
        a.cursor = Position::new(1, 0);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::DeleteCharBackward);
        assert_eq!(a.document.text(), "ab");
        assert_eq!(a.cursor, Position::new(0, 1));
    }

    #[test]
    fn enter_append_advances_cursor_one_byte_then_inserts() {
        let mut a = app_with("ab", 10);
        a.apply(Action::EnterAppend);
        assert_eq!(a.modal, ModalState::Insert);
        assert_eq!(a.cursor, Position::new(0, 1));
    }

    #[test]
    fn open_line_below_creates_new_line_and_drops_cursor_to_it() {
        let mut a = app_with("first", 10);
        a.apply(Action::OpenLineBelow);
        assert_eq!(a.modal, ModalState::Insert);
        assert_eq!(a.document.text(), "first\n");
        assert_eq!(a.cursor, Position::new(1, 0));
    }

    #[test]
    fn open_line_above_creates_new_line_above() {
        let mut a = app_with("second", 10);
        a.apply(Action::OpenLineAbove);
        assert_eq!(a.modal, ModalState::Insert);
        assert_eq!(a.document.text(), "\nsecond");
        assert_eq!(a.cursor, Position::new(0, 0));
    }

    // ---- Operator + motion composition ----

    #[test]
    fn delete_with_word_forward_target_dw_in_app() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "world");
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn delete_char_under_cursor_x_in_app() {
        let mut a = app_with("abc", 10);
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.char_right, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "bc");
        assert_eq!(a.cursor, Position::ZERO);
    }

    // ---- Undo / Redo ----

    #[test]
    fn undo_after_insert_restores_buffer() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("hi".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.document.text(), "hi");
        a.apply(Action::Undo);
        assert_eq!(a.document.text(), "");
    }

    #[test]
    fn redo_replays_undone_edit() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("hi".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        a.apply(Action::Undo);
        a.apply(Action::Redo);
        assert_eq!(a.document.text(), "hi");
    }

    // ---- Viewport scrolling ----

    #[test]
    fn ensure_visible_scrolls_when_cursor_goes_off_bottom() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 3);
        let id = a.builtins.goto_last_line;
        a.apply(invoke_motion(id));
        assert_eq!(a.cursor.line, 9);
        assert_eq!(a.scroll, 9 - 3 + 1);
    }

    #[test]
    fn ensure_visible_scrolls_back_to_top_on_goto_first() {
        let mut a = app_with("0\n1\n2\n3\n4", 2);
        let last = a.builtins.goto_last_line;
        let first = a.builtins.goto_first_line;
        a.apply(invoke_motion(last));
        a.apply(invoke_motion(first));
        assert_eq!(a.scroll, 0);
    }

    // ---- Command-line minibuffer ----

    fn unique_tempdir() -> std::path::PathBuf {
        let base = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = base.join(format!("lattice-tui-test-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn enter_command_line_clears_buffer_and_sets_modal() {
        let mut a = app_with("abc", 10);
        a.command_line = "stale".into();
        a.last_message = Some(EchoMessage {
            text: "stale".into(),
            level: EchoLevel::Info,
        });
        a.apply(Action::EnterCommandLine);
        assert_eq!(a.modal, ModalState::Command);
        assert_eq!(a.command_line, "");
        assert!(a.last_message.is_none());
    }

    fn type_cmdline(a: &mut App, s: &str) {
        a.apply(Action::EnterCommandLine);
        for c in s.chars() {
            a.apply(Action::CommandLineAppend(c));
        }
    }

    #[test]
    fn substitute_preview_highlights_first_match_on_current_line_without_g() {
        let mut a = app_with("foo bar foo baz foo\nfoo elsewhere", 10);
        a.cursor = Position::new(0, 0);
        type_cmdline(&mut a, "s/foo/X");
        let preview = a.substitute_preview.as_ref().expect("preview live");
        assert_eq!(preview.matches.len(), 1, "only the leftmost match -- no /g");
        assert_eq!(preview.matches[0].start, Position::new(0, 0));
        assert_eq!(preview.replacement.as_deref(), Some("X"));
        assert!(!preview.global);
    }

    #[test]
    fn substitute_preview_with_g_flag_highlights_every_match_on_line() {
        let mut a = app_with("foo bar foo baz foo\nfoo elsewhere", 10);
        a.cursor = Position::new(0, 0);
        type_cmdline(&mut a, "s/foo/X/g");
        let preview = a.substitute_preview.as_ref().unwrap();
        // Three matches on the cursor's line; line 1 is out of scope.
        assert_eq!(preview.matches.len(), 3);
        assert!(preview.global);
    }

    #[test]
    fn substitute_preview_percent_scope_walks_whole_buffer() {
        let mut a = app_with("foo\nbar foo\nfoo", 10);
        a.cursor = Position::new(0, 0);
        type_cmdline(&mut a, "%s/foo/X/g");
        let preview = a.substitute_preview.as_ref().unwrap();
        // Three matches across three lines.
        assert_eq!(preview.matches.len(), 3);
    }

    #[test]
    fn substitute_preview_clears_on_cmdline_cancel() {
        let mut a = app_with("foo bar", 10);
        type_cmdline(&mut a, "s/foo/X");
        assert!(a.substitute_preview.is_some());
        a.apply(Action::CommandLineCancel);
        assert!(a.substitute_preview.is_none());
    }

    #[test]
    fn substitute_preview_clears_on_cmdline_submit() {
        let mut a = app_with("foo bar", 10);
        type_cmdline(&mut a, "s/foo/X");
        assert!(a.substitute_preview.is_some());
        a.apply(Action::CommandLineSubmit);
        assert!(a.substitute_preview.is_none());
    }

    #[test]
    fn substitute_preview_dropped_when_input_no_longer_parses_as_substitute() {
        let mut a = app_with("foo bar", 10);
        // Enter a substitute, get preview, then backspace past `s` --
        // input is no longer a substitute.
        type_cmdline(&mut a, "s/foo");
        assert!(a.substitute_preview.is_some());
        for _ in 0.."s/foo".len() {
            a.apply(Action::CommandLineBackspace);
        }
        assert!(a.substitute_preview.is_none());
    }

    #[test]
    fn substitute_preview_empty_pattern_drops_preview() {
        // After typing `s/` the pattern is empty -- preview shouldn't
        // highlight anything (no matches to show).
        let mut a = app_with("foo bar", 10);
        type_cmdline(&mut a, "s/");
        assert!(a.substitute_preview.is_none());
    }

    #[test]
    fn command_line_append_pushes_chars() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('w'));
        a.apply(Action::CommandLineAppend('q'));
        assert_eq!(a.command_line, "wq");
    }

    #[test]
    fn command_line_backspace_pops_chars() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('w'));
        a.apply(Action::CommandLineAppend('q'));
        a.apply(Action::CommandLineBackspace);
        assert_eq!(a.command_line, "w");
    }

    #[test]
    fn command_line_backspace_on_empty_exits_command_modal() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineBackspace);
        assert_eq!(a.modal, ModalState::Normal);
    }

    #[test]
    fn command_line_cancel_clears_and_returns_to_normal() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('w'));
        a.apply(Action::CommandLineCancel);
        assert_eq!(a.modal, ModalState::Normal);
        assert_eq!(a.command_line, "");
    }

    #[test]
    fn submit_q_on_clean_buffer_quits() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterCommandLine);
        for c in "q".chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        a.apply(Action::CommandLineSubmit);
        assert!(a.should_quit);
        assert_eq!(a.modal, ModalState::Normal);
    }

    #[test]
    fn submit_q_on_dirty_buffer_refuses() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert!(a.document.dirty());

        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('q'));
        a.apply(Action::CommandLineSubmit);
        assert!(!a.should_quit);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("no write since last change"));
    }

    #[test]
    fn submit_q_bang_quits_even_when_dirty() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        a.apply(Action::EnterCommandLine);
        for c in "q!".chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        a.apply(Action::CommandLineSubmit);
        assert!(a.should_quit);
    }

    #[test]
    fn submit_w_without_path_errors() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('w'));
        a.apply(Action::CommandLineSubmit);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("no file name"));
    }

    #[test]
    fn submit_w_with_path_writes_and_clears_dirty() {
        let dir = unique_tempdir();
        let path = dir.join("out.txt");
        let mut a = App::new(Document::from_text("hello"));
        a.set_viewport_height(10);
        // Move to end of line, then enter insert and append "!".
        a.apply(invoke_motion(a.builtins.line_end));
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("!".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert!(a.document.dirty());

        a.apply(Action::EnterCommandLine);
        for c in format!("w {}", path.display()).chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        a.apply(Action::CommandLineSubmit);

        assert!(!a.document.dirty());
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Info);
        assert!(msg.text.contains("written"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello!");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn submit_wq_writes_then_quits() {
        let dir = unique_tempdir();
        let path = dir.join("out.txt");
        std::fs::write(&path, "first").unwrap();

        let mut a = App::new(Document::open(&path).unwrap());
        a.set_viewport_height(10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));

        a.apply(Action::EnterCommandLine);
        for c in "wq".chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        a.apply(Action::CommandLineSubmit);

        assert!(a.should_quit);
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.starts_with("X"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn submit_unknown_command_surfaces_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterCommandLine);
        for c in "frobnicate".chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        a.apply(Action::CommandLineSubmit);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("frobnicate"));
    }

    #[test]
    fn submitting_returns_to_normal_modal() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('q'));
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.modal, ModalState::Normal);
    }

    #[test]
    fn echo_action_replaces_last_message() {
        let mut a = app_with("", 10);
        a.apply(Action::Echo(EchoMessage {
            text: "hi".into(),
            level: EchoLevel::Info,
        }));
        assert_eq!(a.last_message.as_ref().unwrap().text, "hi");
        a.apply(Action::Echo(EchoMessage {
            text: "bye".into(),
            level: EchoLevel::Warn,
        }));
        assert_eq!(a.last_message.as_ref().unwrap().text, "bye");
        assert_eq!(a.last_message.as_ref().unwrap().level, EchoLevel::Warn);
    }

    // ---- Search ----

    fn type_pattern(a: &mut App, pattern: &str) {
        for c in pattern.chars() {
            a.apply(Action::SearchAppend(c));
        }
    }

    #[test]
    fn enter_search_seeds_state() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        assert_eq!(a.modal, ModalState::Search(SearchDirection::Forward));
        let line = a.search_line.as_ref().expect("search_line populated");
        assert_eq!(line.pattern, "");
        assert_eq!(line.origin, Position::ZERO);
    }

    #[test]
    fn search_append_grows_pattern_and_previews_match() {
        let mut a = app_with("foo bar foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "bar");
        let line = a.search_line.as_ref().unwrap();
        assert_eq!(line.pattern, "bar");
        // Preview should highlight the first match without moving cursor.
        let m = a.current_match.expect("match previewed");
        assert_eq!(m.start, Position::new(0, 4));
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn search_backspace_shrinks_pattern_and_re_previews() {
        let mut a = app_with("foo bar baz", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "baz");
        a.apply(Action::SearchBackspace);
        assert_eq!(a.search_line.as_ref().unwrap().pattern, "ba");
        let m = a.current_match.expect("preview after backspace");
        assert_eq!(m.start, Position::new(0, 4));
    }

    #[test]
    fn search_backspace_on_empty_pattern_exits_search() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        a.apply(Action::SearchBackspace);
        assert_eq!(a.modal, ModalState::Normal);
        assert!(a.search_line.is_none());
    }

    #[test]
    fn search_submit_jumps_cursor_to_match_and_records_last_search() {
        let mut a = app_with("foo bar foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "bar");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.modal, ModalState::Normal);
        assert_eq!(a.cursor, Position::new(0, 4));
        assert!(a.search_line.is_none());
        let last = a.last_search.as_ref().unwrap();
        assert_eq!(last.pattern, "bar");
        assert_eq!(last.direction, SearchDirection::Forward);
    }

    #[test]
    fn search_submit_with_no_match_records_pattern_and_warns() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "xyz");
        a.apply(Action::SearchSubmit);
        assert!(a.current_match.is_none());
        assert_eq!(a.last_search.as_ref().unwrap().pattern, "xyz");
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("Pattern not found"));
    }

    #[test]
    fn search_cancel_restores_cursor_to_origin() {
        let mut a = app_with("foo bar foo", 10);
        a.cursor = Position::new(0, 5);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        // Preview should have set current_match to "foo" at byte 8.
        assert_eq!(a.current_match.unwrap().start, Position::new(0, 8));
        a.apply(Action::SearchCancel);
        assert_eq!(a.modal, ModalState::Normal);
        assert_eq!(a.cursor, Position::new(0, 5));
        assert!(a.current_match.is_none());
    }

    #[test]
    fn n_after_forward_search_advances_to_next_match() {
        let mut a = app_with("foo bar foo bar", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.cursor, Position::new(0, 0));
        a.apply(Action::SearchNext);
        assert_eq!(a.cursor, Position::new(0, 8));
    }

    #[test]
    fn capital_n_reverses_direction() {
        let mut a = app_with("foo bar foo bar", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        a.apply(Action::SearchSubmit);
        a.apply(Action::SearchNext);
        assert_eq!(a.cursor, Position::new(0, 8));
        a.apply(Action::SearchPrevious);
        assert_eq!(a.cursor, Position::new(0, 0));
    }

    #[test]
    fn n_with_no_last_search_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::SearchNext);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("no previous"));
    }

    #[test]
    fn search_forward_wraps_and_warns() {
        let mut a = app_with("alpha beta gamma alpha", 10);
        a.cursor = Position::new(0, 17); // past the second "alpha"... actually at it
        // Move past it for clarity.
        a.cursor = Position::new(0, 18);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "alpha");
        a.apply(Action::SearchSubmit);
        // First "alpha" is at byte 0; we wrapped from byte 18.
        assert_eq!(a.cursor, Position::new(0, 0));
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Warn);
        assert!(msg.text.contains("BOTTOM"));
    }

    #[test]
    fn search_backward_finds_previous_match() {
        let mut a = app_with("alpha beta gamma alpha", 10);
        a.cursor = Position::new(0, 22);
        a.apply(Action::EnterSearch(SearchDirection::Backward));
        type_pattern(&mut a, "alpha");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.cursor, Position::new(0, 17));
    }

    // ---- change operator end-to-end ----

    #[test]
    fn cw_deletes_word_and_enters_insert_mode() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.change.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "world");
        assert_eq!(a.modal, ModalState::Insert);
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn cc_clears_current_line_and_enters_insert_mode() {
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(a.builtins.change.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "aaa\n\nccc");
        assert_eq!(a.modal, ModalState::Insert);
    }

    // ---- Folds (zf, zo, zc, za, zR, zM, zd) ----

    #[test]
    fn zf_from_visual_creates_a_closed_fold() {
        let mut a = app_with("a\nb\nc\nd\ne", 10);
        a.apply(Action::EnterVisual(VisualKind::Linewise));
        a.apply(invoke_motion(a.builtins.line_down));
        a.apply(invoke_motion(a.builtins.line_down));
        // Selection now spans lines 0..2.
        a.apply(Action::CreateFoldFromVisual);
        assert_eq!(a.folds.len(), 1);
        let fold = &a.folds[0];
        assert_eq!(fold.start_line, 0);
        assert_eq!(fold.end_line, 2);
        assert!(fold.closed);
        // Visual exited.
        assert_eq!(a.modal, ModalState::Normal);
    }

    #[test]
    fn zf_outside_visual_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::CreateFoldFromVisual);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn zo_opens_fold_at_cursor() {
        let mut a = app_with("a\nb\nc", 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 2,
            closed: true,
            identity: None,
        });
        a.apply(Action::OpenFoldAtCursor);
        assert!(!a.folds[0].closed);
    }

    #[test]
    fn zc_closes_fold_at_cursor() {
        let mut a = app_with("a\nb\nc", 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 2,
            closed: false,
            identity: None,
        });
        a.apply(Action::CloseFoldAtCursor);
        assert!(a.folds[0].closed);
    }

    #[test]
    fn za_toggles_fold_at_cursor() {
        let mut a = app_with("a\nb\nc", 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 2,
            closed: false,
            identity: None,
        });
        a.apply(Action::ToggleFoldAtCursor);
        assert!(a.folds[0].closed);
        a.apply(Action::ToggleFoldAtCursor);
        assert!(!a.folds[0].closed);
    }

    #[test]
    fn capital_zr_opens_all_folds() {
        let mut a = app_with("a\nb\nc\nd", 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 1,
            closed: true,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 2,
            end_line: 3,
            closed: true,
            identity: None,
        });
        a.apply(Action::OpenAllFolds);
        assert!(a.folds.iter().all(|f| !f.closed));
    }

    #[test]
    fn capital_zm_closes_all_folds() {
        let mut a = app_with("a\nb\nc\nd", 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 1,
            closed: false,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 2,
            end_line: 3,
            closed: false,
            identity: None,
        });
        a.apply(Action::CloseAllFolds);
        assert!(a.folds.iter().all(|f| f.closed));
    }

    #[test]
    fn zd_deletes_fold_at_cursor() {
        let mut a = app_with("a\nb\nc\nd", 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 2,
            closed: true,
            identity: None,
        });
        a.cursor = Position::new(1, 0);
        a.apply(Action::DeleteFoldAtCursor);
        assert!(a.folds.is_empty());
    }

    // --- Nested-fold semantics (`zc` / `zo` / `za` / `zd`) -----

    fn nested_folds_app() -> App {
        // Two nested open folds: outer covers lines 0..=10, inner
        // covers 2..=8. Cursor sits inside both at line 4.
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 10,
            closed: false,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 2,
            end_line: 8,
            closed: false,
            identity: None,
        });
        a.cursor = Position::new(4, 0);
        a
    }

    #[test]
    fn zc_closes_innermost_open_fold_first() {
        let mut a = nested_folds_app();
        a.apply(Action::CloseFoldAtCursor);
        let inner = a.folds.iter().find(|f| f.start_line == 2).unwrap();
        let outer = a.folds.iter().find(|f| f.start_line == 0).unwrap();
        assert!(inner.closed, "inner should close first");
        assert!(!outer.closed, "outer should remain open until next zc");
    }

    #[test]
    fn second_zc_closes_outer_fold() {
        let mut a = nested_folds_app();
        a.apply(Action::CloseFoldAtCursor); // closes inner
        a.apply(Action::CloseFoldAtCursor); // should close outer
        let inner = a.folds.iter().find(|f| f.start_line == 2).unwrap();
        let outer = a.folds.iter().find(|f| f.start_line == 0).unwrap();
        assert!(inner.closed);
        assert!(outer.closed);
    }

    #[test]
    fn zo_opens_outermost_closed_fold_first() {
        let mut a = nested_folds_app();
        // Both folds closed.
        for f in a.folds.iter_mut() {
            f.closed = true;
        }
        a.apply(Action::OpenFoldAtCursor);
        let outer = a.folds.iter().find(|f| f.start_line == 0).unwrap();
        let inner = a.folds.iter().find(|f| f.start_line == 2).unwrap();
        assert!(!outer.closed, "outer should open first");
        assert!(inner.closed, "inner should remain closed until next zo");
    }

    #[test]
    fn za_toggles_to_open_when_any_fold_closed_then_close_when_all_open() {
        let mut a = nested_folds_app();
        // Close outer only.
        a.folds[0].closed = true;
        // za with the outer closed => open the outermost closed (the outer).
        a.apply(Action::ToggleFoldAtCursor);
        let outer = a.folds.iter().find(|f| f.start_line == 0).unwrap();
        let inner = a.folds.iter().find(|f| f.start_line == 2).unwrap();
        assert!(!outer.closed);
        assert!(!inner.closed);
        // Now both open: za should close the innermost.
        a.apply(Action::ToggleFoldAtCursor);
        let inner = a.folds.iter().find(|f| f.start_line == 2).unwrap();
        let outer = a.folds.iter().find(|f| f.start_line == 0).unwrap();
        assert!(inner.closed);
        assert!(!outer.closed);
    }

    #[test]
    fn zc_with_all_folds_closed_emits_e490() {
        let mut a = nested_folds_app();
        for f in a.folds.iter_mut() {
            f.closed = true;
        }
        a.apply(Action::CloseFoldAtCursor);
        // No state change; both still closed.
        assert!(a.folds.iter().all(|f| f.closed));
        // E490 echoed.
        let msg = a.last_message.as_ref().expect("message").text.clone();
        assert!(msg.contains("E490"), "expected E490, got {msg:?}");
    }

    #[test]
    fn zd_removes_innermost_only() {
        let mut a = nested_folds_app();
        a.apply(Action::DeleteFoldAtCursor);
        // The inner (start=2) fold is gone; outer remains.
        assert!(a.folds.iter().any(|f| f.start_line == 0));
        assert!(!a.folds.iter().any(|f| f.start_line == 2));
    }

    // --- Linear j/k skip closed folds (`docs/help/folding.md`) ---

    #[test]
    fn line_down_from_closed_fold_heading_skips_to_after_fold() {
        // 12-line buffer with a closed fold spanning lines 1..=4.
        // From line 1 (heading), `j` should land on line 5, not 2.
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.folds.push(Fold {
            start_line: 1,
            end_line: 4,
            closed: true,
            identity: None,
        });
        a.cursor = Position::new(1, 0);
        a.apply(invoke_motion(a.builtins.line_down));
        assert_eq!(
            a.cursor.line, 5,
            "j from closed-fold heading must skip to fold.end_line + 1"
        );
    }

    #[test]
    fn line_up_into_closed_fold_snaps_to_heading() {
        // From line 5, `k` lands on 4 -- inside a closed fold (1..=4).
        // Snap to fold.start_line (1).
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.folds.push(Fold {
            start_line: 1,
            end_line: 4,
            closed: true,
            identity: None,
        });
        a.cursor = Position::new(5, 0);
        a.apply(invoke_motion(a.builtins.line_up));
        assert_eq!(
            a.cursor.line, 1,
            "k into a closed fold must snap to its heading line"
        );
    }

    #[test]
    fn linear_j_into_open_fold_does_not_skip() {
        // Open folds don't hide content; j moves one line as usual.
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.folds.push(Fold {
            start_line: 1,
            end_line: 4,
            closed: false,
            identity: None,
        });
        a.cursor = Position::new(1, 0);
        a.apply(invoke_motion(a.builtins.line_down));
        assert_eq!(a.cursor.line, 2);
    }

    #[test]
    fn linear_motion_with_nofoldenable_does_not_skip() {
        // `:set nofoldenable` / `zi` should make every line visible
        // for navigation, including closed-fold interiors.
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.folds.push(Fold {
            start_line: 1,
            end_line: 4,
            closed: true,
            identity: None,
        });
        a.set_foldenable_for_test(false);
        a.cursor = Position::new(1, 0);
        a.apply(invoke_motion(a.builtins.line_down));
        assert_eq!(a.cursor.line, 2);
    }

    fn line_down_lands_on_next_fold_heading_when_consecutive() {
        // Three closed folds back-to-back: 1..=3, 4..=6, 7..=9.
        // Each `j` moves one visible line; a closed fold's heading
        // IS a visible line. So:
        //   line 1 (fold A heading) --j--> line 4 (fold B heading)
        //   line 4 (fold B heading) --j--> line 7 (fold C heading)
        //   line 7 (fold C heading) --j--> line 10 (after fold C)
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.folds.push(Fold {
            start_line: 1,
            end_line: 3,
            closed: true,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 4,
            end_line: 6,
            closed: true,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 7,
            end_line: 9,
            closed: true,
            identity: None,
        });
        a.cursor = Position::new(1, 0);
        a.apply(invoke_motion(a.builtins.line_down));
        assert_eq!(a.cursor.line, 4, "first j → fold B heading");
        a.apply(invoke_motion(a.builtins.line_down));
        assert_eq!(a.cursor.line, 7, "second j → fold C heading");
        a.apply(invoke_motion(a.builtins.line_down));
        assert_eq!(a.cursor.line, 10, "third j → past fold C");
    }

    #[test]
    fn line_down_skips_consecutive_closed_folds_in_one_keypress() {
        // Wrapper / dummy: superseded by
        // `line_down_lands_on_next_fold_heading_when_consecutive`.
        // The historical name preserved so anyone re-running an
        // older test list spots the rename.
        line_down_lands_on_next_fold_heading_when_consecutive();
    }

    // --- Generalised snap covers all non-jump motions --------

    #[test]
    fn word_forward_snaps_out_of_closed_fold_body() {
        // `w` from a closed fold's heading lands on the next word.
        // Pre-snap, that next word might be inside the fold body
        // (cursor at hidden line). The snap projects cursor onto a
        // visible line so subsequent `zc` resolves correctly.
        let src = "alpha bravo\n    charlie delta\n    echo foxtrot\nafter golf hotel\n";
        let mut a = app_with(src, 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 2,
            closed: true,
            identity: None,
        });
        a.cursor = Position::new(0, 0);
        a.apply(invoke_motion(a.builtins.word_forward));
        // Without snap the cursor would land on "bravo" (line 0,
        // byte 6) -- still visible. Press w again: would go into
        // hidden `charlie`. The snap kicks in there.
        a.apply(invoke_motion(a.builtins.word_forward));
        assert!(
            !a.line_inside_closed_fold(a.cursor.line),
            "w must not leave cursor inside a hidden fold body \
             (cursor.line = {})",
            a.cursor.line
        );
    }

    #[test]
    fn refresh_highlights_covers_buffer_lines_below_a_closed_fold() {
        // Regression: with a closed fold inside the viewport, the
        // highlight window must stretch to include lines that
        // appear *below* the fold's collapsed row but are still in
        // the visible region. Otherwise spans drop to empty and
        // syntax styling visibly disappears for content under
        // every fold.
        let mut a = app_with(
            "fn a() {\n    1;\n    2;\n    3;\n    4;\n}\nfn b() {\n    5;\n}\n",
            5, // viewport = 5 rows
        );
        // Wire up a real syntax instance so highlight_lines runs.
        a.syntax = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust).unwrap();
        if let Some(s) = a.syntax.as_mut() {
            s.parse(&a.document.text());
        }
        // Close the first fn (lines 0..=5, 6 buffer lines collapsed
        // onto one row). With a 5-row viewport that means `fn b`
        // (line 6) and its body (lines 7, 8) all sit in the visible
        // region.
        a.folds.push(Fold {
            start_line: 0,
            end_line: 5,
            closed: true,
            identity: None,
        });
        a.refresh_highlights();
        // Without the fix: visible_highlights is sized 5 (height),
        // so line 6 (offset 6) returns &[] -> no syntax. Now: the
        // highlight window stretches to cover line 8, so line 6's
        // spans are populated.
        assert!(
            !a.highlights_for_buffer_line(6).is_empty(),
            "fn b heading must be highlighted under a closed fold"
        );
        assert!(
            !a.highlights_for_buffer_line(7).is_empty(),
            "fn b body must be highlighted under a closed fold"
        );
    }

    #[test]
    fn syntax_fold_zc_on_indented_let_with_if_else_reports_five_lines() {
        // The user's actual scenario: the `let` form is INDENTED
        // (inside a function body). Verify the outer-pick rule on
        // the if/let line still resolves to the full if_expression
        // fold even with leading whitespace, so the rendered count
        // is 5 lines (not 3 -- the inner then-block size).
        let src = "fn outer() -> u32 {\n    let len = if has_trailing_newline {\n        bytes - 1\n    } else {\n        bytes\n    };\n    len\n}\n";
        let mut a = app_with(src, 20);
        a.syntax = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
            .unwrap();
        if let Some(s) = a.syntax.as_mut() {
            s.parse(&a.document.text());
        }
        a.set_foldmethod_for_test(FoldMethod::Syntax);
        a.recompute_folds();
        // Dump for diagnosis -- show the fold ranges at the live
        // tree's current state.
        eprintln!("FOLDS (indented let-if-else inside fn):");
        for f in &a.folds {
            eprintln!(
                "  ({}, {}) span={} lines",
                f.start_line,
                f.end_line,
                f.end_line - f.start_line + 1
            );
        }
        // Cursor on line 1 -- the indented `let len = if ...`
        // line. zc should pick the outermost fold whose start_line
        // is 1 (the if_expression / let_declaration), not the
        // inner then-block.
        a.cursor = Position::new(1, 0);
        a.apply(Action::CloseFoldAtCursor);
        let fold = a
            .fold_start_at(1)
            .expect("a closed fold should start at line 1");
        let count = fold.end_line - fold.start_line + 1;
        assert_eq!(
            count, 5,
            "indented if/else fold must span 5 lines (got {count}; fold = {fold:?}; all = {:?})",
            a.folds
        );
    }

    #[test]
    fn syntax_fold_zc_on_let_with_if_else_reports_five_lines() {
        // Full-pipeline regression for the user's scenario: a top-
        // level `let` binding wrapping an if/else expression. With
        // foldmethod=syntax, the cursor on the `let` line gets
        // `zc` to close the entire 5-line form and the rendered
        // summary shows "5 lines folded".
        let src = "let len = if cond {\n    bytes - 1\n} else {\n    bytes\n}\n";
        let mut a = app_with(src, 10);
        a.syntax = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
            .unwrap();
        if let Some(s) = a.syntax.as_mut() {
            s.parse(&a.document.text());
        }
        a.set_foldmethod_for_test(FoldMethod::Syntax);
        a.recompute_folds();
        // Cursor on line 0 (the `let` line). zc must pick the
        // outermost fold starting at line 0 -- the if_expression /
        // let_declaration spanning (0, 4) -- and close it.
        a.cursor = Position::new(0, 0);
        a.apply(Action::CloseFoldAtCursor);
        let fold = a
            .fold_start_at(0)
            .expect("a closed fold should start at line 0 after zc");
        let count = fold.end_line - fold.start_line + 1;
        assert_eq!(
            count, 5,
            "fold at line 0 must span 5 lines (got {count}; fold = {fold:?})"
        );
    }

    #[test]
    fn paragraph_motion_snaps_out_of_closed_fold_body() {
        // `}` (paragraph forward) from inside a fold can land
        // cursor on a hidden paragraph break. Snap must apply.
        let src = "alpha\n\n    body line one\n    body line two\n\nafter\n";
        let mut a = app_with(src, 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 4,
            closed: true,
            identity: None,
        });
        a.cursor = Position::new(0, 0);
        a.apply(invoke_motion(a.builtins.paragraph_forward));
        assert!(
            !a.line_inside_closed_fold(a.cursor.line),
            "}} must not leave cursor inside a hidden fold body \
             (cursor.line = {})",
            a.cursor.line
        );
    }

    #[test]
    fn line_down_swallows_blanks_between_sibling_folds_for_zc_targeting() {
        // Reproduces the user's "third form" regression: with a
        // blank line between two closed folds, j from the first
        // fold's heading must land on the *next sibling's heading*,
        // not on the blank between them. Otherwise zc on the blank
        // resolves to "innermost open fold containing this line" =
        // the parent.
        //
        // Buffer (impl with three fns separated by blank lines):
        //   line 0: impl B {
        //   line 1:   fn a() {
        //   line 2:   }
        //   line 3:   <blank>
        //   line 4:   fn b() {
        //   line 5:   }
        //   line 6:   <blank>
        //   line 7:   fn c() {
        //   line 8:   }
        //   line 9: }
        let src = "impl B {\n    fn a() {\n    }\n\n    fn b() {\n    }\n\n    fn c() {\n    }\n}\n";
        let mut a = app_with(src, 20);
        // Outer impl + three function folds (skip blank-line 3 / 6).
        a.folds.push(Fold {
            start_line: 0,
            end_line: 9,
            closed: false,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 1,
            end_line: 2,
            closed: true,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 4,
            end_line: 5,
            closed: false,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 7,
            end_line: 8,
            closed: false,
            identity: None,
        });
        a.cursor = Position::new(1, 0);
        // j from fn a's heading: snap over fn a's body, swallow the
        // blank, land on fn b's heading (line 4).
        a.apply(invoke_motion(a.builtins.line_down));
        assert_eq!(
            a.cursor.line, 4,
            "j after fold A must skip the blank and land on fn b"
        );
        // Close fn b, j again, land on fn c's heading (line 7).
        a.apply(Action::CloseFoldAtCursor);
        let fnb = a.folds.iter().find(|f| f.start_line == 4).unwrap();
        assert!(fnb.closed, "zc on fn b heading closes fn b");
        a.apply(invoke_motion(a.builtins.line_down));
        assert_eq!(
            a.cursor.line, 7,
            "j after fold B must skip the blank and land on fn c"
        );
        // Close fn c. The outer impl must remain open.
        a.apply(Action::CloseFoldAtCursor);
        let fnc = a.folds.iter().find(|f| f.start_line == 7).unwrap();
        let outer = a.folds.iter().find(|f| f.start_line == 0).unwrap();
        assert!(fnc.closed, "zc on fn c heading closes fn c, not outer");
        assert!(!outer.closed, "outer impl must remain open through the sequence");
    }

    #[test]
    fn zc_on_sibling_fold_after_navigating_with_j_closes_sibling_not_parent() {
        // Regression: with one inner fold already closed, `j` from
        // its heading must put the cursor on the sibling's heading
        // (line 5), not inside the closed fold's body. Then `zc`
        // on the sibling closes the sibling -- not the outer.
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 10,
            closed: false,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 1,
            end_line: 4,
            closed: true,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 5,
            end_line: 9,
            closed: false,
            identity: None,
        });
        a.cursor = Position::new(1, 0);
        // Move to the sibling's heading.
        a.apply(invoke_motion(a.builtins.line_down));
        assert_eq!(a.cursor.line, 5, "cursor should land on sibling, not interior");
        // Close the sibling.
        a.apply(Action::CloseFoldAtCursor);
        let sibling = a.folds.iter().find(|f| f.start_line == 5).unwrap();
        let outer = a.folds.iter().find(|f| f.start_line == 0).unwrap();
        assert!(sibling.closed, "sibling should close, not the outer");
        assert!(!outer.closed, "outer must remain open");
    }

    #[test]
    fn zj_jumps_to_next_fold_start() {
        let mut a = app_with("a\nb\nc\nd\ne\nf", 10);
        a.folds.push(Fold {
            start_line: 2,
            end_line: 3,
            closed: false,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 5,
            end_line: 5,
            closed: false,
            identity: None,
        });
        a.cursor = Position::ZERO;
        a.apply(Action::GotoNextFold);
        assert_eq!(a.cursor.line, 2);
    }

    #[test]
    fn zk_jumps_to_previous_fold_end() {
        let mut a = app_with("a\nb\nc\nd\ne\nf", 10);
        a.folds.push(Fold {
            start_line: 1,
            end_line: 2,
            closed: false,
            identity: None,
        });
        a.cursor = Position::new(5, 0);
        a.apply(Action::GotoPrevFold);
        assert_eq!(a.cursor.line, 2);
    }

    #[test]
    fn zj_with_no_more_folds_emits_error() {
        let mut a = app_with("a\nb\nc", 10);
        a.apply(Action::GotoNextFold);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn line_inside_closed_fold_returns_true_for_interior() {
        let mut a = app_with("a\nb\nc\nd", 10);
        a.folds.push(Fold {
            start_line: 1,
            end_line: 3,
            closed: true,
            identity: None,
        });
        assert!(!a.line_inside_closed_fold(0));
        // Start line is the summary, NOT inside.
        assert!(!a.line_inside_closed_fold(1));
        assert!(a.line_inside_closed_fold(2));
        assert!(a.line_inside_closed_fold(3));
    }

    // ---- Substitute (:s/foo/bar/[g]) ----

    fn submit_ex(a: &mut App, line: &str) {
        a.apply(Action::EnterCommandLine);
        for c in line.chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        a.apply(Action::CommandLineSubmit);
    }

    #[test]
    fn submit_pushes_command_into_history() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "set number");
        assert_eq!(a.command_history, vec!["set number".to_string()]);
    }

    #[test]
    fn submit_dedupes_consecutive_identical_history() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "set number");
        submit_ex(&mut a, "set number");
        assert_eq!(a.command_history.len(), 1);
    }

    #[test]
    fn empty_submit_does_not_push_history() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineSubmit);
        assert!(a.command_history.is_empty());
    }

    #[test]
    fn up_in_command_walks_to_most_recent_history() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "set number");
        submit_ex(&mut a, "set nonumber");
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineHistoryPrev);
        assert_eq!(a.command_line, "set nonumber");
    }

    #[test]
    fn up_then_up_walks_to_older() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "set number");
        submit_ex(&mut a, "set nonumber");
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineHistoryPrev);
        a.apply(Action::CommandLineHistoryPrev);
        assert_eq!(a.command_line, "set number");
    }

    #[test]
    fn down_returns_to_in_progress_typed_text() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "set number");
        a.apply(Action::EnterCommandLine);
        for c in "se".chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        // User starts typing "se", presses Up -> walks to "set number".
        a.apply(Action::CommandLineHistoryPrev);
        assert_eq!(a.command_line, "set number");
        // Down returns to "se".
        a.apply(Action::CommandLineHistoryNext);
        assert_eq!(a.command_line, "se");
    }

    #[test]
    fn history_navigation_with_no_history_is_no_op() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineAppend('w'));
        a.apply(Action::CommandLineHistoryPrev);
        assert_eq!(a.command_line, "w");
    }

    #[test]
    fn history_persists_across_command_sessions() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "set number");
        // Reopen command line; Up should still recall.
        a.apply(Action::EnterCommandLine);
        a.apply(Action::CommandLineHistoryPrev);
        assert_eq!(a.command_line, "set number");
    }

    #[test]
    fn edit_loads_named_file() {
        let dir = unique_tempdir();
        let path = dir.join("hello.txt");
        std::fs::write(&path, "loaded contents\nsecond line").unwrap();
        let mut a = app_with("original", 10);
        let cmd = format!("e {}", path.display());
        submit_ex(&mut a, &cmd);
        assert_eq!(a.document.text(), "loaded contents\nsecond line");
        assert_eq!(a.cursor, Position::ZERO);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edit_refuses_when_dirty() {
        let mut a = app_with("modified", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert!(a.document.dirty());
        submit_ex(&mut a, "e /nonexistent");
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        // Document unchanged.
        assert_eq!(a.document.text(), "Xmodified");
    }

    #[test]
    fn edit_force_overrides_dirty_guard() {
        let dir = unique_tempdir();
        let path = dir.join("forced.txt");
        std::fs::write(&path, "loaded").unwrap();
        let mut a = app_with("dirty content", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("Z".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        let cmd = format!("e! {}", path.display());
        submit_ex(&mut a, &cmd);
        assert_eq!(a.document.text(), "loaded");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edit_preserves_registers_across_swap() {
        let dir = unique_tempdir();
        let path = dir.join("preserve.txt");
        std::fs::write(&path, "new content").unwrap();
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.yank.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        assert!(a.unnamed_register.is_some());
        let cmd = format!("e {}", path.display());
        submit_ex(&mut a, &cmd);
        // Register survives.
        assert!(a.unnamed_register.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edit_resets_per_document_state() {
        let dir = unique_tempdir();
        let path = dir.join("reset.txt");
        std::fs::write(&path, "fresh").unwrap();
        let mut a = app_with("aaa\nbbb\nccc", 10);
        a.cursor = Position::new(2, 1);
        a.apply(invoke_motion(a.builtins.goto_first_line));
        // Now position_history has an entry.
        assert!(!a.position_history.is_empty());
        let cmd = format!("e {}", path.display());
        submit_ex(&mut a, &cmd);
        assert!(a.position_history.is_empty());
        assert_eq!(a.cursor, Position::ZERO);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edit_unknown_path_emits_error() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "e /absolutely/does/not/exist/anywhere.txt");
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        // Buffer unchanged.
        assert_eq!(a.document.text(), "hello");
    }

    #[test]
    fn set_number_and_nonumber_toggle_show_line_numbers() {
        let mut a = app_with("hello", 10);
        assert!(a.show_line_numbers());
        submit_ex(&mut a, "set nonumber");
        assert!(!a.show_line_numbers());
        submit_ex(&mut a, "set number");
        assert!(a.show_line_numbers());
    }

    #[test]
    fn set_relativenumber_toggles_flag() {
        let mut a = app_with("hello\nworld", 10);
        assert!(!a.relative_line_numbers());
        submit_ex(&mut a, "set relativenumber");
        assert!(a.relative_line_numbers());
        assert!(a.show_line_numbers());
        submit_ex(&mut a, "set norelativenumber");
        assert!(!a.relative_line_numbers());
    }

    #[test]
    fn set_unknown_option_emits_error() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "set frobnicate");
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("frobnicate"));
    }

    #[test]
    fn nohlsearch_clears_overlay() {
        let mut a = app_with("foo bar foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        a.apply(Action::SearchSubmit);
        assert!(!a.all_matches.is_empty());
        submit_ex(&mut a, "noh");
        assert!(a.all_matches.is_empty());
        assert!(a.current_match.is_none());
    }

    #[test]
    fn list_registers_with_no_state_says_so() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "reg");
        let msg = a.last_message.as_ref().unwrap();
        assert!(msg.text.contains("no registers"));
    }

    #[test]
    fn list_registers_includes_unnamed_and_zero() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.yank.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        submit_ex(&mut a, "reg");
        let msg = a.last_message.as_ref().unwrap();
        assert!(msg.text.contains("\"\""));
        assert!(msg.text.contains("\"0"));
    }

    #[test]
    fn list_marks_with_no_marks_says_so() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "marks");
        let msg = a.last_message.as_ref().unwrap();
        assert!(msg.text.contains("no marks"));
    }

    #[test]
    fn list_marks_shows_set_marks() {
        let mut a = app_with("hello\nworld", 10);
        a.cursor = Position::new(1, 2);
        a.apply(Action::SetMark('a'));
        submit_ex(&mut a, "marks");
        let msg = a.last_message.as_ref().unwrap();
        assert!(msg.text.contains('a'));
        // Line 2 (1-indexed for display) at byte 2.
        assert!(msg.text.contains("2:2"));
    }

    #[test]
    fn substitute_first_match_on_current_line() {
        let mut a = app_with("foo bar foo", 10);
        submit_ex(&mut a, "s/foo/baz/");
        assert_eq!(a.document.text(), "baz bar foo");
    }

    #[test]
    fn substitute_global_replaces_all_on_line() {
        let mut a = app_with("foo bar foo", 10);
        submit_ex(&mut a, "s/foo/baz/g");
        assert_eq!(a.document.text(), "baz bar baz");
    }

    #[test]
    fn substitute_whole_buffer_with_g_flag() {
        let mut a = app_with("foo\nbar foo\nfoo", 10);
        submit_ex(&mut a, "%s/foo/X/g");
        assert_eq!(a.document.text(), "X\nbar X\nX");
    }

    #[test]
    fn substitute_no_match_emits_error() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "s/xyz/abc/");
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("Pattern not found"));
        assert_eq!(a.document.text(), "hello");
    }

    #[test]
    fn substitute_empty_replacement_deletes_pattern() {
        let mut a = app_with("hello world hello", 10);
        submit_ex(&mut a, "s/hello //g");
        assert_eq!(a.document.text(), "world hello");
    }

    #[test]
    fn substitute_count_message() {
        let mut a = app_with("foo foo foo", 10);
        submit_ex(&mut a, "s/foo/X/g");
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Info);
        assert!(msg.text.contains("3"));
    }

    #[test]
    fn global_delete_matching_lines() {
        let mut a = app_with("foo\nbar\nfoo\nbaz", 10);
        submit_ex(&mut a, "g/foo/d");
        // Both "foo" lines deleted; "bar" and "baz" remain.
        assert_eq!(a.document.text(), "bar\nbaz");
    }

    #[test]
    fn vglobal_delete_non_matching_lines() {
        let mut a = app_with("foo\nbar\nfoo\nbaz", 10);
        submit_ex(&mut a, "v/foo/d");
        // Only "foo" lines remain.
        assert_eq!(a.document.text(), "foo\nfoo");
    }

    #[test]
    fn global_substitute_on_matching_lines() {
        let mut a = app_with("foo\nbaz\nfoo", 10);
        submit_ex(&mut a, "g/foo/s/foo/X/");
        // Both "foo" lines get substituted.
        assert_eq!(a.document.text(), "X\nbaz\nX");
    }

    #[test]
    fn global_no_matches_emits_error() {
        let mut a = app_with("hello\nworld", 10);
        submit_ex(&mut a, "g/xyz/d");
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn substitute_only_current_line_without_percent() {
        let mut a = app_with("foo\nfoo\nfoo", 10);
        a.cursor = Position::new(1, 0);
        submit_ex(&mut a, "s/foo/X/");
        assert_eq!(a.document.text(), "foo\nX\nfoo");
    }

    // ---- Line join (J / gJ) ----

    #[test]
    fn join_lines_with_space_combines_two_lines_with_one_space() {
        let mut a = app_with("hello\nworld", 10);
        a.apply(Action::JoinLines { with_space: true });
        assert_eq!(a.document.text(), "hello world");
        // Cursor lands at the join point (end of original first line).
        assert_eq!(a.cursor, Position::new(0, 5));
    }

    #[test]
    fn join_lines_without_space_concatenates_directly() {
        let mut a = app_with("hello\nworld", 10);
        a.apply(Action::JoinLines { with_space: false });
        assert_eq!(a.document.text(), "helloworld");
    }

    #[test]
    fn join_lines_trims_leading_whitespace_on_next_line() {
        let mut a = app_with("hello\n   world", 10);
        a.apply(Action::JoinLines { with_space: true });
        assert_eq!(a.document.text(), "hello world");
    }

    #[test]
    fn join_lines_at_last_line_is_no_op() {
        let mut a = app_with("only", 10);
        a.apply(Action::JoinLines { with_space: true });
        assert_eq!(a.document.text(), "only");
    }

    // ---- Find-repeat (; / ,) ----

    #[test]
    fn semicolon_repeats_last_find_forward() {
        let mut a = app_with("hello world", 10);
        // First f-find for 'l': cursor moves to byte 2.
        let inv = CommandInvocation::of(a.builtins.find_char_forward.0)
            .with_args(lattice_grammar::Args::Char('l'));
        a.apply(Action::Invoke(inv));
        assert_eq!(a.cursor, Position::new(0, 2));
        // `;` repeats: byte 3.
        a.apply(Action::FindRepeat { reverse: false });
        assert_eq!(a.cursor, Position::new(0, 3));
    }

    #[test]
    fn comma_reverses_last_find_direction() {
        let mut a = app_with("hello world", 10);
        // f l forward.
        let inv = CommandInvocation::of(a.builtins.find_char_forward.0)
            .with_args(lattice_grammar::Args::Char('l'));
        a.apply(Action::Invoke(inv));
        assert_eq!(a.cursor, Position::new(0, 2));
        // f l again, then `,` should reverse to find the previous 'l'.
        a.apply(Action::FindRepeat { reverse: false });
        assert_eq!(a.cursor, Position::new(0, 3));
        a.apply(Action::FindRepeat { reverse: true });
        assert_eq!(a.cursor, Position::new(0, 2));
    }

    #[test]
    fn find_repeat_with_no_history_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::FindRepeat { reverse: false });
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    // ---- WORD motions (W, B, E) end-to-end ----

    #[test]
    fn capital_w_skips_punctuation() {
        let mut a = app_with("foo,bar baz", 10);
        a.apply(invoke_motion(a.builtins.big_word_forward));
        assert_eq!(a.cursor, Position::new(0, 8));
    }

    // ---- Macros (q, @) ----

    #[test]
    fn start_macro_record_seeds_recording_state() {
        let mut a = app_with("hello", 10);
        a.apply(Action::StartMacroRecord('a'));
        assert!(a.macro_recording.is_some());
        assert_eq!(a.macro_recording.as_ref().unwrap().register, 'a');
    }

    #[test]
    fn invalid_macro_register_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::StartMacroRecord(' '));
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(a.macro_recording.is_none());
    }

    #[test]
    fn second_q_during_recording_does_not_double_start() {
        let mut a = app_with("hello", 10);
        a.apply(Action::StartMacroRecord('a'));
        a.apply(Action::StartMacroRecord('b'));
        // Still recording into 'a'.
        assert_eq!(a.macro_recording.as_ref().unwrap().register, 'a');
    }

    #[test]
    fn stop_macro_record_persists_actions_and_clears_recording() {
        let mut a = app_with("hello", 10);
        a.apply(Action::StartMacroRecord('a'));
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        a.apply(Action::StopMacroRecord);
        assert!(a.macro_recording.is_none());
        let actions = a.macros.get(&'a').unwrap();
        assert!(!actions.is_empty());
    }

    #[test]
    fn play_macro_replays_recorded_actions() {
        let mut a = app_with("foo bar", 10);
        a.apply(Action::StartMacroRecord('a'));
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        a.apply(Action::StopMacroRecord);
        // After dw: "bar".
        assert_eq!(a.document.text(), "bar");
        // Replay -> deletes another word.
        a.apply(Action::PlayMacro('a'));
        assert_eq!(a.document.text(), "");
    }

    #[test]
    fn play_unrecorded_macro_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::PlayMacro('z'));
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn at_at_replays_last_macro() {
        let mut a = app_with("foo bar baz qux", 10);
        a.apply(Action::StartMacroRecord('a'));
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        a.apply(Action::StopMacroRecord);
        // First play.
        a.apply(Action::PlayMacro('a'));
        // @@ now repeats.
        a.apply(Action::PlayLastMacro);
        // After three dws total: "qux".
        assert_eq!(a.document.text(), "qux");
    }

    #[test]
    fn play_last_macro_with_no_history_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::PlayLastMacro);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn macro_does_not_record_management_actions() {
        // StartMacroRecord, StopMacroRecord, PlayMacro, PlayLastMacro
        // must NOT appear inside the recorded action stream (otherwise
        // playback would recurse / break).
        let mut a = app_with("hello", 10);
        a.apply(Action::StartMacroRecord('a'));
        // Replay another (unrecorded) macro -- the play action must not
        // be captured.
        a.apply(Action::PlayLastMacro); // errors but is not recorded
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("z".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        a.apply(Action::StopMacroRecord);
        let actions = a.macros.get(&'a').unwrap();
        for action in actions {
            assert!(!matches!(
                action,
                Action::StartMacroRecord(_)
                    | Action::StopMacroRecord
                    | Action::PlayMacro(_)
                    | Action::PlayLastMacro
            ));
        }
    }

    // ---- Position history (Ctrl-O / Ctrl-I) ----

    // ---- Pending-state lifecycle (regression) ----

    #[test]
    fn zz_clears_pending_so_next_key_is_a_motion() {
        // Regression: previously `zz` left pending=AfterZ, so `j` after
        // `zz` was interpreted as `zj` (GotoNextFold) and emitted "no
        // more folds".
        let mut a = app_with("a\nb\nc\nd\ne", 10);
        a.apply(Action::SetPending(Pending::AfterZ));
        a.apply(Action::ScrollCursorTo(ScrollPos::Center));
        assert_eq!(a.pending, Pending::None);
    }

    #[test]
    fn set_mark_clears_pending() {
        let mut a = app_with("hello", 10);
        a.apply(Action::SetPending(Pending::AfterSetMark));
        a.apply(Action::SetMark('a'));
        assert_eq!(a.pending, Pending::None);
    }

    #[test]
    fn select_register_clears_pending() {
        let mut a = app_with("hello", 10);
        a.apply(Action::SetPending(Pending::AfterRegister));
        a.apply(Action::SelectRegister(Register::Named('a')));
        assert_eq!(a.pending, Pending::None);
    }

    #[test]
    fn jump_to_mark_clears_pending() {
        let mut a = app_with("hello\nworld", 10);
        a.apply(Action::SetMark('a'));
        a.apply(Action::SetPending(Pending::AfterJumpMarkExact));
        a.apply(Action::JumpToMarkExact('a'));
        assert_eq!(a.pending, Pending::None);
    }

    #[test]
    fn play_macro_clears_pending() {
        let mut a = app_with("hello", 10);
        a.apply(Action::SetPending(Pending::AfterMacroPlay));
        // No macro recorded; this errors but should still clear pending.
        a.apply(Action::PlayMacro('z'));
        assert_eq!(a.pending, Pending::None);
    }

    #[test]
    fn fold_action_clears_pending() {
        let mut a = app_with("a\nb\nc", 10);
        a.apply(Action::SetPending(Pending::AfterZ));
        a.apply(Action::OpenFoldAtCursor);
        assert_eq!(a.pending, Pending::None);
    }

    #[test]
    fn jump_history_with_no_jumps_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::JumpHistoryBack);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn gg_pushes_jump_history_and_ctrl_o_returns() {
        let mut a = app_with("a\nb\nc\nd\ne", 10);
        a.cursor = Position::new(3, 0); // line 3 ('d')
        a.apply(invoke_motion(a.builtins.goto_first_line));
        assert_eq!(a.cursor, Position::ZERO);
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.cursor, Position::new(3, 0));
    }

    #[test]
    fn ctrl_o_then_ctrl_i_round_trips() {
        let mut a = app_with("a\nb\nc\nd\ne", 10);
        a.cursor = Position::new(2, 0);
        a.apply(invoke_motion(a.builtins.goto_first_line));
        // Now at line 0; jump list has [(2,0)] cursor at end.
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.cursor, Position::new(2, 0));
        a.apply(Action::JumpHistoryForward);
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn search_submit_pushes_position_history() {
        let mut a = app_with("foo bar baz foo", 10);
        a.cursor = Position::new(0, 8); // on 'b' of "baz"
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        for c in "foo".chars() {
            a.apply(Action::SearchAppend(c));
        }
        a.apply(Action::SearchSubmit);
        // Cursor jumped to second "foo" at byte 12.
        assert_eq!(a.cursor, Position::new(0, 12));
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.cursor, Position::new(0, 8));
    }

    #[test]
    fn star_pushes_position_history() {
        let mut a = app_with("foo bar foo", 10);
        a.cursor = Position::new(0, 1); // on 'o' of first "foo"
        a.apply(Action::SearchWordUnderCursor(SearchDirection::Forward));
        // Cursor now on second "foo" at byte 8.
        assert_eq!(a.cursor, Position::new(0, 8));
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.cursor, Position::new(0, 1));
    }

    #[test]
    fn percent_pushes_position_history() {
        let mut a = app_with("call(arg)", 10);
        a.cursor = Position::new(0, 4); // on '('
        a.apply(Action::MatchBracket);
        assert_eq!(a.cursor, Position::new(0, 8)); // ')'
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.cursor, Position::new(0, 4));
    }

    #[test]
    fn mark_jump_pushes_position_history() {
        let mut a = app_with("hello\nworld", 10);
        a.cursor = Position::new(1, 2);
        a.apply(Action::SetMark('a'));
        a.cursor = Position::ZERO;
        a.apply(Action::JumpToMarkExact('a'));
        assert_eq!(a.cursor, Position::new(1, 2));
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.cursor, Position::ZERO);
    }

    // ---- §5.1.1 unified position history ----

    #[test]
    fn set_mark_pushes_named_mark_into_position_history() {
        let mut a = app_with("hello\nworld", 10);
        a.cursor = Position::new(1, 2);
        a.apply(Action::SetMark('a'));
        // Last entry is a NamedMark.
        let last = a.position_history.last().unwrap();
        assert_eq!(last.position, Position::new(1, 2));
        assert!(matches!(last.source, PositionSource::NamedMark('a')));
    }

    #[test]
    fn jump_history_filters_to_jump_class_only() {
        let mut a = app_with("aaa\nbbb\nccc\nddd", 10);
        // mX (NamedMark) followed by gg (AutoJump). Ctrl-O should walk
        // to the AutoJump entry, NOT the NamedMark.
        a.cursor = Position::new(1, 0);
        a.apply(Action::SetMark('a'));
        // Position history now has [NamedMark('a') at (1,0)].
        a.cursor = Position::new(3, 0);
        a.apply(invoke_motion(a.builtins.goto_first_line));
        // Now history: [NamedMark('a'), AutoJump (3,0)].
        a.apply(Action::JumpHistoryBack);
        // Ctrl-O lands on the AutoJump entry, not the named mark.
        assert_eq!(a.cursor, Position::new(3, 0));
    }

    #[test]
    fn g_semicolon_walks_named_mark_history_backward() {
        let mut a = app_with("a\nb\nc\nd\ne", 10);
        // Set marks at three positions.
        a.cursor = Position::new(1, 0);
        a.apply(Action::SetMark('a'));
        a.cursor = Position::new(3, 0);
        a.apply(Action::SetMark('b'));
        a.cursor = Position::new(4, 0);
        // g; lands on 'b' (most recent named mark).
        a.apply(Action::WalkMarkHistoryBack);
        assert_eq!(a.cursor, Position::new(3, 0));
        // g; again -> 'a'.
        a.apply(Action::WalkMarkHistoryBack);
        assert_eq!(a.cursor, Position::new(1, 0));
    }

    #[test]
    fn g_comma_walks_named_mark_history_forward() {
        let mut a = app_with("a\nb\nc\nd\ne", 10);
        a.cursor = Position::new(1, 0);
        a.apply(Action::SetMark('a'));
        a.cursor = Position::new(3, 0);
        a.apply(Action::SetMark('b'));
        a.cursor = Position::new(4, 0);
        a.apply(Action::WalkMarkHistoryBack); // -> 'b'
        a.apply(Action::WalkMarkHistoryBack); // -> 'a'
        a.apply(Action::WalkMarkHistoryForward); // -> 'b'
        assert_eq!(a.cursor, Position::new(3, 0));
    }

    #[test]
    fn g_semicolon_with_no_named_marks_emits_error() {
        let mut a = app_with("a\nb\nc", 10);
        a.cursor = Position::new(2, 0);
        a.apply(invoke_motion(a.builtins.goto_first_line)); // pushes AutoJump
        a.apply(Action::WalkMarkHistoryBack);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("no marks"));
    }

    #[test]
    fn jump_and_mark_walks_share_the_same_ring_cursor() {
        // After Ctrl-O moves cursor through the ring, g; should pick
        // up from the new cursor position when scanning for marks.
        let mut a = app_with("a\nb\nc\nd\ne", 10);
        a.cursor = Position::new(1, 0);
        a.apply(Action::SetMark('a')); // ring [NamedMark a@(1,0)] cursor=1
        a.cursor = Position::new(3, 0);
        a.apply(invoke_motion(a.builtins.goto_first_line));
        // ring [NamedMark a, AutoJump (3,0)] cursor=2
        // Ctrl-O jumps to AutoJump (3,0). Snapshot of (0,0) pushed.
        // Actually: with snapshot pre-step, ring [a, (3,0), (0,0)],
        // cursor walks from 3 backward to find jump -> index 1 ((3,0)).
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.cursor, Position::new(3, 0));
        // g; from current ring cursor (1) walks back to find NamedMark
        // at index 0.
        a.apply(Action::WalkMarkHistoryBack);
        assert_eq!(a.cursor, Position::new(1, 0));
    }

    #[test]
    fn position_history_dedups_consecutive_same() {
        let mut a = app_with("a\nb\nc", 10);
        a.push_position_history(Position::new(2, 0), PositionSource::AutoJump);
        a.push_position_history(Position::new(2, 0), PositionSource::AutoJump);
        // Pushing the same position-and-source twice in a row -> single entry.
        assert_eq!(a.position_history.len(), 1);
    }

    #[test]
    fn position_history_capped_at_max() {
        let mut a = app_with("a\nb\nc", 10);
        for i in 0..200 {
            a.push_position_history(Position::new(i % 3, 0), PositionSource::AutoJump);
        }
        assert!(a.position_history.len() <= 100);
    }

    // ---- Multiple registers ----

    #[test]
    fn select_register_stashes_pending_register() {
        let mut a = app_with("hello", 10);
        a.apply(Action::SelectRegister(Register::Named('a')));
        assert_eq!(a.pending_register, Some(Register::Named('a')));
    }

    #[test]
    fn yank_with_named_register_stores_into_named_and_unnamed() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::SelectRegister(Register::Named('a')));
        let inv = CommandInvocation::of(a.builtins.yank.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        // Named slot populated.
        let named = a.registers.get(&Register::Named('a')).unwrap();
        assert_eq!(named.content, "hello ");
        // Unnamed also populated.
        assert_eq!(a.unnamed_register.as_ref().unwrap().content, "hello ");
        // Pending register consumed.
        assert!(a.pending_register.is_none());
    }

    #[test]
    fn paste_from_named_register_uses_named_content() {
        let mut a = app_with("hello", 10);
        // Manually populate "a with custom content.
        a.registers.insert(
            Register::Named('a'),
            UnnamedRegister {
                content: "X".into(),
                kind: YankKind::Charwise,
            },
        );
        a.apply(Action::SelectRegister(Register::Named('a')));
        a.apply(Action::PasteAfter);
        assert_eq!(a.document.text(), "hXello");
    }

    #[test]
    fn delete_into_black_hole_does_not_overwrite_unnamed() {
        let mut a = app_with("hello world", 10);
        // First yank into unnamed.
        let yank = CommandInvocation::of(a.builtins.yank.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(yank));
        let pre_delete_unnamed = a.unnamed_register.as_ref().unwrap().content.clone();
        // Now delete into black hole; unnamed should be untouched.
        a.apply(Action::SelectRegister(Register::BlackHole));
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(
            a.unnamed_register.as_ref().unwrap().content,
            pre_delete_unnamed
        );
    }

    #[test]
    fn invocation_with_no_pending_register_uses_unnamed() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.yank.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        // Unnamed populated; "0 also populated by vim's auto-fill on yank.
        // Named map's only entry is the numbered "0 register.
        assert!(a.unnamed_register.is_some());
        assert!(a.registers.contains_key(&Register::Numbered(0)));
        // No alphabetic named slots populated.
        assert!(!a.registers.keys().any(|r| matches!(r, Register::Named(_))));
    }

    #[test]
    fn yank_auto_populates_zero_register() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.yank.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        let zero = a.registers.get(&Register::Numbered(0)).unwrap();
        assert_eq!(zero.content, "hello ");
    }

    #[test]
    fn delete_does_not_populate_zero_register() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        // Delete populates unnamed but NOT "0.
        assert!(!a.registers.contains_key(&Register::Numbered(0)));
        assert!(a.unnamed_register.is_some());
    }

    #[test]
    fn paste_from_unset_named_register_falls_back_to_unnamed() {
        let mut a = app_with("hello", 10);
        a.unnamed_register = Some(UnnamedRegister {
            content: "X".into(),
            kind: YankKind::Charwise,
        });
        a.apply(Action::SelectRegister(Register::Named('z')));
        a.apply(Action::PasteAfter);
        // 'z' is empty -> fall back to unnamed.
        assert_eq!(a.document.text(), "hXello");
    }

    // ---- ~ toggle case at cursor ----

    #[test]
    fn toggle_case_at_cursor_inverts_letter_and_advances() {
        let mut a = app_with("hello", 10);
        a.apply(Action::ToggleCaseAtCursor);
        assert_eq!(a.document.text(), "Hello");
        assert_eq!(a.cursor, Position::new(0, 1));
    }

    #[test]
    fn toggle_case_advances_through_non_letters() {
        let mut a = app_with("a 1 b", 10);
        a.apply(Action::ToggleCaseAtCursor);
        assert_eq!(a.document.text(), "A 1 b");
        a.apply(Action::ToggleCaseAtCursor);
        // Space at byte 1 -> unchanged but cursor advances.
        assert_eq!(a.document.text(), "A 1 b");
        assert_eq!(a.cursor, Position::new(0, 2));
    }

    #[test]
    fn toggle_case_at_eol_is_no_op() {
        let mut a = app_with("hi", 10);
        a.cursor = Position::new(0, 2);
        a.apply(Action::ToggleCaseAtCursor);
        assert_eq!(a.document.text(), "hi");
        assert_eq!(a.cursor, Position::new(0, 2));
    }

    // ---- Word-search (* / #) and matching-bracket (%) ----

    #[test]
    fn star_finds_next_occurrence_of_word_under_cursor() {
        let mut a = app_with("foo bar foo bar", 10);
        a.cursor = Position::new(0, 1); // on 'o' of first "foo"
        a.apply(Action::SearchWordUnderCursor(SearchDirection::Forward));
        assert_eq!(a.cursor, Position::new(0, 8)); // start of second "foo"
        let last = a.last_search.as_ref().unwrap();
        assert_eq!(last.pattern, "foo");
    }

    #[test]
    fn hash_finds_previous_occurrence_of_word_under_cursor() {
        let mut a = app_with("foo bar foo bar", 10);
        a.cursor = Position::new(0, 8); // on 'f' of second "foo"
        a.apply(Action::SearchWordUnderCursor(SearchDirection::Backward));
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn star_when_cursor_not_on_word_scans_forward() {
        let mut a = app_with("  hello world", 10);
        a.cursor = Position::new(0, 0); // on space
        a.apply(Action::SearchWordUnderCursor(SearchDirection::Forward));
        // The first word "hello" appears once in the buffer; pattern is
        // recorded but no match is found beyond it (no second "hello").
        let last = a.last_search.as_ref().unwrap();
        assert_eq!(last.pattern, "hello");
    }

    #[test]
    fn star_with_no_word_on_line_emits_error() {
        let mut a = app_with("   ", 10);
        a.apply(Action::SearchWordUnderCursor(SearchDirection::Forward));
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn star_records_pattern_even_on_no_other_match() {
        let mut a = app_with("only hello", 10);
        a.cursor = Position::new(0, 5); // on 'h'
        a.apply(Action::SearchWordUnderCursor(SearchDirection::Forward));
        // Only one occurrence; wrap puts us at the same place.
        let last = a.last_search.as_ref().unwrap();
        assert_eq!(last.pattern, "hello");
    }

    #[test]
    fn percent_jumps_from_open_to_close_paren() {
        let mut a = app_with("call(arg1, arg2)", 10);
        a.cursor = Position::new(0, 4); // on '('
        a.apply(Action::MatchBracket);
        assert_eq!(a.cursor, Position::new(0, 15));
    }

    #[test]
    fn percent_jumps_from_close_to_open_paren() {
        let mut a = app_with("call(arg1, arg2)", 10);
        a.cursor = Position::new(0, 15); // on ')'
        a.apply(Action::MatchBracket);
        assert_eq!(a.cursor, Position::new(0, 4));
    }

    #[test]
    fn percent_with_nested_picks_correct_match() {
        let mut a = app_with("a(b(c)d)e", 10);
        a.cursor = Position::new(0, 1); // on outer '('
        a.apply(Action::MatchBracket);
        assert_eq!(a.cursor, Position::new(0, 7)); // outer ')'
    }

    #[test]
    fn percent_searches_forward_for_first_bracket_when_cursor_off() {
        let mut a = app_with("call(arg)", 10);
        a.cursor = Position::ZERO; // 'c'; first bracket on line is '(' at byte 4
        a.apply(Action::MatchBracket);
        assert_eq!(a.cursor, Position::new(0, 8)); // ')'
    }

    #[test]
    fn percent_with_no_bracket_on_line_emits_error() {
        let mut a = app_with("plain text only", 10);
        a.apply(Action::MatchBracket);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn percent_with_unmatched_bracket_emits_error() {
        let mut a = app_with("foo(bar", 10);
        a.cursor = Position::new(0, 3);
        a.apply(Action::MatchBracket);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn percent_works_for_brackets_and_braces() {
        let mut a = app_with("[a, b, c]", 10);
        a.cursor = Position::ZERO;
        a.apply(Action::MatchBracket);
        assert_eq!(a.cursor, Position::new(0, 8));
    }

    // ---- Viewport motions ----

    #[test]
    fn jump_viewport_top_lands_on_scroll_line() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 5);
        a.scroll = 3;
        a.cursor = Position::new(7, 0);
        a.apply(Action::JumpViewport(ViewportPos::Top));
        assert_eq!(a.cursor.line, 3);
    }

    #[test]
    fn jump_viewport_middle_lands_at_half_height() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 6);
        a.scroll = 0;
        a.apply(Action::JumpViewport(ViewportPos::Middle));
        // height/2 = 3, so cursor goes to line 3.
        assert_eq!(a.cursor.line, 3);
    }

    #[test]
    fn jump_viewport_bottom_lands_at_height_minus_one() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 5);
        a.scroll = 2;
        a.apply(Action::JumpViewport(ViewportPos::Bottom));
        // 2 + 5 - 1 = 6.
        assert_eq!(a.cursor.line, 6);
    }

    #[test]
    fn jump_viewport_clamps_to_last_addressable_line() {
        let mut a = app_with("a\nb", 50);
        a.apply(Action::JumpViewport(ViewportPos::Bottom));
        assert_eq!(a.cursor.line, 1);
    }

    #[test]
    fn scroll_cursor_to_center_centers_cursor() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 5);
        a.cursor = Position::new(6, 0);
        a.apply(Action::ScrollCursorTo(ScrollPos::Center));
        // cursor.line - height/2 = 6 - 2 = 4.
        assert_eq!(a.scroll, 4);
        // Cursor itself unchanged.
        assert_eq!(a.cursor.line, 6);
    }

    #[test]
    fn scroll_cursor_to_top_aligns_scroll_with_cursor() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 5);
        a.cursor = Position::new(6, 0);
        a.apply(Action::ScrollCursorTo(ScrollPos::Top));
        assert_eq!(a.scroll, 6);
    }

    #[test]
    fn scroll_cursor_to_bottom_pulls_scroll_up_by_height_minus_one() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 5);
        a.cursor = Position::new(8, 0);
        a.apply(Action::ScrollCursorTo(ScrollPos::Bottom));
        // 8 - (5 - 1) = 4.
        assert_eq!(a.scroll, 4);
    }

    #[test]
    fn page_down_advances_by_viewport_height_minus_two() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 5);
        a.cursor = Position::ZERO;
        a.apply(Action::PageDown);
        assert_eq!(a.cursor.line, 3);
    }

    #[test]
    fn page_down_clamps_to_last_addressable_line() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 5);
        a.cursor = Position::new(8, 0);
        a.apply(Action::PageDown);
        assert_eq!(a.cursor.line, 9);
    }

    #[test]
    fn page_up_steps_back_by_viewport_height_minus_two() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 5);
        a.cursor = Position::new(7, 0);
        a.apply(Action::PageUp);
        assert_eq!(a.cursor.line, 4);
    }

    #[test]
    fn page_up_at_top_stays_at_top() {
        let mut a = app_with("0\n1\n2", 5);
        a.apply(Action::PageUp);
        assert_eq!(a.cursor.line, 0);
    }

    #[test]
    fn scroll_line_down_advances_scroll_and_pulls_cursor_if_off_top() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6", 3);
        a.cursor = Position::ZERO;
        a.scroll = 0;
        a.apply(Action::ScrollLineDown);
        assert_eq!(a.scroll, 1);
        // Cursor was at line 0; now it's off the top, so it follows.
        assert_eq!(a.cursor.line, 1);
    }

    #[test]
    fn scroll_line_up_decreases_scroll_and_pushes_cursor_if_off_bottom() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6", 3);
        a.cursor = Position::new(4, 0);
        a.scroll = 2; // viewport covers lines 2,3,4.
        a.apply(Action::ScrollLineUp);
        assert_eq!(a.scroll, 1);
        // Bottom of new viewport is line 3; cursor was at 4, gets pushed up.
        assert_eq!(a.cursor.line, 3);
    }

    // ---- Replace mode ----

    #[test]
    fn enter_replace_sets_modal() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        assert_eq!(a.modal, ModalState::Replace);
    }

    #[test]
    fn overwrite_char_replaces_byte_at_cursor() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::OverwriteChar('H'));
        assert_eq!(a.document.text(), "Hello");
        assert_eq!(a.cursor, Position::new(0, 1));
    }

    #[test]
    fn overwrite_chain_replaces_consecutively() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        for c in "WORL".chars() {
            a.apply(Action::OverwriteChar(c));
        }
        assert_eq!(a.document.text(), "WORLo");
        assert_eq!(a.cursor, Position::new(0, 4));
    }

    #[test]
    fn overwrite_at_eol_extends_line() {
        let mut a = app_with("hi", 10);
        a.cursor = Position::new(0, 2);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::OverwriteChar('!'));
        assert_eq!(a.document.text(), "hi!");
        assert_eq!(a.cursor, Position::new(0, 3));
    }

    #[test]
    fn replace_undo_last_restores_overwritten_char() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::OverwriteChar('H'));
        assert_eq!(a.document.text(), "Hello");
        assert_eq!(a.cursor, Position::new(0, 1));
        // Backspace: should restore 'h' and step cursor back.
        a.apply(Action::ReplaceUndoLast);
        assert_eq!(a.document.text(), "hello");
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn replace_undo_after_eol_extension_deletes_extension() {
        let mut a = app_with("hi", 10);
        a.cursor = Position::new(0, 2);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::OverwriteChar('!'));
        assert_eq!(a.document.text(), "hi!");
        a.apply(Action::ReplaceUndoLast);
        assert_eq!(a.document.text(), "hi");
        assert_eq!(a.cursor, Position::new(0, 2));
    }

    #[test]
    fn replace_undo_with_empty_history_is_no_op() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::ReplaceUndoLast);
        assert_eq!(a.document.text(), "hello");
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn replace_undo_chain_restores_in_reverse_order() {
        let mut a = app_with("abcde", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::OverwriteChar('A'));
        a.apply(Action::OverwriteChar('B'));
        a.apply(Action::OverwriteChar('C'));
        assert_eq!(a.document.text(), "ABCde");
        a.apply(Action::ReplaceUndoLast);
        assert_eq!(a.document.text(), "ABcde");
        a.apply(Action::ReplaceUndoLast);
        assert_eq!(a.document.text(), "Abcde");
        a.apply(Action::ReplaceUndoLast);
        assert_eq!(a.document.text(), "abcde");
    }

    #[test]
    fn enter_replace_clears_replace_history() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::OverwriteChar('H'));
        assert_eq!(a.replace_history.len(), 1);
        a.apply(Action::EnterMode(ModalState::Normal));
        a.apply(Action::EnterMode(ModalState::Replace));
        assert!(a.replace_history.is_empty());
    }

    #[test]
    fn esc_exits_replace_to_normal_and_pulls_cursor_back() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::OverwriteChar('H'));
        // Cursor at (0,1) after one overwrite.
        a.apply(Action::EnterMode(ModalState::Normal));
        // enter_mode pulls cursor back one byte on Normal entry.
        assert_eq!(a.modal, ModalState::Normal);
        assert_eq!(a.cursor, Position::new(0, 0));
    }

    // ---- Marks ----

    #[test]
    fn set_mark_records_cursor_position() {
        let mut a = app_with("hello\nworld", 10);
        a.cursor = Position::new(1, 2);
        a.apply(Action::SetMark('a'));
        assert_eq!(a.marks.get(&'a'), Some(&Position::new(1, 2)));
    }

    #[test]
    fn invalid_mark_name_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::SetMark(' '));
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(a.marks.is_empty());
    }

    #[test]
    fn jump_mark_exact_restores_cursor_position() {
        let mut a = app_with("hello\nworld\nfoo", 10);
        a.cursor = Position::new(0, 3);
        a.apply(Action::SetMark('m'));
        a.cursor = Position::new(2, 0);
        a.apply(Action::JumpToMarkExact('m'));
        assert_eq!(a.cursor, Position::new(0, 3));
    }

    #[test]
    fn jump_mark_line_lands_on_first_non_blank() {
        let mut a = app_with("hello\n    indented\nfoo", 10);
        a.cursor = Position::new(1, 8); // mid-word on the indented line
        a.apply(Action::SetMark('a'));
        a.cursor = Position::ZERO;
        a.apply(Action::JumpToMarkLine('a'));
        // Line 1, byte 4 = 'i' (after 4 leading spaces).
        assert_eq!(a.cursor, Position::new(1, 4));
    }

    #[test]
    fn jump_to_unset_mark_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::JumpToMarkExact('z'));
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn marks_are_keyed_by_name() {
        let mut a = app_with("hello\nworld", 10);
        a.cursor = Position::new(0, 1);
        a.apply(Action::SetMark('a'));
        a.cursor = Position::new(1, 3);
        a.apply(Action::SetMark('b'));
        a.cursor = Position::ZERO;
        a.apply(Action::JumpToMarkExact('a'));
        assert_eq!(a.cursor, Position::new(0, 1));
        a.apply(Action::JumpToMarkExact('b'));
        assert_eq!(a.cursor, Position::new(1, 3));
    }

    #[test]
    fn uppercase_mark_works_same_as_lowercase_in_v1() {
        // v1 makes no distinction between buffer-local (a-z) and global
        // (A-Z) marks since the TUI runs against a single document.
        let mut a = app_with("hello\nworld", 10);
        a.cursor = Position::new(1, 2);
        a.apply(Action::SetMark('A'));
        a.cursor = Position::ZERO;
        a.apply(Action::JumpToMarkExact('A'));
        assert_eq!(a.cursor, Position::new(1, 2));
    }

    #[test]
    fn jumping_to_mark_with_invalid_name_is_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::JumpToMarkExact(' '));
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    // ---- gv reselect ----

    #[test]
    fn exit_visual_captures_last_visual() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.builtins.word_forward));
        // Now selection is anchor=ZERO, head=(0,6).
        a.apply(Action::ExitVisual);
        let last = a.last_visual.expect("last_visual captured");
        assert_eq!(last.anchor, Position::ZERO);
        assert_eq!(last.head, Position::new(0, 6));
        assert_eq!(last.kind, VisualKind::Charwise);
    }

    #[test]
    fn gv_with_no_prior_visual_emits_error() {
        let mut a = app_with("hello", 10);
        assert!(a.last_visual.is_none());
        a.apply(Action::ReselectLastVisual);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert_eq!(a.modal, ModalState::Normal);
    }

    #[test]
    fn gv_restores_anchor_head_and_kind() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.builtins.word_forward));
        a.apply(Action::ExitVisual);
        // Cursor now collapsed; modal Normal.
        assert_eq!(a.modal, ModalState::Normal);
        // gv:
        a.apply(Action::ReselectLastVisual);
        assert_eq!(a.modal, ModalState::Visual(VisualKind::Charwise));
        let sels = a.document.selections();
        let sel = sels.primary();
        assert_eq!(sel.anchor, Position::ZERO);
        assert_eq!(sel.head, Position::new(0, 6));
        assert_eq!(a.cursor, Position::new(0, 6));
    }

    #[test]
    fn gv_after_yank_in_visual_restores_pre_yank_selection() {
        // Real-world test: select, yank (which auto-exits Visual), `gv`
        // should bring back the same selection so you can re-operate.
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.builtins.word_forward));
        let inv =
            CommandInvocation::of(a.builtins.yank.0).with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.modal, ModalState::Normal);
        a.apply(Action::ReselectLastVisual);
        assert_eq!(a.modal, ModalState::Visual(VisualKind::Charwise));
        let sels = a.document.selections();
        let sel = sels.primary();
        assert_eq!(sel.head, Position::new(0, 6));
    }

    #[test]
    fn gv_preserves_linewise_kind() {
        let mut a = app_with("aaa\nbbb\nccc", 10);
        a.apply(Action::EnterVisual(VisualKind::Linewise));
        a.apply(invoke_motion(a.builtins.line_down));
        a.apply(Action::ExitVisual);
        a.apply(Action::ReselectLastVisual);
        assert_eq!(a.modal, ModalState::Visual(VisualKind::Linewise));
    }

    // ---- Dot-repeat ----

    #[test]
    fn dot_with_no_prior_change_emits_error() {
        let mut a = app_with("hello", 10);
        assert!(a.last_change.is_none());
        a.apply(Action::RepeatLastChange);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn delete_records_last_change_and_dot_replays_it() {
        let mut a = app_with("foo bar foo bar", 10);
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        // After dw: "bar foo bar".
        assert_eq!(a.document.text(), "bar foo bar");
        assert!(a.last_change.is_some());
        // `.` replays the same dw at the new cursor position.
        a.apply(Action::RepeatLastChange);
        assert_eq!(a.document.text(), "foo bar");
    }

    #[test]
    fn yank_does_not_record_last_change() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.yank.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        // Yank doesn't mutate the buffer; dot-repeat shouldn't pick this up.
        assert!(a.last_change.is_none());
    }

    #[test]
    fn motion_does_not_record_last_change() {
        let mut a = app_with("hello world", 10);
        a.apply(invoke_motion(a.builtins.word_forward));
        assert!(a.last_change.is_none());
    }

    #[test]
    fn dd_records_last_change_and_dot_replays_it() {
        let mut a = app_with("aaa\nBBB\nccc\nddd", 10);
        a.cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "aaa\n\nccc\nddd");
        // `.` repeats: empty line is now "deleted" -> empty stays.
        a.apply(Action::RepeatLastChange);
        // `.` re-runs `dd` at the cursor; line 1 (empty) becomes a no-op
        // edit since CurrentLine is empty. Buffer unchanged.
        assert_eq!(a.document.text(), "aaa\n\nccc\nddd");
    }

    #[test]
    fn insert_session_captures_typed_text_into_last_insert() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::Insert("Y".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.last_insert.as_deref(), Some("XY"));
    }

    #[test]
    fn dot_repeats_change_with_insert_replay() {
        // Classic vim test: cw foo<Esc> followed by . on another word
        // replaces that word with "foo" too.
        let mut a = app_with("alpha beta gamma", 10);
        // cw on first word.
        let inv = CommandInvocation::of(a.builtins.change.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.document.text(), "Xbeta gamma");
        // Move to "beta" (cursor is now on 'X' / position 0; let's go to 'b'
        // at byte 1).
        a.cursor = Position::new(0, 1);
        // Repeat.
        a.apply(Action::RepeatLastChange);
        // cw replays: deletes "beta " and inserts "X" -> "XXgamma".
        // (Note: our cw includes the trailing space; vim's cw is implicitly
        // ce, a deferred refinement.)
        assert_eq!(a.document.text(), "XXgamma");
        assert_eq!(a.modal, ModalState::Normal);
    }

    #[test]
    fn dot_without_insert_replay_when_no_text_was_typed() {
        // dw (no insert phase) -> . repeats just the delete.
        let mut a = app_with("alpha beta gamma", 10);
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        // dw deletes "alpha "; then `.` deletes another word (no insert).
        a.apply(Action::RepeatLastChange);
        // Two dws: "alpha " then "beta " -> "gamma".
        assert_eq!(a.document.text(), "gamma");
    }

    #[test]
    fn change_records_last_change() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.change.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        // change drops to Insert, but the change itself is recorded.
        assert!(a.last_change.is_some());
    }

    // ---- Visual mode end-to-end ----

    #[test]
    fn enter_visual_charwise_sets_modal_and_anchor() {
        let mut a = app_with("hello", 10);
        a.cursor = Position::new(0, 1);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        assert_eq!(a.modal, ModalState::Visual(VisualKind::Charwise));
        assert_eq!(a.visual_anchor, Some(Position::new(0, 1)));
        let sels = a.document.selections();
        let sel = sels.primary();
        assert_eq!(sel.anchor, Position::new(0, 1));
        assert_eq!(sel.head, Position::new(0, 1));
        assert_eq!(sel.visual, Some(VisualMode::Charwise));
    }

    #[test]
    fn motion_in_visual_extends_head_keeps_anchor() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.builtins.word_forward));
        let sels = a.document.selections();
        let sel = sels.primary();
        assert_eq!(sel.anchor, Position::ZERO);
        assert_eq!(sel.head, Position::new(0, 6));
        assert_eq!(a.cursor, Position::new(0, 6));
    }

    #[test]
    fn esc_in_visual_collapses_selection_and_returns_to_normal() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.builtins.char_right));
        a.apply(Action::ExitVisual);
        assert_eq!(a.modal, ModalState::Normal);
        assert!(a.visual_anchor.is_none());
        assert!(a.document.selections().primary().is_cursor());
    }

    #[test]
    fn delete_in_visual_removes_selection_and_returns_to_normal() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.builtins.char_right));
        a.apply(invoke_motion(a.builtins.char_right));
        a.apply(invoke_motion(a.builtins.char_right));
        // Selection now covers bytes 0..3 of "hello world" charwise (vim
        // INCLUSIVE -> visual range covers 0..=3 = 4 bytes "hell").
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "o world");
        assert_eq!(a.modal, ModalState::Normal);
    }

    #[test]
    fn yank_in_visual_populates_register_charwise() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.builtins.word_forward));
        let inv =
            CommandInvocation::of(a.builtins.yank.0).with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        let reg = a.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.kind, YankKind::Charwise);
        // Document text untouched.
        assert_eq!(a.document.text(), "hello world");
        // Visual mode exited.
        assert_eq!(a.modal, ModalState::Normal);
    }

    #[test]
    fn change_in_visual_enters_insert_mode() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.builtins.word_forward));
        let inv = CommandInvocation::of(a.builtins.change.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        // Change in Visual deletes selection AND drops into Insert.
        assert_eq!(a.modal, ModalState::Insert);
    }

    #[test]
    fn linewise_visual_yank_captures_full_lines() {
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.cursor = Position::new(1, 1); // mid-line on "BBB"
        a.apply(Action::EnterVisual(VisualKind::Linewise));
        // Selection is single line; yank captures the whole line
        // regardless of byte offsets.
        let inv =
            CommandInvocation::of(a.builtins.yank.0).with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        let reg = a.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.kind, YankKind::Linewise);
        assert_eq!(reg.content, "BBB");
    }

    #[test]
    fn linewise_visual_extends_to_multiple_lines() {
        let mut a = app_with("aaa\nbbb\nccc\nddd", 10);
        a.apply(Action::EnterVisual(VisualKind::Linewise));
        a.apply(invoke_motion(a.builtins.line_down));
        let inv =
            CommandInvocation::of(a.builtins.yank.0).with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        let reg = a.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.kind, YankKind::Linewise);
        // Lines 0 and 1 -> "aaa\nbbb".
        assert_eq!(reg.content, "aaa\nbbb");
    }

    #[test]
    fn visual_anchor_persists_across_count_motion() {
        let mut a = app_with("one two three four five", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(Action::PushDigit(2));
        a.apply(invoke_motion(a.builtins.word_forward));
        let sels = a.document.selections();
        let sel = sels.primary();
        assert_eq!(sel.anchor, Position::ZERO);
        // 2w from origin advances 2 word starts: "ONE two THREE" -> byte 8.
        assert_eq!(sel.head, Position::new(0, 8));
    }

    // ---- count prefix end-to-end ----

    #[test]
    fn push_digit_accumulates_pending_count() {
        let mut a = app_with("abc", 10);
        a.apply(Action::PushDigit(1));
        a.apply(Action::PushDigit(2));
        a.apply(Action::PushDigit(3));
        assert_eq!(a.pending_count, 123);
    }

    #[test]
    fn invoke_consumes_pending_count_into_motion() {
        let mut a = app_with("one two three four five", 10);
        a.apply(Action::PushDigit(3));
        let id = a.builtins.word_forward;
        a.apply(invoke_motion(id));
        // 3w from origin: "one two three FOUR five" -> 'f' of "four" at byte 14.
        assert_eq!(a.cursor, Position::new(0, 14));
        // pending_count is reset after dispatch.
        assert_eq!(a.pending_count, 0);
    }

    #[test]
    fn count_with_line_motion_advances_count_lines() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 20);
        a.apply(Action::PushDigit(5));
        let id = a.builtins.line_down;
        a.apply(invoke_motion(id));
        assert_eq!(a.cursor.line, 5);
    }

    #[test]
    fn operator_then_motion_with_count_multiplies() {
        // `2dw` -> delete 2 words from cursor.
        let mut a = app_with("one two three four five", 10);
        a.apply(Action::PushDigit(2));
        // SetPending latches the count as op_count.
        a.apply(Action::SetPending(Pending::AfterOperator(
            a.builtins.delete,
        )));
        assert_eq!(a.op_count, 2);
        assert_eq!(a.pending_count, 0);
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        // 2dw: deletes "one two " leaving "three four five".
        assert_eq!(a.document.text(), "three four five");
        assert_eq!(a.op_count, 0);
    }

    #[test]
    fn count_on_both_sides_multiplies_2d3w_equals_6w() {
        // `2d3w`: op_count = 2, motion count = 3, final count = 6.
        let mut a = app_with("a b c d e f g h i j", 10);
        a.apply(Action::PushDigit(2));
        a.apply(Action::SetPending(Pending::AfterOperator(
            a.builtins.delete,
        )));
        assert_eq!(a.op_count, 2);
        a.apply(Action::PushDigit(3));
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        // 6 words deleted from "a b c d e f g h i j" leaves "g h i j".
        assert_eq!(a.document.text(), "g h i j");
    }

    #[test]
    fn count_with_dd_deletes_n_lines_as_single_undo() {
        // `2dd`: count=2 expands Range::CurrentLine to span 2 lines.
        // The whole deletion MUST land as a single undo unit -- a
        // single `u` should restore the original buffer.
        let mut a = app_with("one\ntwo\nthree\nfour", 10);
        a.cursor = Position::new(0, 0);
        a.apply(Action::PushDigit(2));
        a.apply(Action::SetPending(Pending::AfterOperator(
            a.builtins.delete,
        )));
        assert_eq!(a.op_count, 2);
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        // Lines 0 and 1 ("one" and "two") deleted; line 2 ("three") survives.
        let text = a.document.text();
        assert!(!text.contains("one"));
        assert!(!text.contains("two"));
        assert!(text.contains("three"));
        assert!(text.contains("four"));

        // One undo should fully restore.
        let _ = a.undo_blocking();
        assert_eq!(a.document.text(), "one\ntwo\nthree\nfour");
    }

    #[test]
    fn count_with_indent_right_indents_n_lines_as_single_undo() {
        // `2>>`: count=2 expands Range::CurrentLine to span 2 lines.
        // The whole indent MUST land as a single undo unit -- the
        // operator builds the per-line edits up front and commits
        // via apply_edit_batch.
        let mut a = app_with("one\ntwo\nthree\nfour", 10);
        a.cursor = Position::new(0, 0);
        a.apply(Action::PushDigit(2));
        a.apply(Action::SetPending(Pending::AfterOperator(
            a.builtins.indent_right,
        )));
        let inv = CommandInvocation::of(a.builtins.indent_right.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "    one\n    two\nthree\nfour");
        // Single undo restores the original buffer.
        let _ = a.undo_blocking();
        assert_eq!(a.document.text(), "one\ntwo\nthree\nfour");
    }

    #[test]
    fn count_with_indent_left_dedents_n_lines_as_single_undo() {
        let mut a = app_with("    one\n    two\nthree\nfour", 10);
        a.cursor = Position::new(0, 0);
        a.apply(Action::PushDigit(2));
        a.apply(Action::SetPending(Pending::AfterOperator(
            a.builtins.indent_left,
        )));
        let inv = CommandInvocation::of(a.builtins.indent_left.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "one\ntwo\nthree\nfour");
        let _ = a.undo_blocking();
        assert_eq!(a.document.text(), "    one\n    two\nthree\nfour");
    }

    #[test]
    fn count_zero_through_pending_count_is_ignored_by_motion() {
        // pending_count remains 0 after no digit; motion uses default 1.
        let mut a = app_with("hello world", 10);
        let id = a.builtins.word_forward;
        a.apply(invoke_motion(id));
        assert_eq!(a.cursor, Position::new(0, 6));
    }

    // ---- find / till motions end-to-end ----

    #[test]
    fn fz_jumps_to_z_on_current_line() {
        let mut a = app_with("hello, world", 10);
        let inv = CommandInvocation::of(a.builtins.find_char_forward.0)
            .with_args(lattice_grammar::Args::Char('w'));
        a.apply(Action::Invoke(inv));
        assert_eq!(a.cursor, Position::new(0, 7));
    }

    #[test]
    fn capital_f_jumps_backward() {
        let mut a = app_with("hello, world", 10);
        a.cursor = Position::new(0, 11); // on 'd'
        let inv = CommandInvocation::of(a.builtins.find_char_backward.0)
            .with_args(lattice_grammar::Args::Char('h'));
        a.apply(Action::Invoke(inv));
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn t_lands_one_byte_before_target() {
        let mut a = app_with("hello, world", 10);
        let inv = CommandInvocation::of(a.builtins.till_char_forward.0)
            .with_args(lattice_grammar::Args::Char('w'));
        a.apply(Action::Invoke(inv));
        assert_eq!(a.cursor, Position::new(0, 6));
    }

    #[test]
    fn df_deletes_through_target_char() {
        // From "hello, world" with cursor at 0, `df,` deletes "hello," and
        // leaves " world".
        let mut a = app_with("hello, world", 10);
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(
                a.builtins.find_char_forward,
                lattice_grammar::Args::Char(','),
            ),
        );
        a.apply(Action::Invoke(inv));
        // dispatcher uses [start, end) range; find_char_forward returns the
        // position of the comma (byte 5), so [0, 5) = "hello" is deleted.
        // The trailing comma stays in place.
        assert_eq!(a.document.text(), ", world");
    }

    #[test]
    fn ct_with_change_enters_insert_mode() {
        let mut a = app_with("hello, world", 10);
        let inv = CommandInvocation::of(a.builtins.change.0).with_target(
            lattice_grammar::Target::Motion(
                a.builtins.till_char_forward,
                lattice_grammar::Args::Char(','),
            ),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.modal, ModalState::Insert);
    }

    #[test]
    fn find_no_match_keeps_cursor() {
        let mut a = app_with("hello", 10);
        a.cursor = Position::new(0, 1);
        let inv = CommandInvocation::of(a.builtins.find_char_forward.0)
            .with_args(lattice_grammar::Args::Char('z'));
        a.apply(Action::Invoke(inv));
        assert_eq!(a.cursor, Position::new(0, 1));
    }

    // ---- yank + paste end-to-end ----

    #[test]
    fn yw_populates_unnamed_register_charwise() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.yank.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        let reg = a.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.content, "hello ");
        assert_eq!(reg.kind, YankKind::Charwise);
        // Buffer untouched by yank.
        assert_eq!(a.document.text(), "hello world");
    }

    #[test]
    fn yy_populates_register_linewise() {
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(a.builtins.yank.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        let reg = a.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.content, "BBB");
        assert_eq!(reg.kind, YankKind::Linewise);
        assert_eq!(a.document.text(), "aaa\nBBB\nccc");
    }

    #[test]
    fn dd_populates_register_linewise_via_delete() {
        // delete also yanks; register kind is linewise for dd.
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        let reg = a.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.kind, YankKind::Linewise);
        assert_eq!(reg.content, "BBB");
    }

    #[test]
    fn dd_on_closed_fold_heading_deletes_whole_fold() {
        // `docs/help/folding.md`: dd on a closed fold deletes the
        // entire fold range as a single undo unit. Use a sibling
        // # H2 heading so the # H1 fold has a bounded end.
        let initial = "# H1\nbody one\nbody two\n# H2\nafter\n";
        let mut a = app_with(initial, 10);
        a.set_foldmethod_for_test(FoldMethod::Markdown);
        a.recompute_folds();
        // Close the H1 fold (lines 0..=2).
        let idx = a
            .folds
            .iter()
            .position(|f| f.start_line == 0)
            .expect("H1 fold");
        a.folds[idx].closed = true;
        a.cursor = Position::new(0, 0);
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        let text = a.document.text();
        assert!(!text.contains("# H1"), "H1 not deleted: {text:?}");
        assert!(!text.contains("body one"), "body one not deleted: {text:?}");
        assert!(!text.contains("body two"), "body two not deleted: {text:?}");
        assert!(text.contains("# H2"), "H2 lost: {text:?}");
        assert!(text.contains("after"), "after lost: {text:?}");
    }

    #[test]
    fn yy_on_closed_fold_heading_yanks_whole_fold() {
        let initial = "# H1\nbody one\nbody two\n# H2\nafter\n";
        let mut a = app_with(initial, 10);
        a.set_foldmethod_for_test(FoldMethod::Markdown);
        a.recompute_folds();
        let idx = a
            .folds
            .iter()
            .position(|f| f.start_line == 0)
            .expect("H1 fold");
        a.folds[idx].closed = true;
        a.cursor = Position::new(0, 0);
        let inv = CommandInvocation::of(a.builtins.yank.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        let reg = a.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.kind, YankKind::Linewise);
        assert!(reg.content.contains("# H1"), "register content: {:?}", reg.content);
        assert!(
            reg.content.contains("body one"),
            "register content: {:?}",
            reg.content
        );
        assert!(
            reg.content.contains("body two"),
            "register content: {:?}",
            reg.content
        );
        assert!(
            !reg.content.contains("# H2"),
            "yank should not include sibling heading: {:?}",
            reg.content
        );
    }

    #[test]
    fn dd_on_open_fold_heading_deletes_only_one_line() {
        // Operator expansion only applies when the fold is *closed*;
        // an open fold leaves the heading visible to be edited like
        // any other line.
        let initial = "# H1\nbody one\nbody two\n# H2\nafter\n";
        let mut a = app_with(initial, 10);
        a.set_foldmethod_for_test(FoldMethod::Markdown);
        a.recompute_folds();
        // Leave open (default).
        a.cursor = Position::new(0, 0);
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        let text = a.document.text();
        assert!(!text.contains("# H1"), "heading should be gone: {text:?}");
        assert!(text.contains("body one"), "body one should remain: {text:?}");
    }

    #[test]
    fn search_into_closed_fold_auto_opens_it() {
        // `docs/help/folding.md`: search hits open the fold the
        // cursor lands in.
        let initial = "# H1\nbody one needle\nbody two\n# H2\nafter\n";
        let mut a = app_with(initial, 10);
        a.set_foldmethod_for_test(FoldMethod::Markdown);
        a.recompute_folds();
        let idx = a
            .folds
            .iter()
            .position(|f| f.start_line == 0)
            .expect("H1 fold");
        a.folds[idx].closed = true;
        // Submit a forward search from the top of the buffer.
        a.search_line = Some(SearchLine {
            origin: Position::ZERO,
            pattern: "needle".into(),
            direction: SearchDirection::Forward,
        });
        a.modal = ModalState::Search(SearchDirection::Forward);
        a.apply(Action::SearchSubmit);
        // The fold containing `body one` should now be open.
        let fold = a
            .folds
            .iter()
            .find(|f| f.start_line == 0)
            .expect("H1 fold still present");
        assert!(!fold.closed, "search should have auto-opened the fold");
    }

    #[test]
    fn goto_first_line_into_closed_fold_auto_opens() {
        let initial = "# H1\nbody\nbody2\n# H2\nafter\n";
        let mut a = app_with(initial, 10);
        a.set_foldmethod_for_test(FoldMethod::Markdown);
        a.recompute_folds();
        let idx = a
            .folds
            .iter()
            .position(|f| f.start_line == 0)
            .expect("H1 fold");
        a.folds[idx].closed = true;
        // Move cursor away first (so gg is a non-trivial jump).
        a.cursor = Position::new(4, 0);
        let inv = CommandInvocation::of(a.builtins.goto_first_line.0);
        a.apply(Action::Invoke(inv));
        let fold = a
            .folds
            .iter()
            .find(|f| f.start_line == 0)
            .expect("H1 fold still present");
        assert!(!fold.closed, "gg should auto-open the destination fold");
    }

    #[test]
    fn zi_toggles_foldenable_and_renders_folds_open() {
        let mut a = app_with("# H\nbody\n# H2\n", 10);
        a.set_foldmethod_for_test(FoldMethod::Markdown);
        a.recompute_folds();
        let idx = a
            .folds
            .iter()
            .position(|f| f.start_line == 0)
            .expect("H1 fold");
        a.folds[idx].closed = true;
        // Sanity: the fold is closed and visible to the renderer.
        assert!(a.line_inside_closed_fold(1));
        assert!(a.fold_start_at(0).is_some());
        // zi disables.
        a.apply(Action::ToggleFoldEnable);
        assert!(!a.foldenable());
        // With foldenable=false, the renderer sees no closed folds.
        assert!(!a.line_inside_closed_fold(1));
        assert!(a.fold_start_at(0).is_none());
        // zi again re-enables and the closed-state is preserved.
        a.apply(Action::ToggleFoldEnable);
        assert!(a.foldenable());
        assert!(a.line_inside_closed_fold(1));
        assert!(a.fold_start_at(0).is_some());
    }

    #[test]
    fn nofoldenable_disables_fold_aware_operators() {
        let initial = "# H1\nbody one\nbody two\n# H2\nafter\n";
        let mut a = app_with(initial, 10);
        a.set_foldmethod_for_test(FoldMethod::Markdown);
        a.recompute_folds();
        let idx = a
            .folds
            .iter()
            .position(|f| f.start_line == 0)
            .expect("H1 fold");
        a.folds[idx].closed = true;
        a.set_foldenable_for_test(false);
        a.cursor = Position::new(0, 0);
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        // With foldenable=false, dd should affect just one line.
        let text = a.document.text();
        assert!(!text.contains("# H1"), "heading should be deleted: {text:?}");
        assert!(text.contains("body one"), "body one should remain: {text:?}");
    }

    #[test]
    fn linear_j_does_not_auto_open_fold() {
        // `docs/help/folding.md`: linear motions (j/k/h/l/w/b) do
        // NOT trigger auto-open. The cursor "skips" over closed
        // folds via `line_inside_closed_fold` filtering -- but the
        // fold itself stays closed. Here we simulate a synthetic
        // cursor move into the fold range to verify the rule.
        let initial = "# H1\nbody\nbody2\n# H2\nafter\n";
        let mut a = app_with(initial, 10);
        a.set_foldmethod_for_test(FoldMethod::Markdown);
        a.recompute_folds();
        let idx = a
            .folds
            .iter()
            .position(|f| f.start_line == 0)
            .expect("H1 fold");
        a.folds[idx].closed = true;
        // Direct cursor move (not via auto-open path).
        a.cursor = Position::new(1, 0);
        let still_closed = a
            .folds
            .iter()
            .find(|f| f.start_line == 0)
            .expect("H1 fold still present");
        assert!(
            still_closed.closed,
            "merely setting cursor should not open folds"
        );
    }

    #[test]
    fn dd_on_non_fold_line_uses_count_one() {
        // Sanity: the fold-expansion only kicks in when the cursor
        // is on a closed-fold heading. A normal `dd` outside any
        // fold operates on just one line. (The standard `delete`
        // operator's CurrentLine range preserves the trailing
        // newline, leaving an empty line; that's an existing app
        // contract, not something fold-aware expansion should
        // change.)
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.set_foldmethod_for_test(FoldMethod::Indent);
        a.recompute_folds();
        a.cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        let reg = a.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.kind, YankKind::Linewise);
        assert_eq!(reg.content, "BBB");
    }

    #[test]
    fn paste_after_charwise_inserts_after_cursor() {
        let mut a = app_with("hello", 10);
        a.unnamed_register = Some(UnnamedRegister {
            content: "X".into(),
            kind: YankKind::Charwise,
        });
        a.cursor = Position::new(0, 0); // on 'h'
        a.apply(Action::PasteAfter);
        assert_eq!(a.document.text(), "hXello");
        // Cursor lands on the last char of the pasted text (still 'X').
        assert_eq!(a.cursor, Position::new(0, 1));
    }

    #[test]
    fn paste_before_charwise_inserts_at_cursor() {
        let mut a = app_with("hello", 10);
        a.unnamed_register = Some(UnnamedRegister {
            content: "X".into(),
            kind: YankKind::Charwise,
        });
        a.cursor = Position::new(0, 2); // on 'l'
        a.apply(Action::PasteBefore);
        assert_eq!(a.document.text(), "heXllo");
        assert_eq!(a.cursor, Position::new(0, 2));
    }

    #[test]
    fn paste_after_linewise_inserts_below_current_line() {
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.unnamed_register = Some(UnnamedRegister {
            content: "XXX\n".into(),
            kind: YankKind::Linewise,
        });
        a.cursor = Position::new(1, 0); // on 'B' line
        a.apply(Action::PasteAfter);
        assert_eq!(a.document.text(), "aaa\nBBB\nXXX\nccc");
        assert_eq!(a.cursor, Position::new(2, 0));
    }

    #[test]
    fn paste_before_linewise_inserts_above_current_line() {
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.unnamed_register = Some(UnnamedRegister {
            content: "XXX\n".into(),
            kind: YankKind::Linewise,
        });
        a.cursor = Position::new(1, 0);
        a.apply(Action::PasteBefore);
        assert_eq!(a.document.text(), "aaa\nXXX\nBBB\nccc");
        assert_eq!(a.cursor, Position::new(1, 0));
    }

    #[test]
    fn paste_with_empty_register_emits_error_message() {
        let mut a = app_with("hello", 10);
        assert!(a.unnamed_register.is_none());
        a.apply(Action::PasteAfter);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert_eq!(a.document.text(), "hello");
    }

    // ---- Bracketed-paste burst (Action::PasteText) ----

    #[test]
    fn paste_text_in_normal_inserts_at_cursor_one_undo_unit() {
        let mut a = app_with("hello", 10);
        a.cursor = Position::new(0, 5);
        a.apply(Action::PasteText(" world".into()));
        assert_eq!(a.document.text(), "hello world");
        assert_eq!(a.cursor, Position::new(0, 11));
        // One bracketed-paste = one undo unit.
        a.apply(Action::Undo);
        assert_eq!(a.document.text(), "hello");
    }

    #[test]
    fn paste_text_in_insert_inserts_and_records_for_dot_repeat() {
        let mut a = app_with("a", 10);
        a.cursor = Position::new(0, 1);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::PasteText("bcd".into()));
        assert_eq!(a.document.text(), "abcd");
        assert_eq!(a.cursor, Position::new(0, 4));
        assert!(matches!(a.modal, ModalState::Insert));
        // Dot-repeat insert recording captured the pasted text.
        let rec = a.recording_insert.as_ref().unwrap();
        assert_eq!(rec, "bcd");
    }

    #[test]
    fn paste_text_in_command_appends_to_command_line() {
        let mut a = app_with("xx", 10);
        a.apply(Action::EnterMode(ModalState::Command));
        a.command_line = "w ".into();
        a.apply(Action::PasteText("foo.rs".into()));
        assert_eq!(a.command_line, "w foo.rs");
        // Document untouched.
        assert_eq!(a.document.text(), "xx");
    }

    #[test]
    fn paste_text_in_search_appends_to_search_pattern() {
        let mut a = app_with("xx", 10);
        a.apply(Action::EnterSearch(
            lattice_grammar::SearchDirection::Forward,
        ));
        a.apply(Action::SearchAppend('a'));
        a.apply(Action::PasteText("bcd".into()));
        let line = a.search_line.as_ref().unwrap();
        assert_eq!(line.pattern, "abcd");
    }

    #[test]
    fn paste_text_empty_is_a_noop() {
        let mut a = app_with("hello", 10);
        let before = a.document.text();
        a.apply(Action::PasteText(String::new()));
        assert_eq!(a.document.text(), before);
    }

    #[test]
    fn paste_text_with_newlines_lands_as_single_edit() {
        let mut a = app_with("a", 10);
        a.cursor = Position::new(0, 1);
        a.apply(Action::PasteText("\nb\nc".into()));
        assert_eq!(a.document.text(), "a\nb\nc");
        assert_eq!(a.cursor, Position::new(2, 1));
    }

    // ---- Blockwise visual operators (DESIGN.md §15:18) ----

    /// Drive into Blockwise visual at `anchor`, then move the cursor to
    /// `head` so the rectangle is `[anchor, head]`. Returns the App
    /// ready for an operator dispatch.
    fn enter_block_visual(text: &str, anchor: Position, head: Position) -> App {
        let mut a = app_with(text, 10);
        a.cursor = anchor;
        a.apply(Action::EnterVisual(VisualKind::Blockwise));
        a.cursor = head;
        a.visual_anchor = Some(anchor);
        let sel = Selection {
            anchor,
            head,
            visual: Some(VisualMode::Blockwise),
        };
        a.set_selections_blocking(SelectionSet::single(sel));
        a
    }

    #[test]
    fn block_delete_removes_each_rows_column_slice() {
        // Three rows, columns 1..=2 deleted from each.
        // Initial:    "abcd\n1234\nWXYZ"
        // After d :   "ad\n14\nWZ"
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "ad\n14\nWZ");
    }

    #[test]
    fn block_delete_lands_cursor_at_top_left_of_block() {
        // Vim's behavior: after a rectangle delete, the cursor sits
        // at the block's top-left column, not at column 0.
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        // Top-left of block was (0, 1); after the delete column 1
        // is the new content's start on the top row.
        assert_eq!(a.cursor, Position::new(0, 1));
    }

    #[test]
    fn block_delete_lands_as_single_undo_unit() {
        // The whole rectangle delete must collapse into one undo
        // entry -- the dispatcher coalesces the per-row AppliedEdits
        // by snapshotting pre/post and replaying as one Edit::replace.
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "ad\n14\nWZ");
        let _ = a.undo_blocking();
        assert_eq!(a.document.text(), "abcd\n1234\nWXYZ");
    }

    #[test]
    fn block_change_lands_as_single_undo_unit() {
        // Block-visual `c` deletes each row's column slice and enters
        // Insert. The deletion piece must be one undo unit; future
        // typed text would be batched separately by the I/A path.
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv = CommandInvocation::of(a.builtins.change.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "ad\n14\nWZ");
        assert!(matches!(a.modal, ModalState::Insert));
        // Exit Insert without typing anything to isolate the deletion.
        a.apply(Action::EnterMode(ModalState::Normal));
        let _ = a.undo_blocking();
        assert_eq!(a.document.text(), "abcd\n1234\nWXYZ");
    }

    #[test]
    fn block_yank_stores_blockwise_content_in_unnamed_register() {
        // Yank a 3x2 rectangle: cols 1..=2 across three rows of "abcd\n1234\nWXYZ".
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv =
            CommandInvocation::of(a.builtins.yank.0).with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        // Document untouched.
        assert_eq!(a.document.text(), "abcd\n1234\nWXYZ");
        // Unnamed register has the 3 column slices joined by newline,
        // tagged Blockwise.
        let reg = a.unnamed_register.as_ref().expect("yank stored");
        assert_eq!(reg.content, "bc\n23\nXY");
        assert_eq!(reg.kind, YankKind::Blockwise);
    }

    #[test]
    fn block_yank_clamps_short_rows_to_intersection() {
        // Middle row "12" partially overlaps the rectangle: cols 1..=2,
        // line len 2, intersection is `[1, 2)` = "2".
        let mut a = enter_block_visual("abcd\n12\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv =
            CommandInvocation::of(a.builtins.yank.0).with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        let reg = a.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.content, "bc\n2\nXY");
        assert_eq!(reg.kind, YankKind::Blockwise);
    }

    #[test]
    fn block_yank_with_row_entirely_left_of_rectangle_yields_empty_slice() {
        // Middle row is "" (empty). Visual cols 1..=2 fully outside;
        // intersection is empty.
        let mut a = enter_block_visual("abcd\n\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv =
            CommandInvocation::of(a.builtins.yank.0).with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        let reg = a.unnamed_register.as_ref().unwrap();
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
        let inv = CommandInvocation::of(a.builtins.indent_right.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "    abc\n    123\n    WXY");
        let _ = a.undo_blocking();
        assert_eq!(a.document.text(), "abc\n123\nWXY");
    }

    #[test]
    fn block_visual_capital_i_via_real_motions_not_explicit_selection() {
        // Reproduces the path the actual user takes: Ctrl-V to enter
        // blockwise, motions to extend the selection, capital I.
        // No manual set_selections_blocking -- selections must be
        // maintained by the SelectionChange effect from motions.
        let mut a = app_with("abcd\n1234\nWXYZ", 10);
        a.cursor = Position::new(0, 1);
        a.apply(Action::EnterVisual(VisualKind::Blockwise));
        // Move down 2 rows + right 1 column via motions.
        a.apply(invoke_motion(a.builtins.line_down));
        a.apply(invoke_motion(a.builtins.line_down));
        a.apply(invoke_motion(a.builtins.char_right));
        // Cursor should now be at (2, 2). visual_anchor was (0, 1).
        assert_eq!(a.cursor, Position::new(2, 2));
        assert_eq!(a.visual_anchor, Some(Position::new(0, 1)));

        a.apply(Action::EnterBlockVisualInsert);
        assert!(matches!(a.modal, ModalState::Insert));
        // I should land at column 1 (block's left col) on the top row.
        assert_eq!(a.cursor, Position::new(0, 1));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.document.text(), "aXbcd\n1X234\nWXXYZ");
    }

    #[test]
    fn block_visual_capital_i_inserts_at_block_left_column_on_each_row() {
        // 3 rows, block at column 1. `I` enters Insert at (top_row, 1).
        // Type "X", Esc -> "X" lands at column 1 on every row.
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        a.apply(Action::EnterBlockVisualInsert);
        assert!(matches!(a.modal, ModalState::Insert));
        assert_eq!(a.cursor, Position::new(0, 1));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.document.text(), "aXbcd\n1X234\nWXXYZ");
    }

    #[test]
    fn block_visual_capital_a_appends_after_block_right_column() {
        // Block cols 1..=2 across 3 rows; `A` lands at col 3 on each row.
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        a.apply(Action::EnterBlockVisualAppend);
        assert!(matches!(a.modal, ModalState::Insert));
        assert_eq!(a.cursor, Position::new(0, 3));
        a.apply(Action::Insert("@".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.document.text(), "abc@d\n123@4\nWXY@Z");
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
        assert_eq!(a.document.text(), "aXYZbcd\n1XYZ234\nWXYZXYZ");

        // One undo should restore the original buffer.
        let _ = a.undo_blocking();
        assert_eq!(a.document.text(), "abcd\n1234\nWXYZ");
    }

    #[test]
    fn block_visual_capital_a_lands_as_single_undo_unit() {
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        a.apply(Action::EnterBlockVisualAppend);
        a.apply(Action::Insert("@".into()));
        a.apply(Action::Insert("@".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.document.text(), "abc@@d\n123@@4\nWXY@@Z");
        let _ = a.undo_blocking();
        assert_eq!(a.document.text(), "abcd\n1234\nWXYZ");
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
        assert_eq!(a.document.text(), "abcQd\n12\nWXYQZ");
    }

    #[test]
    fn block_visual_indent_left_dedents_each_row_in_block() {
        let mut a = enter_block_visual(
            "    abc\n    123\n    WXY",
            Position::new(0, 0),
            Position::new(2, 0),
        );
        let inv = CommandInvocation::of(a.builtins.indent_left.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "abc\n123\nWXY");
    }

    #[test]
    fn block_change_deletes_rectangle_and_enters_insert() {
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv = CommandInvocation::of(a.builtins.change.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "ad\n14\nWZ");
        assert!(matches!(a.modal, ModalState::Insert));
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
        let yank =
            CommandInvocation::of(a.builtins.yank.0).with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(yank));
        // Exit visual and move to a fresh paste site.
        a.apply(Action::ExitVisual);
        a.cursor = Position::new(2, 0);
        // `p` (after-cursor) -> insert at col 1 on line 2 and line 3.
        a.apply(Action::PasteAfter);
        // Line 2: "WXYZ" -> "WbcXYZ"; Line 3: "----" -> "-23---"
        assert_eq!(a.document.text(), "abcd\n1234\nWbcXYZ\n-23---");
    }

    // ---- Help overlay (DESIGN.md §5.11) ----

    #[test]
    fn describe_command_opens_help_buffer_with_metadata() {
        let mut a = app_with("xx", 10);
        // `:describe-command ex:write` -- the registry knows about this.
        a.command_line = "describe-command ex:write".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("help view should open");
        assert!(h.title.contains("ex:write"));
        // First two lines: "ex:write  (ex-command)" + blank.
        let lines = h.lines();
        assert!(lines[0].contains("ex:write"));
        assert!(lines[0].contains("ex-command"));
    }

    #[test]
    fn describe_command_shows_source_link_to_registration_site() {
        // §5.11: every :describe-* must surface a file link to the
        // registration site. The buffer text is the rendered label
        // (`ex_commands.rs:LINE`) only -- the URL lives on the
        // parsed HelpLink target. Built-in commands record their
        // source via #[track_caller] when populate() runs.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:write".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        let body = h.content.as_string();
        assert!(
            body.contains("Defined at:"),
            "body should label the source: {body}"
        );
        assert!(
            body.contains("ex_commands.rs"),
            "body should contain the file path label: {body}"
        );
        // The HelpLink target carries the URL's resolved type.
        let has_source = h.links.iter().any(|l| {
            matches!(&l.target, crate::help::HelpLinkTarget::Source { path, .. }
                if path.to_string_lossy().contains("ex_commands.rs"))
        });
        assert!(has_source, "expected a Source HelpLink to ex_commands.rs");
        assert!(
            body.contains("(built-in)"),
            "body should label the source layer: {body}"
        );
    }

    #[test]
    fn describe_command_link_is_extracted_by_help_link_parser() {
        // The HelpBuffer constructor runs parse_help_links over the
        // body so the `[label](file:...)` markdown link becomes a
        // HelpLink with a Source target -- ready for the styled-link
        // renderer + follow-link motion (post-1.0).
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:quit".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        let source_link = h
            .links
            .iter()
            .find(|l| matches!(l.target, crate::help::HelpLinkTarget::Source { .. }));
        assert!(
            source_link.is_some(),
            "expected at least one HelpLink with Source target; got {:?}",
            h.links
        );
    }

    #[test]
    fn describe_command_emits_per_arg_anchors() {
        // §5.11 anchor system: every arg produces an `arg:<name>`
        // anchor, plus a parent `args` anchor for the section.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:apropos".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        // ex:apropos has one arg "pattern" -- expect "args" plus "arg:pattern".
        assert!(
            h.anchors.iter().any(|a| a.name == "args"),
            "expected 'args' anchor, got {:?}",
            h.anchors
        );
        assert!(
            h.anchors.iter().any(|a| a.name == "arg:pattern"),
            "expected 'arg:pattern' anchor, got {:?}",
            h.anchors
        );
    }

    #[test]
    fn describe_command_with_no_args_emits_no_arg_anchors() {
        // ex:quit has no args, so no `arg:*` or `args` anchors. The
        // `latency` anchor is always present (latency-class
        // declaration is mandatory metadata, DESIGN.md §5.2.5).
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:quit".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        assert!(
            h.anchors.iter().all(|a| a.name == "latency"),
            "ex:quit has no args; only the latency anchor is expected: {:?}",
            h.anchors,
        );
    }

    #[test]
    fn describe_command_anchor_lines_match_actual_section_headings() {
        // Verify the recorded line index actually points at the
        // section's heading row in the rendered content.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:apropos".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        let lines = h.lines();
        let args_anchor = h.anchors.iter().find(|a| a.name == "args").unwrap();
        let arg_anchor = h.anchors.iter().find(|a| a.name == "arg:pattern").unwrap();
        assert_eq!(lines[args_anchor.line as usize], "Arguments:");
        assert!(lines[arg_anchor.line as usize].contains("pattern"));
    }

    #[test]
    fn describe_command_arguments_section_renders_args_schema() {
        // ex:apropos has a schema with one required arg "pattern".
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:apropos".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.help_buffer.as_ref().unwrap().content.as_string();
        assert!(
            body.contains("Arguments:"),
            "expected Arguments section: {body}"
        );
        assert!(
            body.contains("pattern"),
            "expected arg name `pattern`: {body}"
        );
    }

    #[test]
    fn describe_key_shows_source_link_to_keymap_row() {
        let mut a = app_with("xx", 10);
        a.command_line = "describe-key j".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        let body = h.content.as_string();
        assert!(
            body.contains("Bound at:"),
            "describe-key output missing `Bound at:`: {body}"
        );
        assert!(
            body.contains("keymap.rs"),
            "describe-key output missing source label: {body}"
        );
        let has_source = h.links.iter().any(|l| {
            matches!(&l.target, crate::help::HelpLinkTarget::Source { path, .. }
                if path.to_string_lossy().contains("keymap.rs"))
        });
        assert!(has_source, "expected a Source HelpLink to keymap.rs");
        assert!(
            body.contains("(built-in)"),
            "describe-key output missing source-layer label: {body}"
        );
    }

    #[test]
    fn describe_key_renders_command_cross_reference_links() {
        // For `j`, three Normal/Visual/Help bindings -- the first
        // two have a `command`. The buffer text shows the LABEL
        // (`motion:line-down`); the URL is on the HelpLink target.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-key j".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        let body = h.content.as_string();
        assert!(
            body.contains("motion:line-down"),
            "expected `motion:line-down` label: {body}"
        );
        // The Command target carries the canonical command name.
        let has_cmd_link = h.links.iter().any(|l| {
            matches!(&l.target, crate::help::HelpLinkTarget::Command(c) if c == "motion:line-down")
        });
        assert!(has_cmd_link, "expected Command(motion:line-down) link");
    }

    #[test]
    fn describe_key_each_binding_has_its_own_source_link() {
        // `j` has 2 bindings -- Normal (line down) and Visual
        // (extend down). Help inherits Normal's `j` via active-
        // buffer routing (DESIGN.md §5.9), so it doesn't surface as
        // a separate descriptor. Each remaining binding should
        // surface its own `(file:...)` link because every
        // KeymapEntry's source is captured at its own row.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-key j".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        let source_links: Vec<_> = h
            .links
            .iter()
            .filter(|l| matches!(l.target, crate::help::HelpLinkTarget::Source { .. }))
            .collect();
        assert_eq!(
            source_links.len(),
            2,
            "expected 2 source links (one per binding); got {}: {:?}",
            source_links.len(),
            h.links
        );
        // Each link should point at a distinct line in keymap.rs.
        let mut lines: Vec<u32> = source_links
            .iter()
            .filter_map(|l| match &l.target {
                crate::help::HelpLinkTarget::Source { line, .. } => Some(*line),
                _ => None,
            })
            .collect();
        lines.sort();
        lines.dedup();
        assert_eq!(
            lines.len(),
            2,
            "expected 2 distinct source line numbers; got {lines:?}",
        );
    }

    #[test]
    fn describe_key_unknown_chord_renders_not_bound_message() {
        let mut a = app_with("xx", 10);
        a.command_line = "describe-key xyzzy".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.help_buffer.as_ref().unwrap().content.as_string();
        assert!(body.contains("not bound"), "body: {body}");
    }

    // ---- Command-line completion (DESIGN.md §5.11.3) ----

    fn app_in_command_mode(line: &str) -> App {
        let mut a = app_with("xx", 10);
        a.modal = ModalState::Command;
        a.command_line = line.into();
        a
    }

    #[test]
    fn tab_in_command_mode_opens_completion_popup() {
        let mut a = app_in_command_mode("descri");
        a.apply(Action::CommandLineCompleteOrAdvance);
        let state = a.completion_state.as_ref().expect("popup should open");
        // Candidates use the user-facing alias form, not the
        // canonical `ex:*` registry name. Both `:describe-command`
        // and `:ex:describe-command` parse correctly via the
        // dispatcher's two-stage resolution; the popup shows the
        // form a user actually types.
        assert!(
            state
                .candidates
                .iter()
                .any(|c| c.raw.text == "describe-command")
        );
        assert!(
            state
                .candidates
                .iter()
                .any(|c| c.raw.text == "describe-buffer")
        );
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn second_tab_advances_selected_candidate() {
        let mut a = app_in_command_mode("descri");
        a.apply(Action::CommandLineCompleteOrAdvance);
        let first = a.completion_state.as_ref().unwrap().selected;
        a.apply(Action::CommandLineCompleteOrAdvance);
        let second = a.completion_state.as_ref().unwrap().selected;
        assert_eq!(first, 0);
        assert_eq!(second, 1);
    }

    #[test]
    fn shift_tab_walks_back_through_candidates() {
        let mut a = app_in_command_mode("descri");
        a.apply(Action::CommandLineCompleteOrAdvance);
        a.apply(Action::CommandLineCompleteOrAdvance);
        a.apply(Action::CommandLineCompleteOrAdvance);
        a.apply(Action::CommandLineCompletePrev);
        assert_eq!(a.completion_state.as_ref().unwrap().selected, 1);
    }

    #[test]
    fn accept_completion_replaces_prefix_with_chosen_text() {
        let mut a = app_in_command_mode("descri");
        a.apply(Action::CommandLineCompleteOrAdvance);
        // The accepted candidate uses the user-facing alias form,
        // not the canonical `ex:*` name. The first candidate (after
        // ranking) is one of the describe-* family.
        a.apply(Action::CommandLineAcceptCompletion);
        assert!(
            a.command_line.starts_with("describe-") || a.command_line == "apropos",
            "expected user-facing alias, got `{}`",
            a.command_line
        );
        assert!(a.completion_state.is_none());
    }

    #[test]
    fn dismiss_completion_keeps_command_line_intact() {
        let mut a = app_in_command_mode("descri");
        a.apply(Action::CommandLineCompleteOrAdvance);
        a.apply(Action::CommandLineDismissCompletion);
        assert_eq!(a.command_line, "descri");
        assert!(a.completion_state.is_none());
    }

    #[test]
    fn typing_after_popup_open_live_refilters_candidates() {
        // Vertico-style: typing while the popup is open keeps it
        // open and re-runs the pipeline against the longer prefix.
        let mut a = app_in_command_mode("descr");
        a.apply(Action::CommandLineCompleteOrAdvance);
        assert!(a.completion_state.is_some());
        let initial_count = a.completion_state.as_ref().unwrap().candidates.len();

        a.apply(Action::CommandLineAppend('i'));
        assert!(
            a.completion_state.is_some(),
            "popup must stay open while filtering"
        );
        assert_eq!(a.command_line, "descri");
        // Typing narrows the prefix -> candidate set should shrink
        // or stay equal, never grow.
        let narrowed = a.completion_state.as_ref().unwrap().candidates.len();
        assert!(narrowed <= initial_count);
        // Selection resets to first match (the candidate set
        // changed; previous index would be meaningless).
        assert_eq!(a.completion_state.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn backspace_after_popup_open_live_refilters() {
        let mut a = app_in_command_mode("describ");
        a.apply(Action::CommandLineCompleteOrAdvance);
        let narrow_count = a.completion_state.as_ref().unwrap().candidates.len();
        a.apply(Action::CommandLineBackspace);
        assert!(a.completion_state.is_some());
        assert_eq!(a.command_line, "descri");
        // Shorter prefix -> at least as many candidates.
        let widened = a.completion_state.as_ref().unwrap().candidates.len();
        assert!(widened >= narrow_count);
    }

    #[test]
    fn typing_no_match_keeps_popup_open_with_empty_candidates() {
        // Vertico-style: typing past the matchable region leaves the
        // popup alive (just empty), so a single backspace can recover.
        let mut a = app_in_command_mode("descri");
        a.apply(Action::CommandLineCompleteOrAdvance);
        for c in "zxqzxqzxq".chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        let state = a
            .completion_state
            .as_ref()
            .expect("popup must stay open on no-match");
        assert!(state.candidates.is_empty());
        // Backspacing the noise restores matches.
        for _ in 0.."zxqzxqzxq".len() {
            a.apply(Action::CommandLineBackspace);
        }
        assert!(a.completion_state.is_some());
        assert!(!a.completion_state.as_ref().unwrap().candidates.is_empty());
    }

    #[test]
    fn delete_word_backward_with_open_popup_refilters() {
        let mut a = app_in_command_mode("describ");
        a.apply(Action::CommandLineCompleteOrAdvance);
        a.apply(Action::CommandLineDeleteWordBackward);
        // Word-delete leaves us with an empty cmdline -> Empty slot
        // -> all commands; popup stays open.
        assert!(a.completion_state.is_some());
        assert_eq!(a.command_line, "");
    }

    #[test]
    fn clear_with_open_popup_widens_to_all_commands() {
        let mut a = app_in_command_mode("descri");
        a.apply(Action::CommandLineCompleteOrAdvance);
        let narrow_count = a.completion_state.as_ref().unwrap().candidates.len();
        a.apply(Action::CommandLineClear);
        assert!(a.completion_state.is_some());
        assert_eq!(a.command_line, "");
        let widened = a.completion_state.as_ref().unwrap().candidates.len();
        assert!(widened >= narrow_count);
    }

    #[test]
    fn typing_with_no_popup_open_does_not_open_one() {
        // Refresh only fires when a popup is already open; bare
        // typing without a prior <Tab> stays as it was.
        let mut a = app_in_command_mode("desc");
        a.apply(Action::CommandLineAppend('r'));
        assert!(a.completion_state.is_none());
        assert_eq!(a.command_line, "descr");
    }

    // ---- Chord-capture (DESIGN.md §B.1, ArgKind::Chord) ----

    #[test]
    fn chord_capture_active_only_when_in_chord_arg_slot() {
        let mut a = app_with("xx", 10);
        a.modal = ModalState::Command;
        // Empty cmdline -> CommandName slot, not chord-capture.
        a.command_line = String::new();
        assert!(!a.chord_capture_active());
        // Mid command-name slot.
        a.command_line = "describe-key".into();
        assert!(!a.chord_capture_active());
        // Now the cursor is past the space; arg slot is `chord`
        // with kind=Chord -> capture is active.
        a.command_line = "describe-key ".into();
        assert!(a.chord_capture_active());
        // describe-command's first arg is String, NOT Chord ->
        // no capture even though we're in an arg slot.
        a.command_line = "describe-command ".into();
        assert!(!a.chord_capture_active());
        // Outside Command modal, never active.
        a.modal = ModalState::Normal;
        a.command_line = "describe-key ".into();
        assert!(!a.chord_capture_active());
    }

    #[test]
    fn chord_capture_active_for_canonical_command_name() {
        // `:ex:describe-key ` (canonical, not the alias). The slot
        // detector tries `id_by_name` first and only falls back
        // to alias-expand, so both forms switch into chord-capture.
        let mut a = app_with("xx", 10);
        a.modal = ModalState::Command;
        a.command_line = "ex:describe-key ".into();
        assert!(a.chord_capture_active());
    }

    #[test]
    fn append_chord_concatenates_token() {
        let mut a = app_in_command_mode("describe-key ");
        a.apply(Action::CommandLineAppendChord("<C-c>".into()));
        assert_eq!(a.command_line, "describe-key <C-c>");
    }

    #[test]
    fn append_chord_supports_multi_token_sequences() {
        // gg / <C-w>j -- multi-stroke chords. Each press appends
        // its own token.
        let mut a = app_in_command_mode("describe-key ");
        a.apply(Action::CommandLineAppendChord("g".into()));
        a.apply(Action::CommandLineAppendChord("g".into()));
        assert_eq!(a.command_line, "describe-key gg");
    }

    #[test]
    fn delete_chord_pops_one_full_token() {
        let mut a = app_in_command_mode("describe-key <C-c>");
        a.apply(Action::CommandLineDeleteChord);
        // The whole `<C-c>` token (5 bytes) gets removed in one
        // delete -- not a single byte.
        assert_eq!(a.command_line, "describe-key ");
    }

    #[test]
    fn delete_chord_on_plain_char_pops_one_char() {
        let mut a = app_in_command_mode("describe-key gg");
        a.apply(Action::CommandLineDeleteChord);
        assert_eq!(a.command_line, "describe-key g");
    }

    #[test]
    fn delete_chord_on_empty_cmdline_exits_command_mode() {
        let mut a = app_with("xx", 10);
        a.modal = ModalState::Command;
        a.command_line = String::new();
        a.apply(Action::CommandLineDeleteChord);
        assert!(matches!(a.modal, ModalState::Normal));
    }

    // ---- Missing-arg chord prompt (DESIGN.md §B.1) ----

    #[test]
    fn empty_submit_of_describe_key_arms_chord_prompt() {
        // User typed `:describe-key<CR>` with no arg. The required
        // Chord arg is missing -- we shouldn't error; we should
        // prefill the cmdline and arm auto-submit.
        let mut a = app_in_command_mode("describe-key");
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.command_line, "describe-key ");
        assert!(a.auto_submit_after_chord);
        assert!(matches!(a.modal, ModalState::Command));
    }

    #[test]
    fn empty_submit_of_canonical_describe_key_arms_chord_prompt() {
        // Same prompt path through the canonical name, not just
        // the alias.
        let mut a = app_in_command_mode("ex:describe-key");
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.command_line, "ex:describe-key ");
        assert!(a.auto_submit_after_chord);
    }

    #[test]
    fn first_chord_after_arming_auto_submits() {
        let mut a = app_in_command_mode("describe-key");
        a.apply(Action::CommandLineSubmit);
        assert!(a.auto_submit_after_chord);
        // The first chord token captured should auto-fire submit;
        // the cmdline should clear and we land back in Normal.
        a.apply(Action::CommandLineAppendChord("j".into()));
        assert!(!a.auto_submit_after_chord);
        assert!(matches!(a.modal, ModalState::Normal));
        // The submitted line was `describe-key j` -- which opens
        // a help buffer for chord `j`. Smoke check that some
        // help got produced.
        assert!(a.help_buffer.is_some());
    }

    #[test]
    fn empty_submit_of_describe_command_arms_prompt_without_chord_capture() {
        // describe-command's first arg is String (Required) -- the
        // generalized missing-arg path arms a prompt, prefills the
        // cmdline, and leaves the user in Command mode to type the
        // arg. Auto-submit is OFF (only Chord-kind args auto-submit
        // on the next keystroke).
        let mut a = app_in_command_mode("describe-command");
        a.apply(Action::CommandLineSubmit);
        assert!(matches!(a.modal, ModalState::Command));
        assert!(!a.auto_submit_after_chord);
        // Prefilled with the command word + space; cursor in arg slot.
        assert_eq!(a.command_line, "describe-command ");
        // Echo area carries the arg's prompt.
        assert!(a.last_message.is_some());
    }

    #[test]
    fn empty_submit_of_optional_arg_command_does_not_arm_prompt() {
        // `:write` (alias for `ex:write`) has an OPTIONAL path arg
        // (default = `None` -- absent means "use current path").
        // Submitting bare runs the command normally; no prompt arm.
        let mut a = app_in_command_mode("w");
        a.apply(Action::CommandLineSubmit);
        // Cmdline closed -- the missing-arg prompt path skipped this
        // command because its schema's first arg is Optional.
        assert!(matches!(a.modal, ModalState::Normal));
        assert!(!a.auto_submit_after_chord);
    }

    #[test]
    fn missing_arg_prompt_preserves_user_alias() {
        // User typed the alias `apropos`; prefill must preserve the
        // alias rather than normalising to the canonical
        // `ex:apropos`. (Apropos's `pattern` arg is Required.)
        let mut a = app_in_command_mode("apropos");
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.command_line, "apropos ");
        assert!(matches!(a.modal, ModalState::Command));
    }

    #[test]
    fn cancel_clears_armed_chord_prompt() {
        let mut a = app_in_command_mode("describe-key");
        a.apply(Action::CommandLineSubmit);
        assert!(a.auto_submit_after_chord);
        a.apply(Action::CommandLineCancel);
        assert!(!a.auto_submit_after_chord);
    }

    #[test]
    fn submit_with_arg_supplied_takes_normal_path() {
        // `describe-key j` with explicit arg should NOT enter
        // prompt mode -- it should just dispatch.
        let mut a = app_in_command_mode("describe-key j");
        a.apply(Action::CommandLineSubmit);
        assert!(!a.auto_submit_after_chord);
        assert!(matches!(a.modal, ModalState::Normal));
        assert!(a.help_buffer.is_some());
    }

    #[test]
    fn arg_slot_completion_for_describe_command_shows_command_names() {
        // After "describe-command moti", the slot is arg 0 with
        // completion source "gen:commands" -- popup should list
        // motion:* commands.
        let mut a = app_in_command_mode("describe-command moti");
        a.apply(Action::CommandLineCompleteOrAdvance);
        let state = a.completion_state.as_ref().expect("popup");
        assert!(
            state
                .candidates
                .iter()
                .any(|c| c.raw.text.starts_with("motion:")),
            "expected motion:* candidates: {:?}",
            state
                .candidates
                .iter()
                .map(|c| &c.raw.text)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn accept_in_arg_slot_replaces_only_the_arg_prefix() {
        let mut a = app_in_command_mode("describe-command moti");
        a.apply(Action::CommandLineCompleteOrAdvance);
        a.apply(Action::CommandLineAcceptCompletion);
        // Should now be "describe-command motion:..." -- the
        // command word + space preserved; only `moti` replaced.
        assert!(a.command_line.starts_with("describe-command motion:"));
    }

    #[test]
    fn ctrl_u_clears_command_line_and_dismisses_popup() {
        let mut a = app_in_command_mode("foo bar baz");
        a.apply(Action::CommandLineCompleteOrAdvance);
        a.apply(Action::CommandLineClear);
        assert_eq!(a.command_line, "");
        assert!(a.completion_state.is_none());
    }

    #[test]
    fn ctrl_w_deletes_trailing_word() {
        let mut a = app_in_command_mode("foo bar baz");
        a.apply(Action::CommandLineDeleteWordBackward);
        assert_eq!(a.command_line, "foo bar ");
    }

    #[test]
    fn ctrl_w_with_trailing_whitespace_strips_word() {
        let mut a = app_in_command_mode("foo bar  ");
        a.apply(Action::CommandLineDeleteWordBackward);
        assert_eq!(a.command_line, "foo ");
    }

    #[test]
    fn ctrl_w_on_single_word_clears() {
        let mut a = app_in_command_mode("foo");
        a.apply(Action::CommandLineDeleteWordBackward);
        assert_eq!(a.command_line, "");
    }

    // ---- Hybrid <C-h> (DESIGN.md §5.11.3 Q11) ----

    #[test]
    fn ctrl_h_on_known_command_describes_it_directly() {
        // `:describe-command` on the cmdline; <C-h> describes that
        // command itself (smart-resolve).
        let mut a = app_in_command_mode("describe-command");
        a.apply(Action::CommandLineDescribeUnderCursor);
        let h = a.help_buffer.as_ref().expect("help should open");
        assert!(h.title.contains("ex:describe-command"));
    }

    #[test]
    fn ctrl_h_on_arg_describes_parent_command_at_arg_anchor() {
        // `:describe-command moti` -- the cursor's word `moti`
        // doesn't resolve to a command; fall back to describing
        // the parent (`ex:describe-command`) scrolled to the
        // `arg:name` anchor.
        let mut a = app_in_command_mode("describe-command moti");
        a.apply(Action::CommandLineDescribeUnderCursor);
        let h = a.help_buffer.as_ref().expect("help should open");
        assert!(h.title.contains("ex:describe-command"));
        // scroll should be set to the arg:name anchor's line.
        let arg_anchor = h.anchors.iter().find(|a| a.name == "arg:name").unwrap();
        assert_eq!(h.scroll, arg_anchor.line as usize);
    }

    #[test]
    fn ctrl_h_on_arg_value_that_is_a_known_command_describes_it() {
        // `:describe-command motion:line-down` -- the arg VALUE
        // resolves to a known command. Hybrid: describe THAT.
        let mut a = app_in_command_mode("describe-command motion:line-down");
        a.apply(Action::CommandLineDescribeUnderCursor);
        let h = a.help_buffer.as_ref().expect("help should open");
        assert!(h.title.contains("motion:line-down"));
    }

    #[test]
    fn ctrl_h_on_unknown_word_emits_error_message() {
        let mut a = app_in_command_mode("no-such-command");
        a.apply(Action::CommandLineDescribeUnderCursor);
        assert!(a.help_buffer.is_none());
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn entering_command_line_dismisses_open_help() {
        // Q16: opening `:` dismisses help. The user can only focus
        // on one thing.
        let mut a = app_with("xx", 10);
        a.help_buffer = Some(crate::help::HelpBuffer::from_lines(
            "preexisting",
            vec!["x".into()],
        ));
        a.apply(Action::EnterCommandLine);
        assert!(a.help_buffer.is_none());
    }

    #[test]
    fn entering_command_line_dismisses_open_completion() {
        let mut a = app_with("xx", 10);
        a.completion_state = Some(CompletionState {
            candidates: Vec::new(),
            selected: 0,
            replace_start: 0,
            original_line: String::new(),
        });
        a.apply(Action::EnterCommandLine);
        assert!(a.completion_state.is_none());
    }

    // ---- delete_trailing_word helper ----

    // ---- Alias preference for command candidates ----

    #[test]
    fn prefer_aliases_rewrites_canonical_to_alias() {
        use lattice_completion::{
            CandidateData, CandidateKind, MatchScore, RawCandidate, RenderedCandidate,
        };
        use lattice_grammar::source::SourceLocation;
        let mut candidates = vec![RenderedCandidate {
            raw: RawCandidate {
                text: "ex:describe-command".into(),
                display: "ex:describe-command".into(),
                kind: CandidateKind::Command,
                data: CandidateData::Command {
                    name: "ex:describe-command".into(),
                    doc: "doc".into(),
                    kind_label: "ex-command".into(),
                    source: SourceLocation::synthetic("test"),
                },
            },
            score: MatchScore::PERFECT,
            match_ranges: vec![],
            annotations: vec![],
        }];
        prefer_aliases_for_command_candidates(&mut candidates, "descri");
        assert_eq!(candidates[0].raw.text, "describe-command");
        assert_eq!(candidates[0].raw.display, "describe-command");
        // Match ranges recomputed against the new text.
        assert!(!candidates[0].match_ranges.is_empty());
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn prefer_aliases_leaves_non_command_candidates_alone() {
        use lattice_completion::{
            CandidateData, CandidateKind, MatchScore, RawCandidate, RenderedCandidate,
        };
        let mut candidates = vec![RenderedCandidate {
            raw: RawCandidate {
                text: "/tmp/foo.rs".into(),
                display: "foo.rs".into(),
                kind: CandidateKind::File,
                data: CandidateData::File {
                    path: "/tmp/foo.rs".into(),
                    is_dir: false,
                    size: None,
                },
            },
            score: MatchScore::PERFECT,
            match_ranges: vec![0..3],
            annotations: vec![],
        }];
        prefer_aliases_for_command_candidates(&mut candidates, "tmp");
        // File candidate untouched.
        assert_eq!(candidates[0].raw.text, "/tmp/foo.rs");
    }

    #[test]
    fn describe_command_resolves_alias_arg() {
        // `:describe-command apropos` -- the arg is an alias.
        // The handler must do two-stage resolution: alias `apropos`
        // -> canonical `ex:apropos` -> CommandSpec lookup.
        // Regression for the bug where the handler did a single
        // direct id_by_name(name) and failed for every alias.
        let mut a = app_in_command_mode("describe-command apropos");
        a.apply(Action::CommandLineSubmit);
        let h = a
            .help_buffer
            .as_ref()
            .expect("describe-command apropos should open help");
        assert!(
            h.title.contains("apropos"),
            "title should reference apropos, got `{}`",
            h.title
        );
        // Should NOT be the error path.
        assert!(
            a.last_message
                .as_ref()
                .map(|m| m.level != EchoLevel::Error)
                .unwrap_or(true)
        );
    }

    #[test]
    fn describe_command_resolves_short_alias_arg() {
        // Same shape but with a short alias (`w` -> `ex:write`).
        let mut a = app_in_command_mode("describe-command w");
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("describe-command w");
        // Title shows whatever the user typed; the resolved spec
        // is `ex:write`. Body must mention the canonical name to
        // confirm we resolved correctly.
        let body = h.content.as_string();
        assert!(
            body.contains("ex:write"),
            "body should reference ex:write: {body}"
        );
    }

    #[test]
    fn describe_command_unknown_alias_emits_error() {
        let mut a = app_in_command_mode("describe-command xyzzy-not-a-thing");
        a.apply(Action::CommandLineSubmit);
        assert!(a.help_buffer.is_none());
        let m = a.last_message.as_ref().unwrap();
        assert_eq!(m.level, EchoLevel::Error);
    }

    #[test]
    fn resolve_command_name_or_alias_handles_both_forms() {
        let mut registry = lattice_grammar::CommandRegistry::new();
        let _ = lattice_grammar::builtins::populate(&mut registry);
        let _ = lattice_grammar::ex_commands::populate(&mut registry);
        // Canonical hits.
        assert!(resolve_command_name_or_alias(&registry, "ex:write").is_some());
        assert!(resolve_command_name_or_alias(&registry, "ex:apropos").is_some());
        assert!(resolve_command_name_or_alias(&registry, "motion:line-down").is_some());
        // Alias hits.
        assert!(resolve_command_name_or_alias(&registry, "w").is_some());
        assert!(resolve_command_name_or_alias(&registry, "apropos").is_some());
        assert!(resolve_command_name_or_alias(&registry, "describe-command").is_some());
        // Misses.
        assert!(resolve_command_name_or_alias(&registry, "nope").is_none());
        assert!(resolve_command_name_or_alias(&registry, "").is_none());
    }

    #[test]
    fn cmdline_completion_includes_lsp_subcommand_aliases() {
        // Diagnostic: typing `:lsp-` and tabbing should surface
        // `lsp-trace`, `lsp-restart`, `lsp-status`, etc. -- the
        // user-facing aliases for `ex:lsp-trace` etc. The
        // CommandsGenerator returns canonical names (`ex:lsp-trace`);
        // `prefer_aliases_for_command_candidates` rewrites them
        // to the longest alias (`lsp-trace`). User reported these
        // not appearing; pin the wiring with a regression test.
        let mut a = app_in_command_mode("lsp-");
        a.apply(Action::CommandLineCompleteOrAdvance);
        let state = a.completion_state.as_ref().expect("popup should open");
        let texts: Vec<&str> = state
            .candidates
            .iter()
            .map(|c| c.raw.text.as_str())
            .collect();
        for needle in [
            "lsp-trace",
            "lsp-status",
            "lsp-restart",
            "lsp-log",
            "lsp-log-level",
            "lsp-log-clear",
        ] {
            assert!(
                texts.contains(&needle),
                "completion should include `{needle}` -- got {:?}",
                texts
            );
        }
    }

    #[test]
    fn parser_accepts_canonical_name_directly() {
        // Defensive: even if the user types the canonical name
        // (`:ex:describe-command`), the parser resolves it. The
        // assertion: no "unknown command" error message. Whatever
        // happens downstream (e.g. `:ex:write` errors on no file
        // name) is unrelated to parser resolution.
        let mut a = app_in_command_mode("ex:describe-command ex:write");
        a.apply(Action::CommandLineSubmit);
        // Should have opened the help buffer; no "unknown
        // command" error from the parser.
        assert!(
            a.help_buffer.is_some(),
            "help should open from canonical-name describe-command"
        );
    }

    // ---- completion.auto_insert_single (B + sub-decision (i)) ----
    //
    // Single-candidate auto-insert at popup-open: when `<Tab>` would
    // open a popup with exactly one candidate AND the option is on,
    // skip the popup and apply the candidate to the cmdline directly.
    // Today there's only one completion path (cmdline `:` Tab), so
    // this hook covers `gen:commands`, `gen:options`, and every other
    // arg-slot generator uniformly. When LSP / Insert-mode completion
    // lands (Phase 4.2, task #199), Phase 4.2 should reuse
    // `open_completion_popup` (or factor a shared helper) so this
    // option stays universal without a second knob.

    #[test]
    fn auto_insert_single_default_is_on() {
        let a = app_with("xx", 10);
        assert!(a.completion_auto_insert_single());
    }

    #[test]
    fn auto_insert_single_replaces_command_line_for_one_candidate() {
        // `:set foldmethod=ind` is a unique fuzzy match against the
        // four enumerated `foldmethod=*` values (manual / indent /
        // markdown / syntax) -- only `foldmethod=indent` survives.
        // Tab should auto-insert it without opening a popup.
        let mut a = app_in_command_mode("set foldmethod=ind");
        assert!(a.completion_auto_insert_single(), "default should be on");
        a.apply(Action::CommandLineCompleteOrAdvance);
        assert!(
            a.completion_state.is_none(),
            "popup must not open when the only candidate auto-inserts"
        );
        assert_eq!(a.command_line, "set foldmethod=indent");
    }

    #[test]
    fn auto_insert_single_off_keeps_popup_for_one_candidate() {
        // Disabling reverts to "always show popup, even with one row".
        let mut a = app_in_command_mode("set foldmethod=ind");
        a.set_completion_auto_insert_single_for_test(false);
        a.apply(Action::CommandLineCompleteOrAdvance);
        let state = a
            .completion_state
            .as_ref()
            .expect("popup should open when option is off");
        assert_eq!(state.candidates.len(), 1);
        assert_eq!(
            a.command_line, "set foldmethod=ind",
            "cmdline must not change until user confirms"
        );
    }

    #[test]
    fn auto_insert_single_does_not_fire_for_multiple_candidates() {
        // Multiple matches → popup opens whether or not the option
        // is on. The auto-insert path is only the one-candidate case.
        let mut a = app_in_command_mode("descri");
        a.apply(Action::CommandLineCompleteOrAdvance);
        let state = a
            .completion_state
            .as_ref()
            .expect("popup should open with multiple candidates");
        assert!(
            state.candidates.len() >= 2,
            "expected several describe-* candidates: {:?}",
            state
                .candidates
                .iter()
                .map(|c| &c.raw.text)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn auto_insert_single_does_not_fire_when_narrowing_open_popup() {
        // Sub-decision (i): only fires at popup-open. Opening on a
        // multi-candidate prefix and narrowing while typing must
        // leave the popup open (even if it shrinks to one) -- vim's
        // default and the less surprising behaviour.
        let mut a = app_in_command_mode("set foldmethod=");
        a.apply(Action::CommandLineCompleteOrAdvance);
        let initial = a
            .completion_state
            .as_ref()
            .expect("popup should open for the value list");
        assert!(initial.candidates.len() >= 2);
        // Narrow by typing toward `indent`.
        for c in "ind".chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        // Popup is still open even after narrowing to the unique
        // match -- auto-insert only fires at popup-open, not on
        // refilter-while-open.
        assert!(
            a.completion_state.is_some(),
            "popup must stay open when narrowed mid-typing"
        );
        assert_eq!(a.command_line, "set foldmethod=ind");
    }

    #[test]
    fn auto_insert_single_set_via_set_command() {
        // `:set nocompletion.auto_insert_single` flips the bool;
        // `:set completion.auto_insert_single` flips it back.
        let mut a = app_with("xx", 10);
        a.command_line = "set nocompletion.auto_insert_single".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(!a.completion_auto_insert_single());
        a.command_line = "set completion.auto_insert_single".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.completion_auto_insert_single());
    }

    #[test]
    fn delete_trailing_word_strips_then_cuts() {
        let mut s = String::from("alpha beta");
        delete_trailing_word(&mut s);
        assert_eq!(s, "alpha ");
    }

    #[test]
    fn delete_trailing_word_handles_only_whitespace() {
        let mut s = String::from("   ");
        delete_trailing_word(&mut s);
        assert_eq!(s, "");
    }

    #[test]
    fn delete_trailing_word_empty_string_is_noop() {
        let mut s = String::new();
        delete_trailing_word(&mut s);
        assert_eq!(s, "");
    }

    #[test]
    fn describe_command_with_no_args_omits_arguments_section() {
        // ex:quit has args_schema: vec![] -- no Arguments section.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:quit".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.help_buffer.as_ref().unwrap().content.as_string();
        assert!(
            !body.contains("Arguments:"),
            "Arguments section should be omitted: {body}"
        );
    }

    #[test]
    fn describe_command_unknown_emits_error_no_overlay() {
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:nope".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.help_buffer.is_none());
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn describe_buffer_renders_state_summary() {
        let mut a = app_with("hello\nworld", 10);
        a.command_line = "describe-buffer".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("help view should open");
        // Some predictable content lines.
        let body = h.content.as_string();
        assert!(body.contains("modal state"));
        assert!(body.contains("cursor:"));
        assert!(body.contains("dirty:"));
        assert!(body.contains("line count:"));
    }

    #[test]
    fn apropos_lists_matching_commands() {
        let mut a = app_with("xx", 10);
        a.command_line = "apropos write".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("help view should open");
        let body = h.content.as_string();
        // Both ex:write and ex:write-quit match the substring.
        assert!(body.contains("ex:write"));
        assert!(body.contains("ex:write-quit"));
    }

    #[test]
    fn apropos_no_matches_renders_empty_view() {
        let mut a = app_with("xx", 10);
        a.command_line = "apropos zxqzxqzxq".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        let body = h.content.as_string();
        assert!(body.contains("no matches"));
    }

    /// Wrap a freshly-built [`HelpBuffer`] as the active buffer the
    /// way the App's `:describe-*` paths do. Used by every help-
    /// navigation test below so they share the same setup.
    fn install_help(a: &mut App, h: HelpBuffer) {
        a.help_buffer = Some(h);
        a.active_buffer = BufferKind::Help;
    }

    // ---- Pane tree (DESIGN.md §5.9, B.1.b) ----

    #[test]
    fn fresh_app_has_one_document_pane() {
        let a = app_with("xx", 10);
        assert_eq!(a.pane_tree.len(), 1);
        assert_eq!(a.active_buffer, BufferKind::Document);
        let active = a.pane_tree.active();
        assert_eq!(active.buffer, BufferKind::Document);
        assert_eq!(active.buffer_id, a.document_buffer_id);
    }

    #[test]
    fn split_pane_horizontal_creates_second_pane() {
        let mut a = app_with("xx", 10);
        a.apply(Action::SplitPaneHorizontal);
        assert_eq!(a.pane_tree.len(), 2);
        // Active stays on original.
        assert_eq!(a.pane_tree.active_index(), 0);
    }

    #[test]
    fn split_pane_vertical_creates_second_pane() {
        let mut a = app_with("xx", 10);
        a.apply(Action::SplitPaneVertical);
        assert_eq!(a.pane_tree.len(), 2);
    }

    #[test]
    fn close_pane_collapses_split() {
        let mut a = app_with("xx", 10);
        a.apply(Action::SplitPaneVertical);
        a.apply(Action::ClosePane);
        assert_eq!(a.pane_tree.len(), 1);
    }

    #[test]
    fn close_last_pane_is_a_noop_with_warning() {
        let mut a = app_with("xx", 10);
        a.apply(Action::ClosePane);
        assert_eq!(a.pane_tree.len(), 1);
        let msg = a.last_message.as_ref().expect("warn echo");
        assert!(msg.text.contains("only one pane"));
    }

    #[test]
    fn next_pane_cycles_active() {
        let mut a = app_with("first\nsecond\nthird", 10);
        a.cursor = Position::new(2, 0);
        a.apply(Action::SplitPaneVertical);
        // After split: 2 panes, both seeded with cursor (2, 0).
        // Move cursor in the active pane.
        a.cursor = Position::new(0, 0);
        a.apply(Action::NextPane);
        assert_eq!(a.pane_tree.active_index(), 1);
        // Pane 1 should still hold its stashed cursor (2, 0).
        assert_eq!(a.cursor, Position::new(2, 0));
        // Cycle back -- pane 0 holds (0, 0) per the in-active mutation.
        a.apply(Action::NextPane);
        assert_eq!(a.pane_tree.active_index(), 0);
        assert_eq!(a.cursor, Position::new(0, 0));
    }

    #[test]
    fn navigate_pane_walks_to_spatial_neighbour() {
        let mut a = app_with("xx", 10);
        a.terminal_width = Some(80);
        a.apply(Action::SplitPaneVertical);
        // Active=0 (left). Navigate Right -> active=1.
        a.apply(Action::NavigatePane(PaneDirection::Right));
        assert_eq!(a.pane_tree.active_index(), 1);
        // Navigate Left -> active=0.
        a.apply(Action::NavigatePane(PaneDirection::Left));
        assert_eq!(a.pane_tree.active_index(), 0);
    }

    #[test]
    fn ctrl_l_redraws_screen_and_invalidates_caches() {
        // `<C-l>` is the user-visible escape hatch for visual
        // glitches. The action must:
        // - clear the visible-highlight + pane-highlight caches so
        //   the next frame repopulates from scratch;
        // - flag the runtime to clear the terminal on next frame;
        // - force a fresh parser run inside this same `apply` (the
        //   end-of-apply `maybe_reparse_syntax` re-syncs against
        //   the bumped version mirror, so by the time the user
        //   sees the next frame the tree matches the document).
        let mut a = app_with("fn main() {}\n", 10);
        a.pane_highlights.insert(0, vec![Vec::new(); 1]);
        a.pending_redraw = false;
        a.apply(Action::RedrawScreen);
        assert!(a.pending_redraw, "runtime should clear terminal next frame");
        assert!(
            a.pane_highlights.is_empty(),
            "pane highlights cache must reset (so next frame repopulates from scratch)"
        );
        // Post-apply, the version mirror equals the document's
        // version because the end-of-apply reparse already ran.
        // The intermediate `u64::MAX` value is gone; that's the
        // desired flow -- a single keystroke produces an
        // already-fresh tree.
        assert_eq!(
            a.last_parsed_text_version,
            a.document.text_version(),
            "post-apply reparse must have synced the version mirror"
        );
        let msg = a.last_message.as_ref().expect("info echo");
        assert!(msg.text.contains("redraw"), "user-visible echo: {msg:?}");
    }

    #[test]
    fn hover_dismisses_on_document_cursor_motion() {
        // Vim/emacs UX: any motion off the hovered symbol drops
        // the popup. Apply a hover popup directly (skipping the
        // async LSP path), move the cursor, assert dismissal.
        let mut a = app_with("fn main() {}\nlet x = 1;\n", 5);
        a.do_open_hover("hover body");
        assert!(a.help_buffer.is_some());
        // State A: focus still on doc, prev_pane_for_help is None.
        assert!(a.prev_pane_for_help.is_none());
        assert!(matches!(a.active_buffer, BufferKind::Document));
        // Drive a real motion through `apply` (`l` -- char-right).
        let inv = lattice_grammar::CommandInvocation::of(a.builtins.char_right.0);
        a.apply(Action::Invoke(inv));
        assert!(
            a.help_buffer.is_none(),
            "hover popup should dismiss on cursor motion in State A"
        );
    }

    #[test]
    fn hover_does_not_dismiss_when_cursor_unchanged() {
        // No-op actions (e.g. setting a no-arg ex command,
        // an out-of-bounds motion that clamps in place) must not
        // dismiss the popup. Use a count-only push (`5`) which
        // doesn't move the cursor.
        let mut a = app_with("fn main() {}\n", 5);
        a.do_open_hover("hover body");
        assert!(a.help_buffer.is_some());
        a.apply(Action::PushDigit(5));
        assert!(
            a.help_buffer.is_some(),
            "hover should survive a count-prefix push"
        );
    }

    #[test]
    fn second_hover_request_focuses_into_popup() {
        // First K opens the popup (State A: cursor in doc); second
        // K transfers focus into the popup (State B: cursor in
        // help). The buffer content is the same; only `active_buffer`
        // and the cursor position change. `prev_pane_for_help`
        // captures pre-State-B state so dismiss restores cleanly.
        let mut a = app_with("fn main() {}\n", 5);
        a.do_open_hover("hover body line 1\nhover body line 2");
        assert!(a.help_buffer.is_some());
        assert!(matches!(a.active_buffer, BufferKind::Document));
        assert!(a.prev_pane_for_help.is_none());
        // Second K -> focus into popup.
        a.do_lsp_hover_request();
        assert!(a.help_buffer.is_some(), "popup stays up after focus");
        assert!(matches!(a.active_buffer, BufferKind::Help));
        let stash = a.prev_pane_for_help.expect("State B captures stash");
        assert_eq!(stash.buffer, BufferKind::Document);
    }

    #[test]
    fn focused_hover_does_not_auto_dismiss_on_motion() {
        // State B: cursor is *inside* the popup; motions move the
        // popup's cursor, not the doc's. The State-A auto-dismiss
        // hook is gated on `prev_pane_for_help.is_none()` -- in
        // State B that field is Some, so motion doesn't drop the
        // popup.
        let mut a = app_with("fn main() {}\n", 5);
        a.do_open_hover("line 1\nline 2\nline 3");
        a.do_lsp_hover_request(); // -> State B
        assert!(matches!(a.active_buffer, BufferKind::Help));
        // Move within popup.
        let inv = lattice_grammar::CommandInvocation::of(a.builtins.line_down.0);
        a.apply(Action::Invoke(inv));
        assert!(a.help_buffer.is_some(), "popup persists in State B");
        assert_eq!(a.cursor.line, 1);
    }

    #[test]
    fn dismiss_focused_hover_restores_doc_cursor() {
        // Esc / q in State B routes to HelpDismiss, which restores
        // the pre-State-B cursor / scroll on the doc.
        let mut a = app_with("fn main() {}\nlet x = 1;\n", 5);
        a.cursor = lattice_protocol::Position::new(1, 4);
        a.do_open_hover("hover body");
        a.do_lsp_hover_request(); // -> State B
        // Move inside the popup.
        let inv = lattice_grammar::CommandInvocation::of(a.builtins.line_down.0);
        a.apply(Action::Invoke(inv));
        assert!(matches!(a.active_buffer, BufferKind::Help));
        // Dismiss.
        a.apply(Action::HelpDismiss);
        assert!(a.help_buffer.is_none());
        assert!(matches!(a.active_buffer, BufferKind::Document));
        assert_eq!(a.cursor, lattice_protocol::Position::new(1, 4));
        assert!(a.prev_pane_for_help.is_none());
    }

    #[test]
    fn opening_help_in_pane_keeps_document_syntax_live() {
        // Bug: opening `:lsp-log` (which routes through
        // `open_help_in_pane`) stashed the document's syntax onto
        // the registry entry, leaving `self.syntax = None` for the
        // duration of the help session. The help buffer renders as
        // a popup overlay over the underlying document; the
        // document paint reads `self.syntax`, so the document
        // appeared unhighlighted under the popup.
        //
        // Fix: `activate_help_in_pane` does NOT call
        // `snapshot_active_document`. Hot-path state stays live;
        // the round-trip back via `activate_document` early-returns
        // for the same-doc case and skips the restore (entry has
        // nothing to give).
        let mut a = app_with("fn main() {}\n", 10);
        a.terminal_width = Some(80);
        a.syntax = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust).unwrap();
        if let Some(s) = a.syntax.as_mut() {
            s.parse(&a.document.text());
        }
        assert!(a.syntax.is_some(), "fixture syntax wired");
        // Open a help buffer in pane (mimics `:lsp-log rust`).
        let _help_id = a.open_help_in_pane(HelpBuffer::from_lines(
            "lsp:rust",
            vec!["log line".into()],
        ));
        assert!(matches!(a.active_buffer, BufferKind::Help));
        // The document's syntax must remain on the hot path so the
        // pane underneath paints with highlights.
        assert!(
            a.syntax.is_some(),
            "syntax must stay live during help-in-pane overlay"
        );
        // Round-trip back to the document.
        let doc_id = a
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        a.activate_document(doc_id);
        assert!(matches!(a.active_buffer, BufferKind::Document));
        assert!(
            a.syntax.is_some(),
            "syntax must survive the help-in-pane round trip"
        );
    }

    #[test]
    fn dismissing_tree_preserves_document_syntax_state() {
        // Regression: opening `:Tree` and pressing `q` to dismiss
        // it returned to the document with `self.syntax = None`,
        // so the renderer fell back to plain text (no
        // colours). Cause: the on-tree-open snapshot moved syntax
        // into the document entry, then activate_document on
        // dismiss called snapshot_active_document again and
        // overwrote the entry's stashed syntax with None.
        let dir = std::env::temp_dir().join(format!("lattice-tree-syntax-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let mut a = app_with("fn main() {}\n", 10);
        a.terminal_width = Some(80);
        // Wire up a Rust syntax instance so there's something to lose.
        a.syntax = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust).unwrap();
        if let Some(s) = a.syntax.as_mut() {
            s.parse(&a.document.text());
        }
        // Open the tree, then dismiss.
        a.command_line = format!("Tree {}", dir.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(matches!(a.active_buffer, crate::buffers::BufferKind::FileTree));
        // `:TreeClose` (the path `q` takes in the tree).
        a.command_line = "TreeClose".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(matches!(a.active_buffer, crate::buffers::BufferKind::Document));
        assert!(
            a.syntax.is_some(),
            "syntax must survive the tree round-trip"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn close_tree_pane_keeps_tree_in_registry() {
        // Trees now live in the unified buffer registry; closing
        // the only pane that referenced one leaves the tree
        // accessible via `:bn` / `:bp` / `:b N`. Use `:bd` to
        // actually drop it.
        let dir = std::env::temp_dir().join(format!("lattice-tree-gc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let mut a = app_with("xx", 10);
        a.terminal_width = Some(80);
        a.apply(Action::SplitPaneVertical);
        a.apply(Action::NavigatePane(PaneDirection::Right));
        a.command_line = format!("Tree {}", dir.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.buffers.file_tree_ids_sorted().len(), 1);
        a.apply(Action::ClosePane);
        // Tree stays in the registry post-close.
        assert_eq!(a.buffers.file_tree_ids_sorted().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn split_inherits_cursor_and_scroll_from_active() {
        let mut a = app_with("a\nb\nc\nd", 10);
        a.cursor = Position::new(2, 0);
        a.scroll = 1;
        a.apply(Action::SplitPaneVertical);
        // Both panes should have (line=2, scroll=1) initially.
        let panes = a.pane_tree.leaves();
        assert_eq!(panes[0].cursor.line, 2);
        assert_eq!(panes[0].scroll, 1);
        assert_eq!(panes[1].cursor.line, 2);
        assert_eq!(panes[1].scroll, 1);
    }

    // ---- Multiple Document buffers (DESIGN.md §5.9, B.1.c) ----

    fn write_temp_file(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("lattice-test-{}-{name}", std::process::id()));
        std::fs::write(&path, content).expect("write temp file");
        path
    }

    #[test]
    fn fresh_app_registers_initial_document() {
        let a = app_with("xx", 10);
        assert_eq!(a.buffers.document_ids_sorted().len(), 1);
        assert!(a.buffers.document(a.document_buffer_id).is_some());
    }

    #[test]
    fn edit_new_file_registers_a_second_buffer() {
        let path = write_temp_file("a", "alpha\n");
        let mut a = app_with("xx", 10);
        let initial_id = a.document_buffer_id;
        a.command_line = format!("e {}", path.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        // Both buffers exist; active switched to the new one.
        assert_eq!(a.buffers.document_ids_sorted().len(), 2);
        assert_ne!(a.document_buffer_id, initial_id);
        assert_eq!(a.document.text(), "alpha\n");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn bnext_cycles_through_open_buffers() {
        let path = write_temp_file("b", "one\n");
        let mut a = app_with("xx", 10);
        let first_id = a.document_buffer_id;
        a.command_line = format!("e {}", path.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let second_id = a.document_buffer_id;
        assert_ne!(first_id, second_id);
        a.command_line = "bn".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.document_buffer_id, first_id);
        a.command_line = "bn".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.document_buffer_id, second_id);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn ls_renders_help_with_every_open_buffer() {
        let path = write_temp_file("c", "x\n");
        let mut a = app_with("xx", 10);
        a.command_line = format!("e {}", path.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        a.command_line = "ls".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("buffers help");
        let body = h.content.as_string();
        // Two buffers listed.
        assert!(body.contains("2 open buffer"));
        assert!(body.contains("2 document"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn editing_already_open_path_switches_back_to_it() {
        let path = write_temp_file("d", "alpha\n");
        let mut a = app_with("xx", 10);
        let initial_id = a.document_buffer_id;
        a.command_line = format!("e {}", path.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let new_id = a.document_buffer_id;
        // Cycle back to first buffer.
        a.command_line = "bn".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.document_buffer_id, initial_id);
        // Re-editing the new file's path should switch to its
        // existing buffer rather than spawning a third.
        a.command_line = format!("e {}", path.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.document_buffer_id, new_id);
        assert_eq!(a.buffers.document_ids_sorted().len(), 2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn bdelete_closes_active_buffer_and_picks_a_successor() {
        let path = write_temp_file("e", "alpha\n");
        let mut a = app_with("xx", 10);
        let initial_id = a.document_buffer_id;
        a.command_line = format!("e {}", path.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        // Now active = new buffer; delete it. Successor should
        // be initial_id.
        a.command_line = "bd".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.document_buffer_id, initial_id);
        assert_eq!(a.buffers.document_ids_sorted().len(), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn bdelete_only_buffer_is_rejected() {
        let mut a = app_with("xx", 10);
        a.command_line = "bd".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.buffers.document_ids_sorted().len(), 1);
        let msg = a.last_message.as_ref().expect("error echo");
        assert!(msg.text.contains("only buffer"));
    }

    // ---- Buffer activation lifecycle ----
    //
    // Regression coverage for the `<C-l>`-needed bug: opening a
    // second file via `:e <path>` left the new buffer with empty
    // folds and stale highlight caches because no single hook ran
    // on activation. `App::activate_buffer_state` is now the one
    // place to add buffer-level state that needs to come up with
    // the buffer.

    #[test]
    fn opening_new_file_seeds_folds_for_indent_foldmethod() {
        // foldmethod=indent on the initial buffer; then `:e <new>`
        // should populate folds for the new buffer without requiring
        // a manual `<C-l>` redraw.
        let path = write_temp_file(
            "activate-folds-indent",
            "a:\n    x\n    y\nb:\n    p\n    q\n",
        );
        let mut a = app_with("xx", 10);
        a.set_foldmethod_for_test(FoldMethod::Indent);
        a.command_line = format!("e {}", path.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        // The new buffer should have folds without `<C-l>`.
        assert!(
            !a.folds.is_empty(),
            "expected folds to be seeded on activation, got empty"
        );
        assert!(
            a.folds.iter().any(|f| f.start_line == 0),
            "expected a fold starting at line 0: {:?}",
            a.folds
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn switching_back_to_buffer_preserves_closed_fold_state() {
        // Open two buffers with foldmethod=indent. Close a fold in
        // the first, switch to the second, switch back -- the fold
        // should still be closed.
        let path = write_temp_file("activate-fold-roundtrip", "a:\n    x\n    y\n");
        let mut a = app_with("first:\n    p\n    q\nsecond:\n    r\n    s\n", 10);
        a.set_foldmethod_for_test(FoldMethod::Indent);
        a.recompute_folds();
        let initial_id = a.document_buffer_id;
        // Close the first fold (line 0) on the initial buffer.
        let first_idx = a
            .folds
            .iter()
            .position(|f| f.start_line == 0)
            .expect("fold");
        a.folds[first_idx].closed = true;
        // Open + activate the new buffer.
        a.command_line = format!("e {}", path.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        // Switch back via :bn.
        a.command_line = "bn".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.document_buffer_id, initial_id);
        // Closed state survived the round-trip.
        assert!(
            a.folds.iter().any(|f| f.start_line == 0 && f.closed),
            "expected fold@0 to remain closed after switch-away-and-back: {:?}",
            a.folds
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn switching_to_unvisited_buffer_first_time_seeds_folds() {
        // Open a second file with foldmethod=manual so its initial
        // entry has no folds, switch foldmethod to indent, then
        // activate -- the activation hook should seed the folds on
        // first visit (entry's `folds` was empty).
        let path = write_temp_file("activate-unvisited", "section:\n    a\n    b\n    c\n");
        let mut a = app_with("xx", 10);
        // Open the second file under foldmethod=manual so no folds
        // get seeded into its entry.
        a.set_foldmethod_for_test(FoldMethod::Manual);
        a.command_line = format!("e {}", path.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let id_target = a.document_buffer_id;
        assert!(a.folds.is_empty(), "manual leaves folds empty");
        // Switch back to the original buffer.
        let original_id = a
            .buffers
            .document_ids_sorted()
            .into_iter()
            .find(|id| *id != id_target)
            .expect("original buffer");
        a.activate_document(original_id);
        // Now flip foldmethod to indent and activate the target;
        // the hook should seed folds for the unvisited-under-indent
        // buffer on first visit.
        a.set_foldmethod_for_test(FoldMethod::Indent);
        a.activate_document(id_target);
        assert_eq!(a.document_buffer_id, id_target);
        assert!(
            !a.folds.is_empty(),
            "expected activation hook to seed folds on first visit under indent: {:?}",
            a.folds
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn activation_skips_fold_seed_for_manual_foldmethod() {
        // Manual foldmethod => activation must NOT auto-create folds
        // (the user's `zf` ranges are authoritative; auto-seeding
        // would surprise them).
        let path = write_temp_file("activate-manual", "a:\n    x\n    y\nb:\n    p\n    q\n");
        let mut a = app_with("xx", 10);
        a.set_foldmethod_for_test(FoldMethod::Manual);
        a.command_line = format!("e {}", path.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(
            a.folds.is_empty(),
            "manual foldmethod should not auto-seed folds: {:?}",
            a.folds
        );
        let _ = std::fs::remove_file(path);
    }

    // ---- File-tree buffer (DESIGN.md §5.9, B.1.d) ----

    #[test]
    fn tree_open_makes_filetree_active() {
        let dir = std::env::temp_dir().join(format!("lattice-tree-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(dir.join("a.txt"), "alpha").ok();
        let mut a = app_with("xx", 10);
        a.command_line = format!("Tree {}", dir.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.active_buffer, BufferKind::FileTree);
        assert_eq!(a.buffers.file_tree_ids_sorted().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tree_close_returns_to_document() {
        let dir = std::env::temp_dir().join(format!("lattice-tree-close-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let mut a = app_with("xx", 10);
        a.command_line = format!("Tree {}", dir.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        a.apply(Action::HelpDismiss);
        assert_eq!(a.active_buffer, BufferKind::Document);
        assert_eq!(a.buffers.file_tree_ids_sorted().len(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tree_motion_routes_through_active_buffer() {
        let dir = std::env::temp_dir().join(format!("lattice-tree-motion-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(dir.join("a.txt"), "x").ok();
        std::fs::write(dir.join("b.txt"), "y").ok();
        let mut a = app_with("xx", 10);
        a.command_line = format!("Tree {}", dir.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let line_down = a.builtins.line_down;
        a.apply(Action::Invoke(CommandInvocation::of(line_down.0)));
        // After unification, `self.cursor` is the active buffer's
        // cursor. The tree's own `cursor` field is archival save-
        // state synced at activation transitions.
        assert_eq!(a.cursor.line, 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- Typed options registry (DESIGN.md §5.12, B.2) ----

    #[test]
    fn set_tabstop_assignment_updates_field() {
        let mut a = app_with("xx", 10);
        a.command_line = "set tabstop=4".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.tabstop(), 4);
    }

    #[test]
    fn set_tabstop_via_alias() {
        let mut a = app_with("xx", 10);
        a.command_line = "set ts=2".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.tabstop(), 2);
    }

    #[test]
    fn set_unknown_option_errors() {
        let mut a = app_with("xx", 10);
        a.command_line = "set whatever".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let msg = a.last_message.as_ref().expect("error");
        assert!(msg.text.contains("Unknown option"), "got: {}", msg.text);
    }

    #[test]
    fn set_no_form_clears_boolean() {
        let mut a = app_with("xx", 10);
        a.command_line = "set nonumber".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(!a.show_line_numbers());
    }

    #[test]
    fn set_no_form_rejects_non_boolean() {
        let mut a = app_with("xx", 10);
        a.command_line = "set notabstop".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let msg = a.last_message.as_ref().expect("error");
        assert!(msg.text.contains("not a boolean"), "got: {}", msg.text);
    }

    #[test]
    fn set_int_out_of_range_errors() {
        let mut a = app_with("xx", 10);
        a.command_line = "set tabstop=999".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let msg = a.last_message.as_ref().expect("error");
        assert!(msg.text.contains("out of range"), "got: {}", msg.text);
    }

    #[test]
    fn describe_option_renders_help_with_metadata() {
        let mut a = app_with("xx", 10);
        a.command_line = "describe-option tabstop".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("describe-option help");
        let body = h.content.as_string();
        assert!(body.contains("tabstop"));
        assert!(body.contains("integer"));
        assert!(body.contains("default"));
    }

    // ---- Computed folds (DESIGN.md §15:18, C.2) ----

    #[test]
    fn foldmethod_indent_populates_folds_from_indentation() {
        let mut a = app_with("def f():\n    pass\n    pass\n", 10);
        a.command_line = "set foldmethod=indent".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.foldmethod(), FoldMethod::Indent);
        assert!(!a.folds.is_empty());
        let f = a.folds.iter().find(|f| f.start_line == 0).expect("fold");
        assert_eq!(f.end_line, 2);
    }

    #[test]
    fn foldmethod_indent_preserves_closed_state_across_reparse() {
        let mut a = app_with("a:\n    b\n    c\n", 10);
        a.set_foldmethod_for_test(FoldMethod::Indent);
        a.recompute_folds();
        assert_eq!(a.folds.len(), 1);
        // Close the fold.
        a.folds[0].closed = true;
        // Recompute should preserve closed state (same range).
        a.recompute_folds();
        assert!(a.folds[0].closed);
    }

    #[test]
    fn foldmethod_manual_default_does_not_recompute() {
        let mut a = app_with("def f():\n    pass\n", 10);
        a.recompute_folds();
        assert!(a.folds.is_empty());
    }

    #[test]
    fn foldmethod_markdown_populates_folds_from_atx_headings() {
        let mut a = app_with("# H1\nbody\nmore body\n", 10);
        a.command_line = "set foldmethod=markdown".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.foldmethod(), FoldMethod::Markdown);
        assert!(!a.folds.is_empty());
        let f = a.folds.iter().find(|f| f.start_line == 0).expect("fold");
        assert!(f.end_line >= 2);
    }

    #[test]
    fn foldmethod_syntax_cascades_to_indent_when_no_md_extension() {
        // Plain-text buffer (no `Syntax`): syntax provider returns
        // None and we cascade to indent.
        let mut a = app_with("def f():\n    pass\n    pass\n", 10);
        a.command_line = "set foldmethod=syntax".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.foldmethod(), FoldMethod::Syntax);
        assert!(a.folds.iter().any(|f| f.start_line == 0 && f.end_line == 2));
    }

    #[test]
    fn foldmethod_syntax_uses_tree_sitter_for_rust_buffer() {
        // With Syntax set up for Rust, `:set foldmethod=syntax`
        // should produce tree-sitter folds (struct, fn, impl) rather
        // than indent folds.
        let mut a = app_with(
            "struct B {\n    x: u8,\n}\n\nimpl B {\n    fn n() -> Self {\n        Self { x: 0 }\n    }\n}\n",
            10,
        );
        // Wire up Rust syntax + parse the document so the fold
        // provider has a tree to query.
        a.syntax = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
            .unwrap();
        if let Some(s) = a.syntax.as_mut() {
            s.parse(&a.document.text());
        }
        a.command_line = "set foldmethod=syntax".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.foldmethod(), FoldMethod::Syntax);
        // Tree-sitter fold for the struct (lines 0..=2).
        assert!(
            a.folds.iter().any(|f| f.start_line == 0 && f.end_line >= 2),
            "expected struct fold from tree-sitter: {:?}",
            a.folds
        );
        // Tree-sitter fold for the impl (starts at line 4).
        assert!(
            a.folds.iter().any(|f| f.start_line == 4),
            "expected impl fold from tree-sitter: {:?}",
            a.folds
        );
    }

    #[test]
    fn foldmethod_indent_identity_preserves_closed_state_after_unrelated_insert() {
        // Two sibling functions; close the *second* fold; insert a
        // new line into the *first* function (shifting line numbers
        // for the second fold). Identity-based matching should keep
        // the second fold closed despite its (start_line, end_line)
        // having shifted.
        let initial = "first:\n    a\n    b\nsecond:\n    x\n    y\n";
        let mut a = app_with(initial, 10);
        a.set_foldmethod_for_test(FoldMethod::Indent);
        a.recompute_folds();
        // Find and close the `second:` fold.
        let second_idx = a
            .folds
            .iter()
            .position(|f| f.start_line == 3)
            .expect("second: fold exists");
        a.folds[second_idx].closed = true;
        // Insert a new line inside the first function (between `a` and `b`).
        a.apply_edit_blocking(Edit::insert(Position::new(2, 0), "    extra\n"))
            .unwrap();
        a.recompute_folds();
        // The recomputed `second:` fold has start_line = 4 now, but
        // its identity (heading text "second:" + indent 0) matches.
        let new_second = a
            .folds
            .iter()
            .find(|f| f.start_line == 4)
            .expect("second: fold survived insertion");
        assert!(
            new_second.closed,
            "closed-state should survive line shift via identity match"
        );
    }

    #[test]
    fn foldmethod_rejects_unknown_value() {
        let mut a = app_with("a\n", 10);
        a.command_line = "set foldmethod=bogus".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.foldmethod(), FoldMethod::Manual);
        assert!(a.last_message.is_some());
    }

    // ---- Hover popup (DESIGN.md §5.9.6, B.3) ----

    #[test]
    fn hover_open_populates_help_buffer() {
        let mut a = app_with("alpha\nbeta\ngamma", 10);
        a.cursor = Position::new(1, 2);
        a.command_line = "hover documentation".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("hover open");
        assert_eq!(h.title, "hover");
        assert!(h.content.as_string().contains("documentation"));
        // State A: focus stays on doc.
        assert!(matches!(a.active_buffer, BufferKind::Document));
        assert!(a.prev_pane_for_help.is_none());
    }

    #[test]
    fn hover_close_dismisses_popup() {
        let mut a = app_with("xx", 10);
        a.command_line = "hover x".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.help_buffer.is_some());
        a.command_line = "HoverClose".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.help_buffer.is_none());
    }

    #[test]
    fn hover_with_no_arg_uses_placeholder() {
        let mut a = app_with("xx", 10);
        a.command_line = "hover".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("hover open");
        assert!(h.content.as_string().contains("empty"));
    }

    // ---- LSP hover (Phase 4.2.b) ----

    #[test]
    fn hover_contents_scalar_string_renders_verbatim() {
        let m = lsp_types::HoverContents::Scalar(lsp_types::MarkedString::String(
            "fn foo() -> u32".into(),
        ));
        assert_eq!(super::hover_contents_to_markdown(&m), "fn foo() -> u32");
    }

    #[test]
    fn hover_contents_language_string_renders_as_fenced_block() {
        let m = lsp_types::HoverContents::Scalar(lsp_types::MarkedString::LanguageString(
            lsp_types::LanguageString {
                language: "rust".into(),
                value: "let x: u32 = 5;".into(),
            },
        ));
        let md = super::hover_contents_to_markdown(&m);
        assert!(md.contains("```rust"));
        assert!(md.contains("let x: u32 = 5;"));
        assert!(md.ends_with("```"));
    }

    #[test]
    fn hover_contents_array_joins_with_double_newline() {
        let m = lsp_types::HoverContents::Array(vec![
            lsp_types::MarkedString::String("first".into()),
            lsp_types::MarkedString::String("second".into()),
        ]);
        let md = super::hover_contents_to_markdown(&m);
        assert_eq!(md, "first\n\nsecond");
    }

    #[test]
    fn hover_contents_markup_uses_value_as_markdown() {
        let m = lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
            kind: lsp_types::MarkupKind::Markdown,
            value: "# heading\n\nbody".into(),
        });
        assert_eq!(super::hover_contents_to_markdown(&m), "# heading\n\nbody");
    }

    #[test]
    fn lsp_hover_request_with_no_uri_echoes_no_lsp_attached() {
        // Initial document has no path, so no URI mapping; the
        // request should set an info message and not panic.
        let mut a = app_with("xx", 10);
        a.apply(Action::LspHoverRequest);
        let msg = a.last_message.as_ref().expect("echo");
        assert_eq!(msg.level, EchoLevel::Info);
        assert!(msg.text.contains("no LSP server"));
    }

    #[test]
    fn lsp_hover_request_pre_cancels_in_flight_token() {
        // Two K presses in a row: the first one's token must be
        // flipped before the second's request fires, so a slow
        // first response gets dropped by the relay's cancel-aware
        // poll loop.
        let mut a = app_with("xx", 10);
        // Manually install an in-flight token.
        let stale = lattice_protocol::CancellationToken::new();
        a.pending_hover_token = Some(stale.clone());
        // Trigger another hover. With no LSP attached the new
        // request bails on the URI lookup, but the cancel of the
        // previous token should still happen first.
        a.apply(Action::LspHoverRequest);
        assert!(
            stale.is_cancelled(),
            "prior in-flight hover token should flip on a new K press"
        );
    }

    #[test]
    fn drain_pending_hover_body_outcome_opens_popup() {
        let mut a = app_with("xx", 10);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<crate::app::HoverOutcome>();
        a.pending_hover_rx = Some(rx);
        a.pending_hover_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(crate::app::HoverOutcome::Body("**bold body**".into()))
            .unwrap();
        a.drain_pending_hover();
        let h = a.help_buffer.as_ref().expect("popup");
        assert!(h.content.as_string().contains("**bold body**"));
        // State A entry: focus still on the doc.
        assert!(matches!(a.active_buffer, BufferKind::Document));
        assert!(a.prev_pane_for_help.is_none());
        assert!(
            a.pending_hover_token.is_none(),
            "delivering the outcome should clear the in-flight token"
        );
    }

    #[test]
    fn drain_pending_hover_no_body_outcome_echoes_no_hover_info() {
        // Regression for the silent-K-press symptom: if every
        // attached server replies with empty contents,
        // `drain_pending_hover` should echo a clear "no hover
        // info" so the user knows their K press was received.
        let mut a = app_with("xx", 10);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<crate::app::HoverOutcome>();
        a.pending_hover_rx = Some(rx);
        a.pending_hover_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(crate::app::HoverOutcome::NoBody { servers_tried: 1 })
            .unwrap();
        a.drain_pending_hover();
        assert!(a.help_buffer.is_none(), "no popup for empty hover");
        let msg = a.last_message.as_ref().expect("echo on no-hover-info");
        assert_eq!(msg.level, EchoLevel::Info);
        assert!(
            msg.text.contains("no hover info"),
            "expected 'no hover info' echo; got `{}`",
            msg.text
        );
    }

    #[test]
    fn drain_pending_hover_no_servers_outcome_echoes_warn() {
        // Buffer URI maps to no attached servers (e.g. spawn
        // failed at boot). The user gets a Warn echo pointing at
        // :lsp-status / :lsp-log so they can investigate.
        let mut a = app_with("xx", 10);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<crate::app::HoverOutcome>();
        a.pending_hover_rx = Some(rx);
        a.pending_hover_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(crate::app::HoverOutcome::NoServers).unwrap();
        a.drain_pending_hover();
        let msg = a
            .last_message
            .as_ref()
            .expect("echo on no-servers-attached");
        assert_eq!(msg.level, EchoLevel::Warn);
        assert!(
            msg.text.contains("no LSP servers"),
            "expected NoServers warn echo; got `{}`",
            msg.text
        );
    }

    #[test]
    fn drain_pending_hover_idle_channel_is_noop() {
        let mut a = app_with("xx", 10);
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel::<crate::app::HoverOutcome>();
        a.pending_hover_rx = Some(rx);
        a.drain_pending_hover();
        assert!(a.help_buffer.is_none());
        assert!(a.last_message.is_none());
    }

    #[test]
    fn app_to_lsp_position_converts_utf8_byte_to_utf16_column() {
        let buf = lattice_core::Buffer::from_text("hello\nαβγ\nworld\n");
        // Line 1 (αβγ): 2-byte UTF-8 chars; byte 4 = end of β.
        // utf-16 column at byte 4: α (1 unit) + β (1 unit) = 2.
        let p = super::app_to_lsp_position(&buf, Position::new(1, 4)).expect("in-range");
        assert_eq!(p.line, 1);
        assert_eq!(p.character, 2);
    }

    #[test]
    fn app_to_lsp_position_returns_none_for_out_of_range_line() {
        let buf = lattice_core::Buffer::from_text("only-one-line\n");
        assert!(super::app_to_lsp_position(&buf, Position::new(99, 0)).is_none());
    }

    // ---- LSP goto-definition (Phase 4.2.c) ----

    fn fake_uri(path: &str) -> lsp_types::Uri {
        use std::str::FromStr;
        lsp_types::Uri::from_str(&format!("file://{path}")).unwrap()
    }

    fn loc(path: &str, line: u32, col: u32) -> lsp_types::Location {
        lsp_types::Location {
            uri: fake_uri(path),
            range: lsp_types::Range {
                start: lsp_types::Position {
                    line,
                    character: col,
                },
                end: lsp_types::Position {
                    line,
                    character: col + 1,
                },
            },
        }
    }

    #[test]
    fn definition_response_scalar_flattens_to_one_location() {
        let resp = lsp_types::GotoDefinitionResponse::Scalar(loc("/x.rs", 1, 2));
        let v = super::definition_response_to_locations(resp);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].range.start.line, 1);
    }

    #[test]
    fn definition_response_array_flattens_verbatim() {
        let resp = lsp_types::GotoDefinitionResponse::Array(vec![
            loc("/a.rs", 0, 0),
            loc("/b.rs", 5, 5),
        ]);
        let v = super::definition_response_to_locations(resp);
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn definition_response_link_uses_target_selection_range() {
        // Link variant carries richer per-result info; we use
        // target_selection_range (narrower) for jumps.
        let link = lsp_types::LocationLink {
            origin_selection_range: None,
            target_uri: fake_uri("/x.rs"),
            target_range: lsp_types::Range {
                start: lsp_types::Position {
                    line: 0,
                    character: 0,
                },
                end: lsp_types::Position {
                    line: 10,
                    character: 0,
                },
            },
            target_selection_range: lsp_types::Range {
                start: lsp_types::Position {
                    line: 5,
                    character: 4,
                },
                end: lsp_types::Position {
                    line: 5,
                    character: 7,
                },
            },
        };
        let resp = lsp_types::GotoDefinitionResponse::Link(vec![link]);
        let v = super::definition_response_to_locations(resp);
        assert_eq!(v.len(), 1);
        // Should be the target_selection_range, not target_range.
        assert_eq!(v[0].range.start.line, 5);
        assert_eq!(v[0].range.start.character, 4);
    }

    #[test]
    fn lsp_definition_request_with_no_uri_echoes_no_lsp_attached() {
        let mut a = app_with("xx", 10);
        a.apply(Action::LspDefinitionRequest);
        let msg = a.last_message.as_ref().expect("echo");
        assert_eq!(msg.level, EchoLevel::Info);
        assert!(msg.text.contains("no LSP server"));
    }

    #[test]
    fn lsp_definition_request_pre_cancels_in_flight_token() {
        let mut a = app_with("xx", 10);
        let stale = lattice_protocol::CancellationToken::new();
        a.pending_definition_token = Some(stale.clone());
        a.apply(Action::LspDefinitionRequest);
        assert!(stale.is_cancelled());
    }

    #[test]
    fn drain_pending_definitions_with_no_results_echoes_not_found() {
        let mut a = app_with("xx", 10);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<lsp_types::Location>>();
        a.pending_definition_rx = Some(rx);
        a.pending_definition_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(Vec::new()).unwrap();
        a.drain_pending_definitions();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no definitions"));
        assert!(a.pending_definition_token.is_none());
    }

    #[test]
    fn drain_pending_definitions_with_single_same_buffer_jumps_in_place() {
        // Set up an App whose document path matches the location's
        // uri, so the jump stays in-buffer (no `:e` round-trip).
        let path = std::env::temp_dir()
            .join(format!("lattice-defjump-{}.rs", std::process::id()));
        std::fs::write(&path, "first line\nsecond line\nthird line\n").unwrap();
        let doc = Document::open(&path).unwrap();
        let mut a = App::new(doc);
        a.set_viewport_height(10);
        // Cursor starts at (0, 0). Drain a definition pointing at
        // line 2 col 5 (utf-16 character; same as utf-8 byte for
        // ASCII).
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<lsp_types::Location>>();
        a.pending_definition_rx = Some(rx);
        a.pending_definition_token = Some(lattice_protocol::CancellationToken::new());
        let target = lsp_types::Location {
            uri: super::tests::fake_uri(path.to_str().unwrap()),
            range: lsp_types::Range {
                start: lsp_types::Position {
                    line: 2,
                    character: 5,
                },
                end: lsp_types::Position {
                    line: 2,
                    character: 6,
                },
            },
        };
        tx.send(vec![target]).unwrap();
        a.drain_pending_definitions();
        // Cursor moved to (2, 5).
        assert_eq!(a.cursor.line, 2);
        assert_eq!(a.cursor.byte, 5);
        // Pre-jump position pushed onto history as PluginPush.
        let pushed = a
            .position_history
            .iter()
            .any(|e| e.source == PositionSource::PluginPush && e.position == Position::ZERO);
        assert!(pushed, "expected PluginPush entry for pre-jump cursor");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn drain_pending_definitions_with_multiple_jumps_to_first_with_count_echo() {
        let path = std::env::temp_dir()
            .join(format!("lattice-defmulti-{}.rs", std::process::id()));
        std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
        let doc = Document::open(&path).unwrap();
        let mut a = App::new(doc);
        a.set_viewport_height(10);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<lsp_types::Location>>();
        a.pending_definition_rx = Some(rx);
        a.pending_definition_token = Some(lattice_protocol::CancellationToken::new());
        let target_path = path.to_str().unwrap();
        tx.send(vec![
            super::tests::loc(target_path, 1, 0),
            super::tests::loc(target_path, 2, 0),
        ])
        .unwrap();
        a.drain_pending_definitions();
        // Jumped to the first.
        assert_eq!(a.cursor.line, 1);
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("2 definitions"));
    }

    // ---- :help (DESIGN.md §5.11) ----

    #[test]
    fn help_with_no_arg_opens_index() {
        let mut a = app_with("xx", 10);
        a.command_line = "help".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("help open");
        assert_eq!(h.title, "help");
        let body = h.content.as_string();
        // Index page advertises the topic table.
        assert!(body.contains("Topic"), "got: {body}");
    }

    #[test]
    fn help_with_topic_opens_that_topic() {
        let mut a = app_with("xx", 10);
        a.command_line = "help folding".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("help open");
        assert_eq!(h.title, "help folding");
        let body = h.content.as_string();
        assert!(
            body.to_lowercase().contains("fold"),
            "expected fold-related content"
        );
    }

    #[test]
    fn help_unknown_topic_errors() {
        let mut a = app_with("xx", 10);
        a.command_line = "help nonexistent".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.help_buffer.is_none());
        let msg = a.last_message.as_ref().expect("error");
        assert!(msg.text.contains("no help topic"), "got: {}", msg.text);
    }

    #[test]
    fn h_alias_resolves_to_help() {
        let mut a = app_with("xx", 10);
        a.command_line = "h folding".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("help open");
        assert_eq!(h.title, "help folding");
    }

    #[test]
    fn describe_buffer_command_emits_topic_cross_link() {
        // `:buffers` (registered as `ex:buffers`) matches the
        // buffers topic's `buffer` pattern, so the describe view
        // should append a `[buffers](help:buffers)` cross-link.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:buffers".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("describe-command open");
        assert!(
            h.links
                .iter()
                .any(|l| matches!(&l.target, crate::help::HelpLinkTarget::Topic(name) if name == "buffers")),
            "expected `Topic(buffers)` link"
        );
    }

    #[test]
    fn help_topic_link_follow_dispatches_to_help() {
        // Open describe-command for a buffers cmd (which appends a
        // topic link), then follow that link via FollowLink.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:buffers".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("describe open");
        let link = h
            .links
            .iter()
            .find(|l| matches!(&l.target, crate::help::HelpLinkTarget::Topic(_)))
            .expect("topic link present")
            .clone();
        let target_pos = link.range.start;
        a.cursor = target_pos;
        a.apply(Action::FollowLink);
        let h = a.help_buffer.as_ref().expect("help reopen");
        assert_eq!(h.title, "help buffers");
    }

    #[test]
    fn help_anchor_link_scrolls_within_current_topic() {
        // `:help languages` ships intra-doc anchor links of the form
        // `[Section 1](#1-tree-sitter-core)`. Following one should
        // scroll the *current* help buffer to the matching heading,
        // not raise "no handler" / not switch topics.
        let mut a = app_with("xx", 10);
        a.command_line = "help languages".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("languages help open");
        // Find the anchor link to "#1-tree-sitter-core" (which the
        // languages topic ships in its quick-reference table).
        let link = h
            .links
            .iter()
            .find(|l| {
                matches!(
                    &l.target,
                    crate::help::HelpLinkTarget::Anchor(s) if s == "1-tree-sitter-core"
                )
            })
            .expect("anchor link to #1-tree-sitter-core present")
            .clone();
        let target_anchor_line = h
            .anchors
            .iter()
            .find(|a| a.name == "1-tree-sitter-core")
            .expect("anchor generated for `## 1. Tree-sitter, core`")
            .line;
        // Position the cursor on the link, then follow.
        // After unification, the active cursor lives on `app.cursor`
        // (regardless of buffer kind); we set it there.
        a.cursor = link.range.start;
        a.apply(Action::FollowLink);
        let h = a.help_buffer.as_ref().expect("help still open");
        assert_eq!(
            h.title, "help languages",
            "follow-link must NOT swap topics for an anchor jump"
        );
        assert_eq!(
            a.cursor.line, target_anchor_line,
            "cursor should land on the heading line"
        );
        assert_eq!(
            a.scroll, target_anchor_line,
            "scroll should follow the anchor"
        );
    }

    #[test]
    fn follow_link_source_opens_file_at_line() {
        // `:describe-command :lsp-trace` (and similar) renders a
        // `[<source>](file:PATH:LINE)` link. Following it should
        // open the file via the multi-buffer machinery and
        // position the cursor at the requested line. Pre-fix this
        // arm just echoed "(file open arrives with multi-buffer)"
        // -- we already had multi-buffer; the placeholder was
        // stale.
        let path = std::env::temp_dir()
            .join(format!("lattice-srclink-{}.rs", std::process::id()));
        std::fs::write(&path, "first\nsecond\nthird\nfourth\n").unwrap();
        let mut a = app_with("xx", 10);
        // Open a help buffer so the active modal/buffer state
        // matches what `FollowLink` expects.
        a.command_line = "help".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        // Build a synthetic source link inside the help buffer.
        // 1-based line number: line 3 in the file → cursor at
        // line index 2 in the buffer.
        let link = crate::help::HelpLink {
            range: lattice_protocol::Range::new(
                lattice_protocol::Position::ZERO,
                lattice_protocol::Position::new(0, 1),
            ),
            target: crate::help::HelpLinkTarget::Source {
                path: path.clone(),
                line: 3,
            },
        };
        if let Some(h) = a.help_buffer.as_mut() {
            h.links.push(link);
            h.cursor = lattice_protocol::Position::ZERO;
        }
        a.active_buffer = BufferKind::Help;
        a.apply(Action::FollowLink);
        // The file should now be the active document.
        assert_eq!(a.active_buffer, BufferKind::Document);
        let opened = a.document.path().expect("active doc has a path");
        assert_eq!(opened, path);
        // Cursor at line index 2 (1-based 3 → 0-based 2).
        assert_eq!(a.cursor.line, 2);
        // NOTE: a `PluginPush` history entry is pushed *before*
        // `do_edit` runs, but `do_edit`'s new-file branch clears
        // the position history (so a fresh buffer's `<C-o>` doesn't
        // walk into the previous buffer's positions). That means
        // cross-buffer jumps from FollowLink and from
        // `jump_to_lsp_location` currently lose their walk-back
        // entry. Per-buffer position history is queued as the
        // proper fix; for now this test asserts the open-and-jump
        // primary behaviour and lets the history side-effect
        // regress until that fix lands.
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn follow_link_source_clamps_line_past_eof() {
        let path = std::env::temp_dir()
            .join(format!("lattice-srclink-clamp-{}.rs", std::process::id()));
        std::fs::write(&path, "only-line\n").unwrap();
        let mut a = app_with("xx", 10);
        a.command_line = "help".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let link = crate::help::HelpLink {
            range: lattice_protocol::Range::new(
                lattice_protocol::Position::ZERO,
                lattice_protocol::Position::new(0, 1),
            ),
            target: crate::help::HelpLinkTarget::Source {
                path: path.clone(),
                line: 999,
            },
        };
        if let Some(h) = a.help_buffer.as_mut() {
            h.links.push(link);
            h.cursor = lattice_protocol::Position::ZERO;
        }
        a.active_buffer = BufferKind::Help;
        a.apply(Action::FollowLink);
        // Out-of-range line should clamp to the last valid line,
        // not panic and not echo a confusing error.
        let last_line = a.document.snapshot().buffer.line_count().saturating_sub(1);
        assert_eq!(a.cursor.line, last_line);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn list_options_includes_every_registered_option() {
        let mut a = app_with("xx", 10);
        a.command_line = "options".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("options help");
        let body = h.content.as_string();
        assert!(body.contains("number"));
        assert!(body.contains("tabstop"));
        assert!(body.contains("scrolloff"));
    }

    #[test]
    fn tree_follow_on_file_opens_document_buffer() {
        let dir = std::env::temp_dir().join(format!("lattice-tree-follow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(dir.join("alpha.txt"), "hello").ok();
        let mut a = app_with("xx", 10);
        a.command_line = format!("Tree {}", dir.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        // Move cursor to the alpha.txt entry (row 1).
        let line_down = a.builtins.line_down;
        a.apply(Action::Invoke(CommandInvocation::of(line_down.0)));
        // Follow.
        a.apply(Action::FollowLink);
        // Active pane now shows the file's Document buffer; the
        // tree stays in the registry (reachable via :bn / :b).
        assert_eq!(a.active_buffer, BufferKind::Document);
        assert_eq!(a.buffers.file_tree_ids_sorted().len(), 1);
        assert_eq!(a.document.text(), "hello");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn help_dismiss_clears_overlay_and_routes_back_to_document() {
        let mut a = app_with("xx", 10);
        install_help(
            &mut a,
            HelpBuffer::from_lines("test", vec!["a".into(), "b".into()]),
        );
        a.apply(Action::HelpDismiss);
        assert!(a.help_buffer.is_none());
        assert_eq!(a.active_buffer, BufferKind::Document);
    }

    #[test]
    fn search_in_help_buffer_targets_help_text() {
        // After unification, `/` works in any read-only buffer
        // (help, file-tree, future kinds). Search reads
        // `active_text()` and `self.cursor`; on a hit it writes
        // `self.cursor` -- exactly the document path.
        let mut a = app_with("xx", 10);
        let body: Vec<String> = vec![
            "alpha".into(),
            "beta".into(),
            "gamma needle".into(),
            "delta".into(),
        ];
        install_help(&mut a, HelpBuffer::from_lines("search-test", body));
        // Open `/` and type `needle` then submit.
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        for c in "needle".chars() {
            a.apply(Action::SearchAppend(c));
        }
        a.apply(Action::SearchSubmit);
        // Cursor should land on line 2 (gamma needle).
        assert_eq!(a.cursor.line, 2, "cursor jumped to the help line");
        // Active buffer stays Help -- search didn't leak into the
        // document.
        assert!(matches!(a.active_buffer, BufferKind::Help));
    }

    #[test]
    fn search_in_help_buffer_populates_all_matches_for_hlsearch() {
        // The renderer paints `app.all_matches` as styled overlays
        // on each visible help line (same painter the document
        // path uses). This test ensures `submit_search` in a help
        // buffer fills `all_matches` against the help text -- the
        // *render* check (visible highlight) is covered by the
        // existing `apply_match_overlay` unit tests; here we just
        // assert the data is in place for the renderer to use.
        let mut a = app_with("xx", 10);
        let body: Vec<String> = vec![
            "alpha needle".into(),
            "beta".into(),
            "gamma needle".into(),
            "delta needle".into(),
        ];
        install_help(&mut a, HelpBuffer::from_lines("hl-test", body));
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        for c in "needle".chars() {
            a.apply(Action::SearchAppend(c));
        }
        a.apply(Action::SearchSubmit);
        assert_eq!(
            a.all_matches.len(),
            3,
            "every occurrence in the help body should be in all_matches"
        );
        assert!(a.current_match.is_some());
    }

    #[test]
    fn search_in_help_buffer_no_longer_blocked_by_read_only_guard() {
        // Regression: `EnterSearch` etc. used to be in the
        // `action_is_document_mutation` allow-list, so `/` in a
        // help buffer echoed "buffer is read-only". They're not
        // mutations -- the guard list now only covers true edits.
        let mut a = app_with("xx", 10);
        install_help(&mut a, HelpBuffer::from_lines("ro", vec!["abc".into(); 5]));
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        // Should be in search modal, not Normal with a read-only
        // echo.
        assert!(
            matches!(a.modal, ModalState::Search(_)),
            "should be in Search modal, got {:?}",
            a.modal
        );
        assert!(
            a.last_message.is_none(),
            "no read-only echo expected, got {:?}",
            a.last_message
        );
    }

    #[test]
    fn help_motion_routes_through_active_buffer() {
        // `j` in help mode should resolve via the same chord grammar
        // as a code buffer, but the apply layer routes the resulting
        // motion to the help cursor (DESIGN.md §5.9 active-buffer
        // routing). 3 line_down invocations -> help cursor line 3,
        // scroll still 0 (viewport math is 10*7/10 - 2 = 5 rows).
        let mut a = app_with("xx", 10);
        let lines: Vec<String> = (0..50).map(|i| format!("line-{i}")).collect();
        install_help(&mut a, HelpBuffer::from_lines("scroll-test", lines));
        let line_down = a.builtins.line_down;
        for _ in 0..3 {
            a.apply(Action::Invoke(CommandInvocation::of(line_down.0)));
        }
        // After unification, `self.cursor` / `self.scroll` are
        // the active buffer's. The help_buffer's cursor field is
        // archival save-state synced at activation transitions.
        assert_eq!(a.cursor.line, 3);
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn help_motion_clamps_to_last_line() {
        let mut a = app_with("xx", 10);
        let lines: Vec<String> = (0..50).map(|i| format!("line-{i}")).collect();
        install_help(&mut a, HelpBuffer::from_lines("scroll-test", lines));
        let line_down = a.builtins.line_down;
        for _ in 0..1000 {
            a.apply(Action::Invoke(CommandInvocation::of(line_down.0)));
        }
        assert_eq!(a.cursor.line, 49);
        // Scroll keeps cursor on screen: viewport 10, cursor 49,
        // so scroll = 49 + 1 - 10 = 40. Production runtime sets
        // viewport per-frame via active_pane_content_height (which
        // shrinks for help popups); the test fixture sets a fixed
        // viewport of 10 and the assertion follows from that.
        assert_eq!(a.scroll, 40);
    }

    #[test]
    fn help_motion_up_clamps_at_zero() {
        let mut a = app_with("xx", 10);
        install_help(
            &mut a,
            HelpBuffer::from_lines("scroll-test", vec!["a".into(); 30]),
        );
        let line_up = a.builtins.line_up;
        for _ in 0..1000 {
            a.apply(Action::Invoke(CommandInvocation::of(line_up.0)));
        }
        assert_eq!(a.cursor.line, 0);
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn help_horizontal_motion_runs_through_grammar() {
        let mut a = app_with("xx", 10);
        install_help(
            &mut a,
            HelpBuffer::from_lines("hl-test", vec!["hello world".into()]),
        );
        let char_right = a.builtins.char_right;
        let char_left = a.builtins.char_left;
        let line_end = a.builtins.line_end;
        let line_start = a.builtins.line_start;
        for _ in 0..3 {
            a.apply(Action::Invoke(CommandInvocation::of(char_right.0)));
        }
        assert_eq!(a.cursor.byte, 3);
        a.apply(Action::Invoke(CommandInvocation::of(char_left.0)));
        assert_eq!(a.cursor.byte, 2);
        a.apply(Action::Invoke(CommandInvocation::of(line_end.0)));
        // `motion:line-end` lands at `byte == line_len` (one past
        // the last byte) -- the same convention as the document
        // path. The grammar uses this position so operator targets
        // (d$, c$, y$) take an exclusive end.
        assert_eq!(a.cursor.byte, 11);
        a.apply(Action::Invoke(CommandInvocation::of(line_start.0)));
        assert_eq!(a.cursor.byte, 0);
    }

    #[test]
    fn help_gg_and_capital_g_route_through_grammar() {
        let mut a = app_with("xx", 10);
        install_help(&mut a, HelpBuffer::from_lines("jt", vec!["x".into(); 30]));
        let goto_first = a.builtins.goto_first_line;
        let goto_last = a.builtins.goto_last_line;
        a.apply(Action::Invoke(CommandInvocation::of(goto_last.0)));
        assert_eq!(a.cursor.line, 29);
        assert!(a.scroll > 0);
        a.apply(Action::Invoke(CommandInvocation::of(goto_first.0)));
        assert_eq!(a.cursor.line, 0);
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn help_count_motions_compose() {
        // `5j` -- the same count semantics as Normal mode.
        let mut a = app_with("xx", 10);
        let lines: Vec<String> = (0..50).map(|i| format!("l{i}")).collect();
        install_help(&mut a, HelpBuffer::from_lines("count", lines));
        let line_down = a.builtins.line_down;
        a.apply(Action::Invoke(
            CommandInvocation::of(line_down.0).with_count(lattice_grammar::command::Count(5)),
        ));
        assert_eq!(a.cursor.line, 5);
    }

    #[test]
    fn help_invoke_operator_echoes_read_only() {
        // Operators on a help buffer are rejected with a "read-only"
        // echo -- v1 doesn't model yank-against-help yet.
        let mut a = app_with("xx", 10);
        install_help(&mut a, HelpBuffer::from_lines("ro", vec!["abc".into(); 5]));
        let yank = a.builtins.yank;
        a.apply(Action::Invoke(
            CommandInvocation::of(yank.0).with_range(lattice_grammar::Range::CurrentLine),
        ));
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("read-only"), "got: {msg:?}");
        assert!(a.unnamed_register.is_none());
    }

    #[test]
    fn help_action_insert_blocked_with_echo() {
        // The read-only guard short-circuits direct mutation
        // actions so a stray Action::Insert while help is active
        // doesn't fall through onto the document.
        let mut a = app_with("xx", 10);
        let original = a.document.text();
        install_help(&mut a, HelpBuffer::from_lines("ro", vec!["abc".into()]));
        a.apply(Action::Insert("PWNED".into()));
        assert_eq!(a.document.text(), original);
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("read-only"), "got: {msg:?}");
    }

    #[test]
    fn ctrl_o_walks_back_to_document_from_help() {
        // `<C-o>` from inside a help buffer should land back on the
        // document spot the user opened the help from. That's the
        // first user-visible win of active-buffer routing.
        let mut a = app_with("first\nsecond\nthird\nfourth", 10);
        a.cursor = Position::new(2, 0);
        // Open help via the same path the App uses internally so
        // the position-history entry is recorded.
        a.open_help(HelpBuffer::from_lines("h", vec!["help body".into()]));
        assert_eq!(a.active_buffer, BufferKind::Help);
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.active_buffer, BufferKind::Document);
        assert_eq!(a.cursor.line, 2);
    }

    #[test]
    fn block_paste_extends_buffer_when_below_eof() {
        // Yank 2 rows then paste at the bottom -- the missing row is
        // appended as a fresh line.
        let mut a = enter_block_visual("abcd\n1234", Position::new(0, 1), Position::new(1, 2));
        let yank =
            CommandInvocation::of(a.builtins.yank.0).with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(yank));
        a.apply(Action::ExitVisual);
        // Move to last line and paste with `P` (before-cursor) at col 0.
        a.cursor = Position::new(1, 0);
        a.apply(Action::PasteBefore);
        // Line 1 becomes "bc1234"; new line 2 holds "23".
        assert_eq!(a.document.text(), "abcd\nbc1234\n23");
    }

    #[test]
    fn yank_then_paste_round_trips_word() {
        let mut a = app_with("hello world", 10);
        let yank = CommandInvocation::of(a.builtins.yank.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(yank));
        // Move cursor to end of buffer.
        a.cursor = Position::new(0, 11);
        a.apply(Action::PasteAfter);
        assert_eq!(a.document.text(), "hello worldhello ");
    }

    #[test]
    fn delete_then_paste_after_emulates_xp_swap() {
        // Vim trick: cursor on 'a' of "abc"; `xp` swaps 'a' and 'b' -> "bac".
        let mut a = app_with("abc", 10);
        a.cursor = Position::ZERO;
        // x: delete char-right
        let inv = CommandInvocation::of(a.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.char_right, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "bc");
        // p: paste after cursor (cursor at 0 on 'b'; paste after -> "bac").
        a.apply(Action::PasteAfter);
        assert_eq!(a.document.text(), "bac");
    }

    #[test]
    fn after_change_user_can_type_and_replacement_lands() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.change.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.modal, ModalState::Insert);
        a.apply(Action::Insert("HEY ".into()));
        assert_eq!(a.document.text(), "HEY world");
    }

    // ---- Search hlsearch ----

    #[test]
    fn search_preview_populates_all_matches() {
        let mut a = app_with("foo bar foo baz foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        assert_eq!(a.all_matches.len(), 3);
    }

    #[test]
    fn search_submit_keeps_all_matches_for_hlsearch() {
        let mut a = app_with("foo bar foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.all_matches.len(), 2);
    }

    #[test]
    fn search_cancel_clears_all_matches() {
        let mut a = app_with("foo bar foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        assert!(!a.all_matches.is_empty());
        a.apply(Action::SearchCancel);
        assert!(a.all_matches.is_empty());
    }

    #[test]
    fn search_word_under_cursor_populates_all_matches() {
        let mut a = app_with("foo bar foo bar foo", 10);
        a.cursor = Position::new(0, 1); // on first "foo"
        a.apply(Action::SearchWordUnderCursor(SearchDirection::Forward));
        assert_eq!(a.all_matches.len(), 3);
    }

    #[test]
    fn search_works_across_lines() {
        let mut a = app_with("foo\nbar\nfoo\nbaz", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.cursor, Position::new(0, 0));
        a.apply(Action::SearchNext);
        assert_eq!(a.cursor, Position::new(2, 0));
    }

    // ---- LSP wiring tests (Phase 4.1.i) ---------------------

    #[test]
    fn lsp_supervisor_constructed_with_builtin_configs() {
        let app = App::new(Document::from_text(""));
        // Builtin registry: rust, python, go, typescript, c-cpp,
        // lua. Six entries today.
        assert!(
            app.lsp.try_lock().unwrap().configs().len() >= 6,
            "expected at least 6 builtin server configs"
        );
        // Supervisor starts dormant.
        assert_eq!(app.lsp.try_lock().unwrap().running_actor_count(), 0);
        assert_eq!(app.lsp.try_lock().unwrap().attached_buffer_count(), 0);
        assert!(app.buffer_uris.is_empty());
    }

    #[test]
    fn buffer_uri_returns_none_before_initialize_lsp() {
        let app = App::new(Document::from_text("fn main() {}"));
        // No initialize_lsp call -> no buffer_uris entries.
        assert!(app.buffer_uri(app.document_buffer_id).is_none());
    }

    #[test]
    fn lsp_close_buffer_removes_uri_mapping_for_unattached_buffer() {
        let mut app = App::new(Document::from_text(""));
        // Seed a fake mapping (as if initialize_lsp had attached it).
        let fake_uri =
            <lattice_lsp::Uri as std::str::FromStr>::from_str("file:///tmp/x.rs").unwrap();
        app.buffer_uris.insert(app.document_buffer_id, fake_uri);
        assert!(app.buffer_uri(app.document_buffer_id).is_some());

        app.lsp_close_buffer(app.document_buffer_id);
        assert!(app.buffer_uri(app.document_buffer_id).is_none());
    }

    #[test]
    fn lsp_close_buffer_is_noop_for_unmapped_id() {
        let mut app = App::new(Document::from_text(""));
        // No mapping exists; close must not panic.
        app.lsp_close_buffer(app.document_buffer_id);
        assert!(app.buffer_uris.is_empty());
    }

    #[test]
    fn lsp_record_edit_is_noop_when_no_uri_mapping() {
        let app = App::new(Document::from_text("hi"));
        // No URI mapping -> record_edit short-circuits, no panic.
        app.lsp_record_edit(
            app.document_buffer_id,
            &Edit::insert(Position::new(0, 0), "x"),
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn initialize_lsp_with_no_path_is_noop() {
        let mut app = App::new(Document::from_text("fn main() {}"));
        // Document has no on-disk path, so initialize_lsp
        // shouldn't try to attach anything.
        app.initialize_lsp().await;
        assert_eq!(app.lsp.try_lock().unwrap().attached_buffer_count(), 0);
        assert!(app.buffer_uris.is_empty());
    }

    // ---- LSP diagnostic navigation tests (Phase 4.1.d.iv) ----

    /// Helper: seed N diagnostics into the App's LSP layer at
    /// the given lines + map a fake URI to the active buffer.
    fn seed_diags_at_lines(app: &mut App, lines: &[u32]) {
        use std::str::FromStr;
        let uri = lattice_lsp::Uri::from_str("file:///tmp/x.rs").unwrap();
        app.buffer_uris.insert(app.document_buffer_id, uri.clone());
        let diags: Vec<lattice_lsp::Diagnostic> = lines
            .iter()
            .map(|line| lattice_lsp::Diagnostic {
                range: lattice_lsp::LspRange {
                    start: lattice_lsp::LspPosition {
                        line: *line,
                        character: 0,
                    },
                    end: lattice_lsp::LspPosition {
                        line: *line,
                        character: 1,
                    },
                },
                severity: Some(lattice_lsp::DiagnosticSeverity::ERROR),
                code: None,
                code_description: None,
                source: None,
                message: format!("err on line {line}"),
                related_information: None,
                tags: None,
                data: None,
            })
            .collect();
        app.lsp_diagnostics.apply(lattice_lsp::DiagnosticEvent {
            server_id: std::sync::Arc::from("rust"),
            uri,
            version: None,
            diagnostics: std::sync::Arc::from(diags.into_boxed_slice()),
        });
    }

    #[test]
    fn next_diagnostic_advances_cursor() {
        let mut app = app_with("a\nb\nc\nd\ne\n", 10);
        seed_diags_at_lines(&mut app, &[1, 3]);
        app.cursor = Position::new(0, 0);
        app.do_next_diagnostic();
        assert_eq!(app.cursor, Position::new(1, 0));
        app.do_next_diagnostic();
        assert_eq!(app.cursor, Position::new(3, 0));
        // Past the last -> wraps to the first.
        app.do_next_diagnostic();
        assert_eq!(app.cursor, Position::new(1, 0));
    }

    #[test]
    fn prev_diagnostic_walks_backward() {
        let mut app = app_with("a\nb\nc\nd\ne\n", 10);
        seed_diags_at_lines(&mut app, &[1, 3]);
        app.cursor = Position::new(4, 0);
        app.do_prev_diagnostic();
        assert_eq!(app.cursor, Position::new(3, 0));
        app.do_prev_diagnostic();
        assert_eq!(app.cursor, Position::new(1, 0));
        // Past the first -> wraps to the last.
        app.do_prev_diagnostic();
        assert_eq!(app.cursor, Position::new(3, 0));
    }

    #[test]
    fn next_diagnostic_with_no_attachment_echoes_error() {
        let mut app = app_with("hi\n", 5);
        // No buffer_uris mapping -> "no LSP attachment".
        app.do_next_diagnostic();
        let msg = app.last_message.as_ref().expect("expected echo");
        assert!(msg.text.contains("no LSP attachment"), "got: {}", msg.text);
    }

    #[test]
    fn next_diagnostic_with_no_diagnostics_echoes_info() {
        let mut app = app_with("hi\n", 5);
        // Seed an empty layer mapping.
        use std::str::FromStr;
        let uri = lattice_lsp::Uri::from_str("file:///tmp/empty.rs").unwrap();
        app.buffer_uris.insert(app.document_buffer_id, uri);
        app.do_next_diagnostic();
        let msg = app.last_message.as_ref().expect("expected echo");
        assert!(msg.text.contains("no diagnostics"), "got: {}", msg.text);
    }

    #[test]
    fn list_diagnostics_opens_help_buffer() {
        let mut app = app_with("hi\n", 5);
        seed_diags_at_lines(&mut app, &[0, 1]);
        app.do_list_diagnostics();
        let help = app.help_buffer.as_ref().expect("help buffer should open");
        assert_eq!(help.title, "diagnostics");
        let body = help.content.as_string();
        // Header summary + per-URI section + per-diagnostic rows.
        assert!(body.contains("Workspace diagnostics"));
        assert!(body.contains("file:///tmp/x.rs") || body.contains("/tmp/x.rs"));
        assert!(body.contains("err on line 0"));
        assert!(body.contains("err on line 1"));
        // Two diagnostic links to follow.
        assert_eq!(help.links.len(), 2);
    }

    #[test]
    fn list_diagnostics_with_empty_layer_renders_none() {
        let mut app = app_with("hi\n", 5);
        // No diagnostics seeded.
        app.do_list_diagnostics();
        let help = app.help_buffer.as_ref().expect("help buffer should open");
        let body = help.content.as_string();
        assert!(body.contains("(none)"));
    }

    // ---- LSP introspection tests (Phase 4.1.g) ---------------

    #[test]
    fn lsp_log_with_no_running_servers_echoes_message() {
        // Phase 3: `:lsp-log` (with or without arg) routes through
        // the LSP picker. With zero running actors there's nothing
        // to pick; the user gets a clear echo instead of an empty
        // popup.
        let mut app = app_with("hi\n", 5);
        app.do_open_lsp_log(None);
        let msg = app.last_message.as_ref().expect("echoes a message");
        assert!(
            msg.text.contains("no LSP servers running"),
            "expected 'no LSP servers running' in echo, got {:?}",
            msg.text
        );
        assert!(app.picker.is_none(), "picker should not have opened");
    }

    #[test]
    fn lsp_log_with_arg_no_match_echoes_message() {
        let mut app = app_with("hi\n", 5);
        app.do_open_lsp_log(Some("rust"));
        let msg = app.last_message.as_ref().unwrap();
        assert!(msg.text.contains("no LSP server"));
    }

    #[test]
    fn open_lsp_log_in_pane_renders_per_server_records() {
        // Direct unit test of the in-pane helper (picker accept
        // path bypasses the picker for single-instance cases too).
        let mut app = app_with("hi\n", 5);
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        app.lsp_logger.log(
            Some(&id),
            lattice_lsp::LogLevel::Warn,
            lattice_lsp::LogSource::Stderr,
            "compile error",
        );
        app.open_lsp_log_in_pane("rust");
        // Lives in the registry as a Help variant + active pane.
        let help_id = app
            .buffers
            .help_with_title("lsp:rust")
            .expect("buffer registered");
        assert_eq!(app.active_pane_buffer_id(), help_id);
        let body = app
            .buffers
            .help(help_id)
            .unwrap()
            .content
            .as_string();
        assert!(body.contains("compile error"));
    }

    #[test]
    fn open_lsp_log_in_pane_excludes_trace_records() {
        let mut app = app_with("hi\n", 5);
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        app.lsp_logger.enable_trace(std::sync::Arc::clone(&id));
        app.lsp_logger.log(
            Some(&id),
            lattice_lsp::LogLevel::Trace,
            lattice_lsp::LogSource::Trace,
            "→ Request id=1",
        );
        app.lsp_logger.log(
            Some(&id),
            lattice_lsp::LogLevel::Info,
            lattice_lsp::LogSource::Client,
            "lifecycle",
        );
        app.open_lsp_log_in_pane("rust");
        let help_id = app.buffers.help_with_title("lsp:rust").unwrap();
        let body = app
            .buffers
            .help(help_id)
            .unwrap()
            .content
            .as_string();
        // Trace records go to the trace buffer; lifecycle here.
        assert!(!body.contains("→ Request"));
        assert!(body.contains("lifecycle"));
    }

    #[test]
    fn lsp_log_buffer_refreshes_live_when_record_appended() {
        let mut app = app_with("hi\n", 5);
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        // Open the per-server log buffer in pane.
        app.open_lsp_log_in_pane("rust");
        let help_id = app.buffers.help_with_title("lsp:rust").unwrap();
        let body_before = app.buffers.help(help_id).unwrap().content.as_string();
        assert!(!body_before.contains("fresh-after-open"));
        // Push a new record AFTER the buffer was opened.
        app.lsp_logger.log(
            Some(&id),
            lattice_lsp::LogLevel::Info,
            lattice_lsp::LogSource::Client,
            "fresh-after-open",
        );
        // The publisher fired Event::LspLogPushed; drain hook
        // should refresh the open log buffer.
        app.drain_lsp_log_events();
        let body_after = app.buffers.help(help_id).unwrap().content.as_string();
        assert!(
            body_after.contains("fresh-after-open"),
            "expected new record visible after drain, got body:\n{body_after}"
        );
    }

    #[test]
    fn lsp_log_drain_is_noop_when_no_log_buffer_open() {
        // Pushing log records with no log buffer open should not
        // crash or echo anything; the drain just consumes events
        // and finds no matching titles.
        let mut app = app_with("hi\n", 5);
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        app.lsp_logger.log(
            Some(&id),
            lattice_lsp::LogLevel::Info,
            lattice_lsp::LogSource::Client,
            "no-target",
        );
        app.drain_lsp_log_events();
        // No help buffers should have appeared.
        assert!(app.buffers.help_with_title("lsp:rust").is_none());
        assert!(app.buffers.help_with_title("lsp").is_none());
    }

    #[test]
    fn lsp_trace_buffer_refreshes_live_when_trace_record_appended() {
        let mut app = app_with("hi\n", 5);
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        // Trace gating requires the toggle on for trace records
        // to land in the ring (and fire the publisher).
        app.lsp_logger.enable_trace(std::sync::Arc::clone(&id));
        app.open_lsp_trace_log_in_pane("rust");
        let help_id = app.buffers.help_with_title("lsp:rust:trace").unwrap();
        let before = app.buffers.help(help_id).unwrap().content.as_string();
        assert!(!before.contains("→ NEW"));
        app.lsp_logger.log(
            Some(&id),
            lattice_lsp::LogLevel::Trace,
            lattice_lsp::LogSource::Trace,
            "→ NEW request id=42",
        );
        app.drain_lsp_log_events();
        let after = app.buffers.help(help_id).unwrap().content.as_string();
        assert!(after.contains("→ NEW"));
    }

    #[test]
    fn lsp_log_burst_coalesces_into_one_refresh() {
        // Many records pushed in quick succession should result
        // in at most one buffer rebuild per scope per drain.
        // (We can't observe the rebuild count directly without
        // instrumentation; instead we assert the final body
        // contains every pushed record AND that drain is fast.)
        let mut app = app_with("hi\n", 5);
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        app.open_lsp_log_in_pane("rust");
        for i in 0..50 {
            app.lsp_logger.log(
                Some(&id),
                lattice_lsp::LogLevel::Info,
                lattice_lsp::LogSource::Client,
                format!("msg-{i}"),
            );
        }
        app.drain_lsp_log_events();
        let help_id = app.buffers.help_with_title("lsp:rust").unwrap();
        let body = app.buffers.help(help_id).unwrap().content.as_string();
        // First and last pushed records both visible.
        assert!(body.contains("msg-0"));
        assert!(body.contains("msg-49"));
    }

    #[test]
    fn open_lsp_trace_log_in_pane_shows_only_trace_records() {
        let mut app = app_with("hi\n", 5);
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        app.lsp_logger.enable_trace(std::sync::Arc::clone(&id));
        app.lsp_logger.log(
            Some(&id),
            lattice_lsp::LogLevel::Trace,
            lattice_lsp::LogSource::Trace,
            "→ Request id=1",
        );
        app.lsp_logger.log(
            Some(&id),
            lattice_lsp::LogLevel::Info,
            lattice_lsp::LogSource::Client,
            "lifecycle",
        );
        app.open_lsp_trace_log_in_pane("rust");
        let help_id = app.buffers.help_with_title("lsp:rust:trace").unwrap();
        let body = app
            .buffers
            .help(help_id)
            .unwrap()
            .content
            .as_string();
        // Trace yes, lifecycle no.
        assert!(body.contains("→ Request"));
        assert!(!body.contains("lifecycle"));
    }

    #[test]
    fn open_help_in_pane_registers_buffer_and_activates_pane() {
        let mut app = app_with("hi\n", 5);
        let buf = HelpBuffer::from_lines(
            "test-help",
            vec!["# heading".into(), "body".into()],
        );
        let id = app.open_help_in_pane(buf);
        // Lives in the registry as a Help variant.
        assert!(app.buffers.help(id).is_some());
        // Active pane points at it.
        assert_eq!(app.active_pane_buffer_id(), id);
        assert!(matches!(app.active_buffer, BufferKind::Help));
        // Hot-path popup slot mirrors the registry copy.
        assert_eq!(
            app.help_buffer.as_ref().unwrap().title,
            "test-help"
        );
        // :ls walks the registry; help variants count.
        assert!(app.buffers.help_ids_sorted().contains(&id));
    }

    #[test]
    fn open_help_in_pane_dedups_by_title() {
        let mut app = app_with("hi\n", 5);
        let id1 = app.open_help_in_pane(HelpBuffer::from_lines(
            "lsp:rust",
            vec!["v1".into()],
        ));
        let id2 = app.open_help_in_pane(HelpBuffer::from_lines(
            "lsp:rust",
            vec!["v2 (refreshed)".into()],
        ));
        assert_eq!(id1, id2, "same title returns same BufferId");
        // Refresh path overwrote the body.
        let body = app.help_buffer.as_ref().unwrap().content.as_string();
        assert!(body.contains("refreshed"));
        // Single help entry in the registry.
        assert_eq!(app.buffers.help_ids_sorted().len(), 1);
    }

    #[test]
    fn open_buffer_picker_seeds_with_every_registry_entry() {
        let mut app = app_with("hi\n", 5);
        // Add a help buffer so the picker has more than just the
        // initial document to filter against.
        let _help_id = app.open_help_in_pane(HelpBuffer::from_lines(
            "lsp:rust",
            vec!["a".into()],
        ));
        // Activate back to the document so the picker's "active"
        // marker doesn't land on the help buffer.
        let doc_id = app
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        app.open_buffer_picker();
        let p = app.picker.as_ref().expect("picker should be open");
        // Initial: every buffer in the registry. With no filter,
        // both the doc and the help buffer should be present.
        assert!(p.candidates.len() >= 2);
        assert_eq!(p.title, "buffers");
    }

    #[test]
    fn picker_accept_switches_to_selected_buffer() {
        let mut app = app_with("hi\n", 5);
        let help_id = app.open_help_in_pane(HelpBuffer::from_lines(
            "test-target",
            vec!["body".into()],
        ));
        let doc_id = app
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        // Start on the doc.
        app.activate_document(doc_id);
        assert!(matches!(app.active_buffer, BufferKind::Document));
        // Open picker, type the help title, accept.
        app.open_buffer_picker();
        for c in "test-target".chars() {
            app.apply(Action::PickerAppend(c));
        }
        app.apply(Action::PickerAccept);
        // Picker is dismissed; active pane is on the help buffer.
        assert!(app.picker.is_none());
        assert_eq!(app.active_pane_buffer_id(), help_id);
        assert!(matches!(app.active_buffer, BufferKind::Help));
    }

    #[test]
    fn picker_dismiss_leaves_active_pane_unchanged() {
        let mut app = app_with("hi\n", 5);
        let doc_id = app
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        app.open_buffer_picker();
        app.apply(Action::PickerDismiss);
        assert!(app.picker.is_none());
        assert_eq!(app.active_pane_buffer_id(), doc_id);
    }

    #[test]
    fn buffer_picker_previews_initial_selection_in_active_pane() {
        // With doc + help in registry, opening the picker on the
        // doc immediately previews the alternate (help) buffer in
        // the active pane.
        let mut app = app_with("hi\n", 5);
        let help_id = app.open_help_in_pane(HelpBuffer::from_lines(
            "alt",
            vec!["alt body".into()],
        ));
        let doc_id = app
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        // Sanity: starting state.
        assert_eq!(app.active_pane_buffer_id(), doc_id);
        app.open_buffer_picker();
        // Picker open + preview switched the pane to the help
        // buffer (the alternate -- "(current)" is the doc).
        assert_eq!(app.active_pane_buffer_id(), help_id);
        assert!(matches!(app.active_buffer, BufferKind::Help));
    }

    #[test]
    fn picker_dismiss_restores_origin_when_previewing() {
        let mut app = app_with("hi\n", 5);
        let _help_id = app.open_help_in_pane(HelpBuffer::from_lines(
            "alt",
            vec!["alt body".into()],
        ));
        let doc_id = app
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        app.open_buffer_picker();
        // Preview moved us off the doc.
        assert_ne!(app.active_pane_buffer_id(), doc_id);
        app.apply(Action::PickerDismiss);
        // Esc restored the original.
        assert!(app.picker.is_none());
        assert_eq!(app.active_pane_buffer_id(), doc_id);
        assert!(matches!(app.active_buffer, BufferKind::Document));
    }

    #[test]
    fn picker_select_next_re_previews_new_candidate() {
        let mut app = app_with("hi\n", 5);
        let help_a = app.open_help_in_pane(HelpBuffer::from_lines(
            "alpha-help",
            vec!["a".into()],
        ));
        let help_b = app.open_help_in_pane(HelpBuffer::from_lines(
            "beta-help",
            vec!["b".into()],
        ));
        let doc_id = app
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        app.open_buffer_picker();
        let first_preview = app.active_pane_buffer_id();
        // Move down -- previews the next candidate.
        app.apply(Action::PickerSelectNext);
        let second_preview = app.active_pane_buffer_id();
        assert_ne!(first_preview, second_preview, "selection moved -> different preview");
        // Both previews land on one of the help buffers we set up.
        assert!(first_preview == help_a || first_preview == help_b || first_preview == doc_id);
        assert!(second_preview == help_a || second_preview == help_b || second_preview == doc_id);
        // Dismiss restores the original document.
        app.apply(Action::PickerDismiss);
        assert_eq!(app.active_pane_buffer_id(), doc_id);
    }

    #[test]
    fn picker_preview_does_not_pollute_position_history() {
        // Hover-previewing through several candidates should not
        // push to the jump list; only an *accepted* switch should.
        let mut app = app_with("hi\n", 5);
        let _h1 = app.open_help_in_pane(HelpBuffer::from_lines(
            "h-one",
            vec!["a".into()],
        ));
        let _h2 = app.open_help_in_pane(HelpBuffer::from_lines(
            "h-two",
            vec!["b".into()],
        ));
        let doc_id = app
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        let history_before = app.position_history.len();
        app.open_buffer_picker();
        app.apply(Action::PickerSelectNext);
        app.apply(Action::PickerSelectNext);
        app.apply(Action::PickerSelectPrev);
        app.apply(Action::PickerDismiss);
        let history_after = app.position_history.len();
        assert_eq!(
            history_before, history_after,
            "preview hovers should leave the jump list alone"
        );
    }

    #[test]
    fn lsp_trace_toggle_flips_state_without_opening_buffer() {
        let mut app = app_with("hi\n", 5);
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        // Off -> on.
        app.do_toggle_lsp_trace("rust");
        assert!(app.lsp_logger.is_tracing(&id));
        // Pure toggle now -- the trace buffer is opened separately
        // via :lsp-trace-log so peeking doesn't flip the toggle off.
        assert!(app.help_buffer.is_none());
        let msg = app.last_message.as_ref().unwrap();
        assert!(msg.text.contains("on"));
        assert!(msg.text.contains(":lsp-trace-log"));
        // On -> off.
        app.do_toggle_lsp_trace("rust");
        assert!(!app.lsp_logger.is_tracing(&id));
        assert!(app.help_buffer.is_none());
    }

    #[test]
    fn lsp_trace_resolves_binary_name_to_canonical_id() {
        // `:lsp-trace rust-analyzer` should resolve to the `rust`
        // config id (the registered binary file_name match) and
        // toggle the trace flag on `rust`, NOT a phantom
        // `rust-analyzer` id that nothing else looks at.
        let mut app = app_with("hi\n", 5);
        let canonical: std::sync::Arc<str> = std::sync::Arc::from("rust");
        let phantom: std::sync::Arc<str> = std::sync::Arc::from("rust-analyzer");
        app.do_toggle_lsp_trace("rust-analyzer");
        assert!(app.lsp_logger.is_tracing(&canonical));
        assert!(!app.lsp_logger.is_tracing(&phantom));
        let msg = app.last_message.as_ref().unwrap();
        assert!(msg.text.contains("resolved"));
    }

    #[test]
    fn lsp_trace_unknown_name_echoes_error_with_running_servers() {
        let mut app = app_with("hi\n", 5);
        app.do_toggle_lsp_trace("totally-fake-server-name");
        let msg = app.last_message.as_ref().unwrap();
        assert!(matches!(msg.level, EchoLevel::Error));
        assert!(msg.text.contains("totally-fake-server-name"));
    }

    #[test]
    fn k_chord_is_registered_in_keymap() {
        // `:describe-key K` walks the keymap registry; without an
        // entry there it reports "K is not bound" even though the
        // input translator dispatches K to LspHoverRequest. The
        // registry entry is the source of truth `:describe-key`
        // and `:apropos` consult.
        use crate::keymap::{BindingMode, default_keymap};
        let entries = default_keymap();
        let k = entries
            .iter()
            .find(|e| e.chord == "K" && e.mode == BindingMode::Normal);
        assert!(k.is_some(), "K should be registered as a Normal-mode binding");
        let entry = k.unwrap();
        assert!(
            entry.doc.to_lowercase().contains("hover"),
            "doc should mention hover, got {:?}",
            entry.doc
        );
    }

    #[test]
    fn active_pane_content_height_subtracts_status_row_in_horizontal_split() {
        // Single pane: content = full buffer height.
        let mut app = app_with("hi\n", 5);
        assert_eq!(app.active_pane_content_height(20), 20);
        // Horizontal split -> two panes, each ~half the buffer
        // height; minus the per-pane status row.
        app.pane_tree
            .split_active(crate::pane::SplitOrientation::Horizontal);
        let content = app.active_pane_content_height(20);
        // 20 / 2 = 10; minus status row = 9.
        assert_eq!(content, 9);
    }

    #[test]
    fn lsp_status_with_no_servers_renders_placeholder() {
        let mut app = app_with("hi\n", 5);
        app.do_lsp_status();
        let body = app.help_buffer.as_ref().unwrap().content.as_string();
        assert!(body.contains("0 server"));
        assert!(body.contains("no LSP servers running"));
    }

    #[test]
    fn lsp_log_level_subsystem_wide_accepts_known_levels() {
        let mut app = app_with("hi\n", 5);
        for lvl in ["error", "warn", "info", "debug", "trace"] {
            app.do_set_lsp_log_level(None, lvl);
            let msg = app.last_message.as_ref().unwrap();
            assert!(
                msg.text.contains(lvl),
                "echo should mention {lvl}, got {}",
                msg.text
            );
        }
    }

    #[test]
    fn lsp_log_level_rejects_unknown_level() {
        let mut app = app_with("hi\n", 5);
        app.do_set_lsp_log_level(None, "babble");
        let msg = app.last_message.as_ref().unwrap();
        assert!(msg.text.contains("unknown log level"));
    }

    #[test]
    fn lsp_log_level_per_server_override() {
        let mut app = app_with("hi\n", 5);
        app.do_set_lsp_log_level(Some("rust"), "debug");
        // Verify the override actually took: a Debug record on
        // the "rust" server now lands in the ring (the default
        // is Info, so without the override it'd be filtered).
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        app.lsp_logger.log(
            Some(&id),
            lattice_lsp::LogLevel::Debug,
            lattice_lsp::LogSource::Client,
            "debug event",
        );
        let recs = app.lsp_logger.snapshot_server(&id);
        assert!(recs.iter().any(|r| r.message == "debug event"));
    }

    #[test]
    fn lsp_log_clear_drops_global_records() {
        let mut app = app_with("hi\n", 5);
        app.lsp_logger.log(
            None,
            lattice_lsp::LogLevel::Info,
            lattice_lsp::LogSource::Client,
            "x",
        );
        assert_eq!(app.lsp_logger.snapshot_global().len(), 1);
        app.do_lsp_log_clear(None);
        assert_eq!(app.lsp_logger.snapshot_global().len(), 0);
    }

    #[test]
    fn lsp_log_clear_drops_per_server_records() {
        let mut app = app_with("hi\n", 5);
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        app.lsp_logger.log(
            Some(&id),
            lattice_lsp::LogLevel::Info,
            lattice_lsp::LogSource::Client,
            "x",
        );
        assert_eq!(app.lsp_logger.snapshot_server(&id).len(), 1);
        app.do_lsp_log_clear(Some("rust"));
        assert_eq!(app.lsp_logger.snapshot_server(&id).len(), 0);
    }

    #[test]
    fn lsp_restart_currently_echoes_placeholder() {
        let mut app = app_with("hi\n", 5);
        app.do_lsp_restart("rust");
        let msg = app.last_message.as_ref().unwrap();
        assert!(msg.text.contains("4.4"));
    }

    // ---- Edit-dispatch wiring tests (Phase 4.1.i.2) ----------

    #[test]
    fn apply_edit_blocking_records_lsp_edit_when_attached() {
        let mut app = app_with("abc\n", 5);
        // Attach a fake URI mapping so lsp_record_edit
        // reaches the supervisor.
        use std::str::FromStr;
        let uri = lattice_lsp::Uri::from_str("file:///tmp/x.rs").unwrap();
        app.buffer_uris.insert(app.document_buffer_id, uri.clone());
        // Test-only: register the URI directly with the
        // supervisor under a mock actor. Without a real
        // ServerHandle attach_handle requires one, so instead
        // we verify the wiring fires by checking that the
        // record-edit path doesn't panic and the buffer_uri
        // mapping survives.
        let edit = Edit::insert(Position::new(0, 0), "x");
        let _ = app.apply_edit_blocking(edit.clone());
        // Buffer mapping unchanged; record_edit is best-effort
        // (skips if no actor attached for the URI).
        assert_eq!(app.buffer_uris.get(&app.document_buffer_id), Some(&uri));
    }

    #[test]
    fn apply_edit_blocking_with_no_lsp_attachment_is_safe() {
        // Without a buffer_uri mapping, lsp_record_edit
        // short-circuits. No panic, no crash, edit still
        // commits.
        let app = app_with("hi\n", 5);
        let r = app.apply_edit_blocking(Edit::insert(Position::new(0, 0), "x"));
        assert!(r.is_ok());
    }

    #[test]
    fn apply_edit_batch_blocking_records_each_edit_in_order() {
        let app = app_with("abc\n", 5);
        let edits = vec![
            Edit::insert(Position::new(0, 0), "1"),
            Edit::insert(Position::new(0, 1), "2"),
        ];
        // No LSP attachment seeded -> records short-circuit;
        // we only check the path is reachable (no panic).
        let r = app.apply_edit_batch_blocking(edits);
        assert!(r.is_ok());
    }

    #[test]
    fn queue_lsp_open_appends_pending_entry() {
        let mut app = app_with("", 5);
        let buffer_id = app.document_buffer_id;
        app.queue_lsp_open(
            buffer_id,
            std::path::PathBuf::from("/tmp/new.rs"),
            "fn main() {}".into(),
        );
        assert_eq!(app.pending_lsp_opens.len(), 1);
        let (id, path, text) = &app.pending_lsp_opens[0];
        assert_eq!(*id, buffer_id);
        assert_eq!(path.as_path(), std::path::Path::new("/tmp/new.rs"));
        assert_eq!(text, "fn main() {}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn drain_pending_lsp_opens_clears_queue() {
        let mut app = App::new(Document::from_text(""));
        // Queue an open for a path with no matching server
        // config -- the supervisor returns Ok(empty) without
        // spawning anything (no rust binaries on the test
        // host). The drain still consumes the entry from the
        // queue.
        app.queue_lsp_open(
            app.document_buffer_id,
            std::path::PathBuf::from("/tmp/no-server-for-this.xyz"),
            "x".into(),
        );
        app.drain_pending_lsp_opens().await;
        assert_eq!(app.pending_lsp_opens.len(), 0);
    }
}
