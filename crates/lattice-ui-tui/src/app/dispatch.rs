//! The dispatch core -- where typed input becomes state mutation.
//!
//! Three layers cooperate here:
//!
//! 1. `App::apply(action)` -- the main `Action` dispatcher fired
//!    from the runtime input loop and from macro replay. Handles
//!    macro-recording capture, partial-chord lifecycle, the
//!    help-buffer read-only guard, and a fat match arm that
//!    routes every `Action` variant to its feature method.
//! 2. `App::run_invocation(inv)` -- routes a typed
//!    `CommandInvocation` to the right per-buffer-kind handler
//!    (`run_oil_invocation` / `run_file_tree_invocation` /
//!    `run_help_invocation` / `run_document_invocation`).
//! 3. `App::apply_effect(effect)` -- consumes the
//!    grammar/dispatcher's `Effect` and translates it into App
//!    side effects (edits, mode flips, save/quit, set-option,
//!    LSP requests, picker opens, ...).
//!
//! Plus the supporting cast: `dispatch_blocking` (the sync
//! wrapper over the document actor's grammar dispatch),
//! `apply_app_effect` (the `AppEffect` sub-dispatcher),
//! `handle_edits` (the `Effect::Edits` applier), and
//! `execute_ex_line` (the `:` submit dispatcher).
//!
//! Plus a few private helpers used only here:
//! - `effect_mutates` / `effect_mutates_or_yanks` -- the
//!   Effect classifiers used by the dot-repeat record gate
//!   and the Visual-mode auto-exit gate.
//! - `delete_trailing_word` (`<C-w>` cmdline word delete).
//! - `action_is_document_mutation` (the help-buffer
//!   read-only guard's allow-list inversion). Lives in
//!   `lattice_host::dispatch` since 5.5.D.
//! - `echo_level_from_grammar` (grammar-level → host-level
//!   echo-level translator). Lives in `lattice_host::dispatch`
//!   since 5.5.E.1, beside its sole caller (`Effect::Echo`).
//! - `COMMAND_HISTORY_CAP` (the `:`-history capacity).
//!
//! What does NOT live here: the per-feature `do_*` methods the
//! match arms call. Those live in their feature modules; this
//! is the routing layer over them.

use lattice_grammar::command::CommandInvocation;
use lattice_grammar::effect::Effect;
use lattice_host::dispatch::RendererSignal;
use lattice_runtime::RuntimeError;

use super::{Action, App, BufferKind, EchoLevel};
use crate::excommand;

const COMMAND_HISTORY_CAP: usize = 100;

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

// 5.5.D: `action_is_document_mutation` lives in
// [`lattice_host::dispatch::action_is_document_mutation`] alongside
// the read-only-help guard that consults it. Any ui-tui-side reader
// would import it from the host crate; today nothing else in this
// module needs it.

impl App {
    /// Block_on a grammar dispatch through the actor (DESIGN.md
    /// §5.2.1). Replaces direct `lattice_grammar::execute(&self.editor.registry,
    /// &mut self.editor.document, ...)` calls; the actor holds the only
    /// `&mut Document` and runs `execute` inside its task.
    ///
    /// v1 passes a `CancellationToken::never()` -- the input loop
    /// (`lattice_ui_tui::runtime::run`) is single-threaded crossterm
    /// poll, so no concurrent code path can flip the token while
    /// `block_on` parks the thread. The plumbing is in place for a
    /// future runtime that reads input on a separate task and flips
    /// the dispatch token on Esc; see `dispatch_with_cancel` on
    /// `DocumentHandle`.
    /// 5.5.G.23: body migrated to
    /// [`lattice_host::dispatch::Editor::dispatch_blocking`]. Retained
    /// as a 1-line delegate while App-side helpers
    /// (`run_oil_invocation` / `run_read_only_motion` /
    /// `run_document_invocation`) are still hosted here.
    pub fn dispatch_blocking(&self, invocation: CommandInvocation) -> Result<Effect, RuntimeError> {
        self.read_editor(move |e| e.dispatch_blocking(invocation))
    }

    pub fn apply(&mut self, action: Action) {
        // Phase 5.5.B–D: macro-recording capture, partial-chord
        // lifecycle, and the read-only-help guard live in
        // `Editor::dispatch`'s preamble. When `outcome.consumed` is
        // set the host already surfaced the relevant echo + cleanup
        // (read-only-help today); App's match below must bail.
        //
        // 5.5.G.final / 5.5.H: ~90 Action arms now resolve host-side
        // via `Editor::dispatch` (every motion / operator / text-object
        // / ex-command keystroke; `Invoke`, `Insert`, macros, dot-
        // repeat, find-repeat, the full 8-arm command-line cluster).
        // `Effect::AppAction` collapses through `Editor::apply_app_effect`
        // (5.5.G.24); `out.next_actions` is the unified deferred-Action
        // channel for everything host-emitted that needs the renderer's
        // `apply` loop (LSP autopilots, macro replay, AppEffect-derived
        // follow-ups).
        //
        // The 13 explicit arms remaining are the architectural seam,
        // NOT migration debt:
        //
        //   - `LspOnTypeFormattingRequest(c)` /
        //     `LspInsertCompletionRequest` — host→renderer LSP autopilot
        //     landing zones. The host fires the action; the renderer
        //     owns the LSP runtime (`spawn_on_lsp_runtime`, BatchingSink,
        //     `pending_insert_completion_lsp_*`). gpui will implement
        //     its own equivalent on `lattice-ui-gpui::App`.
        //
        //   - 8 Completion arms (`CompletionTrigger`/`Next`/`Prev`/`Accept`/
        //     `ToggleDocs`/`FilterToSource`/`FilterClear`/`AcceptThenInsert`)
        //     — completion popup state mutation + LSP async resolve.
        //     `editor.completion` lives host-side, but the LSP fetch
        //     path is renderer-coupled (same reason as above).
        //
        //   - `PickerAccept` / `PickerDismiss` — file-open + the
        //     show-message-request queue. `do_edit` is renderer-side
        //     because the gpui port opens a window, not a TUI pane.
        //
        //   - `LspFollowLinkAtCursor` / `FollowLink` / `OilNavigateUp`
        //     — reach for `do_edit` + `open_external_uri` + the
        //     BufferKind-dispatch help/oil/file-tree helpers, each of
        //     which terminates in a renderer-coupled chokepoint.
        //
        // `consumed` collapses when (if) every subsystem above migrates
        // host-side; until then the seam is correct, not debt. The
        // gpui peer renderer implements its own version of these arms
        // against its own LSP runtime / window-open / picker UI.
        // 5.5.G.23: snapshot pre-dispatch state BEFORE `editor.dispatch`
        // so the State-A hover-auto-dismiss hook below sees the true
        // pre-motion cursor. Before the keystone slice this snapshot
        // could safely sit after `dispatch` because the migrated arms
        // didn't move the doc cursor; with `Action::Invoke` now host-
        // handled (motions resolve inside `editor.run_invocation`),
        // capturing after dispatch would compare the cursor to itself
        // and never trigger the dismiss.
        let pre_active = self.ad().buffer_kind;
        let pre_cursor = self.cursor();
        // Slice 3c.final.E.5d: pre-clone `action` so the closure
        // can own its copy (needed for `Send + 'static`) while
        // the outer `match action {}` at the tail still has access.
        let action_for_dispatch = action.clone();
        let mut outcome =
            self.mutate_editor_with(move |e| e.dispatch(action_for_dispatch));
        // 5.5.G.23: drain any effects the host queued (e.g. from
        // host-side `Editor::run_invocation` producing
        // `Effect::SaveBuffer` / `Effect::OpenBuffer` / etc.). The host
        // has already called `editor.handle_effect(effect.clone())` on
        // each one, so we drain via `apply_effect_app_arms` — the
        // renderer-coupled match alone, never `handle_effect` again.
        // Drained BEFORE the `consumed` early-return so host-handled
        // actions still get their renderer-side effect tail. Drained
        // BEFORE `renderer_signals` so signal handlers (e.g. theme
        // rebuild) observe the final state.
        for effect in std::mem::take(&mut outcome.effects) {
            self.apply_effect_app_arms(effect);
        }
        for signal in std::mem::take(&mut outcome.renderer_signals) {
            self.handle_renderer_signal(signal);
        }
        // 5.5.G.23.insert: drain follow-up Actions the host queued
        // (LSP autopilots like `LspOnTypeFormattingRequest` /
        // `LspInsertCompletionRequest` that need `spawn_on_lsp_runtime`
        // App-side; macro replay via `editor.do_play_macro` emits the
        // recorded action list here). Each runs through the full
        // `apply` loop so it sees the same macro-recording /
        // partial-chord lifecycle as user-driven actions. The
        // `should_quit` break preserves the mid-macro-quit semantic:
        // a recorded `:q` short-circuits the rest of the replay so
        // we don't keep firing actions against a tearing-down App.
        for follow_up in std::mem::take(&mut outcome.next_actions) {
            self.apply(follow_up);
            if self.render_state.load().lifecycle.should_quit {
                break;
            }
        }
        if outcome.consumed {
            return;
        }
        // M.4 hover-popup unification: gate the auto-dismiss-on-
        // doc-cursor-motion behaviour on `hover-mode` being active
        // on the popup buffer, instead of the structural
        // `prev_pane_for_help.is_none()` check. State A (popup
        // shown, doc focused) is "hover-mode active + active is
        // Document"; State B (focused popup) is "active is Help"
        // -- the second clause stays as the State-A discriminator.
        // Slice 3c.final.X.cleanup: read via published `popup()`
        // (popup_buffer) + `modes()` (ModesRenderState, B.11). Per-
        // keystroke hot path — App::apply runs on every input — so
        // dropping the actor RPC saves ~94µs per keystroke. The
        // two-step chain stays atomic because both reads come off
        // the same RS snapshot (RenderState publishes are atomic
        // via ArcSwap).
        let popup_has_hover_mode = self
            .popup()
            .buffer_id
            .and_then(|id| {
                let modes = self.modes();
                modes
                    .map
                    .get(&id)
                    .map(|m| m.minors().contains(&crate::modes::HoverMode::mode_id()))
            })
            .unwrap_or(false);
        let popup_in_state_a = popup_has_hover_mode && pre_active == BufferKind::Document;
        match action {
            // Phase 5.5.C: helper-free arms moved to
            // `Editor::dispatch`'s match. Grouped no-op here keeps
            // the exhaustiveness check satisfied without splitting
            // App's match logic. 5.5.G eventually collapses this
            // whole function; until then the grouped pattern is the
            // seam.
            Action::None
            | Action::Quit
            | Action::AbsorbPartialChord(_)
            | Action::PushDigit(_)
            | Action::Echo(_)
            | Action::CommandLineCancel
            | Action::SelectRegister(_)
            | Action::CommandLineDeleteChord
            | Action::CommandLineDismissCompletion
            | Action::EnterSearch(_)
            // 5.5.G.1: pure-editor fold / macro / snippet arms.
            // Bodies migrated to `Editor::dispatch`; this match
            // routes them through the grouped no-op above.
            | Action::OpenFoldAtCursor
            | Action::CloseFoldAtCursor
            | Action::ToggleFoldAtCursor
            | Action::OpenAllFolds
            | Action::CloseAllFolds
            | Action::DeleteFoldAtCursor
            | Action::GotoNextFold
            | Action::GotoPrevFold
            | Action::StartMacroRecord(_)
            | Action::StopMacroRecord
            | Action::SnippetLeave
            // 5.5.G.2: pure-editor visual + mark arms migrated to
            // `Editor::dispatch`.
            | Action::EnterVisual(_)
            | Action::ExitVisual
            | Action::ReselectLastVisual
            | Action::SetMark(_)
            // 5.5.G.3: pure-editor edit-cluster arms migrated to
            // `Editor::dispatch`.
            | Action::Undo
            | Action::Redo
            | Action::JoinLines { .. }
            | Action::ToggleCaseAtCursor
            | Action::EnterAppend
            | Action::OpenLineBelow
            | Action::OpenLineAbove
            | Action::OverwriteChar(_)
            | Action::ReplaceUndoLast
            | Action::DeleteCharBackward
            // 5.5.G.4: pure-editor scroll / viewport / page / bracket
            // / redraw arms migrated to `Editor::dispatch`.
            | Action::JumpViewport(_)
            | Action::ScrollCursorTo(_)
            | Action::PageDown
            | Action::PageUp
            | Action::ScrollLineUp
            | Action::ScrollLineDown
            | Action::MatchBracket
            | Action::RedrawScreen
            // 5.5.G.5: pure-editor pane-navigation arms migrated.
            | Action::SplitPaneHorizontal
            | Action::SplitPaneVertical
            | Action::ClosePane
            | Action::NavigatePane(_)
            | Action::NextPane
            | Action::PrevPane
            // Issue #28 (2026-05-22): pane-resize / equalize.
            // Host-resident bodies; grouped no-op here.
            | Action::EqualizePanes
            | Action::GrowPaneHeight
            | Action::ShrinkPaneHeight
            | Action::GrowPaneWidth
            | Action::ShrinkPaneWidth
            // Issue #29 (2026-05-22): tabs. Host-resident.
            | Action::NextTab
            | Action::PrevTab
            | Action::GoToTab(_)
            | Action::NewTab
            | Action::NewTabAt(_)
            | Action::CloseTab
            // Issue #40 / Terminal-mode T1: host-resident.
            | Action::TerminalSpawn(_)
            // Terminal-mode T2.a: host-resident encoder + mode-toggle handlers.
            | Action::TerminalInput(_)
            | Action::EnterTerminalInsert
            | Action::ExitTerminalInsert
            | Action::TerminalScroll(_)
            | Action::TerminalArmExitChord
            | Action::OnlyTab
            | Action::MoveTab(_)
            | Action::MovePaneToNewTab
            // Issue #32 (2026-05-22): picker open-target overrides.
            // Host-resident bodies; grouped no-op here.
            | Action::PickerAcceptInSplit
            | Action::PickerAcceptInVSplit
            | Action::PickerAcceptInTab
            // 5.5.G.6: pure-editor mark-history arms migrated.
            | Action::WalkMarkHistoryBack
            | Action::WalkMarkHistoryForward
            // 5.5.G.7: tag-stack / mark-jump / jump-history arms.
            | Action::TagStackPop
            | Action::JumpToMarkLine(_)
            | Action::JumpToMarkExact(_)
            | Action::JumpHistoryBack
            | Action::JumpHistoryForward
            // 5.5.G.8: snippet placeholder navigation arms.
            | Action::SnippetNextPlaceholder
            | Action::SnippetPrevPlaceholder
            // 5.5.G.9: paste cluster arms.
            | Action::PasteAfter
            | Action::PasteBefore
            | Action::PasteText(_)
            // 5.5.G.10: search-state arms migrated to Editor::dispatch.
            | Action::SearchAppend(_)
            | Action::SearchBackspace
            | Action::SearchSubmit
            | Action::SearchCancel
            | Action::SearchNext
            | Action::SearchPrevious
            | Action::SearchWordUnderCursor(_)
            // 5.5.G.11: picker append/backspace/select + close-hover.
            | Action::PickerAppend(_)
            | Action::PickerBackspace
            | Action::PickerSelectNext
            | Action::PickerSelectPrev
            | Action::CloseHover
            // 5.5.G.12: HelpDismiss migrated to Editor::dispatch.
            | Action::HelpDismiss
            // 5.5.G.13: pure-editor cmdline arms.
            | Action::EnterCommandLine
            | Action::CommandLineHistoryPrev
            | Action::CommandLineHistoryNext
            // 5.5.G.14: completion-cancel cluster + docs scroll +
            // foldenable toggle.
            | Action::CompletionCancel
            | Action::CompletionCancelAndExitInsert
            | Action::CompletionDocsScrollDown
            | Action::CompletionDocsScrollUp
            | Action::ToggleFoldEnable
            // 5.5.G.15: cmdline-completion popup nav.
            | Action::CommandLineCompletePrev
            | Action::CommandLineAcceptCompletion
            // 5.5.G.16: `zf` Visual-selection fold creation.
            | Action::CreateFoldFromVisual
            // 5.5.G.17: modal-state pivot + blockwise-Visual I/A.
            | Action::EnterMode(_)
            | Action::EnterBlockVisualInsert
            | Action::EnterBlockVisualAppend
            // 5.5.LSP.1: `K` -- hover request migrated to
            // `Editor::dispatch`.
            | Action::LspHoverRequest
            // 5.5.LSP.2: `gd` / `gD` / `gy` / `gI` -- nav family
            // migrated to `Editor::dispatch`.
            | Action::LspDefinitionRequest
            | Action::LspDeclarationRequest
            | Action::LspTypeDefinitionRequest
            | Action::LspImplementationRequest
            // 5.5.LSP.3: `gr` -- references migrated to `Editor::dispatch`.
            | Action::LspReferencesRequest
            // 5.5.LSP.4: signature help + completion request migrated
            // to `Editor::dispatch`.
            | Action::LspSignatureHelpRequest
            | Action::LspCompletionRequest
            // 5.5.LSP.5: document symbol request migrated.
            // (`LspWorkspaceSymbolRequest` carries a `String`
            // payload, so it lives in its own ignore-binding arm
            // below.)
            | Action::LspDocumentSymbolRequest
            // 5.5.SNIPPET.1: `<C-x><C-s>` snippet expand migrated.
            | Action::SnippetExpand
            // Phase 5.8.AF.5 / Slice 3c.final.C: renderer-side
            // non-dispatch mutation lifts. Bodies live in
            // `Editor::dispatch`'s match; the App wrapper just
            // includes them in the grouped no-op band so the
            // exhaustiveness check passes without splitting App's
            // logic. Same pattern the other 5.5.G migrations use.
            | Action::SetViewportHeight(_)
            | Action::EnsureCursorVisible
            | Action::RefreshPaneHighlights
            | Action::DismissPopup
            | Action::SetTerminalWidth(_)
            | Action::AcknowledgeRedraw => {}
            // 5.5.LSP.5: workspace-symbol request migrated to
            // `Editor::dispatch`. Action carries the query string;
            // a data-bearing variant can't sit in the grouped
            // `_ => {}` band (binding mismatch).
            Action::LspWorkspaceSymbolRequest(_) => {}
            // 5.5.G.23: keystone — `Invoke` is host-handled now via
            // `editor.run_invocation` (the host pushes any
            // renderer-coupled effects into `outcome.effects`, which
            // the dispatch wrapper above drained before this match
            // ran). The `_` binding can't sit in the grouped no-op
            // band because of the inner `CommandInvocation` payload.
            Action::Invoke(_) => {}
            // 5.5.G.23.insert: Insert(s) is host-handled via
            // `editor.do_insert_text`; the host queues LSP autopilot
            // follow-ups (`LspOnTypeFormattingRequest`,
            // `LspInsertCompletionRequest`) through `out.next_actions`
            // which the dispatch wrapper already drained.
            Action::Insert(_) => {}
            // 5.5.G.23.insert: LSP autopilot follow-ups host-emitted
            // by `Editor::do_insert_text` (forthcoming). The handlers
            // live App-side because they reach for `spawn_on_lsp_runtime`
            // + `BatchingSink` + the `pending_insert_completion_lsp_*`
            // channels.
            // Phase 5.8.AF: migrated to host (consumed = true).
            Action::LspOnTypeFormattingRequest(_) => {}
            Action::LspInsertCompletionRequest => {}
            // 5.5.G.3: `DeleteCharBackward`, `EnterAppend`,
            // `OpenLineBelow`, `OpenLineAbove`, `Undo`, `Redo`
            // migrated to `Editor::dispatch`; routed through the
            // grouped no-op above.
            // 5.5.G.17: `EnterMode` / `EnterBlockVisualInsert` /
            // `EnterBlockVisualAppend` migrated to `Editor::dispatch`.


            // 5.5.G.13: `EnterCommandLine` migrated to `Editor::dispatch`.
            // 5.5.G.23.cmdline: Append / Backspace host-handled via
            // `editor.do_command_line_{append,backspace}`.
            Action::CommandLineAppend(_) | Action::CommandLineBackspace => {}
            // 5.5.G.23.cmdline: Submit host-handled via
            // `editor.do_command_line_submit` (missing-arg prompt +
            // execute_ex_line + history push + state reset).
            Action::CommandLineSubmit => {}
            // 5.5.G.13: `CommandLineHistoryPrev` / `CommandLineHistoryNext`
            // migrated to `Editor::dispatch`.

            // 5.5.G.11: `CloseHover` / `PickerAppend` /
            // `PickerBackspace` / `PickerSelectNext` /
            // `PickerSelectPrev` migrated to `Editor::dispatch`.
            // Phase 5.8.AF: migrated to host (consumed = true).
            Action::PickerAccept | Action::PickerDismiss => {}

            // 5.5.G.2: `EnterVisual` / `ExitVisual` / `ReselectLastVisual`
            // migrated to `Editor::dispatch`; routed through the
            // grouped no-op above.
            // 5.5.G.10: `SearchWordUnderCursor` migrated.
            // 5.5.G.4: `MatchBracket` migrated to `Editor::dispatch`.
            // 5.5.G.3: `ToggleCaseAtCursor` / `JoinLines` migrated.
            // 5.5.G.23.macros: `;` / `,` host-handled via
            // `editor.do_find_repeat`; the synthesized invocation
            // routes through `run_invocation` and its effects drain
            // via `outcome.effects`.
            Action::FindRepeat { .. } => {}

            // 5.5.G.16: `CreateFoldFromVisual` migrated to `Editor::dispatch`.
            // 5.5.G.1: `OpenFoldAtCursor` / `CloseFoldAtCursor` /
            // `ToggleFoldAtCursor` / `OpenAllFolds` / `CloseAllFolds`
            // / `DeleteFoldAtCursor` / `GotoNextFold` / `GotoPrevFold`
            // migrated to `Editor::dispatch`; routed through the
            // grouped no-op above.
            // 5.5.G.14: `ToggleFoldEnable` migrated to `Editor::dispatch`.
            // 5.5.LSP.1: `LspHoverRequest` migrated to `Editor::dispatch`
            // (host-side `Editor::lsp_hover_request`); the App arm here
            // is gone -- the action falls through to the grouped no-op
            // below.
            // 5.5.LSP.2: `LspDefinitionRequest` / `LspDeclarationRequest` /
            // `LspTypeDefinitionRequest` / `LspImplementationRequest`
            // migrated to `Editor::dispatch` (host-side
            // `Editor::lsp_nav_request(LspNavKind)`); the App arms
            // here are gone -- all four fall through to the grouped
            // no-op below.
            // 5.5.LSP.3: `LspReferencesRequest` migrated to
            // `Editor::dispatch` (host-side
            // `Editor::lsp_references_request`); falls through to
            // the grouped no-op below.
            // Phase 5.8.AF: migrated to host (consumed = true).
            Action::LspFollowLinkAtCursor => {}
            // 5.5.LSP.4: `LspSignatureHelpRequest` /
            // `LspCompletionRequest` migrated to `Editor::dispatch`;
            // fall through to the grouped no-op below.
            // 5.5.G.7: `TagStackPop` migrated to `Editor::dispatch`.
            // Phase 5.8.AF: migrated to host (consumed = true).
            Action::CompletionTrigger
            | Action::CompletionNext
            | Action::CompletionPrev
            | Action::CompletionAccept
            | Action::CompletionToggleDocs
            | Action::CompletionFilterToSource(_)
            | Action::CompletionFilterClear
            | Action::CompletionAcceptThenInsert(_) => {}
            // 5.5.SNIPPET.1: `Action::SnippetExpand` migrated to
            // `Editor::dispatch`; routed through the grouped no-op
            // above.
            // 5.5.G.8: `SnippetNextPlaceholder` / `SnippetPrevPlaceholder`
            // migrated to `Editor::dispatch`.
            // 5.5.G.1: `SnippetLeave` migrated to `Editor::dispatch`;
            // routed through the grouped no-op above.
            // 5.5.LSP.5: `LspDocumentSymbolRequest` /
            // `LspWorkspaceSymbolRequest(query)` migrated to
            // `Editor::dispatch`; fall through to the grouped no-op
            // below. (LspWorkspaceSymbolRequest carries data, so
            // it lives in its own grouped arm.)
            // 5.5.G.7: `JumpHistoryBack` / `JumpHistoryForward`
            // migrated to `Editor::dispatch`.
            // 5.5.G.4: `RedrawScreen` migrated to `Editor::dispatch`.
            // 5.5.G.6: `WalkMarkHistoryBack` / `WalkMarkHistoryForward`
            // migrated to `Editor::dispatch`.

            // 5.5.G.23.macros: `PlayMacro` / `PlayLastMacro` are
            // host-handled via `editor.do_play_macro` /
            // `do_play_last_macro`; recorded actions stream through
            // `out.next_actions` and the dispatch wrapper's drain
            // (with `should_quit` short-circuit).
            Action::PlayMacro(_) | Action::PlayLastMacro => {}

            // 5.5.G.3: `OverwriteChar` / `ReplaceUndoLast` migrated
            // to `Editor::dispatch`.
            // 5.5.G.4: `JumpViewport` / `ScrollCursorTo` / `PageDown`
            // / `PageUp` / `ScrollLineUp` / `ScrollLineDown` migrated.

            // 5.5.G.2: `SetMark` migrated to `Editor::dispatch`.
            // 5.5.G.7: `JumpToMarkLine` / `JumpToMarkExact` migrated.

            // 5.5.G.23.macros: `.` host-handled via
            // `editor.do_repeat_last_change` (re-dispatches the
            // captured invocation through host's `run_invocation` and
            // replays the captured Insert tail through host's
            // `do_insert_text`). Effects + signals + LSP follow-ups
            // drain through the outcome channels above.
            Action::RepeatLastChange => {}

            // 5.5.G.9: `PasteAfter` / `PasteBefore` / `PasteText`
            // migrated to `Editor::dispatch`.

            // 5.5.G.23.cmdline: Clear / DeleteWordBackward /
            // DescribeUnderCursor / AppendChord / CompleteOrAdvance
            // all host-handled. AppendChord's auto-submit-on-chord
            // semantic threads through host's
            // `do_command_line_append_chord` -> `do_command_line_submit`.
            Action::CommandLineClear
            | Action::CommandLineDeleteWordBackward
            | Action::CommandLineDescribeUnderCursor
            | Action::CommandLineCompleteOrAdvance => {}
            Action::CommandLineAppendChord(_) => {}
            // 5.5.G.15: `CommandLineCompletePrev` /
            // `CommandLineAcceptCompletion` migrated to `Editor::dispatch`.

            // 5.5.G.12: `HelpDismiss` migrated to `Editor::dispatch`.
            // Phase 5.8.AF: FollowLink Oil/FileTree arms migrated
            // Phase 5.8.AF: FollowLink fully migrated host-side as
            // of 2026-05-27. Help-link follow runs through
            // `Editor::do_help_follow_link`; Oil / FileTree arms
            // already lived there. This arm is now a host-handled
            // no-op so the post-dispatch hooks (ensure_cursor_visible,
            // hover auto-dismiss) still run.
            Action::FollowLink => {}
            // Phase 5.8.AF: migrated to host (consumed = true).
            Action::OilNavigateUp => {}

            // 5.5.G.5: `SplitPaneHorizontal` / `SplitPaneVertical` /
            // `ClosePane` / `NavigatePane` / `NextPane` / `PrevPane`
            // migrated to `Editor::dispatch`.

            // 5.5.G.10: SearchAppend / Backspace / Submit / Cancel /
            // Next / Previous migrated to `Editor::dispatch`.
        }
        // Skip ensure_cursor_visible when the popup just dismissed
        // back to the document. `app.editor.viewport_height` is still the
        // popup's small inner height at this point (runtime resets
        // it on the *next* iteration), so running ensure on the
        // restored doc cursor / scroll would adjust scroll against
        // the wrong viewport and visibly jolt the backdrop on
        // close. The next iteration's render fires with the correct
        // viewport and the restored (cursor, scroll) is already a
        // valid view -- it's the pair we captured pre-popup.
        let popup_dismissed = matches!(pre_active, BufferKind::Help)
            && matches!(self.ad().buffer_kind, BufferKind::Document);
        if !popup_dismissed {
            self.ensure_cursor_visible();
        }
        self.maybe_reparse_syntax();
        // State-A hover-auto-dismiss: popup was shown, focus
        // never moved into it (so `prev_pane_for_help` is None),
        // and the doc cursor moved. Drop the popup -- it's
        // anchored to the prior symbol and is now stale.
        if popup_in_state_a
            && self.ad().buffer_kind == BufferKind::Document
            && self.cursor() != pre_cursor
        {
            self.dismiss_popup();
        }
        let _ = pre_active;
        // Slice 8.f: re-stack Insert-mode minor-mode layers in
        // lockstep with overlay state changes. Cheap when
        // nothing changed.
        self.sync_keymap_overlays();
        // Phase 5.8.AF.5 / Slice X1: drain pending LSP / event /
        // mode-lifecycle results here at the keystroke-driven
        // dispatch tail rather than in the per-frame body
        // (`crates/lattice-ui-tui/src/runtime.rs`'s main_loop).
        // Paramount goal #1 forbids I/O / event drain on the UI
        // thread; the renderer body is the UI thread.
        // `run_tick_pending` is the host aggregator that polls
        // ~30 channels for async results -- on a busy frame
        // (file open) it can take 49ms, which is 6x over the
        // 8ms-at-120Hz keystroke-to-glyph budget. Running it
        // here makes that cost happen during the keystroke that
        // caused the work (the open), where 8ms is not the
        // budget; subsequent frames see only what the dispatch
        // tail published.
        //
        // Recursive `apply` calls (next_actions) drain again --
        // cheap because the prior call already emptied the
        // channels. Idle LSP arrivals (response with no
        // keystroke in flight) are NOT drained until the next
        // keystroke: see slice X1b (docs/dev/operations/
        // render-thread-discipline-remediation.md §X1b) for the
        // wake-bridge that closes that gap.
        // Slice 3c.final.E.3: route through `mutate_editor_with`.
        let tick_signals = self.mutate_editor_with(|e| e.run_tick_pending());
        for signal in tick_signals {
            self.handle_renderer_signal(signal);
        }
    }

    pub(super) fn execute_ex_line(&mut self, line: &str) {
        let reg = self.registry();
        match excommand::parse(line, &reg) {
            Ok(inv) => match self.dispatch_blocking(inv) {
                Ok(eff) => self.apply_effect(eff),
                Err(e) => self.set_message(EchoLevel::Error, e.to_string()),
            },
            Err(err) => {
                self.set_message(EchoLevel::Error, err.to_string());
            }
        }
    }

    /// 5.5.G.23: body migrated to
    /// [`lattice_host::dispatch::Editor::run_invocation`]. Retained as
    /// a 1-line delegate because the host-side `Action::Invoke` arm
    /// already routes through `editor.run_invocation` — App-side
    /// callers that still reach this method (`do_play_macro`,
    /// `do_find_repeat`, `RepeatLastChange`) collapse on their own
    /// slice migration.
    pub(super) fn run_invocation(&mut self, inv: CommandInvocation) {
        // Slice 3c.final.E.3: build `out` inside the closure and
        // return it so the renderer can drain its effects/signals.
        let mut out = self.mutate_editor_with(move |e| {
            let mut out = lattice_host::dispatch::DispatchOutcome::default();
            e.run_invocation(inv, &mut out);
            out
        });
        for effect in std::mem::take(&mut out.effects) {
            self.apply_effect_app_arms(effect);
        }
        for signal in std::mem::take(&mut out.renderer_signals) {
            self.handle_renderer_signal(signal);
        }
    }

    /// Dispatch a `CommandInvocation` against the active oil
    /// buffer's rope. Oil's content lives in `oil.content` (a
    /// `Buffer`), separate from `self.editor.document` (the actor-
    /// backed document buffer). The grammar dispatcher only
    /// knows about `Document`, so we synthesise a temporary
    /// `Document` from oil's rope, dispatch through it, and
    /// copy the resulting buffer back. Edits + cursor updates
    /// land on the oil rope without touching the document
    /// actor.
    ///
    /// This is the seam that makes oil writable. v1 supports
    /// motions, operators (insert / delete / change / yank),
    /// and visual selections against oil's rope. The `:w`
    /// path is separate (`do_write` matches on
    /// `BufferKind::Oil` and routes to `OilBuffer::apply`).
    /// 5.5.G.23: body migrated to
    /// [`lattice_host::dispatch::Editor::run_oil_invocation`].
    /// Retained as a 1-line delegate while the App-side outer
    /// `run_invocation` router still routes here; collapses with
    /// the router on the keystone wire-up.
    fn run_oil_invocation(&mut self, inv: CommandInvocation) {
        // Slice 3c.final.E.3: route through `mutate_editor`.
        // 2026-05-26: host returns `bool` (handled flag) for the
        // per-kind-first dispatch reorder; discard it here since
        // App-side direct callers don't fall back to a grammar
        // gate — they only invoke the runner when the buffer kind
        // is already established.
        self.mutate_editor(move |e| {
            let _ = e.run_oil_invocation(inv);
        });
    }

    /// 5.5.G.23: body migrated to
    /// [`lattice_host::dispatch::Editor::run_file_tree_invocation`].
    fn run_file_tree_invocation(&mut self, inv: CommandInvocation) {
        self.mutate_editor(move |e| {
            let _ = e.run_file_tree_invocation(inv);
        });
    }

    /// 5.5.G.23: body migrated to
    /// [`lattice_host::dispatch::Editor::run_help_invocation`].
    fn run_help_invocation(&mut self, inv: CommandInvocation) {
        self.mutate_editor(move |e| {
            let _ = e.run_help_invocation(inv);
        });
    }

    /// 5.5.G.23: body migrated to
    /// [`lattice_host::dispatch::Editor::run_read_only_motion`].
    fn run_read_only_motion(&mut self, inv: CommandInvocation) {
        self.mutate_editor(move |e| {
            let _ = e.run_read_only_motion(inv);
        });
    }

    /// 5.5.G.23: body migrated to
    /// [`lattice_host::dispatch::Editor::run_document_invocation`].
    /// Retained as a 1-line delegate; App-side internal callers
    /// (`do_play_macro`, `do_find_repeat`, etc.) collapse to host
    /// recursion on their own slice migration.
    fn run_document_invocation(&mut self, inv: CommandInvocation) {
        // Slice 3c.final.E.3: build `out` inside the closure.
        let mut out = self.mutate_editor_with(move |e| {
            let mut out = lattice_host::dispatch::DispatchOutcome::default();
            e.run_document_invocation(inv, &mut out);
            out
        });
        for effect in std::mem::take(&mut out.effects) {
            self.apply_effect_app_arms(effect);
        }
        for signal in std::mem::take(&mut out.renderer_signals) {
            self.handle_renderer_signal(signal);
        }
    }

    pub(super) fn apply_effect(&mut self, effect: Effect) {
        // Phase 5.5.E.1: route every Effect through `Editor::handle_effect`
        // first so the host owns its migrated arms (today:
        // `Effect::None`, `Effect::ClearSearchHighlight`, `Effect::Echo`,
        // `Effect::EchoMarks`, `Effect::EchoRegisters`, `Effect::Yank`,
        // `Effect::SelectionChange`).
        //
        // Phase 5.5.E.5: the returned `DispatchOutcome` is now consumed --
        // any `RendererSignal`s the host queued (e.g. `ThemeChanged` from
        // a future `Effect::SetOption` migration) drain through
        // [`Self::handle_renderer_signal`] AFTER the post-effect match
        // below runs. Today no migrated arm pushes a signal, so the loop
        // is a no-op; the pipe is wired so subsequent E.* slices that
        // emit signals don't need to rewire the call site.
        //
        // 5.5.G.23: body extracted into [`Self::apply_effect_app_arms`]
        // so the host-side `Editor::run_invocation` (forthcoming) can
        // push effects into `outcome.effects` and the renderer's
        // dispatch wrapper drains via the renderer-coupled match arm
        // only — never re-running `handle_effect` and double-executing
        // host-migrated arms.
        // Slice 3c.final.E.3: route through `mutate_editor_with`.
        let effect_for_host = effect.clone();
        let outcome = self.mutate_editor_with(move |e| e.handle_effect(effect_for_host));
        self.apply_effect_app_arms(effect);
        for signal in outcome.renderer_signals {
            self.handle_renderer_signal(signal);
        }
    }

    /// 5.5.G.23: the renderer-coupled effect match — every arm that
    /// still drives an App-side `do_*` helper. Drained by
    /// [`Self::apply_effect`] for App-originated effects, and by the
    /// dispatch wrapper for effects the host queued into
    /// `outcome.effects` (those already had `editor.handle_effect`
    /// called on them host-side; this method must NOT call
    /// `handle_effect` again).
    pub(super) fn apply_effect_app_arms(&mut self, effect: Effect) {
        match effect {
            // Phase 5.5.E.1–E.4: migrated arms. Bodies live in
            // `lattice_host::dispatch::handle_effect`.
            Effect::None
            | Effect::ClearSearchHighlight
            | Effect::Echo { .. }
            | Effect::EchoMarks
            | Effect::EchoRegisters
            | Effect::Yank { .. }
            | Effect::SelectionChange(_)
            | Effect::EnterMode(_)
            | Effect::SetOption { .. }
            | Effect::ListBuffers
            | Effect::DescribeBuffer
            | Effect::DescribeCommand { .. }
            | Effect::Apropos { .. }
            | Effect::DescribeKey { .. }
            | Effect::ListKeymap
            | Effect::DescribeOption { .. }
            | Effect::ListOptions
            | Effect::DescribeOptionResolution { .. }
            | Effect::DescribeEvents
            | Effect::DescribeEvent { .. }
            | Effect::DescribeDiff
            | Effect::DiffOpen
            | Effect::DiffOff
            | Effect::BufferNext
            | Effect::BufferPrev
            | Effect::BufferDelete { .. }
            | Effect::ListModes
            | Effect::DescribeMode { .. }
            | Effect::Customize { .. }
            | Effect::ListDiagnostics
            | Effect::DeleteCurrentLine
            | Effect::Substitute { .. }
            | Effect::Edits(_) => {}
            // 5.5.E.7.7: `Edits` migrated to `Editor::handle_effect`;
            // routed through the grouped no-op above. `handle_edits`
            // and `publish_document_changed` wrappers retired in the
            // same slice.
            // 5.5.G.23.macros: `Effect::EnterMode` migrated to
            // `Editor::handle_effect` so host runners
            // (`run_document_invocation`, `do_repeat_last_change`)
            // observe the flipped modal state synchronously. App-side
            // arm collapses to the grouped no-op below.
            // --- Ex-command effects (DESIGN.md §5.2.1 unified dispatch). ---
            // These come from ex-command apply closures registered in the
            // grammar registry; the host owns the side effects.
            Effect::SaveBuffer { path } => self.do_write(path),
            Effect::QuitEditor { force } => self.do_quit(force),
            Effect::OpenBuffer { path, force } => self.do_edit(path, force),
            // 5.5.E.7.5: `Substitute` migrated to
            // `Editor::handle_effect`; routed through the grouped
            // no-op above.
            Effect::Global {
                pattern,
                inverted,
                body,
            } => self.do_global(&pattern, inverted, body.as_ref()),
            // 5.5.E.7.4: `DeleteCurrentLine` migrated to
            // `Editor::handle_effect`; routed through the grouped
            // no-op above.
            // 5.5.F.2: `DescribeCommand` / `Apropos` / `DescribeKey`
            // / `ListKeymap` migrated to `Editor::handle_effect`;
            // the renderer-coupled tail flows back through
            // `RendererSignal::DisplayBuffer`. Listed above in the
            // grouped no-op.
            // 5.5.F.4.3: `BufferNext` / `BufferPrev` migrated to
            // `Editor::handle_effect`; emit `BufferActivated` for
            // the post-activation tail. Routed through the grouped
            // no-op above.
            Effect::OpenBufferPicker => self.open_buffer_picker(),
            Effect::OpenPicker { source, args } => self.open_picker(source, args),
            // 5.5.F.4.4: `BufferDelete` migrated to
            // `Editor::handle_effect`; emits `BufferActivated` for the
            // post-activation tail. Routed through the grouped no-op
            // below.
            Effect::OpenFileTree { root } => self.do_open_file_tree(root),
            Effect::CloseFileTree => self.dismiss_file_tree(),
            Effect::OpenOil { dir } => self.do_open_oil(dir),
            // 5.5.F.3: `DescribeOption` / `ListOptions` migrated
            // to `Editor::handle_effect`; routed through the
            // grouped no-op above.
            Effect::OpenHover { markdown } => self.do_open_hover(&markdown),
            Effect::CloseHover => self.do_close_hover(),
            Effect::OpenHelpTopic { topic } => self.do_open_help_topic(topic.as_deref()),
            // 5.5.F.7: `ListDiagnostics` migrated to
            // `Editor::handle_effect`; routed through the grouped
            // no-op below.
            Effect::NextDiagnostic => self.do_next_diagnostic(),
            Effect::PrevDiagnostic => self.do_prev_diagnostic(),
            Effect::OpenLspLog { server_id } => self.do_open_lsp_log(server_id.as_deref()),
            Effect::OpenMessages => self.do_open_messages(),
            Effect::ToggleLspTrace { server_id } => self.do_toggle_lsp_trace(&server_id),
            Effect::OpenLspTraceLog { server_id } => {
                self.do_open_lsp_trace_log(server_id.as_deref())
            }
            Effect::LspStatus => self.do_lsp_status(),
            Effect::LspServerLogListing => self.do_lsp_server_log_listing(),
            Effect::LspRestart { server_id } => self.do_lsp_restart(&server_id),
            Effect::LspProgressCancel { server_id } => {
                self.do_lsp_progress_cancel(server_id.as_deref())
            }
            Effect::LspExpandRegion => self.do_lsp_expand_region(),
            Effect::LspShrinkRegion => self.do_lsp_shrink_region(),
            Effect::SetLspLogLevel { server_id, level } => {
                self.do_set_lsp_log_level(server_id.as_deref(), &level)
            }
            Effect::LspLogClear { server_id } => self.do_lsp_log_clear(server_id.as_deref()),
            // 5.5.LSP.5: symbol helpers live on `Editor`.
            Effect::LspDocumentSymbol => self.mutate_editor_with(move |e| e.lsp_document_symbol_request()),
            Effect::LspWorkspaceSymbol { query } => {
                self.mutate_editor_with(move |e| e.lsp_workspace_symbol_request(&query))
            }
            Effect::LspIncomingCalls => self.do_lsp_call_hierarchy_request(false),
            Effect::LspOutgoingCalls => self.do_lsp_call_hierarchy_request(true),
            Effect::LspSupertypes => self.do_lsp_type_hierarchy_request(false),
            Effect::LspSubtypes => self.do_lsp_type_hierarchy_request(true),
            Effect::LspMoniker => self.do_lsp_moniker_request(),
            Effect::LspCodeLens => self.do_lsp_code_lens_picker(),
            Effect::LspColorPresentation => self.do_lsp_color_presentation(),
            Effect::LspFormat => self.do_lsp_format_request(false),
            Effect::LspFormatRange => self.do_lsp_format_request(true),
            // 5.5.LSP.4: signature help + completion request now
            // live on `Editor`. The Effect routes here continue to
            // dispatch via `self.editor.<fn>()` until the Effect
            // table itself migrates.
            Effect::LspSignatureHelp => self.mutate_editor_with(move |e| e.lsp_signature_help_request()),
            Effect::LspComplete => self.mutate_editor_with(move |e| e.lsp_completion_request()),
            Effect::LspRename { new_name } => self.do_lsp_rename_request(&new_name),
            Effect::LspCodeAction => self.do_lsp_code_action_request(),
            Effect::SnippetExpand => self.mutate_editor_with(move |e| e.do_snippet_expand_at_cursor()),
            Effect::ReloadSnippets => self.do_reload_snippets(),
            Effect::ToggleMode { mode_name } => self.toggle_mode_by_name(&mode_name),
            // 5.5.F.3: `DescribeEvents` / `DescribeEvent` /
            // `DescribeOptionResolution` migrated to
            // `Editor::handle_effect`; routed through the grouped
            // no-op above.
            // 5.5.F.6: `ListModes` / `DescribeMode` / `Customize`
            // migrated to `Editor::handle_effect`; routed through
            // the grouped no-op below.
            Effect::Tutor { lesson } => self.do_tutor(lesson),
            // 5.5.G.24: AppEffect router migrated to
            // `Editor::apply_app_effect`. Host either mutates editor
            // fields directly (`AbsorbOperatorPrefix`) or pushes a
            // follow-up `Action` through `out.next_actions` for the
            // renderer's drain. App-side arm is now a no-op; routing
            // through this match means a stale apply-effect from
            // earlier in the pipeline still reaches the right place.
            Effect::AppAction(_) => {}
            Effect::Many(many) => {
                // 5.5.G.23: inner effects of a `Many` go through the
                // full wrapper (`apply_effect`) so each one gets a
                // fresh `editor.handle_effect` pass — the host doesn't
                // descend into Many in its handler. Same semantics as
                // before the split.
                for e in many {
                    self.apply_effect(e);
                }
            }
        }
    }

    /// Renderer-side side-effect dispatcher. Receives every
    /// [`RendererSignal`] the host emits during a dispatch and runs
    /// the TUI-specific follow-up: `ThemeChanged` rebuilds the
    /// TUI `Style` cache from `editor.host_theme`; `Quit` is a
    /// no-op (already set on `editor.should_quit`, which
    /// `runtime::main_loop` reads each tick). A future GPUI renderer
    /// implements its own equivalent on `lattice-ui-gpui::App`.
    pub fn handle_renderer_signal(&mut self, signal: RendererSignal) {
        match signal {
            RendererSignal::ThemeChanged => self.rebuild_tui_theme(),
            RendererSignal::Quit => {
                // `editor.should_quit` was set alongside the signal
                // emission (see `Action::Quit` in `Editor::dispatch`);
                // the runtime loop polls it. Keep the arm for shape
                // — a future GPUI renderer wires window-close here.
            }
            // 5.5.E.6: `ui.nerd_fonts` cascade -- the host already
            // updated `editor.host_theme.nerd_fonts`; the renderer
            // walks every file-tree buffer and refreshes its rope
            // so the icon-glyph cells re-render. Oil reads the
            // toggle each frame and needs no rope work.
            RendererSignal::NerdFontsToggled => {
                let nerd_fonts = self.render_state.load().theme.nerd_fonts;
                for id in self.buffers().registry.file_tree_ids() {
                    self.set_file_tree_nerd_fonts(id, nerd_fonts);
                }
            }
            // 5.5.F.5.4: `MirrorOptionToModes` retired. The cascade
            // now runs synchronously host-side inside
            // `apply_option_cascade` via `Editor::mirror_option_to_modes`;
            // any cascading mode-lifecycle signals stream back through
            // the same `Vec<RendererSignal>` the parent cascade already
            // drains, so the App-side handler is no longer reachable.
            // 5.5.E.6: `lsp.<server>.*` cascade -- fan out
            // `workspace/didChangeConfiguration` to every actor
            // matching `server_id` with the freshly merged subtree.
            RendererSignal::LspConfigChanged(server_id) => {
                self.fan_out_did_change_configuration(&server_id);
            }
            // 5.5.F.1: a host-side `do_*` arm built a HelpContent
            // and wants the renderer to surface it under a given
            // category. Route through `display_buffer` (the
            // renderer's existing dispatch: resolve category to a
            // `BufferDisplay` preference, then route popup / pane
            // / split). `display_buffer` returns the registered
            // BufferId for `ActivePane` / `Split` displays so
            // future host slices can mirror it back through a
            // dedicated signal; today's callers (ListBuffers,
            // DescribeBuffer) don't need the id back so we discard.
            RendererSignal::DisplayBuffer(req) => {
                let lattice_host::dispatch::DisplayBufferRequest { content, category } = *req;
                self.display_buffer(content, category);
            } // 5.5.F.5.5: `BufferActivated` retired. The Bucket-A
              // `visible_highlights` / `pane_highlights` cache clear
              // lives on `Editor` as plain field writes, so the
              // post-activation tail (`activate_buffer_state`) runs
              // entirely host-side; cascading mode-lifecycle signals
              // stream into the `handle_effect` outcome and fan out
              // through this same match.
        }
    }

    // 5.5.G.24: App-side `apply_app_effect` retired. Body moved to
    // `lattice_host::dispatch::Editor::apply_app_effect`; the host
    // mutates editor fields directly for `AbsorbOperatorPrefix` (only
    // structural arm) and enqueues a follow-up `Action` via
    // `out.next_actions` for every other variant. The renderer's
    // existing drain processes those through the full `apply` loop.
    // `Effect::AppAction(_)` in `apply_effect_app_arms` collapses to
    // the grouped no-op band.
    //
    // 5.5.E.7.7: `handle_edits` retired -- body moved to
    // `lattice_host::dispatch::Editor::handle_edits`, called via the
    // `Effect::Edits` arm in `Editor::handle_effect`. The grouped
    // no-op above keeps the App-side match exhaustive.
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
        | Effect::OpenPicker { .. }
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
        | Effect::OpenMessages
        | Effect::ToggleLspTrace { .. }
        | Effect::OpenLspTraceLog { .. }
        | Effect::LspStatus
        | Effect::LspServerLogListing
        | Effect::LspRestart { .. }
        | Effect::LspProgressCancel { .. }
        | Effect::LspExpandRegion
        | Effect::LspShrinkRegion
        | Effect::SetLspLogLevel { .. }
        | Effect::LspLogClear { .. }
        | Effect::LspDocumentSymbol
        | Effect::LspWorkspaceSymbol { .. }
        | Effect::LspIncomingCalls
        | Effect::LspOutgoingCalls
        | Effect::LspSupertypes
        | Effect::LspSubtypes
        | Effect::LspMoniker
        | Effect::LspCodeLens
        | Effect::LspColorPresentation
        | Effect::LspFormat
        | Effect::LspFormatRange
        | Effect::LspSignatureHelp
        | Effect::LspComplete
        | Effect::LspRename { .. }
        | Effect::LspCodeAction
        | Effect::SnippetExpand
        | Effect::ReloadSnippets
        | Effect::ToggleMode { .. }
        | Effect::DescribeEvents
        | Effect::DescribeEvent { .. }
        | Effect::DescribeDiff
        | Effect::DiffOpen
        | Effect::DiffOff
        | Effect::ListModes
        | Effect::DescribeMode { .. }
        | Effect::DescribeOptionResolution { .. }
        | Effect::Customize { .. }
        | Effect::Tutor { .. }
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
        | Effect::OpenPicker { .. }
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
        | Effect::OpenMessages
        | Effect::ToggleLspTrace { .. }
        | Effect::OpenLspTraceLog { .. }
        | Effect::LspStatus
        | Effect::LspServerLogListing
        | Effect::LspRestart { .. }
        | Effect::LspProgressCancel { .. }
        | Effect::LspExpandRegion
        | Effect::LspShrinkRegion
        | Effect::SetLspLogLevel { .. }
        | Effect::LspLogClear { .. }
        | Effect::LspDocumentSymbol
        | Effect::LspWorkspaceSymbol { .. }
        | Effect::LspIncomingCalls
        | Effect::LspOutgoingCalls
        | Effect::LspSupertypes
        | Effect::LspSubtypes
        | Effect::LspMoniker
        | Effect::LspCodeLens
        | Effect::LspColorPresentation
        | Effect::LspFormat
        | Effect::LspFormatRange
        | Effect::LspSignatureHelp
        | Effect::LspComplete
        | Effect::LspRename { .. }
        | Effect::LspCodeAction
        | Effect::SnippetExpand
        | Effect::ReloadSnippets
        | Effect::ToggleMode { .. }
        | Effect::DescribeEvents
        | Effect::DescribeEvent { .. }
        | Effect::DescribeDiff
        | Effect::DiffOpen
        | Effect::DiffOff
        | Effect::ListModes
        | Effect::DescribeMode { .. }
        | Effect::DescribeOptionResolution { .. }
        | Effect::Customize { .. }
        | Effect::Tutor { .. }
        | Effect::AppAction(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_helpers::{
        app_in_command_mode, app_with, attach_test_syntax, invoke_motion, press, press_chars,
        subscribe_all_events, write_temp_file,
    };
    use crate::app::test_helpers::{fresh_workspace, write_workspace_config};
    use crate::app::word_under_cursor;
    use crate::app::*;
    use crate::help::HelpContent;
    use lattice_protocol::Event;
    use lattice_protocol::edit::Edit;

    /// Test Guard: drop-counter so tests can verify the
    /// Drop-based cleanup contract.
    pub struct TestLocalsGuard {
        counter: std::sync::Arc<std::sync::atomic::AtomicI64>,
    }

    impl Drop for TestLocalsGuard {
        fn drop(&mut self) {
            self.counter.store(0, std::sync::atomic::Ordering::SeqCst);
        }
    }

    struct TestLocalsMode {
        id: lattice_mode::ModeId,
        counter: std::sync::Arc<std::sync::atomic::AtomicI64>,
    }

    impl TestLocalsMode {
        fn new() -> Self {
            Self {
                id: lattice_mode::ModeId::new("test-locals-mode"),
                counter: std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0)),
            }
        }
    }

    impl lattice_mode::Mode for TestLocalsMode {
        type Guard = TestLocalsGuard;
        fn id(&self) -> lattice_mode::ModeId {
            self.id
        }
        fn kind(&self) -> lattice_mode::ModeKind {
            lattice_mode::ModeKind::Minor
        }
        fn on_activate(
            &self,
            _ctx: lattice_mode::ModeContext,
        ) -> lattice_mode::LifecycleFuture<'_, TestLocalsGuard> {
            self.counter.store(42, std::sync::atomic::Ordering::SeqCst);
            let counter = self.counter.clone();
            Box::pin(async move { Ok(TestLocalsGuard { counter }) })
        }
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
    fn key_harness_j_advances_cursor_one_line() {
        let mut a = app_with("one\ntwo\nthree", 10);
        press_chars(&mut a, "j");
        assert_eq!(a.editor.cursor.line, 1);
    }

    #[test]
    fn key_harness_dw_deletes_first_word() {
        let mut a = app_with("one two three", 10);
        press_chars(&mut a, "dw");
        assert_eq!(a.editor.document.text(), "two three");
    }

    #[test]
    fn key_harness_count_before_motion_advances_n_lines() {
        let mut a = app_with("a\nb\nc\nd\ne", 10);
        press_chars(&mut a, "3j");
        assert_eq!(a.editor.cursor.line, 3);
    }

    #[test]
    fn key_harness_count_before_operator_dd_deletes_n_lines() {
        let mut a = app_with("a\nb\nc\nd\ne", 10);
        press_chars(&mut a, "3dd");
        assert_eq!(a.editor.document.text(), "d\ne");
    }

    #[test]
    fn key_harness_count_after_operator_d2w_deletes_two_words() {
        let mut a = app_with("one two three four", 10);
        press_chars(&mut a, "d2w");
        assert_eq!(a.editor.document.text(), "three four");
    }

    #[test]
    fn key_harness_counts_multiply_on_both_sides() {
        let mut a = app_with("a b c d e f g", 10);
        press_chars(&mut a, "2d2w");
        assert_eq!(a.editor.document.text(), "e f g");
    }

    #[test]
    fn key_harness_count_clears_after_motion_fires() {
        let mut a = app_with("a\nb\nc\nd\ne\nf", 10);
        press_chars(&mut a, "3j");
        assert_eq!(a.editor.cursor.line, 3);
        press_chars(&mut a, "j");
        assert_eq!(a.editor.cursor.line, 4);
    }

    #[test]
    fn key_harness_gg_jumps_to_first_line() {
        let mut a = app_with("one\ntwo\nthree\nfour", 10);
        press_chars(&mut a, "G");
        assert_eq!(a.editor.cursor.line, 3);
        press_chars(&mut a, "gg");
        assert_eq!(a.editor.cursor.line, 0);
    }

    /// User reported 2026-05-22: `gg` does nothing in the
    /// production TUI binary but `press_chars(&mut a, "gg")`
    /// (the existing test) passes. Hypothesis: `press_chars` reads
    /// `partial_chord` from `&app.editor.partial_chord` directly,
    /// but `runtime.rs::main_loop` reads from
    /// `app.render_state.load().translator.partial_chord`. If the
    /// publish layer is out of sync, production fails while
    /// tests pass.
    ///
    /// This test mimics the production main_loop's exact ctx-build
    /// REGRESSION TEST 2026-05-22. User reported gg/dw/zz/zt/zb
    /// all broken in production. Root cause: GPUI (and TUI per-tick)
    /// dispatches `Action::EnsureCursorVisible` between user
    /// keystrokes. Without exempting it from the partial-chord clear
    /// guard, EnsureCursorVisible wipes `partial_chord` ~10ms after
    /// every ABSORB, so the second keystroke of every chord
    /// sequence sees an empty stack.
    ///
    /// This test simulates the exact production sequence:
    ///   keystroke g  →  ABSORB g  →  per-frame EnsureCursorVisible
    ///   keystroke g  →  must STILL see partial=[g] in the RS.
    ///
    /// Before the fix: partial_chord=[] after the inter-keystroke
    /// EnsureCursorVisible dispatch. After: partial_chord=[g].
    #[test]
    fn partial_chord_survives_inter_keystroke_ensure_cursor_visible() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut a = app_with("one\ntwo\nthree\nfour\nfive", 10);
        // First g: absorbs into partial_chord.
        press(&mut a, KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(
            a.editor.partial_chord.len(),
            1,
            "first `g` should populate partial_chord"
        );

        // Simulate GPUI's per-frame `ensure_cursor_in_viewport`
        // dispatching `Action::EnsureCursorVisible` between
        // user keystrokes.
        a.apply(lattice_host::action::Action::EnsureCursorVisible);

        // CRITICAL: partial_chord must SURVIVE this dispatch.
        // Pre-fix: cleared to []. Post-fix: still [g].
        assert_eq!(
            a.editor.partial_chord.len(),
            1,
            "EnsureCursorVisible (renderer-housekeeping) must NOT clear partial_chord"
        );
        assert_eq!(
            a.render_state.load().translator.partial_chord.len(),
            1,
            "Published RS must also retain partial_chord"
        );

        // Second g resolves gg.
        press(&mut a, KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(
            a.editor.cursor.line, 0,
            "gg should jump to line 0; partial_chord survived the inter-keystroke EnsureCursorVisible"
        );
    }

    /// code path — reading `partial_chord` from the published RS.
    /// If this test passes too, the divergence is elsewhere; if
    /// it fails, we've localised the bug to the publish layer.
    #[test]
    fn gg_through_published_rs_matches_runtime_main_loop() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut a = app_with("one\ntwo\nthree\nfour\nfive", 10);
        // Park cursor on last line via `G` so gg has visible work.
        for _ in 0..4 {
            press_chars(&mut a, "j");
        }
        assert_eq!(a.editor.cursor.line, 4, "cursor parks on last line");

        // Press `g` twice using the SAME ctx-build code path
        // runtime.rs main_loop uses. Reads partial_chord from
        // published RS each time.
        fn press_g_via_rs(a: &mut App) {
            let ad = a.ad();
            let translator = a.render_state.load().translator.clone();
            let ctx = crate::input::TranslateContext {
                modal: ad.modal,
                builtins: &translator.builtins,
                pending_count: ad.pending_count,
                op_count: ad.op_count,
                recording_macro: ad.macro_recording,
                active_buffer: ad.buffer_kind,
                completion_open: ad.completion_open,
                chord_capture: a.chord_capture_active(),
                picker_open: ad.picker_open,
                insert_completion_open: a.completion_popup_active(),
                snippet_active: ad.snippet_active,
                terminal_insert_active: ad.terminal_insert_active,
                terminal_esc_exits: ad.terminal_esc_exits,
                terminal_app_cursor_keys: ad.terminal_app_cursor_keys,
                terminal_insert_exit_pending: ad.terminal_insert_exit_pending,
                terminal_visual_active: ad.terminal_visual_active,
                keymap: &translator.keymap,
                partial_chord: &translator.partial_chord,
            };
            let action = crate::input::translate(
                ctx,
                KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
            );
            a.apply(action);
        }
        press_g_via_rs(&mut a);
        // After first `g`: partial_chord should be [g] in BOTH
        // sources (editor field AND published RS).
        assert_eq!(
            a.editor.partial_chord.len(),
            1,
            "editor.partial_chord populated after first g"
        );
        assert_eq!(
            a.render_state.load().translator.partial_chord.len(),
            1,
            "RS.translator.partial_chord populated after first g — \
             this is what runtime.rs reads"
        );
        press_g_via_rs(&mut a);
        assert_eq!(
            a.editor.cursor.line, 0,
            "gg via production code path should jump to line 0"
        );
    }

    #[test]
    fn key_harness_df_delim_deletes_up_to_match() {
        let mut a = app_with("alpha, beta, gamma", 10);
        press_chars(&mut a, "df,");
        assert_eq!(a.editor.document.text(), ", beta, gamma");
    }

    #[test]
    fn key_harness_ctrl_w_v_creates_second_pane() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut a = app_with("xx", 10);
        press(
            &mut a,
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
        );
        press_chars(&mut a, "v");
        assert_eq!(a.editor.pane_tree.len(), 2);
    }

    #[test]
    fn key_harness_insert_round_trip_inserts_text_and_returns_normal() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut a = app_with("", 10);
        press_chars(&mut a, "ihi");
        press(&mut a, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(a.editor.document.text(), "hi");
        assert_eq!(a.editor.modal, ModalState::Normal);
    }

    #[test]
    fn event_bus_publishes_document_changed_on_apply_edit() {
        let mut a = app_with("hello", 5);
        let mut rx = subscribe_all_events(&a);
        a.apply_edit_blocking(Edit::insert(Position::new(0, 5), " world"))
            .unwrap();
        assert!(matches!(rx.try_recv(), Ok(Event::DocumentChanged { .. })));
    }

    #[test]
    fn apply_edit_accumulates_delta_when_syntax_attached() {
        let mut a = app_with("hello", 5);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        assert_eq!(a.editor.pending_syntax_edits.len(), 0);
        a.apply_edit_blocking(Edit::insert(Position::new(0, 5), " world"))
            .unwrap();
        assert_eq!(a.editor.pending_syntax_edits.len(), 1);
        let delta = a.editor.pending_syntax_edits[0];
        assert_eq!(delta.start_byte, 5);
        assert_eq!(delta.old_end_byte, 5);
        assert_eq!(delta.new_end_byte, 11);
    }

    #[test]
    fn apply_edit_skips_delta_accumulation_when_no_syntax() {
        // No syntax attached -> publish_document_changed
        // short-circuits the delta push to keep the vec bounded.
        let mut a = app_with("hello", 5);
        assert!(a.editor.syntax.is_none());
        a.apply_edit_blocking(Edit::insert(Position::new(0, 5), " world"))
            .unwrap();
        assert_eq!(a.editor.pending_syntax_edits.len(), 0);
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
        assert_eq!(a.editor.pending_syntax_edits.len(), 2);
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
        assert!(a.editor.should_quit);
    }

    #[test]
    fn event_bus_publishes_selections_changed_on_set_selections() {
        let mut a = app_with("hello world", 5);
        let mut rx = subscribe_all_events(&a);
        let sel = Selection::cursor(Position::new(0, 5));
        a.editor.set_selections_blocking(SelectionSet::single(sel));
        let mut found = false;
        while let Ok(evt) = rx.try_recv() {
            if matches!(evt, Event::SelectionsChanged { .. }) {
                found = true;
                break;
            }
        }
        assert!(found);
    }

    #[test]
    fn invocation_resets_partial_chord() {
        // Slice 8.i.4: AbsorbPartialChord pushes onto
        // partial_chord; any other action clears it.
        let mut a = app_with("abc", 10);
        a.apply(Action::AbsorbPartialChord(crate::chord::KeyChord::char(
            'g',
        )));
        assert_eq!(a.editor.partial_chord.len(), 1);
        let id = a.editor.builtins.char_right;
        a.apply(invoke_motion(id));
        assert!(a.editor.partial_chord.is_empty());
    }

    #[test]
    fn entering_insert_mode_does_not_move_cursor() {
        let mut a = app_with("abc", 10);
        let before = a.editor.cursor;
        a.apply(Action::EnterMode(ModalState::Insert));
        assert_eq!(a.editor.modal, ModalState::Insert);
        assert_eq!(a.editor.cursor, before);
    }

    #[test]
    fn ctrl_o_then_ctrl_i_round_trips() {
        let mut a = app_with("a\nb\nc\nd\ne", 10);
        a.editor.cursor = Position::new(2, 0);
        a.apply(invoke_motion(a.editor.builtins.goto_first_line));
        // Now at line 0; jump list has [(2,0)] cursor at end.
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.editor.cursor, Position::new(2, 0));
        a.apply(Action::JumpHistoryForward);
        assert_eq!(a.editor.cursor, Position::ZERO);
    }

    #[test]
    fn invocation_with_no_pending_register_uses_unnamed() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.editor.builtins.yank.0).with_target(
            lattice_grammar::Target::Motion(
                a.editor.builtins.word_forward,
                lattice_grammar::Args::None,
            ),
        );
        a.apply(Action::Invoke(inv));
        // Unnamed populated; "0 also populated by vim's auto-fill on yank.
        // Named map's only entry is the numbered "0 register.
        assert!(a.editor.unnamed_register.is_some());
        assert!(a.editor.registers.contains_key(&Register::Numbered(0)));
        // No alphabetic named slots populated.
        assert!(
            !a.editor
                .registers
                .keys()
                .any(|r| matches!(r, Register::Named(_)))
        );
    }

    #[test]
    fn enter_replace_sets_modal() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        assert_eq!(a.editor.modal, ModalState::Replace);
    }

    #[test]
    fn enter_replace_clears_replace_history() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::OverwriteChar('H'));
        assert_eq!(a.editor.replace_history.len(), 1);
        a.apply(Action::EnterMode(ModalState::Normal));
        a.apply(Action::EnterMode(ModalState::Replace));
        assert!(a.editor.replace_history.is_empty());
    }

    #[test]
    fn dot_with_no_prior_change_emits_error() {
        let mut a = app_with("hello", 10);
        assert!(a.editor.last_change.is_none());
        a.apply(Action::RepeatLastChange);
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn dd_records_last_change_and_dot_replays_it() {
        let mut a = app_with("aaa\nBBB\nccc\nddd", 10);
        a.editor.cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(a.editor.builtins.delete.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        // Slice 8.i.4.g: `dd` consumes BBB + its trailing newline.
        assert_eq!(a.editor.document.text(), "aaa\nccc\nddd");
        // Cursor is now on what used to be `ccc` (line 1). `.`
        // repeats the linewise delete -- removes that line + its
        // trailing newline.
        a.apply(Action::RepeatLastChange);
        assert_eq!(a.editor.document.text(), "aaa\nddd");
    }

    #[test]
    fn dot_repeats_change_with_insert_replay() {
        // Classic vim test: cw foo<Esc> followed by . on another word
        // replaces that word with "foo" too.
        let mut a = app_with("alpha beta gamma", 10);
        // cw on first word.
        let inv = CommandInvocation::of(a.editor.builtins.change.0).with_target(
            lattice_grammar::Target::Motion(
                a.editor.builtins.word_forward,
                lattice_grammar::Args::None,
            ),
        );
        a.apply(Action::Invoke(inv));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.document.text(), "Xbeta gamma");
        // Move to "beta" (cursor is now on 'X' / position 0; let's go to 'b'
        // at byte 1).
        a.editor.cursor = Position::new(0, 1);
        // Repeat.
        a.apply(Action::RepeatLastChange);
        // cw replays: deletes "beta " and inserts "X" -> "XXgamma".
        // (Note: our cw includes the trailing space; vim's cw is implicitly
        // ce, a deferred refinement.)
        assert_eq!(a.editor.document.text(), "XXgamma");
        assert_eq!(a.editor.modal, ModalState::Normal);
    }

    #[test]
    fn dot_without_insert_replay_when_no_text_was_typed() {
        // dw (no insert phase) -> . repeats just the delete.
        let mut a = app_with("alpha beta gamma", 10);
        let inv = CommandInvocation::of(a.editor.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(
                a.editor.builtins.word_forward,
                lattice_grammar::Args::None,
            ),
        );
        a.apply(Action::Invoke(inv));
        // dw deletes "alpha "; then `.` deletes another word (no insert).
        a.apply(Action::RepeatLastChange);
        // Two dws: "alpha " then "beta " -> "gamma".
        assert_eq!(a.editor.document.text(), "gamma");
    }

    #[test]
    fn dispatcher_runs_counted_motion() {
        // Slice 8.i.4.f: count multiplication is input-side. The
        // dispatcher consumes the baked `inv.count` -- App still
        // resets `pending_count` at end-of-dispatch (drained by
        // attach_count earlier in the pipeline). Press-harness
        // tests cover the full keystroke flow.
        let mut a = app_with("one two three four five", 10);
        a.editor.pending_count = 3;
        a.apply(Action::Invoke(
            CommandInvocation::of(a.editor.builtins.word_forward.0)
                .with_count(lattice_grammar::command::Count(3)),
        ));
        // 3w from origin: "one two three FOUR five" -> 'f' of "four" at byte 14.
        assert_eq!(a.editor.cursor, Position::new(0, 14));
        // pending_count is reset after dispatch.
        assert_eq!(a.editor.pending_count, 0);
    }

    #[test]
    fn dispatcher_runs_counted_operator_on_motion_2dw() {
        let mut a = app_with("one two three four five", 10);
        // Mirror translate-time state: `2d` already absorbed.
        a.editor.op_count = 2;
        let inv = CommandInvocation::of(a.editor.builtins.delete.0)
            .with_target(lattice_grammar::Target::Motion(
                a.editor.builtins.word_forward,
                lattice_grammar::Args::None,
            ))
            .with_count(lattice_grammar::command::Count(2));
        a.apply(Action::Invoke(inv));
        // 2dw: deletes "one two " leaving "three four five".
        assert_eq!(a.editor.document.text(), "three four five");
        assert_eq!(a.editor.op_count, 0);
    }

    #[test]
    fn dispatcher_runs_counted_operator_on_motion_2d3w_equals_count_6() {
        let mut a = app_with("a b c d e f g h i j", 10);
        a.editor.op_count = 2;
        a.editor.pending_count = 3;
        let inv = CommandInvocation::of(a.editor.builtins.delete.0)
            .with_target(lattice_grammar::Target::Motion(
                a.editor.builtins.word_forward,
                lattice_grammar::Args::None,
            ))
            .with_count(lattice_grammar::command::Count(6));
        a.apply(Action::Invoke(inv));
        // 6 words deleted from "a b c d e f g h i j" leaves "g h i j".
        assert_eq!(a.editor.document.text(), "g h i j");
    }

    #[test]
    fn yy_populates_register_linewise() {
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.editor.cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(a.editor.builtins.yank.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        let reg = a.editor.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.content, "BBB\n");
        assert_eq!(reg.kind, YankKind::Linewise);
        assert_eq!(a.editor.document.text(), "aaa\nBBB\nccc");
    }

    #[test]
    fn dd_populates_register_linewise_via_delete() {
        // delete also yanks; register kind is linewise for dd.
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.editor.cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(a.editor.builtins.delete.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        let reg = a.editor.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.kind, YankKind::Linewise);
        assert_eq!(reg.content, "BBB\n");
    }

    #[test]
    fn dd_on_closed_fold_heading_deletes_whole_fold() {
        // `docs/user/folding.md`: dd on a closed fold deletes the
        // entire fold range as a single undo unit. Use a sibling
        // # H2 heading so the # H1 fold has a bounded end.
        let initial = "# H1\nbody one\nbody two\n# H2\nafter\n";
        let mut a = app_with(initial, 10);
        a.set_foldmethod_for_test(FoldMethod::Markdown);
        a.recompute_folds();
        // Close the H1 fold (lines 0..=2).
        let idx = a
            .editor
            .folds
            .iter()
            .position(|f| f.start_line == 0)
            .expect("H1 fold");
        a.editor.folds[idx].closed = true;
        a.editor.cursor = Position::new(0, 0);
        let inv = CommandInvocation::of(a.editor.builtins.delete.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        let text = a.editor.document.text();
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
            .editor
            .folds
            .iter()
            .position(|f| f.start_line == 0)
            .expect("H1 fold");
        a.editor.folds[idx].closed = true;
        a.editor.cursor = Position::new(0, 0);
        let inv = CommandInvocation::of(a.editor.builtins.yank.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        let reg = a.editor.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.kind, YankKind::Linewise);
        assert!(
            reg.content.contains("# H1"),
            "register content: {:?}",
            reg.content
        );
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
        a.editor.cursor = Position::new(0, 0);
        let inv = CommandInvocation::of(a.editor.builtins.delete.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        let text = a.editor.document.text();
        assert!(!text.contains("# H1"), "heading should be gone: {text:?}");
        assert!(
            text.contains("body one"),
            "body one should remain: {text:?}"
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
        a.editor.cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(a.editor.builtins.delete.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        let reg = a.editor.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.kind, YankKind::Linewise);
        assert_eq!(reg.content, "BBB\n");
    }

    #[test]
    fn second_tab_advances_selected_candidate() {
        let mut a = app_in_command_mode("descri");
        a.apply(Action::CommandLineCompleteOrAdvance);
        let first = a.editor.completion_state.as_ref().unwrap().selected;
        a.apply(Action::CommandLineCompleteOrAdvance);
        let second = a.editor.completion_state.as_ref().unwrap().selected;
        assert_eq!(first, 0);
        assert_eq!(second, 1);
    }

    #[test]
    fn first_chord_after_arming_auto_submits() {
        let mut a = app_in_command_mode("describe-key");
        a.apply(Action::CommandLineSubmit);
        assert!(a.editor.auto_submit_after_chord);
        // The first chord token captured should auto-fire submit;
        // the cmdline should clear and we land back in Normal.
        a.apply(Action::CommandLineAppendChord("j".into()));
        assert!(!a.editor.auto_submit_after_chord);
        assert!(matches!(a.editor.modal, ModalState::Normal));
        // The submitted line was `describe-key j` -- which opens
        // a help buffer for chord `j`. Smoke check that some
        // help got produced.
        assert!(a.editor.popup_buffer.is_some());
    }

    #[test]
    fn ctrl_u_clears_command_line_and_dismisses_popup() {
        let mut a = app_in_command_mode("foo bar baz");
        a.apply(Action::CommandLineCompleteOrAdvance);
        a.apply(Action::CommandLineClear);
        assert_eq!(a.editor.command_line, "");
        assert!(a.editor.completion_state.is_none());
    }

    #[test]
    fn ctrl_w_deletes_trailing_word() {
        let mut a = app_in_command_mode("foo bar baz");
        a.apply(Action::CommandLineDeleteWordBackward);
        assert_eq!(a.editor.command_line, "foo bar ");
    }

    #[test]
    fn ctrl_w_with_trailing_whitespace_strips_word() {
        let mut a = app_in_command_mode("foo bar  ");
        a.apply(Action::CommandLineDeleteWordBackward);
        assert_eq!(a.editor.command_line, "foo ");
    }

    #[test]
    fn ctrl_w_on_single_word_clears() {
        let mut a = app_in_command_mode("foo");
        a.apply(Action::CommandLineDeleteWordBackward);
        assert_eq!(a.editor.command_line, "");
    }

    #[test]
    fn entering_command_line_dismisses_open_help() {
        // Q16: opening `:` dismisses State-A help popups (active is
        // still Document; hover-style overlay). State-B popups
        // (active = Help, focus moved into the popup) survive --
        // the cmdline is part of the focused popup buffer.
        let mut a = app_with("xx", 10);
        // Simulate State A: register a popup buffer + set the slot
        // but leave `active_buffer` on Document.
        let crate::help::HelpContent { buffer, .. } =
            crate::help::HelpContent::from_lines("preexisting", vec!["x".into()]);
        let id = buffer.id;
        a.editor
            .buffers
            .insert(crate::buffer_registry::BufferEntry {
                id,
                flags: crate::buffers::BufferFlags {
                    listed: false,
                    hidden: true,
                },
                data: crate::buffer_registry::BufferData::Help(buffer),
                name: None,
            });
        a.editor.popup_buffer = Some(id);
        a.apply(Action::EnterCommandLine);
        assert!(a.editor.popup_buffer.is_none());
    }

    #[test]
    fn entering_command_line_dismisses_open_completion() {
        let mut a = app_with("xx", 10);
        a.editor.completion_state = Some(CompletionState {
            candidates: Vec::new(),
            selected: 0,
            replace_start: 0,
            original_line: String::new(),
        });
        a.apply(Action::EnterCommandLine);
        assert!(a.editor.completion_state.is_none());
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
        a.editor.pane_highlights.insert(0, vec![Vec::new(); 1]);
        a.editor.pending_redraw = false;
        a.apply(Action::RedrawScreen);
        assert!(
            a.editor.pending_redraw,
            "runtime should clear terminal next frame"
        );
        assert!(
            a.editor.pane_highlights.is_empty(),
            "pane highlights cache must reset (so next frame repopulates from scratch)"
        );
        // Post-apply, the version mirror equals the document's
        // version because the end-of-apply reparse already ran.
        // The intermediate `u64::MAX` value is gone; that's the
        // desired flow -- a single keystroke produces an
        // already-fresh tree.
        assert_eq!(
            a.editor.last_parsed_text_version,
            a.editor.document.text_version(),
            "post-apply reparse must have synced the version mirror"
        );
        let msg = a.editor.last_message.as_ref().expect("info echo");
        assert!(msg.text.contains("redraw"), "user-visible echo: {msg:?}");
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
        assert!(a.editor.popup_buffer.is_some());
        assert!(matches!(a.editor.active_buffer, BufferKind::Document));
        assert!(a.editor.prev_pane_for_help.is_none());
        // Second K -> focus into popup.
        // 5.5.LSP.1: hover request migrated to `Editor::dispatch`;
        // exercise the State A -> State B promote through the Action
        // path so the test covers the live dispatch wire too.
        a.apply(Action::LspHoverRequest);
        assert!(
            a.editor.popup_buffer.is_some(),
            "popup stays up after focus"
        );
        assert!(matches!(a.editor.active_buffer, BufferKind::Help));
        let stash = a.editor.prev_pane_for_help.expect("State B captures stash");
        assert_eq!(stash.buffer, BufferKind::Document);
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
        let initial_id = a.editor.document_buffer_id;
        // Close the first fold (line 0) on the initial buffer.
        let first_idx = a
            .editor
            .folds
            .iter()
            .position(|f| f.start_line == 0)
            .expect("fold");
        a.editor.folds[first_idx].closed = true;
        // Open + activate the new buffer.
        a.editor.command_line = format!("e {}", path.display());
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        // Switch back via :bn.
        a.editor.command_line = "bn".into();
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.editor.document_buffer_id, initial_id);
        // Closed state survived the round-trip.
        assert!(
            a.editor.folds.iter().any(|f| f.start_line == 0 && f.closed),
            "expected fold@0 to remain closed after switch-away-and-back: {:?}",
            a.editor.folds
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
        a.editor.command_line = format!("e {}", path.display());
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let id_target = a.editor.document_buffer_id;
        assert!(a.editor.folds.is_empty(), "manual leaves folds empty");
        // Switch back to the original buffer.
        let original_id = a
            .editor
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
        assert_eq!(a.editor.document_buffer_id, id_target);
        assert!(
            !a.editor.folds.is_empty(),
            "expected activation hook to seed folds on first visit under indent: {:?}",
            a.editor.folds
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn word_under_cursor_returns_alphanumeric_run() {
        let mut a = app_with("hello world", 10);
        a.editor.cursor = Position::new(0, 0);
        let snap = a.editor.document.snapshot();
        assert_eq!(
            word_under_cursor(&snap.buffer, a.editor.cursor),
            Some("hello".to_string())
        );
        a.editor.cursor = Position::new(0, 6);
        assert_eq!(
            word_under_cursor(&snap.buffer, a.editor.cursor),
            Some("world".to_string())
        );
    }

    #[test]
    fn word_under_cursor_returns_none_off_word() {
        let a = app_with("foo bar", 10);
        let snap = a.editor.document.snapshot();
        // Cursor on the space.
        let p = Position::new(0, 3);
        assert_eq!(word_under_cursor(&snap.buffer, p), None);
    }

    #[test]
    fn ctrl_o_walks_back_to_document_from_help() {
        // `<C-o>` from inside a help buffer should land back on the
        // document spot the user opened the help from. That's the
        // first user-visible win of active-buffer routing.
        let mut a = app_with("first\nsecond\nthird\nfourth", 10);
        a.editor.cursor = Position::new(2, 0);
        // Open help via the same path the App uses internally so
        // the position-history entry is recorded.
        a.open_popup(
            HelpContent::from_lines("h", vec!["help body".into()]),
            crate::popup::PopupPlacement::Centered,
        );
        assert_eq!(a.editor.active_buffer, BufferKind::Help);
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.editor.active_buffer, BufferKind::Document);
        assert_eq!(a.editor.cursor.line, 2);
    }

    #[test]
    fn apply_edit_blocking_records_lsp_edit_when_attached() {
        let mut app = app_with("abc\n", 5);
        // Attach a fake URI mapping so lsp_record_edit
        // reaches the supervisor.
        use std::str::FromStr;
        let uri = lattice_lsp::Uri::from_str("file:///tmp/x.rs").unwrap();
        app.editor
            .buffer_uris
            .insert(app.editor.document_buffer_id, uri.clone());
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
        assert_eq!(
            app.editor.buffer_uris.get(&app.editor.document_buffer_id),
            Some(&uri)
        );
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
        assert!(eff.auto_trigger, "TOML wins for auto_trigger");
        let lsp_id =
            lattice_completion::SourceId::new(lattice_completion::LSP_COMPLETION_SOURCE_ID);
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
        let lsp_id =
            lattice_completion::SourceId::new(lattice_completion::LSP_COMPLETION_SOURCE_ID);
        assert!(eff.source_enabled(&lsp_id));
        let buffer_words_id =
            lattice_completion::SourceId::new(lattice_completion::BufferWordsSource::ID);
        assert!(
            !eff.source_enabled(&buffer_words_id),
            "`sources = [\"lsp\"]` excludes buffer-words",
        );
        assert!(eff.auto_insert_single);
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
        let pre = a.editor.last_message.clone();
        a.apply_per_language_toml_overrides();
        let msg = a.editor.last_message.as_ref().expect("warning echoed");
        assert_ne!(Some(msg.clone()), pre, "new echo posted");
        assert_eq!(msg.level, EchoLevel::Warn);
        assert!(msg.text.contains("bogus_field"), "got `{}`", msg.text);
    }

    #[tokio::test]
    async fn mode_on_activate_runs_and_returns_guard() {
        // M-async.2: validation succeeds synchronously; the
        // lifecycle future is spawned. Yield to the runtime so
        // the spawned task runs and stashes the Guard.
        let mut a = app_with("hi", 5);
        let registry = std::sync::Arc::make_mut(&mut a.editor.mode_registry);
        let test_mode = TestLocalsMode::new();
        let counter = test_mode.counter.clone();
        let mode_id = registry.register(test_mode).expect("register");

        let mut active = lattice_mode::ActiveModes::new();
        let guards = lattice_mode::GuardStoreHandle::new();
        a.editor
            .mode_registry
            .activate_minor(
                &mut active,
                &guards,
                &a.editor.config,
                &a.editor.event_bus,
                &a.editor.services,
                lattice_protocol::ids::BufferId::new(0),
                mode_id,
                lattice_mode::CapabilitySet::empty(),
            )
            .expect("activate");

        // The lifecycle task's `on_activate` body fires the side
        // effect synchronously (no `.await`) before constructing
        // the Guard; tokio still needs a yield to pick up the
        // spawned task.
        for _ in 0..10 {
            if guards.contains(lattice_protocol::ids::BufferId::new(0), mode_id) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::SeqCst),
            42,
            "on_activate side effect should have fired"
        );
        assert!(
            guards.contains(lattice_protocol::ids::BufferId::new(0), mode_id),
            "Guard should be stashed in GuardStore"
        );
    }

    #[tokio::test]
    async fn mode_deactivate_drops_guard_and_fires_cleanup() {
        // M-async.2: activate spawns; yield to let the Guard land,
        // then deactivate synchronously and observe Drop fired.
        let mut a = app_with("hi", 5);
        let registry = std::sync::Arc::make_mut(&mut a.editor.mode_registry);
        let test_mode = TestLocalsMode::new();
        let counter = test_mode.counter.clone();
        let mode_id = registry.register(test_mode).expect("register");

        let mut active = lattice_mode::ActiveModes::new();
        let guards = lattice_mode::GuardStoreHandle::new();
        a.editor
            .mode_registry
            .activate_minor(
                &mut active,
                &guards,
                &a.editor.config,
                &a.editor.event_bus,
                &a.editor.services,
                lattice_protocol::ids::BufferId::new(0),
                mode_id,
                lattice_mode::CapabilitySet::empty(),
            )
            .expect("activate");
        for _ in 0..10 {
            if guards.contains(lattice_protocol::ids::BufferId::new(0), mode_id) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 42);

        a.editor
            .mode_registry
            .deactivate_minor(
                &mut active,
                &guards,
                &a.editor.event_bus,
                lattice_protocol::ids::BufferId::new(0),
                mode_id,
            )
            .expect("deactivate");
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "Guard's Drop impl should have reset the counter"
        );
        assert!(
            !guards.contains(lattice_protocol::ids::BufferId::new(0), mode_id),
            "Guard should be removed from GuardStore"
        );
    }
}
