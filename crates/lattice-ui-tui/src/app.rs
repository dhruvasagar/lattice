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

use lattice_core::Buffer;
use lattice_core::CoreError;
use lattice_core::Document;
use lattice_core::buffer::AppliedEdit;
use fancy_regex::Regex;
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
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range as ProtoRange};
use lattice_protocol::selection::{Selection, SelectionSet, VisualMode};
use lattice_runtime::{
    CancellationToken, DocumentHandle, RuntimeError, block_on, spawn_document,
};
use lattice_syntax::{Lang, StyledSpan, Syntax};

use std::collections::HashMap;
use std::sync::Arc;

use crate::excommand;
use crate::help::{HelpBuffer, HelpDisplayMode, command_link, key_link};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    None,
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
    JoinLines { with_space: bool },
    /// Vim's `;` (no-reverse) and `,` (reverse): repeat the last
    /// f/F/t/T find on the current line.
    FindRepeat { reverse: bool },
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
    /// `"<reg>` prefix -- stash the named register for the next operator
    /// / paste invocation.
    SelectRegister(Register),
    /// Vim's `Ctrl-O` -- step backward in the position history.
    JumpHistoryBack,
    /// Vim's `Ctrl-I` (Tab) -- step forward.
    JumpHistoryForward,
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

    // ---- Help overlay (DESIGN.md §5.11) ----
    // The help overlay is a real buffer with cursor + motions. j/k/h/l/0/$
    // move the cursor; the popup auto-scrolls to keep the cursor in view.
    /// `j` -- cursor down one line.
    HelpCursorDown,
    /// `k` -- cursor up one line.
    HelpCursorUp,
    /// `h` -- cursor left one byte (clamps at line start).
    HelpCursorLeft,
    /// `l` -- cursor right one byte (clamps at line end).
    HelpCursorRight,
    /// `0` -- cursor to start of current line.
    HelpCursorLineStart,
    /// `$` -- cursor to end of current line (vim-style: last char).
    HelpCursorLineEnd,
    /// Half-page motion (Ctrl-D / Ctrl-U). `down = true` moves forward.
    /// Cursor follows; auto-scroll keeps it visible.
    HelpHalfPage { down: bool },
    /// Jump to top (`gg`) / bottom (`G`) of the active help overlay.
    HelpJumpTop,
    HelpJumpBottom,
    /// Close the active help overlay (Esc / q).
    HelpDismiss,

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

pub struct App {
    /// Handle to the per-document actor (DESIGN.md §5.2.1, §5.7).
    /// The actor owns the writable [`Document`]; mutations route
    /// through it; reads load a versioned snapshot.
    pub document: DocumentHandle,
    pub cursor: Position,
    /// First visible line in the viewport (0-based).
    pub scroll: u32,
    pub should_quit: bool,
    /// Last height we were drawn at; used by motion clamping and viewport
    /// scrolling. Updated by the renderer before each frame.
    pub viewport_height: u32,
    pub modal: ModalState,
    pub pending: Pending,
    /// Grammar registry shared with the document actor by `Arc`. The
    /// actor calls `lattice_grammar::execute` with this registry from
    /// inside its own task. The App also reads it directly for the
    /// parser, completion pipeline, and introspection -- all
    /// read-only operations.
    pub registry: Arc<CommandRegistry>,
    pub builtins: Builtins,
    /// In-progress text in the `:` minibuffer. Populated only while
    /// `modal == ModalState::Command`.
    pub command_line: String,
    /// Most recent transient status / error message, displayed in the echo
    /// area until replaced.
    pub last_message: Option<EchoMessage>,
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
    /// `:set number` / `:set nonumber`. Default true.
    pub show_line_numbers: bool,
    /// `:set relativenumber` / `:set norelativenumber`. Default false.
    /// When true, the gutter shows distance from the cursor on each
    /// line; the cursor's line shows its absolute number.
    pub relative_line_numbers: bool,
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
    /// One-shot "auto-submit on next chord" flag. Set when the
    /// user submitted a Chord-arg-required command with no value
    /// (`:describe-key<CR>`); the cmdline pre-fills with the
    /// command word + space, and the very next captured chord
    /// auto-fires [`Action::CommandLineSubmit`] without an
    /// explicit `<CR>`. Reset on cancel / submit.
    pub auto_submit_after_chord: bool,
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
            Self::NoCompletionForArg(name) => (
                EchoLevel::Info,
                format!("no completion for arg `{name}`"),
            ),
            Self::NoCompletionAtCursor => {
                (EchoLevel::Info, "no completion at cursor".to_string())
            }
            Self::MissingSource(name) => (
                EchoLevel::Error,
                format!("completion source `{name}` not registered"),
            ),
            Self::PipelineUnconfigured => (
                EchoLevel::Error,
                "completion pipeline not configured (missing default matcher / ranker)"
                    .to_string(),
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

#[derive(Debug, Clone, Copy)]
pub struct Fold {
    pub start_line: u32,
    pub end_line: u32,
    pub closed: bool,
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

/// One entry in the unified position history (§5.1.1). The `document`
/// and `timestamp` fields the spec mentions are omitted in v1 since
/// the TUI runs against a single document; they re-enter when
/// multi-buffer support arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionEntry {
    pub position: Position,
    pub source: PositionSource,
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
        let lang = Lang::detect_from_path(document.path());
        let mut syntax = Syntax::for_language(lang).ok().flatten();
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
        Self {
            document,
            cursor: Position::ZERO,
            scroll: 0,
            should_quit: false,
            viewport_height: 1,
            modal: ModalState::Normal,
            pending: Pending::None,
            registry,
            builtins,
            command_line: String::new(),
            last_message: None,
            syntax,
            last_parsed_text_version,
            visible_highlights: Vec::new(),
            search_line: None,
            last_search: None,
            current_match: None,
            all_matches: Vec::new(),
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
            show_line_numbers: true,
            relative_line_numbers: false,
            command_history: Vec::new(),
            command_history_cursor: None,
            command_history_pending: None,
            help_buffer: None,
            help_display_mode: HelpDisplayMode::default(),
            completion_registry,
            completion_state: None,
            auto_submit_after_chord: false,
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
    /// before this returns.
    pub fn apply_edit_blocking(&self, edit: Edit) -> Result<AppliedEdit, RuntimeError> {
        block_on(self.document.apply_edit(edit))
    }

    /// Block_on `apply_edit_batch`. The batch lands as one undo
    /// unit on the document's undo stack.
    pub fn apply_edit_batch_blocking(
        &self,
        edits: Vec<Edit>,
    ) -> Result<Vec<AppliedEdit>, RuntimeError> {
        block_on(self.document.apply_edit_batch(edits))
    }

    pub fn undo_blocking(&self) -> Result<Vec<AppliedEdit>, RuntimeError> {
        block_on(self.document.undo())
    }

    pub fn redo_blocking(&self) -> Result<Vec<AppliedEdit>, RuntimeError> {
        block_on(self.document.redo())
    }

    pub fn save_blocking(&self) -> Result<std::path::PathBuf, RuntimeError> {
        block_on(self.document.save())
    }

    pub fn save_as_blocking(&self, path: std::path::PathBuf) -> Result<(), RuntimeError> {
        block_on(self.document.save_as(path))
    }

    pub fn set_selections_blocking(&self, selections: SelectionSet) {
        // SetSelections only fails on actor-gone; ignore the
        // Result (post-shutdown nothing meaningful to do).
        let _ = block_on(self.document.set_selections(selections));
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
    pub fn dispatch_blocking(
        &self,
        invocation: CommandInvocation,
    ) -> Result<Effect, RuntimeError> {
        block_on(self.document.dispatch_with_cancel(
            invocation,
            self.cursor,
            CancellationToken::never(),
        ))
    }

    pub fn apply(&mut self, action: Action) {
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
        match action {
            Action::None => {}
            Action::Quit => self.should_quit = true,
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
                self.help_buffer = None;
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
                }
            }
            Action::CommandLineBackspace => {
                if matches!(self.modal, ModalState::Command) {
                    if self.command_line.pop().is_none() {
                        // Empty buffer + backspace -> exit Command modal.
                        self.modal = ModalState::Normal;
                        self.completion_state = None;
                    } else if self.completion_state.is_some() {
                        // Popup live-refilters against the shorter
                        // prefix (vertico-style).
                        self.refresh_completion_popup();
                    }
                }
            }
            Action::CommandLineSubmit => {
                if matches!(self.modal, ModalState::Command) {
                    // Missing-arg prompt path (DESIGN.md §B.1):
                    // if the user submitted with a Chord-required
                    // arg empty (`:describe-key<CR>`), don't fail
                    // -- prefill the cmdline with the command
                    // word + space (cursor lands in the chord-arg
                    // slot, chord-capture auto-activates) and arm
                    // a one-shot auto-submit. The very next
                    // captured chord runs the lookup, no second
                    // <CR> required.
                    if let Some(prefill) = self.try_arm_missing_chord_prompt() {
                        self.command_line = prefill;
                        self.auto_submit_after_chord = true;
                        self.set_message(
                            EchoLevel::Info,
                            "key: press the chord to look up".to_string(),
                        );
                        return;
                    }
                    let line = std::mem::take(&mut self.command_line);
                    self.modal = ModalState::Normal;
                    self.pending = Pending::None;
                    self.command_history_cursor = None;
                    self.command_history_pending = None;
                    self.auto_submit_after_chord = false;
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
                }
            }
            Action::CommandLineHistoryPrev => self.do_command_history_step(true),
            Action::CommandLineHistoryNext => self.do_command_history_step(false),
            Action::Echo(message) => {
                self.last_message = Some(message);
            }

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
            Action::SearchWordUnderCursor(direction) => {
                self.do_search_word_under_cursor(direction)
            }
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
            Action::SelectRegister(reg) => {
                self.pending_register = Some(reg);
            }
            Action::JumpHistoryBack => self.do_jump_history(-1),
            Action::JumpHistoryForward => self.do_jump_history(1),
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
                    self.set_message(
                        EchoLevel::Error,
                        "no previous change to repeat".to_string(),
                    );
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
            Action::CommandLineDescribeUnderCursor => {
                self.do_command_line_describe_under_cursor()
            }
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

            Action::HelpCursorDown => self.do_help_move_cursor(0, 1),
            Action::HelpCursorUp => self.do_help_move_cursor(0, -1),
            Action::HelpCursorLeft => self.do_help_move_cursor(-1, 0),
            Action::HelpCursorRight => self.do_help_move_cursor(1, 0),
            Action::HelpCursorLineStart => {
                if let Some(h) = self.help_buffer.as_mut() {
                    h.cursor_line_start();
                }
            }
            Action::HelpCursorLineEnd => {
                if let Some(h) = self.help_buffer.as_mut() {
                    h.cursor_line_end();
                }
            }
            Action::HelpHalfPage { down } => self.do_help_half_page(down),
            Action::HelpJumpTop => {
                if let Some(h) = self.help_buffer.as_mut() {
                    h.jump_top();
                }
            }
            Action::HelpJumpBottom => {
                let viewport = self.help_bufferport_height();
                if let Some(h) = self.help_buffer.as_mut() {
                    h.jump_bottom(viewport);
                }
            }
            Action::HelpDismiss => {
                self.help_buffer = None;
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
    }

    /// Recompute the per-line styled spans for the current viewport.
    /// Called by the runtime before each `terminal.draw`.
    pub fn refresh_highlights(&mut self) {
        let height = self.viewport_height;
        let start = self.scroll;
        let end = start.saturating_add(height);
        self.visible_highlights = match self.syntax.as_mut() {
            Some(syntax) => syntax.highlight_lines(start, end).unwrap_or_default(),
            None => Vec::new(),
        };
    }

    /// Spans for the line at `viewport_row` (0-based, relative to the top of
    /// the viewport). Empty slice if no syntax or the row is past EOF.
    pub fn highlights_for_viewport_row(&self, viewport_row: u32) -> &[StyledSpan] {
        self.visible_highlights
            .get(viewport_row as usize)
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
        match search::find(
            &self.document.snapshot().buffer,
            &regex,
            line.origin,
            dir,
            &CancellationToken::never(),
        ) {
            Ok(Some(SearchHit { range, .. })) => self.current_match = Some(range),
            _ => self.current_match = None,
        }
        // Live hlsearch: highlight every occurrence as the user types.
        self.all_matches = search::find_all(
            &self.document.snapshot().buffer,
            &regex,
            &CancellationToken::never(),
        )
        .unwrap_or_default();
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
        // Save the pre-search position so Ctrl-O returns.
        let from_pos = line.origin;
        self.push_position_history(from_pos, PositionSource::AutoJump);
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
        match search::find(
            &self.document.snapshot().buffer,
            &regex,
            line.origin,
            dir,
            &CancellationToken::never(),
        ) {
            Ok(Some(hit)) => {
                self.cursor = hit.range.start;
                self.current_match = Some(hit.range);
                self.all_matches = search::find_all(
                    &self.document.snapshot().buffer,
                    &regex,
                    &CancellationToken::never(),
                )
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
            }
            Ok(None) => {
                self.current_match = None;
                self.all_matches.clear();
                self.set_message(EchoLevel::Error, format!("E486: Pattern not found: {}", line.pattern));
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
            self.set_message(EchoLevel::Error, "E35: no previous regular expression".to_string());
            return;
        };
        // Push pre-jump position so Ctrl-O can return.
        let cur = self.cursor;
        self.push_position_history(cur, PositionSource::AutoJump);
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
        // Skip current match: advance one byte in the chosen direction.
        let from = step_byte(&self.document.snapshot().buffer, self.cursor, direction);
        let regex = match compile_search_pattern(&last.pattern) {
            Ok(r) => r,
            Err(msg) => {
                self.set_message(EchoLevel::Error, format!("regex: {msg}"));
                self.current_match = None;
                return;
            }
        };
        match search::find(
            &self.document.snapshot().buffer,
            &regex,
            from,
            dir,
            &CancellationToken::never(),
        ) {
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
            }
            Ok(None) => {
                self.current_match = None;
                self.set_message(EchoLevel::Error, format!("E486: Pattern not found: {}", last.pattern));
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
        let slot = lattice_completion::current_slot(
            &line,
            cursor,
            &self.registry,
            &alias_resolver,
        );

        // Word-at-cursor: try to resolve to a registered command.
        let word = slot.prefix();
        let canonical = if word.is_empty() {
            None
        } else {
            // Try alias resolution; fall through to direct registry
            // name lookup.
            alias_resolver(word).or_else(|| {
                self.registry
                    .id_by_name(word)
                    .and(Some(word.to_string()))
            })
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
                    self.set_message(
                        EchoLevel::Error,
                        format!("no command named `{prefix}`"),
                    );
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
        self.command_line
            .replace_range(state.replace_start..self.command_line.len(), &chosen.raw.text);
    }

    /// On `Action::CommandLineSubmit`, decide whether the line is
    /// an empty-arg invocation of a command whose first required
    /// arg is `Chord`. If so, return the prefill string for the
    /// cmdline (`<command-word> ` -- with trailing space) so the
    /// caller can transition into a chord-capture prompt.
    /// `None` means submit normally.
    fn try_arm_missing_chord_prompt(&self) -> Option<String> {
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
        let canonical = self
            .registry
            .id_by_name(cmd)
            .or_else(|| {
                crate::excommand::aliases()
                    .get(cmd)
                    .copied()
                    .and_then(|c| self.registry.id_by_name(c))
            })?;
        let spec = self.registry.ex_command_spec(canonical)?;
        let first = spec.args_schema.first()?;
        if first.kind != lattice_grammar::ArgKind::Chord {
            return None;
        }
        if !matches!(first.default, lattice_grammar::ArgDefault::Required) {
            // Non-required chord arg has a fallback; let the
            // parser take the default path.
            return None;
        }
        // Preserve the user's spelling (alias vs canonical) plus
        // any bang they typed; just append the trailing space so
        // the cursor lands in the chord-arg slot.
        Some(format!("{raw_cmd} "))
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
        let slot = lattice_completion::current_slot(
            line,
            line.len(),
            &self.registry,
            &alias_resolver,
        );
        matches!(
            &slot,
            lattice_completion::CommandLineSlot::Arg { arg_spec, .. }
                if arg_spec.kind == lattice_grammar::ArgKind::Chord
        )
    }

    /// Build the pipeline for the current slot and run it. Caches
    /// results into `completion_state`.
    fn open_completion_popup(&mut self) {
        match self.compute_completion_state() {
            Ok(state) => {
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
    fn compute_completion_state(
        &self,
    ) -> Result<CompletionState, CompletionComputeError> {
        let line = self.command_line.clone();
        let cursor = line.len();
        let alias_resolver = |short: &str| {
            crate::excommand::aliases()
                .get(short)
                .map(|s| (*s).to_string())
        };
        let slot = lattice_completion::current_slot(
            &line,
            cursor,
            &self.registry,
            &alias_resolver,
        );
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
            lattice_completion::CommandLineSlot::Empty => {
                ("gen:commands", String::new(), 0)
            }
            _ => {
                return Err(CompletionComputeError::NoCompletionAtCursor);
            }
        };

        let Some(generator) = self.completion_registry.generator_by_name(source_name) else {
            return Err(CompletionComputeError::MissingSource(source_name.to_string()));
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
        // Re-initialise syntax for the new doc's language.
        let lang = Lang::detect_from_path(new_doc.path());
        let mut syntax = Syntax::for_language(lang).ok().flatten();
        if let Some(s) = syntax.as_mut() {
            s.parse(&new_doc.text());
        }
        self.last_parsed_text_version = new_doc.text_version();
        self.syntax = syntax;
        self.replace_document_blocking(new_doc);
        // Per-document state resets (vim's behavior).
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
        // Registers, marks, macros, and view options persist.
        self.set_message(
            EchoLevel::Info,
            format!("\"{}\" opened", target.display()),
        );
    }

    /// Vim's `:set <option>`. v1 honors a tiny fixed set; everything
    /// else surfaces as an error rather than silently no-op'ing so the
    /// user gets clear feedback.
    fn do_set(&mut self, option: &str) {
        match option {
            "number" | "nu" => self.show_line_numbers = true,
            "nonumber" | "nonu" => self.show_line_numbers = false,
            "relativenumber" | "rnu" => {
                self.show_line_numbers = true;
                self.relative_line_numbers = true;
            }
            "norelativenumber" | "nornu" => self.relative_line_numbers = false,
            other => {
                self.set_message(EchoLevel::Error, format!("unknown option: {other}"));
            }
        }
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
        let mut entries: Vec<(char, Position)> = self
            .marks
            .iter()
            .map(|(c, p)| (*c, *p))
            .collect();
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
            self.cursor = Position::new(line.min(last_addressable_line(&self.document.snapshot().buffer)), 0);
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
                format!("no lines {} pattern: {pattern}", if inverted { "lacking" } else { "matching" }),
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
            let r = ProtoRange::new(
                Position::new(line, 0),
                Position::new(line, line_len),
            );
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
            Some(p) => self.save_as_blocking(p.clone()).map(|()| p.display().to_string()),
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
        self.should_quit = true;
    }

    // `:wq` / `:x` are now Effect::Many([SaveBuffer, QuitEditor{force}])
    // composed in `lattice_grammar::ex_commands::apply_write_quit`. The
    // do_write + do_quit pair runs in sequence via apply_effect; the
    // quit's force-bit comes from the trailing `!` (DESIGN.md §5.2.1).

    fn run_invocation(&mut self, mut inv: CommandInvocation) {
        // Pending state is consumed by the input layer that built `inv`; any
        // dispatch resets it.
        self.pending = Pending::None;
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
        if final_count > 1 {
            inv = inv.with_count(lattice_grammar::command::Count(final_count));
        }
        self.pending_count = 0;
        self.op_count = 0;
        let was_visual = matches!(self.modal, ModalState::Visual(_));
        let mut should_exit_visual = false;
        let inv_for_repeat = inv.clone();
        match self.dispatch_blocking(inv) {
            Ok(effect) => {
                // Visual exits on any operator-class effect (mutation OR
                // yank-only); dot-repeat only records buffer mutations.
                should_exit_visual = effect_mutates_or_yanks(&effect);
                if effect_mutates(&effect) {
                    self.last_change = Some(inv_for_repeat);
                }
                self.apply_effect(effect);
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
            Effect::Echo {
                level,
                text,
            } => self.set_message(echo_level_from_grammar(level), text),
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
        let last = last_addressable_line(&self.document.snapshot().buffer);
        let line = line.min(last);
        let len = line_byte_len(&self.document.snapshot().buffer, line);
        let byte = self.cursor.byte.min(len);
        self.cursor = Position::new(line, byte);
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
        let last = last_addressable_line(&self.document.snapshot().buffer);
        let new_line = if down {
            self.cursor.line.saturating_add(step).min(last)
        } else {
            self.cursor.line.saturating_sub(step)
        };
        let len = line_byte_len(&self.document.snapshot().buffer, new_line);
        let byte = self.cursor.byte.min(len);
        self.cursor = Position::new(new_line, byte);
    }

    /// Scroll one line. `down = true` -> Ctrl-E (scroll content up,
    /// pulling the next line into view); `down = false` -> Ctrl-Y.
    /// Cursor follows so it stays on-screen.
    fn do_scroll_line(&mut self, down: bool) {
        let height = self.viewport_height.max(1);
        if down {
            let last = last_addressable_line(&self.document.snapshot().buffer);
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
        let len = line_byte_len(&self.document.snapshot().buffer, self.cursor.line);
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
        let mut buffer = HelpBuffer::from_lines_and_anchors(
            format!("describe-command {name}"),
            rendered.lines,
            anchors,
        );
        if let Some(a) = anchor {
            buffer.scroll_to_anchor(a);
        }
        self.help_buffer = Some(buffer);
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
        let dirty = if self.document.dirty() {
            "yes"
        } else {
            "no"
        };
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
            self.show_line_numbers, self.relative_line_numbers
        ));
        self.help_buffer = Some(HelpBuffer::from_lines("describe-buffer", lines));
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
        self.help_buffer = Some(HelpBuffer::from_lines(format!("apropos {pattern}"), lines));
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
            lines.push(format!(
                "{} -- {} binding(s):",
                key_link(chord),
                hits.len()
            ));
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
        self.help_buffer = Some(HelpBuffer::from_lines(format!("describe-key {chord}"), lines));
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
        self.help_buffer = Some(HelpBuffer::from_lines("keymap", lines));
    }

    /// Estimated help-overlay viewport height. The overlay is centred
    /// over the buffer area with a 2-row chrome (border + title), so
    /// the visible content rows are `buffer_rows - 2 - 4` (the outer
    /// 4 is the centring margin). Floor at 1 so scroll math is well-
    /// defined when the terminal is tiny.
    fn help_bufferport_height(&self) -> usize {
        // Approximate: assume the popup fills ~70% of the buffer area
        // vertically (matches `draw_help_overlay`). The render layer
        // re-clamps if the terminal is smaller.
        let buffer = self.viewport_height as usize;
        let popup = (buffer * 7 / 10).saturating_sub(2); // 2 = top+bottom border
        popup.max(1)
    }

    fn do_help_move_cursor(&mut self, dx: i32, dy: i32) {
        let viewport = self.help_bufferport_height();
        if let Some(h) = self.help_buffer.as_mut() {
            h.move_cursor(dx, dy, viewport);
        }
    }

    fn do_help_half_page(&mut self, down: bool) {
        let viewport = self.help_bufferport_height();
        if let Some(h) = self.help_buffer.as_mut() {
            if down {
                h.half_page_down(viewport);
            } else {
                h.half_page_up(viewport);
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
            self.set_message(EchoLevel::Error, format!("invalid macro register: {register}"));
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
            self.set_message(EchoLevel::Error, format!("invalid macro register: {register}"));
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
        if let Some(last) = self.position_history.last()
            && last.position == pos
            && last.source == source
        {
            return;
        }
        if self.position_history_cursor < self.position_history.len() {
            self.position_history.truncate(self.position_history_cursor);
        }
        self.position_history.push(PositionEntry { position: pos, source });
        if self.position_history.len() > POSITION_HISTORY_CAP {
            self.position_history.remove(0);
            // Truncating from the front shifts the cursor too; clamp
            // before we re-anchor it.
            self.position_history_cursor =
                self.position_history_cursor.saturating_sub(1);
        }
        self.position_history_cursor = self.position_history.len();
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
            let already_there = self
                .position_history
                .last()
                .map(|e| e.position == self.cursor)
                .unwrap_or(false);
            if !already_there {
                let cur = self.cursor;
                self.push_position_history(cur, PositionSource::AutoJump);
                // After push the cursor==len. Step it one back so the
                // walk finds the entry preceding our snapshot rather
                // than the snapshot itself.
                self.position_history_cursor =
                    self.position_history.len().saturating_sub(1);
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
        let target_idx = if delta < 0 {
            self.position_history[..self.position_history_cursor]
                .iter()
                .rposition(&pred)
        } else {
            let from = self
                .position_history_cursor
                .saturating_add(1)
                .min(self.position_history.len());
            self.position_history[from..]
                .iter()
                .position(&pred)
                .map(|i| i + from)
        };
        let Some(idx) = target_idx else {
            let bound = if delta < 0 { "start" } else { "end" };
            self.set_message(
                EchoLevel::Error,
                format!("at {bound} of {bound_label}"),
            );
            return;
        };
        self.position_history_cursor = idx;
        self.cursor = self.position_history[idx].position;
        self.clamp_cursor_to_buffer();
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
            self.set_message(EchoLevel::Error, "zf requires a Visual selection".to_string());
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
        });
        // Exit Visual back to Normal at the fold start.
        self.cursor = Position::new(start_line, 0);
        self.do_exit_visual();
    }

    /// Toggle / open / close the fold containing the cursor. `state =
    /// None` toggles; `Some(true)` closes; `Some(false)` opens.
    fn do_set_fold_state_at_cursor(&mut self, state: Option<bool>) {
        let line = self.cursor.line;
        for fold in self.folds.iter_mut() {
            if line >= fold.start_line && line <= fold.end_line {
                fold.closed = match state {
                    None => !fold.closed,
                    Some(s) => s,
                };
                return;
            }
        }
        // No fold here: if state was an explicit close-request, that's
        // a no-op (vim says "No fold found" -- we silently ignore).
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
        self.folds
            .retain(|f| !(line >= f.start_line && line <= f.end_line));
    }

    /// Returns true if `line` is inside a closed fold (and not the fold
    /// start, which is rendered as the summary). The renderer uses this
    /// to skip lines.
    pub fn line_inside_closed_fold(&self, line: u32) -> bool {
        self.folds
            .iter()
            .any(|f| f.closed && line > f.start_line && line <= f.end_line)
    }

    /// Returns Some(fold) if `line` is the start of a closed fold; the
    /// renderer renders the summary header instead of the line content.
    pub fn fold_start_at(&self, line: u32) -> Option<&Fold> {
        self.folds
            .iter()
            .find(|f| f.closed && f.start_line == line)
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
        let range = ProtoRange::new(
            Position::new(line, cur_len),
            Position::new(next_line, trim),
        );
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
        let inv = CommandInvocation::of(motion_id.0)
            .with_args(lattice_grammar::Args::Char(last.target));
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
        let cursor_byte = match self.document.snapshot().buffer.position_to_byte(self.cursor) {
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
            SearchDirection::Forward => step_byte(&self.document.snapshot().buffer, self.cursor, direction),
            SearchDirection::Backward => step_byte(&self.document.snapshot().buffer, self.cursor, direction),
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
                self.set_message(
                    EchoLevel::Error,
                    format!("E486: Pattern not found: {word}"),
                );
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
        let cursor_byte = match self.document.snapshot().buffer.position_to_byte(self.cursor) {
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
    }

    fn do_reselect_visual(&mut self) {
        let Some(last) = self.last_visual else {
            self.set_message(
                EchoLevel::Error,
                "no previous visual selection".to_string(),
            );
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
                if let Ok(applied) =
                    self.apply_edit_blocking(Edit::insert(insert_at, &reg.content))
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
                if let Ok(applied) =
                    self.apply_edit_blocking(Edit::insert(insert_at, &payload))
                {
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
                let _ = self.apply_edit_blocking(Edit::insert(
                    Position::new(last, last_len),
                    "\n",
                ));
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

    fn clamp_cursor_to_buffer(&mut self) {
        let last_line = last_addressable_line(&self.document.snapshot().buffer);
        if self.cursor.line > last_line {
            self.cursor.line = last_line;
        }
        let len = line_byte_len(&self.document.snapshot().buffer, self.cursor.line);
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

    pub fn set_viewport_height(&mut self, height: u32) {
        self.viewport_height = height.max(1);
        self.ensure_cursor_visible();
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
    let escaped: String = s.chars().map(|c| if c == '\n' { '\u{21B5}' } else { c }).collect();
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
        | Effect::ListKeymap => false,
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
        | Effect::ListKeymap => false,
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

    fn invoke_motion(id: lattice_grammar::registry::MotionId) -> Action {
        Action::Invoke(CommandInvocation::of(id.0))
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
        });
        a.folds.push(Fold {
            start_line: 2,
            end_line: 3,
            closed: true,
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
        });
        a.folds.push(Fold {
            start_line: 2,
            end_line: 3,
            closed: false,
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
        });
        a.cursor = Position::new(1, 0);
        a.apply(Action::DeleteFoldAtCursor);
        assert!(a.folds.is_empty());
    }

    #[test]
    fn zj_jumps_to_next_fold_start() {
        let mut a = app_with("a\nb\nc\nd\ne\nf", 10);
        a.folds.push(Fold {
            start_line: 2,
            end_line: 3,
            closed: false,
        });
        a.folds.push(Fold {
            start_line: 5,
            end_line: 5,
            closed: false,
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
        assert!(a.show_line_numbers);
        submit_ex(&mut a, "set nonumber");
        assert!(!a.show_line_numbers);
        submit_ex(&mut a, "set number");
        assert!(a.show_line_numbers);
    }

    #[test]
    fn set_relativenumber_toggles_flag() {
        let mut a = app_with("hello\nworld", 10);
        assert!(!a.relative_line_numbers);
        submit_ex(&mut a, "set relativenumber");
        assert!(a.relative_line_numbers);
        assert!(a.show_line_numbers);
        submit_ex(&mut a, "set norelativenumber");
        assert!(!a.relative_line_numbers);
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
        let sels = a.document.selections(); let sel = sels.primary();
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
        let inv = CommandInvocation::of(a.builtins.yank.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.modal, ModalState::Normal);
        a.apply(Action::ReselectLastVisual);
        assert_eq!(a.modal, ModalState::Visual(VisualKind::Charwise));
        let sels = a.document.selections(); let sel = sels.primary();
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
        let sels = a.document.selections(); let sel = sels.primary();
        assert_eq!(sel.anchor, Position::new(0, 1));
        assert_eq!(sel.head, Position::new(0, 1));
        assert_eq!(sel.visual, Some(VisualMode::Charwise));
    }

    #[test]
    fn motion_in_visual_extends_head_keeps_anchor() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterVisual(VisualKind::Charwise));
        a.apply(invoke_motion(a.builtins.word_forward));
        let sels = a.document.selections(); let sel = sels.primary();
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
        let inv = CommandInvocation::of(a.builtins.yank.0)
            .with_range(lattice_grammar::Range::Selection);
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
        let inv = CommandInvocation::of(a.builtins.yank.0)
            .with_range(lattice_grammar::Range::Selection);
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
        let inv = CommandInvocation::of(a.builtins.yank.0)
            .with_range(lattice_grammar::Range::Selection);
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
        let sels = a.document.selections(); let sel = sels.primary();
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
        a.apply(Action::SetPending(Pending::AfterOperator(a.builtins.delete)));
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
        a.apply(Action::SetPending(Pending::AfterOperator(a.builtins.delete)));
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
        a.apply(Action::SetPending(Pending::AfterOperator(a.builtins.delete)));
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
        a.apply(Action::SetPending(Pending::AfterOperator(a.builtins.indent_right)));
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
        a.apply(Action::SetPending(Pending::AfterOperator(a.builtins.indent_left)));
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
        a.apply(Action::EnterSearch(lattice_grammar::SearchDirection::Forward));
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
        let mut a = enter_block_visual(
            "abcd\n1234\nWXYZ",
            Position::new(0, 1),
            Position::new(2, 2),
        );
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.document.text(), "ad\n14\nWZ");
    }

    #[test]
    fn block_yank_stores_blockwise_content_in_unnamed_register() {
        // Yank a 3x2 rectangle: cols 1..=2 across three rows of "abcd\n1234\nWXYZ".
        let mut a = enter_block_visual(
            "abcd\n1234\nWXYZ",
            Position::new(0, 1),
            Position::new(2, 2),
        );
        let inv = CommandInvocation::of(a.builtins.yank.0)
            .with_range(lattice_grammar::Range::Selection);
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
        let mut a = enter_block_visual(
            "abcd\n12\nWXYZ",
            Position::new(0, 1),
            Position::new(2, 2),
        );
        let inv = CommandInvocation::of(a.builtins.yank.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        let reg = a.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.content, "bc\n2\nXY");
        assert_eq!(reg.kind, YankKind::Blockwise);
    }

    #[test]
    fn block_yank_with_row_entirely_left_of_rectangle_yields_empty_slice() {
        // Middle row is "" (empty). Visual cols 1..=2 fully outside;
        // intersection is empty.
        let mut a = enter_block_visual(
            "abcd\n\nWXYZ",
            Position::new(0, 1),
            Position::new(2, 2),
        );
        let inv = CommandInvocation::of(a.builtins.yank.0)
            .with_range(lattice_grammar::Range::Selection);
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
        let mut a = enter_block_visual(
            "abc\n123\nWXY",
            Position::new(0, 1),
            Position::new(2, 1),
        );
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
        let mut a = enter_block_visual(
            "abcd\n1234\nWXYZ",
            Position::new(0, 1),
            Position::new(2, 2),
        );
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
        let mut a = enter_block_visual(
            "abcd\n1234\nWXYZ",
            Position::new(0, 1),
            Position::new(2, 2),
        );
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
        let mut a = enter_block_visual(
            "abcd\n1234\nWXYZ",
            Position::new(0, 1),
            Position::new(2, 2),
        );
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
        let mut a = enter_block_visual(
            "abcd\n1234\nWXYZ",
            Position::new(0, 1),
            Position::new(2, 2),
        );
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
        let mut a = enter_block_visual(
            "abcd\n12\nWXYZ",
            Position::new(0, 3),
            Position::new(2, 3),
        );
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
        let mut a = enter_block_visual(
            "abcd\n1234\nWXYZ",
            Position::new(0, 1),
            Position::new(2, 2),
        );
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
        let yank = CommandInvocation::of(a.builtins.yank.0)
            .with_range(lattice_grammar::Range::Selection);
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
        // §5.11: every :describe-* must surface a [[file:...]] link
        // so the user can jump to where the thing was registered.
        // Built-in commands record their source via #[track_caller]
        // when populate() runs.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:write".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.help_buffer.as_ref().unwrap().content.as_string();
        assert!(
            body.contains("Defined at:"),
            "body should label the source: {body}"
        );
        assert!(
            body.contains("[[file:") && body.contains("ex_commands.rs"),
            "body should contain a file link to ex_commands.rs: {body}"
        );
        assert!(
            body.contains("(built-in)"),
            "body should label the source layer: {body}"
        );
    }

    #[test]
    fn describe_command_link_is_extracted_by_help_link_parser() {
        // The HelpBuffer constructor runs parse_help_links over the
        // body so the [[file:...]] markup becomes a HelpLink with
        // a Source target -- ready for the styled-link renderer +
        // follow-link motion (post-1.0).
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:quit".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        let source_link = h.links.iter().find(|l| {
            matches!(l.target, crate::help::HelpLinkTarget::Source { .. })
        });
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
        let body = a.help_buffer.as_ref().unwrap().content.as_string();
        assert!(
            body.contains("Bound at:"),
            "describe-key output missing `Bound at:`: {body}"
        );
        assert!(
            body.contains("[[file:") && body.contains("keymap.rs"),
            "describe-key output missing source link: {body}"
        );
        assert!(
            body.contains("(built-in)"),
            "describe-key output missing source-layer label: {body}"
        );
    }

    #[test]
    fn describe_key_renders_command_cross_reference_links() {
        // For `j`, three Normal/Visual/Help bindings -- the first
        // two have a `command` and should produce [[command:...]]
        // cross-reference links.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-key j".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.help_buffer.as_ref().unwrap().content.as_string();
        assert!(
            body.contains("[[command:motion:line-down]]"),
            "expected [[command:motion:line-down]] cross-reference: {body}"
        );
    }

    #[test]
    fn describe_key_each_binding_has_its_own_source_link() {
        // `j` has 3 bindings (Normal, Visual, Help) -- each should
        // surface its own [[file:...]] line because every
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
            3,
            "expected 3 source links (one per binding); got {}: {:?}",
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
            3,
            "expected 3 distinct source line numbers; got {lines:?}",
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
        assert!(state.candidates.iter().any(|c| c.raw.text == "describe-command"));
        assert!(state.candidates.iter().any(|c| c.raw.text == "describe-buffer"));
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
            a.command_line.starts_with("describe-")
                || a.command_line == "apropos",
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
        let state = a.completion_state.as_ref().expect("popup must stay open on no-match");
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
    fn empty_submit_of_describe_command_does_not_arm_prompt() {
        // describe-command's arg is String, not Chord -- normal
        // missing-arg behaviour (parser errors).
        let mut a = app_in_command_mode("describe-command");
        a.apply(Action::CommandLineSubmit);
        assert!(!a.auto_submit_after_chord);
        // Cmdline submitted (and errored); modal is back to Normal.
        assert!(matches!(a.modal, ModalState::Normal));
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
            state.candidates.iter().any(|c| c.raw.text.starts_with("motion:")),
            "expected motion:* candidates: {:?}",
            state.candidates.iter().map(|c| &c.raw.text).collect::<Vec<_>>()
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
        a.help_buffer = Some(crate::help::HelpBuffer::from_lines("preexisting", vec!["x".into()]));
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

    #[test]
    fn help_dismiss_clears_overlay() {
        let mut a = app_with("xx", 10);
        a.help_buffer = Some(HelpBuffer::from_lines("test", vec!["a".into(), "b".into()]));
        a.apply(Action::HelpDismiss);
        assert!(a.help_buffer.is_none());
    }

    #[test]
    fn help_cursor_down_advances_then_pulls_scroll() {
        let mut a = app_with("xx", 10);
        let lines: Vec<String> = (0..50).map(|i| format!("line-{i}")).collect();
        a.help_buffer = Some(HelpBuffer::from_lines("scroll-test", lines));
        // viewport_height is 10; help viewport math caps at 10*7/10 - 2 = 5.
        for _ in 0..3 {
            a.apply(Action::HelpCursorDown);
        }
        let h = a.help_buffer.as_ref().unwrap();
        assert_eq!(h.cursor.line, 3);
        // Cursor still inside the 5-row viewport -> no scroll yet.
        assert_eq!(h.scroll, 0);
    }

    #[test]
    fn help_cursor_clamps_at_last_line() {
        let mut a = app_with("xx", 10);
        let lines: Vec<String> = (0..50).map(|i| format!("line-{i}")).collect();
        a.help_buffer = Some(HelpBuffer::from_lines("scroll-test", lines));
        for _ in 0..1000 {
            a.apply(Action::HelpCursorDown);
        }
        let h = a.help_buffer.as_ref().unwrap();
        assert_eq!(h.cursor.line, 49);
        // Scroll keeps cursor on screen: line 49 + 1 - 5 = 45.
        assert_eq!(h.scroll, 45);
    }

    #[test]
    fn help_cursor_up_clamps_at_zero() {
        let mut a = app_with("xx", 10);
        a.help_buffer = Some(HelpBuffer::from_lines("scroll-test", vec!["a".into(); 30]));
        for _ in 0..1000 {
            a.apply(Action::HelpCursorUp);
        }
        let h = a.help_buffer.as_ref().unwrap();
        assert_eq!(h.cursor.line, 0);
        assert_eq!(h.scroll, 0);
    }

    #[test]
    fn help_cursor_horizontal_motion_within_line() {
        let mut a = app_with("xx", 10);
        a.help_buffer = Some(HelpBuffer::from_lines(
            "hl-test",
            vec!["hello world".into()],
        ));
        a.apply(Action::HelpCursorRight);
        a.apply(Action::HelpCursorRight);
        a.apply(Action::HelpCursorRight);
        assert_eq!(a.help_buffer.as_ref().unwrap().cursor.byte, 3);
        a.apply(Action::HelpCursorLeft);
        assert_eq!(a.help_buffer.as_ref().unwrap().cursor.byte, 2);
        a.apply(Action::HelpCursorLineEnd);
        // vim `$`: last char index = len - 1 = 10.
        assert_eq!(a.help_buffer.as_ref().unwrap().cursor.byte, 10);
        a.apply(Action::HelpCursorLineStart);
        assert_eq!(a.help_buffer.as_ref().unwrap().cursor.byte, 0);
    }

    #[test]
    fn help_jump_top_and_bottom() {
        let mut a = app_with("xx", 10);
        a.help_buffer = Some(HelpBuffer::from_lines("jt", vec!["x".into(); 30]));
        a.apply(Action::HelpJumpBottom);
        let h = a.help_buffer.as_ref().unwrap();
        assert_eq!(h.cursor.line, 29);
        assert!(h.scroll > 0);
        a.apply(Action::HelpJumpTop);
        let h = a.help_buffer.as_ref().unwrap();
        assert_eq!(h.cursor.line, 0);
        assert_eq!(h.scroll, 0);
    }

    #[test]
    fn help_half_page_motions_move_cursor() {
        let mut a = app_with("xx", 10);
        a.help_buffer = Some(HelpBuffer::from_lines(
            "hp",
            (0..30).map(|i| format!("l{i}")).collect(),
        ));
        a.apply(Action::HelpHalfPage { down: true });
        let h = a.help_buffer.as_ref().unwrap();
        // viewport = 5, so half-page = 2.
        assert_eq!(h.cursor.line, 2);
        a.apply(Action::HelpHalfPage { down: false });
        assert_eq!(a.help_buffer.as_ref().unwrap().cursor.line, 0);
    }

    #[test]
    fn block_paste_extends_buffer_when_below_eof() {
        // Yank 2 rows then paste at the bottom -- the missing row is
        // appended as a fresh line.
        let mut a = enter_block_visual(
            "abcd\n1234",
            Position::new(0, 1),
            Position::new(1, 2),
        );
        let yank = CommandInvocation::of(a.builtins.yank.0)
            .with_range(lattice_grammar::Range::Selection);
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
}
