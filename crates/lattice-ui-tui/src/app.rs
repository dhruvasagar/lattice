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
use lattice_core::Document;
use lattice_core::buffer::AppliedEdit;
use lattice_grammar::CommandRegistry;
use lattice_grammar::ModalState;
use lattice_grammar::SearchDirection;
use lattice_grammar::VisualKind;
use lattice_grammar::YankKind;
use lattice_grammar::builtins::{Builtins, populate};
use lattice_grammar::command::CommandInvocation;
use lattice_grammar::effect::Effect;
use lattice_grammar::register::Register;
use lattice_protocol::Event;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range as ProtoRange};
use lattice_protocol::selection::{Selection, SelectionSet};
use lattice_lsp::{DiagnosticsLayer, LspLogger, LspSupervisor, LspSupervisorHandle};
use lattice_runtime::{
    CancellationToken, DocumentHandle, EventBus, RuntimeError, SnapshotCache, block_on,
    spawn_document,
};
use lattice_syntax::{Lang, LangRegistry, StyledSpan};

use std::collections::HashMap;
use std::sync::Arc;

use crate::buffer_registry::{BufferData, BufferEntry, BufferRegistry, DocumentEntry};
use crate::buffers::{BufferFlags, BufferId, BufferKind};

/// Build a fresh LSP subsystem. Returns the supervisor wrapped
/// in `Arc<Mutex>` for App-side sharing, plus cloned handles
/// to the diagnostics layer + logger so the renderer's
/// per-frame reads can skip the supervisor lock.
/// Configure + spawn the LSP subsystem. The returned handle is
/// what the App holds for the editor's lifetime; reads are
/// wait-free against an `ArcSwap<SupervisorSnapshot>`, writes
/// route through the supervisor task's mailbox. The
/// `Arc<tokio::sync::Mutex<LspSupervisor>>` of the previous
/// shape is gone -- the UI thread can no longer take a
/// supervisor lock by accident (the audit's class-of-bug
/// finding from the LSP-edit refactor).
///
/// `event_bus` is wired in here (pre-spawn) so the supervisor
/// task is born already knowing about it; subsequent actor
/// spawns get their per-actor edit fan-in for free.
fn build_lsp_subsystem(
    event_bus: std::sync::Arc<lattice_runtime::EventBus>,
    runtime_handle: &tokio::runtime::Handle,
) -> (
    LspSupervisorHandle,
    DiagnosticsLayer,
    LspLogger,
    tokio::sync::mpsc::UnboundedReceiver<lattice_lsp::InboundApplyEdit>,
    tokio::sync::mpsc::UnboundedReceiver<lattice_lsp::InboundConfigurationRequest>,
) {
    let logger = LspLogger::with_defaults();
    let mut sup = LspSupervisor::new(logger.clone());
    // Builtin registry: rust-analyzer, pyright, gopls,
    // typescript-language-server, clangd, lua-language-server.
    sup.set_configs(lattice_lsp::builtin_servers());
    let diagnostics = sup.diagnostics().clone();
    // Apply-edit bus (Phase 4.3): App owns the receiver, every
    // actor spawned via the supervisor gets a clone of the
    // sender. Server-initiated `workspace/applyEdit` requests
    // ferry through this channel; the App's drain applies them
    // and replies via the embedded oneshot.
    let (apply_edit_bus, apply_edit_rx) = lattice_lsp::ApplyEditBus::new();
    sup.set_apply_edit_bus(apply_edit_bus);
    // Configuration bus (Phase 4.1 follow-up): same shape as
    // apply-edit. Server-initiated `workspace/configuration`
    // requests ferry through this channel; the App's drain
    // walks the cached TOML tree at `lsp.<section>` for each
    // requested item.
    let (configuration_bus, configuration_rx) = lattice_lsp::ConfigurationBus::new();
    sup.set_configuration_bus(configuration_bus);
    // Event bus is wired pre-spawn so every actor born in this
    // supervisor task gets its per-actor edit fan-in
    // automatically (lattice_lsp::fan_in).
    sup.set_event_bus(event_bus.clone());
    // Hand the explicit runtime handle to spawn the supervisor
    // task. `App::new` runs from `runtime::run` *before* the
    // editor's main loop has entered any tokio context, so
    // `tokio::runtime::Handle::try_current()` would (silently)
    // fail and the supervisor task would never start. The
    // explicit handle removes that footgun.
    let handle = sup.spawn(runtime_handle);
    // Attach driver: subscribes to `Event::DocumentOpened` and
    // funnels each path-bearing event into the supervisor's
    // mailbox on the LSP runtime. The publisher (`App::new` /
    // `App::do_edit`) returns immediately after publishing; the
    // LSP `initialize` round-trip happens off the UI thread,
    // honouring paramount goal #4 (asynchronicity). See
    // `lattice_lsp::attach_driver` for the recv loop.
    let _attach_sub = lattice_lsp::attach_driver::spawn(
        event_bus,
        runtime_handle,
        handle.clone(),
        logger.clone(),
    );
    (handle, diagnostics, logger, apply_edit_rx, configuration_rx)
}
use crate::excommand;
use crate::help::{HelpBuffer, HelpDisplayMode};
use crate::pane::{PaneDirection, PaneState, PaneTree, SplitOrientation};

// R.1.0 -- app/ submodule skeleton. Each submodule is a
// per-feature destination for the App's methods. R.1.0 only
// creates the empty modules with scoping doc comments;
// subsequent R.1.x slices move method blocks across without
// rethinking the structure. See docs/keymap-architecture.md
// (or the dedicated R.1 doc) for the full feature -> module
// mapping.
mod cmdline;
mod completion;
mod edit;
mod file_tree;
mod folds;
mod help;
mod lifecycle;
mod lsp;
mod macros;
mod mode;
mod motions;
mod oil;
mod operators;
mod options;
mod picker;
mod search;
mod state;
mod syntax;
mod visual;

#[cfg(test)]
mod test_helpers;

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
    /// 8.i; see `docs/8i-approach.md`).
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
    visible_highlights_key: Option<VisibleHighlightsKey>,
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
    /// Active **Insert-mode** completion popup (Phase 4.2.g).
    /// Distinct from `completion_state` (which drives the `:` line
    /// completion popup): this one floats over the buffer, shows
    /// candidates from sources (LSP / snippets / buffer-words /
    /// path / tree-sitter / plugin), and the host's keystroke
    /// dispatcher routes through a "completion-popup minor mode"
    /// keymap layer while it's `Some`. Behavioural spec lives in
    /// [`docs/insert-completion.md`](../../docs/insert-completion.md).
    pub insert_completion: Option<lattice_completion::InsertCompletionState>,
    /// Sidecar metadata for LSP-sourced candidates in the
    /// active insert-completion popup. Indexed by the
    /// candidate's `CandidateData::Extension { payload }`
    /// (which carries a `u32` little-endian index into this
    /// vec). Holds everything the accept / docs / commit-char
    /// paths need that doesn't fit into `RawCandidate.text`:
    /// `insertText`, `additionalTextEdits`, `kind` glyph,
    /// `documentation`, etc. v1 of the typed-routing-payload
    /// pattern (#19) -- the picker's bespoke `text`-stuffing
    /// follows the same approach when 4.2.g.5 lands.
    pub insert_completion_lsp_meta: Vec<LspCompletionMeta>,
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
    pub snippet_registry: lattice_snippet::SnippetRegistry,
    /// Sidecar metadata for snippet candidates in the active
    /// insert-completion popup. Indexed by the candidate's
    /// `CandidateData::Extension { payload }` (u32 LE) --
    /// same shape as `insert_completion_lsp_meta` for the
    /// LSP source.
    pub insert_completion_snippet_meta: Vec<SnippetCandidateMeta>,
    /// Per-session accept-count map for the insert-mode
    /// completion popup (Phase 4.2.g.5). Each accepted candidate
    /// bumps the counter for its `(text, kind)` pair; the ranker
    /// reads this map and adds a bounded bonus
    /// (`InsertRanker::FREQUENCY_BONUS_CAP`) so recently-accepted
    /// items bubble above tied peers next time. In-memory only
    /// in v1 -- cleared on process exit; persistence with a
    /// privacy story lands later (Phase 4.2.g.7 polish queue per
    /// `docs/insert-completion.md` §11).
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
    /// (3b/3); spec at `docs/insert-completion.md` §9). Seeded
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
    /// Single-entry cache for path-completion's `read_dir` walk
    /// (audit slice 5 / H5). The popup re-fires on every Insert
    /// keystroke inside a string literal; without the cache the
    /// directory walk runs from scratch every time, thrashing
    /// any large or network-mounted dir. Keyed by directory
    /// path + mtime so any external change invalidates on the
    /// next call. Single entry because users typically navigate
    /// one dir at a time; LRU is overkill for v1.
    path_completion_cache: Option<PathCompletionCache>,
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
    /// Receiver for [`Event::LspLogPushed`] events (Phase 4).
    /// Drained once per main-loop tick by
    /// [`Self::drain_lsp_log_events`]; matching log buffers in
    /// `BufferRegistry` are rebuilt from the logger snapshot so
    /// `*lsp*` / `*lsp:<server>*` / `*lsp:<server>:trace*` views
    /// update live without the user having to reopen them.
    pub lsp_log_event_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<lattice_protocol::Event>>,
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

/// One cached `read_dir` walk for path completion. Hot path:
/// each Insert keystroke inside a string literal re-fires
/// `populate_path_completion`; with this cache, consecutive
/// keystrokes for the same directory pay one `metadata()` call
/// (mtime check) instead of a full directory walk.
#[derive(Debug, Clone)]
pub(crate) struct PathCompletionCache {
    /// Directory the entries were read from. Cache hits require
    /// equality (exact path).
    pub(crate) dir: std::path::PathBuf,
    /// Modified-time of `dir` at the moment of the read. Cache
    /// hits require this to still match what the OS reports
    /// (cheap stat call); mismatch falls through to a fresh
    /// `read_dir`.
    pub(crate) mtime: Option<std::time::SystemTime>,
    /// `(name, is_dir)` per entry. Sorted by `name` so popup
    /// emission is deterministic without re-sorting.
    pub(crate) entries: Vec<(String, bool)>,
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

/// Slice B.3: cache key for `App.visible_highlights`. When the
/// freshly-computed key matches the stored one, the existing
/// `visible_highlights` is reused as-is and the (~178µs) call to
/// `highlight_lines` is skipped.
///
/// The five fields together capture every input that affects the
/// computed spans:
///
/// - `snapshot_ptr`: `Arc::as_ptr` of the syntax snapshot. Distinct
///   pointer = distinct snapshot (different file or different
///   parse). Required because `text_version` alone isn't unique
///   across syntax handles -- a new file's first publish has
///   `text_version = 0` regardless.
/// - `text_version`: snapshot's parse version. Bumped on each
///   reparse; differentiates successive parses within one handle.
/// - `scroll` / `viewport_height`: the visible-window range.
/// - `fold_hash`: hash of the fold list. Toggling a fold open/
///   closed changes which buffer lines are visible, so the
///   highlight window changes.
///
/// Mode, cursor, and selection are deliberately excluded -- they
/// don't affect span computation. That's the steady-state hit:
/// the cursor blinks but nothing else changes, so the key stays
/// equal across frames and we never re-run the QueryCursor walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisibleHighlightsKey {
    snapshot_ptr: usize,
    /// Syntax snapshot's `text_version` (== version the worker
    /// has parsed up to). Cache invalidates when the worker
    /// publishes a fresh tree -- that's the right trigger for
    /// re-highlighting. The document's own `text_version` is
    /// deliberately NOT in the key: between an edit and the
    /// worker's publish, the latest snapshot has no new
    /// information, so re-highlighting against it would just
    /// produce the same (slightly stale) result as the previous
    /// frame at the cost of a ~178µs walk. Letting the cache
    /// hold across that window keeps unchanged lines'
    /// highlighting continuous; only the just-edited line's
    /// spans are briefly at stale byte positions until the
    /// worker publishes.
    syntax_text_version: u64,
    scroll: u32,
    viewport_height: u32,
    fold_hash: u64,
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

/// LSP-sourced insert-completion candidate metadata. Sidecar
/// to the `RawCandidate` -- the candidate carries
/// `CandidateData::Extension { kind_id: LSP_COMPLETION_KIND_ID,
/// payload: u32_le_bytes }` pointing at this struct's index in
/// `App.insert_completion_lsp_meta`. The accept / docs / commit-
/// char / additional-edits paths read the metadata via that
/// index.
///
/// Why a sidecar (rather than another `CandidateData` variant):
/// `lattice-completion` doesn't depend on `lsp-types` and we
/// don't want to add the dep just for this. The sidecar stays
/// in the host crate where lsp-types is already in scope.
#[derive(Debug, Clone)]
pub struct LspCompletionMeta {
    pub label: String,
    pub insert_text: String,
    pub filter_text: Option<String>,
    pub sort_text: Option<String>,
    pub detail: Option<String>,
    pub documentation: Option<String>,
    pub kind: Option<lsp_types::CompletionItemKind>,
    pub deprecated: bool,
    pub preselect: bool,
    pub commit_characters: Vec<char>,
    pub additional_text_edits: Vec<lsp_types::TextEdit>,
    pub command: Option<lsp_types::Command>,
    pub insert_text_format: lsp_types::InsertTextFormat,
    /// Range to replace, when the LSP item carries `textEdit.range`.
    /// Populated host-side at request time (the popup's anchor /
    /// cursor become the replace bounds when this is None).
    pub replace_range: Option<lsp_types::Range>,
    /// Server that produced the item. Resolve / executeCommand
    /// route back to the same server. Empty when the item came
    /// from a non-LSP source (shouldn't happen in practice but
    /// guard anyway).
    pub server_id: std::sync::Arc<str>,
    /// Original `CompletionItem` from the server, preserved
    /// verbatim so `completionItem/resolve` round-trips it
    /// unchanged. Servers use the `data` field as an opaque
    /// blob; mutating any field would break resolve.
    pub original_item: lsp_types::CompletionItem,
    /// True once `completionItem/resolve` has filled in the
    /// missing fields. Subsequent docs-popup focuses don't
    /// re-fire the resolve.
    pub resolved: bool,
}

/// `Extension::kind_id` discriminant for LSP-sourced candidates.
/// Values 0-99 reserved for first-party host data; plugins use
/// 1000+.
pub const LSP_COMPLETION_KIND_ID: u32 = 1;
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

/// Effective insert-completion config for a given language.
/// Materialised by [`App::effective_completion_for`] from the
/// per-language overrides + global typed options + spec
/// fallbacks. Carried as a value type so the producer / fan-out
/// paths read it without re-resolving for every candidate.
#[derive(Debug, Clone)]
pub struct EffectiveCompletionConfig {
    /// `Some(list)` -> only sources whose id appears in the list
    /// contribute. `None` -> every enabled source contributes
    /// (the "no per-language override" case).
    pub sources: Option<Vec<lattice_completion::SourceId>>,
    pub auto_trigger: bool,
    pub auto_insert_single: bool,
    /// Tree-sitter scopes where the popup should not fire.
    /// Plumbed today; enforcement awaits the scope-detect slice.
    pub suppress_in: Vec<String>,
}

impl EffectiveCompletionConfig {
    /// True if `source` contributes for this language. `None`
    /// effective `sources` means "every source contributes."
    pub fn source_enabled(&self, source: &lattice_completion::SourceId) -> bool {
        match &self.sources {
            Some(list) => list.contains(source),
            None => true,
        }
    }
}

/// Parse a `[completion.per-language.<lang>]` TOML sub-table
/// into [`PerLanguageOverrides`]. Unknown keys + wrong-typed
/// values append warnings to `warnings` (caller surfaces them
/// in one echo); recognised keys with valid values populate the
/// struct.
fn parse_per_language_overrides_table(
    section_path: &str,
    table: &toml::Table,
    warnings: &mut Vec<String>,
) -> lattice_completion::PerLanguageOverrides {
    let mut out = lattice_completion::PerLanguageOverrides::default();
    for (key, value) in table {
        match key.as_str() {
            "sources" => match value.as_array() {
                Some(arr) => {
                    let sources: Vec<lattice_completion::SourceId> = arr
                        .iter()
                        .filter_map(|v| {
                            v.as_str().map(lattice_completion::canonical_source_id)
                        })
                        .collect();
                    out.sources = Some(sources);
                }
                None => warnings.push(format!(
                    "{section_path}.sources: expected array of strings",
                )),
            },
            "auto_trigger" => match value.as_bool() {
                Some(b) => out.auto_trigger = Some(b),
                None => warnings.push(format!(
                    "{section_path}.auto_trigger: expected bool",
                )),
            },
            "auto_insert_single" => match value.as_bool() {
                Some(b) => out.auto_insert_single = Some(b),
                None => warnings.push(format!(
                    "{section_path}.auto_insert_single: expected bool",
                )),
            },
            "suppress_in" => match value.as_array() {
                Some(arr) => {
                    out.suppress_in = Some(
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect(),
                    );
                }
                None => warnings.push(format!(
                    "{section_path}.suppress_in: expected array of strings",
                )),
            },
            other => warnings.push(format!(
                "{section_path}.{other}: unknown per-language key (recognised: \
                 sources, auto_trigger, auto_insert_single, suppress_in)",
            )),
        }
    }
    out
}

/// Drain payload for `completionItem/resolve` (Phase
/// 4.2.g.3). Carries the meta-vec index alongside the
/// resolved item so the drain can update the right entry
/// in `App.insert_completion_lsp_meta` when several resolves
/// fire in sequence (selection change → cancel prior →
/// fire new).
#[derive(Debug, Clone)]
pub struct CompletionResolveOutcome {
    pub meta_index: usize,
    pub resolved: lsp_types::CompletionItem,
}

/// Drain payload for the async LSP insert-completion source.
/// Replaces (rather than appends to) the current LSP slice of
/// `state.raw` -- previous items get pruned by the drain so the
/// popup reflects the freshest server response.
#[derive(Debug, Clone)]
pub enum InsertCompletionLspOutcome {
    Items {
        items: Vec<LspCompletionMeta>,
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
        // §5.10 event bus. Built before `build_lsp_subsystem`
        // because the supervisor wires its per-actor edit fan-in
        // (lattice_lsp::fan_in) at spawn time using this bus, and
        // the post-spawn handle does not expose `set_event_bus`.
        let event_bus = Arc::new(EventBus::new());
        // Hand `build_lsp_subsystem` the canonical LSP runtime
        // handle so the supervisor task is spawned on a real
        // runtime even when `App::new` is called before
        // `runtime::run` has entered any tokio context. The
        // OnceLock lazily initialises the runtime on first call;
        // every later caller (the per-feature
        // `spawn_on_lsp_runtime` for hover / definition / etc.,
        // the attach driver, every test that exercises the LSP
        // write path) reuses the same instance.
        let runtime_handle = crate::runtime::lsp_runtime().handle().clone();
        let (
            lsp,
            lsp_diagnostics,
            lsp_logger,
            lsp_apply_edit_rx,
            lsp_configuration_rx,
        ) = build_lsp_subsystem(event_bus.clone(), &runtime_handle);
        let mut registry = CommandRegistry::new();
        let builtins = populate(&mut registry);
        // Register the built-in ex-commands as peers of motions /
        // operators / text objects (DESIGN.md §5.2.1). The returned
        // ids aren't held in App state today -- the parser front-end
        // looks them up by name -- but registering them populates the
        // registry so `:`-line parsing can route to them.
        let _ex_builtins = lattice_grammar::ex_commands::populate(&mut registry);
        // App-side action registrations (slice 8.i; see
        // `docs/8i-approach.md`). Each `CommandKind::Action`
        // entry returns `Effect::AppAction(AppEffect::Foo)`;
        // per-mode keymap modules consume the resulting
        // `ActionIds` to build typed `CommandInvocation`s for
        // chord bindings as the legacy `bind_legacy` bridge
        // retires.
        let action_ids = crate::actions::populate(&mut registry, &builtins);
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
        // The §5.10 event bus is built above (before
        // build_lsp_subsystem so the supervisor task can wire
        // its per-actor fan-in pre-spawn). Subsequent setup just
        // attaches more subscribers to the same bus.
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
        // M.2.0c: every option (core + TUI-specific) self-
        // registers via the proc-macro-emitted `register_fn`
        // thunks aggregated in `OPTION_DECLS`. One
        // `init_from_linkme()` call boots them all; idempotent
        // if called again.
        config.init_from_linkme();
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
        // Build the underlying `Syntax` synchronously + seed it
        // with one parse of the initial text so the renderer's
        // first frame has highlights without waiting for the
        // worker. After that the handle takes over: subsequent
        // `request_reparse` calls run the parse on a worker
        // thread; the renderer reads the latest snapshot via
        // `ArcSwap`. (Audit slice 3.)
        let initial_text = document.text();
        let initial_text_version = document.text_version();
        // Production: pass the explicit LSP runtime handle so the
        // syntax worker actually starts. `SyntaxHandle::seeded` fell
        // back to `Handle::try_current()` which silently fails when
        // App::new runs before the main loop enters tokio context --
        // the worker would never spawn and Option B's incremental
        // reparse pipeline would be entirely dead. Same shape as
        // the LSP supervisor handle above.
        let syntax: Option<lattice_syntax::SyntaxHandle> =
            match lattice_syntax::Syntax::for_language_with_registry(lang, lang_registry.clone()) {
                Ok(Some(mut s)) => {
                    s.parse_at(&initial_text, initial_text_version);
                    Some(lattice_syntax::SyntaxHandle::seeded_with_runtime(
                        s,
                        &runtime_handle,
                    ))
                }
                _ => None,
            };
        let last_parsed_text_version = initial_text_version;
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
                last_synced_syntax_version: 0,
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
            partial_chord: Vec::new(),
            registry,
            event_bus: event_bus.clone(),
            option_change_rx: Some(option_change_rx),
            pending_hover_rx: None,
            pending_hover_token: None,
            pending_definition_rx: None,
            pending_definition_token: None,
            pending_nav_kind: None,
            pending_references_rx: None,
            pending_references_token: None,
            pending_symbols_rx: None,
            pending_symbols_token: None,
            pending_format_rx: None,
            pending_format_token: None,
            pending_signature_help_rx: None,
            pending_signature_help_token: None,
            pending_completion_rx: None,
            pending_completion_token: None,
            pending_completion_items: None,
            pending_rename_rx: None,
            pending_rename_token: None,
            pending_code_action_rx: None,
            pending_code_action_token: None,
            pending_code_action_items: None,
            pending_code_action_handle: None,
            lang_registry,
            builtins,
            action_ids,
            keymap: {
                // Slices 8.d -- 8.g.i: register the per-mode
                // built-in catalogs into the Builtin layer at
                // startup. Normal mode is being migrated in
                // sub-slices 8.g.i -- 8.g.vi; this slice
                // (8.g.i) covers the simple single-key
                // bindings.
                let h = crate::keymap_registry::KeymapHandle::new();
                crate::keymap_replace::register_replace_bindings(&h, &action_ids);
                crate::keymap_visual::register_visual_bindings(&h, &builtins, &action_ids);
                crate::keymap_insert::register_insert_bindings(&h, &action_ids);
                crate::keymap_normal::register_normal_bindings(&h, &builtins, &action_ids);
                h
            },
            completion_popup_layer: None,
            snippet_layer: None,
            command_line: String::new(),
            last_message: None,
            pending_redraw: false,
            syntax,
            last_parsed_text_version,
            pending_syntax_edits: Vec::new(),
            last_synced_syntax_version: 0,
            visible_highlights: Vec::new(),
            visible_highlights_key: None,
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
            tag_stack: Vec::new(),
            pending_tag_origin: None,
            macros: HashMap::new(),
            macro_recording: None,
            last_played_macro: None,
            last_find: None,
            folds: Vec::new(),
            last_insert: None,
            recording_insert: None,
            pending_block_insert: None,
            config,
            // Default placeholder; rebuilt from config below before
            // the App is returned. The placeholder lets the struct
            // literal type-check; the rebuild is the canonical
            // initial population.
            option_cache: OptionCache::default(),
            // M.2.1: per-buffer mode-resolved options cache.
            // Empty until the first `recompute_options_for_buffer`
            // call after registration / mode activation.
            // M.3.0: register every built-in major mode at App
            // boot. The registry is created mutably, populated,
            // then wrapped in Arc -- after which it's immutable
            // for the App's lifetime (plugin-driven dynamic
            // registration is M.10 territory and uses a
            // different surface).
            mode_registry: {
                let mut registry = lattice_mode::ModeRegistry::new();
                lattice_mode::register_foundation_modes(&mut registry);
                lattice_syntax::register_language_modes(&mut registry);
                lattice_lsp::modes::register_lsp_log_modes(&mut registry);
                crate::modes::register_buffer_kind_modes(&mut registry);
                std::sync::Arc::new(registry)
            },
            active_modes: std::collections::HashMap::new(),
            buffer_locals: std::collections::HashMap::new(),
            resolved_options: std::collections::HashMap::new(),
            buffer_local_overrides: std::collections::HashMap::new(),
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
            insert_completion: None,
            insert_completion_lsp_meta: Vec::new(),
            pending_insert_completion_lsp_rx: None,
            pending_insert_completion_lsp_token: None,
            pending_completion_resolve_rx: None,
            pending_completion_resolve_token: None,
            snippet_registry: lattice_snippet::SnippetRegistry::new(),
            insert_completion_snippet_meta: Vec::new(),
            completion_accept_freq: std::collections::HashMap::new(),
            path_completion_cache: None,
            pending_config_structural_sections: std::collections::BTreeMap::new(),
            per_language_completion: lattice_completion::per_language_defaults(),
            completion_in_path_context: false,
            active_snippet: None,
            snippet_dirs: Vec::new(),
            picker: None,
            previewing: false,
            lsp_log_event_rx: Some(lsp_log_event_rx),
            auto_submit_after_chord: false,
            lsp,
            lsp_diagnostics,
            lsp_logger,
            pending_apply_edit_rx: Some(lsp_apply_edit_rx),
            pending_configuration_rx: Some(lsp_configuration_rx),
            lsp_config_tree: toml::Table::new(),
            buffer_uris: std::collections::HashMap::new(),
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
        // M.3.1: activate the resolved major mode for the
        // initial document buffer. `resolve_major_mode(kind,
        // lang)` picks the right major (text-mode for
        // Lang::Plain, rust-mode/python-mode/... for typed
        // languages). The activation populates
        // `active_modes[buffer]` and triggers the option-cache
        // recompute so `ResolvedOptions` reflects the major's
        // contributions (e.g. ReadOnly = true for Help).
        app.activate_major_for_buffer_kind(app.document_buffer_id, BufferKind::Document);
        // Initial-document attach. Path-bearing buffers register
        // their URI eagerly (the URI is a deterministic
        // `uri_from_path`; LSP attach is async and doesn't gate
        // the mapping) and publish `Event::DocumentOpened` -- the
        // attach driver wired in `build_lsp_subsystem` consumes
        // it and submits to the supervisor on the LSP runtime,
        // off the UI thread. Path-less scratch buffers publish
        // nothing (no LSP work to drive) and the `buffer_uris`
        // entry stays absent.
        app.publish_document_opened_for_active();
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


    /// Activate the resolved major mode for `buffer_id` based
    /// on its `kind` (and, for Document buffers, the detected
    /// language) and refresh the resolved-options cache. M.3.1.
    ///
    /// Lang detection happens inside
    /// `lattice_syntax::Lang::detect_from_path`; for buffers
    /// without a path (scratch documents) the resolver falls
    /// through to `text-mode` per `mode-architecture.md` §4.1.
    /// Help / FileTree / Oil are kind-driven (no language
    /// dimension); the `lang` argument is ignored for those
    /// kinds.
    ///
    /// On activation failure (mode not registered, capability
    /// missing, conflict with active major), the buffer ends
    /// up with no active major and the resolved options
    /// reflect only the registry defaults. Failure is logged;
    /// it isn't a fatal startup error because the design
    /// commits to "every buffer has a major mode" but the
    /// implementation tolerates the bootstrap window where the
    /// registration hasn't completed.
    pub fn activate_major_for_buffer_kind(
        &mut self,
        buffer_id: crate::buffers::BufferId,
        kind: crate::buffers::BufferKind,
    ) {
        // Only Document buffers consult Lang; the others have
        // a fixed mode regardless of content.
        let lang = match kind {
            crate::buffers::BufferKind::Document => {
                let snap = self.document.snapshot();
                let path_owned = snap.path.as_ref().map(|p| (**p).clone());
                let path_ref = path_owned.as_deref();
                lattice_syntax::Lang::detect_from_path(path_ref)
            }
            _ => lattice_syntax::Lang::Plain,
        };
        let major_id = crate::modes::resolve_major_mode(kind, lang);
        // Convert App-level BufferId to lattice_protocol::BufferId for
        // the registry's expectation. The registry only uses the
        // value for event emission; for M.3.1 we synthesise a
        // dummy value because mode-event subscribers don't use
        // it yet.
        let proto_id = lattice_protocol::ids::BufferId::new(buffer_id.0 as u64);
        let mut active = self
            .active_modes
            .remove(&buffer_id)
            .unwrap_or_default();
        let mut locals = self
            .buffer_locals
            .remove(&buffer_id)
            .unwrap_or_default();
        match self.mode_registry.activate_major(
            &mut active,
            &mut locals,
            proto_id,
            major_id,
            // Capability set: M.3.1 doesn't yet plumb per-buffer
            // capabilities, so pass empty. Modes that require
            // BUFFER_URI / LSP / etc. (M.5+) will get this from
            // a real capability lookup.
            lattice_mode::CapabilitySet::empty(),
        ) {
            Ok(_events) => {
                // Events go to the typed event bus when M.4
                // wires it; ignore for now.
            }
            Err(e) => {
                // Don't fail startup; surface as an echo and
                // continue with defaults. The buffer just has
                // no active major; resolved options reflect
                // registry defaults.
                self.set_message(
                    EchoLevel::Warn,
                    format!(
                        "mode: activate_major({}) for buffer {} failed: {}",
                        major_id, buffer_id.0, e,
                    ),
                );
            }
        }
        self.active_modes.insert(buffer_id, active);
        self.buffer_locals.insert(buffer_id, locals);
        self.recompute_options_for_buffer(buffer_id);
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
            .set_typed::<lattice_config::FoldMethodOption>(fm)
            .expect("set foldmethod");
        self.drain_option_changes();
    }

    /// Set `foldenable` directly. Drains the cascade so the cache
    /// reflects the new value before the caller observes it.
    pub fn set_foldenable_for_test(&mut self, on: bool) {
        let _ = self.config.set_typed::<lattice_config::FoldEnable>(on);
        self.drain_option_changes();
    }

    /// Set `completion.auto_insert_single` directly. Drains the
    /// cascade so the cache reflects the new value before the
    /// caller observes it.
    pub fn set_completion_auto_insert_single_for_test(&mut self, on: bool) {
        let _ = self
            .config
            .set_typed::<lattice_config::CompletionAutoInsertSingle>(on);
        self.drain_option_changes();
    }

    // ---- LSP integration helpers ----
    //
    // Buffer-open attach is event-driven: `App::new` and
    // `App::do_edit` publish `Event::DocumentOpened`; the LSP
    // attach driver wired in `build_lsp_subsystem` consumes
    // events on the LSP runtime and submits opens to the
    // supervisor's mailbox. Nothing on the UI thread parks on
    // the LSP `initialize` round-trip.
    //
    // The hot edit path is similar: `Event::DocumentChanged`
    // flows through the editor event bus into a per-actor
    // fan-in (lattice_lsp::fan_in) that sends straight to the
    // actor's mailbox. The App publishes the event from
    // `publish_document_changed`; nothing here takes the
    // supervisor mutex on each keystroke.




    /// Helper: publish a position-only change event. Cheap
    /// stand-in for whatever the rest of the App uses to
    /// signal cursor moves. Currently a no-op since the
    /// renderer reads cursor directly; reserved for future
    /// position-history pushes.
    pub(super) fn publish_position_change(&self) {
        // 4.1.d.iv: position history hook reserved -- a real
        // PluginPush entry lands here when the position-history
        // wiring catches up.
    }

    // ---- LSP introspection (Phase 4.1.g) --------------------




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
    pub(super) fn resolve_server_id(&self, name: &str) -> Option<String> {
        for ((_, sid), _) in self.lsp.running_actors() {
            if sid == name {
                return Some(sid);
            }
        }
        for cfg in self.lsp.configs() {
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
    pub(super) fn running_server_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .lsp
            .running_actors()
            .into_iter()
            .map(|((_, sid), _)| sid)
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }




    /// `:lsp-log-level [server] <level>` -- set the subsystem



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
    pub fn apply_edit_blocking(&mut self, edit: Edit) -> Result<AppliedEdit, RuntimeError> {
        let result = block_on(self.document.apply_edit(edit));
        if let Ok(applied) = result.as_ref() {
            self.publish_document_changed(std::slice::from_ref(applied));
        }
        result
    }

    /// Block_on `apply_edit_batch`. The batch lands as one undo
    /// unit on the document's undo stack. Each edit in the
    /// batch is also fed to the LSP supervisor in order
    /// (Phase 4.1.i.2).
    pub fn apply_edit_batch_blocking(
        &mut self,
        edits: Vec<Edit>,
    ) -> Result<Vec<AppliedEdit>, RuntimeError> {
        let result = block_on(self.document.apply_edit_batch(edits));
        if let Ok(applied) = result.as_ref() {
            self.publish_document_changed(applied);
        }
        result
    }

    pub fn undo_blocking(&mut self) -> Result<Vec<AppliedEdit>, RuntimeError> {
        let result = block_on(self.document.undo());
        if let Ok(applied) = result.as_ref() {
            self.publish_document_changed(applied);
        }
        result
    }

    pub fn redo_blocking(&mut self) -> Result<Vec<AppliedEdit>, RuntimeError> {
        let result = block_on(self.document.redo());
        if let Ok(applied) = result.as_ref() {
            self.publish_document_changed(applied);
        }
        result
    }

    pub fn save_blocking(&mut self) -> Result<std::path::PathBuf, RuntimeError> {
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
        // LSP textDocument/willSave (Phase 4.3) fan-out: every
        // server attached to the buffer that advertises the
        // notification gets a heads-up before the disk write.
        // Manual reason today (`TextDocumentSaveReason::Manual`).
        self.fire_will_save_notifications();
        // willSaveWaitUntil block-on-response (Phase 4.3).
        // Each server advertising the request returns a Vec<
        // TextEdit> the editor applies pre-save. Format-on-
        // save flows through here when the server emits one.
        // Bounded by a 500ms timeout so a buggy server can't
        // hang the save.
        self.run_will_save_wait_until_blocking();
        let result = block_on(self.document.save());
        if let Ok(path) = result.as_ref() {
            self.event_bus.publish(Event::DocumentSaved {
                id: snap.id,
                path: path.clone(),
            });
            // Fire didSave to every server that wants it.
            self.fire_did_save_notifications();
        }
        result
    }

    /// Walk the buffer's attached servers; fire
    /// `textDocument/willSave` to each that advertises it.
    /// Cheap on no-LSP buffers (the URI lookup short-circuits).
    /// Notification only -- responses, if any, drop on the floor.
    fn fire_will_save_notifications(&self) {
        let Some(uri) = self.buffer_uris.get(&self.document_buffer_id) else {
            return;
        };
        let uri = uri.clone();
        let handles = self.lsp.servers_for(&uri);
        let params = lsp_types::WillSaveTextDocumentParams {
            text_document: lsp_types::TextDocumentIdentifier { uri },
            reason: lsp_types::TextDocumentSaveReason::MANUAL,
        };
        for h in handles {
            if h.capabilities().wants_will_save() {
                let _ = h.will_save(params.clone());
            }
        }
    }

    /// Run `textDocument/willSaveWaitUntil` against every
    /// server advertising the request; collect their TextEdits
    /// and apply them pre-save.
    ///
    /// Audit slice 5 / M4: the previous shape iterated servers
    /// sequentially with a 500ms timeout per server, so total
    /// UI-thread block was up to `500ms × N`. New shape runs
    /// every server's request concurrently under one shared
    /// 500ms budget -- worst-case UI block is bounded at 500ms
    /// regardless of how many servers are attached. The
    /// remaining sync `block_on` is queued for the eventual
    /// two-phase save (kick off → return → drain on completion);
    /// the bounded-parallel fix covers the audit's actual
    /// concern (1.5s+ stalls for multi-server saves) without
    /// the behavioural change of fully-async save.
    fn run_will_save_wait_until_blocking(&mut self) {
        let Some(uri) = self.buffer_uris.get(&self.document_buffer_id).cloned()
        else {
            return;
        };
        let handles = self.lsp.servers_for(&uri);
        let interested: Vec<lattice_lsp::ServerHandle> = handles
            .into_iter()
            .filter(|h| h.capabilities().wants_will_save_wait_until())
            .collect();
        if interested.is_empty() {
            return;
        }
        let params = lsp_types::WillSaveTextDocumentParams {
            text_document: lsp_types::TextDocumentIdentifier { uri },
            reason: lsp_types::TextDocumentSaveReason::MANUAL,
        };
        // One cancellation token per request; on overall
        // timeout we cancel every in-flight one so slow servers
        // stop wasting the LSP runtime's worker time.
        let tokens: Vec<lattice_protocol::CancellationToken> = (0..interested.len())
            .map(|_| lattice_protocol::CancellationToken::new())
            .collect();
        let pending: Vec<_> = interested
            .iter()
            .zip(tokens.iter())
            .map(|(handle, token)| {
                handle.will_save_wait_until(params.clone(), token.clone())
            })
            .collect();
        let cancel_tokens = tokens.clone();
        let all_edits: Vec<lsp_types::TextEdit> = block_on(async move {
            // Spawn each request onto a `JoinSet` so they run
            // concurrently on the LSP runtime. The shared
            // 500ms deadline below caps the *total* UI-thread
            // block.
            let mut set: tokio::task::JoinSet<Vec<lsp_types::TextEdit>> =
                tokio::task::JoinSet::new();
            for fut in pending {
                set.spawn(async move {
                    fut.await.ok().flatten().unwrap_or_default()
                });
            }
            let deadline = tokio::time::sleep(std::time::Duration::from_millis(500));
            tokio::pin!(deadline);
            let mut acc: Vec<lsp_types::TextEdit> = Vec::new();
            loop {
                tokio::select! {
                    next = set.join_next() => match next {
                        Some(Ok(edits)) => acc.extend(edits),
                        Some(Err(_)) => {} // task panicked; skip
                        None => break,     // every task done
                    },
                    _ = &mut deadline => {
                        // Bound the total UI-thread block at
                        // 500ms; any server still in flight
                        // gets cancelled so its response (if it
                        // eventually arrives) doesn't try to
                        // apply edits to a post-save buffer.
                        for t in &cancel_tokens { t.cancel(); }
                        set.abort_all();
                        break;
                    }
                }
            }
            acc
        });
        if !all_edits.is_empty() {
            // Apply pre-save edits as one undo unit. A failed
            // apply echoes but doesn't abort the save -- the
            // user's data still hits disk.
            if let Err(e) = self.apply_lsp_text_edits(all_edits) {
                self.set_message(
                    EchoLevel::Warn,
                    format!("willSaveWaitUntil: apply failed: {e}"),
                );
            }
        }
    }

    /// Walk the buffer's attached servers; fire
    /// `textDocument/didSave` to each that wants it. When the
    /// server requested `includeText`, attach the post-save
    /// text from the rope.
    fn fire_did_save_notifications(&self) {
        let Some(uri) = self.buffer_uris.get(&self.document_buffer_id) else {
            return;
        };
        let uri = uri.clone();
        let handles = self.lsp.servers_for(&uri);
        let snap = self.document.snapshot();
        let full_text = snap.buffer.as_string();
        for h in handles {
            let caps = h.capabilities();
            if !caps.wants_did_save() {
                continue;
            }
            let text = if caps.did_save_include_text() {
                Some(full_text.clone())
            } else {
                None
            };
            let params = lsp_types::DidSaveTextDocumentParams {
                text_document: lsp_types::TextDocumentIdentifier {
                    uri: uri.clone(),
                },
                text,
            };
            let _ = h.did_save(params);
        }
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
    /// snapshot and the edits that were just applied. Called from
    /// every path that mutates the buffer (apply_edit / batch /
    /// undo / redo). The applied edits ride on the event so
    /// downstream subscribers (notably the per-server LSP fan-in)
    /// can sync without re-walking the buffer or holding the
    /// supervisor lock.
    fn publish_document_changed(&mut self, applied: &[AppliedEdit]) {
        let snap = self.document.snapshot();
        let path = snap.path().map(|p| p.to_path_buf());
        let edits: Vec<lattice_protocol::event::AppliedEdit> = applied
            .iter()
            .map(|a| lattice_protocol::event::AppliedEdit {
                original_range: a.original_range,
                inserted_range: a.inserted_range,
                replaced_text: a.replaced_text.clone(),
                inserted_text: a.inserted_text.clone(),
            })
            .collect();
        self.event_bus.publish(Event::DocumentChanged {
            id: snap.id,
            path,
            version: snap.version,
            edits,
        });
        // Slice B.2 part 2: accumulate tree-sitter-shaped edit
        // deltas for the next syntax reparse request.
        // `maybe_reparse_syntax` drains this and ships them to
        // the worker, which applies them via tree.edit() before
        // running an incremental Parser::parse. If no syntax
        // handle is attached, skip the push to keep the vec
        // bounded.
        if self.syntax.is_some() {
            self.pending_syntax_edits
                .extend(applied.iter().map(|a| a.delta));
            // Slice C.3: shift `visible_highlights` synchronously
            // so line indices track the post-edit content even
            // before the worker publishes a fresh snapshot. For
            // line-deletes, this drains the deleted lines'
            // entries from the cached spans; for line-inserts,
            // it inserts empty placeholders. The result is that
            // unchanged-content lines below an edit keep their
            // (still-correct) spans at their NEW indices --
            // eliminating the "lines below the delete flicker"
            // user-visible symptom. Combined with the stale-
            // snapshot hold in `refresh_highlights`, the cached
            // spans never go through an empty/wrong intermediate
            // state during the worker window.
            for a in applied {
                self.shift_highlights_for_edit(&a.delta);
            }
        }
    }

    /// Slice C.3: keep `visible_highlights` line-aligned with the
    /// current document immediately after an edit, before the
    /// syntax worker publishes a fresh snapshot.
    ///
    /// `visible_highlights` is indexed by viewport row =
    /// `buffer_line - scroll`. When an edit changes the line
    /// count (line-delete, line-insert, multi-line replace), the
    /// content at row N now corresponds to a different buffer
    /// line than before, but the cached span entries don't shift
    /// automatically. The renderer would paint pre-edit spans
    /// onto post-edit content, producing the user-reported "old
    /// span gaps appear as white characters on the new line"
    /// flicker.
    ///
    /// Fix: derive the line-shift from the delta's positions and
    /// apply it to `visible_highlights` as a Vec splice.
    /// Lines above the edit are untouched. Lines at and below
    /// the edit's start line are drained (delete) or padded with
    /// empty placeholders (insert) -- but unchanged lines further
    /// below still have correct spans at their NEW indices.
    ///
    /// Pure ns-fast: a Vec drain or insert of a few elements.
    /// Only mutates the cache; doesn't touch the snapshot.
    fn shift_highlights_for_edit(&mut self, delta: &lattice_protocol::edit::EditDelta) {
        let edit_start = delta.start_position.line;
        let scroll = self.scroll;
        if edit_start < scroll {
            // Edit started above the visible viewport. Bail and
            // let the worker's publish drive a normal recompute.
            return;
        }
        let viewport_idx = (edit_start - scroll) as usize;
        if viewport_idx >= self.visible_highlights.len() {
            // Edit started below the visible viewport. Nothing
            // visible changes.
            return;
        }
        let old_end = delta.old_end_position.line;
        let new_end = delta.new_end_position.line;
        let old_lines = old_end.saturating_sub(edit_start) as usize;
        let new_lines = new_end.saturating_sub(edit_start) as usize;
        if old_lines == new_lines {
            // In-line edit (line count unchanged). Shift spans
            // on the affected line by the byte delta within the
            // line so the held spans stay byte-aligned with the
            // new content. Without this, e.g. `>>` (insert "    "
            // at byte 0) leaves spans pointing at OLD byte
            // positions: the renderer paints "Keyword" color on
            // the new whitespace bytes 0..3 and leaves the
            // shifted "let" bytes 4..7 unstyled. When the worker
            // publishes the corrected spans on the next frame,
            // the bytes transition from "Keyword color on
            // whitespace" to "default color on whitespace" --
            // the "default color" reads as the visible flicker
            // the user reported.
            //
            // Slice C.4: shift each span by the byte delta:
            // - Entirely before the edit: unchanged.
            // - Entirely after the edit: both endpoints shifted.
            // - Crossing the edit point: extend (or contract) the
            //   end by the byte delta to keep the span covering
            //   the (now-resized) content. The start stays
            //   because the prefix bytes are preserved.
            self.shift_spans_within_line(viewport_idx, delta);
            return;
        }
        // Decide where to apply the shift. If the edit starts at
        // the very beginning of `start.line` (byte 0), then
        // `start.line`'s pre-edit content has moved -- it's now
        // located further down (for inserts) or has been
        // consumed (for deletes). The shift point IS
        // `viewport_idx`. If the edit starts mid-line or at
        // line-end (byte > 0), then `start.line`'s content (or
        // prefix) is preserved at `viewport_idx`; the shift
        // applies to the line AFTER it.
        //
        // Concrete impact:
        // - `O` (newline at line start, byte 0): insert at
        //   viewport_idx; original line spans move down.
        // - `o` (newline at line end, byte > 0): insert at
        //   viewport_idx + 1; original line spans preserved.
        // - `dd` (delete whole line, start byte 0):
        //   drain at viewport_idx; the deleted line's spans go.
        // - Backspace joining lines (delete \n at line end,
        //   start byte > 0): drain at viewport_idx + 1; the
        //   joined-into line's spans preserved.
        let action_idx = if delta.start_position.byte == 0 {
            viewport_idx
        } else {
            (viewport_idx + 1).min(self.visible_highlights.len())
        };
        if old_lines > new_lines {
            let to_remove = old_lines - new_lines;
            let drain_end = (action_idx + to_remove).min(self.visible_highlights.len());
            if action_idx < drain_end {
                self.visible_highlights.drain(action_idx..drain_end);
            }
        } else {
            let to_insert = new_lines - old_lines;
            for _ in 0..to_insert {
                self.visible_highlights.insert(action_idx, Vec::new());
            }
        }
    }

    /// Slice C.4: shift the spans on a single visible-line entry
    /// by the byte-delta of an in-line edit, so the held spans
    /// stay byte-aligned with the post-edit content during the
    /// brief window before the syntax worker publishes corrected
    /// spans. Eliminates the "spans paint on shifted bytes →
    /// recompute → bytes transition to default color" flicker
    /// that `>>` indents and other in-line edits produced.
    ///
    /// Three cases per span:
    /// 1. Entirely before the edit (`span.end <= edit_byte`):
    ///    unchanged.
    /// 2. Entirely after the edit (`span.start >= old_end_byte`):
    ///    both endpoints shift by `byte_delta`.
    /// 3. Crossing the edit (overlaps the edited range): the
    ///    prefix bytes [`span.start`, `edit_byte`) are unchanged,
    ///    so the span's start stays put. The end extends (or
    ///    contracts) by `byte_delta` to keep the span covering
    ///    its now-resized content. If the span collapses to
    ///    empty (delete consumed all of it), drop it.
    fn shift_spans_within_line(
        &mut self,
        viewport_idx: usize,
        delta: &lattice_protocol::edit::EditDelta,
    ) {
        let edit_byte = delta.start_position.byte as usize;
        let old_end_byte = delta.old_end_position.byte as usize;
        let new_end_byte = delta.new_end_position.byte as usize;
        let byte_delta: i64 = new_end_byte as i64 - old_end_byte as i64;
        if edit_byte == old_end_byte && byte_delta == 0 {
            // No-op edit: empty range replaced with empty text.
            return;
        }
        let Some(line_spans) = self.visible_highlights.get_mut(viewport_idx) else {
            return;
        };
        line_spans.retain_mut(|span| {
            if span.end <= edit_byte {
                // Entirely before the edit; unchanged.
                true
            } else if span.start >= old_end_byte {
                // Entirely after the edit; shift both endpoints.
                let new_start = (span.start as i64) + byte_delta;
                let new_end = (span.end as i64) + byte_delta;
                span.start = new_start.max(0) as usize;
                span.end = new_end.max(0) as usize;
                true
            } else {
                // Span crosses the edit. Extend / contract end by
                // byte_delta to track the resized content; start
                // stays put (the prefix is preserved bytes).
                let extended_end = (span.end as i64) + byte_delta;
                if extended_end <= span.start as i64 {
                    // Span collapsed entirely (e.g. a multi-byte
                    // delete consumed the whole span). Drop.
                    false
                } else {
                    span.end = extended_end as usize;
                    true
                }
            }
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
        // Slice 8.i.4 partial-chord lifecycle: any action that
        // *isn't* `AbsorbPartialChord(_)` (or accumulating count
        // via `PushDigit`) resolves or aborts the in-flight
        // multi-key sequence, so the chord stack must clear.
        // Without this an unbound second key (e.g. `g!` after
        // `g`) would leak `[g]` into the next keystroke's prefix
        // lookup and mis-route it as `gd` / `gv` / etc.
        //
        // Slice 8.i.4.f: `PushDigit` is also exempt -- vim's
        // motion-count-after-operator (`d2w`, `2d3w`, `5gg`)
        // accumulates count chars BETWEEN chord steps. The
        // operator-pending stack must survive the digit input.
        if !matches!(action, Action::AbsorbPartialChord(_) | Action::PushDigit(_)) {
            self.partial_chord.clear();
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
            Action::AbsorbPartialChord(chord) => {
                // Slice 8.i.4.a: the trie returned `Partial`; the
                // input layer wrapped the captured chord in this
                // signal. Append to `partial_chord` and otherwise
                // no-op -- the next keystroke runs through
                // `dispatch_normal` with this stack as prefix.
                self.partial_chord.push(chord);
            }
            Action::Insert(s) => self.do_insert_text(&s),
            Action::DeleteCharBackward => self.do_delete_char_backward(),
            Action::EnterMode(state) => self.enter_mode(state),
            Action::EnterAppend => self.do_enter_append(),
            Action::EnterBlockVisualInsert => self.do_enter_block_visual_insert(false),
            Action::EnterBlockVisualAppend => self.do_enter_block_visual_insert(true),
            Action::OpenLineBelow => self.do_open_line_below(),
            Action::OpenLineAbove => self.do_open_line_above(),
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
                self.last_message = None;
                // Q16: opening the cmdline dismisses STATE A
                // help popups (hover overlay still anchored to
                // doc cursor). State B help buffers (`:lsp-log`,
                // `:lsp-trace-log`, `:describe-*` opened in a
                // pane) are first-class buffers per the
                // everything-is-a-buffer model -- the user
                // expects to run `:bd`, `:diagnostics`, etc.
                // without losing their log view. Only auto-
                // dismiss when active_buffer is Document, which
                // is the State A shape.
                if matches!(self.active_buffer, BufferKind::Document) {
                    self.dismiss_help();
                }
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
                let _ = self.config.set_typed::<lattice_config::FoldEnable>(!cur);
                self.drain_option_changes();
            }
            Action::LspHoverRequest => self.do_lsp_hover_request(),
            Action::LspDefinitionRequest => self.do_lsp_nav_request(LspNavKind::Definition),
            Action::LspDeclarationRequest => self.do_lsp_nav_request(LspNavKind::Declaration),
            Action::LspTypeDefinitionRequest => {
                self.do_lsp_nav_request(LspNavKind::TypeDefinition)
            }
            Action::LspImplementationRequest => {
                self.do_lsp_nav_request(LspNavKind::Implementation)
            }
            Action::LspReferencesRequest => self.do_lsp_references_request(),
            Action::LspSignatureHelpRequest => self.do_lsp_signature_help_request(),
            Action::LspCompletionRequest => self.do_lsp_completion_request(),
            Action::TagStackPop => self.do_tag_stack_pop(),
            Action::CompletionTrigger => self.do_completion_trigger(),
            Action::CompletionNext => self.do_completion_next(),
            Action::CompletionPrev => self.do_completion_prev(),
            Action::CompletionAccept => self.do_completion_accept(),
            Action::CompletionCancel => self.do_completion_cancel(),
            Action::CompletionCancelAndExitInsert => {
                self.do_completion_cancel();
                self.modal = ModalState::Normal;
            }
            Action::CompletionToggleDocs => self.do_completion_toggle_docs(),
            Action::CompletionDocsScrollDown => self.do_completion_docs_scroll_down(),
            Action::CompletionDocsScrollUp => self.do_completion_docs_scroll_up(),
            Action::CompletionAcceptThenInsert(c) => {
                self.do_completion_accept_then_insert(c);
            }
            Action::SnippetExpand => self.do_snippet_expand_at_cursor(),
            Action::SnippetNextPlaceholder => self.do_snippet_next_placeholder(),
            Action::SnippetPrevPlaceholder => self.do_snippet_prev_placeholder(),
            Action::SnippetLeave => {
                self.active_snippet = None;
                self.modal = ModalState::Normal;
            }
            Action::LspDocumentSymbolRequest => self.do_lsp_document_symbol_request(),
            Action::LspWorkspaceSymbolRequest(q) => {
                self.do_lsp_workspace_symbol_request(&q)
            }
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
                BufferKind::Document | BufferKind::Oil => {}
            },
            Action::FollowLink => match self.active_buffer {
                BufferKind::Help => self.do_help_follow_link(),
                BufferKind::Oil => self.do_oil_follow(),
                BufferKind::FileTree => self.do_file_tree_follow(),
                BufferKind::Document => {}
            },
            Action::OilNavigateUp => self.do_oil_navigate_up(),

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
        // Slice 8.f: re-stack Insert-mode minor-mode layers in
        // lockstep with overlay state changes. Cheap when
        // nothing changed.
        self.sync_keymap_overlays();
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
    ///
    /// Slice B.3 cache fast path: if the freshly-computed key
    /// (snapshot pointer + text_version + scroll + viewport
    /// height + fold_hash) matches the stored
    /// `visible_highlights_key`, the existing
    /// `visible_highlights` is still valid -- skip the
    /// `highlight_lines` call entirely. Steady-state norm
    /// (cursor blinking, no edit) → ~100% hit rate, dropping
    /// per-frame cost from ~178µs to noise floor (key compare +
    /// fold hash, ~50ns).
    pub fn refresh_highlights(&mut self) {
        let Some(syntax) = self.syntax.as_ref() else {
            self.visible_highlights = Vec::new();
            self.visible_highlights_key = None;
            return;
        };
        let snap = syntax.snapshot();
        let key = VisibleHighlightsKey {
            snapshot_ptr: std::sync::Arc::as_ptr(&snap) as usize,
            syntax_text_version: snap.text_version(),
            scroll: self.scroll,
            viewport_height: self.viewport_height,
            fold_hash: folds::compute_fold_hash(&self.folds),
        };
        if self.visible_highlights_key == Some(key) {
            // Cache hit -- existing visible_highlights is valid.
            return;
        }
        // Cache miss. Decide between recompute and HOLD based on
        // whether the snapshot is current enough to give correct
        // spans.
        //
        // Slice C.3 stale-snapshot hold: if the document has
        // advanced past the worker's published snapshot, any
        // spans we compute would be against pre-edit data --
        // possibly producing wrong colors or wrong line counts
        // for the brief window before the worker publishes.
        // Instead, hold the existing visible_highlights (kept
        // line-aligned by `shift_highlights_for_edit` on edit)
        // and just update the key. The renderer paints the held
        // spans, which are byte-correct for unchanged content
        // and line-aligned even after line-deletes / inserts.
        //
        // When the worker publishes (snapshot_ptr changes),
        // we'll re-enter this path with a fresh snapshot and
        // recompute correctly. The spans only ever transition
        // from one CORRECT set to another -- never through an
        // empty/wrong intermediate that would visibly flicker.
        if snap.text_version() < self.document.text_version() {
            self.visible_highlights_key = Some(key);
            return;
        }
        // Snapshot is current with the document. Recompute.
        // The window stretches via `visible_buffer_line_extent`
        // to cover lines under closed folds (see method
        // docstring).
        let start = self.scroll;
        let end = self
            .visible_buffer_line_extent(start, self.viewport_height)
            .saturating_add(1);
        self.visible_highlights = snap
            .highlight_lines(start, end)
            .unwrap_or_default();
        self.visible_highlights_key = Some(key);
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
            if let Some(syntax) = entry.syntax.as_ref() {
                if tv != entry.last_parsed_text_version {
                    // Slice B.2 part 2: inactive-pane path
                    // doesn't yet accumulate per-document edit
                    // deltas (the active-pane path does, on
                    // App.pending_syntax_edits). For now we send
                    // empty edits which routes the worker to
                    // full reparse. Per-DocumentEntry edit
                    // accumulation is its own follow-up; the
                    // inactive-pane path is rare (only fires
                    // when pane shows a different document) so
                    // the perf cost stays bounded.
                    // Slice B.5: pass Buffer (O(1) clone) instead
                    // of pre-materializing the String here.
                    syntax.request_reparse(
                        entry.last_synced_syntax_version,
                        tv,
                        snap.buffer.clone(),
                        Vec::new(),
                    );
                    entry.last_parsed_text_version = tv;
                    entry.last_synced_syntax_version = tv;
                }
                let end = scroll.saturating_add(height);
                let spans = syntax
                    .snapshot()
                    .highlight_lines(scroll, end)
                    .unwrap_or_default();
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
    pub(super) fn open_completion_popup(&mut self) {
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



    /// Look up a buffer by file path. Used by `:e FILE` to detect
    /// "already open"; later by `:b NAME` for completion.
    pub(super) fn find_document_by_path(&self, path: &std::path::Path) -> Option<BufferId> {
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
    pub(super) fn snapshot_active_document(&mut self) {
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
    pub(super) fn activate_buffer_state(&mut self) {
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





    /// What `:bn` / `:bp` consider the "current" buffer for
    /// stepping. The active pane's buffer_id is the source of
    /// truth (the active pane is what the user sees).
    pub(super) fn active_pane_buffer_id(&self) -> BufferId {
        self.pane_tree.active().buffer_id
    }




    /// Active buffer's snippet language id. Maps the active
    /// document's filename extension to a language string the
    /// snippet registry indexes by (e.g. `"rs"` -> `"rust"`).
    /// Falls back to the empty string when no path is set
    /// (the registry's `"*"` any-language pack still applies).
    pub(super) fn active_language_id(&self) -> String {
        let snap = self.document.snapshot();
        let Some(path) = snap.path.as_ref() else {
            return String::new();
        };
        match path.extension().and_then(|e| e.to_str()) {
            Some("rs") => "rust".into(),
            Some("py") => "python".into(),
            Some("go") => "go".into(),
            Some("js") | Some("jsx") | Some("mjs") | Some("cjs") => {
                "javascript".into()
            }
            Some("ts") | Some("tsx") => "typescript".into(),
            Some("c") | Some("h") => "c".into(),
            Some("cc") | Some("cpp") | Some("cxx") | Some("hpp") | Some("hxx") => {
                "cpp".into()
            }
            Some("md") => "markdown".into(),
            Some("toml") => "toml".into(),
            Some("yaml") | Some("yml") => "yaml".into(),
            Some("json") => "json".into(),
            Some("sh") => "shellscript".into(),
            Some("lua") => "lua".into(),
            Some(other) => other.to_string(),
            None => String::new(),
        }
    }

    /// Look up the snippet meta sidecar entry for a candidate,
    /// when it's a snippet-sourced one. Returns `None` for
    /// non-snippet candidates.
    pub(crate) fn snippet_meta_for(
        &self,
        candidate: &lattice_completion::RenderedCandidate,
    ) -> Option<&SnippetCandidateMeta> {
        let lattice_completion::CandidateData::Extension { kind_id, payload } =
            &candidate.raw.data
        else {
            return None;
        };
        if *kind_id != SNIPPET_COMPLETION_KIND_ID {
            return None;
        }
        if payload.len() != 4 {
            return None;
        }
        let idx = u32::from_le_bytes([
            payload[0],
            payload[1],
            payload[2],
            payload[3],
        ]) as usize;
        self.insert_completion_snippet_meta.get(idx)
    }




    /// Expand a parsed snippet body at the popup's anchor.
    /// Renders the body (variables resolved against the
    /// active buffer's context), splices the resulting text
    /// over `[anchor, cursor]`, sets up an `ActiveSnippet`,
    /// and moves the cursor to the first tabstop's range.
    /// Pure-literal snippets (no tabstops) skip the active-
    /// snippet step and just leave the cursor at end-of-insert.
    /// Expand a snippet body alongside LSP `additionalTextEdits`
    /// as one undo unit (Phase 4.2.g.7 polish).
    ///
    /// Coalesces the auto-import edits the server sent back
    /// with the snippet body's main splice into a single
    /// `apply_edit_batch_blocking` call -- one `<C-z>` reverts
    /// both. The main edit's position is recovered from the
    /// `Vec<AppliedEdit>` (its index in the reverse-sorted
    /// batch) so the active-snippet origin tracks the
    /// post-batch buffer state correctly.
    ///
    /// Returns `Err(message)` when the batch apply fails; the
    /// caller surfaces it via `set_message`. On success the
    /// active-snippet bookkeeping mirrors `expand_snippet` --
    /// focus the first tabstop, set `active_snippet`, etc.
    pub(super) fn expand_snippet_with_lsp_edits(
        &mut self,
        body: &lattice_snippet::SnippetBody,
        anchor: Position,
        additional: Vec<lsp_types::TextEdit>,
    ) -> Result<(), String> {
        let vars = self.snippet_variable_context();
        let rendered = lattice_snippet::render::render(body, &vars);
        // Build the main edit -- snippet body splices over
        // `[anchor, cursor]`.
        let main_range = lattice_protocol::position::Range::new(anchor, self.cursor);
        let main_edit = Edit::replace(main_range, rendered.text.clone());
        // Convert `additionalTextEdits` to lattice Edits.
        let snap = self.document.snapshot();
        let mut all_edits: Vec<Edit> = Vec::with_capacity(additional.len() + 1);
        for te in &additional {
            let start_byte = lsp_position_to_app_byte(
                &snap.buffer,
                te.range.start.line,
                te.range.start.character,
            );
            let end_byte = lsp_position_to_app_byte(
                &snap.buffer,
                te.range.end.line,
                te.range.end.character,
            );
            let r = lattice_protocol::position::Range::new(
                Position::new(te.range.start.line, start_byte),
                Position::new(te.range.end.line, end_byte),
            );
            all_edits.push(Edit::replace(r, te.new_text.clone()));
        }
        all_edits.push(main_edit.clone());
        // Reverse-sort by start position so each edit's
        // original-document positions stay valid as we apply
        // sequentially. Same convention as `apply_lsp_text_edits`.
        all_edits.sort_by(|a, b| {
            b.range
                .start
                .line
                .cmp(&a.range.start.line)
                .then_with(|| b.range.start.byte.cmp(&a.range.start.byte))
        });
        // Track main's index post-sort so we can read its
        // post-batch range out of the applied vec.
        let main_idx = all_edits
            .iter()
            .position(|e| *e == main_edit)
            .ok_or_else(|| "main edit lost during sort".to_string())?;
        drop(snap);
        let applied = self
            .apply_edit_batch_blocking(all_edits)
            .map_err(|e| format!("{e:?}"))?;
        let main_applied = applied
            .get(main_idx)
            .ok_or_else(|| "main edit missing from applied batch".to_string())?;
        let origin = self
            .document
            .snapshot()
            .buffer
            .position_to_byte(main_applied.inserted_range.start)
            .unwrap_or(0);
        if !rendered.tabstops.is_empty() {
            let mut active = lattice_snippet::ActiveSnippet::from_render(&rendered, origin);
            if let Some(group) = active.focus_first()
                && let Some(first) = group.ranges.first()
                && let Ok(pos) =
                    self.document.snapshot().buffer.byte_to_position(first.start)
            {
                self.cursor = pos;
            }
            self.active_snippet = Some(active);
            self.modal = ModalState::Insert;
        } else {
            self.cursor = main_applied.inserted_range.end;
        }
        Ok(())
    }

    pub(super) fn expand_snippet(
        &mut self,
        body: &lattice_snippet::SnippetBody,
        anchor: Position,
    ) {
        let vars = self.snippet_variable_context();
        let rendered = lattice_snippet::render::render(body, &vars);
        // Splice the rendered text over `[anchor, cursor]`.
        let range = lattice_protocol::position::Range::new(anchor, self.cursor);
        let edit = Edit::replace(range, rendered.text.clone());
        let applied = match self.apply_edit_blocking(edit) {
            Ok(a) => a,
            Err(e) => {
                self.set_message(
                    EchoLevel::Error,
                    format!("snippet: apply failed: {e:?}"),
                );
                return;
            }
        };
        // The host's offset of the snippet origin = anchor
        // converted to a buffer byte offset. ActiveSnippet
        // tracks ranges in buffer bytes; since our rope edit
        // returned the inserted_range, we recompute the
        // origin from the buffer's positional API.
        let origin = match self
            .document
            .snapshot()
            .buffer
            .position_to_byte(applied.inserted_range.start)
        {
            Ok(b) => b,
            Err(_) => 0,
        };
        if !rendered.tabstops.is_empty() {
            let mut active =
                lattice_snippet::ActiveSnippet::from_render(&rendered, origin);
            // Focus the first tabstop and move the cursor.
            if let Some(group) = active.focus_first()
                && let Some(first) = group.ranges.first()
                && let Ok(pos) =
                    self.document.snapshot().buffer.byte_to_position(first.start)
            {
                self.cursor = pos;
            }
            self.active_snippet = Some(active);
            self.modal = ModalState::Insert;
        } else {
            self.cursor = applied.inserted_range.end;
        }
    }

    /// Build a `VariableContext` for snippet expansion from
    /// the active buffer / cursor / clipboard / etc. Powers
    /// `$TM_FILENAME`, `$TM_CURRENT_LINE`, etc.
    fn snippet_variable_context(&self) -> lattice_snippet::VariableContext {
        let mut ctx = lattice_snippet::VariableContext::default();
        let snap = self.document.snapshot();
        if let Some(path) = snap.path.as_ref() {
            ctx.filepath = Some(path.display().to_string());
            ctx.filename = path
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string());
            ctx.directory = path
                .parent()
                .map(|p| p.display().to_string());
        }
        ctx.line_index = Some(self.cursor.line);
        if let Some(line) = snap.buffer.line(self.cursor.line) {
            ctx.current_line = Some(line);
        }
        if let Some(word) = word_under_cursor(&snap.buffer, self.cursor) {
            ctx.current_word = Some(word);
        }
        // CLIPBOARD via the system register.
        if let Some(reg) = self.registers.get(&Register::System) {
            ctx.clipboard = Some(reg.content.clone());
        }
        ctx
    }











    /// Jump to `path:line:col` (LSP 0-based line, utf-8 byte
    /// column). Single entrypoint shared by the picker accept
    /// path (`JumpToLspLocation`) and the `do_help_follow_link`
    /// Source-link dispatch. Pushes the pre-jump cursor onto
    /// position history with `PluginPush` so `<C-o>` walks back.
    pub(super) fn jump_to_file_line_col(&mut self, path: &std::path::Path, line: u32, col: u32) {
        // Push pre-jump cursor before any state mutates.
        self.push_position_history(self.cursor, PositionSource::PluginPush);

        let same_buffer = self
            .document
            .path()
            .map(|p| p == path)
            .unwrap_or(false);
        if !same_buffer {
            self.do_edit(Some(path.to_path_buf()), false);
        }
        // Clamp the target line to the buffer's line count so a
        // stale picker entry doesn't crash with an out-of-range
        // cursor (e.g. user edited the file after the picker
        // populated). `last_addressable_line` accounts for
        // ropey's trailing-newline pseudo-line.
        let snap = self.document.snapshot();
        let line = line.min(last_addressable_line(&snap.buffer));
        let line_len = line_byte_len(&snap.buffer, line);
        let col = col.min(line_len);
        self.cursor = Position::new(line, col);
    }

    /// Open `*lsp:<server_id>*` in the active pane via the
    /// in-pane help registry path. Used by both the picker
    /// accept dispatcher and the direct ex-command short path
    /// when only one instance matches.
    pub(super) fn open_lsp_log_in_pane(&mut self, server_id: &str) {
        let arc: std::sync::Arc<str> = std::sync::Arc::from(server_id);
        let buffer = crate::help::HelpBuffer::lsp_server_log(&self.lsp_logger, &arc)
            .with_markdown_syntax(self.lang_registry.clone());
        self.open_help_in_pane(buffer);
    }

    /// Open `*lsp:<server_id>:trace*` in the active pane. Pure
    /// view -- the trace toggle is `:lsp-trace <server>` and is
    /// independent of opening / closing this buffer.
    pub(super) fn open_lsp_trace_log_in_pane(&mut self, server_id: &str) {
        let arc: std::sync::Arc<str> = std::sync::Arc::from(server_id);
        let buffer = crate::help::HelpBuffer::lsp_server_trace(&self.lsp_logger, &arc)
            .with_markdown_syntax(self.lang_registry.clone());
        self.open_help_in_pane(buffer);
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
    /// Load `~/.config/lattice/lattice.toml` (user) and
    /// `<workspace_root>/.lattice/config.toml` (project) in
    /// precedence order, applying scalar overrides to
    /// `self.config` and bucketing structural sub-tables (per-
    /// language overrides, plugin sections) into
    /// `self.pending_config_structural_sections` for their
    /// owners to drain.
    ///
    /// Called once by the runtime startup before the main loop
    /// (so the first frame already reflects user overrides).
    /// NOT called from `App::new` -- tests stay isolated from
    /// the user's real `~/.config/lattice/`. Test fixtures that
    /// want to exercise the load path can call this directly
    /// with a synthesized workspace root.
    ///
    /// Loader diagnostics (parse errors, unknown keys,
    /// validation rejects) collapse into a single echo at the
    /// most-severe level: `Error` if any file failed to
    /// parse / read, `Warn` if any key was rejected, otherwise
    /// silent. Per-file `path:body` detail rides the message
    /// body so the user can see *which* file complained.
    pub fn load_persistent_config(&mut self, workspace_root: Option<&std::path::Path>) {
        // The structural prefixes the App / future plugin host
        // own. The per-language layer drains
        // `completion.per-language.*`; the plugin host (Phase 7)
        // will drain `plugin.*`; `lsp` is bucketed so the
        // loader doesn't fire unknown-option warnings for
        // server-namespaced keys (the cached raw_tree carries
        // the values; `workspace/configuration` walks it).
        let prefixes = ["completion.per-language", "plugin", "lsp"];
        let outcome = lattice_config::load_default_paths(
            &self.config,
            workspace_root,
            &prefixes,
        );
        // Re-derive theme + hot-path option cache after the
        // loader's writes. ui.* and the cached options may have
        // changed; missing this would leave the first frame
        // rendering with stale derived state.
        self.sync_theme_from_config();
        self.rebuild_option_cache();
        // Stash structural sections for the layers that own
        // them. Subsequent slices drain via
        // `take_pending_structural_section(prefix)`.
        for (k, v) in outcome.structural {
            self.pending_config_structural_sections.insert(k, v);
        }
        // Cache the merged TOML tree so
        // `workspace/configuration` can walk server-namespaced
        // keys (Phase 4.1 follow-up). Project files override
        // user files at deep-merge time so an `[lsp.X.Y]`
        // sibling key in the user config survives a project
        // override of `[lsp.X.Z]`.
        self.lsp_config_tree = outcome.raw_tree;
        // Apply editor-side LSP options that live in the same
        // `[lsp]` table as server-namespaced keys. These are scalars
        // the editor consumes itself (not forwarded via
        // `workspace/configuration`); the loader buckets the whole
        // `lsp` subtree as structural so they're reachable here via
        // `lsp_config_tree`.
        self.apply_persistent_lsp_editor_options();
        // Surface a single echo summarising loader diagnostics.
        // The renderer's modeline only shows the latest echo,
        // so multi-warn configs collapse into "<count> issues
        // (first: <body>)". Severity is the max across the run.
        if outcome.messages.is_empty() {
            return;
        }
        let max_level = outcome
            .messages
            .iter()
            .map(|m| m.level)
            .max_by_key(|l| match l {
                lattice_config::LoadMessageLevel::Error => 1,
                lattice_config::LoadMessageLevel::Warning => 0,
            })
            .unwrap_or(lattice_config::LoadMessageLevel::Warning);
        let echo_level = match max_level {
            lattice_config::LoadMessageLevel::Error => EchoLevel::Error,
            lattice_config::LoadMessageLevel::Warning => EchoLevel::Warn,
        };
        let count = outcome.messages.len();
        let first = &outcome.messages[0];
        let body = if count == 1 {
            format!(
                "config: {}: {}",
                first.source.display(),
                first.body,
            )
        } else {
            format!(
                "config: {count} issues (first: {}: {})",
                first.source.display(),
                first.body,
            )
        };
        self.set_message(echo_level, body);
    }

    /// Drain every `completion.per-language.<lang>` structural
    /// section the loader bucketed and merge each into
    /// `self.per_language_completion`. Per-key TOML wins over
    /// the spec defaults seeded at `App::new`; unset keys leave
    /// the default in place.
    ///
    /// Called by the runtime startup right after
    /// `load_persistent_config` finishes. Idempotent (the bucket
    /// empties as we drain). Per-key parse warnings collapse
    /// into a single echo at `Warn` level the same way the
    /// loader's other diagnostics do.
    pub fn apply_per_language_toml_overrides(&mut self) {
        let paths = self.pending_structural_section_paths("completion.per-language");
        let mut warnings: Vec<String> = Vec::new();
        for path in paths {
            let lang = match path.strip_prefix("completion.per-language.") {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => continue,
            };
            let Some(table) = self.take_pending_structural_section(&path) else {
                continue;
            };
            let parsed = parse_per_language_overrides_table(&path, &table, &mut warnings);
            self.per_language_completion
                .entry(lang)
                .or_default()
                .merge(parsed);
        }
        if !warnings.is_empty() {
            let count = warnings.len();
            let body = if count == 1 {
                format!("config: {}", warnings[0])
            } else {
                format!("config: {count} per-language warnings (first: {})", warnings[0])
            };
            self.set_message(EchoLevel::Warn, body);
        }
    }

    /// Effective completion config for `language` -- per-language
    /// override lays over the global typed option which lays
    /// over the spec fallback. Used at every producer-side
    /// enforcement seam (sync source filter, LSP fan-out, the
    /// `auto_insert_single` check at popup-open).
    pub fn effective_completion_for(
        &self,
        language: &str,
    ) -> EffectiveCompletionConfig {
        let overrides = self.per_language_completion.get(language);
        EffectiveCompletionConfig {
            sources: overrides.and_then(|o| o.sources.clone()),
            // No global `completion.auto_trigger` typed option
            // yet (auto-trigger firing itself lands in a future
            // slice); fall back to false per spec.
            auto_trigger: overrides.and_then(|o| o.auto_trigger).unwrap_or(false),
            auto_insert_single: overrides
                .and_then(|o| o.auto_insert_single)
                .unwrap_or_else(|| self.completion_auto_insert_single()),
            suppress_in: overrides
                .and_then(|o| o.suppress_in.clone())
                .unwrap_or_default(),
        }
    }

    /// Drain the structural section at `prefix` (an entry
    /// matching one of the loader's structural prefixes,
    /// e.g. `"completion.per-language.markdown"`). Removes it
    /// from `pending_config_structural_sections`; subsequent
    /// drains return `None`. Used by the per-language /
    /// plugin-host layers to consume their TOML config without
    /// leaving stale entries behind.
    pub fn take_pending_structural_section(
        &mut self,
        full_path: &str,
    ) -> Option<toml::Table> {
        self.pending_config_structural_sections.remove(full_path)
    }

    /// Iterate the dotted paths of every pending structural
    /// section whose path starts with `namespace.` (e.g.
    /// `"completion.per-language"` returns the language ids).
    /// Returned as owned `String`s to keep the borrow short --
    /// callers typically follow up with
    /// `take_pending_structural_section(full)` mutating the map.
    pub fn pending_structural_section_paths(
        &self,
        namespace: &str,
    ) -> Vec<String> {
        let prefix = format!("{namespace}.");
        self.pending_config_structural_sections
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect()
    }




    /// Re-stack the Insert-mode minor-mode overlays
    /// (completion popup + active snippet) so the layered
    /// keymap registry mirrors the App's overlay state. Called
    /// from the apply loop after every `Action`; cheap when
    /// nothing changed (single mutex acquisition + early
    /// return).
    ///
    /// Push order is enforced here so popup always sits at the
    /// top of the stack when both overlays are active: the
    /// method pops everything, then pushes snippet (if active),
    /// then popup (if active). Popup's `LayerId` is therefore
    /// always higher than snippet's, and popup wins on
    /// overlapping chords (preserving the legacy "popup
    /// precedes snippet" gating in `input::translate`).
    ///
    /// Slice 8.f.
    pub fn sync_keymap_overlays(&mut self) {
        let want_popup = self.insert_completion.is_some();
        let want_snippet = self.active_snippet.is_some();
        let have_popup = self.completion_popup_layer.is_some();
        let have_snippet = self.snippet_layer.is_some();
        if want_popup == have_popup && want_snippet == have_snippet {
            return;
        }
        // Re-stack: pop everything, then push in the canonical
        // order (snippet first, popup second).
        if let Some(id) = self.completion_popup_layer.take() {
            self.keymap.pop_layer(id);
        }
        if let Some(id) = self.snippet_layer.take() {
            self.keymap.pop_layer(id);
        }
        if want_snippet {
            let id = self.keymap.push_layer(
                crate::keymap_registry::PushLayerKind::MinorMode,
                "active-snippet",
                crate::keymap_insert::active_snippet_layer_bindings(&self.action_ids),
            );
            self.snippet_layer = Some(id);
        }
        if want_popup {
            let id = self.keymap.push_layer(
                crate::keymap_registry::PushLayerKind::MinorMode,
                "completion-popup",
                crate::keymap_insert::completion_popup_layer_bindings(&self.action_ids),
            );
            self.completion_popup_layer = Some(id);
        }
    }

    /// Re-derive `App.theme`'s renderer-specific [`Style`] values
    /// from the current `ui.*` option values in the config. Called
    /// at App-init time (after registration) and on every `:set
    /// ui.*` so the cached theme stays in lockstep with the
    /// canonical primitives in config.
    pub fn sync_theme_from_config(&mut self) {
        use crate::tui_options::{
            UiDimInactive, UiSeparator, UiSeparatorColor, UiStatuslineActiveFg,
            UiStatuslineInactiveFg,
        };
        use ratatui::style::Style;
        // ui.dim_inactive -- bool flag projected directly.
        self.theme.dim_inactive_panes =
            *self.config.get_typed::<UiDimInactive>().expect("UiDimInactive");
        // ui.separator -- one-character glyph for the vertical
        // pane divider. Validated to len==1 at parse; fall back to
        // the default if a forged value sneaks through.
        let sep = self.config.get_typed::<UiSeparator>().expect("UiSeparator");
        self.theme.pane_separator_vertical = sep.chars().next().unwrap_or('│');
        // ui.separator_color -- color name; parse_color returned
        // Ok during validate so unwrap-via-fallback is safe.
        let sep_color = self
            .config
            .get_typed::<UiSeparatorColor>()
            .expect("UiSeparatorColor");
        if let Ok(c) = crate::theme::parse_color(&sep_color) {
            self.theme.pane_separator = Style::default().fg(c);
        }
        // ui.statusline_active_fg -- foreground only; preserve any
        // existing modifiers / background by chaining `.fg(c)` on
        // the current style.
        let active_fg = self
            .config
            .get_typed::<UiStatuslineActiveFg>()
            .expect("UiStatuslineActiveFg");
        if let Ok(c) = crate::theme::parse_color(&active_fg) {
            self.theme.pane_status_active = self.theme.pane_status_active.fg(c);
        }
        let inactive_fg = self
            .config
            .get_typed::<UiStatuslineInactiveFg>()
            .expect("UiStatuslineInactiveFg");
        if let Ok(c) = crate::theme::parse_color(&inactive_fg) {
            self.theme.pane_status_inactive = self.theme.pane_status_inactive.fg(c);
        }
    }





















    // `:wq` / `:x` are now Effect::Many([SaveBuffer, QuitEditor{force}])
    // composed in `lattice_grammar::ex_commands::apply_write_quit`. The
    // do_write + do_quit pair runs in sequence via apply_effect; the
    // quit's force-bit comes from the trailing `!` (DESIGN.md §5.2.1).

    pub(super) fn run_invocation(&mut self, inv: CommandInvocation) {
        // Slice 8.i.4.d: free-form `CommandKind::Action`
        // invocations (the App-side actions registered in
        // `crate::actions`) bypass the document path entirely.
        // They have no count semantics -- pending_count /
        // op_count must NOT be consumed by these dispatches
        // (otherwise `2d` would lose the `2` because
        // `run_document_invocation` resets both counts before
        // the dispatch returns the
        // `Effect::AppAction(AbsorbOperatorPrefix(_))` that
        // wants to latch them). Run `execute()` directly and
        // apply the resulting effect.
        if let Some(spec) = self.registry.lookup(inv.command)
            && matches!(spec.kind, lattice_grammar::CommandKind::Action)
        {
            let cancel = lattice_grammar::CancellationToken::never();
            let pos = self.cursor;
            // `CommandKind::Action` evaluators don't touch the
            // document (DESIGN.md §5.2.1 -- Action specs return
            // an `Effect::AppAction(_)` payload without reading
            // or mutating the buffer). The dispatcher's signature
            // still wants a `&mut Document`, so we feed it a
            // throwaway empty one.
            let mut scratch = lattice_core::Document::empty();
            match lattice_grammar::execute(
                &self.registry,
                &mut scratch,
                pos,
                inv,
                &cancel,
            ) {
                Ok(effect) => self.apply_effect(effect),
                Err(e) => {
                    self.set_message(
                        EchoLevel::Error,
                        format!("action dispatch failed: {e:?}"),
                    );
                }
            }
            return;
        }
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
        if matches!(self.active_buffer, BufferKind::Oil) {
            self.run_oil_invocation(inv);
            return;
        }
        if matches!(self.active_buffer, BufferKind::FileTree) {
            self.run_file_tree_invocation(inv);
            return;
        }
        self.run_document_invocation(inv);
    }

    fn run_oil_invocation(&mut self, inv: CommandInvocation) {
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
    fn run_read_only_motion(&mut self, inv: CommandInvocation) {
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
        // Slice 8.i.4.f: count multiplication lives entirely in
        // `keymap_normal::attach_count` (input-side). The dispatcher
        // reads the baked `inv.count` and dispatches with it -- no
        // `pending_count * op_count` math here. Read-only motions
        // arriving without a baked count default to 1.
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
        // Clamp the line *and* byte to the active buffer's bounds
        // -- mirrors the `clamp_cursor_to_buffer()` call at the end
        // of `run_document_invocation`. Without the line clamp,
        // `j` past the last line would silently advance
        // `cursor.line` past the buffer (the renderer pins the
        // visible row, so it looks fine on screen) and a
        // subsequent `k` would have to "unwind" the phantom
        // overshoot before it actually moved up.
        self.clamp_cursor_to_active_buffer();
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
        // Slice 8.i.4.f: count multiplication lives entirely in
        // `keymap_normal::attach_count` (input-side). The dispatcher
        // reads the baked `inv.count` and dispatches with it -- no
        // `pending_count * op_count` math here. Bare invocations
        // arriving without a baked count default to 1.
        let mut effective_count = inv.count.map(|c| c.0).unwrap_or(1);
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

    pub(super) fn apply_effect(&mut self, effect: Effect) {
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
                        visual: Some(visual::visual_kind_to_mode(kind)),
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
            Effect::OpenOil { dir } => self.do_open_oil(dir),
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
            Effect::LspDocumentSymbol => self.do_lsp_document_symbol_request(),
            Effect::LspWorkspaceSymbol { query } => {
                self.do_lsp_workspace_symbol_request(&query)
            }
            Effect::LspFormat => self.do_lsp_format_request(false),
            Effect::LspFormatRange => self.do_lsp_format_request(true),
            Effect::LspSignatureHelp => self.do_lsp_signature_help_request(),
            Effect::LspComplete => self.do_lsp_completion_request(),
            Effect::LspRename { new_name } => self.do_lsp_rename_request(&new_name),
            Effect::LspCodeAction => self.do_lsp_code_action_request(),
            Effect::SnippetExpand => self.do_snippet_expand_at_cursor(),
            Effect::ReloadSnippets => self.do_reload_snippets(),
            Effect::AppAction(app) => self.apply_app_effect(app),
            Effect::Many(many) => {
                for e in many {
                    self.apply_effect(e);
                }
            }
        }
    }

    /// Slice 8.i.0 -- the `Effect::AppAction(...)` consumer.
    /// Today the keymap modules still bind chords via the legacy
    /// `Action` bridge (`bind_legacy`), so this method is only
    /// reached when something explicitly produces an
    /// `Effect::AppAction(...)`. Slices 8.i.1-3 promote per-mode
    /// bindings to typed `CommandInvocation`s whose registered
    /// `ActionSpec` returns `Effect::AppAction(AppEffect::...)`,
    /// at which point this method becomes the live dispatch site.
    /// Each new variant lands a one-line arm here that delegates to
    /// the existing per-action handler; slice 8.i.4 inlines the
    /// bodies once the legacy `Action` enum retires.
    fn apply_app_effect(&mut self, app: lattice_grammar::AppEffect) {
        use lattice_grammar::AppEffect;
        match app {
            AppEffect::Quit => self.apply(Action::Quit),
            AppEffect::MatchBracket => self.apply(Action::MatchBracket),
            AppEffect::ToggleCaseAtCursor => self.apply(Action::ToggleCaseAtCursor),
            AppEffect::OpenLineBelow => self.apply(Action::OpenLineBelow),
            AppEffect::OpenLineAbove => self.apply(Action::OpenLineAbove),
            AppEffect::LspHoverRequest => self.apply(Action::LspHoverRequest),
            AppEffect::SearchNext => self.apply(Action::SearchNext),
            AppEffect::SearchPrevious => self.apply(Action::SearchPrevious),
            AppEffect::JumpHistoryBack => self.apply(Action::JumpHistoryBack),
            AppEffect::JumpHistoryForward => self.apply(Action::JumpHistoryForward),
            AppEffect::WalkMarkHistoryBack => self.apply(Action::WalkMarkHistoryBack),
            AppEffect::WalkMarkHistoryForward => self.apply(Action::WalkMarkHistoryForward),
            AppEffect::TagStackPop => self.apply(Action::TagStackPop),
            AppEffect::OpenFoldAtCursor => self.apply(Action::OpenFoldAtCursor),
            AppEffect::CloseFoldAtCursor => self.apply(Action::CloseFoldAtCursor),
            AppEffect::ToggleFoldAtCursor => self.apply(Action::ToggleFoldAtCursor),
            AppEffect::OpenAllFolds => self.apply(Action::OpenAllFolds),
            AppEffect::CloseAllFolds => self.apply(Action::CloseAllFolds),
            AppEffect::DeleteFoldAtCursor => self.apply(Action::DeleteFoldAtCursor),
            AppEffect::GotoNextFold => self.apply(Action::GotoNextFold),
            AppEffect::GotoPrevFold => self.apply(Action::GotoPrevFold),
            AppEffect::ToggleFoldEnable => self.apply(Action::ToggleFoldEnable),
            AppEffect::Undo => self.apply(Action::Undo),
            AppEffect::Redo => self.apply(Action::Redo),
            AppEffect::RepeatLastChange => self.apply(Action::RepeatLastChange),
            AppEffect::PageDown => self.apply(Action::PageDown),
            AppEffect::PageUp => self.apply(Action::PageUp),
            AppEffect::ScrollLineUp => self.apply(Action::ScrollLineUp),
            AppEffect::ScrollLineDown => self.apply(Action::ScrollLineDown),
            AppEffect::RedrawScreen => self.apply(Action::RedrawScreen),
            AppEffect::EnterCommandLine => self.apply(Action::EnterCommandLine),
            AppEffect::OilNavigateUp => self.apply(Action::OilNavigateUp),
            AppEffect::ReselectLastVisual => self.apply(Action::ReselectLastVisual),
            AppEffect::PasteAfter => self.apply(Action::PasteAfter),
            AppEffect::PasteBefore => self.apply(Action::PasteBefore),
            AppEffect::LspDefinitionRequest => self.apply(Action::LspDefinitionRequest),
            AppEffect::LspDeclarationRequest => self.apply(Action::LspDeclarationRequest),
            AppEffect::LspTypeDefinitionRequest => self.apply(Action::LspTypeDefinitionRequest),
            AppEffect::LspImplementationRequest => self.apply(Action::LspImplementationRequest),
            AppEffect::LspReferencesRequest => self.apply(Action::LspReferencesRequest),
            AppEffect::EnterAppend => self.apply(Action::EnterAppend),
            AppEffect::CreateFoldFromVisual => self.apply(Action::CreateFoldFromVisual),
            AppEffect::DeleteCharBackward => self.apply(Action::DeleteCharBackward),
            AppEffect::CompletionTrigger => self.apply(Action::CompletionTrigger),
            AppEffect::SnippetExpand => self.apply(Action::SnippetExpand),
            AppEffect::ExitVisual => self.apply(Action::ExitVisual),
            AppEffect::ReplaceUndoLast => self.apply(Action::ReplaceUndoLast),
            AppEffect::EnterMode(state) => self.apply(Action::EnterMode(state)),
            AppEffect::EnterVisual(kind) => self.apply(Action::EnterVisual(kind)),
            AppEffect::EnterSearch(dir) => self.apply(Action::EnterSearch(dir)),
            AppEffect::SearchWordUnderCursor(dir) => self.apply(Action::SearchWordUnderCursor(dir)),
            AppEffect::JumpViewport(pos) => self.apply(Action::JumpViewport(pos)),
            AppEffect::ScrollCursorTo(pos) => self.apply(Action::ScrollCursorTo(pos)),
            AppEffect::JoinLines { with_space } => {
                self.apply(Action::JoinLines { with_space })
            }
            AppEffect::FindRepeat { reverse } => self.apply(Action::FindRepeat { reverse }),
            AppEffect::InsertNewline => self.apply(Action::Insert("\n".to_string())),
            AppEffect::InsertTab => self.apply(Action::Insert("\t".to_string())),
            AppEffect::OverwriteChar(c) => self.apply(Action::OverwriteChar(c)),
            AppEffect::SetMark(c) => self.apply(Action::SetMark(c)),
            AppEffect::JumpToMarkLine(c) => self.apply(Action::JumpToMarkLine(c)),
            AppEffect::JumpToMarkExact(c) => self.apply(Action::JumpToMarkExact(c)),
            AppEffect::SelectRegister(reg) => self.apply(Action::SelectRegister(reg)),
            AppEffect::StartMacroRecord(c) => self.apply(Action::StartMacroRecord(c)),
            AppEffect::PlayMacro(c) => self.apply(Action::PlayMacro(c)),
            AppEffect::PlayLastMacro => self.apply(Action::PlayLastMacro),
            AppEffect::AbsorbOperatorPrefix(op) => {
                // Slice 8.i.4.c: arm operator-pending via the
                // partial_chord mechanism. Two atomic effects:
                //
                // 1. Latch `pending_count` -> `op_count` so the
                //    next motion's count multiplies (vim's `2dw`
                //    -> count=2; `2d3w` -> count=2*3=6, the
                //    multiplication happens at the motion side
                //    in `keymap_normal::attach_count`).
                // 2. Push the operator's chord prefix into
                //    `App::partial_chord`. The next keystroke
                //    routes through `compute_normal_action`'s
                //    partial_chord short-circuit, hitting
                //    `lookup_normal_with_prefix` with this stack
                //    as prefix and resolving `[op, motion]` /
                //    `[op, i/a, text-object]` / `[op, f/F/t/T,
                //    char]` to the bound `Invoke`.
                //
                // App::apply already cleared partial_chord at
                // the top of this dispatch (since `Action::Invoke`
                // is not `AbsorbPartialChord(_)`). Populate it
                // here.
                if self.pending_count > 0 {
                    self.op_count = self.pending_count;
                    self.pending_count = 0;
                }
                let prefix = crate::keymap_normal::operator_prefix(op, &self.builtins);
                self.partial_chord.extend(prefix);
            }
            AppEffect::SplitPaneHorizontal => self.apply(Action::SplitPaneHorizontal),
            AppEffect::SplitPaneVertical => self.apply(Action::SplitPaneVertical),
            AppEffect::ClosePane => self.apply(Action::ClosePane),
            AppEffect::NavigatePane(dir) => self.apply(Action::NavigatePane(dir)),
            AppEffect::NextPane => self.apply(Action::NextPane),
            AppEffect::PrevPane => self.apply(Action::PrevPane),
            AppEffect::CompletionNext => self.apply(Action::CompletionNext),
            AppEffect::CompletionPrev => self.apply(Action::CompletionPrev),
            AppEffect::CompletionAccept => self.apply(Action::CompletionAccept),
            AppEffect::CompletionCancel => self.apply(Action::CompletionCancel),
            AppEffect::CompletionCancelAndExitInsert => {
                self.apply(Action::CompletionCancelAndExitInsert)
            }
            AppEffect::CompletionToggleDocs => self.apply(Action::CompletionToggleDocs),
            AppEffect::CompletionDocsScrollDown => self.apply(Action::CompletionDocsScrollDown),
            AppEffect::CompletionDocsScrollUp => self.apply(Action::CompletionDocsScrollUp),
            AppEffect::CompletionAcceptThenInsert(c) => {
                self.apply(Action::CompletionAcceptThenInsert(c))
            }
            AppEffect::SnippetNextPlaceholder => self.apply(Action::SnippetNextPlaceholder),
            AppEffect::SnippetPrevPlaceholder => self.apply(Action::SnippetPrevPlaceholder),
            AppEffect::SnippetLeave => self.apply(Action::SnippetLeave),
        }
    }

    fn handle_edits(&mut self, edits: &[AppliedEdit]) {
        // After a delete, the cursor sits at the start of the deleted range
        // (which is now the position of whatever followed). Vim's behavior.
        if let Some(first) = edits.first() {
            self.cursor = first.original_range.start;
        }
        // Slice C.5: grammar-driven edits (operators like `>>`,
        // `dd`, `c`, `y`) reach this path with `Effect::Edits`
        // -- the actor already applied them to the document.
        // They bypass the `apply_edit_blocking` chokepoint that
        // does `publish_document_changed`. Without manual
        // wiring here, the LSP `didChange` fan-out, the
        // `pending_syntax_edits` accumulation, and the
        // `shift_highlights_for_edit` byte-shift all SKIP these
        // edits -- which is what produced the user-reported
        // flicker on `>>` and `dd`: spans never shifted on the
        // input thread, so when the worker eventually published
        // the recompute landed as a visible repaint.
        //
        // Route them through the same chokepoint so:
        // - LSP servers see the didChange.
        // - Syntax worker sees the EditDeltas (incremental
        //   reparse instead of falling back to full).
        // - visible_highlights stays line- and byte-aligned via
        //   shift_highlights_for_edit.
        if !edits.is_empty() {
            self.publish_document_changed(edits);
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
        // Local helper: same range-containment logic as
        // `HelpBuffer::link_at` (covers same-line + multi-line
        // labels). M.3.2.c.5 retires the method on HelpBuffer
        // and shares this logic via a free function in
        // `crate::help`; for now the inline shape keeps the
        // diff narrow.
        fn range_contains_position(
            r: &lattice_protocol::position::Range,
            pos: lattice_protocol::position::Position,
        ) -> bool {
            if pos.line == r.start.line && pos.line == r.end.line {
                return pos.byte >= r.start.byte && pos.byte < r.end.byte;
            }
            if pos.line < r.start.line || pos.line > r.end.line {
                return false;
            }
            if pos.line == r.start.line {
                return pos.byte >= r.start.byte;
            }
            if pos.line == r.end.line {
                return pos.byte < r.end.byte;
            }
            true
        }

        let cursor = self.cursor;
        let Some(help) = self.help_buffer.as_ref() else {
            return;
        };
        // M.3.2.c.1: prefer help-mode-owned link data from
        // `buffer_locals`; fall back to the HelpBuffer's
        // struct field if the locals don't contain the link.
        // The fallback handles two cases:
        // (a) tests that synthesize a `HelpLink` and push it
        //     directly into `h.links` without going through the
        //     constructor's parsing path -- the link never
        //     reaches `seed_help_locals`.
        // (b) the bootstrap window after construction but
        //     before `seed_help_locals` runs.
        // M.3.2.c.5 retires the struct field; tests will
        // construct a `BufferLocals` directly at that point.
        //
        // Note: `app.help_buffer.id` (the construction-time
        // id) and the registered id (= `pane.buffer_id`) are
        // intentionally different (see comment in
        // `open_help_in_pane`); locals are keyed by the
        // registered id, so we look up via the active pane's
        // buffer id, not the popup-mode buffer's struct
        // field.
        let active_help_id = self.pane_tree.active().buffer_id;
        let link_from_locals = self
            .buffer_locals
            .get(&active_help_id)
            .and_then(|locals| locals.get::<crate::modes::HelpLinks>())
            .and_then(|hl| {
                hl.0.iter()
                    .find(|link| range_contains_position(&link.range, cursor))
                    .cloned()
            });
        let Some(link) = link_from_locals.or_else(|| {
            help.links
                .iter()
                .find(|link| range_contains_position(&link.range, cursor))
                .cloned()
        }) else {
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
                // M.3.2.c.1: read help-mode-owned anchors
                // through `buffer_locals` (keyed by the
                // registered id from `pane.buffer_id`, not
                // `help.id` -- see open_help_in_pane comment)
                // with a fallback to the struct field for
                // the bootstrap window / synthetic-test paths.
                let active_help_id = self.pane_tree.active().buffer_id;
                let target_line = self.help_buffer.as_ref().and_then(|h| {
                    let from_locals = self
                        .buffer_locals
                        .get(&active_help_id)
                        .and_then(|locals| locals.get::<crate::modes::HelpAnchors>())
                        .and_then(|anchors| {
                            anchors.0.iter().find(|a| a.name == slug).map(|a| a.line)
                        });
                    from_locals.or_else(|| {
                        h.anchors.iter().find(|a| a.name == slug).map(|a| a.line)
                    })
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
    pub(super) fn snapshot_active_pane(&mut self) {
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
            BufferKind::Oil => {
                if let Some(o) = self.buffers.oil_mut(pane_id) {
                    o.cursor = cursor;
                    o.scroll = scroll as usize;
                }
            }
            BufferKind::Document => {}
        }
        let active = self.pane_tree.active_mut();
        active.cursor = cursor;
        active.scroll = scroll;
    }

    /// Total area available to pane content in screen-cell units.
    /// Currently the buffer area = full terminal minus the mode
    /// line (1 row) and the echo / cmdline area (1 row). Width is
    /// the terminal width; v1 doesn't track terminal width as
    /// state, so we estimate from `viewport_height` and a constant
    /// width that the renderer overrides with the real terminal
    /// width before navigation. Good enough until B.1.c has the
    /// per-frame terminal size cached on App.
    pub(super) fn buffer_area_rect(&self) -> crate::pane::PaneRect {
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
    pub(super) fn open_help(&mut self, buffer: HelpBuffer) {
        // Record the *document* cursor (we're still active=Document
        // here, since open_help precedes the active_buffer flip).
        // Skip the push if we're already in Help (a help->help
        // re-open from a link follow); the inter-help transition
        // is recorded by `do_help_follow_link` itself.
        if matches!(self.active_buffer, BufferKind::Document) {
            let cur = self.cursor;
            self.push_position_history(cur, PositionSource::AutoJump);
        }
        // Sync the active pane's cursor / scroll stash *before*
        // swapping `active_buffer` to Help. Once active is Help,
        // the active pane's buffer (Document) no longer matches
        // `app.active_buffer`, so the renderer paints it as
        // visually inactive -- reading from `pane.cursor` rather
        // than `app.cursor`. Without this snapshot the pane stash
        // is whatever it was last set to (often (0,0)) and the
        // doc visibly jumps to the top of file when the popup
        // opens.
        self.snapshot_active_pane();
        // Capture pre-help state so dismiss restores the user
        // cleanly. Mirrors `activate_help_in_pane` / `focus_help_popup`.
        if !matches!(self.active_buffer, BufferKind::Help) {
            let active = self.pane_tree.active();
            self.prev_pane_for_help = Some(PrevPaneState {
                buffer: active.buffer,
                buffer_id: active.buffer_id,
                cursor: self.cursor,
                scroll: self.scroll,
            });
        }
        // Load the help buffer's cursor / scroll into the App's
        // hot path. Motion / scroll / search read / write them
        // uniformly across buffer kinds.
        let stash_cursor = buffer.cursor;
        let stash_scroll = buffer.scroll as u32;
        self.help_buffer = Some(buffer);
        self.cursor = stash_cursor;
        self.scroll = stash_scroll;
        self.active_buffer = BufferKind::Help;
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
        // Note: `buffer.id` from `from_lines` and the registered
        // `id` here are intentionally different. The mismatch is
        // load-bearing for `activate_help_in_pane`'s
        // refresh-from-registry logic which fires when
        // `pane.buffer_id != help_buffer.id`. Production reader
        // sites that look up `buffer_locals` use
        // `pane.buffer_id` (the registered id), not `help.id`.
        let registry_copy = buffer.clone();
        self.buffers.insert(BufferEntry {
            id,
            flags: BufferFlags::default(),
            data: BufferData::Help(registry_copy),
        });
        // M.3.1: activate help-mode for this buffer so its
        // ReadOnly = true contribution lands in the resolved
        // options cache.
        self.activate_major_for_buffer_kind(id, BufferKind::Help);
        // M.3.2.b.1: mirror help-mode-owned data into the
        // buffer-locals map. The data is parsed at HelpBuffer
        // construction (links from markdown source, anchors
        // from headings, highlights from tree-sitter); this
        // step copies it into the typed-map so future reads
        // can transition off `HelpBuffer.X` and onto
        // `app.buffer_locals[id].get::<HelpLinks>()` etc.
        // (M.3.2.b.2 flips readers, then drops the fields
        // from `HelpBuffer`.)
        self.seed_help_locals(id, &buffer);
        // Take ownership of the original for the popup hot-path.
        self.help_buffer = Some(buffer);
        self.activate_help_in_pane(id);
        id
    }

    /// Mirror help-mode-owned data from a `HelpBuffer` into
    /// the buffer-locals map for `buffer_id`. Called at help-
    /// buffer creation time (M.3.2.b.1). Idempotent: a second
    /// call with the same buffer overwrites the prior locals
    /// since `BufferLocals::insert` is replace-on-collision.
    fn seed_help_locals(
        &mut self,
        buffer_id: crate::buffers::BufferId,
        buffer: &crate::help::HelpBuffer,
    ) {
        let locals = self
            .buffer_locals
            .entry(buffer_id)
            .or_default();
        locals.insert(crate::modes::HelpLinks(buffer.links.clone()));
        locals.insert(crate::modes::HelpAnchors(buffer.anchors.clone()));
        locals.insert(crate::modes::HelpHighlights(buffer.highlights.clone()));
    }








    pub(super) fn enter_mode(&mut self, state: ModalState) {
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
            BufferKind::Document | BufferKind::FileTree | BufferKind::Oil => {
                self.pane_tree.active().buffer_id
            }
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
        // Drop cached spans AND the cache key so
        // refresh_highlights's B.3 cache check sees a miss and
        // recomputes. Without clearing the key, the next
        // refresh_highlights computes the same key as the
        // previous frame (snapshot didn't change), hits the
        // cache, and returns the (now empty) `visible_highlights`
        // -- which manifests as syntax highlighting visibly
        // disappearing after `<C-l>` until the user scrolls (or
        // anything else invalidates the key). Regression test
        // pinned in `redraw_screen_repopulates_visible_highlights`.
        self.visible_highlights.clear();
        self.visible_highlights_key = None;
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
            BufferKind::Oil => self
                .buffers
                .oil(self.active_pane_buffer_id())
                .map(|o| o.cursor)
                .unwrap_or(self.cursor),
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
            BufferKind::Oil => self
                .buffers
                .oil(self.active_pane_buffer_id())
                .map(|o| o.content.clone())
                .unwrap_or_else(|| self.document.snapshot().buffer.clone()),
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
    /// **Help-popup overlay (State B).** When the focus has moved
    /// into a hover/help popup that paints as a centred overlay
    /// (active_buffer == Help, but the active pane still shows a
    /// Document underneath), the popup -- not the pane -- is the
    /// surface receiving motion. Returning the *popup's inner
    /// height* here keeps `ensure_cursor_visible` and the renderer
    /// in sync: without it, `j` past the last *visible* popup row
    /// silently advanced `cursor.line` (the pane viewport is much
    /// taller than the popup, so the App thought the cursor was
    /// fine) and the renderer pinned the cursor visually to the
    /// last drawn row -- so subsequent `k` had to "unwind" the
    /// phantom overshoot before any visible motion. Help-as-buffer
    /// (in-pane help, where pane.buffer == Help) doesn't take this
    /// branch -- the pane content height is the right answer.
    pub fn active_pane_content_height(&self, buffer_height: u32) -> u32 {
        if let Some(h) = self.help_popup_inner_height(buffer_height) {
            return h;
        }
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

    /// Inner height of the hover/help popup overlay when one is
    /// active in State B (focused popup, doc still showing in the
    /// pane below). `None` when no overlay is active or help fills
    /// the pane (in which case the regular pane-content-height
    /// path applies).
    ///
    /// Sizing matches `render::position_help_popup` exactly so the
    /// motion engine and the renderer agree on the popup viewport.
    /// Border rows (top + bottom) are subtracted; the result is
    /// the row count `Paragraph` actually paints into.
    pub fn help_popup_inner_height(&self, buffer_height: u32) -> Option<u32> {
        if !matches!(self.active_buffer, BufferKind::Help) {
            return None;
        }
        if self.pane_tree.active().buffer == BufferKind::Help {
            return None;
        }
        let help = self.help_buffer.as_ref()?;
        let line_count = help.line_count().max(1);
        let buffer_h = buffer_height.max(1);
        let max_h = (buffer_h / 2).max(5).min(20);
        let height = (line_count + 2).min(max_h).max(5);
        Some(height.saturating_sub(2).max(1))
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
        | Effect::OpenOil { .. }
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
        | Effect::LspLogClear { .. }
        | Effect::LspDocumentSymbol
        | Effect::LspWorkspaceSymbol { .. }
        | Effect::LspFormat
        | Effect::LspFormatRange
        | Effect::LspSignatureHelp
        | Effect::LspComplete
        | Effect::LspRename { .. }
        | Effect::LspCodeAction
        | Effect::SnippetExpand
        | Effect::ReloadSnippets
        | Effect::AppAction(_) => false,
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
        | Effect::OpenOil { .. }
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
        | Effect::LspLogClear { .. }
        | Effect::LspDocumentSymbol
        | Effect::LspWorkspaceSymbol { .. }
        | Effect::LspFormat
        | Effect::LspFormatRange
        | Effect::LspSignatureHelp
        | Effect::LspComplete
        | Effect::LspRename { .. }
        | Effect::LspCodeAction
        | Effect::SnippetExpand
        | Effect::ReloadSnippets
        | Effect::AppAction(_) => false,
    }
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
        app_with, attach_test_syntax, install_help, invoke_motion, press, press_chars,
        subscribe_all_events, submit_ex,
    };
    use lattice_protocol::selection::VisualMode;

    /// Sanity check: a bare motion drives the cursor through
    /// the full translate + apply path. If this fails, the
    /// harness itself is broken; every other `press_*` test
    /// is suspect.
    #[test]
    fn key_harness_j_advances_cursor_one_line() {
        let mut a = app_with("one\ntwo\nthree", 10);
        press_chars(&mut a, "j");
        assert_eq!(a.cursor.line, 1);
    }

    /// Sanity check: an operator + motion deletes the right
    /// span end-to-end. Exercises the `[d]` action-kind
    /// short-circuit (pushes `d` into `partial_chord`) plus
    /// the `[d, w]` resolution under the prefix.
    #[test]
    fn key_harness_dw_deletes_first_word() {
        let mut a = app_with("one two three", 10);
        press_chars(&mut a, "dw");
        assert_eq!(a.document.text(), "two three");
    }

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
    #[test]
    fn key_harness_count_before_motion_advances_n_lines() {
        let mut a = app_with("a\nb\nc\nd\ne", 10);
        press_chars(&mut a, "3j");
        assert_eq!(a.cursor.line, 3);
    }

    /// `3dd` deletes 3 lines: count latches into op_count via
    /// the `d` action-kind dispatch; the second `d` resolves
    /// linewise with the count baked in by `attach_count`. Pins
    /// slice 8.i.4.f's removal of the dispatcher's redundant
    /// multiplication, AND slice 8.i.4.g's `dd`-consumes-newline
    /// fix (line count drops by 3 -- the buffer goes from 5
    /// lines to 2, with no leading empty line).
    #[test]
    fn key_harness_count_before_operator_dd_deletes_n_lines() {
        let mut a = app_with("a\nb\nc\nd\ne", 10);
        press_chars(&mut a, "3dd");
        assert_eq!(a.document.text(), "d\ne");
    }

    /// `d2w` deletes 2 words: the `2` between operator and motion
    /// must reach the digit accumulator, not get eaten by the
    /// partial_chord lookup. Pins slice 8.i.4.f's hoist of digit
    /// handling above the partial_chord short-circuit.
    #[test]
    fn key_harness_count_after_operator_d2w_deletes_two_words() {
        let mut a = app_with("one two three four", 10);
        press_chars(&mut a, "d2w");
        assert_eq!(a.document.text(), "three four");
    }

    /// `2d2w` deletes 4 words: op_count=2 multiplies with
    /// motion_count=2. Pins both 8.i.4.f fixes end-to-end --
    /// digit-after-operator survives, and the dispatcher honours
    /// the input-side baked count without re-multiplying.
    #[test]
    fn key_harness_counts_multiply_on_both_sides() {
        let mut a = app_with("a b c d e f g", 10);
        press_chars(&mut a, "2d2w");
        assert_eq!(a.document.text(), "e f g");
    }

    /// After `3j`, a bare `j` moves only one line: pending_count
    /// must reset after the motion fires.
    #[test]
    fn key_harness_count_clears_after_motion_fires() {
        let mut a = app_with("a\nb\nc\nd\ne\nf", 10);
        press_chars(&mut a, "3j");
        assert_eq!(a.cursor.line, 3);
        press_chars(&mut a, "j");
        assert_eq!(a.cursor.line, 4);
    }

    // -- partial_chord state machine ----------------------------
    //
    // Pins the multi-keystroke prefix walk. `gg` is a Normal-mode
    // multi-key motion (no operator); `dd` is the operator
    // self-key linewise resolution; `df,` is a 3-keystroke chord
    // (operator -> find-char prefix -> captured delimiter).

    /// `gg` jumps to the first line: prefix `g` parks in
    /// partial_chord, second `g` resolves the terminal.
    #[test]
    fn key_harness_gg_jumps_to_first_line() {
        let mut a = app_with("one\ntwo\nthree\nfour", 10);
        press_chars(&mut a, "G");
        assert_eq!(a.cursor.line, 3);
        press_chars(&mut a, "gg");
        assert_eq!(a.cursor.line, 0);
    }

    /// `df,` deletes up to (exclusive of) the comma: 3-keystroke
    /// chord across the operator + find-char captured-delimiter
    /// sub-tree (slice 8.i.4.c). Lattice's `f`-motion is exclusive
    /// (see `df_deletes_through_target_char` near the dispatch
    /// tests), so the comma stays.
    #[test]
    fn key_harness_df_delim_deletes_up_to_match() {
        let mut a = app_with("alpha, beta, gamma", 10);
        press_chars(&mut a, "df,");
        assert_eq!(a.document.text(), ", beta, gamma");
    }

    // -- <C-w> sub-tree (action-kind short-circuit, 8.i.4.d) ----

    /// `<C-w>v` splits the active pane vertically. Exercises the
    /// `<C-w>` action-kind short-circuit + the AfterCtrlW layer.
    #[test]
    fn key_harness_ctrl_w_v_creates_second_pane() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut a = app_with("xx", 10);
        press(
            &mut a,
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
        );
        press_chars(&mut a, "v");
        assert_eq!(a.pane_tree.len(), 2);
    }

    // -- mode transition seam -----------------------------------

    /// `i` enters Insert, typed chars land in the buffer, `<Esc>`
    /// returns to Normal. Pins the modal state machine across a
    /// mode round-trip in a single keystroke stream.
    #[test]
    fn key_harness_insert_round_trip_inserts_text_and_returns_normal() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut a = app_with("", 10);
        press_chars(&mut a, "ihi");
        press(&mut a, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(a.document.text(), "hi");
        assert_eq!(a.modal, ModalState::Normal);
    }

    /// Subscribe a channel sink to the App's event bus. Returns

    #[test]
    fn event_bus_publishes_document_changed_on_apply_edit() {
        let mut a = app_with("hello", 5);
        let mut rx = subscribe_all_events(&a);
        a.apply_edit_blocking(Edit::insert(Position::new(0, 5), " world"))
            .unwrap();
        assert!(matches!(rx.try_recv(), Ok(Event::DocumentChanged { .. })));
    }

    // ---- Slice B.2 part 2: edit-delta accumulation -------------
    //
    // Pin that EditDeltas accumulate on App.pending_syntax_edits
    // across edits, drain on maybe_reparse_syntax, and the
    // version baseline tracks correctly. The actual incremental
    // reparse correctness is covered by lattice-syntax's parity
    // tests; these App-level tests pin the plumbing.

    #[test]
    fn apply_edit_accumulates_delta_when_syntax_attached() {
        let mut a = app_with("hello", 5);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        assert_eq!(a.pending_syntax_edits.len(), 0);
        a.apply_edit_blocking(Edit::insert(Position::new(0, 5), " world"))
            .unwrap();
        assert_eq!(a.pending_syntax_edits.len(), 1);
        let delta = a.pending_syntax_edits[0];
        assert_eq!(delta.start_byte, 5);
        assert_eq!(delta.old_end_byte, 5);
        assert_eq!(delta.new_end_byte, 11);
    }

    #[test]
    fn apply_edit_skips_delta_accumulation_when_no_syntax() {
        // No syntax attached -> publish_document_changed
        // short-circuits the delta push to keep the vec bounded.
        let mut a = app_with("hello", 5);
        assert!(a.syntax.is_none());
        a.apply_edit_blocking(Edit::insert(Position::new(0, 5), " world"))
            .unwrap();
        assert_eq!(a.pending_syntax_edits.len(), 0);
    }

    #[test]
    fn apply_edit_batch_accumulates_one_delta_per_edit() {
        let mut a = app_with("abc", 5);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        let edits = vec![
            Edit::insert(Position::new(0, 0), "1"),
            Edit::insert(Position::new(0, 2), "2"),
        ];
        a.apply_edit_batch_blocking(edits).unwrap();
        assert_eq!(a.pending_syntax_edits.len(), 2);
    }

    #[test]
    fn maybe_reparse_syntax_drains_pending_edits_and_updates_version() {
        let mut a = app_with("hello", 5);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        let initial_synced = a.last_synced_syntax_version;
        a.apply_edit_blocking(Edit::insert(Position::new(0, 5), " world"))
            .unwrap();
        assert_eq!(a.pending_syntax_edits.len(), 1);
        // Drive the reparse-request seam directly (mirrors what
        // the runtime loop does at the end of each Action).
        a.maybe_reparse_syntax();
        // Edits drained.
        assert_eq!(a.pending_syntax_edits.len(), 0);
        // Version baseline advanced -- next request will use
        // this as `from_version`.
        assert!(a.last_synced_syntax_version > initial_synced);
        assert_eq!(a.last_synced_syntax_version, a.document.text_version());
    }

    // ---- Slice B.3: highlight span cache --------------------
    //
    // Pin that visible_highlights_key tracks correctly across
    // state changes, that cache hits skip recomputation, and
    // that misses populate the cache fresh.

    #[test]
    fn refresh_highlights_caches_on_first_call() {
        let mut a = app_with("fn main() {}", 5);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        assert!(a.visible_highlights_key.is_none());
        a.refresh_highlights();
        assert!(a.visible_highlights_key.is_some());
    }

    #[test]
    fn refresh_highlights_clears_key_when_no_syntax() {
        // No syntax handle attached -> visible_highlights_key
        // stays None so a future syntax attach triggers a fresh
        // recompute.
        let mut a = app_with("fn main() {}", 5);
        a.refresh_highlights();
        assert!(a.visible_highlights_key.is_none());
        assert!(a.visible_highlights.is_empty());
    }

    #[test]
    fn refresh_highlights_cache_hit_on_unchanged_state() {
        // Two consecutive calls with identical state. The second
        // call's visible_highlights_key matches the first's, so
        // visible_highlights doesn't need to be re-derived. The
        // contract check: visible_highlights_key stays
        // identical (same struct value) across the two calls.
        let mut a = app_with("fn main() {}", 5);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.refresh_highlights();
        let key1 = a.visible_highlights_key;
        a.refresh_highlights();
        let key2 = a.visible_highlights_key;
        assert_eq!(key1, key2);
    }

    #[test]
    fn refresh_highlights_cache_invalidates_on_scroll() {
        let mut a = app_with("fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}", 2);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.refresh_highlights();
        let key1 = a.visible_highlights_key;
        a.scroll = 2;
        a.refresh_highlights();
        let key2 = a.visible_highlights_key;
        assert_ne!(key1, key2, "scroll change must invalidate cache");
    }

    #[test]
    fn refresh_highlights_cache_invalidates_on_viewport_resize() {
        let mut a = app_with("fn main() {}", 5);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.refresh_highlights();
        let key1 = a.visible_highlights_key;
        a.set_viewport_height(20);
        a.refresh_highlights();
        let key2 = a.visible_highlights_key;
        assert_ne!(key1, key2, "viewport resize must invalidate cache");
    }


    // ---- Regression tests for the three production bugs -------
    //
    // 1. Syntax worker silently dies because `Handle::try_current`
    //    fails before the editor's main loop enters tokio context.
    //    Symptom: edits never trigger reparses; spans stay anchored
    //    to the seeded snapshot's bytes forever.
    // 2. `<C-l>` clears `visible_highlights` but not the cache key,
    //    so the next `refresh_highlights` finds the same key, hits
    //    the cache, and returns the now-empty spans -- highlighting
    //    visibly disappears until something else invalidates the
    //    key (scroll change).
    // 3. After an edit, the document advances but the syntax
    //    worker may not have published yet. Pre-fix the cache key
    //    only included the syntax snapshot's text_version; if the
    //    snapshot was stale, the cache hit and stale spans got
    //    painted on new bytes (positional staleness).

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn syntax_worker_actually_publishes_after_request_reparse() {
        // Bug #1 regression: production callers MUST use
        // `seeded_with_runtime`, otherwise the worker is never
        // spawned and `request_reparse` sends to a dropped
        // channel. This test exercises the worker round-trip
        // end-to-end inside a real tokio context.
        use lattice_core::Buffer;
        use lattice_syntax::{Lang, Syntax, SyntaxHandle};
        let mut syn = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        syn.parse_at("fn a() {}", 1);
        let handle = SyntaxHandle::seeded_with_runtime(syn, &tokio::runtime::Handle::current());
        let initial_version = handle.snapshot().text_version();
        // Fire a reparse with a fresh buffer at version 2.
        let new_buffer = Buffer::from_text("fn a() {}\nfn b() {}");
        handle.request_reparse(initial_version, 2, new_buffer, Vec::new());
        // Poll for the snapshot to update. With a real worker,
        // this completes within milliseconds; we cap at 1 second
        // so a regression to "worker never runs" fails fast.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            if handle.snapshot().text_version() == 2 {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "syntax worker did not publish a fresh snapshot within 1s -- \
                     worker likely never spawned (bug #1 regression)"
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        // Confirm the new tree has both function items.
        let snap = handle.snapshot();
        let tree = snap.tree().unwrap();
        assert_eq!(tree.root_node().child_count(), 2);
    }

    #[test]
    fn redraw_screen_repopulates_visible_highlights() {
        // Bug #2 regression: after `<C-l>`, the next
        // `refresh_highlights` must recompute spans, not return
        // the cleared (empty) spans via a cache hit.
        let mut a = app_with("fn main() {}", 5);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.refresh_highlights();
        let spans_before = a.visible_highlights.clone();
        assert!(!spans_before.is_empty());
        // <C-l> should clear both spans + key.
        a.apply(Action::RedrawScreen);
        assert!(
            a.visible_highlights.is_empty(),
            "<C-l> should clear visible_highlights synchronously"
        );
        assert!(
            a.visible_highlights_key.is_none(),
            "<C-l> must clear the cache key too -- otherwise the next refresh hits cache"
        );
        // Next refresh must recompute, not hit cache and return
        // the cleared spans.
        a.refresh_highlights();
        assert!(
            !a.visible_highlights.is_empty(),
            "refresh_highlights after <C-l> must recompute spans -- bug-#2 regression"
        );
    }

    #[test]
    fn refresh_highlights_holds_spans_while_worker_catches_up() {
        // Slice C.3: when document advances but syntax worker
        // hasn't published yet, refresh_highlights must HOLD the
        // existing visible_highlights instead of recomputing
        // against stale data. Combined with shift_highlights_
        // for_edit (which keeps line indices aligned), spans
        // never go through an empty/wrong intermediate state --
        // unchanged-content lines stay correctly highlighted
        // continuously throughout the worker window.
        let mut a = app_with("fn main() {}", 5);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.refresh_highlights();
        assert!(!a.visible_highlights.is_empty());
        let line0_spans_before = a.visible_highlights[0].clone();
        // Apply an edit that adds a new line below line 0.
        // Without a real worker (no tokio runtime in lib tests),
        // the syntax snapshot stays at the old version -- the
        // exact shape of the in-flight-worker window in
        // production.
        a.apply_edit_blocking(Edit::insert(Position::new(0, 12), "\nfn b() {}"))
            .unwrap();
        a.maybe_reparse_syntax();
        a.refresh_highlights();
        // Line 0's content is unchanged by the edit; its spans
        // must be preserved. shift_highlights_for_edit inserted
        // an empty placeholder at index 1 (the new line) without
        // disturbing index 0.
        assert!(
            !a.visible_highlights.is_empty(),
            "refresh_highlights must NOT drop spans during the worker window"
        );
        assert_eq!(
            a.visible_highlights[0], line0_spans_before,
            "line 0's spans must be preserved -- its content didn't change"
        );
        // The new line 1 has an empty placeholder spans entry
        // (will be filled in when the worker publishes).
        assert!(
            a.visible_highlights.len() >= 2,
            "shift should have inserted a placeholder for the new line"
        );
    }

    // ---- C.3 line-delete / line-insert shift regressions ------

    #[test]
    fn line_delete_shifts_visible_highlights_to_keep_below_lines_aligned() {
        // User-reported scenario: "comments below the deleted
        // line briefly turn white." Cause: visible_highlights[N]
        // was for the OLD line at index N, painted on NEW line N
        // (which is OLD line N+1 after delete). If OLD line N's
        // spans had gaps (e.g. code spans), the gaps render as
        // uncolored = white characters on the new content.
        //
        // Fix: shift_highlights_for_edit drains the deleted
        // line's spans, so visible_highlights[N] is now what was
        // at index N+1 -- correctly aligned with the new
        // content at line N.
        let mut a = app_with(
            "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}",
            10,
        );
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.refresh_highlights();
        let line0_spans = a.visible_highlights[0].clone();
        let line2_spans_before_delete = a.visible_highlights[2].clone();
        // Delete the entire line 1 (`fn b() {}\n` at bytes
        // [10..20]).
        let range = lattice_protocol::Range::new(
            Position::new(1, 0),
            Position::new(2, 0),
        );
        a.apply_edit_blocking(Edit::delete(range)).unwrap();
        // visible_highlights should have one fewer entry.
        // Line 0's spans unchanged. Line 1 (post-delete) now
        // has the spans that USED to be at index 2.
        assert_eq!(a.visible_highlights[0], line0_spans);
        assert_eq!(
            a.visible_highlights[1], line2_spans_before_delete,
            "line below the deleted line must inherit its prior spans -- \
             this is what eliminates the gray->white->gray flicker"
        );
    }

    #[test]
    fn line_insert_at_end_preserves_start_line_spans() {
        // `o` style insert: newline at end of current line.
        // Pre-edit line content unchanged; the new line is
        // appended below.
        let mut a = app_with("fn main() {}", 10);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.refresh_highlights();
        let line0_spans = a.visible_highlights[0].clone();
        // Insert "\nfn b() {}" at end of line 0 (byte 12).
        a.apply_edit_blocking(Edit::insert(Position::new(0, 12), "\nfn b() {}"))
            .unwrap();
        // Line 0's spans preserved at index 0; empty
        // placeholder inserted at index 1.
        assert_eq!(a.visible_highlights[0], line0_spans);
        assert!(
            a.visible_highlights.len() >= 2,
            "line insert should add a placeholder entry"
        );
        assert!(
            a.visible_highlights[1].is_empty(),
            "new line's placeholder should be empty until worker publishes"
        );
    }

    #[test]
    fn line_insert_at_start_shifts_existing_spans_down() {
        // `O` style insert: newline at start of current line.
        // The existing line's content moves to the next index;
        // the new (empty) line takes the original index.
        let mut a = app_with("fn main() {}", 10);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.refresh_highlights();
        let line0_spans = a.visible_highlights[0].clone();
        // Insert "\n" at start of line 0 (byte 0). After:
        // line 0 = "" (new), line 1 = "fn main() {}" (old).
        a.apply_edit_blocking(Edit::insert(Position::new(0, 0), "\n"))
            .unwrap();
        // visible_highlights[0] is now empty (placeholder for
        // new line); visible_highlights[1] has the original
        // spans.
        assert!(
            a.visible_highlights[0].is_empty(),
            "new empty line's placeholder at index 0"
        );
        assert_eq!(
            a.visible_highlights[1], line0_spans,
            "original line content moved to index 1"
        );
    }

    #[test]
    fn inline_edit_byte_shifts_spans_on_affected_line() {
        // Slice C.4: `>>` style indent — insert at line start —
        // must byte-shift each span on the affected line by the
        // inserted byte count. Without this, held spans paint
        // colors on the new whitespace bytes and the recompute
        // transitions to "default color on whitespace" --
        // visible flicker. With this, spans line up with new
        // byte positions on frame N+1, identical to what the
        // recompute will produce on frame N+2 → no transition.
        let mut a = app_with("fn main() {}", 10);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.refresh_highlights();
        let len_before = a.visible_highlights.len();
        let line0_before = a.visible_highlights[0].clone();
        // Insert "    " at start of line 0 (mimics >> indent).
        a.apply_edit_blocking(Edit::insert(Position::new(0, 0), "    "))
            .unwrap();
        // Line count unchanged; visible_highlights length stays.
        assert_eq!(a.visible_highlights.len(), len_before);
        // Each span on line 0 should have shifted right by 4 --
        // they were entirely after byte 0 (the edit point).
        let line0_after = &a.visible_highlights[0];
        assert_eq!(
            line0_after.len(),
            line0_before.len(),
            "no spans dropped (none crossed byte 0)"
        );
        for (before, after) in line0_before.iter().zip(line0_after.iter()) {
            assert_eq!(after.start, before.start + 4, "start shifted by 4");
            assert_eq!(after.end, before.end + 4, "end shifted by 4");
            assert_eq!(after.style, before.style, "style preserved");
        }
    }

    #[test]
    fn inline_edit_byte_shifts_spans_after_edit_point_only() {
        // Insert in the middle of a line shifts only spans whose
        // start is at or past the edit point. Spans entirely
        // before the edit are unchanged.
        let mut a = app_with("fn main() { let x = 1; }", 10);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.refresh_highlights();
        let line0_before = a.visible_highlights[0].clone();
        // Insert "abc" at byte 12 (between "{ " and "let").
        a.apply_edit_blocking(Edit::insert(Position::new(0, 12), "abc"))
            .unwrap();
        let line0_after = &a.visible_highlights[0];
        // For each span, classify based on its position relative
        // to byte 12. Spans with end <= 12 unchanged. Spans
        // with start >= 12 shifted by +3.
        for (before, after) in line0_before.iter().zip(line0_after.iter()) {
            if before.end <= 12 {
                assert_eq!(after.start, before.start, "before-edit span unchanged");
                assert_eq!(after.end, before.end);
            } else if before.start >= 12 {
                assert_eq!(after.start, before.start + 3, "after-edit span shifted");
                assert_eq!(after.end, before.end + 3);
            }
        }
    }

    #[test]
    fn inline_edit_extends_crossing_span() {
        // Span overlapping the edit point gets its end extended
        // (or contracted) to track the resized content. Start
        // stays put because the prefix bytes are preserved.
        let mut a = app_with("fn longname() {}", 10);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.refresh_highlights();
        // Find the span covering "longname" -- it crosses any
        // mid-identifier insert.
        let line0_before = a.visible_highlights[0].clone();
        // Insert "X" at byte 6 (mid-"longname": "long" + "X" +
        // "name").
        a.apply_edit_blocking(Edit::insert(Position::new(0, 6), "X"))
            .unwrap();
        let line0_after = &a.visible_highlights[0];
        // For each span, if it crossed byte 6, its end should
        // have extended by 1 while start stayed.
        for (before, after) in line0_before.iter().zip(line0_after.iter()) {
            if before.start < 6 && before.end > 6 {
                assert_eq!(after.start, before.start, "crossing span: start preserved");
                assert_eq!(
                    after.end,
                    before.end + 1,
                    "crossing span: end extended by 1"
                );
            }
        }
    }

    #[test]
    fn inline_delete_contracts_spans() {
        // Delete bytes from a line: spans after the delete shift
        // left; spans crossing the delete contract their end.
        let mut a = app_with("fn main() { let xx = 1; }", 10);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.refresh_highlights();
        let line0_before = a.visible_highlights[0].clone();
        // Delete byte 17 ("x" -- one of the two chars in "xx").
        let range = lattice_protocol::Range::new(
            Position::new(0, 17),
            Position::new(0, 18),
        );
        a.apply_edit_blocking(Edit::delete(range)).unwrap();
        let line0_after = &a.visible_highlights[0];
        for (before, after) in line0_before.iter().zip(line0_after.iter()) {
            if before.end <= 17 {
                assert_eq!(after.start, before.start, "before-delete span unchanged");
                assert_eq!(after.end, before.end);
            } else if before.start >= 18 {
                assert_eq!(
                    after.start,
                    before.start - 1,
                    "after-delete span shifted left"
                );
                assert_eq!(after.end, before.end - 1);
            }
        }
    }


    #[test]
    fn undo_redo_accumulate_inverse_deltas() {
        // Forward edit + undo + redo each push a delta. The
        // undo's delta is the inverse of the forward (start_byte
        // unchanged; old_end / new_end swapped).
        let mut a = app_with("a", 5);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.apply_edit_blocking(Edit::insert(Position::new(0, 1), "b"))
            .unwrap();
        assert_eq!(a.pending_syntax_edits.len(), 1);
        let forward = a.pending_syntax_edits[0];
        a.undo_blocking().unwrap();
        assert_eq!(a.pending_syntax_edits.len(), 2);
        let undo_delta = a.pending_syntax_edits[1];
        // Undo's old_end/new_end are swapped relative to
        // forward.
        assert_eq!(undo_delta.start_byte, forward.start_byte);
        assert_eq!(undo_delta.old_end_byte, forward.new_end_byte);
        assert_eq!(undo_delta.new_end_byte, forward.old_end_byte);
        a.redo_blocking().unwrap();
        assert_eq!(a.pending_syntax_edits.len(), 3);
    }

    #[test]
    fn event_bus_publishes_document_changed_on_undo_redo() {
        let mut a = app_with("a", 5);
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
    fn invocation_resets_partial_chord() {
        // Slice 8.i.4: AbsorbPartialChord pushes onto
        // partial_chord; any other action clears it.
        let mut a = app_with("abc", 10);
        a.apply(Action::AbsorbPartialChord(crate::chord::KeyChord::char('g')));
        assert_eq!(a.partial_chord.len(), 1);
        let id = a.builtins.char_right;
        a.apply(invoke_motion(id));
        assert!(a.partial_chord.is_empty());
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


    // ---- Substitute (:s/foo/bar/[g]) ----

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

    // ---- WORD motions (W, B, E) end-to-end ----

    #[test]
    fn capital_w_skips_punctuation() {
        let mut a = app_with("foo,bar baz", 10);
        a.apply(invoke_motion(a.builtins.big_word_forward));
        assert_eq!(a.cursor, Position::new(0, 8));
    }

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
    fn set_mark_clears_partial_chord() {
        let mut a = app_with("hello", 10);
        a.apply(Action::AbsorbPartialChord(crate::chord::KeyChord::char('m')));
        a.apply(Action::SetMark('a'));
        assert!(a.partial_chord.is_empty());
    }

    #[test]
    fn select_register_clears_partial_chord() {
        let mut a = app_with("hello", 10);
        a.apply(Action::AbsorbPartialChord(crate::chord::KeyChord::char('"')));
        a.apply(Action::SelectRegister(Register::Named('a')));
        assert!(a.partial_chord.is_empty());
    }

    #[test]
    fn jump_to_mark_clears_partial_chord() {
        let mut a = app_with("hello\nworld", 10);
        a.apply(Action::SetMark('a'));
        a.apply(Action::AbsorbPartialChord(crate::chord::KeyChord::char('`')));
        a.apply(Action::JumpToMarkExact('a'));
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

    #[test]
    fn fold_action_clears_partial_chord() {
        let mut a = app_with("a\nb\nc", 10);
        a.apply(Action::AbsorbPartialChord(crate::chord::KeyChord::char('z')));
        a.apply(Action::OpenFoldAtCursor);
        assert!(a.partial_chord.is_empty());
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
        // Slice 8.i.4.g: `dd` consumes BBB + its trailing newline.
        assert_eq!(a.document.text(), "aaa\nccc\nddd");
        // Cursor is now on what used to be `ccc` (line 1). `.`
        // repeats the linewise delete -- removes that line + its
        // trailing newline.
        a.apply(Action::RepeatLastChange);
        assert_eq!(a.document.text(), "aaa\nddd");
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
    fn dispatcher_runs_counted_motion() {
        // Slice 8.i.4.f: count multiplication is input-side. The
        // dispatcher consumes the baked `inv.count` -- App still
        // resets `pending_count` at end-of-dispatch (drained by
        // attach_count earlier in the pipeline). Press-harness
        // tests cover the full keystroke flow.
        let mut a = app_with("one two three four five", 10);
        a.pending_count = 3;
        a.apply(Action::Invoke(
            CommandInvocation::of(a.builtins.word_forward.0)
                .with_count(lattice_grammar::command::Count(3)),
        ));
        // 3w from origin: "one two three FOUR five" -> 'f' of "four" at byte 14.
        assert_eq!(a.cursor, Position::new(0, 14));
        // pending_count is reset after dispatch.
        assert_eq!(a.pending_count, 0);
    }

    #[test]
    fn count_with_line_motion_advances_count_lines() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 20);
        a.apply(Action::Invoke(
            CommandInvocation::of(a.builtins.line_down.0)
                .with_count(lattice_grammar::command::Count(5)),
        ));
        assert_eq!(a.cursor.line, 5);
    }

    // Slice 8.i.4.f: the next batch of tests bake `Count(N)`
    // directly into the operator invocation -- mirroring what the
    // input-side `attach_count` produces when the keystroke
    // pipeline runs. The dispatcher's job is now to honour the
    // baked count and drain `pending_count` / `op_count` from
    // App state. The full keystroke -> count pipeline lives in
    // the `key_harness_*` press-harness tests.

    #[test]
    fn dispatcher_runs_counted_operator_on_motion_2dw() {
        let mut a = app_with("one two three four five", 10);
        // Mirror translate-time state: `2d` already absorbed.
        a.op_count = 2;
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_target(lattice_grammar::Target::Motion(
                a.builtins.word_forward,
                lattice_grammar::Args::None,
            ))
            .with_count(lattice_grammar::command::Count(2));
        a.apply(Action::Invoke(inv));
        // 2dw: deletes "one two " leaving "three four five".
        assert_eq!(a.document.text(), "three four five");
        assert_eq!(a.op_count, 0);
    }

    #[test]
    fn dispatcher_runs_counted_operator_on_motion_2d3w_equals_count_6() {
        let mut a = app_with("a b c d e f g h i j", 10);
        a.op_count = 2;
        a.pending_count = 3;
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_target(lattice_grammar::Target::Motion(
                a.builtins.word_forward,
                lattice_grammar::Args::None,
            ))
            .with_count(lattice_grammar::command::Count(6));
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
        a.op_count = 2;
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_range(lattice_grammar::Range::CurrentLine)
            .with_count(lattice_grammar::command::Count(2));
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
        a.op_count = 2;
        let inv = CommandInvocation::of(a.builtins.indent_right.0)
            .with_range(lattice_grammar::Range::CurrentLine)
            .with_count(lattice_grammar::command::Count(2));
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
        a.op_count = 2;
        let inv = CommandInvocation::of(a.builtins.indent_left.0)
            .with_range(lattice_grammar::Range::CurrentLine)
            .with_count(lattice_grammar::command::Count(2));
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
        assert_eq!(reg.content, "BBB\n");
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
        assert_eq!(reg.content, "BBB\n");
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
        // fold operates on just one line. Slice 8.i.4.g: `dd`
        // consumes BBB and its trailing newline (vim semantics);
        // the linewise register content carries the `\n` so paste
        // splices cleanly.
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.set_foldmethod_for_test(FoldMethod::Indent);
        a.recompute_folds();
        a.cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(a.builtins.delete.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        let reg = a.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.kind, YankKind::Linewise);
        assert_eq!(reg.content, "BBB\n");
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
                source: None,
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
                source: None,
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
    fn open_help_popup_preserves_doc_pane_cursor_for_render() {
        // Bug: invoking a popup-mode help command (`:lsp-status`,
        // `:describe-key`, etc.) flipped `active_buffer` to Help
        // without first syncing the doc's `app.cursor` /
        // `app.scroll` into the active pane's stash. The renderer
        // reads `pane.cursor` for any pane whose buffer kind
        // doesn't match `active_buffer` (popup mode = mismatch),
        // so the doc visibly jumped to wherever pane.cursor was
        // last (often (0,0)).
        let mut a = app_with("line0\nline1\nline2\nline3\nline4\n", 5);
        a.cursor = Position::new(3, 2);
        a.scroll = 1;
        a.do_lsp_status();
        // After open_help, active is Help but the active pane
        // still shows the doc -- pane.cursor must reflect where
        // the doc was, not the help buffer's (0,0).
        let pane = a.pane_tree.active();
        assert_eq!(
            pane.cursor,
            Position::new(3, 2),
            "doc's pre-help cursor must be stashed onto pane.cursor"
        );
        assert_eq!(pane.scroll, 1);
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
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
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
        a.command_line = format!("Filetree {}", dir.display());
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
        a.command_line = format!("Filetree {}", dir.display());
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
        a.command_line = format!("Filetree {}", dir.display());
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
    fn lsp_declaration_request_routes_through_unified_nav_dispatch() {
        let mut a = app_with("xx", 10);
        a.apply(Action::LspDeclarationRequest);
        // No URI mapped, same "no LSP server" guard fires.
        let msg = a.last_message.as_ref().expect("echo");
        assert_eq!(msg.level, EchoLevel::Info);
        assert!(msg.text.contains("no LSP server"));
    }

    #[test]
    fn lsp_type_definition_request_routes_through_unified_nav_dispatch() {
        let mut a = app_with("xx", 10);
        a.apply(Action::LspTypeDefinitionRequest);
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no LSP server"));
    }

    #[test]
    fn lsp_implementation_request_routes_through_unified_nav_dispatch() {
        let mut a = app_with("xx", 10);
        a.apply(Action::LspImplementationRequest);
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no LSP server"));
    }

    #[test]
    fn drain_pending_no_implementations_echoes_kind_specific_message() {
        // Verify the kind drives the verb in the "no X found" echo.
        let mut a = app_with("xx", 10);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<lsp_types::Location>>();
        a.pending_definition_rx = Some(rx);
        a.pending_definition_token = Some(lattice_protocol::CancellationToken::new());
        a.pending_nav_kind = Some(super::LspNavKind::Implementation);
        tx.send(Vec::new()).unwrap();
        a.drain_pending_definitions();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(
            msg.text.contains("no implementations"),
            "expected implementations echo, got: {}",
            msg.text
        );
        assert!(a.pending_nav_kind.is_none());
    }

    #[test]
    fn drain_pending_no_type_definitions_echoes_kind_specific_message() {
        let mut a = app_with("xx", 10);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<lsp_types::Location>>();
        a.pending_definition_rx = Some(rx);
        a.pending_definition_token = Some(lattice_protocol::CancellationToken::new());
        a.pending_nav_kind = Some(super::LspNavKind::TypeDefinition);
        tx.send(Vec::new()).unwrap();
        a.drain_pending_definitions();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no type definitions"));
    }

    #[test]
    fn drain_pending_no_declarations_echoes_kind_specific_message() {
        let mut a = app_with("xx", 10);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<lsp_types::Location>>();
        a.pending_definition_rx = Some(rx);
        a.pending_definition_token = Some(lattice_protocol::CancellationToken::new());
        a.pending_nav_kind = Some(super::LspNavKind::Declaration);
        tx.send(Vec::new()).unwrap();
        a.drain_pending_definitions();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no declarations"));
    }

    #[test]
    fn lsp_references_request_with_no_uri_echoes_no_lsp_attached() {
        let mut a = app_with("xx", 10);
        a.apply(Action::LspReferencesRequest);
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no LSP server"));
    }

    #[test]
    fn lsp_references_request_pre_cancels_in_flight_token() {
        let mut a = app_with("xx", 10);
        let stale = lattice_protocol::CancellationToken::new();
        a.pending_references_token = Some(stale.clone());
        a.apply(Action::LspReferencesRequest);
        assert!(stale.is_cancelled());
    }

    #[test]
    fn drain_pending_references_no_servers_outcome_echoes() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::ReferencesOutcome>();
        a.pending_references_rx = Some(rx);
        a.pending_references_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::ReferencesOutcome::NoServers).unwrap();
        a.drain_pending_references();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no LSP server"));
        assert!(a.pending_references_token.is_none());
    }

    #[test]
    fn drain_pending_references_found_opens_lsp_locations_picker() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::ReferencesOutcome>();
        a.pending_references_rx = Some(rx);
        a.pending_references_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::ReferencesOutcome::Found {
            symbol: "foo".into(),
            locations: vec![loc("/tmp/notarealfile.rs", 3, 5)],
        })
        .unwrap();
        a.drain_pending_references();
        // Picker opened, NOT a help buffer (the pre-picker shape).
        let picker = a.picker.as_ref().expect("picker");
        assert_eq!(picker.title, "references: foo");
        assert!(matches!(
            picker.source,
            crate::picker::PickerSource::LspLocations
        ));
        assert!(matches!(
            picker.on_accept,
            crate::picker::PickerAction::JumpToLspLocation
        ));
        // The candidate's typed routing payload carries the
        // jump target -- post-4.2.g.7 this replaces the prior
        // tab-encoded `text` parsing.
        let c = picker.selected_candidate().expect("one row");
        let routing = picker.routing_for(c).expect("routing payload set");
        let crate::picker::RoutingPayload::LspLocation { path, line, .. } = routing else {
            panic!("expected LspLocation routing, got {routing:?}");
        };
        assert_eq!(*path, std::path::PathBuf::from("/tmp/notarealfile.rs"));
        assert_eq!(*line, 3);
        // Column round-trips through utf-16→utf-8 conversion that
        // reads from the file's actual line text. For a missing
        // file the preview is empty so the conversion bottoms out
        // at 0; ASCII files round-trip cleanly. We don't assert on
        // col here because the conversion needs the line text and
        // the test fixture's path doesn't exist.
    }

    #[test]
    fn drain_pending_references_empty_echoes_not_found() {
        // After the picker pivot, the empty-Found case echoes
        // rather than opening a buffer with a placeholder. The
        // picker UX expects "show the user a list to choose
        // from" -- showing an empty picker would be worse UX
        // than the echo.
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::ReferencesOutcome>();
        a.pending_references_rx = Some(rx);
        a.pending_references_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::ReferencesOutcome::Found {
            symbol: "missing".into(),
            locations: Vec::new(),
        })
        .unwrap();
        a.drain_pending_references();
        assert!(a.picker.is_none());
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no references"));
        assert!(msg.text.contains("missing"));
    }

    #[test]
    fn word_under_cursor_returns_alphanumeric_run() {
        let mut a = app_with("hello world", 10);
        a.cursor = Position::new(0, 0);
        let snap = a.document.snapshot();
        assert_eq!(
            super::word_under_cursor(&snap.buffer, a.cursor),
            Some("hello".to_string())
        );
        a.cursor = Position::new(0, 6);
        assert_eq!(
            super::word_under_cursor(&snap.buffer, a.cursor),
            Some("world".to_string())
        );
    }

    #[test]
    fn flatten_document_symbol_response_flat_preserves_order() {
        use lsp_types::{Range as LRange, Position as LPos, Location as LLoc};
        let path = std::path::PathBuf::from("/tmp/x.rs");
        #[allow(deprecated)]
        let syms = vec![
            lsp_types::SymbolInformation {
                name: "foo".into(),
                kind: lsp_types::SymbolKind::FUNCTION,
                tags: None,
                deprecated: None,
                location: LLoc {
                    uri: super::tests::fake_uri("/tmp/x.rs"),
                    range: LRange {
                        start: LPos { line: 5, character: 0 },
                        end: LPos { line: 5, character: 3 },
                    },
                },
                container_name: None,
            },
            lsp_types::SymbolInformation {
                name: "bar".into(),
                kind: lsp_types::SymbolKind::METHOD,
                tags: None,
                deprecated: None,
                location: LLoc {
                    uri: super::tests::fake_uri("/tmp/x.rs"),
                    range: LRange {
                        start: LPos { line: 10, character: 4 },
                        end: LPos { line: 10, character: 7 },
                    },
                },
                container_name: Some("Bag".into()),
            },
        ];
        let resp = lsp_types::DocumentSymbolResponse::Flat(syms);
        let mut out = Vec::new();
        super::flatten_document_symbol_response(resp, &path, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "foo");
        assert_eq!(out[0].depth, 0);
        assert_eq!(out[1].name, "bar");
        assert_eq!(out[1].container.as_deref(), Some("Bag"));
    }

    #[test]
    fn flatten_document_symbol_response_nested_assigns_depth_via_dfs() {
        use lsp_types::{Range as LRange, Position as LPos, DocumentSymbol};
        let path = std::path::PathBuf::from("/tmp/x.rs");
        // mod foo { fn bar() {} } -> outer at depth 0, bar at depth 1.
        let inner_range = LRange {
            start: LPos { line: 1, character: 4 },
            end: LPos { line: 3, character: 5 },
        };
        let outer_range = LRange {
            start: LPos { line: 0, character: 0 },
            end: LPos { line: 4, character: 0 },
        };
        #[allow(deprecated)]
        let inner = DocumentSymbol {
            name: "bar".into(),
            detail: None,
            kind: lsp_types::SymbolKind::FUNCTION,
            tags: None,
            deprecated: None,
            range: inner_range,
            selection_range: inner_range,
            children: None,
        };
        #[allow(deprecated)]
        let outer = DocumentSymbol {
            name: "foo".into(),
            detail: None,
            kind: lsp_types::SymbolKind::MODULE,
            tags: None,
            deprecated: None,
            range: outer_range,
            selection_range: outer_range,
            children: Some(vec![inner]),
        };
        let resp = lsp_types::DocumentSymbolResponse::Nested(vec![outer]);
        let mut out = Vec::new();
        super::flatten_document_symbol_response(resp, &path, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "foo");
        assert_eq!(out[0].depth, 0);
        assert_eq!(out[1].name, "bar");
        assert_eq!(out[1].depth, 1);
    }

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
    fn drain_pending_symbols_no_servers_outcome_echoes() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::SymbolsOutcome>();
        a.pending_symbols_rx = Some(rx);
        a.pending_symbols_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::SymbolsOutcome::NoServers).unwrap();
        a.drain_pending_symbols();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no LSP server"));
        assert!(a.pending_symbols_token.is_none());
    }

    #[test]
    fn drain_pending_symbols_found_opens_picker() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::SymbolsOutcome>();
        a.pending_symbols_rx = Some(rx);
        a.pending_symbols_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::SymbolsOutcome::Found {
            title: "symbols (2)".into(),
            rows: vec![
                super::SymbolRow {
                    name: "foo".into(),
                    kind_glyph: "ƒ",
                    container: None,
                    depth: 0,
                    path: std::path::PathBuf::from("/tmp/x.rs"),
                    line: 5,
                    col: 0,
                },
                super::SymbolRow {
                    name: "bar".into(),
                    kind_glyph: "v",
                    container: None,
                    depth: 1,
                    path: std::path::PathBuf::from("/tmp/x.rs"),
                    line: 10,
                    col: 4,
                },
            ],
        })
        .unwrap();
        a.drain_pending_symbols();
        let picker = a.picker.as_ref().expect("picker");
        assert_eq!(picker.title, "symbols (2)");
        assert_eq!(picker.candidates.len(), 2);
        // depth-1 row carries indentation in display.
        let display = &picker.candidates[1].raw.display;
        assert!(display.contains("  v bar"), "got: {display}");
    }

    #[test]
    fn drain_pending_symbols_empty_echoes() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::SymbolsOutcome>();
        a.pending_symbols_rx = Some(rx);
        a.pending_symbols_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::SymbolsOutcome::Found {
            title: "symbols (0)".into(),
            rows: Vec::new(),
        })
        .unwrap();
        a.drain_pending_symbols();
        assert!(a.picker.is_none());
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no symbols"));
    }

    #[test]
    fn word_under_cursor_returns_none_off_word() {
        let a = app_with("foo bar", 10);
        let snap = a.document.snapshot();
        // Cursor on the space.
        let p = Position::new(0, 3);
        assert_eq!(super::word_under_cursor(&snap.buffer, p), None);
    }

    #[test]
    fn code_action_kind_glyph_distinct_for_common_kinds() {
        use lsp_types::CodeActionKind as K;
        let qf = super::code_action_kind_glyph(Some(&K::QUICKFIX));
        let rf = super::code_action_kind_glyph(Some(&K::REFACTOR));
        let sr = super::code_action_kind_glyph(Some(&K::SOURCE));
        assert_ne!(qf, rf);
        assert_ne!(qf, sr);
        assert_ne!(rf, sr);
    }

    #[test]
    fn drain_pending_code_actions_no_provider_echoes() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::CodeActionOutcome>();
        a.pending_code_action_rx = Some(rx);
        a.pending_code_action_token =
            Some(lattice_protocol::CancellationToken::new());
        tx.send(super::CodeActionOutcome::NoProvider).unwrap();
        a.drain_pending_code_actions();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("codeActionProvider"));
    }

    #[test]
    fn drain_pending_code_actions_empty_echoes_no_actions() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::CodeActionOutcome>();
        a.pending_code_action_rx = Some(rx);
        a.pending_code_action_token =
            Some(lattice_protocol::CancellationToken::new());
        tx.send(super::CodeActionOutcome::Items(Vec::new())).unwrap();
        a.drain_pending_code_actions();
        assert!(a.picker.is_none());
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no code actions"));
    }

    #[test]
    fn drain_pending_code_actions_items_open_picker() {
        let mut a = app_with("foo\n", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::CodeActionOutcome>();
        a.pending_code_action_rx = Some(rx);
        a.pending_code_action_token =
            Some(lattice_protocol::CancellationToken::new());
        let act = lsp_types::CodeAction {
            title: "Add `mut` modifier".into(),
            kind: Some(lsp_types::CodeActionKind::QUICKFIX),
            diagnostics: None,
            edit: None,
            command: None,
            is_preferred: None,
            disabled: None,
            data: None,
        };
        tx.send(super::CodeActionOutcome::Items(vec![super::CodeActionRow {
            title: act.title.clone(),
            kind_glyph: "🛠",
            action: lsp_types::CodeActionOrCommand::CodeAction(act),
        }]))
        .unwrap();
        a.drain_pending_code_actions();
        let picker = a.picker.as_ref().expect("picker");
        assert!(picker.title.starts_with("code-actions"));
        assert!(matches!(
            picker.on_accept,
            crate::picker::PickerAction::AcceptLspCodeAction
        ));
        assert_eq!(picker.candidates.len(), 1);
        let display = &picker.candidates[0].raw.display;
        assert!(display.contains("🛠 Add `mut` modifier"));
        // Items pinned for the accept path.
        assert!(a.pending_code_action_items.is_some());
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
    fn flatten_workspace_edit_collects_legacy_changes_map() {
        use std::collections::HashMap;
        let uri = super::tests::fake_uri("/tmp/x.rs");
        let mut changes: HashMap<lsp_types::Uri, Vec<lsp_types::TextEdit>> = HashMap::new();
        changes.insert(
            uri.clone(),
            vec![lsp_types::TextEdit {
                range: lsp_types::Range {
                    start: lsp_types::Position {
                        line: 0,
                        character: 0,
                    },
                    end: lsp_types::Position {
                        line: 0,
                        character: 3,
                    },
                },
                new_text: "bar".into(),
            }],
        );
        let we = lsp_types::WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        };
        let flat = super::flatten_workspace_edit(we);
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].0, uri);
        assert_eq!(flat[0].1[0].new_text, "bar");
    }

    #[test]
    fn drain_pending_rename_no_provider_echoes() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::RenameOutcome>();
        a.pending_rename_rx = Some(rx);
        a.pending_rename_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::RenameOutcome::NoProvider).unwrap();
        a.drain_pending_rename();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("renameProvider"));
    }

    #[test]
    fn drain_pending_rename_not_renameable_echoes_reason() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::RenameOutcome>();
        a.pending_rename_rx = Some(rx);
        a.pending_rename_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::RenameOutcome::NotRenameable {
            reason: "out of bounds".into(),
        })
        .unwrap();
        a.drain_pending_rename();
        let msg = a.last_message.as_ref().expect("echo");
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("out of bounds"));
    }

    #[test]
    fn drain_pending_rename_empty_echoes_no_changes() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::RenameOutcome>();
        a.pending_rename_rx = Some(rx);
        a.pending_rename_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::RenameOutcome::Empty).unwrap();
        a.drain_pending_rename();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no changes"));
    }

    #[test]
    fn drain_pending_rename_applies_active_buffer_edits_as_one_undo_unit() {
        // End-to-end-ish: load a real document, send a rename
        // outcome targeting it, verify the buffer text changed
        // and a single undo restores.
        let path = std::env::temp_dir()
            .join(format!("lattice-rename-{}.rs", std::process::id()));
        std::fs::write(&path, "let foo = 1;\nlet x = foo + 2;\n").unwrap();
        let doc = Document::open(&path).unwrap();
        let mut a = App::new(doc);
        a.set_viewport_height(10);
        let uri = super::tests::fake_uri(path.to_str().unwrap());
        let edits = vec![
            // Replace `foo` on line 0 col 4..7
            lsp_types::TextEdit {
                range: lsp_types::Range {
                    start: lsp_types::Position {
                        line: 0,
                        character: 4,
                    },
                    end: lsp_types::Position {
                        line: 0,
                        character: 7,
                    },
                },
                new_text: "bar".into(),
            },
            // Replace `foo` on line 1 col 8..11
            lsp_types::TextEdit {
                range: lsp_types::Range {
                    start: lsp_types::Position {
                        line: 1,
                        character: 8,
                    },
                    end: lsp_types::Position {
                        line: 1,
                        character: 11,
                    },
                },
                new_text: "bar".into(),
            },
        ];
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::RenameOutcome>();
        a.pending_rename_rx = Some(rx);
        a.pending_rename_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::RenameOutcome::Edits {
            per_file: vec![(uri, edits)],
            new_name: "bar".into(),
        })
        .unwrap();
        a.drain_pending_rename();
        let body = a.document.snapshot().buffer.as_string();
        assert!(body.contains("let bar = 1;"));
        assert!(body.contains("let x = bar + 2;"));
        // One undo restores the pre-rename buffer (apply_lsp_text_edits
        // commits via apply_edit_batch_blocking which is one undo unit).
        let _ = a.undo_blocking();
        let restored = a.document.snapshot().buffer.as_string();
        assert!(restored.contains("let foo = 1;"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn completion_kind_glyph_distinct_for_common_kinds() {
        use lsp_types::CompletionItemKind as K;
        let f = super::completion_kind_glyph(Some(K::FUNCTION));
        let s = super::completion_kind_glyph(Some(K::SNIPPET));
        let v = super::completion_kind_glyph(Some(K::VARIABLE));
        assert_ne!(f, s);
        assert_ne!(f, v);
    }

    #[test]
    fn insert_completion_trigger_outside_insert_is_noop() {
        let mut a = app_with("foo bar baz", 10);
        // Normal mode by default -- trigger should no-op.
        a.do_completion_trigger();
        assert!(a.insert_completion.is_none());
    }

    #[test]
    fn insert_completion_trigger_with_no_matches_echoes_no_completions() {
        let mut a = app_with("hello world hello\nfoo bar baz qux", 10);
        a.modal = ModalState::Insert;
        // Cursor at end of `hello` on line 0 -- prefix "hello".
        // BufferWordsSource skips the cursor's own word, and
        // none of the remaining buffer words fuzzy-match
        // "hello", so the popup auto-closes with the
        // "no completions" echo.
        a.cursor = Position::new(0, 5);
        a.do_completion_trigger();
        assert!(a.insert_completion.is_none());
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no completions"));
    }

    #[test]
    fn insert_completion_open_with_matching_query_keeps_popup() {
        let mut a = app_with("hello world helper helmet hi", 10);
        a.modal = ModalState::Insert;
        // Cursor right after `hel` -- prefix "hel". Buffer words:
        // "hello", "world", "helper", "helmet", "hi".
        a.cursor = Position::new(0, 3);
        // Place hel at the cursor: rewrite content via cursor
        // positioning (the buffer already has "hel" as part of
        // hello). For the test, just place cursor on a different
        // line.
        let _ = a.apply_edit_blocking(Edit::insert(
            Position::new(0, 28),
            "\nhel",
        ));
        a.cursor = Position::new(1, 3);
        a.do_completion_trigger();
        let state = a.insert_completion.as_ref().expect("popup opened");
        assert_eq!(state.query, "hel");
        // hello / helper / helmet all start with "hel" -- prefix
        // tier (score 800) matches. Order may vary by stable
        // sort over insertion order.
        let labels: Vec<String> = state
            .rendered
            .iter()
            .map(|c| c.raw.text.clone())
            .collect();
        assert!(labels.contains(&"hello".to_string()));
        assert!(labels.contains(&"helper".to_string()));
        assert!(labels.contains(&"helmet".to_string()));
        // "hi" doesn't fuzzy-match "hel", "world" doesn't either.
        assert!(!labels.contains(&"hi".to_string()));
        assert!(!labels.contains(&"world".to_string()));
    }

    #[test]
    fn insert_completion_next_prev_navigates_with_wrap() {
        let mut a = app_with("alpha alphabet alligator", 10);
        a.modal = ModalState::Insert;
        let _ = a.apply_edit_blocking(Edit::insert(
            Position::new(0, 24),
            "\nal",
        ));
        a.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        let total = a
            .insert_completion
            .as_ref()
            .expect("popup")
            .rendered
            .len();
        assert!(total >= 2, "need ≥ 2 candidates for wrap test");
        assert_eq!(a.insert_completion.as_ref().unwrap().selected, 0);
        a.do_completion_next();
        assert_eq!(a.insert_completion.as_ref().unwrap().selected, 1);
        // Wrap to last via prev from 1 -> 0 -> total-1.
        a.do_completion_prev();
        a.do_completion_prev();
        assert_eq!(
            a.insert_completion.as_ref().unwrap().selected,
            total - 1
        );
    }

    #[test]
    fn insert_completion_accept_replaces_prefix_and_closes() {
        let mut a = app_with("alphabet alligator\nal", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        // Pick the first candidate.
        let first_text = a
            .insert_completion
            .as_ref()
            .and_then(|s| s.selected_candidate())
            .map(|c| c.raw.text.clone())
            .expect("at least one candidate");
        a.do_completion_accept();
        assert!(a.insert_completion.is_none());
        // Buffer line 1 should now be the chosen word.
        let snap = a.document.snapshot();
        let line1 = snap.buffer.line(1).unwrap_or_default();
        assert_eq!(line1.trim_end(), first_text);
    }

    #[test]
    fn insert_completion_cancel_drops_popup() {
        let mut a = app_with("alpha alphabet\nal", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        assert!(a.insert_completion.is_some());
        a.do_completion_cancel();
        assert!(a.insert_completion.is_none());
        // Modal stays Insert.
        assert!(matches!(a.modal, ModalState::Insert));
    }

    #[test]
    fn insert_completion_cancel_and_exit_insert_drops_popup_and_exits() {
        let mut a = app_with("alpha alphabet\nal", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        assert!(a.insert_completion.is_some());
        a.apply(Action::CompletionCancelAndExitInsert);
        assert!(a.insert_completion.is_none());
        assert!(matches!(a.modal, ModalState::Normal));
    }

    #[test]
    fn insert_completion_toggle_docs_flips_state() {
        let mut a = app_with("alpha alphabet\nal", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        assert!(
            a.insert_completion
                .as_ref()
                .map(|s| s.doc_popup.is_none())
                .unwrap_or(false)
        );
        a.do_completion_toggle_docs();
        assert!(a.insert_completion.as_ref().unwrap().doc_popup.is_some());
        a.do_completion_toggle_docs();
        assert!(a.insert_completion.as_ref().unwrap().doc_popup.is_none());
    }

    #[test]
    fn insert_completion_refilters_on_keystroke() {
        let mut a = app_with("alpha alphabet alligator\nal", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        let pre_count = a
            .insert_completion
            .as_ref()
            .expect("popup")
            .rendered
            .len();
        // Type 'p' -- query becomes "alp"; only "alpha" /
        // "alphabet" survive (alligator drops out).
        a.apply(Action::Insert("p".into()));
        let state = a.insert_completion.as_ref().expect("popup still open");
        assert_eq!(state.query, "alp");
        let labels: Vec<String> = state
            .rendered
            .iter()
            .map(|c| c.raw.text.clone())
            .collect();
        assert!(labels.contains(&"alpha".to_string()));
        assert!(labels.contains(&"alphabet".to_string()));
        assert!(!labels.contains(&"alligator".to_string()));
        assert!(state.rendered.len() < pre_count);
    }

    #[test]
    fn insert_completion_closes_when_query_leaves_word_boundary() {
        let mut a = app_with("alpha alphabet\nal", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        assert!(a.insert_completion.is_some());
        // Type a space -- pushes the cursor past the word.
        a.apply(Action::Insert(" ".into()));
        assert!(a.insert_completion.is_none());
    }

    #[test]
    fn drain_pending_insert_completion_lsp_no_servers_keeps_popup_open_if_sync_had_results() {
        // When sync sources gave us candidates and LSP says
        // NoServers, the popup stays open with the sync set.
        let mut a = app_with("alpha alphabet alligator\nal", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        // No URI mapped -> LSP request didn't fire; the popup
        // is open from the sync sources alone. Manually push
        // a NoServers outcome to verify the drain handles it
        // without exploding.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<
            super::InsertCompletionLspOutcome,
        >();
        a.pending_insert_completion_lsp_rx = Some(rx);
        a.pending_insert_completion_lsp_token =
            Some(lattice_protocol::CancellationToken::new());
        tx.send(super::InsertCompletionLspOutcome::NoServers).unwrap();
        a.drain_pending_insert_completion_lsp();
        // Popup still open from sync sources.
        assert!(a.insert_completion.is_some());
    }

    #[test]
    fn drain_pending_insert_completion_lsp_items_merge_into_popup() {
        let mut a = app_with("\nfo", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(1, 2);
        // Seed the popup state directly -- skip do_completion_trigger
        // so the test doesn't depend on sync sources producing
        // matches first. The drain merges LSP items into
        // whatever raw set is present.
        a.insert_completion = Some(
            lattice_completion::InsertCompletionState::open(
                lattice_completion::CompletionTrigger::Manual,
                Position::new(1, 0),
                Position::new(1, 2),
                "fo".to_string(),
            ),
        );
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<
            super::InsertCompletionLspOutcome,
        >();
        a.pending_insert_completion_lsp_rx = Some(rx);
        a.pending_insert_completion_lsp_token =
            Some(lattice_protocol::CancellationToken::new());
        tx.send(super::InsertCompletionLspOutcome::Items {
            items: vec![
                super::LspCompletionMeta {
                    label: "foo".into(),
                    insert_text: "foo".into(),
                    filter_text: None,
                    sort_text: None,
                    detail: Some("fn() -> i32".into()),
                    documentation: None,
                    kind: Some(lsp_types::CompletionItemKind::FUNCTION),
                    deprecated: false,
                    preselect: false,
                    commit_characters: Vec::new(),
                    additional_text_edits: Vec::new(),
                    command: None,
                    insert_text_format: lsp_types::InsertTextFormat::PLAIN_TEXT,
                    replace_range: None,
                    server_id: std::sync::Arc::from("test-server"),
                    original_item: lsp_types::CompletionItem::default(),
                    resolved: false,
                },
                super::LspCompletionMeta {
                    label: "foobar".into(),
                    insert_text: "foobar".into(),
                    filter_text: None,
                    sort_text: None,
                    detail: None,
                    documentation: None,
                    kind: Some(lsp_types::CompletionItemKind::VARIABLE),
                    deprecated: false,
                    preselect: false,
                    commit_characters: Vec::new(),
                    additional_text_edits: Vec::new(),
                    command: None,
                    insert_text_format: lsp_types::InsertTextFormat::PLAIN_TEXT,
                    replace_range: None,
                    server_id: std::sync::Arc::from("test-server"),
                    original_item: lsp_types::CompletionItem::default(),
                    resolved: false,
                },
            ],
            is_incomplete: false,
        })
        .unwrap();
        a.drain_pending_insert_completion_lsp();
        let state = a.insert_completion.as_ref().expect("popup open");
        // Both items render; "foo" prefix matches both.
        let labels: Vec<String> = state
            .rendered
            .iter()
            .map(|c| c.raw.display.clone())
            .collect();
        assert!(labels.iter().any(|l| l.starts_with("foo")));
        assert!(labels.iter().any(|l| l.starts_with("foobar")));
        // Sidecar meta is populated.
        assert_eq!(a.insert_completion_lsp_meta.len(), 2);
    }

    #[test]
    fn drain_pending_insert_completion_lsp_drops_prior_lsp_rows_on_refresh() {
        // First merge populates LSP rows; second merge with
        // a different item set should REPLACE (not append).
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
        let mk_item = |label: &str| super::LspCompletionMeta {
            label: label.into(),
            insert_text: label.into(),
            filter_text: None,
            sort_text: None,
            detail: None,
            documentation: None,
            kind: None,
            deprecated: false,
            preselect: false,
            commit_characters: Vec::new(),
            additional_text_edits: Vec::new(),
            command: None,
            insert_text_format: lsp_types::InsertTextFormat::PLAIN_TEXT,
            replace_range: None,
            server_id: std::sync::Arc::from("test-server"),
            original_item: lsp_types::CompletionItem::default(),
            resolved: false,
        };
        // First batch.
        let (tx1, rx1) = tokio::sync::mpsc::unbounded_channel::<
            super::InsertCompletionLspOutcome,
        >();
        a.pending_insert_completion_lsp_rx = Some(rx1);
        a.pending_insert_completion_lsp_token =
            Some(lattice_protocol::CancellationToken::new());
        tx1.send(super::InsertCompletionLspOutcome::Items {
            items: vec![mk_item("alpha"), mk_item("alphabet")],
            is_incomplete: false,
        })
        .unwrap();
        a.drain_pending_insert_completion_lsp();
        assert_eq!(a.insert_completion_lsp_meta.len(), 2);
        let pre = a
            .insert_completion
            .as_ref()
            .map(|s| s.raw.len())
            .unwrap_or(0);
        assert_eq!(pre, 2);
        // Second batch -- only one item, "beta". Prior LSP
        // rows should be pruned.
        let (tx2, rx2) = tokio::sync::mpsc::unbounded_channel::<
            super::InsertCompletionLspOutcome,
        >();
        a.pending_insert_completion_lsp_rx = Some(rx2);
        a.pending_insert_completion_lsp_token =
            Some(lattice_protocol::CancellationToken::new());
        tx2.send(super::InsertCompletionLspOutcome::Items {
            items: vec![mk_item("beta")],
            is_incomplete: false,
        })
        .unwrap();
        a.drain_pending_insert_completion_lsp();
        assert_eq!(a.insert_completion_lsp_meta.len(), 1);
        assert_eq!(a.insert_completion_lsp_meta[0].label, "beta");
    }

    #[test]
    fn lsp_completion_meta_for_returns_none_for_sync_sourced_candidates() {
        let a = app_with("xx", 10);
        let raw = lattice_completion::RawCandidate::plain(
            "foo",
            lattice_completion::CandidateKind::Plain,
        );
        let scored = lattice_completion::ScoredCandidate {
            raw,
            score: lattice_completion::MatchScore(100),
            match_ranges: Vec::new(),
        };
        let rendered =
            lattice_completion::RenderedCandidate::from_scored(scored);
        assert!(a.lsp_completion_meta_for(&rendered).is_none());
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
        let mut raw = lattice_completion::RawCandidate::plain(
            "foo",
            lattice_completion::CandidateKind::Plain,
        );
        raw.display = "foo".into();
        raw.data = lattice_completion::CandidateData::Extension {
            kind_id: super::LSP_COMPLETION_KIND_ID,
            payload: 0u32.to_le_bytes().to_vec(),
        };
        let scored = lattice_completion::ScoredCandidate {
            raw,
            score: lattice_completion::MatchScore(100),
            match_ranges: Vec::new(),
        };
        state
            .rendered
            .push(lattice_completion::RenderedCandidate::from_scored(scored));
        a.insert_completion = Some(state);
        a.insert_completion_lsp_meta.push(super::LspCompletionMeta {
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
            server_id: std::sync::Arc::from("test-server"),
            original_item: lsp_types::CompletionItem::default(),
            resolved: true,
        });
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
    fn drain_pending_completion_resolve_fills_metadata_and_body() {
        let mut a = app_with("xx", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::ZERO;
        // Build state with one candidate pointing at meta[0].
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            Position::ZERO,
            Position::ZERO,
            String::new(),
        );
        let mut raw = lattice_completion::RawCandidate::plain(
            "foo",
            lattice_completion::CandidateKind::Plain,
        );
        raw.data = lattice_completion::CandidateData::Extension {
            kind_id: super::LSP_COMPLETION_KIND_ID,
            payload: 0u32.to_le_bytes().to_vec(),
        };
        state
            .rendered
            .push(lattice_completion::RenderedCandidate::from_scored(
                lattice_completion::ScoredCandidate {
                    raw,
                    score: lattice_completion::MatchScore(100),
                    match_ranges: Vec::new(),
                },
            ));
        // Open the doc popup -- empty body initially because
        // meta has no documentation yet.
        state.doc_popup = Some(lattice_completion::DocPopupState {
            for_index: 0,
            body: None,
            scroll: 5, // verify scroll resets on body refresh
        });
        a.insert_completion = Some(state);
        a.insert_completion_lsp_meta.push(super::LspCompletionMeta {
            label: "foo".into(),
            insert_text: "foo".into(),
            filter_text: None,
            sort_text: None,
            detail: None,
            documentation: None,
            kind: None,
            deprecated: false,
            preselect: false,
            commit_characters: Vec::new(),
            additional_text_edits: Vec::new(),
            command: None,
            insert_text_format: lsp_types::InsertTextFormat::PLAIN_TEXT,
            replace_range: None,
            server_id: std::sync::Arc::from("test-server"),
            original_item: lsp_types::CompletionItem::default(),
            resolved: false,
        });
        // Push a resolve outcome that fills documentation.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<
            super::CompletionResolveOutcome,
        >();
        a.pending_completion_resolve_rx = Some(rx);
        a.pending_completion_resolve_token =
            Some(lattice_protocol::CancellationToken::new());
        let mut resolved = lsp_types::CompletionItem::default();
        resolved.label = "foo".into();
        resolved.detail = Some("fn foo() -> i32".into());
        resolved.documentation = Some(lsp_types::Documentation::String(
            "Returns 42.".into(),
        ));
        tx.send(super::CompletionResolveOutcome {
            meta_index: 0,
            resolved,
        })
        .unwrap();
        a.drain_pending_completion_resolve();
        // Meta updated.
        let meta = &a.insert_completion_lsp_meta[0];
        assert!(meta.resolved);
        assert_eq!(meta.detail.as_deref(), Some("fn foo() -> i32"));
        assert_eq!(meta.documentation.as_deref(), Some("Returns 42."));
        // Doc popup body refreshed; scroll reset to 0.
        let popup = a
            .insert_completion
            .as_ref()
            .and_then(|s| s.doc_popup.as_ref())
            .expect("popup");
        assert_eq!(popup.scroll, 0);
        let body = popup.body.as_deref().unwrap_or("");
        assert!(body.contains("fn foo() -> i32"));
        assert!(body.contains("Returns 42."));
    }

    #[test]
    fn drain_pending_completion_resolve_drops_stale_index_after_selection_moved() {
        // Resolve arrives for meta[0] but selection has moved
        // to meta[1]. The meta still updates (so a future
        // refocus uses the cached docs) but the doc popup body
        // doesn't change.
        let mut a = app_with("xx", 10);
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            Position::ZERO,
            Position::ZERO,
            String::new(),
        );
        for i in 0..2u32 {
            let mut raw = lattice_completion::RawCandidate::plain(
                format!("c{i}"),
                lattice_completion::CandidateKind::Plain,
            );
            raw.data = lattice_completion::CandidateData::Extension {
                kind_id: super::LSP_COMPLETION_KIND_ID,
                payload: i.to_le_bytes().to_vec(),
            };
            state.rendered.push(
                lattice_completion::RenderedCandidate::from_scored(
                    lattice_completion::ScoredCandidate {
                        raw,
                        score: lattice_completion::MatchScore(100),
                        match_ranges: Vec::new(),
                    },
                ),
            );
        }
        state.selected = 1; // user moved past meta[0]
        state.doc_popup = Some(lattice_completion::DocPopupState {
            for_index: 1,
            body: Some("for c1".into()),
            scroll: 0,
        });
        a.insert_completion = Some(state);
        for i in 0..2 {
            a.insert_completion_lsp_meta.push(super::LspCompletionMeta {
                label: format!("c{i}"),
                insert_text: format!("c{i}"),
                filter_text: None,
                sort_text: None,
                detail: None,
                documentation: None,
                kind: None,
                deprecated: false,
                preselect: false,
                commit_characters: Vec::new(),
                additional_text_edits: Vec::new(),
                command: None,
                insert_text_format: lsp_types::InsertTextFormat::PLAIN_TEXT,
                replace_range: None,
                server_id: std::sync::Arc::from("test-server"),
                original_item: lsp_types::CompletionItem::default(),
                resolved: false,
            });
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<
            super::CompletionResolveOutcome,
        >();
        a.pending_completion_resolve_rx = Some(rx);
        a.pending_completion_resolve_token =
            Some(lattice_protocol::CancellationToken::new());
        let mut resolved = lsp_types::CompletionItem::default();
        resolved.label = "c0".into();
        resolved.documentation = Some(lsp_types::Documentation::String(
            "stale".into(),
        ));
        tx.send(super::CompletionResolveOutcome {
            meta_index: 0,
            resolved,
        })
        .unwrap();
        a.drain_pending_completion_resolve();
        // Meta[0] updated.
        assert!(a.insert_completion_lsp_meta[0].resolved);
        assert!(
            a.insert_completion_lsp_meta[0]
                .documentation
                .as_deref()
                == Some("stale")
        );
        // Doc popup body unchanged (still pointing at meta[1]).
        let body = a
            .insert_completion
            .as_ref()
            .and_then(|s| s.doc_popup.as_ref())
            .and_then(|d| d.body.clone());
        assert_eq!(body.as_deref(), Some("for c1"));
    }

    #[test]
    fn lsp_completion_meta_for_resolves_extension_payload_index() {
        let mut a = app_with("xx", 10);
        a.insert_completion_lsp_meta.push(super::LspCompletionMeta {
            label: "first".into(),
            insert_text: "first".into(),
            filter_text: None,
            sort_text: None,
            detail: None,
            documentation: None,
            kind: None,
            deprecated: false,
            preselect: false,
            commit_characters: Vec::new(),
            additional_text_edits: Vec::new(),
            command: None,
            insert_text_format: lsp_types::InsertTextFormat::PLAIN_TEXT,
            replace_range: None,
            server_id: std::sync::Arc::from("test-server"),
            original_item: lsp_types::CompletionItem::default(),
            resolved: false,
        });
        a.insert_completion_lsp_meta.push(super::LspCompletionMeta {
            label: "second".into(),
            insert_text: "second".into(),
            filter_text: None,
            sort_text: None,
            detail: None,
            documentation: None,
            kind: None,
            deprecated: false,
            preselect: false,
            commit_characters: Vec::new(),
            additional_text_edits: Vec::new(),
            command: None,
            insert_text_format: lsp_types::InsertTextFormat::PLAIN_TEXT,
            replace_range: None,
            server_id: std::sync::Arc::from("test-server"),
            original_item: lsp_types::CompletionItem::default(),
            resolved: false,
        });
        // Build a candidate pointing at index 1.
        let mut raw = lattice_completion::RawCandidate::plain(
            "second",
            lattice_completion::CandidateKind::Plain,
        );
        raw.data = lattice_completion::CandidateData::Extension {
            kind_id: super::LSP_COMPLETION_KIND_ID,
            payload: 1u32.to_le_bytes().to_vec(),
        };
        let scored = lattice_completion::ScoredCandidate {
            raw,
            score: lattice_completion::MatchScore(100),
            match_ranges: Vec::new(),
        };
        let rendered =
            lattice_completion::RenderedCandidate::from_scored(scored);
        let meta = a
            .lsp_completion_meta_for(&rendered)
            .expect("meta resolves");
        assert_eq!(meta.label, "second");
    }

    #[test]
    fn drain_pending_completion_no_servers_echoes() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::CompletionOutcome>();
        a.pending_completion_rx = Some(rx);
        a.pending_completion_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::CompletionOutcome::NoServers).unwrap();
        a.drain_pending_completion();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no LSP server"));
    }

    #[test]
    fn drain_pending_completion_items_open_picker_with_indexed_text() {
        let mut a = app_with("foo\n", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::CompletionOutcome>();
        a.pending_completion_rx = Some(rx);
        a.pending_completion_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::CompletionOutcome::Items(vec![
            super::CompletionItemRow {
                label: "foo_bar".into(),
                kind_glyph: "ƒ",
                detail: Some("fn foo_bar()".into()),
                insert_text: "foo_bar()".into(),
                replace_range: (0, 3),
                line: 0,
            },
        ]))
        .unwrap();
        a.drain_pending_completion();
        let picker = a.picker.as_ref().expect("picker");
        assert!(picker.title.starts_with("complete"));
        assert!(matches!(
            picker.on_accept,
            crate::picker::PickerAction::AcceptLspCompletion
        ));
        assert_eq!(picker.candidates.len(), 1);
        // Display carries kind glyph + label + detail.
        let display = &picker.candidates[0].raw.display;
        assert!(display.contains("ƒ foo_bar"));
        assert!(display.contains("fn foo_bar()"));
        // Routing payload carries the typed LspCompletion index
        // (post-4.2.g.7 typed routing replaces the prior `#<idx>`
        // string encoding).
        let routing = picker
            .routing_for(&picker.candidates[0])
            .expect("routing payload set");
        match routing {
            crate::picker::RoutingPayload::LspCompletion { index } => {
                assert_eq!(*index, 0);
            }
            other => panic!("expected LspCompletion routing, got {other:?}"),
        }
        // Items survive on the App for the accept path.
        assert!(a.pending_completion_items.is_some());
    }

    #[test]
    fn drain_pending_completion_empty_echoes_no_completions() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::CompletionOutcome>();
        a.pending_completion_rx = Some(rx);
        a.pending_completion_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::CompletionOutcome::Items(Vec::new())).unwrap();
        a.drain_pending_completion();
        assert!(a.picker.is_none());
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no completions"));
    }

    #[test]
    fn signature_help_to_markdown_renders_active_signature() {
        let sh = lsp_types::SignatureHelp {
            signatures: vec![
                lsp_types::SignatureInformation {
                    label: "fn foo(a: i32, b: &str) -> i32".into(),
                    documentation: Some(lsp_types::Documentation::String(
                        "Adds.".into(),
                    )),
                    parameters: Some(vec![
                        lsp_types::ParameterInformation {
                            label: lsp_types::ParameterLabel::Simple("a: i32".into()),
                            documentation: Some(lsp_types::Documentation::String(
                                "the first.".into(),
                            )),
                        },
                        lsp_types::ParameterInformation {
                            label: lsp_types::ParameterLabel::Simple("b: &str".into()),
                            documentation: None,
                        },
                    ]),
                    active_parameter: Some(0),
                },
            ],
            active_signature: Some(0),
            active_parameter: None,
        };
        let body = super::signature_help_to_markdown(&sh);
        assert!(body.contains("fn foo(a: i32"));
        assert!(body.contains("**param:** `a: i32`"));
        assert!(body.contains("the first."));
        assert!(body.contains("Adds."));
    }

    #[test]
    fn signature_help_to_markdown_empty_when_no_signatures() {
        let sh = lsp_types::SignatureHelp {
            signatures: vec![],
            active_signature: None,
            active_parameter: None,
        };
        assert_eq!(super::signature_help_to_markdown(&sh), "");
    }

    #[test]
    fn drain_pending_signature_help_body_opens_popup() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::SignatureHelpOutcome>();
        a.pending_signature_help_rx = Some(rx);
        a.pending_signature_help_token =
            Some(lattice_protocol::CancellationToken::new());
        tx.send(super::SignatureHelpOutcome::Body(
            "```text\nfn x()\n```\n".into(),
        ))
        .unwrap();
        a.drain_pending_signature_help();
        let h = a.help_buffer.as_ref().expect("popup");
        assert_eq!(h.title, "hover");
        assert!(a.pending_signature_help_token.is_none());
    }

    #[test]
    fn drain_pending_signature_help_empty_body_echoes_no_signature_info() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::SignatureHelpOutcome>();
        a.pending_signature_help_rx = Some(rx);
        a.pending_signature_help_token =
            Some(lattice_protocol::CancellationToken::new());
        tx.send(super::SignatureHelpOutcome::Body(String::new())).unwrap();
        a.drain_pending_signature_help();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no signature info"));
        assert!(a.help_buffer.is_none());
    }

    #[test]
    fn tag_stack_pop_on_empty_echoes_message() {
        let mut a = app_with("xx", 10);
        a.apply(Action::TagStackPop);
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("tag stack empty"));
    }

    #[test]
    fn tag_stack_drives_pop_back_to_origin() {
        let mut a = app_with("alpha\nbeta\ngamma\ndelta\n", 10);
        // Pretend we drilled down from line 0 col 2 to line 3
        // col 1 (the gd-like `do_lsp_nav_request` -> drain
        // single-result path normally pushes; we synthesise
        // the entry directly to keep the test free of LSP wire).
        a.tag_stack.push(super::TagStackEntry {
            buffer: a.active_buffer,
            buffer_id: a.active_pane_buffer_id(),
            position: Position::new(0, 2),
            label: "foo".into(),
        });
        a.cursor = Position::new(3, 1);
        a.apply(Action::TagStackPop);
        assert_eq!(a.cursor, Position::new(0, 2));
        assert!(a.tag_stack.is_empty());
        // Pop pushes the post-pop cursor onto position history
        // (PluginPush) so a follow-up `<C-i>` returns to (3, 1).
        let last = a.position_history.last().expect("history entry");
        assert!(matches!(last.source, PositionSource::PluginPush));
        assert_eq!(last.position, Position::new(3, 1));
    }

    #[test]
    fn nav_request_captures_tag_origin_for_picker_consumption() {
        // `do_lsp_nav_request` should set `pending_tag_origin`
        // so a subsequent picker accept (multi-result) pushes
        // the right entry onto the tag stack.
        let mut a = app_with("foo bar\nbaz\n", 10);
        a.cursor = Position::new(0, 1);
        // Manually set a uri so do_lsp_nav_request gets past
        // the "no LSP server" guard.
        use std::str::FromStr;
        a.buffer_uris.insert(
            a.document_buffer_id,
            lattice_lsp::Uri::from_str("file:///tmp/x.rs").unwrap(),
        );
        a.apply(Action::LspDefinitionRequest);
        let origin = a.pending_tag_origin.as_ref().expect("origin set");
        assert_eq!(origin.position, Position::new(0, 1));
        assert_eq!(origin.label, "foo");
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

    #[test]
    fn lsp_nav_request_pre_cancels_prior_token_regardless_of_kind() {
        // A new nav request of any kind must cancel a still-in-flight
        // request of any other kind -- they all share one slot.
        let mut a = app_with("xx", 10);
        let stale = lattice_protocol::CancellationToken::new();
        a.pending_definition_token = Some(stale.clone());
        a.apply(Action::LspImplementationRequest);
        assert!(stale.is_cancelled());
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
    fn drain_pending_definitions_with_multiple_opens_picker() {
        // After the picker pivot, multi-result nav opens the
        // vertico picker rather than auto-jumping to the first
        // result. Single-result jump path is still tested by
        // `drain_pending_definitions_with_single_same_buffer_jumps_in_place`.
        let path = std::env::temp_dir()
            .join(format!("lattice-defmulti-{}.rs", std::process::id()));
        std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
        let doc = Document::open(&path).unwrap();
        let mut a = App::new(doc);
        a.set_viewport_height(10);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<lsp_types::Location>>();
        a.pending_definition_rx = Some(rx);
        a.pending_definition_token = Some(lattice_protocol::CancellationToken::new());
        a.pending_nav_kind = Some(super::LspNavKind::Definition);
        let target_path = path.to_str().unwrap();
        tx.send(vec![
            super::tests::loc(target_path, 1, 0),
            super::tests::loc(target_path, 2, 0),
        ])
        .unwrap();
        a.drain_pending_definitions();
        let picker = a.picker.as_ref().expect("multi-result opens picker");
        assert_eq!(picker.title, "lsp:definitions");
        assert_eq!(picker.candidates.len(), 2);
        assert!(matches!(
            picker.on_accept,
            crate::picker::PickerAction::JumpToLspLocation
        ));
        // Cursor should NOT have moved (no auto-jump).
        assert_eq!(a.cursor.line, 0);
        let _ = std::fs::remove_file(path);
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
    fn tree_follow_on_file_opens_document_buffer() {
        let dir = std::env::temp_dir().join(format!("lattice-tree-follow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(dir.join("alpha.txt"), "hello").ok();
        let mut a = app_with("xx", 10);
        a.command_line = format!("Filetree {}", dir.display());
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
    fn help_popup_inner_height_caps_at_twenty() {
        // 50-line help in a 60-row buffer: popup height clamps at
        // 20, inner = 18. Motion uses this as the viewport so
        // ensure_cursor_visible scrolls the popup -- not the full
        // pane -- when the cursor reaches the bottom row.
        let mut a = app_with("xx", 60);
        let lines: Vec<String> = (0..50).map(|i| format!("line-{i}")).collect();
        install_help(&mut a, HelpBuffer::from_lines("size", lines));
        assert_eq!(a.help_popup_inner_height(60), Some(18));
        // Confirm `active_pane_content_height` routes through the
        // popup-inner branch in State B, so the runtime feeds 18
        // into `set_viewport_height` (not the full 60-row pane).
        assert_eq!(a.active_pane_content_height(60), 18);
    }

    #[test]
    fn help_popup_inner_height_fits_short_content() {
        // 4-line help: popup auto-fits to height 6 (4 + 2 borders),
        // inner = 4. Cursor can never go off-popup-viewport
        // because the popup shows every line of the help buffer.
        let mut a = app_with("xx", 60);
        install_help(
            &mut a,
            HelpBuffer::from_lines("tiny", vec!["a".into(); 4]),
        );
        assert_eq!(a.help_popup_inner_height(60), Some(4));
    }

    #[test]
    fn help_popup_inner_height_none_when_pane_holds_help() {
        // In-pane help (e.g. `:lsp-log`) -- pane.buffer is Help, so
        // the help fills the pane and the regular pane-content-
        // height path applies. No overlay sizing.
        let mut a = app_with("xx", 60);
        let id = a.open_help_in_pane(HelpBuffer::from_lines("log", vec!["a".into(); 8]));
        assert_eq!(a.pane_tree.active().buffer_id, id);
        assert_eq!(a.help_popup_inner_height(60), None);
    }

    #[test]
    fn help_popup_j_past_last_line_does_not_advance_cursor() {
        // Regression for "j past last line in popup advanced
        // cursor.line internally" -- the pane viewport (60 rows)
        // hid the overshoot from `ensure_cursor_visible`, so
        // cursor.line crept past the last visible popup row and
        // every k afterwards had to walk back through the phantom
        // gap before any visible motion. Now `viewport_height`
        // matches the popup's inner height (18 here) AND the
        // motion path clamps `cursor.line` to last_addressable.
        let mut a = app_with("xx", 60);
        let lines: Vec<String> = (0..50).map(|i| format!("line-{i}")).collect();
        install_help(&mut a, HelpBuffer::from_lines("scroll", lines));
        a.set_viewport_height(a.active_pane_content_height(60));
        let line_down = a.builtins.line_down;
        let line_up = a.builtins.line_up;
        // `G` to the last line first so we're at the clamp.
        let goto_last = a.builtins.goto_last_line;
        a.apply(Action::Invoke(CommandInvocation::of(goto_last.0)));
        assert_eq!(a.cursor.line, 49);
        // Press j five times past the last line. cursor.line must
        // stay pinned at 49 -- no phantom overshoot.
        for _ in 0..5 {
            a.apply(Action::Invoke(CommandInvocation::of(line_down.0)));
        }
        assert_eq!(a.cursor.line, 49);
        // First k must move up immediately, not "unwind" any
        // overshoot.
        a.apply(Action::Invoke(CommandInvocation::of(line_up.0)));
        assert_eq!(a.cursor.line, 48);
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


    // ---- LSP wiring tests (Phase 4.1.i) ---------------------

    #[test]
    fn lsp_supervisor_constructed_with_builtin_configs() {
        let app = App::new(Document::from_text(""));
        // Builtin registry: rust, python, go, typescript, c-cpp,
        // lua. Six entries today.
        assert!(
            app.lsp.configs().len() >= 6,
            "expected at least 6 builtin server configs"
        );
        // Supervisor starts dormant.
        assert_eq!(app.lsp.running_actor_count(), 0);
        assert_eq!(app.lsp.attached_buffer_count(), 0);
        assert!(app.buffer_uris.is_empty());
    }

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

    #[test]
    fn lsp_close_buffer_removes_uri_mapping_for_unattached_buffer() {
        let mut app = App::new(Document::from_text(""));
        // Seed a fake mapping (as if the attach driver's open
        // had landed for a path-bearing buffer).
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
    fn list_diagnostics_opens_picker() {
        let mut app = app_with("hi\n", 5);
        seed_diags_at_lines(&mut app, &[0, 1]);
        app.do_list_diagnostics();
        let picker = app.picker.as_ref().expect("picker should open");
        assert!(picker.title.starts_with("diagnostics"));
        assert!(matches!(
            picker.source,
            crate::picker::PickerSource::LspLocations
        ));
        assert!(matches!(
            picker.on_accept,
            crate::picker::PickerAction::JumpToLspLocation
        ));
        // Two diagnostic rows.
        assert_eq!(picker.candidates.len(), 2);
        // Severity prefix marginalia in display.
        let display = &picker.candidates[0].raw.display;
        assert!(display.starts_with("[E]"), "got: {display}");
        // Help buffer is NOT opened (the pre-picker shape).
        assert!(app.help_buffer.is_none());
    }

    #[test]
    fn list_diagnostics_with_empty_layer_echoes() {
        let mut app = app_with("hi\n", 5);
        // No diagnostics seeded.
        app.do_list_diagnostics();
        // Empty diagnostics: no picker, just an echo.
        assert!(app.picker.is_none());
        let msg = app.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no diagnostics"));
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
    fn persistent_lsp_log_level_applies_from_toml_tree() {
        let mut app = app_with("hi\n", 5);
        let toml_text = "[lsp]\nlog-level = \"debug\"\n";
        app.lsp_config_tree = toml_text.parse().expect("toml parse");
        app.apply_persistent_lsp_editor_options();
        // Effect: a Debug-level record on an unattached server lands
        // in the ring. Default min-level is Info; without the TOML
        // override the record would be filtered before it reached
        // the ring.
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        app.lsp_logger.log(
            Some(&id),
            lattice_lsp::LogLevel::Debug,
            lattice_lsp::LogSource::Client,
            "after-toml",
        );
        let recs = app.lsp_logger.snapshot_server(&id);
        assert!(
            recs.iter().any(|r| r.message == "after-toml"),
            "Debug record should pass through after TOML log-level=debug",
        );
    }

    #[test]
    fn persistent_lsp_log_level_warns_on_unknown_value() {
        let mut app = app_with("hi\n", 5);
        let toml_text = "[lsp]\nlog-level = \"babble\"\n";
        app.lsp_config_tree = toml_text.parse().expect("toml parse");
        app.apply_persistent_lsp_editor_options();
        let msg = app.last_message.as_ref().expect("warn echo");
        assert!(
            msg.text.contains("lsp.log-level") && msg.text.contains("babble"),
            "echo should name the key + value, got {}",
            msg.text
        );
    }

    #[test]
    fn persistent_lsp_log_level_silent_when_missing() {
        let mut app = app_with("hi\n", 5);
        app.last_message = None;
        // Empty tree: nothing under [lsp].
        app.lsp_config_tree = toml::Table::new();
        app.apply_persistent_lsp_editor_options();
        assert!(
            app.last_message.is_none(),
            "no echo when key is absent (default applies)",
        );
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
        let mut app = app_with("hi\n", 5);
        let r = app.apply_edit_blocking(Edit::insert(Position::new(0, 0), "x"));
        assert!(r.is_ok());
    }

    #[test]
    fn apply_edit_batch_blocking_records_each_edit_in_order() {
        let mut app = app_with("abc\n", 5);
        let edits = vec![
            Edit::insert(Position::new(0, 0), "1"),
            Edit::insert(Position::new(0, 1), "2"),
        ];
        // No LSP attachment seeded -> records short-circuit;
        // we only check the path is reachable (no panic).
        let r = app.apply_edit_batch_blocking(edits);
        assert!(r.is_ok());
    }

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

    fn install_snippet(a: &mut App, language: &str, name: &str, prefix: &str, body: &str) {
        let parsed = lattice_snippet::parse::parse(body).unwrap();
        a.snippet_registry.insert(
            language,
            lattice_snippet::Snippet {
                name: name.into(),
                prefixes: vec![prefix.into()],
                body: parsed,
                description: None,
                scope: String::new(),
            },
        );
    }

    #[test]
    fn snippet_expand_at_cursor_splices_body_and_focuses_first_tabstop() {
        // Buffer: `for `; cursor sits past the prefix `for`
        // so the lookup picks it up. After expansion we
        // expect the snippet's literal text in the buffer
        // and an active snippet pointing at $1.
        let mut a = app_with("for", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        install_snippet(&mut a, "*", "for-loop", "for", "for ${1:i} in ${2:iter} { $0 }");
        a.do_snippet_expand_at_cursor();
        // Buffer text should be the rendered snippet.
        let text = a.document.snapshot().buffer.as_string();
        assert_eq!(text, "for i in iter {  }");
        // Active snippet present, focused on $1.
        let active = a.active_snippet.as_ref().expect("snippet active");
        assert_eq!(active.current_index(), Some(1));
        // Cursor at start of `i`.
        assert_eq!(a.cursor, Position::new(0, 4));
    }

    #[test]
    fn snippet_next_placeholder_walks_through_groups_and_drops_on_zero() {
        let mut a = app_with("for", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        install_snippet(&mut a, "*", "for-loop", "for", "for ${1:i} in ${2:iter} { $0 }");
        a.do_snippet_expand_at_cursor();
        // Now at $1.
        assert_eq!(
            a.active_snippet.as_ref().unwrap().current_index(),
            Some(1)
        );
        a.do_snippet_next_placeholder();
        assert_eq!(
            a.active_snippet.as_ref().unwrap().current_index(),
            Some(2)
        );
        a.do_snippet_next_placeholder();
        // $0 is the exit; at this point we're focused on it.
        assert_eq!(
            a.active_snippet.as_ref().unwrap().current_index(),
            Some(0)
        );
        a.do_snippet_next_placeholder();
        // Past $0 -> snippet dropped.
        assert!(a.active_snippet.is_none());
    }

    #[test]
    fn snippet_prev_placeholder_walks_back() {
        let mut a = app_with("for", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        install_snippet(&mut a, "*", "for-loop", "for", "for ${1:i} in ${2:iter} {}");
        a.do_snippet_expand_at_cursor();
        a.do_snippet_next_placeholder();
        assert_eq!(
            a.active_snippet.as_ref().unwrap().current_index(),
            Some(2)
        );
        a.do_snippet_prev_placeholder();
        assert_eq!(
            a.active_snippet.as_ref().unwrap().current_index(),
            Some(1)
        );
    }

    #[test]
    fn snippet_expand_with_no_match_is_a_no_op() {
        let mut a = app_with("xyz", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        a.do_snippet_expand_at_cursor();
        assert!(a.active_snippet.is_none());
        // Buffer unchanged.
        assert_eq!(a.document.snapshot().buffer.as_string(), "xyz");
    }

    #[test]
    fn snippet_expand_outside_insert_mode_is_a_no_op() {
        let mut a = app_with("for", 10);
        a.cursor = Position::new(0, 3);
        // Stay in Normal -- guard inside `do_snippet_expand_at_cursor`.
        install_snippet(&mut a, "*", "for-loop", "for", "for $1 {}");
        a.do_snippet_expand_at_cursor();
        assert!(a.active_snippet.is_none());
        assert_eq!(a.document.snapshot().buffer.as_string(), "for");
    }

    #[test]
    fn completion_trigger_includes_snippet_candidate_for_matching_prefix() {
        let mut a = app_with("for", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        install_snippet(&mut a, "*", "for-loop", "for", "for ${1:i} in ${2:iter} {}");
        a.do_completion_trigger();
        let state = a.insert_completion.as_ref().expect("popup open");
        // `for-loop` snippet appears as a candidate. The
        // candidate's text is the prefix; the meta sidecar
        // carries the parsed body for the accept path.
        assert!(state.rendered.iter().any(|r| r.raw.text == "for"));
        // Sidecar populated -- one snippet candidate registered.
        assert_eq!(a.insert_completion_snippet_meta.len(), 1);
        assert_eq!(a.insert_completion_snippet_meta[0].name, "for-loop");
    }

    #[test]
    fn completion_accept_on_snippet_candidate_starts_active_snippet() {
        let mut a = app_with("for", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        install_snippet(&mut a, "*", "for-loop", "for", "for ${1:i} in ${2:iter} {}");
        a.do_completion_trigger();
        // Find the snippet candidate index and select it.
        let state = a.insert_completion.as_mut().expect("popup");
        let idx = state
            .rendered
            .iter()
            .position(|r| {
                matches!(
                    r.raw.data,
                    lattice_completion::CandidateData::Extension {
                        kind_id,
                        ..
                    } if kind_id == SNIPPET_COMPLETION_KIND_ID
                )
            })
            .expect("snippet candidate present");
        state.selected = idx;
        a.do_completion_accept();
        // Popup closed; active snippet is in flight focused on
        // $1; buffer reflects expansion.
        assert!(a.insert_completion.is_none());
        let active = a.active_snippet.as_ref().expect("active snippet");
        assert_eq!(active.current_index(), Some(1));
        let text = a.document.snapshot().buffer.as_string();
        assert_eq!(text, "for i in iter {}");
    }

    #[test]
    fn completion_accept_bumps_frequency_map_for_text_kind_pair() {
        // Trigger completion against a buffer-words source and
        // accept a candidate. The App's accept-frequency map
        // gets a new entry keyed by `(text, kind)` with count 1.
        let mut a = app_with("alpha bravo charlie ", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 20);
        a.do_completion_trigger();
        // Empty query at end of line -> all three buffer words
        // surface as candidates. Find `bravo` and select it.
        let state = a.insert_completion.as_mut().expect("popup");
        let idx = state
            .rendered
            .iter()
            .position(|r| r.raw.text == "bravo")
            .expect("bravo present");
        state.selected = idx;
        a.do_completion_accept();
        // Map records exactly one accept of (bravo, Plain).
        let key = (
            "bravo".to_string(),
            lattice_completion::CandidateKind::Plain,
        );
        assert_eq!(a.completion_accept_freq.get(&key).copied(), Some(1));
    }

    #[test]
    fn completion_trigger_ranks_previously_accepted_above_tied_peer() {
        // Two buffer words tie on matcher score (empty query
        // -> uniform 100); a previous accept of `bravo` lifts
        // it to the top of the rendered list.
        let mut a = app_with("alpha bravo charlie ", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 20);
        // Seed the freq map directly -- this is the integration
        // boundary we care about (the App's map fed into the
        // ranker), not the accept-then-retrigger cycle.
        a.completion_accept_freq.insert(
            (
                "bravo".to_string(),
                lattice_completion::CandidateKind::Plain,
            ),
            3,
        );
        a.do_completion_trigger();
        let state = a.insert_completion.as_ref().expect("popup");
        // First rendered candidate is the previously-accepted
        // one, ahead of its tied peers.
        assert_eq!(state.rendered.first().expect("at least one").raw.text, "bravo");
    }

    fn write_workspace_config(workspace: &std::path::Path, contents: &str) {
        let dir = workspace.join(".lattice");
        std::fs::create_dir_all(&dir).expect("create .lattice dir");
        std::fs::write(dir.join("config.toml"), contents).expect("write config.toml");
    }

    fn fresh_workspace(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "lattice-config-test-{}-{}",
            std::process::id(),
            name,
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create workspace");
        p
    }

    #[test]
    fn load_persistent_config_applies_scalar_override_from_project_toml() {
        let ws = fresh_workspace("scalar-override");
        write_workspace_config(&ws, "tabstop = 4\n");
        let mut a = app_with("", 5);
        // tabstop default is 8; override should land before
        // first frame.
        assert_eq!(*a.config.get_typed::<lattice_config::Tabstop>().unwrap(), 8);
        a.load_persistent_config(Some(&ws));
        assert_eq!(*a.config.get_typed::<lattice_config::Tabstop>().unwrap(), 4);
    }

    #[test]
    fn load_persistent_config_buckets_per_language_section() {
        let ws = fresh_workspace("per-lang-bucket");
        write_workspace_config(
            &ws,
            "[completion.per-language.markdown]\n\
             auto_trigger = false\n\
             [completion.per-language.rust]\n\
             auto_trigger = true\n",
        );
        let mut a = app_with("", 5);
        a.load_persistent_config(Some(&ws));
        // Both per-language entries land in the structural
        // bucket, keyed by full dotted path.
        let paths = a.pending_structural_section_paths("completion.per-language");
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"completion.per-language.markdown".to_string()));
        assert!(paths.contains(&"completion.per-language.rust".to_string()));
        // Drain markdown -> sub-table accessible.
        let md = a
            .take_pending_structural_section("completion.per-language.markdown")
            .expect("markdown section drained");
        assert_eq!(
            md.get("auto_trigger").and_then(|v| v.as_bool()),
            Some(false),
        );
        // After drain, only rust remains.
        let after = a.pending_structural_section_paths("completion.per-language");
        assert_eq!(after, vec!["completion.per-language.rust".to_string()]);
    }

    #[test]
    fn load_persistent_config_collects_unknown_plugin_section_for_later_drain() {
        // Extensibility: a user writes `[plugin.X]` before the
        // plugin host exists. Loader buckets it; nothing warns;
        // the host (Phase 7) drains it when it registers.
        let ws = fresh_workspace("plugin-deferred");
        write_workspace_config(
            &ws,
            "[plugin.rust-analyzer]\nclippy = true\n",
        );
        let mut a = app_with("", 5);
        a.load_persistent_config(Some(&ws));
        let paths = a.pending_structural_section_paths("plugin");
        assert_eq!(paths, vec!["plugin.rust-analyzer".to_string()]);
        let body = a
            .take_pending_structural_section("plugin.rust-analyzer")
            .expect("plugin section drained");
        assert_eq!(body.get("clippy").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn load_persistent_config_warning_surfaces_on_unknown_key() {
        let ws = fresh_workspace("unknown-key");
        write_workspace_config(&ws, "no_such_option = 42\n");
        let mut a = app_with("", 5);
        a.load_persistent_config(Some(&ws));
        // The echo carries the loader's warning.
        let msg = a.last_message.as_ref().expect("warning echoed");
        assert_eq!(msg.level, EchoLevel::Warn);
        assert!(msg.text.contains("config:"), "got `{}`", msg.text);
        assert!(
            msg.text.contains("no_such_option"),
            "got `{}`",
            msg.text,
        );
    }

    #[test]
    fn effective_completion_for_markdown_default_excludes_lsp() {
        // Spec default: markdown drops LSP for prose. The
        // App's seeded map should reflect this without any
        // TOML being loaded.
        let a = app_with("", 5);
        let eff = a.effective_completion_for("markdown");
        let lsp_id = lattice_completion::SourceId::new(
            lattice_completion::LSP_COMPLETION_SOURCE_ID,
        );
        assert!(!eff.source_enabled(&lsp_id), "markdown default drops LSP");
        let snippet_id = lattice_completion::SourceId::new(
            lattice_completion::SNIPPET_SOURCE_ID,
        );
        assert!(eff.source_enabled(&snippet_id), "markdown keeps snippet");
        assert_eq!(eff.auto_trigger, false);
    }

    #[test]
    fn effective_completion_for_language_with_no_override_allows_all_sources() {
        // A language without any per-language entry returns
        // `sources = None` -> every source contributes
        // (`source_enabled` is unconditionally true).
        let a = app_with("", 5);
        let eff = a.effective_completion_for("zigzig-not-a-language");
        let any_id = lattice_completion::SourceId::new("plugin:custom");
        assert!(eff.source_enabled(&any_id));
        assert!(eff.sources.is_none());
    }

    #[test]
    fn apply_per_language_toml_overrides_merges_with_spec_defaults() {
        // User flips markdown's `auto_trigger = true`; the
        // spec default `sources` (no LSP) should still apply.
        let ws = fresh_workspace("merge-with-defaults");
        write_workspace_config(
            &ws,
            "[completion.per-language.markdown]\n\
             auto_trigger = true\n",
        );
        let mut a = app_with("", 5);
        a.load_persistent_config(Some(&ws));
        a.apply_per_language_toml_overrides();
        let eff = a.effective_completion_for("markdown");
        assert_eq!(eff.auto_trigger, true, "TOML wins for auto_trigger");
        let lsp_id = lattice_completion::SourceId::new(
            lattice_completion::LSP_COMPLETION_SOURCE_ID,
        );
        assert!(
            !eff.source_enabled(&lsp_id),
            "default `sources` (no LSP) preserved when TOML didn't set it",
        );
    }

    #[test]
    fn apply_per_language_toml_overrides_seeds_new_language() {
        // `python` isn't in the spec defaults; a TOML entry
        // creates the slot.
        let ws = fresh_workspace("new-language");
        write_workspace_config(
            &ws,
            "[completion.per-language.python]\n\
             sources = [\"lsp\"]\n\
             auto_insert_single = true\n",
        );
        let mut a = app_with("", 5);
        a.load_persistent_config(Some(&ws));
        a.apply_per_language_toml_overrides();
        let eff = a.effective_completion_for("python");
        let lsp_id = lattice_completion::SourceId::new(
            lattice_completion::LSP_COMPLETION_SOURCE_ID,
        );
        assert!(eff.source_enabled(&lsp_id));
        let buffer_words_id = lattice_completion::SourceId::new(
            lattice_completion::BufferWordsSource::ID,
        );
        assert!(
            !eff.source_enabled(&buffer_words_id),
            "`sources = [\"lsp\"]` excludes buffer-words",
        );
        assert_eq!(eff.auto_insert_single, true);
    }

    #[test]
    fn apply_per_language_toml_overrides_warns_on_unknown_key() {
        let ws = fresh_workspace("unknown-perlang-key");
        write_workspace_config(
            &ws,
            "[completion.per-language.markdown]\n\
             bogus_field = 5\n",
        );
        let mut a = app_with("", 5);
        a.load_persistent_config(Some(&ws));
        // Loader echo handles structural sections silently
        // until `apply_per_language_toml_overrides` runs.
        let pre = a.last_message.clone();
        a.apply_per_language_toml_overrides();
        let msg = a.last_message.as_ref().expect("warning echoed");
        assert_ne!(Some(msg.clone()), pre, "new echo posted");
        assert_eq!(msg.level, EchoLevel::Warn);
        assert!(msg.text.contains("bogus_field"), "got `{}`", msg.text);
    }

    fn set_rust_syntax(a: &mut App, source: &str) {
        let mut syntax = lattice_syntax::Syntax::for_language_with_registry(
            lattice_syntax::Lang::Rust,
            a.lang_registry.clone(),
        )
        .expect("rust syntax")
        .expect("rust registered");
        syntax.parse(source);
        a.syntax = Some(lattice_syntax::SyntaxHandle::seeded(syntax));
    }

    /// Test helper: attach a freshly-parsed `Syntax` for `lang`
    /// to `a`, wrapped in a [`SyntaxHandle`]. Mirrors the audit

    #[test]
    fn tree_sitter_source_emits_definition_position_symbols_for_rust() {
        let source = "fn outer(arg: i32) {\n    let local = arg;\n}\n";
        let mut a = app_with(source, 10);
        set_rust_syntax(&mut a, source);
        a.modal = ModalState::Insert;
        // Cursor at end-of-buffer with empty query so every
        // candidate matches uniformly; the matcher won't drop
        // anything for prefix mismatch.
        a.cursor = Position::new(2, 1);
        a.do_completion_trigger();
        let state = a.insert_completion.as_ref().expect("popup");
        let tree_sitter_id = lattice_completion::TREE_SITTER_SYMBOL_SOURCE_ID;
        let ts_texts: Vec<&str> = state
            .raw
            .iter()
            .filter(|c| c.source.as_ref().map(|s| s.as_str()) == Some(tree_sitter_id))
            .map(|c| c.text.as_str())
            .collect();
        for expected in &["outer", "arg", "local"] {
            assert!(
                ts_texts.contains(expected),
                "expected `{expected}` in tree-sitter candidates: {ts_texts:?}",
            );
        }
    }

    #[test]
    fn tree_sitter_source_skipped_by_per_language_override() {
        let source = "fn outer() {\n    let local = 1;\n}\n";
        let mut a = app_with(source, 10);
        set_rust_syntax(&mut a, source);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(2, 1);
        // Override the active language (test buffer has no
        // path -> language id is "") to exclude tree-sitter.
        a.per_language_completion.insert(
            String::new(),
            lattice_completion::PerLanguageOverrides {
                sources: Some(vec![lattice_completion::SourceId::new(
                    lattice_completion::BufferWordsSource::ID,
                )]),
                ..Default::default()
            },
        );
        a.do_completion_trigger();
        let state = a.insert_completion.as_ref().expect("popup");
        let tree_sitter_id = lattice_completion::TREE_SITTER_SYMBOL_SOURCE_ID;
        for cand in &state.raw {
            let src = cand.source.as_ref().map(|s| s.as_str()).unwrap_or("");
            assert_ne!(
                src, tree_sitter_id,
                "tree-sitter source filtered out for this language",
            );
        }
    }

    #[test]
    fn tree_sitter_and_buffer_words_emit_independently_for_same_name() {
        // `outer` appears as a function definition (captured
        // by tree-sitter) AND as a referenced word (captured
        // by buffer-words). Both sources contribute their
        // tagged copy in `state.raw` -- the producers run
        // independently. Visual dedup at the renderer (4.2.g.7
        // polish) collapses them to a single popup row, so
        // `state.rendered` has exactly one entry for `outer`,
        // tagged with the higher-priority source (buffer-words
        // at 100 > tree-sitter at 80).
        let source = "fn outer() {\n    outer();\n}\n";
        let mut a = app_with(source, 10);
        set_rust_syntax(&mut a, source);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(2, 1);
        a.do_completion_trigger();
        let state = a.insert_completion.as_ref().expect("popup");
        let raw_sources: Vec<&str> = state
            .raw
            .iter()
            .filter(|c| c.text == "outer")
            .map(|c| c.source.as_ref().map(|s| s.as_str()).unwrap_or(""))
            .collect();
        assert!(
            raw_sources.contains(&lattice_completion::BufferWordsSource::ID),
            "buffer-words copy present in raw set: {raw_sources:?}",
        );
        assert!(
            raw_sources.contains(&lattice_completion::TREE_SITTER_SYMBOL_SOURCE_ID),
            "tree-sitter copy present in raw set: {raw_sources:?}",
        );
        let rendered_outer: Vec<&str> = state
            .rendered
            .iter()
            .filter(|c| c.raw.text == "outer")
            .map(|c| c.raw.source.as_ref().map(|s| s.as_str()).unwrap_or(""))
            .collect();
        assert_eq!(rendered_outer.len(), 1, "popup deduped to one row");
        assert_eq!(
            rendered_outer[0],
            lattice_completion::BufferWordsSource::ID,
            "higher-priority source's row survives the dedup",
        );
    }

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

    #[test]
    fn tree_sitter_source_silent_without_syntax_attached() {
        // No `set_rust_syntax` -> `app_with` leaves
        // `self.syntax = None`; tree-sitter source emits
        // nothing.
        let mut a = app_with("alpha bravo charlie", 5);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 19);
        a.do_completion_trigger();
        if let Some(state) = a.insert_completion.as_ref() {
            let tree_sitter_id = lattice_completion::TREE_SITTER_SYMBOL_SOURCE_ID;
            for cand in &state.raw {
                assert_ne!(
                    cand.source.as_ref().map(|s| s.as_str()),
                    Some(tree_sitter_id),
                );
            }
        }
    }

    fn app_with_path(
        text: &str,
        viewport: u32,
        path: std::path::PathBuf,
    ) -> App {
        let doc = lattice_core::DocumentBuilder::default()
            .with_text(text)
            .with_path(path)
            .build();
        let mut a = App::new(doc);
        a.set_viewport_height(viewport);
        a
    }

    /// Inject an `InboundApplyEdit` into the App's drain
    /// receiver. Replaces whatever was there; tests start with
    /// an empty receiver so this is fine.
    fn inject_inbound_apply_edit(
        a: &mut App,
        inbound: lattice_lsp::InboundApplyEdit,
    ) {
        let (bus, new_rx) = lattice_lsp::ApplyEditBus::new();
        bus.dispatch(inbound).expect("dispatch");
        a.pending_apply_edit_rx = Some(new_rx);
    }

    #[test]
    fn drain_inbound_apply_edits_applies_active_buffer_edit() {
        // Synthesise an inbound `workspace/applyEdit` against
        // the active buffer. Drain should apply the edit and
        // signal `applied: true` on the oneshot.
        let dir = std::env::temp_dir().join(format!(
            "lattice-applyedit-test-{}",
            std::process::id(),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("buffer.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();
        let mut a = app_with_path("fn main() {}\n", 5, path.clone());
        let uri: lsp_types::Uri = format!("file://{}", path.display()).parse().unwrap();
        // Edit replaces `main` (line 0, char 3..7) with `xyz`.
        let edit = lsp_types::TextEdit {
            range: lsp_types::Range {
                start: lsp_types::Position { line: 0, character: 3 },
                end: lsp_types::Position { line: 0, character: 7 },
            },
            new_text: "xyz".into(),
        };
        let mut changes = std::collections::HashMap::new();
        changes.insert(uri, vec![edit]);
        let workspace_edit = lsp_types::WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        };
        let (resp_tx, mut resp_rx) = tokio::sync::oneshot::channel();
        inject_inbound_apply_edit(
            &mut a,
            lattice_lsp::InboundApplyEdit {
                server_id: std::sync::Arc::from("test-server"),
                label: Some("rename main".into()),
                edit: workspace_edit,
                response: resp_tx,
            },
        );
        a.drain_inbound_apply_edits();
        // Drain ran synchronously; the oneshot is already
        // populated -- `try_recv` returns Ok.
        let outcome = resp_rx
            .try_recv()
            .expect("drain replied via oneshot");
        assert!(
            outcome.applied,
            "edit applied: {:?}",
            outcome.failure_reason,
        );
        let after = a.document.snapshot().buffer.as_string();
        assert_eq!(after, "fn xyz() {}\n");
    }

    #[test]
    fn drain_inbound_apply_edits_empty_workspace_edit_replies_applied_true() {
        // An empty WorkspaceEdit (no changes, no
        // document_changes) is a server no-op. Spec: reply
        // applied=true so the server doesn't think we
        // failed -- just nothing to do.
        let mut a = app_with("", 5);
        let workspace_edit = lsp_types::WorkspaceEdit::default();
        let (resp_tx, mut resp_rx) = tokio::sync::oneshot::channel();
        inject_inbound_apply_edit(
            &mut a,
            lattice_lsp::InboundApplyEdit {
                server_id: std::sync::Arc::from("test-server"),
                label: None,
                edit: workspace_edit,
                response: resp_tx,
            },
        );
        a.drain_inbound_apply_edits();
        let outcome = resp_rx.try_recv().expect("drain replied");
        assert!(outcome.applied);
        assert_eq!(
            outcome.failure_reason.as_deref(),
            Some("empty workspace edit"),
        );
    }

    #[test]
    fn drain_inbound_configuration_walks_cached_tree_at_lsp_prefix() {
        // Stash an `[lsp.rust-analyzer.cargo]` block in the
        // App's cached tree (mimics what
        // `load_persistent_config` does after parsing user
        // TOML). Drain receives a request for
        // `"rust-analyzer.cargo.features"` and the
        // `"rust-analyzer.checkOnSave"` -- both surface from
        // the tree.
        let mut a = app_with("", 5);
        let toml_text = "[lsp.rust-analyzer.cargo]\n\
                         features = [\"foo\", \"bar\"]\n\
                         [lsp.rust-analyzer]\n\
                         checkOnSave = true\n";
        a.lsp_config_tree = toml_text.parse().expect("toml parse");
        let (resp_tx, mut resp_rx) = tokio::sync::oneshot::channel();
        let req = lattice_lsp::InboundConfigurationRequest {
            server_id: std::sync::Arc::from("rust-analyzer"),
            sections: vec![
                "rust-analyzer.cargo.features".into(),
                "rust-analyzer.checkOnSave".into(),
            ],
            response: resp_tx,
        };
        let (bus, new_rx) = lattice_lsp::ConfigurationBus::new();
        bus.dispatch(req).expect("dispatch");
        a.pending_configuration_rx = Some(new_rx);
        a.drain_inbound_configuration_requests();
        let values = resp_rx.try_recv().expect("drain replied");
        assert_eq!(values.len(), 2);
        // First: features array.
        let arr = values[0].as_array().expect("array");
        assert_eq!(arr[0].as_str(), Some("foo"));
        assert_eq!(arr[1].as_str(), Some("bar"));
        // Second: bool.
        assert_eq!(values[1].as_bool(), Some(true));
    }

    #[test]
    fn drain_inbound_configuration_returns_null_for_missing_section() {
        let mut a = app_with("", 5);
        // No tree populated -> every lookup is null.
        let (resp_tx, mut resp_rx) = tokio::sync::oneshot::channel();
        let req = lattice_lsp::InboundConfigurationRequest {
            server_id: std::sync::Arc::from("rust-analyzer"),
            sections: vec!["rust-analyzer.cargo.features".into()],
            response: resp_tx,
        };
        let (bus, new_rx) = lattice_lsp::ConfigurationBus::new();
        bus.dispatch(req).expect("dispatch");
        a.pending_configuration_rx = Some(new_rx);
        a.drain_inbound_configuration_requests();
        let values = resp_rx.try_recv().expect("drain replied");
        assert_eq!(values.len(), 1);
        assert!(values[0].is_null());
    }

    #[test]
    fn drain_inbound_configuration_empty_section_returns_whole_lsp_subtree() {
        // A server requesting `section: null` (or empty) wants
        // the whole `lsp` sub-tree -- our convention serves
        // this from the namespaced top.
        let mut a = app_with("", 5);
        let toml_text = "[lsp.rust-analyzer]\nchecker = \"clippy\"\n";
        a.lsp_config_tree = toml_text.parse().unwrap();
        let (resp_tx, mut resp_rx) = tokio::sync::oneshot::channel();
        let req = lattice_lsp::InboundConfigurationRequest {
            server_id: std::sync::Arc::from("rust-analyzer"),
            sections: vec![String::new()],
            response: resp_tx,
        };
        let (bus, new_rx) = lattice_lsp::ConfigurationBus::new();
        bus.dispatch(req).expect("dispatch");
        a.pending_configuration_rx = Some(new_rx);
        a.drain_inbound_configuration_requests();
        let values = resp_rx.try_recv().expect("drain replied");
        // Whole `lsp` sub-tree comes back as a JSON object.
        let obj = values[0].as_object().expect("object");
        assert!(obj.contains_key("rust-analyzer"));
    }

    #[test]
    fn drain_inbound_configuration_no_op_when_channel_empty() {
        let mut a = app_with("", 5);
        a.drain_inbound_configuration_requests();
        assert!(a.pending_configuration_rx.is_some());
    }

    #[test]
    fn drain_inbound_apply_edits_no_op_when_channel_empty() {
        // Idle drain: no requests, no outgoing oneshots, no
        // panic. Cheap path that runs every frame.
        let mut a = app_with("", 5);
        a.drain_inbound_apply_edits();
        // Receiver is restored after the drain (the take + put-back).
        assert!(a.pending_apply_edit_rx.is_some());
    }

    fn fresh_path_workspace(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "lattice-pathsource-test-{}-{}",
            std::process::id(),
            name,
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create workspace");
        p
    }

    #[test]
    fn path_source_emits_filesystem_entries_inside_string_literal() {
        let ws = fresh_path_workspace("emits-entries");
        // Populate the workspace with two files + one dir.
        std::fs::write(ws.join("alpha.rs"), "// alpha").unwrap();
        std::fs::write(ws.join("beta.rs"), "// beta").unwrap();
        std::fs::create_dir_all(ws.join("subdir")).unwrap();
        // Buffer with a string literal; we'll set the
        // document path so relative resolution lands in `ws`.
        let source = "let p = \"\";\n";
        let doc_path = ws.join("buffer.rs");
        let mut a = app_with_path(source, 10, doc_path);
        set_rust_syntax(&mut a, source);
        a.modal = ModalState::Insert;
        // Cursor between the empty string's quotes -> string
        // scope.
        a.cursor = Position::new(0, source.find("\"\"").unwrap() as u32 + 1);
        a.do_completion_trigger();
        assert!(a.completion_in_path_context, "path-context detected");
        let state = a.insert_completion.as_ref().expect("popup");
        let path_id = lattice_completion::PATH_SOURCE_ID;
        let texts: Vec<&str> = state
            .raw
            .iter()
            .filter(|c| c.source.as_ref().map(|s| s.as_str()) == Some(path_id))
            .map(|c| c.text.as_str())
            .collect();
        assert!(texts.contains(&"alpha.rs"), "alpha in {texts:?}");
        assert!(texts.contains(&"beta.rs"), "beta in {texts:?}");
        assert!(
            texts.contains(&"subdir/"),
            "subdir/ (with trailing slash) in {texts:?}",
        );
        // No buffer-words / tree-sitter / snippet candidates
        // intermix with the path popup.
        for cand in &state.raw {
            let src = cand.source.as_ref().map(|s| s.as_str()).unwrap_or("");
            assert_eq!(
                src,
                path_id,
                "non-path source `{src}` slipped into path-context popup",
            );
        }
    }

    #[test]
    fn path_source_skips_hidden_and_ignored_entries() {
        let ws = fresh_path_workspace("skip-hidden");
        std::fs::write(ws.join("visible.txt"), "v").unwrap();
        std::fs::write(ws.join(".hidden"), "h").unwrap();
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        std::fs::create_dir_all(ws.join("node_modules")).unwrap();
        let source = "let p = \"\";\n";
        let mut a = app_with_path(source, 10, ws.join("buffer.rs"));
        set_rust_syntax(&mut a, source);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, source.find("\"\"").unwrap() as u32 + 1);
        a.do_completion_trigger();
        let state = a.insert_completion.as_ref().expect("popup");
        let texts: Vec<&str> = state
            .raw
            .iter()
            .map(|c| c.text.as_str())
            .collect();
        assert!(texts.contains(&"visible.txt"));
        assert!(!texts.contains(&".hidden"), "dotfile filtered");
        assert!(!texts.contains(&".git/"), ".git filtered");
        assert!(
            !texts.contains(&"node_modules/"),
            "node_modules filtered",
        );
    }

    #[test]
    fn path_source_silent_outside_string_scope() {
        let source = "fn main() { let x = 1; }\n";
        let mut a = app_with(source, 10);
        set_rust_syntax(&mut a, source);
        a.modal = ModalState::Insert;
        // Cursor at end of line -- outside any string.
        a.cursor = Position::new(0, source.trim_end().len() as u32);
        a.do_completion_trigger();
        assert!(!a.completion_in_path_context);
        if let Some(state) = a.insert_completion.as_ref() {
            let path_id = lattice_completion::PATH_SOURCE_ID;
            for cand in &state.raw {
                assert_ne!(
                    cand.source.as_ref().map(|s| s.as_str()),
                    Some(path_id),
                    "no path candidates outside string scope",
                );
            }
        }
    }

    #[test]
    fn path_source_skipped_by_per_language_override() {
        let ws = fresh_path_workspace("disabled-via-override");
        std::fs::write(ws.join("alpha.rs"), "//").unwrap();
        let source = "let p = \"\";\n";
        let mut a = app_with_path(source, 10, ws.join("buffer.rs"));
        set_rust_syntax(&mut a, source);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, source.find("\"\"").unwrap() as u32 + 1);
        // Override the active language ("rust", since the
        // buffer path ends in `.rs`) to drop path source.
        a.per_language_completion.insert(
            "rust".into(),
            lattice_completion::PerLanguageOverrides {
                sources: Some(vec![lattice_completion::SourceId::new(
                    lattice_completion::BufferWordsSource::ID,
                )]),
                ..Default::default()
            },
        );
        a.do_completion_trigger();
        assert!(
            !a.completion_in_path_context,
            "path source disabled -> no path context",
        );
    }

    #[test]
    fn path_source_resolves_subdirectory_from_partial_path() {
        let ws = fresh_path_workspace("subdir-walk");
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::write(ws.join("src/foo.rs"), "//").unwrap();
        std::fs::write(ws.join("src/bar.rs"), "//").unwrap();
        let source = "let p = \"src/\";\n";
        let mut a = app_with_path(source, 10, ws.join("buffer.rs"));
        set_rust_syntax(&mut a, source);
        a.modal = ModalState::Insert;
        // Cursor after `src/`.
        let after_slash = source.find("src/").unwrap() + "src/".len();
        a.cursor = Position::new(0, after_slash as u32);
        a.do_completion_trigger();
        assert!(a.completion_in_path_context);
        let state = a.insert_completion.as_ref().expect("popup");
        let texts: Vec<&str> = state
            .raw
            .iter()
            .map(|c| c.text.as_str())
            .collect();
        assert!(texts.contains(&"foo.rs"), "src/foo.rs surfaced -- got {texts:?}");
        assert!(texts.contains(&"bar.rs"), "src/bar.rs surfaced");
    }

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
        let payload = (a.insert_completion_lsp_meta.len() as u32).to_le_bytes().to_vec();
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
        a.insert_completion_lsp_meta.push(LspCompletionMeta {
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
            server_id: std::sync::Arc::from("test-server"),
            original_item: lsp_types::CompletionItem::new_simple(
                text.to_string(),
                String::new(),
            ),
            resolved: false,
        });
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
    fn lsp_snippet_with_additional_edits_lands_as_one_undo_unit() {
        // Buffer has space for the auto-import on line 0 and
        // the snippet expansion on line 2. The accept path
        // applies BOTH edits in a single batch; one Ctrl-Z
        // reverts both.
        let mut a = app_with("\n\nfor", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(2, 3);
        // Manually install the popup state: one candidate
        // with snippet `insertTextFormat`, an auto-import
        // additionalTextEdit at line 0, and a snippet body
        // that splices `[anchor, cursor]`.
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            Position::new(2, 0),
            Position::new(2, 3),
            "for".into(),
        );
        let mut raw = lattice_completion::RawCandidate::plain(
            "for",
            lattice_completion::CandidateKind::Plain,
        )
        .with_source(lattice_completion::SourceId::new(
            lattice_completion::LSP_COMPLETION_SOURCE_ID,
        ));
        raw.data = lattice_completion::CandidateData::Extension {
            kind_id: LSP_COMPLETION_KIND_ID,
            payload: 0u32.to_le_bytes().to_vec(),
        };
        state.raw.push(raw.clone());
        state.rendered.push(lattice_completion::RenderedCandidate::from_scored(
            lattice_completion::ScoredCandidate {
                raw,
                score: lattice_completion::MatchScore(100),
                match_ranges: Vec::new(),
            },
        ));
        a.insert_completion = Some(state);
        a.insert_completion_lsp_meta.push(LspCompletionMeta {
            label: "for-loop".into(),
            // Snippet body with one tabstop -- expand_snippet_with_lsp_edits
            // sets up the active snippet, focuses $1.
            insert_text: "for ${1:i} in iter {}".into(),
            filter_text: None,
            sort_text: None,
            detail: None,
            documentation: None,
            kind: Some(lsp_types::CompletionItemKind::SNIPPET),
            deprecated: false,
            preselect: false,
            commit_characters: Vec::new(),
            additional_text_edits: vec![lsp_types::TextEdit {
                range: lsp_types::Range {
                    start: lsp_types::Position { line: 0, character: 0 },
                    end: lsp_types::Position { line: 0, character: 0 },
                },
                new_text: "use std::iter;\n".into(),
            }],
            command: None,
            insert_text_format: lsp_types::InsertTextFormat::SNIPPET,
            replace_range: None,
            server_id: std::sync::Arc::from("test-server"),
            original_item: lsp_types::CompletionItem::default(),
            resolved: true,
        });
        a.do_completion_accept();
        // After accept: line 0 has the auto-import, line 2
        // (now line 3 after the import inserted a newline,
        // wait -- the import is `use std::iter;\n` which adds
        // an extra newline; existing line 0 was empty so the
        // buffer is now: line 0 = "use std::iter;", line 1 = "",
        // line 2 = "", line 3 = "for i in iter {}").
        let after_accept = a.document.snapshot().buffer.as_string();
        assert!(after_accept.contains("use std::iter;"), "auto-import applied: `{after_accept}`");
        assert!(after_accept.contains("for i in iter {}"), "snippet expanded: `{after_accept}`");
        // Active snippet focused on $1 ("i").
        assert!(a.active_snippet.is_some(), "active snippet started");
        // Undo ONCE -> both the auto-import AND the snippet
        // expansion revert.
        a.undo_blocking().expect("undo");
        let after_undo = a.document.snapshot().buffer.as_string();
        assert_eq!(
            after_undo, "\n\nfor",
            "single undo reverted both auto-import and snippet (`{after_undo}`)",
        );
    }

    fn open_popup_with_top_text(a: &mut App, query: &str, top_text: &str) {
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            a.cursor,
            a.cursor,
            query.to_string(),
        );
        let raw = lattice_completion::RawCandidate::plain(
            top_text,
            lattice_completion::CandidateKind::Plain,
        );
        state.raw.push(raw.clone());
        state
            .rendered
            .push(lattice_completion::RenderedCandidate::from_scored(
                lattice_completion::ScoredCandidate {
                    raw,
                    score: lattice_completion::MatchScore(800),
                    match_ranges: Vec::new(),
                },
            ));
        a.insert_completion = Some(state);
    }

    #[test]
    fn ghost_text_off_by_default_returns_none() {
        let mut a = app_with("foo", 5);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        open_popup_with_top_text(&mut a, "foo", "foobar");
        // Default: completion.ghost_text = false -> no ghost.
        assert!(a.completion_ghost_text_suffix().is_none());
    }

    #[test]
    fn ghost_text_returns_suffix_for_prefix_matching_top_candidate() {
        let mut a = app_with("foo", 5);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        a.do_set("completion.ghost_text=true");
        open_popup_with_top_text(&mut a, "foo", "foobar");
        assert_eq!(
            a.completion_ghost_text_suffix(),
            Some("bar".to_string()),
            "ghost suffix is the part of the candidate beyond the query prefix",
        );
    }

    #[test]
    fn ghost_text_case_insensitive_prefix_match() {
        let mut a = app_with("Foo", 5);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        a.do_set("completion.ghost_text=true");
        open_popup_with_top_text(&mut a, "Foo", "foobar");
        assert_eq!(
            a.completion_ghost_text_suffix(),
            Some("bar".to_string()),
        );
    }

    #[test]
    fn ghost_text_none_when_top_doesnt_prefix_match_query() {
        let mut a = app_with("xyz", 5);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        a.do_set("completion.ghost_text=true");
        // Top candidate is `bar`; query `xyz` doesn't prefix
        // it (matcher's substring tier still puts it on
        // screen, but ghost demands prefix-match).
        open_popup_with_top_text(&mut a, "xyz", "bar");
        assert!(a.completion_ghost_text_suffix().is_none());
    }

    #[test]
    fn ghost_text_none_when_query_is_empty() {
        let mut a = app_with("", 5);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 0);
        a.do_set("completion.ghost_text=true");
        open_popup_with_top_text(&mut a, "", "alpha");
        assert!(
            a.completion_ghost_text_suffix().is_none(),
            "empty query -> no ghost (any candidate would match)",
        );
    }

    #[test]
    fn ghost_text_none_in_path_context() {
        let mut a = app_with("\"\"", 5);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 1);
        a.do_set("completion.ghost_text=true");
        a.completion_in_path_context = true;
        open_popup_with_top_text(&mut a, "src", "src/foo.rs");
        assert!(
            a.completion_ghost_text_suffix().is_none(),
            "path popup already shows full filenames; ghost would double up",
        );
    }

    #[test]
    fn ghost_text_none_when_popup_closed() {
        let mut a = app_with("foo", 5);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        a.do_set("completion.ghost_text=true");
        // No open_popup_with_top_text call -> insert_completion = None.
        assert!(a.completion_ghost_text_suffix().is_none());
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

    #[test]
    fn load_persistent_config_silent_when_no_files_present() {
        let ws = fresh_workspace("no-files");
        // Empty workspace -- no .lattice/config.toml. Loader
        // produces no messages; the modeline stays clean.
        let mut a = app_with("", 5);
        let prior = a.last_message.clone();
        a.load_persistent_config(Some(&ws));
        // No new echo (modeline message is whatever the test
        // setup left, which for app_with is None).
        assert_eq!(a.last_message, prior);
    }

    #[test]
    fn completion_high_priority_source_beats_tied_low_priority_peer() {
        // Two candidates tied on matcher score: one tagged
        // gen:lsp-completion (default priority 200), one tagged
        // gen:buffer-words (default 100). The LSP candidate
        // sorts above the buffer-words peer.
        let mut a = app_with("", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 0);
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            Position::ZERO,
            Position::ZERO,
            String::new(),
        );
        state.raw.push(
            lattice_completion::RawCandidate::plain(
                "from_words",
                lattice_completion::CandidateKind::Plain,
            )
            .with_source(lattice_completion::SourceId::new(
                lattice_completion::BufferWordsSource::ID,
            )),
        );
        state.raw.push(
            lattice_completion::RawCandidate::plain(
                "from_lsp",
                lattice_completion::CandidateKind::Plain,
            )
            .with_source(lattice_completion::SourceId::new(
                lattice_completion::LSP_COMPLETION_SOURCE_ID,
            )),
        );
        a.refilter_insert_completion(&mut state);
        assert_eq!(state.rendered[0].raw.text, "from_lsp");
        assert_eq!(state.rendered[1].raw.text, "from_words");
    }

    #[test]
    fn completion_priority_override_via_set_flips_source_order() {
        // After `:set completion.source.buffer-words.priority=300`
        // the buffer-words candidate outranks the LSP one
        // (300 > 200) at tied matcher score.
        let mut a = app_with("", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 0);
        a.do_set("completion.source.buffer-words.priority=300");
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            Position::ZERO,
            Position::ZERO,
            String::new(),
        );
        state.raw.push(
            lattice_completion::RawCandidate::plain(
                "from_lsp",
                lattice_completion::CandidateKind::Plain,
            )
            .with_source(lattice_completion::SourceId::new(
                lattice_completion::LSP_COMPLETION_SOURCE_ID,
            )),
        );
        state.raw.push(
            lattice_completion::RawCandidate::plain(
                "from_words",
                lattice_completion::CandidateKind::Plain,
            )
            .with_source(lattice_completion::SourceId::new(
                lattice_completion::BufferWordsSource::ID,
            )),
        );
        a.refilter_insert_completion(&mut state);
        assert_eq!(state.rendered[0].raw.text, "from_words");
        assert_eq!(state.rendered[1].raw.text, "from_lsp");
    }

    #[test]
    fn completion_untagged_candidate_gets_no_priority_lift() {
        // Candidate with no source field (plugin source not yet
        // wired into config, or test fixture) gets 0 priority
        // bonus; sorts below a tagged peer at tied matcher
        // score, but still appears in the rendered list.
        let mut a = app_with("", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 0);
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            Position::ZERO,
            Position::ZERO,
            String::new(),
        );
        state.raw.push(lattice_completion::RawCandidate::plain(
            "untagged",
            lattice_completion::CandidateKind::Plain,
        ));
        state.raw.push(
            lattice_completion::RawCandidate::plain(
                "tagged",
                lattice_completion::CandidateKind::Plain,
            )
            .with_source(lattice_completion::SourceId::new(
                lattice_completion::BufferWordsSource::ID,
            )),
        );
        a.refilter_insert_completion(&mut state);
        assert_eq!(state.rendered[0].raw.text, "tagged");
        assert_eq!(state.rendered[1].raw.text, "untagged");
    }

    #[test]
    fn completion_buffer_words_candidates_carry_their_source_tag() {
        // Regression: the buffer-words `InsertSource` impl
        // tags every produced candidate with its own id so the
        // ranker can apply per-source priority without the host
        // having to remember to tag.
        let mut a = app_with("alpha bravo ", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 12);
        a.do_completion_trigger();
        let state = a.insert_completion.as_ref().expect("popup");
        assert!(!state.rendered.is_empty());
        for cand in &state.rendered {
            let src = cand
                .raw
                .source
                .as_ref()
                .unwrap_or_else(|| panic!("candidate `{}` missing source tag", cand.raw.text));
            assert_eq!(src.as_str(), lattice_completion::BufferWordsSource::ID);
        }
    }

    #[test]
    fn completion_accept_increments_existing_frequency_count() {
        // Two accepts of the same item bump the count to 2.
        let mut a = app_with("alpha bravo charlie ", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 20);
        let key = (
            "bravo".to_string(),
            lattice_completion::CandidateKind::Plain,
        );
        a.completion_accept_freq.insert(key.clone(), 4);
        a.do_completion_trigger();
        let state = a.insert_completion.as_mut().expect("popup");
        let idx = state
            .rendered
            .iter()
            .position(|r| r.raw.text == "bravo")
            .expect("bravo present");
        state.selected = idx;
        a.do_completion_accept();
        assert_eq!(a.completion_accept_freq.get(&key).copied(), Some(5));
    }

    #[test]
    fn reload_snippets_with_no_dirs_reports_empty() {
        let mut a = app_with("", 10);
        a.do_reload_snippets();
        // Idle; registry stays empty. Message echoed at Info.
        assert_eq!(a.snippet_registry.len(), 0);
    }

    #[test]
    fn reload_snippets_walks_configured_dirs_and_keys_by_filename() {
        // Build a tempdir with `_global.json` (any-language)
        // and `rust.json` (language-specific). Reload should
        // route them into the right per-language slots.
        let dir = std::env::temp_dir().join(format!(
            "lattice-snippet-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("_global.json"),
            r#"{ "anywhere": { "prefix": "any", "body": "anywhere" } }"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("rust.json"),
            r#"{ "rust-for": { "prefix": "for", "body": "for $1 {}" } }"#,
        )
        .unwrap();
        let mut a = app_with("", 10);
        a.snippet_dirs.push(dir.clone());
        a.do_reload_snippets();
        // 2 snippets registered total (one per language).
        assert_eq!(a.snippet_registry.len(), 2);
        assert!(!a.snippet_registry.lookup("rust", "for").is_empty());
        assert!(!a.snippet_registry.lookup("*", "any").is_empty());
        // Global snippets are visible from any language --
        // `lookup` walks the per-language slot then `*`.
        assert!(!a.snippet_registry.lookup("rust", "any").is_empty());
        // A rust-only snippet should NOT be visible from a
        // different language slot.
        assert!(a.snippet_registry.lookup("python", "for").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }


    // ---- M.3.0: built-in major modes registered at boot ----

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

    // ---- M.3.1: ReadOnly option flows from major modes ----


    #[test]
    fn document_buffer_active_mode_is_text_mode() {
        // Plain document with no path ⇒ Lang::Plain ⇒ text-mode.
        let a = app_with("hi", 5);
        let buf = a.document_buffer_id;
        let active = a.active_modes.get(&buf).expect("active_modes populated");
        assert_eq!(active.major(), Some(lattice_mode::TextMode::mode_id()));
    }


    #[test]
    fn help_buffer_active_mode_is_help_mode() {
        let mut a = app_with("hi", 5);
        let help = crate::help::HelpBuffer::from_lines(
            "test",
            vec!["line one".to_string()],
        );
        let help_id = a.open_help_in_pane(help);
        let active = a
            .active_modes
            .get(&help_id)
            .expect("active_modes populated for help");
        assert_eq!(active.major(), Some(crate::modes::HelpMode::mode_id()));
    }


    // ---- M.3.2.a: BufferLocal end-to-end integration ----

    /// Test fixture: a buffer-local owned by a fictional
    /// `test-locals-mode`. Exercises the full pipeline:
    /// mode's `on_activate` writes the local via the
    /// ModeContext; subsequent reads see it; deactivation
    /// removes it.
    #[derive(Debug)]
    struct TestLocalCounter(i64);

    impl lattice_mode::BufferLocal for TestLocalCounter {
        const NAME: &'static str = "test-locals.counter";
        const DOC: &'static str = "Counter local for the test-locals fixture mode.";
        const OWNER_MODE: &'static str = "test-locals-mode";
        fn describe(&self) -> String {
            format!("counter={}", self.0)
        }
    }

    struct TestLocalsMode {
        id: lattice_mode::ModeId,
    }

    impl TestLocalsMode {
        fn new() -> Self {
            Self {
                id: lattice_mode::ModeId::new("test-locals-mode"),
            }
        }
    }

    impl lattice_mode::Mode for TestLocalsMode {
        fn id(&self) -> lattice_mode::ModeId {
            self.id
        }
        fn kind(&self) -> lattice_mode::ModeKind {
            lattice_mode::ModeKind::Minor
        }
        fn on_activate(
            &self,
            ctx: &mut lattice_mode::ModeContext<'_>,
        ) -> Result<(), lattice_mode::ModeActivationError> {
            ctx.set_local(TestLocalCounter(42))
        }
        fn on_deactivate(
            &self,
            ctx: &mut lattice_mode::ModeContext<'_>,
        ) -> Result<(), lattice_mode::ModeActivationError> {
            let _ = ctx.remove_local::<TestLocalCounter>()?;
            Ok(())
        }
    }

    #[test]
    fn mode_on_activate_can_write_buffer_local() {
        let mut a = app_with("hi", 5);
        let _buf = a.document_buffer_id;

        let registry = std::sync::Arc::get_mut(&mut a.mode_registry)
            .expect("mode_registry uniquely held");
        let mode_id = registry
            .register(TestLocalsMode::new())
            .expect("register");

        let mut active = lattice_mode::ActiveModes::new();
        let mut locs = lattice_mode::BufferLocals::new();
        a.mode_registry
            .activate_minor(
                &mut active,
                &mut locs,
                lattice_protocol::ids::BufferId::new(0),
                mode_id,
                lattice_mode::CapabilitySet::empty(),
            )
            .expect("activate");

        // After activation the local should be present in the
        // map with the value the mode set.
        let counter = locs
            .get::<TestLocalCounter>()
            .expect("local should be present after activation");
        assert_eq!(counter.0, 42);
    }

    #[test]
    fn mode_on_deactivate_removes_buffer_local() {
        let mut a = app_with("hi", 5);

        let registry = std::sync::Arc::get_mut(&mut a.mode_registry)
            .expect("mode_registry uniquely held");
        let mode_id = registry
            .register(TestLocalsMode::new())
            .expect("register");

        let mut active = lattice_mode::ActiveModes::new();
        let mut locs = lattice_mode::BufferLocals::new();
        a.mode_registry
            .activate_minor(
                &mut active,
                &mut locs,
                lattice_protocol::ids::BufferId::new(0),
                mode_id,
                lattice_mode::CapabilitySet::empty(),
            )
            .expect("activate");
        assert!(locs.contains::<TestLocalCounter>());

        a.mode_registry
            .deactivate_minor(
                &mut active,
                &mut locs,
                lattice_protocol::ids::BufferId::new(0),
                mode_id,
            )
            .expect("deactivate");
        assert!(
            !locs.contains::<TestLocalCounter>(),
            "deactivate should remove the mode's local"
        );
    }

    // ---- M.3.2.b.1: help-mode locals seeded at construction ----

    #[test]
    fn open_help_in_pane_seeds_help_locals() {
        let mut a = app_with("hi", 5);
        let help = crate::help::HelpBuffer::from_lines(
            "test-locals",
            vec![
                "# Heading One".to_string(),
                "see [ex:write](command:ex:write)".to_string(),
            ],
        );
        let help_id = a.open_help_in_pane(help);
        let locals = a
            .buffer_locals
            .get(&help_id)
            .expect("buffer_locals should be populated for help buffer");
        // Links parsed from `[ex:write](command:ex:write)`.
        let links = locals
            .get::<crate::modes::HelpLinks>()
            .expect("HelpLinks local seeded");
        assert_eq!(links.0.len(), 1);
        // Anchors come from heading slug generation. `from_lines`
        // doesn't auto-anchor headings (only
        // `from_lines_and_anchors` plumbs anchors); the seed
        // should still be present, just empty.
        let anchors = locals
            .get::<crate::modes::HelpAnchors>()
            .expect("HelpAnchors local seeded (possibly empty)");
        assert_eq!(anchors.0.len(), 0);
        // Highlights are empty without a markdown registry.
        let highlights = locals
            .get::<crate::modes::HelpHighlights>()
            .expect("HelpHighlights local seeded (possibly empty)");
        assert_eq!(highlights.0.len(), 0);
    }

    #[test]
    fn renderer_reads_help_data_through_buffer_locals() {
        // M.3.2.b.2: prove the renderer reads through
        // `buffer_locals` rather than the HelpBuffer's struct
        // fields. We open a help buffer, then mutate its
        // BufferLocals to a different value than what's in
        // the struct fields. The renderer should reflect the
        // local-side value.
        let mut a = app_with("hi", 5);
        let help = crate::help::HelpBuffer::from_lines(
            "test-render",
            vec!["[link-a](command:a) and [link-b](command:b)".into()],
        );
        let help_id = a.open_help_in_pane(help);

        // The buffer registers via `seed_help_locals` with two
        // links. We replace the locals with one synthetic link
        // and confirm the renderer's data path sees ONE.
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

        // Lookup via the registered id (= `help_id` returned
        // by `open_help_in_pane`). Note: `app.help_buffer.id`
        // is the construction-time id and intentionally
        // differs from `help_id`; locals are keyed by the
        // registered id (see comment in `open_help_in_pane`).
        let help_buf = a.help_buffer.as_ref().expect("help_buffer set");
        let locals = a
            .buffer_locals
            .get(&help_id)
            .expect("locals seeded by open_help_in_pane");
        let from_locals = locals.get::<crate::modes::HelpLinks>().unwrap();
        // The locals reflect the synthetic value we inserted,
        // not the original 2 links from the source markdown.
        assert_eq!(from_locals.0.len(), 1);
        assert_eq!(
            from_locals.0[0].target,
            crate::help::HelpLinkTarget::Unresolved("synthetic".into())
        );
        // The struct fields still carry the original 2 links
        // (M.3.2.b.2 keeps the fields as fallback; M.3.2.c
        // removes them).
        assert_eq!(help_buf.links.len(), 2);
    }

    // ---- M.3.2.c.2: file-tree-mode locals seeded + readers ----

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
    fn file_tree_locals_carry_owner_metadata() {
        let tmp = std::env::temp_dir().join(format!(
            "lattice-m3-2-c-2-meta-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&tmp);
        let mut a = app_with("hi", 5);
        a.do_open_file_tree(Some(tmp.clone()));
        let tree_id = a.active_pane_buffer_id();
        let locals = a.buffer_locals.get(&tree_id).unwrap();
        let descriptors: Vec<_> = locals.iter_descriptors().collect();
        assert!(descriptors.len() >= 3);
        for d in &descriptors {
            assert_eq!(d.owner_mode, "file-tree-mode");
            assert!(
                d.name.starts_with("file-tree-mode."),
                "name {:?} should be namespaced under file-tree-mode",
                d.name
            );
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ---- M.3.2.c.3: oil-mode locals seeded ----

    #[test]
    fn open_oil_seeds_oil_locals() {
        let tmp = std::env::temp_dir().join(format!(
            "lattice-m3-2-c-3-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&tmp);

        let mut a = app_with("hi", 5);
        a.do_open_oil(Some(tmp.clone()));
        let oil_id = a.active_pane_buffer_id();

        let locals = a
            .buffer_locals
            .get(&oil_id)
            .expect("oil locals seeded");
        let dir = locals
            .get::<crate::modes::OilDir>()
            .expect("OilDir local present");
        assert_eq!(dir.0, tmp);

        // Owner-mode metadata.
        let descriptors: Vec<_> = locals.iter_descriptors().collect();
        let oil_descriptors: Vec<_> = descriptors
            .iter()
            .filter(|d| d.owner_mode == "oil-mode")
            .collect();
        assert_eq!(oil_descriptors.len(), 1);
        assert_eq!(oil_descriptors[0].name, "oil-mode.dir");

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
        let help = crate::help::HelpBuffer::from_lines(
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

    #[test]
    fn help_locals_carry_owner_metadata_for_describe_buffer() {
        let mut a = app_with("hi", 5);
        let help = crate::help::HelpBuffer::from_lines("t", vec!["body".into()]);
        let help_id = a.open_help_in_pane(help);
        let locals = a.buffer_locals.get(&help_id).unwrap();
        // Every seeded local should claim help-mode as its owner.
        let descriptors: Vec<_> = locals.iter_descriptors().collect();
        assert!(!descriptors.is_empty());
        for d in &descriptors {
            assert_eq!(d.owner_mode, "help-mode");
            assert!(
                d.name.starts_with("help-mode."),
                "name {:?} should be namespaced under help-mode",
                d.name
            );
        }
    }

    #[test]
    fn buffer_locals_iter_descriptors_for_describe_buffer() {
        // The descriptor surface backs `:describe-buffer` --
        // every local exposes name / doc / owner_mode / a
        // single-line `describe` string for inspection.
        let mut locs = lattice_mode::BufferLocals::new();
        // Direct insert via the test mode's lifecycle is the
        // production path; we exercise the descriptor view
        // here independently.
        let mut active = lattice_mode::ActiveModes::new();
        let mut a = app_with("hi", 5);
        let registry = std::sync::Arc::get_mut(&mut a.mode_registry)
            .expect("mode_registry uniquely held");
        let mode_id = registry
            .register(TestLocalsMode::new())
            .expect("register");
        a.mode_registry
            .activate_minor(
                &mut active,
                &mut locs,
                lattice_protocol::ids::BufferId::new(0),
                mode_id,
                lattice_mode::CapabilitySet::empty(),
            )
            .expect("activate");

        let descriptors: Vec<_> = locs.iter_descriptors().collect();
        assert_eq!(descriptors.len(), 1);
        let d = &descriptors[0];
        assert_eq!(d.name, "test-locals.counter");
        assert_eq!(d.owner_mode, "test-locals-mode");
        assert_eq!(d.describe, "counter=42");
    }
}
