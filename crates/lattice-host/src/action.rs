//! Action enum -- the dispatch language of the editor.
//!
//! Phase 5.2: extracted from `lattice-ui-tui::app` to its own
//! module in `lattice-host`. Renderer-agnostic by construction
//! (every variant references either `lattice-grammar` types,
//! `lattice-protocol` types, `lattice-host::chord::KeyChord`,
//! or std types). Both `lattice-ui-tui` and the future
//! `lattice-ui-gpui` produce `Action` values via their own
//! keystroke dispatch and feed them into the same host
//! `App::apply` path.
//!
//! `lattice-ui-tui::app` re-exports `Action`, `FindKind`,
//! `EchoMessage`, `EchoLevel` so existing `crate::app::Action`
//! call sites continue to resolve unchanged across the move.

use lattice_grammar::ModalState;
use lattice_grammar::PaneDirection;
use lattice_grammar::Register;
use lattice_grammar::ScrollPos;
use lattice_grammar::SearchDirection;
use lattice_grammar::ViewportPos;
use lattice_grammar::VisualKind;
use lattice_grammar::command::CommandInvocation;

/// Single-line message rendered in the echo area below the mode
/// line (DESIGN.md §5.9.10). Replaced by the next call to
/// `App::set_message` (no timeout-based fade yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoMessage {
    pub text: String,
    pub level: EchoLevel,
}

/// Renderer-side display level for echo messages. Mirrors
/// `lattice_grammar::EchoLevel` (wire-typed) but kept separate so
/// renderers can adopt their own display semantics around the
/// shared wire levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EchoLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// `f` / `F` / `t` / `T` direction-and-stop discriminant for
/// the inline find family.
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
    /// Insert-mode line editing (readline/vim `<C-a>`, `<C-e>`, `<C-w>`,
    /// `<C-u>`, `<C-k>`, `<C-t>`, `<C-d>`, …) — general across all buffers.
    InsertLineEdit(lattice_grammar::InsertLineEdit),
    /// Move into a different modal state (Insert, Normal, ...).
    EnterMode(ModalState),
    /// Vim's `a`: move cursor one byte right (clamped) and enter Insert.
    EnterAppend,
    /// Vim's `I`: move to first non-blank of line and enter Insert.
    EnterInsertFirstNonBlank,
    /// Vim's `A`: move to end of line and enter Insert.
    EnterAppendEndOfLine,
    /// Vim's `gj`: move down one display line (wrap segment).
    DisplayLineDown,
    /// Vim's `gk`: move up one display line (wrap segment).
    DisplayLineUp,
    /// Vim's `g0`: move to the first byte of the current display segment.
    DisplayLineStart,
    /// Vim's `g$`: move to the last byte of the current display segment.
    DisplayLineEnd,
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
    /// SN.3d Select-mode entry (`gh` / `gH` / `g<C-h>`) — anchor a
    /// zero-width Select selection of the named kind at the cursor,
    /// mirroring [`Self::EnterVisual`]. Typing then overtypes it.
    EnterSelect(VisualKind),
    /// SN.3d Select mode — a bare printable key replaces the whole
    /// selection with this char and drops into Insert (one undo step).
    /// The load-bearing new behaviour: see
    /// `docs/dev/architecture/select-mode.md` §3. Emitted only by
    /// `translate_select`'s printable fallthrough; the host handler
    /// (`do_select_overtype`) lands it as a single `Edit::replace`.
    SelectOvertype(char),
    /// SN.3d Select-mode `<Esc>` — collapse the selection to a cursor
    /// and drop to Normal (the Select analogue of [`Self::ExitVisual`]).
    ExitSelect,
    /// SN.3d `<C-g>` — toggle between `Visual(k)` and `Select(k)`,
    /// preserving the selection geometry (reserved in both modes for
    /// this toggle). One handler flips whichever of the two is active.
    ToggleVisualSelect,
    /// Vim's `o` in Visual -- swap the cursor to the other end of the
    /// selection so motions / text objects act on that end.
    SwapVisualEnds,
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
    /// org-cycle `z<Space>` / `:fold-cycle` -- cycle the fold under the
    /// cursor through FOLDED → CHILDREN → SUBTREE.
    CycleFoldAtCursor,
    /// org-cycle `z<Tab>` / `:fold-cycle-global` -- cycle the whole buffer
    /// through OVERVIEW → CONTENTS → SHOW-ALL.
    CycleFoldsGlobal,
    /// `zp` / `:fold-goto-parent` -- move the cursor to the parent heading
    /// (one level up the fold hierarchy).
    GotoParentFold,
    /// Vim's `zd` -- delete the fold containing the cursor.
    DeleteFoldAtCursor,
    /// Vim's `zj` -- move cursor to the start of the next fold.
    GotoNextFold,
    /// Vim's `zk` -- move cursor to the end of the previous fold.
    GotoPrevFold,
    /// Vim's `zi` -- toggle [`App::foldenable`]. With folds disabled
    /// every line renders flat regardless of any closed flag.
    ToggleFoldEnable,
    // L7 (lsp-architecture.md §16): the 7 nav `Action::Lsp*Request`
    // variants (`K` / `gd` / `gD` / `gy` / `gI` / `gr` / `gx`) removed.
    // The nav surface is mode-owned now — `lsp-mode`'s `action_handlers()`
    // closures emit `Effect::Lsp(LspRequest::…)`, dispatched host-side by
    // `editor.lsp_request` onto the unchanged request substrate. The
    // non-nav LSP actions below (signature-help / completion / symbols /
    // on-type-formatting) are ex-command / insert-autopilot triggered,
    // not chord-bound, and stay.
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
    // SN.3c.1 (2026-06-14): `Action::SnippetExpand` removed.
    // `<C-x><C-s>` is mode-owned now (`snippet-mode`'s `keymap()` +
    // `action_handlers()` emit `Effect::ExpandSnippet`); no host
    // `Action` round-trip. (`feedback_mode_owns_its_surface`.)
    // SN.2b (2026-06-12): `SnippetNextPlaceholder` /
    // `SnippetPrevPlaceholder` removed — `<Tab>` / `<S-Tab>`
    // placeholder navigation is mode-owned
    // (`active-snippet-mode`'s `ActionHandlerRegistry` closures),
    // not a host `Action`.
    // SN.3c.2 (2026-06-14): `Action::SnippetLeave` removed.
    // `<Esc>` while a snippet is active is mode-owned now
    // (`active-snippet-mode`'s per-buffer handler clears the
    // session + returns `Effect::EnterMode(Normal)`); no host
    // `Action` round-trip. (`feedback_mode_owns_its_surface`.)
    // CR.1 (2026-06-24): `Action::DiffGet` / `Action::DiffPut` deleted.
    // The diff `do`/`dp` chords are mode-owned (`DiffMode::action_handlers()`
    // → `Effect::ApplyEdit`), so they flow through the generic
    // `Action::ApplyEdit` below instead of a host-side diff variant — the
    // mode-ownership acid test (`feedback_mode_owns_its_surface`).
    /// CR.0: host counterpart of [`lattice_grammar::Effect::ApplyEdit`].
    /// Applies a mode-computed `edit` to `target` (routing through the
    /// active-document pipeline when `target` is the focused buffer, or
    /// the peer-buffer registry handle otherwise) and, when `cursor` is
    /// `Some`, parks the active cursor at that row. `handle_effect`
    /// translates the `Effect` into this `Action` and queues it on
    /// `out.next_actions`; the applier arm in `handle_action` calls
    /// `Editor::apply_targeted_edit`. The generic primitive the diff
    /// (and future) modes drive instead of host `do_<x>` methods.
    ApplyEdit {
        target: lattice_core::BufferId,
        edit: lattice_protocol::edit::Edit,
        cursor: Option<lattice_protocol::position::Position>,
    },
    // M.10.7 (2026-06-03): four dead Action variants deleted —
    // `MultibufferExpand`, `SearchTrigger`, `SearchJumpToSource`,
    // `SearchRefresh`. All four are now mode-owned via the
    // M.10.1.b ActionHandlerRegistry (the chord/ex-command
    // routes through `run_invocation` → registry consultation →
    // handler) OR work happens inline in the apply_effect arm
    // (`:multibuffer-expand` + `:search`). No production path
    // constructs these variants any longer.
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
    /// HS.2: manual horizontal scroll (vim `z{l,h,L,H,s,e}`).
    HorizontalScroll(lattice_grammar::HScroll),
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
    /// Open the command picker (`:`/`M-x`). On accept: if the
    /// chosen command needs a required arg, arm the cmdline;
    /// otherwise execute immediately.
    OpenCommandPicker,
    /// MB.3: `q:` -- open the command-line *history* picker over
    /// `command_history`; accept loads the picked command into the
    /// `:` line without executing.
    OpenHistoryPicker,
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
    /// MB.2: toggle the `:` line's expanded tier-2 mini-buffer band
    /// (`<C-x><C-e>`); collapse returns the edited text to the one-row
    /// line for review.
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
    /// Issue #32 (2026-05-22): `<C-s>` — accept candidate,
    /// opening files in a horizontal split. Non-file outcomes
    /// ignore the override.
    PickerAcceptInSplit,
    /// `<C-v>` — accept candidate in a vertical split.
    PickerAcceptInVSplit,
    /// `<C-t>` — accept candidate in a new tab.
    PickerAcceptInTab,
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
    /// `<C-w>o` / `:only` / emacs `C-x 1` -- close every pane except
    /// the active one. S3b (2026-06-22).
    OnlyPane,
    /// `<C-w>{h,j,k,l}` -- move the active pane cardinally.
    NavigatePane(PaneDirection),
    /// `<C-w>w` -- cycle to the next pane in declaration order.
    NextPane,
    /// `<C-w>W` -- cycle to the previous pane.
    PrevPane,
    /// Issue #29 (2026-05-22): vim's `gt` — next tab.
    NextTab,
    /// Vim's `gT` — previous tab.
    PrevTab,
    /// Vim's `{N}gt` — switch to tab N (1-indexed; clamped).
    GoToTab(u32),
    /// `:tabnew` — new empty tab (scratch buffer).
    NewTab,
    /// `:tabnew <path>` — new tab opening `path`.
    NewTabAt(String),
    /// Issue #40 / Terminal-mode T1 (2026-05-22):
    /// `:terminal [cmd]` — spawn a shell (or `cmd` if given)
    /// under a fresh PTY and activate as a new terminal
    /// buffer. None ⇒ spawn the user's shell from
    /// `terminal.shell` (default `$SHELL` else `/bin/sh`).
    TerminalSpawn(Option<String>),
    /// Terminal-mode T2.a (2026-05-25): write encoded bytes to
    /// the active Terminal buffer's PTY stdin. Emitted by the
    /// translate layer when the user is in Terminal-Insert mode
    /// on a Terminal buffer and the chord encodes to ANSI via
    /// `keymap_terminal::key_to_ansi`. The host handler
    /// (`Editor::do_terminal_input`) looks up the active
    /// buffer's `PtyHandle` and forwards. No-op on non-Terminal
    /// active buffers (defensive — translate never emits in
    /// that state but the handler stays safe).
    TerminalInput(Vec<u8>),
    /// Terminal-mode T2.a: activate `terminal-insert-mode` on
    /// the active Terminal buffer. Emitted by `i` (later `a`/
    /// `I`/`A`) in Normal-in-terminal. No-op when the active
    /// buffer is not a Terminal.
    EnterTerminalInsert,
    /// Terminal-mode T2.a: deactivate `terminal-insert-mode`
    /// on the active Terminal buffer. Emitted by `<C-\><C-n>`
    /// (and, T2.b, optionally `<Esc>` when `terminal.esc_exits`
    /// is true).
    ExitTerminalInsert,
    /// Terminal-mode T3 (2026-05-25): re-position the active
    /// terminal's scrollback viewport. Emitted by Normal-in-
    /// terminal motions (`j`/`k`/`<C-d>`/`<C-u>`/`gg`/`G`).
    /// No-op when the active buffer is not a Terminal.
    TerminalScroll(lattice_terminal::TerminalScrollKind),
    /// Terminal-mode T2.c (2026-05-25): user pressed `<C-\>` in
    /// Terminal-Insert; arm the two-key exit chord on the
    /// active terminal buffer. The next keystroke resolves it
    /// (`<C-n>` exits; anything else sends `\x1c` + that key
    /// to the PTY). Cleared automatically by the
    /// `ExitTerminalInsert` / `TerminalInput` arms.
    TerminalArmExitChord,
    /// T4 (2026-05-25): `<C-w>T` — move the active pane (and
    /// its buffer) to a fresh tab. Vim convention. The
    /// previous tab loses the pane via the standard close-pane
    /// path; the new tab opens with a single pane referencing
    /// the same `BufferId`.
    MovePaneToNewTab,
    /// `:tabclose` — close active tab (no-op when only one tab).
    CloseTab,
    /// `:tabonly` — close every tab except the active one.
    OnlyTab,
    /// `:tabmove [N]` — move the active tab to position N
    /// (1-indexed). Negative or omitted N is handled by the
    /// ex-command parser; the runtime value here is the
    /// resolved target index.
    MoveTab(u32),
    /// `<C-w>=` -- reset every split's ratio to 0.5 (equalize all
    /// panes). Issue #28 (2026-05-22).
    EqualizePanes,
    /// `<C-w>+` -- grow the active pane vertically (nudge the
    /// nearest HorizontalSplit ancestor's ratio).
    GrowPaneHeight,
    /// `<C-w>-` -- shrink the active pane vertically.
    ShrinkPaneHeight,
    /// `<C-w>>` -- grow the active pane horizontally (nudge the
    /// nearest VerticalSplit ancestor's ratio).
    GrowPaneWidth,
    /// `<C-w><` -- shrink the active pane horizontally.
    ShrinkPaneWidth,

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

    // ---- 5.5.G.23.insert: host→App LSP autopilot follow-ups ----
    /// 5.5.G.23.insert: emitted by host-side `Editor::do_insert_text`
    /// after a typed character matches the active document's
    /// `onTypeFormatting` trigger-char set. App-side handler fires
    /// `textDocument/onTypeFormatting` against the highest-priority
    /// server advertising the trigger and applies the returned edits
    /// as one undo unit.
    LspOnTypeFormattingRequest(char),
    /// 5.5.G.23.insert: emitted by host-side
    /// `Editor::maybe_refresh_insert_completion_after_edit` when the
    /// last LSP completion response was `isIncomplete` and the popup
    /// just refiltered against the new query. App-side handler
    /// dispatches `textDocument/completion` through the
    /// `LspCompletionSource`'s async fan-out.
    LspInsertCompletionRequest,

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
    /// MB.5c: toggle the `/`·`?` search line's expanded tier-2
    /// mini-buffer band (`<C-x><C-e>`).
    SearchLineToggleExpand,
    /// Repeat the last search in its original direction.
    SearchNext,
    /// Repeat the last search in the opposite direction.
    SearchPrevious,
    // ---- Phase 5.8.AF.5 / Slice 3c.final.C ----
    // Renderer-thread non-dispatch mutations lifted to Action
    // variants so the renderer doesn't need `&mut Editor` to
    // perform them. Each fires from the per-frame setup code in
    // the TUI's `main_loop` / GPUI's `EditorView::render` and
    // dispatches through the standard `apply` tail (which
    // publishes RenderState).
    /// Set the active pane's viewport-height (rows). Triggered by
    /// the renderer when the window size changes. Mirrors the
    /// pre-3c.final `App::set_viewport_height` shape — clamps to
    /// `>= 1` and runs `ensure_cursor_visible` host-side.
    SetViewportHeight(u32),
    /// Auto-scroll so the cursor stays visible in the active
    /// pane. Idempotent: no-op when the cursor is already on
    /// screen.
    EnsureCursorVisible,
    /// Dismiss the active popup (closes `popup_buffer`, restores
    /// the previous pane focus). No-op when no popup is open.
    DismissPopup,
    /// Mirror the TUI's terminal width into editor state so
    /// status-line layout matches what crossterm reported.
    SetTerminalWidth(u16),
    /// Clear `pending_redraw` after the renderer has cleared the
    /// terminal buffer in response to `<C-l>` (`RedrawScreen`).
    AcknowledgeRedraw,
    /// SN.3c.2b: run a sequence of actions in order. Produced by
    /// `dispatch_insert` for a `fall_through` binding —
    /// `[mode_action, native_action]` — where the mode's chord
    /// augments a native chord (`active-snippet-mode`'s `<Esc>` clears
    /// the session, then continues to the builtin `<Esc>` → exit
    /// insert). The renderer's `apply` applies each in order. General
    /// (not snippet-specific): any future binding that wants to run +
    /// continue resolves to a `Chain`.
    Chain(Vec<Action>),
}
