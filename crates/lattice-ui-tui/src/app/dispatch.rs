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
//!   read-only guard's allow-list inversion).
//! - `echo_level_from_grammar` (grammar-level → host-level
//!   echo-level translator).
//! - `COMMAND_HISTORY_CAP` (the `:`-history capacity).
//!
//! What does NOT live here: the per-feature `do_*` methods the
//! match arms call. Those live in their feature modules; this
//! is the routing layer over them.

use lattice_core::buffer::AppliedEdit;
use lattice_grammar::ModalState;
use lattice_grammar::command::CommandInvocation;
use lattice_grammar::effect::Effect;
use lattice_protocol::Event;
use lattice_protocol::selection::{Selection, SelectionSet};
use lattice_runtime::{CancellationToken, RuntimeError, block_on};

use super::{
    Action, App, BufferKind, EchoLevel, FindKind, LastFind, LspNavKind, PositionSource,
    SearchLine, is_valid_mark_name, visual,
};
use crate::excommand;
use crate::pane::SplitOrientation;

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

impl App {
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

    pub(super) fn execute_ex_line(&mut self, line: &str) {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
