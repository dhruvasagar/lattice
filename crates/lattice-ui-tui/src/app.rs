//! Pure application state and transitions.
//!
//! The state machine is intentionally separated from the IO loop so it can be
//! unit-tested without spinning up a terminal. Each input keystroke becomes
//! an `Action`; `App::apply` consumes the action, dispatching motion / edit
//! work through `lattice_grammar::execute()` where appropriate.
//!
//! ## Module layout
//!
//! This file holds the `App` struct definition, the cross-feature data
//! types it carries (`Action`, the LSP outcome enums + structs,
//! `OptionCache`, `PositionEntry`, `LspNavKind`, `CompletionState`,
//! `Fold`, `EchoMessage`, `EchoLevel`, `SearchLine`, ...), the
//! cross-module free helpers (`line_byte_len`, `is_word_char_byte`,
//! `word_under_cursor`, etc.), and a `mod tests` block of cross-
//! feature integration tests. Per-feature App methods live in
//! `app/<feature>.rs` submodules -- see `docs/dev/notes/ui-tui-refactor.md`
//! for the full per-module catalog. The R.1.x slice sequence
//! (R.1.0 -- R.1.98) split the App's monolithic impl block apart.
//!
//! ## Where to look for App methods
//!
//! - `app/dispatch.rs` -- `apply` / `apply_effect` /
//!   `apply_app_effect` / `handle_edits` / `dispatch_blocking` /
//!   `run_*_invocation` / `execute_ex_line` / Effect classifiers.
//! - `app/lsp.rs` -- every `lattice-lsp` consumer (requests,
//!   drains, log buffers, hover, navigation, references,
//!   signature help, completion, rename, code action, format,
//!   symbols, trace, on-type formatting, trigger chars,
//!   workspace/applyEdit + workspace/configuration drains).
//! - `app/lifecycle.rs` -- `:e` / `:w` / `:q` / `:bn` / `:ls` /
//!   `<C-l>`, save family, help-buffer adoption, document swap,
//!   buffer-state hooks, pane snapshot, event publishers.
//! - `app/completion.rs` -- popup state machine, ranker, ghost
//!   text, snippet expansion, refilter, `EffectiveCompletionConfig`.
//! - `app/edit.rs` -- actor-bridge mutation wrappers,
//!   yank / paste / register store, Insert+Replace primitives,
//!   `:d`, block-insert.
//! - `app/motions.rs` -- bracket match, history walkers,
//!   mark jump, viewport / scroll, cursor clamp, viewport
//!   sizing, active-buffer accessors.
//! - `app/folds.rs` -- fold compute / open / close / auto-open.
//! - `app/search.rs` -- `/`, `?`, `:s`, `:%s`, find family.
//! - `app/options.rs` -- `:set`, typed options, customize,
//!   per-language overrides, typed-option getters.
//! - `app/cmdline.rs` -- `:` minibuffer + completion.
//! - `app/help.rs` -- `:help`, `:describe-*`, `:apropos`,
//!   `:keymap`, `do_help_follow_link`.
//! - `app/highlights.rs` -- tree-sitter highlight cache +
//!   per-frame refresh + post-edit shift.
//! - `app/visual.rs` -- charwise / linewise / blockwise
//!   selection state, `set_selections_blocking`.
//! - `app/picker.rs` -- picker state machine + candidate
//!   builders.
//! - `app/boot.rs` -- `App::new`, `build_lsp_subsystem`,
//!   `load_persistent_config`, `sync_keymap_overlays`,
//!   `sync_theme_from_config`.
//! - `app/file_tree.rs` -- file-tree buffer ops.
//! - `app/oil.rs` -- oil buffer ops.
//! - `app/macros.rs` -- `q` recording / `@` replay.
//! - `app/mode.rs` -- `modal_label` + `enter_mode` +
//!   `activate_major_for_buffer_kind`.
//! - `app/syntax.rs` -- `maybe_reparse_syntax`.
//! - `app/test_helpers.rs` -- shared test fixtures.

use lattice_core::Buffer;
#[cfg(test)]
use lattice_core::Document;
use lattice_grammar::CommandRegistry;
use lattice_grammar::ModalState;
use lattice_grammar::SearchDirection;
use lattice_grammar::VisualKind;
use lattice_grammar::YankKind;
use lattice_grammar::builtins::Builtins;
use lattice_grammar::command::CommandInvocation;
use lattice_grammar::register::Register;
use lattice_protocol::Event;
use lattice_protocol::position::{Position, Range as ProtoRange};
#[cfg(test)]
use lattice_protocol::selection::{Selection, SelectionSet};
use lattice_lsp::{DiagnosticsLayer, LspLogger, LspSupervisorHandle};
use lattice_runtime::{DocumentHandle, EventBus, SnapshotCache};
use lattice_syntax::{LangRegistry, StyledSpan};

use std::collections::HashMap;
use std::sync::Arc;

use crate::buffer_registry::{BufferData, BufferEntry, BufferRegistry, DocumentEntry};
use crate::buffers::{BufferFlags, BufferId, BufferKind};

use crate::pane::{PaneDirection, PaneTree};

// R.1.0 -- app/ submodule skeleton. Each submodule is a
// per-feature destination for the App's methods. R.1.0 only
// creates the empty modules with scoping doc comments;
// subsequent R.1.x slices move method blocks across without
// rethinking the structure. See docs/dev/architecture/keymap-architecture.md
// (or the dedicated R.1 doc) for the full feature -> module
// mapping.
mod boot;
mod cmdline;
mod completion;
mod display;
mod dispatch;
mod edit;
mod file_tree;
mod folds;
mod help;
mod highlights;
mod lifecycle;
mod lsp;
mod macros;
mod mode;
mod motions;
mod oil;
mod operators;
mod options;
mod picker;
mod popup;
mod search;
mod state;
mod syntax;
mod visual;

#[cfg(test)]
pub(crate) mod test_helpers;

// Slice 8.i.4.d: the `Pending` enum and `Action::SetPending`
// variant retired here. All multi-key Normal-mode chord state
// flows through `App::partial_chord` driven by
// `Action::AbsorbPartialChord` (slices 8.i.4.a/b) plus
// `AppEffect::AbsorbOperatorPrefix(_)` for operator prefixes
// (slice 8.i.4.c). `App::apply`'s pending field, the
// `Action::SetPending(_)` arm, and the "non-SetPending clears
// pending" guard are gone.

/// Vim's `H` / `M` / `L` cursor target within the visible
/// viewport. Slice 8.i.2.c hoisted the type into
/// `lattice_grammar::app_effect` so `AppEffect::JumpViewport`
/// can carry it; this is a re-export so existing
/// `crate::app::ViewportPos` callers stay compiling.
pub use lattice_grammar::ViewportPos;

/// Vim's `zz` / `zt` / `zb` post-scroll cursor target. Re-export
/// of the `lattice_grammar` definition (see `ViewportPos` above
/// for rationale).
pub use lattice_grammar::ScrollPos;

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
pub(super) fn resolve_command_name_or_alias(
    registry: &lattice_grammar::CommandRegistry,
    name: &str,
) -> Option<lattice_grammar::CommandId> {
    if let Some(id) = registry.id_by_name(name) {
        return Some(id);
    }
    let canonical = crate::excommand::aliases().get(name).copied()?;
    registry.id_by_name(canonical)
}


#[derive(Debug, Clone)]
pub enum Action {
    None,
    Quit,
    /// Run a CommandInvocation through `lattice_grammar::execute()`.
    Invoke(CommandInvocation),
    /// Slice 8.i.4.a -- absorb the captured chord into
    /// `App::partial_chord`, marking that we're partway through
    /// a multi-key sequence the trie hasn't fully resolved yet.
    /// Replaces the `Action::SetPending(Pending::After*)` flow
    /// for prefixes whose only role was "wait for the next key"
    /// (`g`, `z`, `<C-w>`, `m`, `'`, `` ` ``, `"`, `q`, `@`).
    /// `App::apply` appends the chord and otherwise no-ops; the
    /// next keystroke runs through `dispatch_normal` with
    /// `partial_chord` as the prefix, hitting the trie's
    /// resolved binding (`gd`, `zo`, `<C-w>v`, ...). Parameterised
    /// Pending variants (`AfterOperator(_)`,
    /// `AfterTextObject{_}`, `AfterFindChar{_}`, `AfterCtrlX`)
    /// stay on the `SetPending` flow for now -- 8.i.4.b retires
    /// those.
    AbsorbPartialChord(crate::chord::KeyChord),
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
    /// `gD` (Phase 4.2.c follow-up). Same dispatch shape as
    /// [`Self::LspDefinitionRequest`] but routes
    /// `textDocument/declaration`. Useful for languages where
    /// declaration ≠ definition (header / forward declaration in
    /// C-family, `extern` in Rust, etc.).
    LspDeclarationRequest,
    /// `gy` (Phase 4.2.c follow-up). `textDocument/typeDefinition`
    /// -- "where is the *type* of this expression defined?".
    /// Steps from a value's call-site to its struct / class /
    /// interface declaration.
    LspTypeDefinitionRequest,
    /// `gI` (Phase 4.2.c follow-up). `textDocument/implementation`
    /// -- "where are the implementations of this trait /
    /// interface?". Often returns multiple locations; shares
    /// definition's pick-or-list dispatch.
    LspImplementationRequest,
    /// `gr` (Phase 4.2.d). Send `textDocument/references` to
    /// every attached LSP server; render the merged + deduped
    /// list as a `*lsp:references*` help-style buffer with one
    /// `[path:line:col](file:...)` row per hit. `<CR>` on a row
    /// jumps to the location via the existing
    /// `do_help_follow_link` Source-link path. Cancellation
    /// token rides on follow-up `gr` so a slow server can't
    /// drop a stale popup over a moved cursor.
    LspReferencesRequest,
    /// `:lsp-signature-help` (Phase 4.3). Sends
    /// `textDocument/signatureHelp` to attached servers; the
    /// first non-empty response renders into a popup near the
    /// cursor. In Insert mode the same request fires
    /// automatically when the user types a server-advertised
    /// trigger character (commonly `(` and `,`).
    LspSignatureHelpRequest,
    /// `:complete` (Phase 4.2.g, picker-flavoured). Fires
    /// `textDocument/completion` at the cursor; the merged item
    /// list opens as a vertico picker (label + kind glyph +
    /// detail). Accept replaces the prefix-under-cursor with
    /// the item's insert text. Snippet expansion + lazy
    /// `completionItem/resolve` are queued behind buffer-level
    /// Insert-mode completion (which doesn't exist yet -- this
    /// is the bridge until that lands).
    LspCompletionRequest,
    /// `<C-t>` -- pop the tag stack (vim's tag-stack
    /// `:pop`). Walks back through the LIFO chain of `gd` /
    /// `gD` / `gy` / `gI` drill-downs. Independent of the
    /// jump-list `<C-o>` walk: the stack and the list have
    /// different push semantics and can have different lengths.
    TagStackPop,
    /// **Insert-mode completion** (Phase 4.2.g.1). Manually
    /// open the popup at the cursor or refresh an open one.
    /// Bound by default to `<C-x><C-o>` / `<C-Space>` /
    /// smart-tab.
    CompletionTrigger,
    /// Move the popup selection down (`<C-n>` / `<Down>` /
    /// `<Tab>` cycle).
    CompletionNext,
    /// Move the popup selection up (`<C-p>` / `<Up>` /
    /// `<S-Tab>` cycle).
    CompletionPrev,
    /// Accept the focused candidate (`<C-y>` / `<Tab>` /
    /// `<CR>`). Splices the candidate's insert text into the
    /// buffer at the popup's anchor and closes the popup.
    CompletionAccept,
    /// Close the popup, stay in Insert (`<C-e>`).
    CompletionCancel,
    /// Close the popup AND exit Insert mode (`<Esc>`). Mirrors
    /// vim's `<Esc>` semantics with one extra step (drop the
    /// popup before the modal switch).
    CompletionCancelAndExitInsert,
    /// Toggle the side documentation popup for the focused
    /// candidate (`<C-d>`, only inside the completion-popup
    /// minor mode).
    CompletionToggleDocs,
    /// Scroll the docs side popup forward (`<C-f>` inside
    /// the completion-popup minor mode).
    CompletionDocsScrollDown,
    /// Scroll the docs side popup backward (`<C-b>` inside
    /// the completion-popup minor mode).
    CompletionDocsScrollUp,
    /// Restrict the popup to a single completion source.
    /// String is the `SourceId` (e.g. `"gen:buffer-words"`).
    /// Bound to the popup-mode filter chords introduced in
    /// CSM.K2 (`<C-b>` buffer, `<C-o>` lsp, `<C-f>` path,
    /// `<C-t>` tree-sitter, ...).
    CompletionFilterToSource(String),
    /// Clear the active source filter (`<C-Space>`). Restores
    /// the mixed merged candidate list.
    CompletionFilterClear,
    /// Insert-mode character key while the completion popup
    /// is open (Phase 4.2.g.7 commit-char polish). The App's
    /// handler decides at apply time:
    ///
    /// - If the typed `char` is in the focused candidate's
    ///   effective commit-character set (LSP-supplied per-item
    ///   list union'd with `completion.extra_commit_chars`),
    ///   the popup accepts the candidate then inserts the
    ///   typed `char` afterward (vim convention: a commit
    ///   character behaves like "accept and continue typing").
    /// - Otherwise the typed `char` flows through plain
    ///   `do_insert_text`; the popup refilters against the
    ///   updated query as if the layer had returned `None`.
    ///
    /// Routing every popup-time char through this single
    /// action keeps the input layer ignorant of commit-char
    /// state -- the App reads it once at apply time.
    CompletionAcceptThenInsert(char),
    /// `<C-x><C-s>` -- direct snippet expansion (Phase
    /// 4.2.g.4). Looks up the word at the cursor in the
    /// per-language snippet registry; expands the matching
    /// snippet directly without surfacing the popup. No-op
    /// when no snippet matches.
    SnippetExpand,
    /// `<Tab>` inside the active-snippet minor mode -- jump
    /// to the next placeholder. Returns to no-op when at
    /// `$0`; the minor mode then deactivates.
    SnippetNextPlaceholder,
    /// `<S-Tab>` inside the active-snippet minor mode --
    /// jump to the previous placeholder.
    SnippetPrevPlaceholder,
    /// `<Esc>` while a snippet is active -- exit the snippet
    /// (placeholders become plain text); also exits Insert
    /// per vim convention.
    SnippetLeave,
    /// `:lsp-symbols` (Phase 4.2.e). Send
    /// `textDocument/documentSymbol` to every attached server;
    /// render the merged outline as a vertico picker. Selecting
    /// a row jumps to the symbol's location.
    LspDocumentSymbolRequest,
    /// `:lsp-workspace-symbol [query]` (Phase 4.2.f). Send
    /// `workspace/symbol` to every attached server with the
    /// user-supplied query string (server-side substring filter).
    /// Empty query returns the server's idea of "everything"
    /// (rust-analyzer streams all crate symbols). Picker UX
    /// mirrors the document-symbol path.
    LspWorkspaceSymbolRequest(String),
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
    /// `-` in any normal-mode context — context-sensitive:
    /// • Document / FileTree → open oil for parent dir of current file / hovered entry
    /// • Oil buffer → `oil.navigate_up()`
    OilNavigateUp,

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
/// took it over. Used by `dismiss_popup` to restore the user to the
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
    /// In-flight partial-chord stack from the trie (slice 8.i.4).
    /// When the trie returns `LookupResult::Partial`,
    /// `dispatch_normal` / `dispatch_insert` emit
    /// `Action::AbsorbPartialChord(c)` and `App::apply` appends
    /// `c` here. The next keystroke runs through the trie with
    /// this stack as prefix and resolves the full multi-key
    /// chord. Cleared on every non-`AbsorbPartialChord` action.
    /// Operator-prefix pushes (8 prefixes: `d`, `c`, `y`, `>`,
    /// `<`, `gU`, `gu`, `g~`) come from
    /// `AppEffect::AbsorbOperatorPrefix(_)` via
    /// `apply_app_effect`, which also latches `pending_count`
    /// into `op_count` atomically with the prefix push.
    pub partial_chord: Vec<crate::chord::KeyChord>,
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
    /// Which navigation flavour the in-flight request is for. Used
    /// by [`Self::drain_pending_definitions`] to pick the right
    /// "no X found" echo and the pre-jump history-source tag.
    /// `None` when no nav request is in flight; matches the
    /// nullness of `pending_definition_token`.
    ///
    /// All four nav requests (definition / declaration /
    /// typeDefinition / implementation) share the same
    /// `pending_definition_*` slot because there's never a
    /// reason to have more than one in flight at once -- a
    /// follow-up nav cancels its predecessor.
    pub pending_nav_kind: Option<LspNavKind>,
    /// Receiver for in-flight references responses (Phase 4.2.d).
    /// References gets its own slot (separate from
    /// `pending_definition_*`) because the result handling differs
    /// -- references opens a buffer-backed list view rather than
    /// jumping. The two surfaces could coexist (a hover popup +
    /// a references list), so they don't fight over the slot.
    pub pending_references_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<ReferencesOutcome>>,
    /// Cancellation token for the most recent references request.
    /// Flipped on follow-up `gr` so a slow server's response
    /// can't open a list against a moved cursor.
    pub pending_references_token: Option<lattice_protocol::CancellationToken>,
    /// Receiver for in-flight document-symbol responses (Phase
    /// 4.2.e). Same shape as references -- the merged symbol
    /// outline arrives as a `Vec<SymbolRow>` ready to seed the
    /// picker.
    pub pending_symbols_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<SymbolsOutcome>>,
    pub pending_symbols_token: Option<lattice_protocol::CancellationToken>,
    /// Receiver for in-flight format responses (Phase 4.3).
    pub pending_format_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<FormatOutcome>>,
    pub pending_format_token: Option<lattice_protocol::CancellationToken>,
    /// Receiver for in-flight signature help responses (Phase
    /// 4.3). The drain feeds the markdown body into the same
    /// popup pipeline `K` uses, so the user can dismiss with
    /// `<Esc>` or move the cursor to clear.
    pub pending_signature_help_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<SignatureHelpOutcome>>,
    pub pending_signature_help_token: Option<lattice_protocol::CancellationToken>,
    /// Receiver for in-flight LSP completion responses (Phase
    /// 4.2.g). The accept path stitches the chosen item's
    /// insert_text into the buffer at the captured replace
    /// range.
    pub pending_completion_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<CompletionOutcome>>,
    pub pending_completion_token: Option<lattice_protocol::CancellationToken>,
    /// Captured at request fire so the accept path can splice
    /// the chosen item's text into the buffer at the right
    /// position. Cleared on dismiss / outcome consumption.
    pub pending_completion_items: Option<Vec<CompletionItemRow>>,
    /// Receiver for in-flight `:rename` responses (Phase 4.3).
    /// Drained per-frame; the `Edits` arm fans out across every
    /// affected URI applying TextEdits (one undo unit per file
    /// in v1; cross-file atomic application is queued).
    pub pending_rename_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<RenameOutcome>>,
    pub pending_rename_token: Option<lattice_protocol::CancellationToken>,
    /// Receiver for in-flight `:code-actions` responses (Phase
    /// 4.3). Drained per-frame; the Items arm pins items on
    /// the App and opens a picker.
    pub pending_code_action_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<CodeActionOutcome>>,
    pub pending_code_action_token: Option<lattice_protocol::CancellationToken>,
    /// Pinned code-action items, indexed by the picker's
    /// candidate `text` (`#<idx>`). Cleared on dismiss /
    /// outcome consumption / accept.
    pub pending_code_action_items: Option<Vec<CodeActionRow>>,
    /// The handle that produced the in-flight code-action
    /// request, kept alive so the resolve / executeCommand
    /// follow-up routes to the same server. v1 picks the first
    /// server that advertises the provider; the handle clone
    /// is cheap (Arc-backed).
    pub pending_code_action_handle: Option<lattice_lsp::ServerHandle>,
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
    /// App-side typed action IDs (`CommandKind::Action`
    /// registrations from `crate::actions::populate`). Each
    /// field is a `CommandId` resolving to an `ActionSpec` whose
    /// `apply` returns `Effect::AppAction(AppEffect::Foo)`. Per-
    /// mode keymap modules consume this alongside `builtins` to
    /// build typed `CommandInvocation`s for chord bindings (slice
    /// 8.i; see `docs/dev/notes/8i-approach.md`).
    pub action_ids: crate::actions::ActionIds,
    /// Layered keymap registry (DESIGN.md §5.2.3, audit slice 8.c).
    /// Populated at construction time; the input dispatcher reads
    /// from it on every keystroke. Wait-free reads via the
    /// internal `ArcSwap`; concurrent registration writes (mode
    /// push/pop, plugin registration, `:bind`) never stall the
    /// input path. Slices 8.d / 8.e / 8.f wire Replace, Visual,
    /// and Insert through this; Normal follows in 8.g.
    pub keymap: crate::keymap_registry::KeymapHandle,
    /// `LayerId` of the active completion-popup minor-mode
    /// layer when the popup is open; `None` otherwise. Pushed /
    /// popped by [`Self::sync_keymap_overlays`] in lockstep with
    /// `self.insert_completion`. Slice 8.f.
    completion_popup_layer: Option<crate::keymap_registry::LayerId>,
    /// `LayerId` of the active-snippet minor-mode layer when a
    /// snippet is in flight; `None` otherwise. Same lockstep
    /// pattern as [`Self::completion_popup_layer`].
    snippet_layer: Option<crate::keymap_registry::LayerId>,
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
    ///
    /// Audit slice 3: this is now an async handle. Reparses run
    /// on a worker task (`tokio::task::spawn_blocking`) so the
    /// UI thread never parses; reads against the latest snapshot
    /// are wait-free via `ArcSwap`. The `Syntax` struct itself
    /// stays accessible for one-shot users (help-buffer
    /// markdown highlighting).
    pub syntax: Option<lattice_syntax::SyntaxHandle>,
    /// `text_version` last sent to the syntax handle's reparse
    /// channel. Used to skip republishing identical state when
    /// no text mutation has happened since the previous frame.
    last_parsed_text_version: u64,
    /// Slice B.2 part 2: tree-sitter-shaped edit deltas
    /// accumulated since the last `maybe_reparse_syntax` call.
    /// Pushed by `publish_document_changed` after each
    /// `Buffer::apply_edit`; drained by `maybe_reparse_syntax`
    /// and shipped to the syntax worker as `Vec<EditDelta>` for
    /// incremental reparse via `tree.edit()` + `Parser::parse(_,
    /// Some(&old_tree))`. Empty between Actions; never grows
    /// unboundedly because every Action ends in
    /// `maybe_reparse_syntax` which drains.
    pending_syntax_edits: Vec<lattice_protocol::edit::EditDelta>,
    /// `text_version` the syntax worker's tree is known to be at.
    /// Sent as `from_version` on the next reparse request so the
    /// worker can verify edits apply to the correct tree
    /// baseline; mismatch triggers full-reparse fallback. Reset
    /// to 0 when a fresh syntax handle is wired up (file open /
    /// language change / active-buffer switch).
    last_synced_syntax_version: u64,
    /// Per-line `StyledSpan`s for the currently visible viewport, indexed
    /// from `[scroll, scroll + viewport_height)`. Recomputed each frame by
    /// `refresh_highlights` (called from the runtime before drawing).
    pub visible_highlights: Vec<Vec<StyledSpan>>,
    /// Slice B.3: cache key validating the contents of
    /// `visible_highlights`. When `refresh_highlights` finds the
    /// freshly-computed key matches this stored key, the
    /// existing `visible_highlights` is still valid and the
    /// `highlight_lines` call is skipped entirely (~178µs at
    /// 24-line viewport on rust per BENCHMARKS.md →
    /// noise-floor on cache hit).
    ///
    /// Invariant: `Some` ⟹ `visible_highlights` was computed
    /// against this key's snapshot+state. `None` after construction,
    /// after the syntax handle is replaced, and after a state-
    /// change render where the key check passes the recompute
    /// branch (the new key is stored).
    ///
    /// Steady-state hit rate: ~100% (cursor blinking, no edit).
    /// Drops cleanly to 0% during edits/scroll/fold-toggle.
    visible_highlights_key: Option<highlights::VisibleHighlightsKey>,
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
    /// P.2: MRU list of canonical paths the user has opened via
    /// `:edit` (or any path that flowed through `do_edit`).
    /// Newest first; dedup keeps a single entry per path. Capped
    /// at `MAX_RECENT_FILES`. Surfaces in `:recent` (the
    /// recent-files picker) and as input to per-language /
    /// per-workspace history features later.
    pub recent_files: Vec<std::path::PathBuf>,
    /// Vim-style tag stack (DESIGN.md §5.1.1 follow-up). Distinct
    /// from the jump list: every "drill-down" navigation
    /// (`gd` / `gD` / `gy` / `gI` and their multi-result picker
    /// accept variants) pushes one entry; `<C-t>` pops the most
    /// recent entry and walks back to the recorded position.
    /// The user's mental model: `<C-o>` walks all jumps in
    /// chronological order; `<C-t>` pops only the LIFO tag-style
    /// drill-downs. They coexist deliberately and can have
    /// different lengths.
    pub tag_stack: Vec<TagStackEntry>,
    /// Pre-jump origin captured when an LSP nav request fires;
    /// transferred to `tag_stack` on the actual jump (single-
    /// result drain or multi-result picker accept). Cleared on
    /// picker dismiss / nav cancellation / drain with no results.
    pub pending_tag_origin: Option<TagStackEntry>,
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
    /// surface. Renderer-agnostic options self-register via
    /// the `linkme`-aggregated `OPTION_DECLS` slice; this
    /// renderer's own options register via
    /// [`crate::tui_options::register_tui_options`].
    pub config: std::sync::Arc<lattice_config::ConfigRegistry>,
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
    // M.2.0c: TUI-specific options self-register via the
    // linkme slice. No `tui_options` field needed -- callers
    // read directly via `config.get_typed::<UiDimInactive>()`
    // etc. (see `sync_theme_from_config`).
    /// Mode registry (M.1). Owns the catalogue of registered
    /// modes; activation / deactivation routes through here.
    /// One process-shared registry; all Documents share the
    /// same mode definitions.
    pub mode_registry: std::sync::Arc<lattice_mode::ModeRegistry>,
    /// Mode-keyed pane render dispatch (M.4 follow-up). Populated
    /// at boot; lookup walks active minors then the major to find
    /// the per-buffer renderer + status formatter, with the
    /// document path as the fallback when no provider matches.
    /// Replaces the helper-side `match buffer.kind` in
    /// `draw_pane_content` and `pane_status_label`.
    pub pane_render_registry: crate::pane_render::PaneRenderRegistry,
    /// Per-buffer active modes (major + minors). M.1 wired the
    /// field on `Document` for the document buffer, but
    /// `Document` lives behind the actor's snapshot-cache, so
    /// for M.2.1 the App layer maintains a parallel
    /// per-buffer map keyed by `buffers::BufferId` -- this is
    /// the version `recompute_options_for_buffer` reads to
    /// pull mode contributions. `Document.modes` and this map
    /// converge in M.4 when `ActiveModes` joins
    /// `DocumentSnapshot`.
    pub active_modes: std::collections::HashMap<
        crate::buffers::BufferId,
        lattice_mode::ActiveModes,
    >,
    /// Per-buffer mode-owned local state (M.3.2.a). Modes
    /// populate locals via the `BufferLocal` typed-map during
    /// `on_activate`; the App routes
    /// `&mut BufferLocals` into the registry's activation
    /// methods. M.3.2.b/c migrates existing per-variant data
    /// (`SyntaxHandle`, `Vec<Fold>`, etc.) into locals owned
    /// by their respective modes; until then this map exists
    /// to thread through the new activation API and to back
    /// `:describe-buffer`'s inspection (no entries until
    /// M.3.2.b).
    pub buffer_locals: std::collections::HashMap<
        crate::buffers::BufferId,
        lattice_mode::BufferLocals,
    >,
    /// Per-buffer mode-resolved options cache (M.2.1, see
    /// `mode-architecture.md` §6.3 / §9.4 — note: the doc shows
    /// this on `Document`, but lattice-core cannot depend on
    /// lattice-config without a dep cycle, so the cache lives
    /// at the App layer keyed by `buffers::BufferId` (the App's
    /// per-buffer key, not the lower-level
    /// `lattice_protocol::BufferId`). Refreshed eagerly on mode
    /// toggle and option write per §6.3.1. Reads via type-keyed
    /// access against the cached snapshot are O(1).
    pub resolved_options: std::collections::HashMap<
        crate::buffers::BufferId,
        lattice_config::ResolvedOptions,
    >,
    /// Buffer-local explicit overrides (`:setlocal foo=bar`)
    /// per buffer. Inputs to resolution; the resolver chains
    /// these with mode contributions before writing
    /// [`Self::resolved_options`]. Empty for buffers the user
    /// has never run `:setlocal` against.
    pub buffer_local_overrides: std::collections::HashMap<
        crate::buffers::BufferId,
        lattice_config::OptionOverrideSet,
    >,
    /// Free-form help topic registry (DESIGN.md §5.11). `:help`
    /// reads from this; built-ins are sourced from `docs/user/*.md`
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
    /// Active popup overlay's buffer id (DESIGN.md §5.11; M.4).
    /// `Some(id)` while a `:describe-*` / `:apropos` / hover / etc.
    /// popup is open; the actual buffer lives in
    /// [`Self::buffers`] (the unified registry) with
    /// `BufferFlags { listed: false, hidden: true }`. Resolve the
    /// concrete handle through [`Self::popup_help`] /
    /// [`Self::popup_help_mut`] -- the slot itself is just a
    /// reference into the registry. Display strategy is governed
    /// by [`lattice_core::ui::display::BufferDisplayCategory`];
    /// callers route through [`Self::display_buffer`].
    pub popup_buffer: Option<crate::buffers::BufferId>,
    /// LIFO stack of snapshots taken every time the popup's content
    /// gets swapped in place by a help → help link follow (e.g.
    /// `:describe-buffer` → click `[text-mode](mode:text-mode)` →
    /// `:describe-mode text-mode`). One popup buffer is reused
    /// across the navigation so jump-list / marks / search /
    /// register state stay coherent; this stack records what was
    /// in the buffer before each swap so `<C-o>` from inside the
    /// popup can restore the prior frame without leaving Help.
    /// Drained on `dismiss_popup`; reset whenever a fresh top-
    /// level popup opens from outside Help.
    pub popup_back_stack: Vec<crate::app::popup::PopupSnapshot>,
    /// Pane state captured before activating help -- used by
    /// `dismiss_popup` to restore the user to whatever buffer +
    /// cursor + scroll they came from. Set by both display
    /// paths (in-pane via `activate_help_in_pane`, popup via
    /// `open_help_popup_overlay`); cleared by dismiss. v1 single-
    /// pane scope -- multi-pane help dismissal will key by pane
    /// id when that scenario surfaces.
    pub prev_pane_for_help: Option<PrevPaneState>,
    /// Where the popup overlay sits on screen when one is open.
    /// Carried on App (not on the buffer) because the popup is a
    /// generic rectangular surface inside which any buffer kind
    /// renders -- placement is a property of the popup, not of
    /// whatever buffer happens to be its content. Cursor-anchored
    /// popups (hover / signature help) set this when they open;
    /// the default (centred) covers `:lsp-status`, `:describe-*`,
    /// `:apropos`, `:help`, `:keymap`, `:options`, `:ls`, ...
    pub popup_placement: crate::popup::PopupPlacement,

    /// Pluggable completion pipeline (DESIGN.md §5.11.3). Owned by
    /// the App at v1 -- promotes to a sibling crate when plugins
    /// need cross-buffer access.
    pub completion_registry: lattice_completion::CompletionRegistry,
    /// Active completion popup. `Some` while the user has Tab-
    /// triggered completion in the `:` line.
    pub completion_state: Option<CompletionState>,
    /// Active **Insert-mode** completion popup (Phase 4.2.g).
    /// Distinct from `completion_state` (which drives the `:` line
    /// completion popup): this one floats over the buffer, shows
    /// candidates from sources (LSP / snippets / buffer-words /
    /// path / tree-sitter / plugin), and the host's keystroke
    /// dispatcher routes through a "completion-popup minor mode"
    /// keymap layer while it's `Some`. Behavioural spec lives in
    /// [`docs/dev/architecture/insert-completion.md`](../../docs/dev/architecture/insert-completion.md).
    pub insert_completion: Option<lattice_completion::InsertCompletionState>,
    /// Receiver for in-flight LSP insert-completion responses.
    /// Drained per-frame; the drain merges new items into
    /// `insert_completion.raw` and refilters.
    pub pending_insert_completion_lsp_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<InsertCompletionLspOutcome>>,
    /// Cancellation token for the most recent LSP insert-
    /// completion request. Flipped on every re-trigger
    /// (isIncomplete refresh, manual `<C-Space>` re-fire) so a
    /// slow server's stale response can't pollute the popup.
    pub pending_insert_completion_lsp_token:
        Option<lattice_protocol::CancellationToken>,
    /// Receiver for in-flight `completionItem/resolve` results
    /// (Phase 4.2.g.3). Populates the focused candidate's
    /// LspCompletionMeta `documentation` field + the docs
    /// popup body. Cancelled when selection changes / popup
    /// closes / fresher resolve fires.
    pub pending_completion_resolve_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<CompletionResolveOutcome>>,
    pub pending_completion_resolve_token:
        Option<lattice_protocol::CancellationToken>,
    /// Per-language snippet registry (Phase 4.2.g.4). Loaded
    /// at startup from bundled / user / project paths via
    /// `lattice-snippet::load`; the `gen:snippet` source
    /// consults it per-popup-trigger.
    /// CSM.5: held as `Arc<ArcSwap<...>>` so the mode-captured
    /// handle stays valid across `:reload-snippets`. Source reads
    /// load the current snapshot via `.load()` (wait-free); the
    /// reload path swaps the inner via `.store()` so the mode's
    /// next produce sees the fresh data.
    pub snippet_registry: std::sync::Arc<arc_swap::ArcSwap<lattice_snippet::SnippetRegistry>>,
    /// Sidecar metadata for snippet candidates in the active
    /// insert-completion popup. Indexed by the candidate's
    /// `CandidateData::Extension { payload }` (u32 LE) --
    /// same shape as `insert_completion_lsp_meta` for the
    /// LSP source.
    /// CSM.5: retired. Snippet candidates now carry their stable
    /// `name` in the `Extension::payload` field; the accept path
    /// re-resolves the body via `App.snippet_registry.by_name`.
    /// Field kept as an empty Vec for one slice so callers that
    /// haven't migrated still compile; field deletion in a
    /// follow-up cleanup slice.
    pub insert_completion_snippet_meta: Vec<SnippetCandidateMeta>,
    /// Per-session accept-count map for the insert-mode
    /// completion popup (Phase 4.2.g.5). Each accepted candidate
    /// bumps the counter for its `(text, kind)` pair; the ranker
    /// reads this map and adds a bounded bonus
    /// (`InsertRanker::FREQUENCY_BONUS_CAP`) so recently-accepted
    /// items bubble above tied peers next time. In-memory only
    /// in v1 -- cleared on process exit; persistence with a
    /// privacy story lands later (Phase 4.2.g.7 polish queue per
    /// `docs/dev/architecture/insert-completion.md` §11).
    pub completion_accept_freq:
        std::collections::HashMap<(String, lattice_completion::CandidateKind), u32>,
    /// TOML structural sections collected by the config loader at
    /// startup but not yet routed to their owners. Keyed by full
    /// dotted path (e.g. `"completion.per-language.markdown"`,
    /// `"plugin.rust-analyzer"`); value is the sub-table verbatim.
    /// Phase 4.2.g.5 (3b/3) drains the `completion.per-language.*`
    /// entries into `per_language_completion`; the plugin host
    /// (Phase 7) will drain `plugin.*`. v1's bucket means a user
    /// who writes `[plugin.X]` optimistically doesn't lose the
    /// content -- it's preserved here for the plugin host to pick
    /// up when it registers.
    pub pending_config_structural_sections:
        std::collections::BTreeMap<String, toml::Table>,
    /// Per-language insert-completion overrides (Phase 4.2.g.5
    /// (3b/3); spec at `docs/dev/architecture/insert-completion.md` §9). Seeded
    /// at App init from
    /// [`lattice_completion::per_language_defaults`] (markdown
    /// drops LSP, rust enables auto-fire, etc.); user TOML in
    /// `[completion.per-language.<lang>]` sections layers on top
    /// via [`Self::apply_per_language_toml_overrides`]. Read by
    /// [`Self::effective_completion_for`] which walks
    /// per-language -> global option -> hardcoded fallback for
    /// each effective field.
    pub per_language_completion:
        std::collections::HashMap<String, lattice_completion::PerLanguageOverrides>,
    /// `true` while the active insert-completion popup is in
    /// path-completion mode (Phase 4.2.g.6 (2/2)). Set at
    /// popup-trigger time when the cursor sits inside a string
    /// literal AND `gen:path` is enabled for the active
    /// language; cleared on popup dismiss / accept. Drives
    /// path-aware anchor resolution in `do_completion_trigger`
    /// and source-set selection in
    /// `populate_insert_completion_sync` (path source only;
    /// other sync sources skip).
    pub completion_in_path_context: bool,
    /// Live snippet expansion. `Some` while a snippet is
    /// active and `<Tab>` / `<S-Tab>` navigate placeholders.
    /// Dropped on `$0` consumption / `<Esc>` / cursor moving
    /// outside the snippet's tabstop ranges.
    pub active_snippet: Option<lattice_snippet::ActiveSnippet>,
    /// Per-language directories from which snippet packs are
    /// loaded on startup / `:reload-snippets` (Phase 4.2.g.4).
    /// Each entry is a directory of `*.json` files in
    /// friendly-snippets format; the file's stem is the
    /// language id (e.g. `rust.json` -> language `"rust"`,
    /// `_global.json` -> the all-language `*` slot). Tests
    /// seed this with a tempdir; production reads from
    /// `~/.config/lattice/snippets/` (wired in startup -- see
    /// `App::default_snippet_dirs`).
    pub snippet_dirs: Vec<std::path::PathBuf>,
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
    /// Receiver for [`lattice_lsp::LspLogPushed`] events (Phase
    /// 4; M.5.3.b moved the event type from `lattice-protocol`'s
    /// central enum to `lattice-lsp::events`). Drained once per
    /// main-loop tick by [`Self::drain_lsp_log_events`];
    /// matching log buffers in `BufferRegistry` are rebuilt from
    /// the logger snapshot so `*lsp*` / `*lsp:<server>*` /
    /// `*lsp:<server>:trace*` views update live without the
    /// user having to reopen them.
    pub lsp_log_event_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<lattice_lsp::LspLogPushed>>,
    // `completion.auto_insert_single` lives on the typed-options
    // registry (`self.config` type-keyed by
    // `lattice_config::CompletionAutoInsertSingle`). Read via
    // [`Self::completion_auto_insert_single`].
    /// One-shot "auto-submit on next chord" flag. Set when the
    /// user submitted a Chord-arg-required command with no value
    /// (`:describe-key<CR>`); the cmdline pre-fills with the
    /// command word + space, and the very next captured chord
    /// auto-fires [`Action::CommandLineSubmit`] without an
    /// explicit `<CR>`. Reset on cancel / submit.
    pub auto_submit_after_chord: bool,
    /// LSP subsystem handle (DESIGN.md §5.4, Phase 4.1.h +
    /// audit slice 1). Reads (`servers_for`, `running_actors`,
    /// `configs`, ...) are wait-free against an
    /// `ArcSwap<SupervisorSnapshot>`; writes (`open_buffer`,
    /// `attach_handle`, `close_buffer`, `flush*`, `shutdown`)
    /// route through the supervisor task's mailbox. The UI
    /// thread can call any read on the keystroke / render path
    /// without ever blocking, and the previous
    /// `Arc<tokio::sync::Mutex<LspSupervisor>>` -- which
    /// silently dropped work via `try_lock` whenever an async
    /// `:e <path>` held the mutex across the LSP `initialize`
    /// handshake -- is gone.
    pub lsp: LspSupervisorHandle,
    /// Cloned handle to the supervisor's diagnostics layer.
    /// `DiagnosticsLayer` is Clone-via-Arc-internal so this is
    /// cheap; the renderer's per-frame `app.lsp_diagnostics
    /// .line_severity(...)` reads happen without taking the
    /// supervisor lock.
    pub lsp_diagnostics: DiagnosticsLayer,
    /// Cloned handle to the supervisor's logger. Same lock-
    /// free read pattern as `lsp_diagnostics`.
    pub lsp_logger: LspLogger,
    /// Server-initiated `workspace/applyEdit` request stream
    /// (Phase 4.3). Drained per-frame by
    /// [`Self::drain_inbound_apply_edits`]: each request lands
    /// as an [`lattice_lsp::InboundApplyEdit`] carrying a
    /// `WorkspaceEdit` + a oneshot the App fills with the
    /// outcome. The receiver is taken once at App init; `None`
    /// after the runtime hands it off to the drain (we
    /// take-and-restore around `try_recv` so the drain's loop
    /// can run without holding `&mut self` on the receiver
    /// itself; matches `drain_option_changes` etc.).
    pub pending_apply_edit_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<lattice_lsp::InboundApplyEdit>>,
    /// Server-initiated `workspace/configuration` request stream
    /// (Phase 4.1 follow-up). Drained per-frame by
    /// [`Self::drain_inbound_configuration_requests`]; each
    /// request walks the cached TOML tree at `lsp.<section>`
    /// for every requested item and replies with the
    /// per-section `serde_json::Value`s via the embedded
    /// oneshot. Same take-and-restore pattern as the other
    /// drain channels.
    pub pending_configuration_rx: Option<
        tokio::sync::mpsc::UnboundedReceiver<lattice_lsp::InboundConfigurationRequest>,
    >,
    /// Merged TOML tree of every loaded config file (user +
    /// project, project deep-merged on top). Populated by
    /// [`Self::load_persistent_config`] from the loader's new
    /// `LoadOutcome.raw_tree` field. The
    /// `workspace/configuration` drain walks this by
    /// `lsp.<section>` to surface server-namespaced settings
    /// the typed registry doesn't pre-register.
    pub lsp_config_tree: toml::Table,
    /// `BufferId` → `Uri` map. Maintained by buffer-open /
    /// buffer-close paths; the supervisor's API is keyed by
    /// `Uri`, so this is the bridge. Set eagerly when a
    /// path-bearing buffer is opened (the URI is a
    /// deterministic `uri_from_path`); attachment of an actual
    /// LSP server happens asynchronously via the
    /// `Event::DocumentOpened` attach driver.
    pub buffer_uris: std::collections::HashMap<BufferId, lattice_lsp::Uri>,
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

/// Result of a `gr` (LSP references) request, sent from the
/// spawned task to the App's main thread via
/// [`App::pending_references_rx`]. Carries the symbol-under-
/// cursor verbatim so the rendered help buffer's title reads
/// `References for "foo"` and the user has confirmation of what
/// they searched for.
#[derive(Debug, Clone)]
pub enum ReferencesOutcome {
    /// Merged + deduped reference list across attached servers.
    /// May be empty (Found(symbol, [])) when servers know about
    /// the symbol but it has no other call sites; the help buffer
    /// renders an explicit "(no references)" line.
    Found {
        symbol: String,
        locations: Vec<lsp_types::Location>,
    },
    /// The buffer's URI maps to no attached servers. Echo
    /// "no LSP server attached" so the user can investigate.
    NoServers,
}

/// CSM.8b: `LspCompletionMeta` + `LSP_COMPLETION_KIND_ID`
/// moved into `lattice-lsp::completion` so the type lives in
/// the crate that owns lsp-types. The host imports them via
/// this re-export -- candidate payloads carry the serde-
/// encoded form directly (`encode_meta` / `decode_meta`) so
/// the parallel `App.insert_completion_lsp_meta` sidecar is
/// gone; the candidate IS the metadata.
pub use lattice_lsp::completion::{
    LSP_COMPLETION_KIND_ID, LspCompletionMeta, decode_meta, encode_meta,
};
/// `Extension::kind_id` discriminant for snippet-sourced
/// candidates (Phase 4.2.g.4). Sidecar metadata lives in
/// `App.insert_completion_snippet_meta`.
pub const SNIPPET_COMPLETION_KIND_ID: u32 = 2;

/// Sidecar metadata for snippet candidates in the popup. The
/// host renders the snippet body on accept and starts an
/// `ActiveSnippet`; this struct carries the parsed body +
/// the display fields the popup row uses.
#[derive(Debug, Clone)]
pub struct SnippetCandidateMeta {
    pub name: String,
    pub prefix: String,
    pub description: Option<String>,
    pub body: lattice_snippet::SnippetBody,
}

/// Drain payload for `completionItem/resolve` (Phase
/// 4.2.g.3). CSM.8b.5: `meta_index` is now the index of the
/// fired candidate within state.raw's LSP-row sequence; the
/// drain decodes that row's payload, applies the resolved
/// fields, re-encodes in place. Multiple resolves in flight
/// (selection change → cancel prior → fire new) cancel via
/// the supplied token.
#[derive(Debug, Clone)]
pub struct CompletionResolveOutcome {
    pub meta_index: usize,
    pub resolved: lsp_types::CompletionItem,
}

/// Drain payload for the async LSP insert-completion source.
/// Replaces (rather than appends to) the current LSP slice of
/// `state.raw` -- previous items get pruned by the drain so the
/// popup reflects the freshest server response. CSM.8b: each
/// `RawCandidate`'s `CandidateData::Extension` payload carries
/// the serde-encoded `LspCompletionMeta`; the drain decodes
/// to populate the sidecar mirror while it still exists
/// (CSM.8b.4/5 retire the mirror).
#[derive(Debug, Clone)]
pub enum InsertCompletionLspOutcome {
    Items {
        candidates: Vec<lattice_completion::RawCandidate>,
        is_incomplete: bool,
    },
    NoServers,
}

/// One row of an LSP completion picker. Carries the item
/// label, kind glyph, optional detail blurb, and the insert
/// text. `replace_range` is the byte range in the active line
/// to splice the insert text over -- driven by the item's
/// `text_edit` when present, else a word-boundary heuristic
/// host-side.
#[derive(Debug, Clone)]
pub struct CompletionItemRow {
    pub label: String,
    pub kind_glyph: &'static str,
    pub detail: Option<String>,
    /// Text to insert (raw -- no snippet expansion yet).
    pub insert_text: String,
    /// Replace range as `(start_byte, end_byte)` on the active
    /// line.
    pub replace_range: (u32, u32),
    /// Line the replace range is on (LSP 0-based). Always the
    /// cursor's line; carried explicitly to keep the accept
    /// path independent of cursor mutations.
    pub line: u32,
}

/// Outcome of a `textDocument/completion` request.
#[derive(Debug, Clone)]
pub enum CompletionOutcome {
    Items(Vec<CompletionItemRow>),
    NoServers,
}

/// One row of a code-action picker. Carries the action title,
/// kind glyph, and the original `CodeAction` payload (or its
/// raw `Command`-only form). Action items survive on the App
/// across the request → picker accept gap so the resolve / apply
/// path can read them by index.
#[derive(Debug, Clone)]
pub struct CodeActionRow {
    pub title: String,
    pub kind_glyph: &'static str,
    pub action: lsp_types::CodeActionOrCommand,
}

/// Outcome of a `:code-actions` request. Drained per frame.
#[derive(Debug, Clone)]
pub enum CodeActionOutcome {
    /// Fresh code-action result list. Drain opens a picker.
    Items(Vec<CodeActionRow>),
    /// Resolved action (post-`codeAction/resolve`). Drain
    /// applies directly -- the picker is already gone.
    Resolved(lsp_types::CodeAction),
    NoProvider,
}

/// Outcome of a `:rename` request. The success arm pre-flattens
/// the WorkspaceEdit into a per-file `Vec<TextEdit>` map so the
/// App-side apply path doesn't have to walk lsp-types' enum
/// shapes. `NoProvider` echoes when no attached server
/// advertises `renameProvider`; `NotRenameable` when
/// prepareRename refused; `Empty` when the rename succeeded
/// but the server returned no edits (no symbol matches).
#[derive(Debug, Clone)]
pub enum RenameOutcome {
    Edits {
        /// Per-file edits keyed by URI string. Each Vec is
        /// already in the order the server returned (the apply
        /// path reverse-sorts before applying, same as
        /// formatting).
        per_file: Vec<(lsp_types::Uri, Vec<lsp_types::TextEdit>)>,
        new_name: String,
    },
    NoProvider,
    NotRenameable {
        reason: String,
    },
    Empty,
}

/// Outcome of a `textDocument/signatureHelp` request. The
/// response carries multiple signatures (one per overload) plus
/// the active signature/parameter indices. We collapse to the
/// active overload + parameter highlight for the popup body.
#[derive(Debug, Clone)]
pub enum SignatureHelpOutcome {
    /// Pre-rendered markdown body for the popup. Empty body
    /// means "no signature info" (server returned None or an
    /// empty `signatures` array).
    Body(String),
    /// No server attached / no provider advertised.
    NoServers,
}

/// Outcome of a `:format` / `:format-range` request. Drained
/// per frame; the App applies the edits as one undo unit or
/// echoes the appropriate failure / no-op state.
#[derive(Debug, Clone)]
pub enum FormatOutcome {
    /// Server returned a (possibly empty) edit list. Empty == no
    /// changes needed; non-empty == apply.
    Edits(Vec<lsp_types::TextEdit>),
    /// No attached server advertises the relevant formatting
    /// provider (`is_range` distinguishes whole-buffer from
    /// range-format providers since they're separate caps).
    NoProvider {
        is_range: bool,
    },
}

/// One row of a document-symbol / workspace-symbol picker. Carries
/// the symbol's name, kind, depth (for in-document hierarchy
/// indent), and the location to jump to. Built host-side from
/// the LSP `DocumentSymbolResponse` / `Vec<SymbolInformation>`
/// so the picker doesn't depend on lsp-types.
#[derive(Debug, Clone)]
pub struct SymbolRow {
    pub name: String,
    pub kind_glyph: &'static str,
    pub container: Option<String>,
    /// Indent depth (0 = top-level). Document-symbol responses
    /// nest; workspace-symbol responses are flat (depth = 0).
    pub depth: u32,
    pub path: std::path::PathBuf,
    /// LSP 0-based line.
    pub line: u32,
    /// utf-8 byte column.
    pub col: u32,
}

/// Outcome of a document-symbol / workspace-symbol request --
/// drained per frame and either opens a picker or echoes.
#[derive(Debug, Clone)]
pub enum SymbolsOutcome {
    Found {
        title: String,
        rows: Vec<SymbolRow>,
    },
    NoServers,
}

/// Which navigation request flavour produced an in-flight nav
/// response (Phase 4.2.c). All four share the same dispatch
/// shape (per-server `Vec<Location>` merge + dedup + jump-or-
/// list) -- the kind only changes the LSP method called and
/// the user-facing "no X found" echo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspNavKind {
    Definition,
    Declaration,
    TypeDefinition,
    Implementation,
}

impl LspNavKind {
    /// Verb used in echoes ("no definitions found",
    /// "3 implementations; jumping to first", etc.).
    pub fn noun_plural(self) -> &'static str {
        match self {
            Self::Definition => "definitions",
            Self::Declaration => "declarations",
            Self::TypeDefinition => "type definitions",
            Self::Implementation => "implementations",
        }
    }

    /// Single-word noun used in error contexts ("definition target
    /// uri is not a file", etc.).
    pub fn noun_singular(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::Declaration => "declaration",
            Self::TypeDefinition => "type definition",
            Self::Implementation => "implementation",
        }
    }
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
    // M.7.3.a: whitespace decoration on/off + per-category
    // glyphs. The `show_whitespace` bool gates the renderer's
    // whitespace pre-pass; the per-category `Option<char>`s say
    // *what* glyph to substitute when a category fires.
    // `None` ⇒ that category is not decorated (typed-option
    // empty string parses to None at cache rebuild time).
    pub show_whitespace: bool,
    pub current_line_highlight: bool,
    pub whitespace_tab: Option<char>,
    pub whitespace_trailing: Option<char>,
    pub whitespace_leading: Option<char>,
    pub whitespace_space: Option<char>,
    pub whitespace_eol: Option<char>,
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
            // M.7.3.a defaults: features off; emacs-like glyph
            // defaults for the categories the option declares
            // are rebuilt from the typed options on first
            // `rebuild_option_cache` run.
            show_whitespace: false,
            current_line_highlight: false,
            whitespace_tab: Some('→'),
            whitespace_trailing: Some('·'),
            whitespace_leading: Some('·'),
            whitespace_space: None,
            whitespace_eol: None,
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

/// One entry on the vim-style tag stack. Pushed by `gd` (and
/// the goto-* family) at the pre-jump cursor; popped by `<C-t>`
/// to walk back. Distinct from the jump list because the user's
/// mental model for `<C-t>` is "undo the drill-down chain", not
/// "step through every cursor jump in chronological order".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagStackEntry {
    /// Buffer the user was looking at when they fired the
    /// `gd` (or sibling) chord. Used to switch back to the
    /// right buffer kind on `<C-t>` -- e.g. drilling out of a
    /// help buffer pops back into the help buffer, not the
    /// document.
    pub buffer: BufferKind,
    pub buffer_id: BufferId,
    /// Cursor position at the time of the drill-down.
    pub position: Position,
    /// Symbol the user drilled into (e.g. `"foo"` for `gd` on
    /// `foo`). Empty when no symbol was under cursor at the
    /// time of the request. Renders in the `:tags` view (when
    /// added) so users can see their walked chain.
    pub label: String,
}

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
            .field("command_line", &self.command_line)
            .field("last_message", &self.last_message)
            .field("dirty", &self.document.dirty())
            .finish()
    }
}

impl App {
    /// Set the echo / status-line message. The renderer reads the
    /// most recent value into the bottom row each frame.
    pub fn set_message(&mut self, level: EchoLevel, text: impl Into<String>) {
        self.last_message = Some(EchoMessage {
            text: text.into(),
            level,
        });
    }
}

pub(crate) fn line_byte_len(buf: &Buffer, line: u32) -> u32 {
    // §8.2 hot path: use ropey's O(log n) line API instead of
    // materialising the whole buffer.
    buf.line_byte_len(line)
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

pub(super) fn is_valid_mark_name(c: char) -> bool {
    c.is_ascii_alphabetic() || c.is_ascii_digit()
}

/// Render a register's content into a one-line preview (truncated and
/// with newlines escaped). Used by `:reg`.
pub(super) fn preview_register(s: &str) -> String {
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

pub(super) fn is_word_char_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Cross-source visual dedup for the insert-completion popup
/// (Phase 4.2.g.7 polish). Keeps the FIRST occurrence of each
/// `raw.text`; subsequent rows with the same text drop out.
/// Called after the ranker has sorted descending by score, so
/// the surviving row is the highest-ranked entry per text --
/// the buffer-words copy of `outer` outranks the tree-sitter
/// copy at the spec's 100/80 priority split, so the popup row
/// for `outer` carries the buffer-words tag.
///
/// Selection / navigation / accept all index into the deduped
/// vec naturally; this is the only place we coalesce rows
/// across sources, and it runs before the popup paints.
pub(super) fn dedup_rendered_by_text(rendered: &mut Vec<lattice_completion::RenderedCandidate>) {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    rendered.retain(|cand| seen.insert(cand.raw.text.clone()));
}

/// True for bytes the path-completion source treats as part of
/// a filename / directory component. Wider than `is_word_char_byte`
/// so common filename characters (`.`, `-`, `~`) ride the same
/// segment; the trigger anchor breaks at `/` (dir boundary)
/// rather than at these characters.
pub(super) fn is_path_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(b, b'_' | b'-' | b'.' | b'~' | b'+' | b'@')
}



/// True if `line_idx` is empty or whitespace-only. Used by the
/// fold-aware j/k snap to swallow trailing blanks between sibling
/// folds (so `j` from a closed fold's heading lands on the next
/// sibling's heading, not on the blank between them).
pub(super) fn is_blank_line(buffer: &lattice_core::Buffer, line_idx: u32) -> bool {
    buffer
        .line(line_idx)
        .map(|s| s.trim().is_empty())
        .unwrap_or(true)
}





/// Phase 4.2 features (hover, definition, references, completion)
/// all need this; later we'll thread the per-server negotiated
/// `PositionEncodingKind` through here so utf-8 / utf-32 servers
/// don't pay the utf-16 conversion. For 4.2.b utf-16 is correct
/// for every server we care about today.
/// Render an LSP `SignatureHelp` response into a markdown body
/// the popup renderer can display. Picks the active signature
/// (server-supplied `active_signature` index, default 0) and
/// inlines the active parameter's documentation when present.
/// Returns the empty string when the response carries no
/// signatures.
pub(crate) fn signature_help_to_markdown(sh: &lsp_types::SignatureHelp) -> String {
    if sh.signatures.is_empty() {
        return String::new();
    }
    let active_sig_idx = sh.active_signature.unwrap_or(0) as usize;
    let sig = sh
        .signatures
        .get(active_sig_idx)
        .or_else(|| sh.signatures.first())
        .expect("non-empty checked above");
    let mut out = String::new();
    // Active signature's call form -- renders as a fenced code
    // block so the popup's markdown highlighter picks up
    // syntax highlighting (whatever language the server says
    // -- we don't know, so default to text).
    out.push_str("```text\n");
    out.push_str(&sig.label);
    out.push_str("\n```\n");
    // Parameter highlight: append a short note pointing at the
    // active parameter's name. The popup overlay doesn't yet
    // do per-character paragraph styling, so this is the
    // friendliest representation we have without bespoke
    // rendering.
    if let Some(active_param_idx) = sig.active_parameter.or(sh.active_parameter)
        && let Some(params) = sig.parameters.as_ref()
        && let Some(param) = params.get(active_param_idx as usize)
    {
        let label_str = match &param.label {
            lsp_types::ParameterLabel::Simple(s) => s.clone(),
            lsp_types::ParameterLabel::LabelOffsets(_) => String::new(),
        };
        if !label_str.is_empty() {
            out.push_str(&format!("\n**param:** `{label_str}`\n"));
        }
        if let Some(doc) = param.documentation.as_ref() {
            let doc_str = match doc {
                lsp_types::Documentation::String(s) => s.clone(),
                lsp_types::Documentation::MarkupContent(mc) => mc.value.clone(),
            };
            if !doc_str.is_empty() {
                out.push('\n');
                out.push_str(&doc_str);
                out.push('\n');
            }
        }
    }
    // Signature-level documentation when present.
    if let Some(doc) = sig.documentation.as_ref() {
        let doc_str = match doc {
            lsp_types::Documentation::String(s) => s.clone(),
            lsp_types::Documentation::MarkupContent(mc) => mc.value.clone(),
        };
        if !doc_str.is_empty() {
            out.push('\n');
            out.push_str(&doc_str);
            out.push('\n');
        }
    }
    out
}

/// Inverse of `app_to_lsp_position` -- LSP utf-16 character
/// column → utf-8 byte column on the given line. Used by
/// `apply_lsp_text_edits` to convert TextEdit ranges back to
/// the App's `Position` shape.
pub(crate) fn lsp_position_to_app_byte(buffer: &Buffer, line: u32, character: u32) -> u32 {
    let line_text = buffer.line(line).unwrap_or_default();
    lattice_lsp::position::utf16_column_to_utf8_byte(&line_text, character)
}

pub(crate) fn app_to_lsp_position(buffer: &Buffer, p: Position) -> Option<lsp_types::Position> {
    let line_text = buffer.line(p.line)?;
    let character = lattice_lsp::position::utf8_byte_to_utf16_column(&line_text, p.byte);
    Some(lsp_types::Position {
        line: p.line,
        character,
    })
}

/// Flatten a `DocumentSymbolResponse` into our pre-rendered
/// `SymbolRow` shape. The legacy `Flat(Vec<SymbolInformation>)`
/// variant is one row per symbol with no nesting; the modern
/// `Nested(Vec<DocumentSymbol>)` variant carries
/// `children: Vec<DocumentSymbol>`, walked depth-first to
/// preserve outline ordering.
pub(crate) fn flatten_document_symbol_response(
    resp: lsp_types::DocumentSymbolResponse,
    path: &std::path::Path,
    out: &mut Vec<SymbolRow>,
) {
    match resp {
        lsp_types::DocumentSymbolResponse::Flat(syms) => {
            for sym in syms {
                if let Some(row) = symbol_information_to_row(&sym) {
                    out.push(row);
                }
            }
        }
        lsp_types::DocumentSymbolResponse::Nested(syms) => {
            fn walk(
                syms: Vec<lsp_types::DocumentSymbol>,
                path: &std::path::Path,
                depth: u32,
                out: &mut Vec<SymbolRow>,
            ) {
                for sym in syms {
                    out.push(SymbolRow {
                        name: sym.name.clone(),
                        kind_glyph: symbol_kind_glyph(sym.kind),
                        container: None,
                        depth,
                        path: path.to_path_buf(),
                        line: sym.selection_range.start.line,
                        col: sym.selection_range.start.character,
                    });
                    if let Some(children) = sym.children {
                        walk(children, path, depth + 1, out);
                    }
                }
            }
            walk(syms, path, 0, out);
        }
    }
}

/// Map a flat `SymbolInformation` (legacy outline + workspace
/// symbol shape) into our row type. Returns `None` when the
/// location's URI doesn't resolve to a path.
/// Convert a modern (LSP 3.17+) `WorkspaceSymbol` into a
/// `SymbolRow` (Phase 4.2 follow-up). When the symbol's
/// `location` came back as the `WorkspaceLocation` (URI-only)
/// variant, fires `workspaceSymbol/resolve` against the
/// originating server to upgrade to a real `Location` with
/// `range`. Returns `None` when:
/// - The URI doesn't map to a path.
/// - Resolve fails (server doesn't actually advertise it,
///   or returns a still-unresolved shape).
/// - Cancellation fires while we're awaiting resolve.
pub(crate) async fn workspace_symbol_to_row(
    handle: &lattice_lsp::ServerHandle,
    sym: lsp_types::WorkspaceSymbol,
    token: &lattice_protocol::CancellationToken,
) -> Option<SymbolRow> {
    use lsp_types::OneOf;
    let (path, line, col) = match &sym.location {
        OneOf::Left(loc) => (
            lattice_lsp::actor::uri_to_path(&loc.uri)?,
            loc.range.start.line,
            loc.range.start.character,
        ),
        OneOf::Right(wsl) => {
            let path = lattice_lsp::actor::uri_to_path(&wsl.uri)?;
            // Server's resolveProvider absent -> no point firing.
            // Fall back to (0, 0); the user can still navigate
            // to the file.
            if !handle.capabilities().workspace_symbol_resolve_provider() {
                (path, 0, 0)
            } else {
                match handle.workspace_symbol_resolve(sym.clone(), token.clone()).await {
                    Ok(resolved) => match resolved.location {
                        OneOf::Left(loc) => (
                            lattice_lsp::actor::uri_to_path(&loc.uri)
                                .unwrap_or(path),
                            loc.range.start.line,
                            loc.range.start.character,
                        ),
                        // Server replied without populating range
                        // -- spec violation, but defensive: fall
                        // back to (0, 0) instead of dropping.
                        OneOf::Right(_) => (path, 0, 0),
                    },
                    // Resolve failed -- log via the symbol's
                    // path-only fallback. Caller still gets a
                    // navigable row.
                    Err(_) => (path, 0, 0),
                }
            }
        }
    };
    Some(SymbolRow {
        name: sym.name,
        kind_glyph: symbol_kind_glyph(sym.kind),
        container: sym.container_name,
        depth: 0,
        path,
        line,
        col,
    })
}

pub(crate) fn symbol_information_to_row(
    sym: &lsp_types::SymbolInformation,
) -> Option<SymbolRow> {
    let path = lattice_lsp::actor::uri_to_path(&sym.location.uri)?;
    Some(SymbolRow {
        name: sym.name.clone(),
        kind_glyph: symbol_kind_glyph(sym.kind),
        container: sym.container_name.clone(),
        depth: 0,
        path,
        line: sym.location.range.start.line,
        col: sym.location.range.start.character,
    })
}

/// Extract a placeholder string from a `PrepareRenameResponse`.
/// The spec gives three shapes: a Range (no placeholder, just
/// "you can rename here"), a Range+Placeholder (preferred), or
/// a DefaultBehavior signal (server defers to the editor's
/// word-under-cursor heuristic). We pull the placeholder when
/// present, else `None` so the App's caller can fall back to
/// the heuristic.
pub(crate) fn prepare_rename_placeholder(
    resp: &lsp_types::PrepareRenameResponse,
) -> Option<String> {
    match resp {
        lsp_types::PrepareRenameResponse::RangeWithPlaceholder { placeholder, .. } => {
            Some(placeholder.clone())
        }
        lsp_types::PrepareRenameResponse::Range(_) => None,
        lsp_types::PrepareRenameResponse::DefaultBehavior { .. } => None,
    }
}

/// Flatten a `WorkspaceEdit` into a per-file
/// `Vec<(Uri, Vec<TextEdit>)>`. Handles both the legacy `changes`
/// HashMap shape and the modern `document_changes` shape (which
/// also carries DocumentChangeOperation::Op create/rename/delete
/// -- those are skipped in v1; rename returns plain text edits
/// for ~100% of identifiers).
///
/// Empty Vec means "nothing to apply" (the App echoes `Empty`).
pub(crate) fn flatten_workspace_edit(
    we: lsp_types::WorkspaceEdit,
) -> Vec<(lsp_types::Uri, Vec<lsp_types::TextEdit>)> {
    let mut out: Vec<(lsp_types::Uri, Vec<lsp_types::TextEdit>)> = Vec::new();
    if let Some(changes) = we.changes {
        for (uri, edits) in changes {
            if !edits.is_empty() {
                out.push((uri, edits));
            }
        }
    }
    if let Some(doc_changes) = we.document_changes {
        match doc_changes {
            lsp_types::DocumentChanges::Edits(edits) => {
                for te_doc in edits {
                    let uri = te_doc.text_document.uri.clone();
                    let raw_edits: Vec<lsp_types::TextEdit> = te_doc
                        .edits
                        .into_iter()
                        .filter_map(|e| match e {
                            lsp_types::OneOf::Left(te) => Some(te),
                            // AnnotatedTextEdit -- strip the
                            // annotation; v1 doesn't surface
                            // change-annotations to the user.
                            lsp_types::OneOf::Right(ate) => Some(ate.text_edit),
                        })
                        .collect();
                    if !raw_edits.is_empty() {
                        out.push((uri, raw_edits));
                    }
                }
            }
            // create-file / rename-file / delete-file ops are
            // skipped in v1 -- the rename use case is identifier
            // rewrites, which servers return as plain text edits.
            lsp_types::DocumentChanges::Operations(_) => {}
        }
    }
    out
}

/// Single-character glyph for an LSP `CodeActionKind`. Maps
/// the standard kinds (quickfix, refactor*, source*) to a
/// short visual marker for the picker margin. Unknown / custom
/// kinds and bare `Command` payloads land on `?`.
pub(crate) fn code_action_kind_glyph(
    kind: Option<&lsp_types::CodeActionKind>,
) -> &'static str {
    use lsp_types::CodeActionKind as K;
    let Some(kind) = kind else {
        return "?";
    };
    if *kind == K::QUICKFIX {
        "🛠"
    } else if *kind == K::REFACTOR {
        "♻"
    } else if *kind == K::REFACTOR_EXTRACT {
        "↗"
    } else if *kind == K::REFACTOR_INLINE {
        "↘"
    } else if *kind == K::REFACTOR_REWRITE {
        "↺"
    } else if *kind == K::SOURCE {
        "★"
    } else if *kind == K::SOURCE_ORGANIZE_IMPORTS {
        "≡"
    } else if *kind == K::SOURCE_FIX_ALL {
        "✓"
    } else {
        "?"
    }
}

/// Single-character glyph for an LSP `CompletionItemKind`.
/// Same shape as `symbol_kind_glyph` but maps the completion-
/// item kind enum (which is wider -- snippets, keywords,
/// folders, etc.). Kept narrow on purpose; richer per-kind
/// styling lives in the renderer once buffer-level Insert
/// completion lands.
pub(crate) fn completion_kind_glyph(kind: Option<lsp_types::CompletionItemKind>) -> &'static str {
    use lsp_types::CompletionItemKind as K;
    match kind {
        Some(K::FUNCTION) | Some(K::METHOD) | Some(K::CONSTRUCTOR) => "ƒ",
        Some(K::VARIABLE) | Some(K::FIELD) | Some(K::PROPERTY) => "v",
        Some(K::CONSTANT) => "K",
        Some(K::CLASS) | Some(K::INTERFACE) => "🅒",
        Some(K::STRUCT) => "🅢",
        Some(K::ENUM) | Some(K::ENUM_MEMBER) => "🅔",
        Some(K::MODULE) => "📦",
        Some(K::FILE) | Some(K::FOLDER) => "📄",
        Some(K::SNIPPET) => "✂",
        Some(K::KEYWORD) => "K",
        Some(K::TEXT) => "≡",
        Some(K::REFERENCE) => "→",
        _ => "?",
    }
}

/// Single-character glyph for an LSP `SymbolKind`. Picked to
/// fit a fixed-width column in picker rows so the marginalia
/// column stays aligned. Falls back to `?` for kinds we don't
/// have a specific glyph for.
pub(crate) fn symbol_kind_glyph(kind: lsp_types::SymbolKind) -> &'static str {
    use lsp_types::SymbolKind as K;
    match kind {
        K::FILE => "📄",
        K::MODULE | K::NAMESPACE | K::PACKAGE => "📦",
        K::CLASS | K::INTERFACE => "🅒",
        K::METHOD | K::FUNCTION => "ƒ",
        K::CONSTRUCTOR => "🅒",
        K::PROPERTY | K::FIELD => "•",
        K::VARIABLE => "v",
        K::CONSTANT => "K",
        K::STRING | K::NUMBER | K::BOOLEAN | K::ARRAY | K::OBJECT => "≡",
        K::ENUM | K::ENUM_MEMBER => "🅔",
        K::STRUCT => "🅢",
        K::EVENT => "🅔",
        K::OPERATOR => "⊕",
        K::TYPE_PARAMETER => "T",
        _ => "?",
    }
}

/// Word (alphanumeric + `_` run) under `cursor` in `buffer`, or
/// `None` when the cursor isn't on a word byte. Mirrors vim's
/// `<cword>` for the simple case that `:references` needs to
/// label its results buffer ("References for \"foo\""). Walks
/// the line once at the cursor column; doesn't scan forward to
/// the next word like `do_search_word_under_cursor` does --
/// "no symbol under cursor" is preferable to a label that
/// jumps to a different identifier than the user pointed at.
pub(crate) fn word_under_cursor(buffer: &Buffer, cursor: Position) -> Option<String> {
    let line = buffer.line(cursor.line)?;
    let bytes = line.as_bytes();
    let byte_idx = cursor.byte as usize;
    if byte_idx >= bytes.len() || !is_word_char_byte(bytes[byte_idx]) {
        return None;
    }
    let mut start = byte_idx;
    while start > 0 && is_word_char_byte(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = byte_idx;
    while end < bytes.len() && is_word_char_byte(bytes[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes[start..end]).into_owned())
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

pub(super) fn previous_position(buf: &Buffer, p: Position) -> Position {
    if p.byte > 0 {
        Position::new(p.line, p.byte - 1)
    } else if p.line > 0 {
        let prev_line = p.line - 1;
        Position::new(prev_line, line_byte_len(buf, prev_line))
    } else {
        p
    }
}


#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use super::test_helpers::{
        app_with, attach_test_syntax, invoke_motion, submit_ex, write_temp_file,
    };
    use crate::help::HelpContent;

    /// Sanity check: a bare motion drives the cursor through
    /// the full translate + apply path. If this fails, the
    /// harness itself is broken; every other `press_*` test
    /// is suspect.
    /// Sanity check: an operator + motion deletes the right
    /// span end-to-end. Exercises the `[d]` action-kind
    /// short-circuit (pushes `d` into `partial_chord`) plus
    /// the `[d, w]` resolution under the prefix.
    // -- count flow seam ----------------------------------------
    //
    // These pin the path translate -> attach_count -> dispatcher
    // multiplication. Slice 8.i.4.d documented the failure mode if
    // the action-kind short-circuit didn't bypass
    // `run_document_invocation`'s `pending_count = 0;`: the reset
    // would fire before `AbsorbOperatorPrefix` could latch op_count,
    // breaking `2dw`-style flows. These tests would catch a
    // regression of that fix.

    /// `3j` moves down 3 lines: pending_count fed straight to a
    /// motion (no operator latch path).
    /// `3dd` deletes 3 lines: count latches into op_count via
    /// the `d` action-kind dispatch; the second `d` resolves
    /// linewise with the count baked in by `attach_count`. Pins
    /// slice 8.i.4.f's removal of the dispatcher's redundant
    /// multiplication, AND slice 8.i.4.g's `dd`-consumes-newline
    /// fix (line count drops by 3 -- the buffer goes from 5
    /// lines to 2, with no leading empty line).
    /// `d2w` deletes 2 words: the `2` between operator and motion
    /// must reach the digit accumulator, not get eaten by the
    /// partial_chord lookup. Pins slice 8.i.4.f's hoist of digit
    /// handling above the partial_chord short-circuit.
    /// `2d2w` deletes 4 words: op_count=2 multiplies with
    /// motion_count=2. Pins both 8.i.4.f fixes end-to-end --
    /// digit-after-operator survives, and the dispatcher honours
    /// the input-side baked count without re-multiplying.
    /// After `3j`, a bare `j` moves only one line: pending_count
    /// must reset after the motion fires.
    // -- partial_chord state machine ----------------------------
    //
    // Pins the multi-keystroke prefix walk. `gg` is a Normal-mode
    // multi-key motion (no operator); `dd` is the operator
    // self-key linewise resolution; `df,` is a 3-keystroke chord
    // (operator -> find-char prefix -> captured delimiter).

    /// `gg` jumps to the first line: prefix `g` parks in
    /// partial_chord, second `g` resolves the terminal.
    /// `df,` deletes up to (exclusive of) the comma: 3-keystroke
    /// chord across the operator + find-char captured-delimiter
    /// sub-tree (slice 8.i.4.c). Lattice's `f`-motion is exclusive
    /// (see `df_deletes_through_target_char` near the dispatch
    /// tests), so the comma stays.
    // -- <C-w> sub-tree (action-kind short-circuit, 8.i.4.d) ----

    /// `<C-w>v` splits the active pane vertically. Exercises the
    /// `<C-w>` action-kind short-circuit + the AfterCtrlW layer.
    // -- mode transition seam -----------------------------------

    /// `i` enters Insert, typed chars land in the buffer, `<Esc>`
    /// returns to Normal. Pins the modal state machine across a
    /// mode round-trip in a single keystroke stream.
    /// Subscribe a channel sink to the App's event bus. Returns

    // ---- Slice B.2 part 2: edit-delta accumulation -------------
    //
    // Pin that EditDeltas accumulate on App.pending_syntax_edits
    // across edits, drain on maybe_reparse_syntax, and the
    // version baseline tracks correctly. The actual incremental
    // reparse correctness is covered by lattice-syntax's parity
    // tests; these App-level tests pin the plumbing.




    // ---- Initial state ----

    #[test]
    fn new_app_starts_at_origin_in_normal_mode() {
        let a = app_with("abc", 10);
        assert_eq!(a.cursor, Position::ZERO);
        assert_eq!(a.scroll, 0);
        assert!(!a.should_quit);
        assert_eq!(a.modal, ModalState::Normal);
        assert!(a.partial_chord.is_empty());
    }

    #[test]
    fn every_advertised_completion_source_resolves_at_boot() {
        // Every `ArgSpec::completion = Some(...)` declared in
        // `lattice_grammar::ex_commands` must resolve to a
        // generator registered at App boot. Otherwise typing
        // `<Tab>` on that arg silently produces no candidates --
        // the bug class that motivated this slice.
        let a = app_with("hi", 5);
        for name in a.registry.names() {
            let id = a.registry.id_by_name(name).unwrap();
            let Some(spec) = a.registry.ex_command_spec(id) else {
                continue;
            };
            for arg in &spec.args_schema {
                if let Some(source) = arg.completion {
                    assert!(
                        a.completion_registry.generator_by_name(source).is_some(),
                        "{name}'s arg `{}` advertises completion source `{source}` \
                         but no generator with that name is registered at boot",
                        arg.name,
                    );
                }
            }
        }
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

    // ---- Insert mode ----

    // ---- Operator + motion composition ----

    // ---- Undo / Redo ----

    // ---- Viewport scrolling ----

    // ---- Command-line minibuffer ----


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
    // ---- change operator end-to-end ----


    // ---- Substitute (:s/foo/bar/[g]) ----

    // ---- Line join (J / gJ) ----

    // ---- WORD motions (W, B, E) end-to-end ----

    // ---- Position history (Ctrl-O / Ctrl-I) ----

    // ---- partial_chord lifecycle (regression) ----
    //
    // Slice 8.i.4 retired the legacy `Pending` enum; the regression
    // these tests pin is now expressed against `partial_chord`:
    // any non-`AbsorbPartialChord(_)` action must clear the
    // chord stack so a leftover prefix doesn't mis-route the
    // next keystroke.

    #[test]
    fn zz_clears_partial_chord_so_next_key_is_a_motion() {
        // Regression: previously `zz` left pending=AfterZ, so `j` after
        // `zz` was interpreted as `zj` (GotoNextFold) and emitted "no
        // more folds". Now expressed against partial_chord.
        let mut a = app_with("a\nb\nc\nd\ne", 10);
        a.apply(Action::AbsorbPartialChord(crate::chord::KeyChord::char('z')));
        a.apply(Action::ScrollCursorTo(ScrollPos::Center));
        assert!(a.partial_chord.is_empty());
    }

    #[test]
    fn play_macro_clears_partial_chord() {
        let mut a = app_with("hello", 10);
        a.apply(Action::AbsorbPartialChord(crate::chord::KeyChord::char('@')));
        // No macro recorded; this errors but should still clear partial_chord.
        a.apply(Action::PlayMacro('z'));
        assert!(a.partial_chord.is_empty());
    }


    // ---- §5.1.1 unified position history ----

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

    // ---- Multiple registers ----

    // ---- ~ toggle case at cursor ----

    // ---- Word-search (* / #) and matching-bracket (%) ----

    #[test]
    fn hash_finds_previous_occurrence_of_word_under_cursor() {
        let mut a = app_with("foo bar foo bar", 10);
        a.cursor = Position::new(0, 8); // on 'f' of second "foo"
        a.apply(Action::SearchWordUnderCursor(SearchDirection::Backward));
        assert_eq!(a.cursor, Position::ZERO);
    }

    // ---- Viewport motions ----

    // ---- Replace mode ----

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
    fn invalid_mark_name_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::SetMark(' '));
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(a.marks.is_empty());
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

    // ---- Dot-repeat ----

    #[test]
    fn motion_does_not_record_last_change() {
        let mut a = app_with("hello world", 10);
        a.apply(invoke_motion(a.builtins.word_forward));
        assert!(a.last_change.is_none());
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

    // Slice 8.i.4.f: the next batch of tests bake `Count(N)`
    // directly into the operator invocation -- mirroring what the
    // input-side `attach_count` produces when the keystroke
    // pipeline runs. The dispatcher's job is now to honour the
    // baked count and drain `pending_count` / `op_count` from
    // App state. The full keystroke -> count pipeline lives in
    // the `key_harness_*` press-harness tests.

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
        // `docs/user/folding.md`: linear motions (j/k/h/l/w/b) do
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

    // ---- Bracketed-paste burst (Action::PasteText) ----

    // ---- Blockwise visual operators (DESIGN.md §15:18) ----

    /// Drive into Blockwise visual at `anchor`, then move the cursor to
    /// `head` so the rectangle is `[anchor, head]`. Returns the App
    /// ready for an operator dispatch.
    // ---- Help overlay (DESIGN.md §5.11) ----

    // ---- Command-line completion (DESIGN.md §5.11.3) ----

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

    // ---- Chord-capture (DESIGN.md §B.1, ArgKind::Chord) ----

    // ---- Missing-arg chord prompt (DESIGN.md §B.1) ----

    #[test]
    fn cancel_clears_armed_chord_prompt() {
        let mut a = app_in_command_mode("describe-key");
        a.apply(Action::CommandLineSubmit);
        assert!(a.auto_submit_after_chord);
        a.apply(Action::CommandLineCancel);
        assert!(!a.auto_submit_after_chord);
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

    // ---- Hybrid <C-h> (DESIGN.md §5.11.3 Q11) ----

    // ---- delete_trailing_word helper ----

    // ---- Alias preference for command candidates ----

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
            a.popup_buffer.is_some(),
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

    // ---- Pane tree (DESIGN.md §5.9, B.1.b) ----

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
        assert!(a.popup_buffer.is_some(), "popup persists in State B");
        assert_eq!(a.cursor.line, 1);
    }

    // ---- Multiple Document buffers (DESIGN.md §5.9, B.1.c) ----


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

    // ---- Buffer activation lifecycle ----
    //
    // Regression coverage for the `<C-l>`-needed bug: opening a
    // second file via `:e <path>` left the new buffer with empty
    // folds and stale highlight caches because no single hook ran
    // on activation. `App::activate_buffer_state` is now the one
    // place to add buffer-level state that needs to come up with
    // the buffer.

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

    // ---- Typed options registry (DESIGN.md §5.12, B.2) ----



    // ---- Hover popup (DESIGN.md §5.9.6, B.3) ----

    // ---- LSP hover (Phase 4.2.b) ----

    // ---- LSP goto-definition (Phase 4.2.c) ----

    #[test]
    fn symbol_kind_glyph_distinct_for_common_kinds() {
        // We don't assert the *exact* glyph (those may evolve);
        // we just want the common kinds to map to *distinct*
        // glyphs so a glance distinguishes a fn from a struct.
        use lsp_types::SymbolKind as K;
        let f = super::symbol_kind_glyph(K::FUNCTION);
        let s = super::symbol_kind_glyph(K::STRUCT);
        let m = super::symbol_kind_glyph(K::MODULE);
        let v = super::symbol_kind_glyph(K::VARIABLE);
        assert_ne!(f, s);
        assert_ne!(f, m);
        assert_ne!(f, v);
    }

    #[test]
    fn prepare_rename_placeholder_extracted_from_range_with_placeholder() {
        let r = lsp_types::Range {
            start: lsp_types::Position {
                line: 0,
                character: 0,
            },
            end: lsp_types::Position {
                line: 0,
                character: 3,
            },
        };
        let resp = lsp_types::PrepareRenameResponse::RangeWithPlaceholder {
            range: r,
            placeholder: "foo".into(),
        };
        assert_eq!(
            super::prepare_rename_placeholder(&resp),
            Some("foo".to_string())
        );
        let resp = lsp_types::PrepareRenameResponse::Range(r);
        assert_eq!(super::prepare_rename_placeholder(&resp), None);
    }

    #[test]
    fn picker_dismiss_clears_pending_tag_origin() {
        let mut a = app_with("foo\n", 10);
        a.pending_tag_origin = Some(super::TagStackEntry {
            buffer: a.active_buffer,
            buffer_id: a.active_pane_buffer_id(),
            position: Position::new(0, 0),
            label: "foo".into(),
        });
        // Open + dismiss a picker. We don't need real candidates;
        // a non-Some picker dismiss already takes the picker
        // first. Simulate by setting picker Some.
        let mut p = crate::picker::Picker::new(
            "test",
            crate::picker::PickerSource::LspLocations,
            crate::picker::PickerAction::JumpToLspLocation,
        );
        p.set_lsp_locations(Vec::new());
        a.picker = Some(p);
        a.apply(Action::PickerDismiss);
        assert!(a.pending_tag_origin.is_none());
    }

    #[test]
    fn diagnostics_picker_clears_stale_tag_origin() {
        // If a stale nav-intent origin was set (race scenario:
        // gd fired but drain hasn't run; user invokes
        // :diagnostics), opening the diagnostics picker MUST
        // clear the origin so a later JumpToLspLocation accept
        // doesn't push the wrong entry.
        let mut a = app_with("foo\n", 10);
        a.pending_tag_origin = Some(super::TagStackEntry {
            buffer: a.active_buffer,
            buffer_id: a.active_pane_buffer_id(),
            position: Position::new(0, 0),
            label: "stale".into(),
        });
        a.do_list_diagnostics();
        assert!(a.pending_tag_origin.is_none());
    }

    // ---- :help (DESIGN.md §5.11) ----

    #[test]
    fn h_alias_resolves_to_help() {
        let mut a = app_with("xx", 10);
        a.command_line = "h folding".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("help open");
        assert_eq!(h.title, "help folding");
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


    // ---- LSP wiring tests (Phase 4.1.i) ---------------------

    #[test]
    fn pathless_document_does_not_register_buffer_uri() {
        // Path-less scratch document -> `App::new` publishes no
        // `Event::DocumentOpened` (well, *publishes one for
        // observability, but with `path: None`*) and registers
        // no `buffer_uris` entry. The attach driver ignores
        // path-less events.
        let app = App::new(Document::from_text("fn main() {}"));
        assert!(app.buffer_uri(app.document_buffer_id).is_none());
    }

    // ---- LSP diagnostic navigation tests (Phase 4.1.d.iv) ----

    /// Helper: seed N diagnostics into the App's LSP layer at
    /// the given lines + map a fake URI to the active buffer.
    // ---- LSP introspection tests (Phase 4.1.g) ---------------

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

    // ---- Edit-dispatch wiring tests (Phase 4.1.i.2) ----------

    /// Path-bearing initial documents publish
    /// `Event::DocumentOpened` from `App::new` and register the
    /// URI eagerly. The attach driver picks the event up off
    /// the bus and submits to the supervisor on the LSP runtime
    /// -- the UI thread never parks. We verify the eager URI
    /// registration here (`buffer_uris` is observable on the
    /// public App API); full driver -> supervisor behaviour
    /// (the publish path itself) is covered in
    /// `lattice_lsp::attach_driver::tests`.
    #[test]
    fn path_bearing_initial_document_registers_uri_eagerly() {
        use std::str::FromStr;
        // Build a Document with a fixed path. We can't use
        // `Document::open` here without I/O; the builder
        // surface (DocumentBuilder::with_path) lets us seed
        // the path directly.
        let path = std::path::PathBuf::from("/tmp/lattice-test/initial.rs");
        let doc = lattice_core::DocumentBuilder::default()
            .with_path(path.clone())
            .with_text("fn main() {}")
            .build();
        let app = App::new(doc);
        let expected =
            <lattice_lsp::Uri as FromStr>::from_str(
                lattice_lsp::actor::uri_from_path(&path).as_str(),
            )
            .unwrap();
        assert_eq!(
            app.buffer_uri(app.document_buffer_id),
            Some(&expected),
            "path-bearing initial document must register URI eagerly"
        );
    }

    // ---- Snippet host integration (Phase 4.2.g.4) ----


    /// Test helper: attach a freshly-parsed `Syntax` for `lang`
    /// to `a`, wrapped in a [`SyntaxHandle`]. Mirrors the audit

    #[test]
    fn dedup_helper_keeps_first_occurrence_by_text() {
        // Direct unit test on the dedup helper. Ranker has
        // already sorted; we feed in a vec mimicking the
        // post-rank state (highest-ranked entry first per
        // text), confirm the deduped vec keeps the first
        // occurrence and preserves order otherwise.
        use lattice_completion::{
            CandidateKind, MatchScore, RawCandidate, RenderedCandidate, ScoredCandidate,
            SourceId,
        };
        let mk = |text: &str, source: &str, score: u32| {
            let raw = RawCandidate::plain(text, CandidateKind::Plain)
                .with_source(SourceId::new(source));
            RenderedCandidate::from_scored(ScoredCandidate {
                raw,
                score: MatchScore(score),
                match_ranges: Vec::new(),
            })
        };
        let mut rendered = vec![
            mk("outer", "gen:buffer-words", 200),
            mk("alpha", "gen:buffer-words", 180),
            mk("outer", "gen:tree-sitter-symbol", 180),
            mk("beta", "gen:tree-sitter-symbol", 150),
            mk("alpha", "gen:tree-sitter-symbol", 150),
        ];
        super::dedup_rendered_by_text(&mut rendered);
        let texts: Vec<&str> = rendered.iter().map(|c| c.raw.text.as_str()).collect();
        let sources: Vec<&str> = rendered
            .iter()
            .map(|c| c.raw.source.as_ref().map(|s| s.as_str()).unwrap_or(""))
            .collect();
        assert_eq!(texts, vec!["outer", "alpha", "beta"]);
        // Each kept row carries the higher-ranked source's
        // tag (the first occurrence of each text).
        assert_eq!(
            sources,
            vec![
                "gen:buffer-words",
                "gen:buffer-words",
                "gen:tree-sitter-symbol",
            ],
        );
    }

    /// Inject an `InboundApplyEdit` into the App's drain
    /// receiver. Replaces whatever was there; tests start with
    /// an empty receiver so this is fine.

    fn install_lsp_candidate_with_commit_chars(
        a: &mut App,
        text: &str,
        commit_chars: Vec<char>,
        anchor: Position,
    ) {
        let cursor = a.cursor;
        let snap = a.document.snapshot();
        let line = snap.buffer.line(cursor.line).unwrap_or_default();
        let query = line
            .get(anchor.byte as usize..cursor.byte as usize)
            .unwrap_or("")
            .to_string();
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            anchor,
            cursor,
            query,
        );
        let meta = LspCompletionMeta {
            label: text.to_string(),
            insert_text: text.to_string(),
            filter_text: None,
            sort_text: None,
            detail: None,
            documentation: None,
            kind: Some(lsp_types::CompletionItemKind::FUNCTION),
            deprecated: false,
            preselect: false,
            commit_characters: commit_chars,
            additional_text_edits: Vec::new(),
            command: None,
            insert_text_format: lsp_types::InsertTextFormat::PLAIN_TEXT,
            replace_range: None,
            server_id: "test-server".to_string(),
            original_item: lsp_types::CompletionItem::new_simple(
                text.to_string(),
                String::new(),
            ),
            resolved: false,
        };
        let payload = lattice_lsp::completion::encode_meta(&meta);
        let mut raw = lattice_completion::RawCandidate::plain(
            text,
            lattice_completion::CandidateKind::Plain,
        )
        .with_source(lattice_completion::SourceId::new(
            lattice_completion::LSP_COMPLETION_SOURCE_ID,
        ));
        raw.data = lattice_completion::CandidateData::Extension {
            kind_id: LSP_COMPLETION_KIND_ID,
            payload,
        };
        state.raw.push(raw);
        a.refilter_insert_completion(&mut state);
        a.insert_completion = Some(state);
    }

    #[test]
    fn commit_char_in_lsp_item_accepts_then_inserts() {
        let mut a = app_with("foo", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        install_lsp_candidate_with_commit_chars(
            &mut a,
            "foo",
            vec!['.', '('],
            Position::new(0, 0),
        );
        a.do_completion_accept_then_insert('.');
        // Popup closed; accept replaced the partial with the
        // full LSP insert, then `.` was appended.
        assert!(a.insert_completion.is_none(), "popup closed on commit");
        assert_eq!(a.document.snapshot().buffer.as_string(), "foo.");
    }

    #[test]
    fn non_commit_char_is_plain_insert_popup_refilters() {
        let mut a = app_with("foo", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        install_lsp_candidate_with_commit_chars(
            &mut a,
            "foo",
            vec!['.'],
            Position::new(0, 0),
        );
        a.do_completion_accept_then_insert('a');
        // `a` isn't a commit char -> the focused candidate
        // wasn't accepted; `a` was inserted plainly. The
        // refresh hook closes the popup because the new
        // query "fooa" no longer matches the candidate
        // "foo" prefix-wise (matcher returns no rows).
        assert_eq!(a.document.snapshot().buffer.as_string(), "fooa");
    }

    #[test]
    fn extra_commit_chars_option_contributes_globally() {
        let mut a = app_with("foo", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        // Server says no commit chars; the global option
        // adds `,`.
        install_lsp_candidate_with_commit_chars(
            &mut a,
            "foo",
            Vec::new(),
            Position::new(0, 0),
        );
        a.do_set("completion.extra_commit_chars=,");
        a.do_completion_accept_then_insert(',');
        assert!(a.insert_completion.is_none());
        assert_eq!(a.document.snapshot().buffer.as_string(), "foo,");
    }

    #[test]
    fn sync_candidate_honors_extra_commit_chars_only() {
        // A buffer-words candidate has no per-item commit
        // list (sync sources don't carry one). The global
        // extras still apply.
        let mut a = app_with("alpha bravo ", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 12);
        a.do_completion_trigger();
        // Server-supplied list is empty for sync candidates;
        // set the global extras to include `;`.
        a.do_set("completion.extra_commit_chars=;");
        // Focus the `alpha` candidate (insert at cursor).
        if let Some(state) = a.insert_completion.as_mut() {
            state.selected = state
                .rendered
                .iter()
                .position(|r| r.raw.text == "alpha")
                .expect("alpha");
        }
        a.do_completion_accept_then_insert(';');
        // Popup closed; `alpha` inserted then `;`.
        assert!(a.insert_completion.is_none());
        let text = a.document.snapshot().buffer.as_string();
        assert!(text.ends_with("alpha;"), "got `{text}`");
    }


    #[test]
    fn populate_insert_completion_sync_drops_disabled_source() {
        // Inject a per-language override that limits rust to
        // snippets only -> buffer-words emit is suppressed even
        // though the buffer is full of word-completion fodder.
        let mut a = app_with("foo bar baz qux quux ", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 21);
        // Pretend the active language is rust by overriding
        // the `rust` slot. (Test buffer has no path so
        // active_language_id() returns ""; insert that as the
        // key directly to land the override.)
        a.per_language_completion.insert(
            String::new(),
            lattice_completion::PerLanguageOverrides {
                sources: Some(vec![lattice_completion::SourceId::new(
                    lattice_completion::SNIPPET_SOURCE_ID,
                )]),
                ..Default::default()
            },
        );
        a.do_completion_trigger();
        // Popup either closed (no candidates) or has only
        // snippet items. Buffer-words mustn't appear.
        if let Some(state) = a.insert_completion.as_ref() {
            for cand in &state.rendered {
                let src = cand
                    .raw
                    .source
                    .as_ref()
                    .map(|s| s.as_str())
                    .unwrap_or("");
                assert_ne!(
                    src,
                    lattice_completion::BufferWordsSource::ID,
                    "buffer-words filtered out",
                );
            }
        }
    }


    // ---- M.3.0: built-in major modes registered at boot ----

    // ---- M.3.1: ReadOnly option flows from major modes ----


    #[test]
    fn document_buffer_active_mode_is_text_mode() {
        // Plain document with no path ⇒ Lang::Plain ⇒ text-mode.
        let a = app_with("hi", 5);
        let buf = a.document_buffer_id;
        let active = a.active_modes.get(&buf).expect("active_modes populated");
        assert_eq!(active.major(), Some(lattice_mode::TextMode::mode_id()));
    }



    // ---- M.3.2.b.1: help-mode locals seeded at construction ----

    #[test]
    fn renderer_reads_help_data_through_buffer_locals() {
        // M.3.2.c.5: BufferLocals are the canonical owner of help
        // per-buffer state -- the HelpBuffer struct no longer
        // carries `links` / `anchors` / `highlights` fields. Open
        // a help buffer (which seeds two parsed links into
        // locals), then mutate the locals; readers must reflect
        // the mutation since there's no struct-field fallback.
        let mut a = app_with("hi", 5);
        let help = crate::help::HelpContent::from_lines(
            "test-render",
            vec!["[link-a](command:a) and [link-b](command:b)".into()],
        );
        let help_id = a.open_help_in_pane(help);

        let synthetic = crate::modes::HelpLinks(vec![crate::help::HelpLink {
            range: lattice_protocol::position::Range::new(
                lattice_protocol::position::Position::ZERO,
                lattice_protocol::position::Position::new(0, 5),
            ),
            target: crate::help::HelpLinkTarget::Unresolved("synthetic".into()),
        }]);
        a.buffer_locals
            .get_mut(&help_id)
            .expect("locals seeded")
            .insert(synthetic);

        let _ = a.popup_help().expect("popup_buffer set");
        let locals = a
            .buffer_locals
            .get(&help_id)
            .expect("locals seeded by open_help_in_pane");
        let from_locals = locals.get::<crate::modes::HelpLinks>().unwrap();
        assert_eq!(from_locals.0.len(), 1);
        assert_eq!(
            from_locals.0[0].target,
            crate::help::HelpLinkTarget::Unresolved("synthetic".into())
        );
    }

    // ---- M.3.2.c.2: file-tree-mode locals seeded + readers ----

    // ---- M.3.2.c.3: oil-mode locals seeded ----


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
    fn capital_w_skips_punctuation() {
        let mut a = app_with("foo,bar baz", 10);
        a.apply(invoke_motion(a.builtins.big_word_forward));
        assert_eq!(a.cursor, Position::new(0, 8));
    }

    #[test]
    fn fold_action_clears_partial_chord() {
        let mut a = app_with("a\nb\nc", 10);
        a.apply(Action::AbsorbPartialChord(crate::chord::KeyChord::char('z')));
        a.apply(Action::OpenFoldAtCursor);
        assert!(a.partial_chord.is_empty());
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
    fn change_records_last_change() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.builtins.change.0).with_target(
            lattice_grammar::Target::Motion(a.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        // change drops to Insert, but the change itself is recorded.
        assert!(a.last_change.is_some());
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

    fn app_in_command_mode(line: &str) -> App {
        let mut a = app_with("xx", 10);
        a.modal = ModalState::Command;
        a.command_line = line.into();
        a
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
    fn close_last_pane_is_a_noop_with_warning() {
        let mut a = app_with("xx", 10);
        a.apply(Action::ClosePane);
        assert_eq!(a.pane_tree.len(), 1);
        let msg = a.last_message.as_ref().expect("warn echo");
        assert!(msg.text.contains("only one pane"));
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
        assert!(a.popup_buffer.is_none());
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
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        assert!(a.syntax.is_some(), "fixture syntax wired");
        // Open a help buffer in pane (mimics `:lsp-log rust`).
        let _help_id = a.open_help_in_pane(HelpContent::from_lines(
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
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        // Open the tree, then dismiss.
        a.command_line = format!("Filetree {}", dir.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(matches!(a.active_buffer, crate::buffers::BufferKind::FileTree));
        // `:TreeClose` (the path `q` takes in the tree).
        a.command_line = "FiletreeClose".into();
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
        a.command_line = format!("Filetree {}", dir.display());
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.buffers.file_tree_ids_sorted().len(), 1);
        a.apply(Action::ClosePane);
        // Tree stays in the registry post-close.
        assert_eq!(a.buffers.file_tree_ids_sorted().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
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
    fn docs_toggle_pulls_body_from_cached_metadata_documentation() {
        let mut a = app_with("xx", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::ZERO;
        // Seed popup state with a single LSP candidate that
        // already has documentation cached.
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            Position::ZERO,
            Position::ZERO,
            String::new(),
        );
        let meta = super::LspCompletionMeta {
            label: "foo".into(),
            insert_text: "foo".into(),
            filter_text: None,
            sort_text: None,
            detail: Some("fn foo() -> i32".into()),
            documentation: Some("Returns 42.".into()),
            kind: Some(lsp_types::CompletionItemKind::FUNCTION),
            deprecated: false,
            preselect: false,
            commit_characters: Vec::new(),
            additional_text_edits: Vec::new(),
            command: None,
            insert_text_format: lsp_types::InsertTextFormat::PLAIN_TEXT,
            replace_range: None,
            server_id: "test-server".to_string(),
            original_item: lsp_types::CompletionItem::default(),
            resolved: true,
        };
        let mut raw = lattice_completion::RawCandidate::plain(
            "foo",
            lattice_completion::CandidateKind::Plain,
        );
        raw.display = "foo".into();
        raw.data = lattice_completion::CandidateData::Extension {
            kind_id: super::LSP_COMPLETION_KIND_ID,
            payload: lattice_lsp::completion::encode_meta(&meta),
        };
        let scored = lattice_completion::ScoredCandidate {
            raw,
            score: lattice_completion::MatchScore(100),
            match_ranges: Vec::new(),
        };
        state
            .rendered
            .push(lattice_completion::RenderedCandidate::from_scored(scored));
        // CSM.8b.5: candidate payload IS the meta -- no sidecar push.
        let _ = meta;
        a.insert_completion = Some(state);
        a.do_completion_toggle_docs();
        let body = a
            .insert_completion
            .as_ref()
            .and_then(|s| s.doc_popup.as_ref())
            .and_then(|d| d.body.clone())
            .expect("body populated");
        assert!(body.contains("fn foo() -> i32"));
        assert!(body.contains("Returns 42."));
    }

    #[test]
    fn docs_toggle_a_second_time_closes_popup() {
        let mut a = app_with("xx", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::ZERO;
        a.insert_completion = Some(
            lattice_completion::InsertCompletionState::open(
                lattice_completion::CompletionTrigger::Manual,
                Position::ZERO,
                Position::ZERO,
                String::new(),
            ),
        );
        a.do_completion_toggle_docs();
        // Even with no candidate, the popup opens with an
        // empty body slot. Toggling again closes it.
        let was_open = a
            .insert_completion
            .as_ref()
            .map(|s| s.doc_popup.is_some())
            .unwrap_or(false);
        assert!(was_open);
        a.do_completion_toggle_docs();
        let now_closed = a
            .insert_completion
            .as_ref()
            .map(|s| s.doc_popup.is_none())
            .unwrap_or(true);
        assert!(now_closed);
    }

    #[test]
    fn docs_scroll_clamps_at_zero_and_advances_by_eight() {
        let mut a = app_with("xx", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::ZERO;
        a.insert_completion = Some(
            lattice_completion::InsertCompletionState::open(
                lattice_completion::CompletionTrigger::Manual,
                Position::ZERO,
                Position::ZERO,
                String::new(),
            ),
        );
        a.do_completion_toggle_docs();
        // Default scroll is 0; up clamps at 0.
        assert_eq!(
            a.insert_completion
                .as_ref()
                .and_then(|s| s.doc_popup.as_ref())
                .map(|d| d.scroll),
            Some(0)
        );
        a.apply(Action::CompletionDocsScrollUp);
        assert_eq!(
            a.insert_completion
                .as_ref()
                .and_then(|s| s.doc_popup.as_ref())
                .map(|d| d.scroll),
            Some(0)
        );
        a.apply(Action::CompletionDocsScrollDown);
        assert_eq!(
            a.insert_completion
                .as_ref()
                .and_then(|s| s.doc_popup.as_ref())
                .map(|d| d.scroll),
            Some(8)
        );
        a.apply(Action::CompletionDocsScrollDown);
        assert_eq!(
            a.insert_completion
                .as_ref()
                .and_then(|s| s.doc_popup.as_ref())
                .map(|d| d.scroll),
            Some(16)
        );
        a.apply(Action::CompletionDocsScrollUp);
        assert_eq!(
            a.insert_completion
                .as_ref()
                .and_then(|s| s.doc_popup.as_ref())
                .map(|d| d.scroll),
            Some(8)
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
        // M.3.2.c.5: production reads route through buffer_locals;
        // seed the synthetic link there directly. The locals key
        // is the popup buffer's construction id (centred-popup
        // resolution rule).
        let buf_id = a.popup_buffer.unwrap();
        if let Some(h) = a.popup_help_mut() {
            h.cursor = lattice_protocol::Position::ZERO;
        }
        let mut existing_links = a
            .buffer_locals
            .get(&buf_id)
            .and_then(|l| l.get::<crate::modes::HelpLinks>())
            .map(|l| l.0.clone())
            .unwrap_or_default();
        existing_links.push(link);
        a.buffer_locals
            .entry(buf_id)
            .or_default()
            .insert(crate::modes::HelpLinks(existing_links));
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
        // M.3.2.c.5: production reads route through buffer_locals;
        // seed the synthetic link there directly. The locals key
        // is the popup buffer's construction id (centred-popup
        // resolution rule).
        let buf_id = a.popup_buffer.unwrap();
        if let Some(h) = a.popup_help_mut() {
            h.cursor = lattice_protocol::Position::ZERO;
        }
        let mut existing_links = a
            .buffer_locals
            .get(&buf_id)
            .and_then(|l| l.get::<crate::modes::HelpLinks>())
            .map(|l| l.0.clone())
            .unwrap_or_default();
        existing_links.push(link);
        a.buffer_locals
            .entry(buf_id)
            .or_default()
            .insert(crate::modes::HelpLinks(existing_links));
        a.active_buffer = BufferKind::Help;
        a.apply(Action::FollowLink);
        // Out-of-range line should clamp to the last valid line,
        // not panic and not echo a confusing error.
        let last_line = a.document.snapshot().buffer.line_count().saturating_sub(1);
        assert_eq!(a.cursor.line, last_line);
        let _ = std::fs::remove_file(path);
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
    fn app_boot_registers_every_built_in_major_mode() {
        let a = app_with("hi", 5);
        // Foundation
        assert!(
            a.mode_registry
                .is_registered(lattice_mode::TextMode::mode_id())
        );
        // Languages (lattice-syntax)
        assert!(
            a.mode_registry
                .is_registered(lattice_syntax::RustMode::mode_id())
        );
        assert!(
            a.mode_registry
                .is_registered(lattice_syntax::PythonMode::mode_id())
        );
        assert!(
            a.mode_registry
                .is_registered(lattice_syntax::JavascriptMode::mode_id())
        );
        assert!(
            a.mode_registry
                .is_registered(lattice_syntax::MarkdownMode::mode_id())
        );
        // Buffer-kind majors (lattice-ui-tui)
        assert!(
            a.mode_registry
                .is_registered(crate::modes::HelpMode::mode_id())
        );
        assert!(
            a.mode_registry
                .is_registered(crate::modes::FileTreeMode::mode_id())
        );
        assert!(
            a.mode_registry
                .is_registered(crate::modes::OilMode::mode_id())
        );
        // LSP log majors (lattice-lsp)
        assert!(
            a.mode_registry
                .is_registered(lattice_lsp::modes::LspLogMode::mode_id())
        );
        assert!(
            a.mode_registry
                .is_registered(lattice_lsp::modes::LspTraceLogMode::mode_id())
        );
        assert!(
            a.mode_registry
                .is_registered(lattice_lsp::modes::LspServerLogMode::mode_id())
        );
    }

    #[test]
    fn open_file_tree_seeds_file_tree_locals() {
        // Construct a temp dir + file, open as a tree, confirm
        // the locals are populated.
        let tmp = std::env::temp_dir().join(format!(
            "lattice-m3-2-c-2-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&tmp);
        let f = tmp.join("file.txt");
        let _ = std::fs::write(&f, "hi");

        let mut a = app_with("hi", 5);
        // Drive via the production path. `do_open_file_tree`
        // constructs the FileTreeBuffer, calls
        // `seed_file_tree_locals`, inserts into the registry,
        // and activates the pane on it.
        a.do_open_file_tree(Some(tmp.clone()));
        let tree_id = a.active_pane_buffer_id();

        let locals = a
            .buffer_locals
            .get(&tree_id)
            .expect("file-tree locals seeded");
        let root = locals
            .get::<crate::modes::FileTreeRoot>()
            .expect("FileTreeRoot local present");
        assert_eq!(root.0, tmp);
        let entries = locals
            .get::<crate::modes::FileTreeEntries>()
            .expect("FileTreeEntries local present");
        // At minimum the root row + the file row.
        assert!(entries.0.len() >= 2);
        // FileTreeNerdFonts seeded; concrete value matches
        // App's `theme.nerd_fonts` (we don't assert a specific
        // boolean since the theme default may evolve -- the
        // important contract is "the local exists post-seed").
        assert!(locals.get::<crate::modes::FileTreeNerdFonts>().is_some());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn set_ui_nerd_fonts_rerenders_open_file_tree() {
        // Regression for the bug where toggling `ui.nerd_fonts`
        // updated the theme but left existing file-tree ropes
        // rendering the old palette. The rope embeds the icon
        // glyphs, so a palette flip must re-render every open
        // tree -- otherwise the user keeps seeing `?` boxes (or
        // BMP fallbacks) until they reopen the tree.
        let tmp = std::env::temp_dir().join(format!(
            "lattice-tree-nerd-rerender-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&tmp);
        std::fs::write(tmp.join("main.rs"), "").unwrap();

        let mut a = app_with("hi", 5);
        a.do_open_file_tree(Some(tmp.clone()));
        let tree_id = a.active_pane_buffer_id();

        // Default is BMP fallback -- the rope should contain
        // the source-code middle-dot, not the nerd-font rust
        // glyph.
        let body = a.buffers.file_tree(tree_id).unwrap().content.as_string();
        assert!(body.contains("· main.rs"), "expected BMP fallback in rope, got: {body}");
        assert!(!body.contains("󱘗 "), "nerd-font glyph leaked into default rope: {body}");

        // Flip the typed option via the same path `:set` takes.
        // The change handler must re-render every open tree
        // against the new palette.
        submit_ex(&mut a, "set ui.nerd_fonts=on");

        let body = a.buffers.file_tree(tree_id).unwrap().content.as_string();
        assert!(body.contains("󱘗 main.rs"), "expected nerd-font glyph post-toggle, got: {body}");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn follow_link_reads_link_from_buffer_locals() {
        // M.3.2.c.1: prove `do_help_follow_link` reads the
        // link data from `buffer_locals` (canonical), not the
        // HelpBuffer's struct field. We open a help buffer,
        // overwrite its locals with a synthetic link pointing
        // somewhere different than the buffer's actual links,
        // and verify FollowLink dispatches based on the
        // locals-side link.
        let mut a = app_with("xx", 10);
        let help = crate::help::HelpContent::from_lines(
            "test-locals-link",
            vec!["plain text -- no markdown link".into()],
        );
        let help_id = a.open_help_in_pane(help);

        // Replace the locals-side links with a synthetic
        // Topic link that the production reader should pick
        // up -- the HelpBuffer's own `links` is empty (no
        // markdown link in the source), so without the
        // locals-first read, FollowLink would say "no link
        // under cursor".
        let synthetic = crate::modes::HelpLinks(vec![crate::help::HelpLink {
            range: lattice_protocol::Range::new(
                lattice_protocol::Position::ZERO,
                lattice_protocol::Position::new(0, 5),
            ),
            target: crate::help::HelpLinkTarget::Topic("synthetic-topic".into()),
        }]);
        a.buffer_locals
            .get_mut(&help_id)
            .expect("locals seeded")
            .insert(synthetic);

        a.cursor = lattice_protocol::Position::new(0, 0);
        // `open_help_in_pane` already activates the pane on
        // the registered help buffer; FollowLink reads
        // `pane_tree.active().buffer_id` to look up locals.
        a.apply(Action::FollowLink);

        // The link target was `help:synthetic-topic`; the
        // FollowLink path routes Topic targets to
        // `:help <topic>`. The topic doesn't exist so we
        // expect an info echo about the topic; the key
        // assertion is "the link was found and dispatched"
        // which we observe via the message kind. If the
        // production path had read from the (empty) struct
        // field, the message would have been "no link under
        // cursor".
        let msg = a.last_message.as_ref().expect("echo set by FollowLink");
        assert!(
            !msg.text.contains("no link under cursor"),
            "production reader should have found the link via buffer_locals, \
             got message: {}",
            msg.text
        );
    }

}
