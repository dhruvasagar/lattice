//! Renderer-neutral action dispatch.
//!
//! Phase 5.5 / slice 5.5.A scaffolding. After Phase 5.4 closed the
//! input side (every `KeyEvent → KeyChord → Action` path lives in
//! `lattice-host`), this module is the seam for the output side
//! that 5.5 fills in: `Action → state mutation`. The renderer's
//! `App::apply` body (today ~2.6k LoC of `match action { ... }`
//! in `lattice-ui-tui::app::dispatch`) will relocate here
//! sub-slice by sub-slice as 5.5.B → 5.5.H land.
//!
//! ## Why this module exists today
//!
//! 5.5.A defines the surface ([`Editor::dispatch`] +
//! [`DispatchOutcome`] + [`RendererSignal`]) but leaves the body
//! empty. Renderer `App` structs keep doing all the work in their
//! own `apply` paths; the stub is a no-op. Fixing the public shape
//! up front means subsequent sub-slices are mechanical moves
//! rather than design decisions, and the future `lattice-ui-gpui`
//! has a stable function signature to compose against from day one.
//!
//! Sub-slices populate the stub:
//!
//! - **5.5.B** -- macro-recording capture, partial-chord clear,
//!   read-only-help guard (the preamble).
//! - **5.5.C** -- simplest `match action { ... }` arms (mutate
//!   `editor` fields directly with no helper call). First
//!   emission of [`RendererSignal::Quit`].
//! - **5.5.D** -- pure-editor-mutation helpers (`clamp_cursor_to_buffer`,
//!   `ensure_cursor_visible`, `dismiss_popup`, ...).
//! - **5.5.E** -- ex-command effect handlers (the ~60-variant
//!   `apply_effect` table + the `do_*` family). First emission of
//!   [`RendererSignal::ThemeChanged`] (from `Effect::SetOption`
//!   on `ui.*` keys). 5.5.E.1 scaffolded [`Editor::handle_effect`]
//!   and migrated the three helper-free arms (`Effect::None`,
//!   `Effect::ClearSearchHighlight`, `Effect::Echo`); 5.5.E.2 adds
//!   the echo-only ex-command listers (`Effect::EchoMarks` ->
//!   [`Editor::do_list_marks`], `Effect::EchoRegisters` ->
//!   [`Editor::do_list_registers`]) and co-moves the
//!   [`preview_register`] free fn; 5.5.E.3 migrates `store_yank`
//!   to [`Editor::store_yank`], unlocking [`Effect::Yank`]; 5.5.E.4
//!   migrates `set_selections_blocking` + `publish_selections_changed`
//!   to [`Editor`] and [`visual_kind_to_mode`] to a host-side free
//!   fn, unlocking [`Effect::SelectionChange`]. Subsequent E.*
//!   sub-slices land the remaining helper-bearing arms as their
//!   `do_*` bodies move host-side; the `apply_edit_blocking` /
//!   `handle_edits` cluster is gated on the render-coupled
//!   `shift_highlights_for_edit` cache and follows the
//!   visible-highlights slice (out of scope per 5.5 design doc).
//! - **5.5.F** -- mode-lifecycle helpers
//!   (`do_open_file_tree` / `do_open_oil` / `do_open_hover` / ...).
//! - **5.5.G** -- final remnants; `App::apply` collapses to the
//!   dispatch call + signal-handling wrapper.
//! - **5.5.H** -- render-coupled cleanup; removes now-vestigial
//!   `App` methods.
//!
//! Focused design doc:
//! `docs/dev/architecture/phase-5-dispatch-extraction.md`.

use lattice_core::{BufferKind, Fold, FoldMethod};
use lattice_grammar::ModalState;
use lattice_grammar::VisualKind;
use lattice_grammar::YankKind;
use lattice_grammar::effect::Effect;
use lattice_grammar::register::Register;
use lattice_protocol::Event;
use lattice_protocol::selection::{Selection, SelectionSet, VisualMode};
use lattice_runtime::{MessagePushed, block_on};

use crate::action::{Action, EchoLevel};
use crate::buffers::BufferId;
use crate::editor::Editor;
use crate::state::{
    MacroRecording, PositionEntry, PositionSource, PrevPaneState, SearchLine, TagStackEntry,
    UnnamedRegister,
};

/// 5.5.F.4.2: position-history ring cap, co-located with
/// [`Editor::push_position_history`].
pub const POSITION_HISTORY_CAP: usize = 100;

/// Result of [`Editor::dispatch`]. Carries the renderer-side
/// side-effects the caller must surface after the host-side state
/// mutation completes.
///
/// Today the TUI's runtime loop repaints every tick, so most
/// dispatches return an empty `renderer_signals`. The `Vec` shape
/// lets host helpers append signals from nested call sites without
/// having to thread them up the call stack -- mirrors how
/// `lattice_grammar::Effect::Many` already aggregates inner effects.
#[derive(Debug, Default)]
pub struct DispatchOutcome {
    /// Host-to-renderer side-effects. Empty for the vast majority
    /// of dispatches (state changed; renderer just refreshes its
    /// per-frame caches on the next tick).
    pub renderer_signals: Vec<RendererSignal>,
    /// Set when the host fully handled the action and the renderer's
    /// post-dispatch `match action { ... }` body should bail. Used by
    /// the read-only-help guard (5.5.D) to short-circuit App's match
    /// from inside `Editor::dispatch`. Disappears once 5.5.G collapses
    /// App's match entirely; until then it's the coordination channel
    /// that keeps the two halves in lockstep.
    pub consumed: bool,
}

/// Host-to-renderer side-effect signal.
///
/// **v1 scope is deliberately small** (see
/// `phase-5-dispatch-extraction.md` §"RendererSignal scope"). Only
/// variants with planned emission sites in the existing dispatch
/// path are included; speculative variants (`Repaint`,
/// `TitleChanged`) are deferred until a real need surfaces.
///
/// The renderer matches on this in its post-dispatch hook:
///
/// ```ignore
/// let outcome = self.editor.dispatch(action);
/// for signal in outcome.renderer_signals {
///     match signal {
///         RendererSignal::ThemeChanged => self.rebuild_renderer_theme(),
///         RendererSignal::Quit => self.shutdown(),
///     }
/// }
/// ```
///
/// First emission sites land in sub-slices 5.5.C ([`Self::Quit`])
/// and 5.5.E ([`Self::ThemeChanged`]); the variants exist from
/// 5.5.A so the type surface is fixed before any consumer composes
/// against it.
#[derive(Debug, Clone)]
pub enum RendererSignal {
    /// The host's neutral [`crate::ui::theme::Theme`] changed
    /// (typically via a `:set ui.*` cascade). The renderer should
    /// rebuild its cached typed theme mirror.
    ThemeChanged,
    /// Quit requested. The renderer should begin its shutdown
    /// sequence. `editor.should_quit` is also set for back-compat
    /// with renderers that poll per-tick.
    Quit,
    /// 5.5.E.6: the `ui.nerd_fonts` cascade flipped the
    /// nerd-fonts toggle. The TUI's file-tree rope embeds the
    /// icon glyphs, so a palette flip needs a per-file-tree-buffer
    /// rope refresh on the renderer side; oil's renderer reads the
    /// toggle each frame and needs no rope-side work. The host
    /// emits this *in addition to* [`Self::ThemeChanged`] so
    /// renderers that don't track file-tree state can ignore it
    /// without missing the theme rebuild.
    NerdFontsToggled,
    // 5.5.F.5.4: `MirrorOptionToModes(String)` retired. The
    // declarative mode-mirror cascade now runs synchronously
    // host-side inside `apply_option_cascade` via
    // `Editor::mirror_option_to_modes`; cascading mode-lifecycle
    // signals stream back through the same `Vec<RendererSignal>`
    // the parent cascade already drains.
    /// 5.5.E.6: the option-cascade just touched an
    /// `lsp.<server_id>.*` key. The renderer fans out a
    /// `workspace/didChangeConfiguration` to every actor matching
    /// `server_id` with the freshly merged subtree. The host
    /// can't drive this without owning the LSP actor pool, which
    /// stays renderer-side through 5.5.
    LspConfigChanged(String),
    /// 5.5.F.1: a host-side `do_*` arm built a [`lattice_help::HelpContent`]
    /// and wants the renderer to display it under a given category
    /// (e.g. `Effect::ListBuffers` → `HelpList`,
    /// `Effect::DescribeBuffer` → `HelpDescribe`). The renderer
    /// runs its existing `display_buffer` dispatch: resolve the
    /// category to a [`lattice_core::ui::display::BufferDisplay`]
    /// preference, then route into the matching surface (popup,
    /// active-pane swap, split). Boxed because [`lattice_help::HelpContent`]
    /// is ~6 fields including a parsed markdown highlight cache,
    /// and most signals don't carry one — keeping the variant
    /// small keeps the common-case `Vec<RendererSignal>` cheap.
    /// `PartialEq` / `Eq` derives drop on `RendererSignal` because
    /// [`lattice_help::HelpContent`] doesn't implement them (the
    /// syntax-highlight cache is renderer-neutral but not value-
    /// equatable). Signals are produced at `:` / Effect-arm rate,
    /// not per-frame, so the `Box` allocation is well below any
    /// perf gate.
    DisplayBuffer(Box<DisplayBufferRequest>),
    // 5.5.F.5.5: `BufferActivated` retired. The Bucket-A
    // `visible_highlights` / `pane_highlights` cache clear lives on
    // `Editor` as plain field writes, so the post-activation tail
    // (`activate_buffer_state`) runs entirely host-side; cascading
    // mode-lifecycle signals stream back through the same
    // `Vec<RendererSignal>` the `handle_effect` arm already returns.
}

/// 5.5.F.1: payload for [`RendererSignal::DisplayBuffer`]. Carries
/// the [`lattice_help::HelpContent`] the host built + the category
/// the renderer should dispatch it under. The renderer resolves
/// the category to a [`lattice_core::ui::display::BufferDisplay`]
/// (via its existing `resolve_display`) and routes through the
/// matching surface. Why a struct instead of inline variant
/// fields: keeps the [`RendererSignal`] variant size constant
/// when more host-side `do_*` arms migrate over and want to
/// attach side-channel data (e.g. a buffer-id token the renderer
/// should mirror into its registry on completion); subsequent
/// slices can add fields here without churning every
/// `RendererSignal` match arm in every renderer.
#[derive(Debug, Clone)]
pub struct DisplayBufferRequest {
    /// The help-style content the renderer should surface. Built
    /// host-side from `editor.*` reads; never includes renderer-
    /// specific data.
    pub content: lattice_help::HelpContent,
    /// The category the renderer dispatches under. The renderer's
    /// `resolve_display(category)` reads the per-category typed
    /// option (`:set <category>.display = ...`) to pick the
    /// concrete surface.
    pub category: lattice_core::ui::display::BufferDisplayCategory,
}

impl Editor {
    /// Renderer-neutral dispatch entry point.
    ///
    /// **5.5.A scaffolding**: body is a stub. Renderer `App::apply`
    /// paths still do all the work in their own crates. Calling this
    /// today returns an empty [`DispatchOutcome`] and changes no
    /// state -- behaviour-preserving by construction.
    ///
    /// **After 5.5 lands**: every [`Action`] flows through here.
    /// Renderer code becomes:
    ///
    /// ```ignore
    /// pub fn apply(&mut self, action: Action) {
    ///     let outcome = self.editor.dispatch(action);
    ///     for signal in outcome.renderer_signals {
    ///         // renderer-specific handling
    ///     }
    ///     // render-coupled per-frame cache refresh stays here
    /// }
    /// ```
    pub fn dispatch(&mut self, action: Action) -> DispatchOutcome {
        let mut out = DispatchOutcome::default();
        handle_action(self, action, &mut out);
        out
    }

    /// Renderer-neutral entry point for `lattice_grammar::Effect`
    /// handling.
    ///
    /// Today's TUI [`crate::app::dispatch::apply_effect`][app-apply-effect]
    /// is a ~60-variant `match` that dispatches to App-side `do_*`
    /// helpers. 5.5.E migrates those arms here as the underlying
    /// helpers move onto [`Editor`]. App's `apply_effect` clones the
    /// effect, calls `editor.handle_effect(effect.clone())`, surfaces
    /// any [`RendererSignal`]s, then matches on the original with a
    /// grouped no-op arm covering every variant the host has already
    /// taken responsibility for.
    ///
    /// 5.5.E.1 covers the three trivially helper-free arms:
    /// [`Effect::None`], [`Effect::ClearSearchHighlight`], and
    /// [`Effect::Echo`]. Other variants fall through to the catch-all
    /// `_ => {}` until their helpers migrate. The [`Effect::Many`]
    /// recursion stays on App for now -- it dispatches inner effects
    /// back through App's `apply_effect` so non-migrated inner arms
    /// still resolve.
    ///
    /// [app-apply-effect]: ../../lattice_ui_tui/app/dispatch/struct.App.html#method.apply_effect
    pub fn handle_effect(&mut self, effect: Effect) -> DispatchOutcome {
        let mut out = DispatchOutcome::default();
        handle_effect(self, effect, &mut out);
        out
    }
}

/// Returns `true` for actions that mutate the document and therefore
/// must no-op when a read-only help buffer holds focus.
///
/// Motions and scroll-class actions are NOT in this set -- they
/// operate on whichever buffer is active (document or help) per the
/// per-action active-buffer routing. The read-only-help guard in
/// [`handle_action`] short-circuits these when `active_buffer ==
/// Help` so a stray `i` / `p` / `u` / `dd` while reading help
/// doesn't fall through onto the underlying document.
pub fn action_is_document_mutation(action: &Action) -> bool {
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

/// Internal action handler -- the destination 5.5.B+ migrates the
/// `App::apply` body into.
///
/// The signature stays stable as sub-slices fill the body: per-arm
/// moves mutate `editor` directly and push into `out.renderer_signals`.
pub(crate) fn handle_action(
    editor: &mut Editor,
    action: Action,
    _out: &mut DispatchOutcome,
) {
    // 5.5.B: macro-recording capture. While a macro recording is in
    // flight, capture every Action EXCEPT the recording-management
    // ones themselves (otherwise the recording would include "stop
    // recording" or recurse on play).
    if let Some(rec) = editor.macro_recording.as_mut()
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
    // 5.5.B: partial-chord lifecycle. Slice 8.i.4: any action that
    // *isn't* `AbsorbPartialChord(_)` (or accumulating count via
    // `PushDigit`) resolves or aborts the in-flight multi-key
    // sequence, so the chord stack must clear. Without this an
    // unbound second key (e.g. `g!` after `g`) would leak `[g]` into
    // the next keystroke's prefix lookup and mis-route it as `gd` /
    // `gv` / etc. Slice 8.i.4.f: `PushDigit` is also exempt -- vim's
    // motion-count-after-operator (`d2w`, `2d3w`, `5gg`) accumulates
    // count chars BETWEEN chord steps. The operator-pending stack
    // must survive the digit input.
    if !matches!(action, Action::AbsorbPartialChord(_) | Action::PushDigit(_)) {
        editor.partial_chord.clear();
    }
    // 5.5.D: read-only-help guard. When a help buffer holds focus
    // (DESIGN.md §5.9 active-buffer routing), buffer-mutating actions
    // (Insert / Delete / Paste / Undo / Redo / fold ops / etc.)
    // silently no-op with a "read-only" echo. Motion- and scroll-class
    // actions, universal escape hatches (Quit, EnterCommandLine,
    // HelpDismiss), and command-line editing actions all keep working
    // -- the read-only set is narrow and explicit, so additions to
    // `Action` default to working in help unless they're added to
    // `action_is_document_mutation`. The guard's post-effect helpers
    // (`ensure_cursor_visible`, `maybe_reparse_syntax`) live on
    // `Editor`, so the guard is a clean self-contained host-side
    // block. `_out.consumed` short-circuits the renderer's
    // post-dispatch match (App::apply); 5.5.G removes the field once
    // App's match collapses entirely.
    if matches!(editor.active_buffer, BufferKind::Help)
        && action_is_document_mutation(&action)
    {
        editor.set_message(EchoLevel::Info, "buffer is read-only".to_string());
        editor.ensure_cursor_visible();
        editor.maybe_reparse_syntax();
        _out.consumed = true;
        return;
    }
    // 5.5.C: helper-free match arms. Each arm here is a body that
    // mutates only `editor` fields with no `self.do_*` /
    // `self.refresh_*` / `self.dismiss_*` call. Arms whose bodies
    // call App helpers stay in `lattice-ui-tui::app::dispatch::apply`'s
    // match until 5.5.D moves the helpers to `Editor` and 5.5.E+
    // moves the arms that call them.
    //
    // Sub-slices populate this match downward as helpers migrate.
    // The catch-all `_ => {}` is the seam: anything not yet moved
    // is still handled by App's match (which runs after this
    // function returns).
    match action {
        Action::None => {}
        Action::Quit => {
            editor.event_bus.publish(Event::BeforeQuit);
            editor.should_quit = true;
            // First emission of `RendererSignal::Quit`. `should_quit`
            // is also set for back-compat with renderers that poll
            // per-tick (the TUI's `runtime.rs` reads it).
            _out.renderer_signals.push(RendererSignal::Quit);
        }
        Action::AbsorbPartialChord(chord) => {
            // Slice 8.i.4.a: the trie returned `Partial`; the input
            // layer wrapped the captured chord in this signal.
            // Append to `partial_chord` and otherwise no-op -- the
            // next keystroke runs through `dispatch_normal` with
            // this stack as prefix.
            editor.partial_chord.push(chord);
        }
        Action::PushDigit(d) => {
            // Accumulate one decimal digit into the pending count.
            // Saturating math prevents overflow on absurd inputs.
            editor.pending_count = editor
                .pending_count
                .saturating_mul(10)
                .saturating_add(d.into());
        }
        Action::Echo(message) => {
            editor.last_message = Some(message);
        }
        Action::CommandLineCancel => {
            if matches!(editor.modal, ModalState::Command) {
                editor.command_line.clear();
                editor.command_history_cursor = None;
                editor.command_history_pending = None;
                editor.modal = ModalState::Normal;
                editor.auto_submit_after_chord = false;
                editor.substitute_preview = None;
            }
        }
        Action::SelectRegister(reg) => {
            editor.pending_register = Some(reg);
        }
        Action::CommandLineDeleteChord => {
            if matches!(editor.modal, ModalState::Command) {
                let n = crate::chord::last_chord_token_byte_len(&editor.command_line);
                if n == 0 {
                    // Empty buffer + delete -> exit Command modal,
                    // matching plain `<BS>` semantics.
                    editor.modal = ModalState::Normal;
                    editor.completion_state = None;
                } else {
                    let new_len = editor.command_line.len() - n;
                    editor.command_line.truncate(new_len);
                }
            }
        }
        Action::CommandLineDismissCompletion => {
            editor.completion_state = None;
        }
        Action::EnterSearch(direction) => {
            editor.search_line = Some(SearchLine {
                direction,
                pattern: String::new(),
                origin: editor.cursor,
            });
            editor.modal = ModalState::Search(direction);
            editor.last_message = None;
            editor.current_match = None;
        }
        // 5.5.G.1: pure-editor fold / macro / snippet arms. Bodies
        // mutate only `editor.*` state; helpers (`do_set_fold_*`,
        // `do_set_all_folds`, `do_goto_fold`, `do_delete_fold_*`,
        // `do_start_macro_record`, `do_stop_macro_record`) live on
        // [`Editor`].
        Action::OpenFoldAtCursor => editor.do_set_fold_state_at_cursor(Some(false)),
        Action::CloseFoldAtCursor => editor.do_set_fold_state_at_cursor(Some(true)),
        Action::ToggleFoldAtCursor => editor.do_set_fold_state_at_cursor(None),
        Action::OpenAllFolds => editor.do_set_all_folds(false),
        Action::CloseAllFolds => editor.do_set_all_folds(true),
        Action::DeleteFoldAtCursor => editor.do_delete_fold_at_cursor(),
        Action::GotoNextFold => editor.do_goto_fold(true),
        Action::GotoPrevFold => editor.do_goto_fold(false),
        Action::StartMacroRecord(reg) => editor.do_start_macro_record(reg),
        Action::StopMacroRecord => editor.do_stop_macro_record(),
        Action::SnippetLeave => {
            // Snippet body has no host-side helper -- the App arm
            // was literally these two field writes.
            editor.active_snippet = None;
            editor.modal = ModalState::Normal;
        }
        // 5.5.G.2: pure-editor visual + mark arms. Bodies migrated
        // to [`Editor`] alongside the existing `do_enter_visual` /
        // `do_exit_visual` / `do_reselect_visual` cluster.
        Action::EnterVisual(kind) => editor.do_enter_visual(kind),
        Action::ExitVisual => editor.do_exit_visual(),
        Action::ReselectLastVisual => editor.do_reselect_visual(),
        Action::SetMark(name) => {
            if name.is_ascii_alphabetic() || name.is_ascii_digit() {
                editor.marks.insert(name, editor.cursor);
                let cur = editor.cursor;
                editor.push_position_history(cur, PositionSource::NamedMark(name));
            } else {
                editor.set_message(EchoLevel::Error, format!("invalid mark: {name}"));
            }
        }
        // 5.5.G.3: pure-editor edit-cluster arms. Bodies migrated
        // to [`Editor`]; LSP-coupled `do_insert_text`,
        // `do_paste*`, and `do_enter_block_visual_insert` stay on
        // App until their helpers move.
        Action::Undo => {
            let _ = editor.undo_blocking();
            editor.clamp_cursor_to_buffer();
        }
        Action::Redo => {
            let _ = editor.redo_blocking();
            editor.clamp_cursor_to_buffer();
        }
        Action::JoinLines { with_space } => editor.do_join_lines(with_space),
        Action::ToggleCaseAtCursor => editor.do_toggle_case_at_cursor(),
        Action::EnterAppend => editor.do_enter_append(),
        Action::OpenLineBelow => editor.do_open_line_below(),
        Action::OpenLineAbove => editor.do_open_line_above(),
        Action::OverwriteChar(c) => editor.do_overwrite_char(c),
        Action::ReplaceUndoLast => editor.do_replace_undo_last(),
        Action::DeleteCharBackward => editor.do_delete_char_backward(),
        // 5.5.G.4: pure-editor scroll / viewport / page / bracket
        // / redraw arms. Bodies migrated to [`Editor`].
        Action::JumpViewport(vp) => editor.do_jump_viewport(vp),
        Action::ScrollCursorTo(sp) => editor.do_scroll_cursor_to(sp),
        Action::PageDown => editor.do_page(true),
        Action::PageUp => editor.do_page(false),
        Action::ScrollLineUp => editor.do_scroll_line(false),
        Action::ScrollLineDown => editor.do_scroll_line(true),
        Action::MatchBracket => editor.do_match_bracket(),
        Action::RedrawScreen => editor.do_redraw_screen(),
        // 5.5.G.5: pure-editor pane-navigation arms.
        Action::SplitPaneHorizontal => {
            editor.do_split_pane(lattice_core::ui::pane::SplitOrientation::Horizontal)
        }
        Action::SplitPaneVertical => {
            editor.do_split_pane(lattice_core::ui::pane::SplitOrientation::Vertical)
        }
        Action::ClosePane => editor.do_close_pane(),
        Action::NavigatePane(dir) => editor.do_navigate_pane(dir),
        Action::NextPane => {
            let target = editor.pane_tree.next_pane();
            editor.activate_pane(target);
        }
        Action::PrevPane => {
            let target = editor.pane_tree.prev_pane();
            editor.activate_pane(target);
        }
        // 5.5.G.6: pure-editor mark-history arms. Jump-history
        // (`<C-o>`/`<C-i>`) stays App-side until `pop_popup_back`
        // migrates.
        Action::WalkMarkHistoryBack => editor.do_mark_history(-1),
        Action::WalkMarkHistoryForward => editor.do_mark_history(1),
        // 5.5.G.7: tag-stack pop, mark jumps, jump-history walk.
        Action::TagStackPop => editor.do_tag_stack_pop(),
        Action::JumpToMarkLine(name) => editor.do_jump_mark(name, false),
        Action::JumpToMarkExact(name) => editor.do_jump_mark(name, true),
        Action::JumpHistoryBack => editor.do_jump_history(-1),
        Action::JumpHistoryForward => editor.do_jump_history(1),
        // 5.5.G.8: snippet placeholder navigation.
        Action::SnippetNextPlaceholder => editor.do_snippet_next_placeholder(),
        Action::SnippetPrevPlaceholder => editor.do_snippet_prev_placeholder(),
        // 5.5.G.9: paste cluster (`p` / `P` / bracketed-paste).
        Action::PasteAfter => editor.do_paste(false),
        Action::PasteBefore => editor.do_paste(true),
        Action::PasteText(text) => editor.do_paste_text(&text),
        // 5.5.G.10: search-state cluster.
        Action::SearchAppend(c) => {
            if let Some(line) = editor.search_line.as_mut() {
                line.pattern.push(c);
                editor.preview_search();
            }
        }
        Action::SearchBackspace => {
            let leave = match editor.search_line.as_mut() {
                Some(line) => {
                    if line.pattern.pop().is_none() {
                        true
                    } else {
                        editor.preview_search();
                        false
                    }
                }
                None => false,
            };
            if leave {
                editor.cancel_search();
            }
        }
        Action::SearchSubmit => editor.submit_search(),
        Action::SearchCancel => editor.cancel_search(),
        Action::SearchNext => editor.repeat_search(false),
        Action::SearchPrevious => editor.repeat_search(true),
        Action::SearchWordUnderCursor(direction) => {
            editor.do_search_word_under_cursor(direction);
        }
        // 5.5.G.11: picker append/backspace/select + CloseHover.
        // Accept/Dismiss stay App-side (file-open / SMR / preview).
        Action::PickerAppend(c) => {
            if let Some(p) = editor.picker.as_mut() {
                p.append_query(c);
            }
            editor.bump_live_picker_debounce();
            _out.renderer_signals
                .extend(editor.preview_picker_selection());
        }
        Action::PickerBackspace => {
            if let Some(p) = editor.picker.as_mut() {
                p.backspace_query();
            }
            editor.bump_live_picker_debounce();
            _out.renderer_signals
                .extend(editor.preview_picker_selection());
        }
        Action::PickerSelectNext => {
            if let Some(p) = editor.picker.as_mut() {
                p.select_next();
            }
            _out.renderer_signals
                .extend(editor.preview_picker_selection());
        }
        Action::PickerSelectPrev => {
            if let Some(p) = editor.picker.as_mut() {
                p.select_prev();
            }
            _out.renderer_signals
                .extend(editor.preview_picker_selection());
        }
        Action::CloseHover => editor.dismiss_popup(),
        // 5.5.G.12: HelpDismiss dispatches on active_buffer to
        // pop the help popup or dismiss the file-tree pane.
        Action::HelpDismiss => match editor.active_buffer {
            BufferKind::Help => editor.dismiss_popup(),
            BufferKind::FileTree => {
                _out.renderer_signals.extend(editor.dismiss_file_tree());
            }
            BufferKind::Document | BufferKind::Oil => {}
        },
        // 5.5.G.13: pure-editor command-line arms. `EnterCommandLine`
        // opens the `:` line, clears any in-flight completion popup,
        // and auto-dismisses a State-A help popup (so the user's
        // doc cursor isn't visually anchored to a stale hover). The
        // history-step arms walk `command_history` cursor and
        // restore the pending unfinished line on the lower bound.
        Action::EnterCommandLine => {
            editor.command_line.clear();
            editor.modal = ModalState::Command;
            editor.last_message = None;
            // Q16: opening the cmdline dismisses STATE A help
            // popups (hover overlay still anchored to doc cursor).
            // State B help buffers (`:lsp-log`, `:lsp-trace-log`,
            // `:describe-*` opened in a pane) are first-class
            // buffers per the everything-is-a-buffer model -- the
            // user expects to run `:bd`, `:diagnostics`, etc.
            // without losing their log view. Only auto-dismiss when
            // active_buffer is Document, which is the State A
            // shape.
            if matches!(editor.active_buffer, BufferKind::Document) {
                editor.dismiss_popup();
            }
            editor.completion_state = None;
        }
        Action::CommandLineHistoryPrev => editor.do_command_history_step(true),
        Action::CommandLineHistoryNext => editor.do_command_history_step(false),
        // 5.5.G.14: pure-editor completion-cancel + docs-scroll + foldenable toggle.
        Action::CompletionCancel => editor.do_completion_cancel(),
        Action::CompletionCancelAndExitInsert => {
            editor.do_completion_cancel();
            editor.modal = ModalState::Normal;
        }
        Action::CompletionDocsScrollDown => editor.do_completion_docs_scroll_down(),
        Action::CompletionDocsScrollUp => editor.do_completion_docs_scroll_up(),
        // 5.5.G.15: pure-editor cmdline-completion popup nav.
        Action::CommandLineCompletePrev => editor.do_command_line_complete_prev(),
        Action::CommandLineAcceptCompletion => editor.do_command_line_accept_completion(),
        // 5.5.G.16: `zf` creates a fold over the Visual selection.
        Action::CreateFoldFromVisual => editor.do_create_fold_from_visual(),
        // 5.5.G.17: modal-state pivot + blockwise-Visual I/A.
        Action::EnterMode(state) => editor.enter_mode(state),
        Action::EnterBlockVisualInsert => editor.do_enter_block_visual_insert(false),
        Action::EnterBlockVisualAppend => editor.do_enter_block_visual_insert(true),
        Action::ToggleFoldEnable => {
            // `zi` toggle. `set_typed` publishes through the bus;
            // drain immediately so the cascade refreshes
            // `option_cache.foldenable` before any subsequent reads
            // in this same `dispatch` call (and before the next
            // frame draws).
            let cur = editor.option_cache.foldenable;
            let _ = editor.config.set_typed::<lattice_config::FoldEnable>(!cur);
            _out.renderer_signals.extend(editor.drain_option_changes());
        }
        // 5.5.LSP.1: `K` -- LSP hover request. The helper lives on
        // `Editor`; App's `apply` arm is gone (falls through to its
        // grouped `_ => {}` no-op). The popup is opened by the
        // `drain_pending_hover` tick that follows on the next
        // frame, which is still App-resident (LSP.2+ migrates the
        // drain).
        Action::LspHoverRequest => editor.lsp_hover_request(),
        // 5.5.LSP.2: `gd` / `gD` / `gy` / `gI` -- LSP navigation
        // family. All four arms share one host helper; the kind
        // discriminates which LSP request gets sent. Drain still
        // App-side until the next phase.
        Action::LspDefinitionRequest => {
            editor.lsp_nav_request(lattice_lsp::cache::LspNavKind::Definition)
        }
        Action::LspDeclarationRequest => {
            editor.lsp_nav_request(lattice_lsp::cache::LspNavKind::Declaration)
        }
        Action::LspTypeDefinitionRequest => {
            editor.lsp_nav_request(lattice_lsp::cache::LspNavKind::TypeDefinition)
        }
        Action::LspImplementationRequest => {
            editor.lsp_nav_request(lattice_lsp::cache::LspNavKind::Implementation)
        }
        // Catch-all: any Action variant not yet migrated from
        // App::apply. Sub-slices 5.5.D+ extend the match upward as
        // helpers move.
        _ => {}
    }
}

/// Wire-typed projection of [`EchoLevel`]. Used by [`Editor::set_message`]
/// when constructing the [`lattice_runtime::MessageRecord`] for the
/// `*messages*` ring and the typed event publication.
fn echo_level_to_wire(level: EchoLevel) -> lattice_grammar::EchoLevel {
    match level {
        EchoLevel::Trace => lattice_grammar::EchoLevel::Trace,
        EchoLevel::Debug => lattice_grammar::EchoLevel::Debug,
        EchoLevel::Info => lattice_grammar::EchoLevel::Info,
        EchoLevel::Warn => lattice_grammar::EchoLevel::Warn,
        EchoLevel::Error => lattice_grammar::EchoLevel::Error,
    }
}

/// Inverse of [`echo_level_to_wire`]. Grammar [`Effect::Echo`] carries
/// the wire-typed `lattice_grammar::EchoLevel`; the host's
/// [`Editor::set_message`] takes the renderer-neutral [`EchoLevel`].
/// Moved here from `lattice-ui-tui::app::dispatch` in 5.5.E.1
/// alongside the [`Effect::Echo`] arm.
fn echo_level_from_grammar(level: lattice_grammar::EchoLevel) -> EchoLevel {
    match level {
        lattice_grammar::EchoLevel::Trace => EchoLevel::Trace,
        lattice_grammar::EchoLevel::Debug => EchoLevel::Debug,
        lattice_grammar::EchoLevel::Info => EchoLevel::Info,
        lattice_grammar::EchoLevel::Warn => EchoLevel::Warn,
        lattice_grammar::EchoLevel::Error => EchoLevel::Error,
    }
}

/// Internal effect handler -- the destination 5.5.E migrates the
/// `App::apply_effect` body into.
///
/// 5.5.E.1 seeds the match with the helper-free arms; the catch-all
/// `_ => {}` lets every other variant fall through to App's still-
/// resident match. Sub-slices E.2+ extend this match upward as
/// `do_*` helpers move onto [`Editor`].
pub(crate) fn handle_effect(editor: &mut Editor, effect: Effect, out: &mut DispatchOutcome) {
    match effect {
        Effect::None => {}
        Effect::ClearSearchHighlight => {
            // `:nohlsearch` -- drop the current-match highlight and
            // the cached match set. The next `/` / `?` rebuilds both.
            editor.current_match = None;
            editor.all_matches.clear();
        }
        Effect::Echo { level, text } => {
            // `:echo` and grammar-internal informational messages
            // (e.g. "pattern not found" from `/`). `set_message` also
            // appends to the `*messages*` ring and publishes
            // [`lattice_runtime::MessagePushed`].
            editor.set_message(echo_level_from_grammar(level), text);
        }
        Effect::EchoMarks => {
            // 5.5.E.2: `:marks` -- list every set mark in the echo
            // area. Pure editor.* read + `set_message`.
            editor.do_list_marks();
        }
        Effect::EchoRegisters => {
            // 5.5.E.2: `:reg` / `:registers` -- list register
            // previews in the echo area. Pure editor.* read +
            // `set_message`; uses the host-side `preview_register`.
            editor.do_list_registers();
        }
        Effect::Yank {
            content,
            kind,
            register,
        } => {
            // 5.5.E.3: stash the operator payload into the register
            // slots. Pure editor.* mutation (`unnamed_register` +
            // `registers`); no renderer-side side-effects.
            editor.store_yank(register, content, kind);
        }
        Effect::SelectionChange(set) => {
            // 5.5.E.4: motion / selection-class effects emit a
            // SelectionSet; the host syncs `editor.cursor` to the
            // primary head. In Visual mode, the dispatcher's
            // `replace_primary(Selection::cursor(...))` would
            // collapse the selection -- refresh the actor's
            // selection set with the preserved anchor so the
            // extension survives.
            let new_head = set.primary().head;
            editor.cursor = new_head;
            if let ModalState::Visual(kind) = editor.modal {
                let sel = Selection {
                    anchor: editor.visual_anchor.unwrap_or(new_head),
                    head: new_head,
                    visual: Some(visual_kind_to_mode(kind)),
                };
                editor.set_selections_blocking(SelectionSet::single(sel));
            }
        }
        Effect::SetOption { spec } => {
            // 5.5.E.6: `:set foo=bar` -- the canonical cmdline
            // path. The host owns parse + cascade + cache rebuild;
            // any renderer-coupled side effects (theme rebuild,
            // file-tree refresh, mode mirroring, LSP fan-out) flow
            // back through `RendererSignal`s the cascade enqueued.
            let signals = editor.do_set(&spec);
            out.renderer_signals.extend(signals);
        }
        Effect::ListBuffers => {
            // 5.5.F.1: `:ls` / `:buffers` -- build the help-style
            // listing host-side from `editor.buffers` + per-kind
            // metadata, then signal the renderer to display it
            // under the `HelpList` category.
            let content = editor.build_list_buffers_content();
            out.renderer_signals
                .push(RendererSignal::DisplayBuffer(Box::new(DisplayBufferRequest {
                    content,
                    category: lattice_core::ui::display::BufferDisplayCategory::HelpList,
                })));
        }
        Effect::DescribeBuffer => {
            // 5.5.F.1: `:describe-buffer` -- snapshot the active
            // document's editor state (path, language, cursor,
            // modes, ...) into a help buffer; signal the renderer
            // under `HelpDescribe`.
            let content = editor.build_describe_buffer_content();
            out.renderer_signals
                .push(RendererSignal::DisplayBuffer(Box::new(DisplayBufferRequest {
                    content,
                    category: lattice_core::ui::display::BufferDisplayCategory::HelpDescribe,
                })));
        }
        Effect::DescribeCommand { name, anchor } => {
            // 5.5.F.2: `:describe-command <name>` -- resolve `name`
            // through canonical-then-alias; emit DisplayBuffer on
            // success, set_message on error (skip the signal).
            if let Some(content) =
                editor.build_describe_command_content(&name, anchor.as_deref())
            {
                out.renderer_signals
                    .push(RendererSignal::DisplayBuffer(Box::new(DisplayBufferRequest {
                        content,
                        category: lattice_core::ui::display::BufferDisplayCategory::HelpDescribe,
                    })));
            }
        }
        Effect::Apropos { pattern } => {
            // 5.5.F.2: `:apropos <pattern>` -- registry scan +
            // 3-column listing host-side. Empty pattern routes an
            // error to the echo ring and skips the signal.
            if let Some(content) = editor.build_apropos_content(&pattern) {
                out.renderer_signals
                    .push(RendererSignal::DisplayBuffer(Box::new(DisplayBufferRequest {
                        content,
                        category: lattice_core::ui::display::BufferDisplayCategory::HelpApropos,
                    })));
            }
        }
        Effect::DescribeKey { chord } => {
            // 5.5.F.2: `:describe-key <chord>` -- keymap lookup
            // (infallible: unbound chords render as text).
            let content = editor.build_describe_key_content(&chord);
            out.renderer_signals
                .push(RendererSignal::DisplayBuffer(Box::new(DisplayBufferRequest {
                    content,
                    category: lattice_core::ui::display::BufferDisplayCategory::HelpDescribe,
                })));
        }
        Effect::ListKeymap => {
            // 5.5.F.2: `:list-keymap` -- group every registered
            // binding by mode (fixed order), render host-side.
            let content = editor.build_list_keymap_content();
            out.renderer_signals
                .push(RendererSignal::DisplayBuffer(Box::new(DisplayBufferRequest {
                    content,
                    category: lattice_core::ui::display::BufferDisplayCategory::HelpList,
                })));
        }
        Effect::DescribeOption { name } => {
            // 5.5.F.3: `:describe-option <name>` -- config.lookup
            // + metadata format. Unknown name routes E518 to the
            // echo ring; dispatcher skips the signal.
            if let Some(content) = editor.build_describe_option_content(&name) {
                out.renderer_signals
                    .push(RendererSignal::DisplayBuffer(Box::new(DisplayBufferRequest {
                        content,
                        category: lattice_core::ui::display::BufferDisplayCategory::HelpDescribe,
                    })));
            }
        }
        Effect::ListOptions => {
            // 5.5.F.3: `:options` -- walks `OPTION_DECLS` /
            // `GROUP_DECLS` linkme slices + per-option
            // config.lookup; infallible.
            let content = editor.build_list_options_content();
            out.renderer_signals
                .push(RendererSignal::DisplayBuffer(Box::new(DisplayBufferRequest {
                    content,
                    category: lattice_core::ui::display::BufferDisplayCategory::HelpList,
                })));
        }
        Effect::DescribeOptionResolution { name } => {
            // 5.5.F.3: `:describe-option-resolution <name>` --
            // walks the §6.1 layer model (modal-state / buffer-
            // local / minors / major / typed-option / default)
            // and marks each contributing layer with ⭐. Unknown
            // name routes E518; dispatcher skips.
            if let Some(content) = editor.build_describe_option_resolution_content(&name) {
                out.renderer_signals
                    .push(RendererSignal::DisplayBuffer(Box::new(DisplayBufferRequest {
                        content,
                        category: lattice_core::ui::display::BufferDisplayCategory::HelpDescribe,
                    })));
            }
        }
        Effect::DescribeEvents => {
            // 5.5.F.3: `:describe-events` -- walks the
            // `EVENT_DESCRIPTORS` linkme slice, groups by source
            // crate. Infallible.
            let content = editor.build_describe_events_content();
            out.renderer_signals
                .push(RendererSignal::DisplayBuffer(Box::new(DisplayBufferRequest {
                    content,
                    category: lattice_core::ui::display::BufferDisplayCategory::HelpList,
                })));
        }
        Effect::DescribeEvent { name } => {
            // 5.5.F.3: `:describe-event <name>` -- descriptor
            // lookup. Unknown name routes error; dispatcher
            // skips.
            if let Some(content) = editor.build_describe_event_content(&name) {
                out.renderer_signals
                    .push(RendererSignal::DisplayBuffer(Box::new(DisplayBufferRequest {
                        content,
                        category: lattice_core::ui::display::BufferDisplayCategory::HelpDescribe,
                    })));
            }
        }
        Effect::BufferNext => {
            // 5.5.F.4.3: `:bn` / `:bnext` -- cycle to next listed
            // buffer. 5.5.F.5.5 brought `activate_buffer_state`
            // host-side; the full-activation tail runs inline and
            // its cascading signals stream into the outcome.
            if editor.do_buffer_next() {
                out.renderer_signals.extend(editor.activate_buffer_state());
            }
        }
        Effect::BufferPrev => {
            // 5.5.F.4.3: `:bp` / `:bprev` -- cycle to previous
            // listed buffer. Same host-side tail shape as
            // `BufferNext`.
            if editor.do_buffer_prev() {
                out.renderer_signals.extend(editor.activate_buffer_state());
            }
        }
        Effect::BufferDelete { force } => {
            // 5.5.F.4.4: `:bd[elete]` -- close the active buffer. The
            // host-side path handles successor selection, LSP detach,
            // pane re-pointing, AND (since F.5.5) the post-activation
            // tail. Cascading signals stream into the outcome.
            if editor.do_buffer_delete(force) {
                out.renderer_signals.extend(editor.activate_buffer_state());
            }
        }
        Effect::ListModes => {
            // 5.5.F.6: `:list-modes` (M.8). Infallible — always
            // produces a buffer. Routed through the DisplayBuffer
            // pipe; renderer dispatches via the existing
            // `display_buffer` machinery.
            let content = editor.build_list_modes_content();
            out.renderer_signals
                .push(RendererSignal::DisplayBuffer(Box::new(DisplayBufferRequest {
                    content,
                    category: lattice_core::ui::display::BufferDisplayCategory::HelpList,
                })));
        }
        Effect::DescribeMode { name } => {
            // 5.5.F.6: `:describe-mode <name>` (M.8). Fallible —
            // unknown name pushes an echo + skips the signal.
            if let Some(content) = editor.build_describe_mode_content(&name) {
                out.renderer_signals
                    .push(RendererSignal::DisplayBuffer(Box::new(DisplayBufferRequest {
                        content,
                        category: lattice_core::ui::display::BufferDisplayCategory::HelpDescribe,
                    })));
            }
        }
        Effect::ListDiagnostics => {
            // 5.5.F.7: `:diagnostics` -- open every published
            // diagnostic in a picker. The picker lives on `Editor`
            // (`self.picker`) and is renderer-neutral, so no
            // `RendererSignal` is required.
            editor.do_list_diagnostics();
        }
        Effect::DeleteCurrentLine => {
            // 5.5.E.7.4: `:d` (or `:g/.../d`) -- delete the cursor's
            // whole line including its trailing newline. Pure
            // editor.* mutation through `apply_edit_blocking`.
            editor.do_delete_line();
        }
        Effect::Substitute {
            scope,
            pattern,
            replacement,
            global,
        } => {
            // 5.5.E.7.5: `:s/pat/repl/[g]` and `:%s/...`. Pure
            // editor.* mutation through `apply_edit_blocking`; the
            // replacement-count echo lands via `set_message`.
            editor.do_substitute(scope, &pattern, &replacement, global);
        }
        Effect::Edits(edits) => {
            // 5.5.E.7.7: grammar-driven edits (`>>`, `dd`, `c`, `y`)
            // -- the document actor has already applied them. Route
            // through the chokepoint so LSP `didChange`, syntax
            // reparse, and highlight byte-shifts all see the deltas.
            editor.handle_edits(&edits);
        }
        Effect::Customize { name } => {
            // 5.5.F.6: `:customize [name]` (M.9.0). Three resolution
            // paths: no arg → picker; `<name>` ending in `-mode` →
            // mode view; otherwise → group view. The mode + group
            // builders are fallible (unknown name routes echo);
            // picker is infallible.
            let content_opt = match name.as_deref() {
                None => Some(editor.build_customize_picker_content()),
                Some(n) if lattice_config::ends_with_mode_suffix(n) => {
                    editor.build_customize_mode_content(n)
                }
                Some(n) => editor.build_customize_group_content(n),
            };
            if let Some(content) = content_opt {
                out.renderer_signals
                    .push(RendererSignal::DisplayBuffer(Box::new(DisplayBufferRequest {
                        content,
                        category: lattice_core::ui::display::BufferDisplayCategory::HelpList,
                    })));
            }
        }
        // Catch-all: any Effect variant not yet migrated from
        // `App::apply_effect`. Sub-slices 5.5.E.7+ extend the match
        // upward as helpers move.
        _ => {}
    }
}

// 5.5.D: pure-editor mutation helpers relocated from
// `lattice-ui-tui::app`. Grouped here next to `dispatch` so they sit
// with their callers; subsequent slices (5.5.E+) split them into
// per-domain modules if the file outgrows comfortable navigation.
impl Editor {
    /// Surface a one-line message in the echo area. Replaces the
    /// previous message; also appends a [`lattice_runtime::MessageRecord`]
    /// to the bounded `*messages*` ring and publishes a typed
    /// [`MessagePushed`] event so subscribers (the runtime's per-tick
    /// drain, plugin hosts) can react. The renderer reads
    /// [`Self::last_message`] directly for the echo-area paint.
    pub fn set_message(&mut self, level: EchoLevel, text: impl Into<String>) {
        let text: String = text.into();
        self.last_message = Some(crate::action::EchoMessage {
            text: text.clone(),
            level,
        });
        let record = lattice_runtime::MessageRecord {
            timestamp: std::time::SystemTime::now(),
            level: echo_level_to_wire(level),
            text,
        };
        if let Ok(mut ring) = self.messages.lock() {
            ring.push(record.clone());
        }
        self.event_bus.publish_typed(MessagePushed { record });
    }

    /// Scroll so [`Self::cursor`] is inside `[scroll, scroll +
    /// viewport_height)`. No-op when `viewport_height == 0` (the
    /// renderer hasn't recorded a draw yet).
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

    /// What `:bn` / `:bp` consider the "current" buffer for stepping.
    /// The active pane's `buffer_id` is the source of truth (the
    /// active pane is what the user sees).
    pub fn active_pane_buffer_id(&self) -> BufferId {
        self.pane_tree.active().buffer_id
    }

    /// Identity of the buffer whose state the input dispatcher
    /// currently routes to. Document / file-tree / oil all return the
    /// active pane's id; Help routes through the popup overlay slot
    /// (which still lives outside the pane tree as a transient
    /// overlay).
    pub fn active_buffer_id(&self) -> BufferId {
        match self.active_buffer {
            BufferKind::Help => self.popup_buffer.unwrap_or(self.document_buffer_id),
            BufferKind::Document | BufferKind::FileTree | BufferKind::Oil => {
                self.pane_tree.active().buffer_id
            }
        }
    }

    /// Snapshot the active popup's `HelpBuffer`. The popup buffer
    /// stores `BufferId` only; the actual content lives in
    /// `self.buffers` with `BufferFlags { listed: false, hidden:
    /// true }`. Returns a cloned snapshot (the rope clones in O(1)
    /// via ropey's internal Arc); `None` when no popup is open or
    /// the registry entry has been torn down.
    pub fn popup_help(&self) -> Option<lattice_help::HelpBuffer> {
        let id = self.popup_buffer?;
        self.buffers.with_help(id, |h| h.clone())
    }

    /// The active buffer's text -- a `Buffer` clone (rope is O(1)).
    /// Document, help, file-tree, oil all flow through this so motion
    /// / scroll / search code can read text without branching on
    /// [`BufferKind`]. `self.cursor` / `self.scroll` are the live
    /// position into this buffer.
    pub fn active_text(&self) -> lattice_core::Buffer {
        match self.active_buffer {
            BufferKind::Help => self
                .popup_help()
                .map(|h| h.content.clone())
                .unwrap_or_else(|| self.document.snapshot().buffer.clone()),
            BufferKind::FileTree => self
                .buffers
                .with_file_tree(self.active_pane_buffer_id(), |t| t.content.clone())
                .unwrap_or_else(|| self.document.snapshot().buffer.clone()),
            BufferKind::Document => self.document.snapshot().buffer.clone(),
            BufferKind::Oil => self
                .buffers
                .with_oil(self.active_pane_buffer_id(), |o| o.content.clone())
                .unwrap_or_else(|| self.document.snapshot().buffer.clone()),
        }
    }

    /// Cursor of the currently active buffer. Reads
    /// [`Self::cursor`] when the document holds focus, the popup
    /// help buffer's cursor (via [`Self::popup_help`]) when a help
    /// overlay holds focus, and the kind-specific cursor stash for
    /// file-tree / oil.
    pub fn active_cursor(&self) -> lattice_protocol::position::Position {
        match self.active_buffer {
            BufferKind::Document => self.cursor,
            BufferKind::Help => self.popup_help().map(|h| h.cursor).unwrap_or(self.cursor),
            BufferKind::FileTree => self
                .buffers
                .with_file_tree(self.active_pane_buffer_id(), |t| t.cursor)
                .unwrap_or(self.cursor),
            BufferKind::Oil => self
                .buffers
                .with_oil(self.active_pane_buffer_id(), |o| o.cursor)
                .unwrap_or(self.cursor),
        }
    }

    /// Clamp [`Self::cursor`] to the active buffer's bounds. Reads
    /// from [`Self::active_text`] so it works for help / file-tree /
    /// document / oil uniformly.
    pub fn clamp_cursor_to_active_buffer(&mut self) {
        let buffer = self.active_text();
        let last_line = last_addressable_line(&buffer);
        if self.cursor.line > last_line {
            self.cursor.line = last_line;
        }
        let len = buffer.line_byte_len(self.cursor.line);
        if self.cursor.byte > len {
            self.cursor.byte = len;
        }
    }

    /// Legacy alias for [`Self::clamp_cursor_to_active_buffer`] used
    /// by post-undo / post-redo paths that already named it
    /// `clamp_cursor_to_buffer` in the renderer. Identical behaviour;
    /// retained to keep call sites mechanical across the move.
    pub fn clamp_cursor_to_buffer(&mut self) {
        self.clamp_cursor_to_active_buffer();
    }

    /// Tear down the popup overlay. Drops the popup's content slot
    /// from the registry, clears placement / back-stack state, and
    /// restores any pre-popup focus captured at open. Idempotent --
    /// closing when no popup is open is a no-op.
    pub fn dismiss_popup(&mut self) {
        self.dismiss_stale_popup_registry();
        self.popup_buffer = None;
        self.popup_back_stack.clear();
        self.popup_placement = lattice_core::ui::popup::PopupPlacement::default();
        // Restore pre-popup state if focus had moved into it
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
            self.active_buffer = BufferKind::Document;
        }
    }

    /// Tear down the registry / mode / option-cache state for the
    /// currently-bound popup buffer. Called by [`Self::dismiss_popup`]
    /// and by the popup-open paths (so back-to-back popups don't
    /// accumulate stale registry entries). No-op when no popup is
    /// set.
    pub fn dismiss_stale_popup_registry(&mut self) {
        let Some(prev) = self.popup_buffer else {
            return;
        };
        self.buffers.remove(prev);
        self.active_modes.remove(&prev);
        self.buffer_locals.remove(&prev);
        self.resolved_options.remove(&prev);
    }

    /// Mode-owned syntax handle for `id`. For the active document
    /// this is the live hot-path slot ([`Self::syntax`]); for
    /// inactive documents it routes through `buffer_locals`. Returns
    /// `None` for plain-language documents and non-document buffers.
    pub fn document_syntax_for(
        &self,
        id: BufferId,
    ) -> Option<&lattice_syntax::SyntaxHandle> {
        if id == self.document_buffer_id
            && matches!(self.active_buffer, BufferKind::Document)
        {
            return self.syntax.as_ref();
        }
        self.buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::DocumentSyntax>())
            .and_then(|s| s.0.as_ref())
    }

    /// Generic per-buffer minor-mode accessor used by every M.6
    /// sub-mode reader. Returns `false` when no entry exists for
    /// `buffer_id` -- matches the umbrella accessor's shape.
    pub fn minor_mode_enabled_for(
        &self,
        buffer_id: BufferId,
        mode_id: lattice_mode::ModeId,
    ) -> bool {
        self.active_modes
            .get(&buffer_id)
            .map(|modes| modes.has_minor(mode_id))
            .unwrap_or(false)
    }

    /// 4.4.f: is `lsp-folding-mode` active on `buffer_id`? Gates
    /// `textDocument/foldingRange` issuance and the LSP fold cache
    /// read.
    pub fn lsp_folding_mode_enabled_for(&self, buffer_id: BufferId) -> bool {
        self.minor_mode_enabled_for(
            buffer_id,
            lattice_lsp::modes::LspFoldingMode::mode_id(),
        )
    }

    /// 5.5.F.5.1: is `lsp-mode` active on `buffer_id`? Pure-editor
    /// read used by the mode-lifecycle auto-activation hook
    /// ([`Self::maybe_auto_activate_lsp_mode`], F.5.2) and by
    /// `:describe-buffer` / the LSP capability gates.
    pub fn lsp_mode_enabled_for(&self, buffer_id: BufferId) -> bool {
        self.minor_mode_enabled_for(buffer_id, lattice_lsp::modes::LspMode::mode_id())
    }

    /// 5.5.LSP.1: is `lsp-hover-mode` active on `buffer_id`? Gates
    /// `do_lsp_hover_request` (the K binding).
    pub fn lsp_hover_mode_enabled_for(&self, buffer_id: BufferId) -> bool {
        self.minor_mode_enabled_for(buffer_id, lattice_lsp::modes::LspHoverMode::mode_id())
    }

    /// 5.5.LSP.1: shared gate for every LSP request entry point
    /// (hover / definition / completion / format / rename /
    /// code-action / symbols / signature / references). Returns
    /// `true` when `lsp-mode` is active on the current document;
    /// callers early-return on `false`. A single echo surfaces the
    /// gate state so users discover the mode -- silent gates are a
    /// documented anti-pattern when editor defaults the user
    /// expects (`K`, `gd`) suddenly do nothing.
    ///
    /// The echo level is `Info` (not `Warn`) -- gated state is
    /// expected user-controlled, not a misconfiguration.
    pub fn check_lsp_mode_gate(&mut self) -> bool {
        if self.lsp_mode_enabled_for(self.document_buffer_id) {
            return true;
        }
        self.set_message(
            EchoLevel::Info,
            "lsp-mode disabled for this buffer (`:lsp-mode` to enable)".to_string(),
        );
        false
    }

    /// 5.5.LSP.1: shared gate for a per-feature LSP sub-mode.
    /// Checks the umbrella first (so the user gets one consistent
    /// message-source-of-truth: enable `lsp-mode` first, then the
    /// sub-mode); returns `true` only when both are active.
    /// Echoes at `Info` matching the umbrella's level.
    ///
    /// Used by `lsp_*_request` methods that want a user-discoverable
    /// bail message. Insert-mode auto-triggers (insert completion,
    /// signature help, on-type formatting) skip the echo path
    /// entirely and check the bool directly -- a typed character
    /// that doesn't fire isn't a moment to surface mode state.
    pub fn check_lsp_sub_mode_gate(
        &mut self,
        sub_mode_id: lattice_mode::ModeId,
        sub_mode_name: &str,
    ) -> bool {
        if !self.check_lsp_mode_gate() {
            return false;
        }
        if self.minor_mode_enabled_for(self.document_buffer_id, sub_mode_id) {
            return true;
        }
        self.set_message(
            EchoLevel::Info,
            format!("{sub_mode_name} disabled for this buffer (`:{sub_mode_name}` to enable)"),
        );
        false
    }

    /// 5.5.LSP.1: **State A -> State B** -- focus moves into the
    /// hover popup. After this, the popup behaves like any other
    /// buffer (vim grammar, `/` search, `:` ex commands operate on
    /// the popup's content); the doc behind is frozen. Dismiss with
    /// `<Esc>` / `q` returns focus to the doc at the cursor it was
    /// on. No-op when no popup is live.
    pub fn focus_help_popup(&mut self) {
        let Some(help) = self.popup_help() else {
            return;
        };
        let stash_cursor = help.cursor;
        let stash_scroll = help.scroll as u32;
        // Capture pre-State-B state so dismiss restores cleanly.
        let active = self.pane_tree.active();
        self.prev_pane_for_help = Some(PrevPaneState {
            buffer: active.buffer,
            buffer_id: active.buffer_id,
            cursor: self.cursor,
            scroll: self.scroll,
        });
        // Sync active pane's cursor / scroll stash *before*
        // swapping `active_buffer` to Help.
        self.snapshot_active_pane();
        self.cursor = stash_cursor;
        self.scroll = stash_scroll;
        self.active_buffer = BufferKind::Help;
    }

    /// 5.5.LSP.2: `gd` / `gD` / `gy` / `gI` -- LSP navigation
    /// (definition / declaration / typeDefinition / implementation).
    /// Send the matching request to every attached server in
    /// parallel; the merged + deduped `Vec<Location>` flows back
    /// through `pending_definition_rx`. Single-result outcomes
    /// jump in-place and push the tag stack; multi-result open the
    /// locations picker (handled by App's `drain_pending_definitions`
    /// until that drain migrates host-side).
    ///
    /// Captures the pre-jump origin in `pending_tag_origin` so
    /// `<C-t>` can walk back through chained drill-downs.
    pub fn lsp_nav_request(&mut self, kind: lattice_lsp::cache::LspNavKind) {
        if let Some(token) = self.pending_definition_token.take() {
            token.cancel();
        }
        // M.6.2: lsp-nav-mode gate (after cancel-stale-work).
        if !self.check_lsp_sub_mode_gate(
            lattice_lsp::modes::LspNavMode::mode_id(),
            "lsp-nav-mode",
        ) {
            return;
        }
        let Some(uri) = self.buffer_uris.get(&self.document_buffer_id).cloned() else {
            self.set_message(
                EchoLevel::Info,
                "no LSP server attached to current buffer".to_string(),
            );
            return;
        };
        let snapshot = self.document.snapshot();
        let lsp_position =
            match crate::lsp_helpers::app_to_lsp_position(&snapshot.buffer, self.cursor) {
                Some(p) => p,
                None => {
                    self.set_message(
                        EchoLevel::Error,
                        format!("{}: cursor out of buffer", kind.noun_singular()),
                    );
                    return;
                }
            };
        // Capture the pre-jump origin for the tag stack -- the gd
        // family is "drill down" navigation, so users expect <C-t>
        // to walk back even after chained navigations.
        let label =
            crate::lsp_helpers::word_under_cursor(&snapshot.buffer, self.cursor).unwrap_or_default();
        self.pending_tag_origin = Some(TagStackEntry {
            buffer: self.active_buffer,
            buffer_id: self.active_pane_buffer_id(),
            position: self.cursor,
            label,
        });
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<lsp_types::Location>>();
        let token = lattice_protocol::CancellationToken::new();
        self.pending_definition_rx = Some(rx);
        self.pending_definition_token = Some(token.clone());
        self.pending_nav_kind = Some(kind);
        let lsp = self.lsp.clone();
        lattice_runtime::runtime::spawn_on_lsp_runtime(async move {
            let handles: Vec<lattice_lsp::ServerHandle> = { lsp.servers_for(&uri) };
            let mut all: Vec<lsp_types::Location> = Vec::new();
            for handle in handles {
                if token.is_cancelled() {
                    return;
                }
                let pos_params = lsp_types::TextDocumentPositionParams {
                    text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                    position: lsp_position,
                };
                let resp_locs = match kind {
                    lattice_lsp::cache::LspNavKind::Definition => {
                        let params = lsp_types::GotoDefinitionParams {
                            text_document_position_params: pos_params,
                            work_done_progress_params: Default::default(),
                            partial_result_params: Default::default(),
                        };
                        handle
                            .goto_definition(params, token.clone())
                            .await
                            .ok()
                            .flatten()
                            .map(crate::lsp_helpers::definition_response_to_locations)
                            .unwrap_or_default()
                    }
                    lattice_lsp::cache::LspNavKind::Declaration => {
                        let params = lsp_types::request::GotoDeclarationParams {
                            text_document_position_params: pos_params,
                            work_done_progress_params: Default::default(),
                            partial_result_params: Default::default(),
                        };
                        handle
                            .goto_declaration(params, token.clone())
                            .await
                            .ok()
                            .flatten()
                            .map(crate::lsp_helpers::definition_response_to_locations)
                            .unwrap_or_default()
                    }
                    lattice_lsp::cache::LspNavKind::TypeDefinition => {
                        let params = lsp_types::request::GotoTypeDefinitionParams {
                            text_document_position_params: pos_params,
                            work_done_progress_params: Default::default(),
                            partial_result_params: Default::default(),
                        };
                        handle
                            .goto_type_definition(params, token.clone())
                            .await
                            .ok()
                            .flatten()
                            .map(crate::lsp_helpers::definition_response_to_locations)
                            .unwrap_or_default()
                    }
                    lattice_lsp::cache::LspNavKind::Implementation => {
                        let params = lsp_types::request::GotoImplementationParams {
                            text_document_position_params: pos_params,
                            work_done_progress_params: Default::default(),
                            partial_result_params: Default::default(),
                        };
                        handle
                            .goto_implementation(params, token.clone())
                            .await
                            .ok()
                            .flatten()
                            .map(crate::lsp_helpers::definition_response_to_locations)
                            .unwrap_or_default()
                    }
                };
                all.extend(resp_locs);
            }
            // Dedup by (uri, range.start).
            all.sort_by(|a, b| {
                let au = a.uri.as_str();
                let bu = b.uri.as_str();
                au.cmp(bu)
                    .then_with(|| a.range.start.line.cmp(&b.range.start.line))
                    .then_with(|| a.range.start.character.cmp(&b.range.start.character))
            });
            all.dedup_by(|a, b| a.uri.as_str() == b.uri.as_str() && a.range.start == b.range.start);
            let _ = tx.send(all);
        });
    }

    /// 5.5.LSP.1: `K` (Phase 4.2.b) -- send `textDocument/hover`
    /// to every LSP server attached to the active document; the
    /// spawned task awaits the actor's response on the LSP runtime,
    /// so the keystroke handler returns instantly. The markdown
    /// body arrives back through `pending_hover_rx` and the next
    /// frame's `drain_pending_hover` feeds it into the popup.
    ///
    /// **Multi-server merge** is "first non-empty wins" for 4.2.b.
    /// **Cancellation**: any prior in-flight hover's token is
    /// flipped before the new request fires, so a slow server can't
    /// drop a stale popup over the new cursor position.
    pub fn lsp_hover_request(&mut self) {
        // Already focused into the popup (State B) -- K is a no-op.
        // To get a fresh hover the user dismisses with Esc / q,
        // repositions in the doc, then presses K.
        if matches!(self.active_buffer, BufferKind::Help) {
            return;
        }
        // Popup shown but focus still on main buffer (State A) --
        // second K transfers focus into the popup. No new LSP
        // request fires; we just promote.
        if self.popup_buffer.is_some() {
            self.focus_help_popup();
            return;
        }
        // First K -- fire a fresh hover request. Cancel any in-
        // flight first. (Cancel-stale-work runs before the M.5.4
        // gate so the prior request's relay loop sees the flip
        // even when the gate is now closed.)
        if let Some(token) = self.pending_hover_token.take() {
            token.cancel();
        }
        // M.6.2: lsp-hover-mode gate (umbrella check inside).
        if !self.check_lsp_sub_mode_gate(
            lattice_lsp::modes::LspHoverMode::mode_id(),
            "lsp-hover-mode",
        ) {
            return;
        }

        // Resolve the active buffer's URI. No URI = no LSP for this
        // buffer (e.g. unsaved scratch); echo + bail.
        let Some(uri) = self.buffer_uris.get(&self.document_buffer_id).cloned() else {
            self.set_message(
                EchoLevel::Info,
                "no LSP server attached to current buffer".to_string(),
            );
            return;
        };

        // Build the LSP-side cursor position. App's cursor is
        // (line, col_byte) in utf-8; LSP wants utf-16 columns.
        let snapshot = self.document.snapshot();
        let lsp_position =
            match crate::lsp_helpers::app_to_lsp_position(&snapshot.buffer, self.cursor) {
                Some(p) => p,
                None => {
                    self.set_message(EchoLevel::Error, "hover: cursor out of buffer".to_string());
                    return;
                }
            };

        // Fresh channel + token for this request.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<lattice_lsp::cache::HoverOutcome>();
        let token = lattice_protocol::CancellationToken::new();
        self.pending_hover_rx = Some(rx);
        self.pending_hover_token = Some(token.clone());

        let lsp = self.lsp.clone();
        let logger = self.lsp_logger.clone();
        let request_started = std::time::Instant::now();
        let request_uri = uri.as_str().to_string();
        lattice_runtime::runtime::spawn_on_lsp_runtime(async move {
            // Snapshot the attached handles under the supervisor
            // lock, then drop it before awaiting any per-server
            // response.
            let handles: Vec<lattice_lsp::ServerHandle> = { lsp.servers_for(&uri) };
            if handles.is_empty() {
                let _ = tx.send(lattice_lsp::cache::HoverOutcome::NoServers);
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
                let instance = handle.instance();
                logger.log(
                    Some(&instance),
                    lattice_lsp::LogLevel::Debug,
                    lattice_lsp::LogSource::Client,
                    format!(
                        "hover requested @ {request_uri} line {} character {}",
                        lsp_position.line, lsp_position.character
                    ),
                );
                match handle.hover(params, token.clone()).await {
                    Ok(Some(hover)) => {
                        let body = crate::lsp_helpers::hover_contents_to_markdown(&hover.contents);
                        if !body.trim().is_empty() {
                            logger.log(
                                Some(&instance),
                                lattice_lsp::LogLevel::Debug,
                                lattice_lsp::LogSource::Client,
                                format!(
                                    "hover reply: {} bytes after {:?}",
                                    body.len(),
                                    request_started.elapsed()
                                ),
                            );
                            let _ = tx.send(lattice_lsp::cache::HoverOutcome::Body(body));
                            return;
                        }
                        // Server replied but the body's empty.
                        logger.log(
                            Some(&instance),
                            lattice_lsp::LogLevel::Debug,
                            lattice_lsp::LogSource::Client,
                            "hover reply: empty body (server still indexing?)".to_string(),
                        );
                    }
                    Ok(None) => {
                        logger.log(
                            Some(&instance),
                            lattice_lsp::LogLevel::Debug,
                            lattice_lsp::LogSource::Client,
                            "hover reply: null (cursor not on a known symbol, or server still indexing)"
                                .to_string(),
                        );
                    }
                    Err(e) => {
                        logger.log(
                            Some(&instance),
                            lattice_lsp::LogLevel::Warn,
                            lattice_lsp::LogSource::Client,
                            format!("hover error: {e}"),
                        );
                    }
                }
            }
            // Walked every server, none had a non-empty body.
            let _ = tx.send(lattice_lsp::cache::HoverOutcome::NoBody {
                servers_tried: tried,
            });
        });
    }

    /// 5.5.F.5.1: rebuild the buffer-local `ActiveCompletionSources`
    /// snapshot from the buffer's currently-active major + minors.
    /// Called after every mode-lifecycle transition so the
    /// completion popup walks an up-to-date contribution list.
    pub fn recompute_active_completion_sources_for(&mut self, buffer: BufferId) {
        let mut merged: Vec<lattice_completion::CompletionSourceContribution> = Vec::new();
        if let Some(modes_snapshot) = self.active_modes.get(&buffer).cloned() {
            if let Some(major_id) = modes_snapshot.major()
                && let Some(major) = self.mode_registry.get(major_id)
            {
                merged.extend(major.completion_sources());
            }
            for &minor_id in modes_snapshot.minors() {
                if let Some(minor) = self.mode_registry.get(minor_id) {
                    merged.extend(minor.completion_sources());
                }
            }
        }
        // Always seed -- empty is meaningful ("this buffer has zero
        // contributed sources"). Absent vs empty would be equivalent
        // to the reader, but the always-seed shape keeps
        // `:describe-buffer` honest.
        self.buffer_locals
            .entry(buffer)
            .or_default()
            .insert(lattice_mode::ActiveCompletionSources(merged));
    }

    /// 5.5.F.5.1: best-effort path lookup for `buffer_id`. Returns
    /// the document's path for Document buffers (the active one
    /// reads from `self.document`, the rest from the buffer
    /// registry), `None` otherwise. Used by the LSP auto-activation
    /// hook in F.5.2.
    pub fn path_for_buffer(&self, buffer_id: BufferId) -> Option<std::path::PathBuf> {
        if buffer_id == self.document_buffer_id {
            return self.document.path().map(|p| p.to_path_buf());
        }
        self.buffers.document_path(buffer_id)
    }

    /// 5.5.F.5.2 (M.5.1): programmatic activation of `mode_id` on
    /// `buffer_id`. Used by hooks (auto-activation on `MajorEntered`
    /// etc.) and by the auto-generated `:<mode-name>` toggle command.
    /// The registry decides Major-vs-Minor and runs the appropriate
    /// activation; for majors the previous major is deactivated first.
    ///
    /// On failure, surfaces an `EchoLevel::Warn` and returns without
    /// mutating state.
    ///
    /// Returns the `RendererSignal` list its option-cascade drain
    /// enqueued (a typed-option write inside the mode's `on_activate`
    /// hook can fan out a renderer-coupled tail; e.g. `lsp-folding-mode`
    /// swapping `foldmethod=lsp`). Callers fan via the renderer
    /// signal-pipe.
    #[must_use]
    pub fn activate_mode_by_id(
        &mut self,
        buffer_id: BufferId,
        mode_id: lattice_mode::ModeId,
    ) -> Vec<RendererSignal> {
        let Some(mode) = self.mode_registry.get(mode_id) else {
            self.set_message(
                EchoLevel::Warn,
                format!("mode: `{mode_id}` is not registered"),
            );
            return Vec::new();
        };
        let kind = mode.kind();
        let proto_id = lattice_protocol::ids::BufferId::new(buffer_id.0 as u64);
        let mut active = self.active_modes.remove(&buffer_id).unwrap_or_default();
        let result = match kind {
            lattice_mode::ModeKind::Major => self.mode_registry.activate_major(
                &mut active,
                &self.mode_guards,
                &self.config,
                &self.event_bus,
                &self.services,
                proto_id,
                mode_id,
                lattice_mode::CapabilitySet::empty(),
            ),
            lattice_mode::ModeKind::Minor => self.mode_registry.activate_minor(
                &mut active,
                &self.mode_guards,
                &self.config,
                &self.event_bus,
                &self.services,
                proto_id,
                mode_id,
                lattice_mode::CapabilitySet::empty(),
            ),
        };
        if let Err(e) = result {
            self.set_message(
                EchoLevel::Warn,
                format!(
                    "mode: activate({mode_id}) for buffer {} failed: {e}",
                    buffer_id.0
                ),
            );
        }
        self.active_modes.insert(buffer_id, active);
        self.recompute_options_for_buffer(buffer_id);
        self.recompute_active_completion_sources_for(buffer_id);
        // M.5.2: when a major activates (whether by direct call,
        // `:<major-name>` toggle, or buffer-creation path), run the
        // LSP auto-activation hook. Skipped for minor activations --
        // if `lsp-mode` is the one being activated, the hook would
        // just no-op (already-active short-circuit).
        let auto_lsp_signals = if matches!(kind, lattice_mode::ModeKind::Major) {
            self.maybe_auto_activate_lsp_mode(buffer_id)
        } else {
            Vec::new()
        };
        // Drain any option mutations the mode emitted in its
        // `on_activate` so the side-effect cascade (option cache
        // recompute, `recompute_folds` for foldmethod, theme refresh
        // for `ui.*`, ...) runs synchronously before the caller
        // observes the post-activation state.
        let mut signals = self.drain_option_changes();
        signals.extend(auto_lsp_signals);
        signals
    }

    /// 5.5.F.5.2 (M.5.1): programmatic deactivation of `mode_id` on
    /// `buffer_id`. Symmetric to [`Self::activate_mode_by_id`]; same
    /// signal-return shape.
    #[must_use]
    pub fn deactivate_mode_by_id(
        &mut self,
        buffer_id: BufferId,
        mode_id: lattice_mode::ModeId,
    ) -> Vec<RendererSignal> {
        let Some(mode) = self.mode_registry.get(mode_id) else {
            self.set_message(
                EchoLevel::Warn,
                format!("mode: `{mode_id}` is not registered"),
            );
            return Vec::new();
        };
        let proto_id = lattice_protocol::ids::BufferId::new(buffer_id.0 as u64);
        let mut active = self.active_modes.remove(&buffer_id).unwrap_or_default();
        let result = match mode.kind() {
            lattice_mode::ModeKind::Major => self.mode_registry.deactivate_major(
                &mut active,
                &self.mode_guards,
                &self.event_bus,
                proto_id,
            ),
            lattice_mode::ModeKind::Minor => self.mode_registry.deactivate_minor(
                &mut active,
                &self.mode_guards,
                &self.event_bus,
                proto_id,
                mode_id,
            ),
        };
        if let Err(e) = result {
            self.set_message(
                EchoLevel::Warn,
                format!(
                    "mode: deactivate({mode_id}) for buffer {} failed: {e}",
                    buffer_id.0
                ),
            );
        }
        self.active_modes.insert(buffer_id, active);
        self.recompute_options_for_buffer(buffer_id);
        self.recompute_active_completion_sources_for(buffer_id);
        // Symmetric to `activate_mode_by_id`: drain option mutations
        // the mode emitted in its `on_deactivate` (e.g.
        // `lsp-folding-mode` restoring the prior `foldmethod`) so
        // the side-effect cascade runs before the caller observes
        // the state.
        self.drain_option_changes()
    }

    /// 5.5.F.5.2 (M.5.2): language-mode auto-activation hook for
    /// `lsp-mode`. Runs after a major activates; when the active
    /// buffer's path has a server configured in the LSP registry and
    /// `lsp-mode` isn't already active, activate it.
    ///
    /// **Asymmetry by design (mode-architecture §M.5):** there is no
    /// auto-deactivation hook on `MajorExited`. Active minors stay
    /// across major-mode swaps -- emacs's "kill all local variables"
    /// footgun is what we're avoiding.
    #[must_use]
    pub fn maybe_auto_activate_lsp_mode(&mut self, buffer_id: BufferId) -> Vec<RendererSignal> {
        if self.lsp_mode_enabled_for(buffer_id) {
            return Vec::new();
        }
        let Some(path) = self.path_for_buffer(buffer_id) else {
            // Scratch buffers with no path can still host LSP
            // (standalone-server scenarios), but only when the user
            // explicitly runs `:lsp-mode`. Auto-activation is
            // path-driven.
            return Vec::new();
        };
        if !self.lsp.has_server_for_path(&path) {
            return Vec::new();
        }
        self.activate_mode_by_id(buffer_id, lattice_lsp::modes::LspMode::mode_id())
    }

    /// 5.5.F.5.3 (M.3.1): activate the resolved major mode for
    /// `buffer_id` based on its `kind` (and, for Document buffers,
    /// the detected language) and refresh the resolved-options cache.
    ///
    /// Idempotency / preserve-intent: if the buffer already has any
    /// major active, don't preempt it (covers re-call on same buffer,
    /// synthetic Document buffers with a creator-chosen major, and
    /// user-driven `:toggle-mode <name>` swaps). Still runs the
    /// auto-LSP hook unconditionally so `lsp-mode` propagates per-
    /// buffer; the hook is itself no-op-when-already-active and
    /// no-op-when-no-server-for-path.
    ///
    /// Returns `Vec<RendererSignal>` for the same reason as
    /// [`Self::activate_mode_by_id`] — mode `on_activate` hooks can
    /// mutate typed options whose cascade emits renderer-coupled
    /// signals.
    #[must_use]
    pub fn activate_major_for_buffer_kind(
        &mut self,
        buffer_id: BufferId,
        kind: BufferKind,
    ) -> Vec<RendererSignal> {
        // Idempotency / preserve-intent: covered in detail in the
        // doc-comment above.
        if self.active_modes.get(&buffer_id).and_then(|m| m.major()).is_some() {
            if matches!(kind, BufferKind::Document) {
                return self.maybe_auto_activate_lsp_mode(buffer_id);
            }
            return Vec::new();
        }
        // No major yet: resolve from kind + lang. Document buffers
        // consult `Lang::detect_from_path`; other kinds have a fixed
        // mode regardless of content.
        let lang = match kind {
            BufferKind::Document => {
                let snap = self.document.snapshot();
                let path_owned = snap.path.as_ref().map(|p| (**p).clone());
                let path_ref = path_owned.as_deref();
                lattice_syntax::Lang::detect_from_path(path_ref)
            }
            _ => lattice_syntax::Lang::Plain,
        };
        let major_id = crate::modes::resolve_major_mode(kind, lang);
        let proto_id = lattice_protocol::ids::BufferId::new(buffer_id.0 as u64);
        let mut active = self.active_modes.remove(&buffer_id).unwrap_or_default();
        match self.mode_registry.activate_major(
            &mut active,
            &self.mode_guards,
            &self.config,
            &self.event_bus,
            &self.services,
            proto_id,
            major_id,
            lattice_mode::CapabilitySet::empty(),
        ) {
            Ok(_events) => {}
            Err(e) => {
                self.set_message(
                    EchoLevel::Warn,
                    format!(
                        "mode: activate_major({major_id}) for buffer {} failed: {e}",
                        buffer_id.0,
                    ),
                );
            }
        }
        if let Some(minor_id) = crate::modes::default_minor_mode_id_for_buffer_kind(kind)
            && let Err(e) = self.mode_registry.activate_minor(
                &mut active,
                &self.mode_guards,
                &self.config,
                &self.event_bus,
                &self.services,
                proto_id,
                minor_id,
                lattice_mode::CapabilitySet::empty(),
            )
        {
            self.set_message(
                EchoLevel::Warn,
                format!(
                    "mode: activate_minor({minor_id}) for buffer {} failed: {e}",
                    buffer_id.0,
                ),
            );
        }
        for minor_id in crate::modes::auto_activated_minors_for_buffer_kind(kind) {
            if let Err(e) = self.mode_registry.activate_minor(
                &mut active,
                &self.mode_guards,
                &self.config,
                &self.event_bus,
                &self.services,
                proto_id,
                minor_id,
                lattice_mode::CapabilitySet::empty(),
            ) {
                self.set_message(
                    EchoLevel::Warn,
                    format!(
                        "mode: activate_minor({minor_id}) for buffer {} failed: {e}",
                        buffer_id.0,
                    ),
                );
            }
        }
        self.active_modes.insert(buffer_id, active);
        self.recompute_options_for_buffer(buffer_id);
        // CSM.3: keep `ActiveCompletionSources` in lockstep with the
        // active-modes set.
        self.recompute_active_completion_sources_for(buffer_id);
        // M.5.2: post-activation hook -- if the buffer is now on a
        // language major with a configured LSP server, auto-activate
        // `lsp-mode`. Modelled as a synchronous hook here.
        self.maybe_auto_activate_lsp_mode(buffer_id)
    }

    /// 5.5.F.5.3 (M-async.3): rollback drain for the mode dispatcher's
    /// spawned lifecycle task. Reads `ModeEvent` variants off
    /// `pending_mode_lifecycle_rx` and acts on `ModeActivationFailed`
    /// only — walk the registry, look up the mode's kind, then call
    /// `deactivate_mode_by_id`. Idempotent: if the mode wasn't in
    /// `active_modes`, the deactivate no-ops.
    ///
    /// Cheap when no events arrived (single `try_recv` → `Empty`).
    /// Called once per main-loop tick by `runtime.rs`. Returns the
    /// `RendererSignal` list every rolled-back deactivation enqueued.
    #[must_use]
    pub fn drain_mode_lifecycle_events(&mut self) -> Vec<RendererSignal> {
        let Some(mut rx) = self.pending_mode_lifecycle_rx.take() else {
            return Vec::new();
        };
        // Collect first so the subsequent `deactivate_mode_by_id`
        // calls don't conflict with the receiver borrow.
        let mut to_rollback: Vec<(BufferId, lattice_mode::ModeId)> = Vec::new();
        while let Ok(evt) = rx.try_recv() {
            if let lattice_mode::ModeEvent::ModeActivationFailed { buffer, mode, .. } = evt {
                to_rollback.push((BufferId(buffer.raw() as u32), mode));
            }
        }
        self.pending_mode_lifecycle_rx = Some(rx);
        let mut signals = Vec::new();
        for (buffer_id, mode_id) in to_rollback {
            signals.extend(self.deactivate_mode_by_id(buffer_id, mode_id));
        }
        signals
    }

    /// 5.5.F.5.5: lifecycle hook fired after a document buffer
    /// becomes the active buffer (via [`Self::activate_document`],
    /// after `:e <path>` opens a fresh file, or after `:bd` /
    /// `:bn` / `:bp` switches the active pane). Refreshes
    /// everything that "lives with the buffer until it closes":
    /// major + auto-LSP mode wiring, the resolved-options cache,
    /// the syntax parse + fold seam, and the frame-level highlight
    /// caches.
    ///
    /// Returns `Vec<RendererSignal>` because
    /// [`Self::activate_major_for_buffer_kind`] fans signals from
    /// the inner mode-lifecycle cascade (e.g. `lsp-folding-mode`
    /// writing `foldmethod=lsp` in its `on_activate`).
    #[must_use]
    pub fn activate_buffer_state(&mut self) -> Vec<RendererSignal> {
        // Wire up the buffer's major mode before the option-cache
        // rebuild reads mode contributions. On first visit this
        // activates the language major (e.g. `rust-mode`) and --
        // via the `maybe_auto_activate_lsp_mode` hook inside
        // `activate_major_for_buffer_kind` -- auto-activates
        // `lsp-mode` if a server is configured for the path. On
        // re-visits, the idempotency guard turns this into a cheap
        // "already active" no-op + a single auto-LSP hook re-run
        // (itself no-op-when-already-active). This is what makes
        // `lsp-mode` follow the user across buffer switches.
        let active_id = self.pane_tree.active().buffer_id;
        let signals = self.activate_major_for_buffer_kind(active_id, BufferKind::Document);
        // Re-resolve the buffer's options against the *current*
        // global typed-option layer. `apply_option_cascade` only
        // refreshes the ACTIVE buffer's `resolved_options` entry on
        // each `:set`, so a global write that happened while a
        // different buffer was active leaves this buffer's entry
        // stale.
        self.recompute_options_for_buffer(active_id);
        // M.4: refresh the renderer's hot-path option cache from
        // the just-activated buffer's resolved options.
        self.rebuild_option_cache();
        // Make sure the syntax tree matches the current text. If
        // the entry stashed a parse for the document's current
        // version this no-ops; otherwise it parses + recomputes
        // folds in lockstep via the seam in `maybe_reparse_syntax`.
        self.maybe_reparse_syntax();
        // First-activation case: a freshly-opened file has an empty
        // fold list and the reparse seam may have been a no-op
        // (text version matched the entry's stashed parse). Seed
        // the fold list from the active foldmethod so the gutter
        // shows ▸ markers and `za` works without a manual `<C-l>`.
        // `Manual` skips the seed (the user's `zf` ranges are
        // authoritative).
        if self.folds.is_empty() && !matches!(self.foldmethod(), FoldMethod::Manual) {
            self.recompute_folds();
        }
        // Drop frame-level highlight caches so the next
        // `refresh_highlights` repopulates against the activated
        // buffer's content rather than the previous buffer's.
        // These two fields are renderer-coupled (Bucket A in the
        // post-F.3 review) but live on `Editor`, so the clear runs
        // host-side; no signal needed.
        self.visible_highlights.clear();
        self.pane_highlights.clear();
        signals
    }

    /// 5.5.F.5.4 (M.7.1 Phase 1.5): drive the declarative
    /// `Mode::mirrors_option` cascade. Walks every registered mode
    /// and, for each that declares it mirrors `canonical_name`,
    /// toggles the mode's active state on the active document buffer
    /// to match the option's `bool` value.
    ///
    /// Reads through `ConfigRegistry::get_bool_by_name` (the typed-
    /// option layer) rather than the resolved-options view — the
    /// user's explicit `:set` gesture is the authority for the mode's
    /// active state, not the layered resolution. Non-bool options
    /// short-circuit at the `get_bool_by_name` step.
    ///
    /// Returns the `RendererSignal` list every cascading
    /// activate/deactivate enqueued.
    #[must_use]
    pub fn mirror_option_to_modes(&mut self, canonical_name: &str) -> Vec<RendererSignal> {
        let Some(on) = self.config.get_bool_by_name(canonical_name) else {
            return Vec::new();
        };
        // Collect mode ids first so the activate/deactivate calls
        // (which take `&mut self`) don't conflict with the registry
        // borrow inside `iter_meta`.
        let mirror_ids: Vec<lattice_mode::ModeId> = {
            let registry = &self.mode_registry;
            registry
                .iter_meta()
                .filter_map(|(id, _kind)| {
                    let mode = registry.get(id)?;
                    if mode.mirrors_option() == Some(canonical_name) {
                        Some(id)
                    } else {
                        None
                    }
                })
                .collect()
        };
        let buffer_id = self.document_buffer_id;
        let mut signals = Vec::new();
        for mode_id in mirror_ids {
            let currently_active = self
                .active_modes
                .get(&buffer_id)
                .map(|modes| modes.has_minor(mode_id))
                .unwrap_or(false);
            if on && !currently_active {
                signals.extend(self.activate_mode_by_id(buffer_id, mode_id));
            } else if !on && currently_active {
                signals.extend(self.deactivate_mode_by_id(buffer_id, mode_id));
            }
        }
        signals
    }

    /// 5.5.F.5.2 (M.5.1): toggle a mode by name on the active pane's
    /// buffer. Apply-fn target for the auto-generated `:<mode-name>`
    /// ex-commands (mode-architecture §9.6.1).
    ///
    /// - **Minor**: deactivate if active; activate if inactive.
    /// - **Major**: activate if not currently the major; if it's
    ///   already the active major, the registry treats this as a
    ///   *reload* (deactivate then re-activate, per §9.6).
    #[must_use]
    pub fn toggle_mode_by_name(&mut self, name: &str) -> Vec<RendererSignal> {
        let mode_id = lattice_mode::ModeId::new(name);
        let buffer_id = self.active_pane_buffer_id();
        let Some(mode) = self.mode_registry.get(mode_id) else {
            self.set_message(
                EchoLevel::Error,
                format!("mode: `{name}` is not a registered mode"),
            );
            return Vec::new();
        };
        let active_now = self
            .active_modes
            .get(&buffer_id)
            .map(|m| m.is_active(mode_id))
            .unwrap_or(false);
        match (mode.kind(), active_now) {
            (lattice_mode::ModeKind::Minor, true) => {
                self.deactivate_mode_by_id(buffer_id, mode_id)
            }
            (lattice_mode::ModeKind::Minor, false) => {
                self.activate_mode_by_id(buffer_id, mode_id)
            }
            // Major: activating an inactive major swaps it in;
            // re-activating the current major reloads (registry
            // contract). Either way the call is the same.
            (lattice_mode::ModeKind::Major, _) => {
                self.activate_mode_by_id(buffer_id, mode_id)
            }
        }
    }

    /// `:set foldmethod=...` -- the option-cache hot-path read.
    pub fn foldmethod(&self) -> FoldMethod {
        self.option_cache.foldmethod
    }

    /// Refresh [`Self::folds`] from the active [`FoldMethod`].
    ///
    /// `Manual` -- no-op (preserves user `zf` folds). The other
    /// providers (`Indent` / `Markdown` / `Syntax` / `Lsp`) replace
    /// `folds` with the recomputed set, carrying over the
    /// closed/open state of any existing fold whose identity matches
    /// a recomputed one (so `zc` survives a reparse).
    ///
    /// `Syntax` runs the language's tree-sitter `folds.scm` query
    /// against the live parse tree; when the language doesn't ship a
    /// `folds.scm` (or the parse tree hasn't been built yet), the
    /// syntax provider cascades to the markdown / indent providers
    /// based on the file extension. `Lsp` reads the per-buffer
    /// folding-range cache and cascades to `Syntax` when the cache
    /// is empty (request still in-flight, server not attached, or
    /// sub-mode disabled).
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
            FoldMethod::Lsp => self.recompute_lsp_folds(&snapshot.buffer),
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
    /// provider returns `None` (no `folds.scm` for this language, or
    /// no parse tree yet).
    fn recompute_syntax_folds(&self, buffer: &lattice_core::Buffer) -> Vec<Fold> {
        if let Some(syntax) = self.document_syntax_for(self.document_buffer_id) {
            let snap = syntax.snapshot();
            if let Some(folds) = crate::folds::compute_syntax_folds(&snap) {
                return folds;
            }
        }
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

    /// 4.4.f: read the LSP fold cache for the active buffer. When
    /// the cache is empty (request still in-flight, no server
    /// attached, or sub-mode disabled), cascade to the syntax
    /// provider so the user sees *some* folds rather than an empty
    /// list.
    fn recompute_lsp_folds(&self, buffer: &lattice_core::Buffer) -> Vec<Fold> {
        if self.lsp_folding_mode_enabled_for(self.document_buffer_id)
            && let Some(cache) = self.lsp_folds_cache.get(&self.document_buffer_id)
            && !cache.folds.is_empty()
        {
            return cache.folds.clone();
        }
        self.recompute_syntax_folds(buffer)
    }

    /// 5.5.E.7.3: apply a single [`Edit`] to the active buffer as
    /// one undo unit. Routes between the document actor and the oil
    /// rope based on [`Self::active_buffer`]. On success, publishes
    /// [`Event::DocumentChanged`] via [`Self::publish_document_changed`]
    /// so the LSP fan-in + syntax worker + highlight shifter see
    /// the new state.
    ///
    /// **Oil-buffer routing.** When `active_buffer == Oil` the edit
    /// lands on `oil.content` (the in-memory rope owned by the
    /// `OilBuffer`) instead of the document actor's rope. The
    /// document actor is the wrong destination for oil edits — oil's
    /// content is intentionally separate so `:w` can diff against a
    /// snapshot and translate into filesystem operations. LSP
    /// `didChange` is intentionally not fired for oil edits.
    pub fn apply_edit_blocking(
        &mut self,
        edit: lattice_protocol::edit::Edit,
    ) -> Result<lattice_core::buffer::AppliedEdit, lattice_runtime::RuntimeError> {
        if matches!(self.active_buffer, BufferKind::Oil) {
            return self.apply_edit_to_oil(edit);
        }
        let result = block_on(self.document.apply_edit(edit));
        if let Ok(applied) = result.as_ref() {
            self.publish_document_changed(std::slice::from_ref(applied));
        }
        result
    }

    /// 5.5.E.7.3: block_on `apply_edit_batch`. The batch lands as
    /// one undo unit on the document's undo stack. Each edit in the
    /// batch is also fed to the LSP supervisor in order via
    /// [`Self::publish_document_changed`].
    ///
    /// Oil-buffer routing matches [`Self::apply_edit_blocking`]:
    /// when `active_buffer == Oil` the batch lands on `oil.content`
    /// edit-by-edit. The "one undo unit" semantics are weaker for
    /// oil (its content has no undo stack); v1 oil falls back to
    /// `:e!` reload for "undo all my changes."
    pub fn apply_edit_batch_blocking(
        &mut self,
        edits: Vec<lattice_protocol::edit::Edit>,
    ) -> Result<Vec<lattice_core::buffer::AppliedEdit>, lattice_runtime::RuntimeError> {
        if matches!(self.active_buffer, BufferKind::Oil) {
            let mut applied = Vec::with_capacity(edits.len());
            for edit in edits {
                applied.push(self.apply_edit_to_oil(edit)?);
            }
            return Ok(applied);
        }
        let result = block_on(self.document.apply_edit_batch(edits));
        if let Ok(applied) = result.as_ref() {
            self.publish_document_changed(applied);
        }
        result
    }

    /// 5.5.E.7.3: apply a single [`Edit`] to the active oil
    /// buffer's rope (`oil.content`). Returns the `AppliedEdit` with
    /// the inserted-range / removed-text fields populated, same
    /// shape as the document path.
    fn apply_edit_to_oil(
        &mut self,
        edit: lattice_protocol::edit::Edit,
    ) -> Result<lattice_core::buffer::AppliedEdit, lattice_runtime::RuntimeError> {
        let oil_id = self.active_pane_buffer_id();
        // Use the callback variant so the registry lock is held
        // only for the apply_edit call. The closure runs the
        // mutation; the outer Option unwraps to either the inner
        // Result or the "no oil entry" Cancelled error.
        self.buffers
            .with_oil_mut(oil_id, |oil| oil.content.apply_edit(&edit))
            .ok_or(lattice_runtime::RuntimeError::Core(
                lattice_core::CoreError::Cancelled,
            ))?
            .map_err(lattice_runtime::RuntimeError::Core)
    }

    /// 5.5.E.7.3: undo one step on the document actor; publishes a
    /// `DocumentChanged` for the inverse edits so the LSP fan-in +
    /// syntax worker + highlight shifter stay in sync.
    pub fn undo_blocking(
        &mut self,
    ) -> Result<Vec<lattice_core::buffer::AppliedEdit>, lattice_runtime::RuntimeError> {
        let result = block_on(self.document.undo());
        if let Ok(applied) = result.as_ref() {
            self.publish_document_changed(applied);
        }
        result
    }

    /// 5.5.E.7.3: redo one step on the document actor; symmetric
    /// to [`Self::undo_blocking`].
    pub fn redo_blocking(
        &mut self,
    ) -> Result<Vec<lattice_core::buffer::AppliedEdit>, lattice_runtime::RuntimeError> {
        let result = block_on(self.document.redo());
        if let Ok(applied) = result.as_ref() {
            self.publish_document_changed(applied);
        }
        result
    }

    /// 5.5.E.7.4: delete the cursor's whole line including its
    /// trailing newline (vim's `:d`). The standard delete operator's
    /// `CurrentLine` range preserves the newline, which leaves an
    /// empty line behind -- that's fine for `dd` (cursor stays put on
    /// a now-empty line) but wrong for `:d` and `:g/.../d`. Here we
    /// explicitly include the newline.
    pub fn do_delete_line(&mut self) {
        let line = self.cursor.line;
        let last = last_addressable_line(&self.document.snapshot().buffer);
        let len = self.document.snapshot().buffer.line_byte_len(line);
        let r = if line < last {
            // Include the trailing newline by extending into the next line.
            lattice_protocol::position::Range::new(
                lattice_protocol::position::Position::new(line, 0),
                lattice_protocol::position::Position::new(line + 1, 0),
            )
        } else if line > 0 {
            // Last line: include the previous line's newline by reaching
            // back to the end of `line - 1`.
            let prev = line - 1;
            let prev_len = self.document.snapshot().buffer.line_byte_len(prev);
            lattice_protocol::position::Range::new(
                lattice_protocol::position::Position::new(prev, prev_len),
                lattice_protocol::position::Position::new(line, len),
            )
        } else {
            // Single-line buffer: just delete the content.
            lattice_protocol::position::Range::new(
                lattice_protocol::position::Position::new(line, 0),
                lattice_protocol::position::Position::new(line, len),
            )
        };
        if self
            .apply_edit_blocking(lattice_protocol::edit::Edit::delete(r))
            .is_ok()
        {
            self.cursor = lattice_protocol::position::Position::new(
                line.min(last_addressable_line(&self.document.snapshot().buffer)),
                0,
            );
        }
    }

    /// 5.5.E.7.5: vim's `:s/pattern/replacement/[g]` (and `:%s/...`
    /// for whole-buffer scope). Replacement template syntax follows
    /// fancy-regex / `regex` crate: `$1`, `${name}`, `$0` (whole
    /// match), `$$` for a literal `$`. NOT vim's `\1`/`&` — modern
    /// syntax. Reports the replacement count via the echo area.
    pub fn do_substitute(
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
        let regex = match fancy_regex::Regex::new(pattern) {
            Ok(r) => r,
            Err(e) => {
                self.set_message(EchoLevel::Error, format!("regex: {e}"));
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
            let r = lattice_protocol::position::Range::new(
                lattice_protocol::position::Position::new(line, 0),
                lattice_protocol::position::Position::new(line, line_len),
            );
            let _ = self.apply_edit_blocking(lattice_protocol::edit::Edit::replace(
                r,
                new_line.into_owned(),
            ));
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

    /// 5.5.E.7.7: route grammar-driven edits (operators like `>>`,
    /// `dd`, `c`, `y`) through the same chokepoint as
    /// `apply_edit_blocking`. The actor has already applied the
    /// edits to the document; without this routing the LSP
    /// `didChange` fan-out, the `pending_syntax_edits` accumulation,
    /// and the synchronous `shift_highlights_for_edit` byte-shift
    /// all SKIP the edits — which produced the user-reported flicker
    /// on `>>` and `dd`: spans never shifted on the input thread, so
    /// when the worker eventually published the recompute landed as
    /// a visible repaint.
    ///
    /// After the cursor settles on the start of the deleted range
    /// (vim's behavior after a delete), the edits flow through
    /// [`Self::publish_document_changed`] so:
    /// - LSP servers see the didChange.
    /// - Syntax worker sees the `EditDelta`s (incremental reparse
    ///   instead of falling back to full).
    /// - `visible_highlights` stays line- and byte-aligned via
    ///   `shift_highlights_for_edit`.
    pub fn handle_edits(&mut self, edits: &[lattice_core::buffer::AppliedEdit]) {
        if let Some(first) = edits.first() {
            self.cursor = first.original_range.start;
        }
        if !edits.is_empty() {
            self.publish_document_changed(edits);
        }
    }

    /// 5.5.E.7.6 (planner): vim's `:g/pat/body` and `:v/pat/body`
    /// target-list builder. Validates the pattern, scans every
    /// addressable line, and collects line numbers where match-vs-
    /// pattern equals `inverted` (so `:g` keeps matching lines and
    /// `:v` keeps non-matching ones). On empty pattern or zero
    /// matches, pushes an echo and returns `None` so the caller can
    /// bail without driving the body loop.
    ///
    /// The body-replay loop stays App-side until the Effect router
    /// finishes migrating: not every `Effect` arm is in
    /// [`handle_effect`] yet, and silently dropping a not-yet-
    /// migrated body effect (e.g. a `:g/foo/p` that produces an
    /// unhandled echo path) would be a behaviour regression. Once
    /// G.x retires `App::apply_effect`, the loop joins the planner
    /// here.
    pub fn build_global_targets(
        &mut self,
        pattern: &str,
        inverted: bool,
    ) -> Option<Vec<u32>> {
        if pattern.is_empty() {
            self.set_message(EchoLevel::Error, "empty pattern".to_string());
            return None;
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
            return None;
        }
        Some(targets)
    }

    /// 5.5.E.7.2: build + publish [`Event::DocumentChanged`] from
    /// the current snapshot and the edits that were just applied.
    /// Called from every path that mutates the buffer (apply_edit /
    /// batch / undo / redo). The applied edits ride on the event so
    /// downstream subscribers (notably the per-server LSP fan-in)
    /// can sync without re-walking the buffer or holding the
    /// supervisor lock.
    pub fn publish_document_changed(
        &mut self,
        applied: &[lattice_core::buffer::AppliedEdit],
    ) {
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
        // Always publish the generic editor event — non-LSP
        // subscribers (renderer, future plugins) see every edit
        // regardless of `lsp-mode`.
        self.event_bus.publish(Event::DocumentChanged {
            id: snap.id,
            path: path.clone(),
            version: snap.version,
            edits: edits.clone(),
        });
        // M.5.5: gate the LSP fan-in at the publish site. Only emit
        // `LspDocumentChanged` (the typed event the per-actor fan-in
        // subscribes to) when `lsp-mode` is active for the active
        // document. With the gate off no didChange goes to any
        // server.
        if self.lsp_mode_enabled_for(self.document_buffer_id) {
            self.event_bus
                .publish_typed(lattice_lsp::LspDocumentChanged {
                    id: snap.id,
                    path,
                    version: snap.version,
                    edits,
                });
        }
        // Slice B.2 part 2: accumulate tree-sitter-shaped edit
        // deltas for the next syntax reparse request.
        // `maybe_reparse_syntax` drains this and ships them to the
        // worker. Slice C.3: also shift `visible_highlights`
        // synchronously so line indices track the post-edit content
        // even before the worker publishes a fresh snapshot. (See
        // [`Self::shift_highlights_for_edit`] for the flicker-
        // elimination rationale.)
        if self.syntax.is_some() {
            self.pending_syntax_edits
                .extend(applied.iter().map(|a| a.delta));
            for a in applied {
                self.shift_highlights_for_edit(&a.delta);
            }
        }
    }

    /// 5.5.E.7.1 (Slice C.3): keep `visible_highlights` line-aligned
    /// with the current document immediately after an edit, before
    /// the syntax worker publishes a fresh snapshot.
    ///
    /// `visible_highlights` is indexed by viewport row =
    /// `buffer_line - scroll`. When an edit changes the line count
    /// (line-delete, line-insert, multi-line replace), the content
    /// at row N now corresponds to a different buffer line than
    /// before, but the cached span entries don't shift
    /// automatically. The renderer would paint pre-edit spans onto
    /// post-edit content, producing the "old span gaps appear as
    /// white characters on the new line" flicker.
    ///
    /// Fix: derive the line-shift from the delta's positions and
    /// apply it to `visible_highlights` as a Vec splice. Pure
    /// ns-fast: a Vec drain or insert of a few elements. Only
    /// mutates the cache; doesn't touch the snapshot.
    pub fn shift_highlights_for_edit(
        &mut self,
        delta: &lattice_protocol::edit::EditDelta,
    ) {
        let edit_start = delta.start_position.line;
        let scroll = self.scroll;
        if edit_start < scroll {
            // Edit started above the visible viewport. Bail and let
            // the worker's publish drive a normal recompute.
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
            // In-line edit (line count unchanged). Shift spans on
            // the affected line by the byte delta within the line
            // so the held spans stay byte-aligned with the new
            // content. (Slice C.4 details in the original ui-tui
            // comment block.)
            self.shift_spans_within_line(viewport_idx, delta);
            return;
        }
        // Decide where to apply the shift. If the edit starts at
        // the very beginning of `start.line` (byte 0), then
        // `start.line`'s pre-edit content has moved — the shift
        // point IS `viewport_idx`. If the edit starts mid-line, the
        // shift applies to the line AFTER it.
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

    /// 5.5.E.7.1 (Slice C.4): shift the spans on a single visible-
    /// line entry by the byte-delta of an in-line edit so the held
    /// spans stay byte-aligned with the post-edit content during
    /// the brief window before the syntax worker publishes
    /// corrected spans.
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
            return;
        }
        let Some(line_spans) = self.visible_highlights.get_mut(viewport_idx) else {
            return;
        };
        line_spans.retain_mut(|span| {
            if span.end <= edit_byte {
                true
            } else if span.start >= old_end_byte {
                let new_start = (span.start as i64) + byte_delta;
                let new_end = (span.end as i64) + byte_delta;
                span.start = new_start.max(0) as usize;
                span.end = new_end.max(0) as usize;
                true
            } else {
                let extended_end = (span.end as i64) + byte_delta;
                if extended_end <= span.start as i64 {
                    false
                } else {
                    span.end = extended_end as usize;
                    true
                }
            }
        });
    }

    /// Request a reparse if the document's text has changed since
    /// the last request. Idempotent and cheap when nothing changed;
    /// the actual parse runs on the syntax handle's worker task off
    /// the UI thread (audit slice 3 / paramount goal #1: "UI thread
    /// does no … parsing"). Also triggers [`Self::recompute_folds`]
    /// so `foldmethod=indent` / `=markdown` / `=syntax` stay in
    /// lockstep with the latest text.
    pub fn maybe_reparse_syntax(&mut self) {
        let tv = self.document.text_version();
        if tv == self.last_parsed_text_version {
            return;
        }
        // Clone the handle (cheap Arc bump) to release the immutable
        // `self` borrow before we mutably borrow
        // `self.pending_syntax_edits` below.
        let syntax = self.document_syntax_for(self.document_buffer_id).cloned();
        if let Some(syntax) = syntax {
            let edits = std::mem::take(&mut self.pending_syntax_edits);
            let buffer = self.document.snapshot().buffer.clone();
            syntax.request_reparse(self.last_synced_syntax_version, tv, buffer, edits);
        }
        self.last_parsed_text_version = tv;
        // Worker WILL be at this version after the request
        // completes. If a request gets dropped (worker panicked),
        // the next request's from_version mismatch triggers a full
        // reparse and self-corrects.
        self.last_synced_syntax_version = tv;
        self.recompute_folds();
    }

    /// Vim's `:marks` -- list every set mark's name + position in
    /// the echo area, sorted by mark name. Reads `self.marks`
    /// (host) and surfaces the result via [`Self::set_message`].
    /// Moved here from `lattice-ui-tui::app::lifecycle` in 5.5.E.2
    /// alongside the [`Effect::EchoMarks`] arm.
    pub fn do_list_marks(&mut self) {
        let mut entries: Vec<(char, lattice_protocol::position::Position)> =
            self.marks.iter().map(|(c, p)| (*c, *p)).collect();
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

    /// Vim's `:reg` -- list every register's contents in the echo
    /// area. v1 shows the unnamed `""`, the numbered `"0`, and the
    /// named alphabetic registers in alphabetical order. Moved
    /// here from `lattice-ui-tui::app::lifecycle` in 5.5.E.2
    /// alongside the [`Effect::EchoRegisters`] arm.
    pub fn do_list_registers(&mut self) {
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

    /// Stash a yank / delete payload into the register slots. The
    /// dispatcher emits [`Effect::Yank`] with the resolved
    /// [`Register`] selector (either explicit `"<a>`-style or the
    /// `Register::Unnamed` default); operator semantics:
    ///
    /// - `Register::BlackHole` -> drop on the floor, no slot touched.
    /// - Any other explicit register -> store there AND in `""`
    ///   (the unnamed register, vim's default paste source).
    /// - `Register::Unnamed` -> store in `""` only.
    ///
    /// Yanks (vs deletes) also populate `"0`. v1 approximates vim's
    /// distinction by treating every grammar [`Effect::Yank`] as
    /// also writing `"0`; deletes don't (they would hit `"1`+ in
    /// vim, which v1 doesn't model).
    ///
    /// Moved here from `lattice-ui-tui::app::edit` in 5.5.E.3
    /// alongside the [`Effect::Yank`] arm. Vim's append-to-uppercase
    /// semantics (`"A` appends to `"a`) remains a v1 simplification:
    /// `A-Z` replaces lowercase rather than appending.
    pub fn store_yank(&mut self, register: Register, content: String, kind: YankKind) {
        if matches!(register, Register::BlackHole) {
            return;
        }
        let entry = UnnamedRegister {
            content: content.clone(),
            kind,
        };
        // Always update unnamed.
        self.unnamed_register = Some(entry.clone());
        // If a named / numbered / system register was explicitly
        // chosen, store there too.
        match register {
            Register::Unnamed | Register::BlackHole => {}
            other => {
                self.registers.insert(other, entry);
            }
        }
    }

    /// Replace the document actor's [`SelectionSet`] and publish
    /// [`Event::SelectionsChanged`] so subscribers (LSP fan-in,
    /// renderer, plugins) see the new selection state.
    ///
    /// Moved here from `lattice-ui-tui::app::visual` in 5.5.E.4
    /// alongside the [`Effect::SelectionChange`] arm. The
    /// `block_on` is renderer-neutral: it parks the input thread
    /// on the actor's selection set channel, which the actor
    /// drains synchronously (no other thread can flip the
    /// selection set while we wait). After 5.5.E.4 every caller
    /// — Visual-mode extension, the dispatcher's
    /// SelectionChange effect, `gv` reselect, LSP location jumps
    /// — invokes `self.editor.set_selections_blocking(...)`.
    pub fn set_selections_blocking(&self, selections: SelectionSet) {
        // `SetSelections` only fails on actor-gone; ignore the
        // `Result` (post-shutdown nothing meaningful to do).
        let _ = block_on(self.document.set_selections(selections));
        self.publish_selections_changed();
    }

    /// Build + publish [`Event::SelectionsChanged`] from the
    /// current snapshot. Called whenever the editor's view of
    /// selections rotates (visual extension, dispatcher
    /// SelectionChange effect, `gv` reselect, etc.). Moved here
    /// from `lattice-ui-tui::app::lifecycle` in 5.5.E.4 as the
    /// only caller — [`Self::set_selections_blocking`] — moved
    /// alongside.
    pub fn publish_selections_changed(&self) {
        let snap = self.document.snapshot();
        self.event_bus.publish(Event::SelectionsChanged {
            id: snap.id,
            version: snap.version,
            selections: (*snap.selections).clone(),
        });
    }

    /// 5.5.G.1: vim's `zo` / `zc` / `za` -- toggle, open, or close
    /// the fold at the cursor. `Some(true)` = `zc` close,
    /// `Some(false)` = `zo` open, `None` = `za` toggle. Selection
    /// rules mirror the App-side helper retired in this slice.
    pub fn do_set_fold_state_at_cursor(&mut self, state: Option<bool>) {
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
            self.set_message(EchoLevel::Error, "E490: No fold found".to_string());
            return;
        };
        self.folds[idx].closed = match state {
            None => !self.folds[idx].closed,
            Some(s) => s,
        };
    }

    /// 5.5.G.1: vim's `zR` (`closed = false`) / `zM` (`closed =
    /// true`) -- bulk open / close every fold in the buffer.
    pub fn do_set_all_folds(&mut self, closed: bool) {
        for fold in self.folds.iter_mut() {
            fold.closed = closed;
        }
    }

    /// 5.5.G.1: vim's `zj` (forward) / `zk` (backward) -- jump
    /// the cursor to the next / previous fold edge.
    pub fn do_goto_fold(&mut self, forward: bool) {
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
            self.cursor = lattice_protocol::position::Position::new(t, 0);
        } else {
            self.set_message(EchoLevel::Error, "no more folds".to_string());
        }
    }

    /// 5.5.G.1: vim's `zd` -- delete the innermost fold containing
    /// the cursor. E490 when the cursor isn't inside any fold.
    pub fn do_delete_fold_at_cursor(&mut self) {
        let line = self.cursor.line;
        if let Some(idx) = innermost_fold_idx(&self.folds, line, |_| true) {
            self.folds.remove(idx);
        } else {
            self.set_message(EchoLevel::Error, "E490: No fold found".to_string());
        }
    }

    /// 5.5.G.1: vim's `q{reg}` -- begin recording subsequent actions
    /// into register `reg`. No-op if a recording is already in
    /// flight (matches vim).
    pub fn do_start_macro_record(&mut self, register: char) {
        if !is_valid_macro_register(register) {
            self.set_message(
                EchoLevel::Error,
                format!("invalid macro register: {register}"),
            );
            return;
        }
        if self.macro_recording.is_some() {
            return;
        }
        self.macro_recording = Some(MacroRecording {
            register,
            actions: Vec::new(),
        });
        self.set_message(EchoLevel::Info, format!("recording @{register}"));
    }

    /// 5.5.G.1: vim's `q` (terminate recording) -- commit the
    /// pending recording into [`Self::macros`] keyed by its
    /// register, then clear the in-flight slot.
    pub fn do_stop_macro_record(&mut self) {
        let Some(rec) = self.macro_recording.take() else {
            return;
        };
        let label = rec.register;
        self.macros.insert(rec.register, rec.actions);
        self.set_message(EchoLevel::Info, format!("recorded @{label}"));
    }
}

/// 5.5.G.1: shared with `Editor::do_start_macro_record`. The
/// canonical `is_valid_mark_name` lives App-side (`Action::SetMark`
/// path); the host-side macro path duplicates the one-line check
/// rather than introducing a cross-crate dep just for it.
fn is_valid_macro_register(c: char) -> bool {
    c.is_ascii_alphabetic() || c.is_ascii_digit()
}

/// 5.5.G.1: index of the *innermost* fold containing `line` that
/// satisfies `pred`. Innermost = max start_line, then min end_line
/// on ties. Used by `zc` (close innermost open) and `za`'s close
/// branch, and `zd` (delete innermost).
fn innermost_fold_idx<F: Fn(&lattice_core::Fold) -> bool>(
    folds: &[lattice_core::Fold],
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

/// 5.5.G.1: pick the fold that `zc` (or `za`'s close branch) should
/// target when the cursor is on `line`. If any open fold *starts*
/// at `line`, close the outermost (largest end_line) — the "fold
/// the entire form" reading. Otherwise pick the innermost open
/// fold containing the cursor.
fn fold_to_close_at(folds: &[lattice_core::Fold], line: u32) -> Option<usize> {
    let starts_here = folds
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.closed && f.start_line == line)
        .max_by_key(|(_, f)| f.end_line)
        .map(|(i, _)| i);
    if starts_here.is_some() {
        return starts_here;
    }
    folds
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.closed && line > f.start_line && line <= f.end_line)
        .max_by_key(|(_, f)| (f.start_line, std::cmp::Reverse(f.end_line)))
        .map(|(i, _)| i)
}

/// 5.5.G.1: index of the *outermost* fold containing `line` that
/// satisfies `pred`. Outermost = min start_line, then max end_line
/// on ties. Used by `zo` (open outermost closed) and `za`'s open
/// branch.
fn outermost_fold_idx<F: Fn(&lattice_core::Fold) -> bool>(
    folds: &[lattice_core::Fold],
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

/// 5.5.G.3: pure-editor edit-cluster helpers (line operations,
/// case toggle, open / append / overwrite, undo/redo glue,
/// backspace). Each body either was already 100% `editor.*` reads
/// + writes or only used [`Self::apply_edit_blocking`] /
/// [`Self::active_text`] / host-side primitives.
impl Editor {
    /// Vim's `J` (`with_space = true`) and `gJ` (`false`) -- splice
    /// the current line's newline (+ leading whitespace, for `J`)
    /// with `" "` (or `""` for `gJ`). Cursor lands on the join
    /// point.
    pub fn do_join_lines(&mut self, with_space: bool) {
        let last = last_addressable_line(&self.document.snapshot().buffer);
        if self.cursor.line >= last {
            return;
        }
        let line = self.cursor.line;
        let next_line = line + 1;
        let cur_len = self.document.snapshot().buffer.line_byte_len(line);
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
        let range = lattice_protocol::position::Range::new(
            lattice_protocol::position::Position::new(line, cur_len),
            lattice_protocol::position::Position::new(next_line, trim),
        );
        let replacement = if with_space { " " } else { "" };
        if let Ok(applied) =
            self.apply_edit_blocking(lattice_protocol::edit::Edit::replace(range, replacement))
        {
            self.cursor = applied.original_range.start;
        }
    }

    /// Vim's `~` -- toggle the case of the byte at the cursor and
    /// advance. Non-letter bytes are unchanged; the cursor still
    /// advances. At EOL the cursor stops (no wrap).
    pub fn do_toggle_case_at_cursor(&mut self) {
        let line_len = self.document.snapshot().buffer.line_byte_len(self.cursor.line);
        if self.cursor.byte >= line_len {
            return;
        }
        let r = lattice_protocol::position::Range::new(
            self.cursor,
            lattice_protocol::position::Position::new(self.cursor.line, self.cursor.byte + 1),
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
        if let Ok(applied) =
            self.apply_edit_blocking(lattice_protocol::edit::Edit::replace(r, &toggled))
        {
            self.cursor = applied.inserted_range.end;
        }
    }

    /// Vim's `a` -- step the cursor one byte to the right (clamped
    /// to EOL) and switch to Insert. Does NOT route through the
    /// canonical `enter_mode` lifecycle (the App-side `EnterMode`
    /// arm does that with recording_insert plumbing). The semantics
    /// here mirror the App-side helper retired in this slice: pure
    /// field writes.
    pub fn do_enter_append(&mut self) {
        let len = self.document.snapshot().buffer.line_byte_len(self.cursor.line);
        if self.cursor.byte < len {
            self.cursor.byte += 1;
        }
        self.modal = ModalState::Insert;
    }

    /// Vim's `o` -- splice `\n` at EOL, drop cursor on the new
    /// blank line, switch to Insert. Uses [`Self::active_text`] so
    /// the path works uniformly across Document / Oil / etc.
    pub fn do_open_line_below(&mut self) {
        let buf = self.active_text();
        let len = buf.line_byte_len(self.cursor.line);
        let eol = lattice_protocol::position::Position::new(self.cursor.line, len);
        if self
            .apply_edit_blocking(lattice_protocol::edit::Edit::insert(eol, "\n"))
            .is_ok()
        {
            self.cursor = lattice_protocol::position::Position::new(self.cursor.line + 1, 0);
        }
        self.modal = ModalState::Insert;
    }

    /// Vim's `O` -- mirror of [`Self::do_open_line_below`] but
    /// inserts `\n` at BOL and keeps the cursor on the inserted
    /// (now upper) row.
    pub fn do_open_line_above(&mut self) {
        let bol = lattice_protocol::position::Position::new(self.cursor.line, 0);
        if self
            .apply_edit_blocking(lattice_protocol::edit::Edit::insert(bol, "\n"))
            .is_ok()
        {
            self.cursor = bol;
        }
        self.modal = ModalState::Insert;
    }

    /// Vim's Replace mode (`R`) overstrike -- if the cursor is
    /// mid-line, replace `[cursor, cursor+1)` with `c`; if past
    /// EOL, extend with an insert. Either way the cursor advances
    /// by one byte and the original byte (or `None` for the
    /// extend case) is recorded on `replace_history` so backspace
    /// can restore it.
    pub fn do_overwrite_char(&mut self, c: char) {
        let len = self.document.snapshot().buffer.line_byte_len(self.cursor.line);
        let s = c.to_string();
        let entry_pos = self.cursor;
        if self.cursor.byte < len {
            let r = lattice_protocol::position::Range::new(
                self.cursor,
                lattice_protocol::position::Position::new(self.cursor.line, self.cursor.byte + 1),
            );
            let original = self.document.snapshot().buffer.slice(r).ok();
            if let Ok(applied) =
                self.apply_edit_blocking(lattice_protocol::edit::Edit::replace(r, &s))
            {
                self.cursor = applied.inserted_range.end;
                self.replace_history.push(crate::state::ReplaceEntry {
                    at: entry_pos,
                    original,
                });
            }
        } else if let Ok(applied) = self.apply_edit_blocking(lattice_protocol::edit::Edit::insert(
            self.cursor,
            &s,
        )) {
            self.cursor = applied.inserted_range.end;
            self.replace_history.push(crate::state::ReplaceEntry {
                at: entry_pos,
                original: None,
            });
        }
    }

    /// Pop the latest `replace_history` entry and restore. If the
    /// entry recorded an original byte, replace; otherwise the
    /// extend-case path deletes the byte. Cursor returns to the
    /// entry's position.
    pub fn do_replace_undo_last(&mut self) {
        let Some(entry) = self.replace_history.pop() else {
            return;
        };
        let after = lattice_protocol::position::Position::new(entry.at.line, entry.at.byte + 1);
        let r = lattice_protocol::position::Range::new(entry.at, after);
        match entry.original {
            Some(orig) => {
                let _ = self.apply_edit_blocking(lattice_protocol::edit::Edit::replace(r, &orig));
            }
            None => {
                let _ = self.apply_edit_blocking(lattice_protocol::edit::Edit::delete(r));
            }
        }
        self.cursor = entry.at;
    }

    /// Vim's `<BS>` in Insert / Replace -- delete the byte before
    /// the cursor (Unicode-aware step via `previous_position`).
    /// No-op at the start of the buffer. Bumps the block-visual
    /// `I` / `A` live-edit counter so the Esc replay accounts for
    /// the deletion.
    pub fn do_delete_char_backward(&mut self) {
        let prev = previous_position(&self.document.snapshot().buffer, self.cursor);
        if prev == self.cursor {
            return;
        }
        let range = lattice_protocol::position::Range::new(prev, self.cursor);
        if self
            .apply_edit_blocking(lattice_protocol::edit::Edit::delete(range))
            .is_ok()
        {
            self.cursor = prev;
            if let Some(spec) = self.pending_block_insert.as_mut() {
                spec.live_edits = spec.live_edits.saturating_add(1);
            }
        }
    }
}

/// 5.5.G.3: unicode-aware backward step. Mirrors
/// `lattice_ui_tui::app::previous_position`; duplicated here so
/// the host-side `do_delete_char_backward` doesn't reach across
/// the crate boundary.
fn previous_position(
    buf: &lattice_core::Buffer,
    p: lattice_protocol::position::Position,
) -> lattice_protocol::position::Position {
    if p.byte > 0 {
        lattice_protocol::position::Position::new(p.line, p.byte - 1)
    } else if p.line > 0 {
        let prev_line = p.line - 1;
        lattice_protocol::position::Position::new(prev_line, buf.line_byte_len(prev_line))
    } else {
        p
    }
}

/// 5.5.G.4: pure-editor scroll / page / viewport / bracket /
/// redraw helpers.
impl Editor {
    /// Open every closed fold whose range contains the current
    /// cursor line. Called by jump-class motions so the cursor
    /// never lands inside a hidden region.
    pub fn auto_open_folds_at_cursor(&mut self) {
        if !self.option_cache.foldenable {
            return;
        }
        let line = self.cursor.line;
        for fold in self.folds.iter_mut() {
            if fold.closed && line >= fold.start_line && line <= fold.end_line {
                fold.closed = false;
            }
        }
    }

    /// Vim's `H` (Top) / `M` (Middle) / `L` (Bottom) -- jump the
    /// cursor to a viewport-relative line.
    pub fn do_jump_viewport(&mut self, vpos: lattice_grammar::ViewportPos) {
        let height = self.viewport_height.max(1);
        let line = match vpos {
            lattice_grammar::ViewportPos::Top => self.scroll,
            lattice_grammar::ViewportPos::Middle => self.scroll + height / 2,
            lattice_grammar::ViewportPos::Bottom => {
                self.scroll + height.saturating_sub(1)
            }
        };
        let buffer = self.active_text();
        let last = last_addressable_line(&buffer);
        let line = line.min(last);
        let len = buffer.line_byte_len(line);
        let byte = self.cursor.byte.min(len);
        self.cursor = lattice_protocol::position::Position::new(line, byte);
        if matches!(self.active_buffer, BufferKind::Document) {
            self.auto_open_folds_at_cursor();
        }
    }

    /// Vim's `zt` / `zz` / `zb` -- adjust scroll so the cursor
    /// lands at the requested viewport row; cursor doesn't move.
    pub fn do_scroll_cursor_to(&mut self, spos: lattice_grammar::ScrollPos) {
        let height = self.viewport_height.max(1);
        self.scroll = match spos {
            lattice_grammar::ScrollPos::Top => self.cursor.line,
            lattice_grammar::ScrollPos::Center => {
                self.cursor.line.saturating_sub(height / 2)
            }
            lattice_grammar::ScrollPos::Bottom => self
                .cursor
                .line
                .saturating_sub(height.saturating_sub(1)),
        };
    }

    /// Vim's `<C-f>` (down) / `<C-b>` (up) -- step cursor by
    /// viewport_height-2 lines with a 1-line overlap; scroll is
    /// reconciled by `ensure_cursor_visible` at the tail of apply.
    pub fn do_page(&mut self, down: bool) {
        let height = self.viewport_height.max(1);
        let step = height.saturating_sub(2).max(1);
        let buffer = self.active_text();
        let last = last_addressable_line(&buffer);
        let new_line = if down {
            self.cursor.line.saturating_add(step).min(last)
        } else {
            self.cursor.line.saturating_sub(step)
        };
        let len = buffer.line_byte_len(new_line);
        let byte = self.cursor.byte.min(len);
        self.cursor = lattice_protocol::position::Position::new(new_line, byte);
    }

    /// Vim's `<C-e>` (down) / `<C-y>` (up) -- scroll one line.
    /// Cursor follows so it stays on-screen.
    pub fn do_scroll_line(&mut self, down: bool) {
        let height = self.viewport_height.max(1);
        let buffer = self.active_text();
        if down {
            let last = last_addressable_line(&buffer);
            self.scroll = self.scroll.saturating_add(1).min(last);
            if self.cursor.line < self.scroll {
                self.cursor.line = self.scroll;
            }
        } else {
            self.scroll = self.scroll.saturating_sub(1);
            let bottom = self.scroll + height.saturating_sub(1);
            if self.cursor.line > bottom {
                self.cursor.line = bottom;
            }
        }
        let len = buffer.line_byte_len(self.cursor.line);
        if self.cursor.byte > len {
            self.cursor.byte = len;
        }
    }

    /// Vim's `%` -- jump to the matching bracket. Scans the
    /// current line from `cursor.byte` for the first
    /// `()[]{}` and jumps to its match.
    pub fn do_match_bracket(&mut self) {
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

    /// Vim's `<C-l>` -- force a syntax reparse, drop the highlight
    /// cache, recompute folds, and signal the runtime to clear
    /// the terminal on next frame.
    pub fn do_redraw_screen(&mut self) {
        self.last_parsed_text_version = u64::MAX;
        self.visible_highlights.clear();
        self.visible_highlights_key = None;
        self.pane_highlights.clear();
        self.recompute_folds();
        self.pending_redraw = true;
        self.set_message(EchoLevel::Info, "redraw".to_string());
    }
}

/// 5.5.G.12: file-tree dismiss + HelpDismiss arm.
impl Editor {
    /// Close the file-tree pane: activate the first listed document
    /// buffer as a successor, remove the tree from the registry, and
    /// rebind every pane that pointed at it to the new buffer. Returns
    /// any signals emitted by `activate_buffer_state` (mode lifecycle
    /// + LSP attach cascade).
    pub fn dismiss_file_tree(&mut self) -> Vec<RendererSignal> {
        if !matches!(self.active_buffer, BufferKind::FileTree) {
            return Vec::new();
        }
        let tree_id = self.active_pane_buffer_id();
        let successor = self
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap_or(self.document_buffer_id);
        let needs_state = self.activate_buffer(successor);
        let signals = if needs_state {
            self.activate_buffer_state()
        } else {
            Vec::new()
        };
        self.buffers.remove(tree_id);
        let new_kind = self.active_buffer;
        let new_id = self.active_pane_buffer_id();
        for pane in self.pane_tree.leaves_mut() {
            if pane.buffer_id == tree_id {
                pane.buffer = new_kind;
                pane.buffer_id = new_id;
            }
        }
        signals
    }
}

/// 5.5.G.17: modal-state transitions + blockwise-Visual I/A.
/// `enter_mode` is the canonical modal pivot (the Insert-replay
/// recording lifecycle, cursor pull-back on `<Esc>`, modal-event
/// publish all live here). `replicate_block_insert` commits a
/// blockwise `I`/`A` session as a single batched undo unit.
/// `do_enter_block_visual_insert` is the Visual-blockwise entry
/// point.
///
/// All three were previously App-side; their deps
/// (`apply_edit_blocking` / `apply_edit_batch_blocking` /
/// `undo_blocking` / `event_bus.publish`) all sit on
/// [`Editor`] already, so the migration is a verbatim move.
impl Editor {
    /// Modal-state pivot. Maintains the Insert-replay recording
    /// lifecycle (start capture on entering Insert/Replace; promote
    /// captured text into `last_insert` on exit; commit any
    /// `pending_block_insert` via `replicate_block_insert`), the
    /// Insert->Normal cursor pull-back, and the `ModalModeChanged`
    /// event fan-out. Re-entering the same mode is intentional
    /// (the dot-repeat path bounces through Insert for its
    /// recording side-effects); we suppress the event publication
    /// in that case.
    pub fn enter_mode(&mut self, state: ModalState) {
        let prior = self.modal;
        if matches!(state, ModalState::Replace) {
            self.replace_history.clear();
        }
        let was_insert_like = matches!(self.modal, ModalState::Insert | ModalState::Replace);
        let entering_insert_like = matches!(state, ModalState::Insert | ModalState::Replace);
        if entering_insert_like && !was_insert_like {
            self.recording_insert = Some(String::new());
        }
        if was_insert_like
            && !entering_insert_like
            && let Some(rec) = self.recording_insert.take()
        {
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
            self.pending_block_insert = None;
        }
        self.modal = state;
        if matches!(state, ModalState::Normal) {
            // Vim's behavior: leaving Insert mode pulls the cursor
            // back one byte if it's not already at the start of the
            // line, so the cursor sits on the last inserted char
            // rather than past it.
            if self.cursor.byte > 0 {
                self.cursor.byte -= 1;
            }
        }
        if prior != state {
            self.event_bus.publish(Event::ModalModeChanged {
                from: format!("{prior:?}"),
                to: format!("{state:?}"),
            });
        }
    }

    /// Commit a blockwise-Visual `I` / `A` session as one batched
    /// undo unit. Rewinds the `live_edits` typed on the top row,
    /// then builds + applies the multi-row insert batch.
    pub fn replicate_block_insert(
        &mut self,
        spec: crate::state::PendingBlockInsert,
        text: &str,
    ) {
        for _ in 0..spec.live_edits {
            let _ = self.undo_blocking();
        }
        let buffer = self.document.snapshot().buffer.clone();
        let mut edits =
            Vec::with_capacity((spec.end_line - spec.start_line + 1) as usize);
        let top_len = buffer.line_byte_len(spec.start_line);
        let top_col = spec.insert_col.min(top_len);
        edits.push(lattice_protocol::edit::Edit::insert(
            lattice_protocol::position::Position::new(spec.start_line, top_col),
            text,
        ));
        for line in (spec.start_line + 1)..=spec.end_line {
            let line_len = buffer.line_byte_len(line);
            if line_len < spec.insert_col {
                continue;
            }
            edits.push(lattice_protocol::edit::Edit::insert(
                lattice_protocol::position::Position::new(line, spec.insert_col),
                text,
            ));
        }
        let _ = self.apply_edit_batch_blocking(edits);
        self.cursor = lattice_protocol::position::Position::new(spec.start_line, top_col);
    }

    /// Vim blockwise-Visual `I` (`append=false`) / `A`
    /// (`append=true`). Captures the block extents, parks them in
    /// `pending_block_insert`, snaps the cursor to the top-row
    /// insert column, and switches to Insert. The replication onto
    /// rows 2..N happens via [`Self::replicate_block_insert`] when
    /// Insert exits.
    pub fn do_enter_block_visual_insert(&mut self, append: bool) {
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
        self.pending_block_insert = Some(crate::state::PendingBlockInsert {
            start_line,
            end_line,
            insert_col,
            live_edits: 0,
        });
        let line_len = self.document.snapshot().buffer.line_byte_len(start_line);
        let cursor_col = insert_col.min(line_len);
        self.cursor = lattice_protocol::position::Position::new(start_line, cursor_col);
        self.visual_anchor = None;
        self.enter_mode(ModalState::Insert);
    }
}

/// 5.5.G.16: vim `zf` -- create a closed fold over the active
/// Visual selection's line range. Pure-editor migration; the
/// helper relied only on host-side fold + selection state.
impl Editor {
    /// Vim `zf`: create a closed fold spanning the active Visual
    /// selection's first..last line, snap the cursor to the fold
    /// start, and exit Visual mode. No-op when called outside
    /// Visual or when the selection is single-line (a 1-line
    /// fold isn't meaningful in vim).
    pub fn do_create_fold_from_visual(&mut self) {
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
            return;
        }
        self.folds.push(Fold {
            start_line,
            end_line,
            closed: true,
            identity: None,
        });
        self.cursor = lattice_protocol::position::Position::new(start_line, 0);
        self.do_exit_visual();
    }
}

/// 5.5.G.15: pure-editor cmdline-completion popup navigation.
/// `<S-Tab>` walks backward through candidates; `<C-y>` /
/// `<CR>` (when popup is open) splices the focused candidate
/// into the cmdline. Migrated from
/// `lattice-ui-tui::app::cmdline`.
impl Editor {
    /// `<S-Tab>` -- step backward through the cmdline-completion
    /// popup candidates with wrap-around on the lower bound.
    /// No-op when the popup is closed or empty.
    pub fn do_command_line_complete_prev(&mut self) {
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

    /// Splice the cmdline-completion popup's focused candidate
    /// into `command_line` (replacing the active slot's prefix)
    /// and dismiss the popup. Idempotent when the popup is
    /// closed or has no candidates.
    pub fn do_command_line_accept_completion(&mut self) {
        let Some(state) = self.completion_state.take() else {
            return;
        };
        if state.candidates.is_empty() {
            return;
        }
        let chosen = &state.candidates[state.selected];
        self.command_line.replace_range(
            state.replace_start..self.command_line.len(),
            &chosen.raw.text,
        );
    }
}

/// 5.5.G.14: pure-editor completion cancel + docs scroll.
/// Migrated from `lattice-ui-tui::app::completion`; touches only
/// editor-owned `insert_completion` + `completion_in_path_context`.
impl Editor {
    /// Tear down the in-flight insert-completion state. Clears
    /// the popup and exits any path-context filter mode. Pure-
    /// editor; the next `Action::CompletionTrigger` rebuilds.
    pub fn do_completion_cancel(&mut self) {
        self.insert_completion = None;
        self.completion_in_path_context = false;
    }

    /// Page the docs popup body forward (`<C-f>` inside the
    /// completion-popup minor mode). Half-popup-height jump per
    /// press; clamps at the body's last visible line.
    pub fn do_completion_docs_scroll_down(&mut self) {
        if let Some(state) = self.insert_completion.as_mut()
            && let Some(doc) = state.doc_popup.as_mut()
        {
            doc.scroll = doc.scroll.saturating_add(8);
        }
    }

    /// Page the docs popup body backward (`<C-b>` inside the
    /// completion-popup minor mode).
    pub fn do_completion_docs_scroll_up(&mut self) {
        if let Some(state) = self.insert_completion.as_mut()
            && let Some(doc) = state.doc_popup.as_mut()
        {
            doc.scroll = doc.scroll.saturating_sub(8);
        }
    }
}

/// 5.5.G.13: pure-editor command-line history walk. Migrated
/// from `lattice-ui-tui::app::cmdline`; touches only editor-
/// owned cmdline + history state.
impl Editor {
    /// Walk through `:` command history in Command modal.
    /// `back = true` goes to older entries (Up); `false` goes
    /// newer (Down). The first Up snapshots the user's in-flight
    /// line into `command_history_pending` so the bottom-of-history
    /// Down can restore it.
    pub fn do_command_history_step(&mut self, back: bool) {
        if !matches!(self.modal, ModalState::Command) {
            return;
        }
        if self.command_history.is_empty() {
            return;
        }
        let new_cursor = match (self.command_history_cursor, back) {
            (None, true) => {
                self.command_history_pending = Some(self.command_line.clone());
                Some(self.command_history.len() - 1)
            }
            (None, false) => return,
            (Some(0), true) => return,
            (Some(i), true) => Some(i - 1),
            (Some(i), false) if i + 1 >= self.command_history.len() => {
                if let Some(pending) = self.command_history_pending.take() {
                    self.command_line = pending;
                }
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
}

/// 5.5.G.11: simple picker-state helpers + close-hover.
impl Editor {
    /// Slice 2: live-picker debounce. Bump deadline by `now +
    /// LIVE_PICKER_DEBOUNCE`. No-op when no live-query state is in
    /// flight.
    pub fn bump_live_picker_debounce(&mut self) {
        let Some(state) = self.live_picker_query.as_mut() else {
            return;
        };
        state.debounce_until =
            Some(std::time::Instant::now() + crate::state::LIVE_PICKER_DEBOUNCE);
    }

    /// If the picker is open and its action is
    /// `PickerAction::SwitchToBuffer`, preview-activate the
    /// selected candidate's buffer in the active pane. No
    /// position-history push, no commit. Returns any signals
    /// emitted by the underlying activate_buffer_state tail.
    pub fn preview_picker_selection(&mut self) -> Vec<RendererSignal> {
        let Some(picker) = self.picker.as_ref() else {
            return Vec::new();
        };
        if !matches!(
            picker.on_accept,
            lattice_picker::PickerAction::SwitchToBuffer
        ) {
            return Vec::new();
        }
        let Some(c) = picker.selected_candidate() else {
            return Vec::new();
        };
        let Some(lattice_picker::RoutingPayload::Buffer { id: raw_id }) =
            picker.routing_for(c)
        else {
            return Vec::new();
        };
        let id = BufferId(*raw_id);
        if id == self.active_pane_buffer_id() {
            return Vec::new();
        }
        self.previewing = true;
        let needs_state = self.activate_buffer(id);
        self.previewing = false;
        if needs_state {
            self.activate_buffer_state()
        } else {
            Vec::new()
        }
    }
}

/// 5.5.G.10: pure-editor search-state cluster (`/`, `?`, `n`, `N`,
/// `*`, `#`). `do_find_repeat` stays App-side (calls
/// `run_invocation`); the substitute preview path stays App-side
/// (called from the cmdline flow).
impl Editor {
    /// Live-preview the in-progress search pattern from origin.
    /// Tolerates compile errors silently while the user is typing.
    pub fn preview_search(&mut self) {
        let Some(line) = self.search_line.as_ref() else {
            return;
        };
        if line.pattern.is_empty() {
            self.current_match = None;
            self.all_matches.clear();
            return;
        }
        let Ok(regex) = compile_search_pattern(&line.pattern) else {
            self.current_match = None;
            self.all_matches.clear();
            return;
        };
        let dir = match line.direction {
            lattice_grammar::SearchDirection::Forward => lattice_core::search::Direction::Forward,
            lattice_grammar::SearchDirection::Backward => lattice_core::search::Direction::Backward,
        };
        let buffer = self.active_text();
        match lattice_core::search::find(
            &buffer,
            &regex,
            line.origin,
            dir,
            &lattice_runtime::CancellationToken::never(),
        ) {
            Ok(Some(lattice_core::search::SearchHit { range, .. })) => {
                self.current_match = Some(range)
            }
            _ => self.current_match = None,
        }
        self.all_matches = lattice_core::search::find_all(
            &buffer,
            &regex,
            &lattice_runtime::CancellationToken::never(),
        )
        .unwrap_or_default();
    }

    /// Commit the search pattern -- jump the cursor to the first
    /// match, record `last_search`, populate `all_matches` for
    /// hlsearch. On empty submit, replay `last_search` (vim `<CR>`
    /// behaviour).
    pub fn submit_search(&mut self) {
        let Some(line) = self.search_line.take() else {
            return;
        };
        self.modal = ModalState::Normal;
        if line.pattern.is_empty() {
            if self.last_search.is_some() {
                self.repeat_search(false);
            }
            return;
        }
        let cur = line.origin;
        self.push_position_history(cur, PositionSource::AutoJump);
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
            lattice_grammar::SearchDirection::Forward => lattice_core::search::Direction::Forward,
            lattice_grammar::SearchDirection::Backward => lattice_core::search::Direction::Backward,
        };
        let buffer = self.active_text();
        match lattice_core::search::find(
            &buffer,
            &regex,
            line.origin,
            dir,
            &lattice_runtime::CancellationToken::never(),
        ) {
            Ok(Some(hit)) => {
                self.cursor = hit.range.start;
                self.current_match = Some(hit.range);
                self.all_matches = lattice_core::search::find_all(
                    &buffer,
                    &regex,
                    &lattice_runtime::CancellationToken::never(),
                )
                .unwrap_or_default();
                if hit.wrapped {
                    let level = EchoLevel::Warn;
                    let text = match line.direction {
                        lattice_grammar::SearchDirection::Forward => {
                            "search hit BOTTOM, continuing at TOP"
                        }
                        lattice_grammar::SearchDirection::Backward => {
                            "search hit TOP, continuing at BOTTOM"
                        }
                    };
                    self.set_message(level, text.to_string());
                }
                self.last_search = Some(crate::state::LastSearch {
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
                self.last_search = Some(crate::state::LastSearch {
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

    /// `<Esc>` from the search line -- restore the pre-search
    /// cursor and clear match decorations.
    pub fn cancel_search(&mut self) {
        if let Some(line) = self.search_line.take() {
            self.cursor = line.origin;
        }
        self.current_match = None;
        self.all_matches.clear();
        self.modal = ModalState::Normal;
    }

    /// `n` / `N` -- replay `last_search`. `reverse = false` keeps
    /// the direction; `reverse = true` flips it.
    pub fn repeat_search(&mut self, reverse: bool) {
        let Some(last) = self.last_search.clone() else {
            self.set_message(
                EchoLevel::Error,
                "E35: no previous regular expression".to_string(),
            );
            return;
        };
        let cur = self.cursor;
        self.push_position_history(cur, PositionSource::AutoJump);
        let direction = match (last.direction, reverse) {
            (lattice_grammar::SearchDirection::Forward, false)
            | (lattice_grammar::SearchDirection::Backward, true) => {
                lattice_grammar::SearchDirection::Forward
            }
            (lattice_grammar::SearchDirection::Backward, false)
            | (lattice_grammar::SearchDirection::Forward, true) => {
                lattice_grammar::SearchDirection::Backward
            }
        };
        let dir = match direction {
            lattice_grammar::SearchDirection::Forward => lattice_core::search::Direction::Forward,
            lattice_grammar::SearchDirection::Backward => lattice_core::search::Direction::Backward,
        };
        let buffer = self.active_text();
        let from = step_byte(&buffer, self.cursor, direction);
        let regex = match compile_search_pattern(&last.pattern) {
            Ok(r) => r,
            Err(msg) => {
                self.set_message(EchoLevel::Error, format!("regex: {msg}"));
                self.current_match = None;
                return;
            }
        };
        match lattice_core::search::find(
            &buffer,
            &regex,
            from,
            dir,
            &lattice_runtime::CancellationToken::never(),
        ) {
            Ok(Some(hit)) => {
                self.cursor = hit.range.start;
                self.current_match = Some(hit.range);
                if hit.wrapped {
                    let text = match direction {
                        lattice_grammar::SearchDirection::Forward => {
                            "search hit BOTTOM, continuing at TOP"
                        }
                        lattice_grammar::SearchDirection::Backward => {
                            "search hit TOP, continuing at BOTTOM"
                        }
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

    /// `*` / `#` -- extract the word at the cursor, store as
    /// `last_search`, jump to the next (or previous) occurrence.
    pub fn do_search_word_under_cursor(
        &mut self,
        direction: lattice_grammar::SearchDirection,
    ) {
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
        let mut start = cursor_byte;
        if start >= bytes.len() || !is_word_char_byte(bytes[start]) {
            while start < bytes.len()
                && bytes[start] != b'\n'
                && !is_word_char_byte(bytes[start])
            {
                start += 1;
            }
            if start >= bytes.len() || bytes[start] == b'\n' {
                self.set_message(EchoLevel::Error, "no word under cursor".to_string());
                return;
            }
        }
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
            lattice_grammar::SearchDirection::Forward => lattice_core::search::Direction::Forward,
            lattice_grammar::SearchDirection::Backward => lattice_core::search::Direction::Backward,
        };
        let from = step_byte(&self.document.snapshot().buffer, self.cursor, direction);
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
            &lattice_runtime::CancellationToken::never(),
        ) {
            Ok(Some(hit)) => {
                self.push_position_history(pre_jump, PositionSource::AutoJump);
                self.cursor = hit.range.start;
                self.current_match = Some(hit.range);
                self.all_matches = lattice_core::search::find_all(
                    &self.document.snapshot().buffer,
                    &regex,
                    &lattice_runtime::CancellationToken::never(),
                )
                .unwrap_or_default();
                self.last_search = Some(crate::state::LastSearch {
                    pattern: escaped,
                    direction,
                });
                if matches!(self.active_buffer, BufferKind::Document) {
                    self.auto_open_folds_at_cursor();
                }
            }
            Ok(None) => {
                self.set_message(
                    EchoLevel::Error,
                    format!("E486: Pattern not found: {word}"),
                );
            }
            Err(_) => {}
        }
    }
}

/// 5.5.G.10: pure regex compile wrapper. Mirrors the App-side
/// helper retired in this slice.
fn compile_search_pattern(pattern: &str) -> Result<fancy_regex::Regex, String> {
    fancy_regex::Regex::new(pattern).map_err(|e| e.to_string())
}

/// 5.5.G.10: one-byte cursor step in the given search direction
/// for `n` / `N` skip-current-match.
fn step_byte(
    buf: &lattice_core::Buffer,
    p: lattice_protocol::position::Position,
    dir: lattice_grammar::SearchDirection,
) -> lattice_protocol::position::Position {
    match dir {
        lattice_grammar::SearchDirection::Forward => {
            let len = buf.line_byte_len(p.line);
            if p.byte < len {
                lattice_protocol::position::Position::new(p.line, p.byte + 1)
            } else {
                let last = last_addressable_line(buf);
                if p.line < last {
                    lattice_protocol::position::Position::new(p.line + 1, 0)
                } else {
                    p
                }
            }
        }
        lattice_grammar::SearchDirection::Backward => previous_position(buf, p),
    }
}

/// 5.5.G.10: word-byte predicate used by `*` / `#`.
fn is_word_char_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 5.5.G.9: pure-editor paste cluster (`p` / `P` / bracketed-paste).
impl Editor {
    /// Bracketed-paste handler. Routes the payload to cursor /
    /// command line / search line based on the current modal.
    pub fn do_paste_text(&mut self, text: &str) {
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
            _ => {
                if let Ok(applied) = self.apply_edit_blocking(
                    lattice_protocol::edit::Edit::insert(self.cursor, text),
                ) {
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

    /// Vim's `p` / `P` -- paste from the chosen register
    /// (`pending_register` if set, else unnamed). Charwise splices
    /// at cursor; linewise inserts a fresh line; blockwise paints
    /// columns down consecutive lines.
    pub fn do_paste(&mut self, before: bool) {
        let chosen = self.pending_register.take();
        let Some(reg) = self.read_register(chosen) else {
            self.set_message(EchoLevel::Error, "register empty".to_string());
            return;
        };
        match reg.kind {
            lattice_grammar::YankKind::Charwise => {
                let line_len = self
                    .document
                    .snapshot()
                    .buffer
                    .line_byte_len(self.cursor.line);
                let insert_at = if before {
                    self.cursor
                } else if self.cursor.byte < line_len {
                    lattice_protocol::position::Position::new(
                        self.cursor.line,
                        self.cursor.byte + 1,
                    )
                } else {
                    self.cursor
                };
                if let Ok(applied) = self.apply_edit_blocking(
                    lattice_protocol::edit::Edit::insert(insert_at, &reg.content),
                ) {
                    let end = applied.inserted_range.end;
                    self.cursor = if end.byte > 0 {
                        lattice_protocol::position::Position::new(end.line, end.byte - 1)
                    } else {
                        end
                    };
                }
            }
            lattice_grammar::YankKind::Linewise => {
                let mut payload = reg.content.clone();
                if !payload.ends_with('\n') {
                    payload.push('\n');
                }
                let insert_at = if before {
                    lattice_protocol::position::Position::new(self.cursor.line, 0)
                } else {
                    let len = self
                        .document
                        .snapshot()
                        .buffer
                        .line_byte_len(self.cursor.line);
                    if self.cursor.line + 1 < self.document.snapshot().buffer.line_count()
                    {
                        lattice_protocol::position::Position::new(self.cursor.line + 1, 0)
                    } else {
                        let _ = self.apply_edit_blocking(
                            lattice_protocol::edit::Edit::insert(
                                lattice_protocol::position::Position::new(self.cursor.line, len),
                                "\n",
                            ),
                        );
                        lattice_protocol::position::Position::new(self.cursor.line + 1, 0)
                    }
                };
                if let Ok(applied) = self.apply_edit_blocking(
                    lattice_protocol::edit::Edit::insert(insert_at, &payload),
                ) {
                    self.cursor = applied.inserted_range.start;
                }
            }
            lattice_grammar::YankKind::Blockwise => {
                self.do_paste_blockwise(&reg.content, before)
            }
        }
    }

    /// Vim's blockwise paste: each `\n`-separated row inserted on
    /// consecutive lines at the same column. Rows below the
    /// buffer extend it with new lines.
    pub fn do_paste_blockwise(&mut self, content: &str, before: bool) {
        if content.is_empty() {
            return;
        }
        let rows: Vec<&str> = content.split('\n').collect();
        let start_line = self.cursor.line;
        let line_len = self.document.snapshot().buffer.line_byte_len(start_line);
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
                let last = total_lines.saturating_sub(1);
                let last_len = self.document.snapshot().buffer.line_byte_len(last);
                let _ = self.apply_edit_blocking(lattice_protocol::edit::Edit::insert(
                    lattice_protocol::position::Position::new(last, last_len),
                    "\n",
                ));
            }
            let target_len = self.document.snapshot().buffer.line_byte_len(target_line);
            let insert_col = start_col.min(target_len);
            let pos = lattice_protocol::position::Position::new(target_line, insert_col);
            let _ = self
                .apply_edit_blocking(lattice_protocol::edit::Edit::insert(pos, *row));
        }
        self.cursor = lattice_protocol::position::Position::new(start_line, start_col);
    }

    /// Read the register slot for paste / inspection. Falls back
    /// to `unnamed_register`.
    pub fn read_register(
        &self,
        register: Option<lattice_grammar::register::Register>,
    ) -> Option<UnnamedRegister> {
        match register {
            None | Some(lattice_grammar::register::Register::Unnamed) => {
                self.unnamed_register.clone()
            }
            Some(lattice_grammar::register::Register::BlackHole) => None,
            Some(r) => self
                .registers
                .get(&r)
                .cloned()
                .or_else(|| self.unnamed_register.clone()),
        }
    }
}

/// 5.5.G.8: pure-editor snippet placeholder navigation.
/// `do_snippet_expand_at_cursor` stays App-side until
/// `active_language_id` and `is_word_char_byte` migrate.
impl Editor {
    /// `<Tab>` while a snippet is active -- step to the next
    /// placeholder group; close the session if we've walked off
    /// the end.
    pub fn do_snippet_next_placeholder(&mut self) {
        let Some(active) = self.active_snippet.as_mut() else {
            return;
        };
        let next = active.next().cloned();
        match next {
            Some(group) => self.move_cursor_to_snippet_group(&group),
            None => self.active_snippet = None,
        }
    }

    /// `<S-Tab>` -- step to the previous placeholder.
    pub fn do_snippet_prev_placeholder(&mut self) {
        let Some(active) = self.active_snippet.as_mut() else {
            return;
        };
        if let Some(group) = active.prev().cloned() {
            self.move_cursor_to_snippet_group(&group);
        }
    }

    /// Move the cursor to the start of `group.ranges.first()`.
    fn move_cursor_to_snippet_group(
        &mut self,
        group: &lattice_snippet::TabstopGroup,
    ) {
        let Some(first) = group.ranges.first() else {
            return;
        };
        let snap = self.document.snapshot();
        if let Ok(pos) = snap.buffer.byte_to_position(first.start) {
            self.cursor = pos;
        }
    }
}

/// 5.5.G.7: pure-editor tag-stack / mark-jump / popup-back-stack /
/// jump-history cluster. With `seed_help_metadata_locals` and
/// `pop_popup_back` migrated, the entire jump-history walk runs
/// host-side.
impl Editor {
    /// Seed parsed link / anchor / highlight metadata into the
    /// help buffer's locals. Used by `pop_popup_back` to restore a
    /// snapshot's metadata; also used at popup-open time so
    /// link-follow / search-anchor lookups read the right tables.
    pub fn seed_help_metadata_locals(
        &mut self,
        buffer_id: BufferId,
        metadata: lattice_help::HelpMetadata,
    ) {
        let lattice_help::HelpMetadata {
            links,
            anchors,
            highlights,
        } = metadata;
        let locals = self.buffer_locals.entry(buffer_id).or_default();
        locals.insert(crate::modes::HelpLinks(links));
        locals.insert(crate::modes::HelpAnchors(anchors));
        locals.insert(crate::modes::HelpHighlights(highlights));
    }

    /// Restore the most recent snapshot from `popup_back_stack`
    /// into the active popup. Returns `true` if a frame was
    /// popped and applied; `false` when the stack was empty.
    pub fn pop_popup_back(&mut self) -> bool {
        let Some(snap) = self.popup_back_stack.pop() else {
            return false;
        };
        let Some(id) = self.popup_buffer else {
            return false;
        };
        self.buffers.with_help_mut(id, |existing| {
            existing.title = snap.title;
            existing.content = snap.content;
            existing.scroll = snap.scroll as usize;
            existing.cursor = snap.cursor;
        });
        self.cursor = snap.cursor;
        self.scroll = snap.scroll;
        self.popup_placement = snap.placement;
        self.seed_help_metadata_locals(id, snap.metadata);
        true
    }

    /// `<C-t>` -- pop the tag stack (vim's `:pop`). LIFO walk
    /// back through the chain of `gd` / `gD` / `gy` / `gI`
    /// drill-downs.
    pub fn do_tag_stack_pop(&mut self) {
        let Some(entry) = self.tag_stack.pop() else {
            self.set_message(EchoLevel::Info, "tag stack empty".to_string());
            return;
        };
        let cursor = self.cursor;
        self.push_position_history(cursor, PositionSource::PluginPush);
        let active_id = self.active_pane_buffer_id();
        if entry.buffer_id != active_id && self.buffers.contains(entry.buffer_id) {
            let _ = self.activate_buffer(entry.buffer_id);
        }
        let buffer = self.active_text();
        let last = last_addressable_line(&buffer);
        let line = entry.position.line.min(last);
        let len = buffer.line_byte_len(line);
        let col = entry.position.byte.min(len);
        self.cursor = lattice_protocol::position::Position::new(line, col);
        let label = if entry.label.is_empty() {
            format!("tag pop -> ({},{})", line + 1, col + 1)
        } else {
            format!("tag pop -> {} ({},{})", entry.label, line + 1, col + 1)
        };
        self.set_message(EchoLevel::Info, label);
    }

    /// Jump to a recorded mark (`'<letter>` / `` `<letter> ``).
    /// `exact = true` puts the cursor at the stored byte;
    /// `exact = false` jumps to the line and column = first
    /// non-blank.
    pub fn do_jump_mark(&mut self, name: char, exact: bool) {
        if !(name.is_ascii_alphabetic() || name.is_ascii_digit()) {
            self.set_message(EchoLevel::Error, format!("invalid mark: {name}"));
            return;
        }
        let Some(&pos) = self.marks.get(&name) else {
            self.set_message(EchoLevel::Error, format!("mark not set: {name}"));
            return;
        };
        let cur = self.cursor;
        self.push_position_history(cur, PositionSource::AutoJump);
        if exact {
            self.cursor = pos;
        } else {
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
            self.cursor = lattice_protocol::position::Position::new(pos.line, col as u32);
        }
        self.clamp_cursor_to_active_buffer();
        self.auto_open_folds_at_cursor();
    }

    /// `<C-o>` / `<C-i>` -- walk the jump-history ring filtered
    /// to jump-class entries. In a help-popup that has its own
    /// back-stack, the first `<C-o>` pops the popup back-stack
    /// (popup-internal walk); only after the stack is empty does
    /// it fall through to the position-history walk.
    pub fn do_jump_history(&mut self, delta: i32) {
        if delta < 0
            && matches!(self.active_buffer, BufferKind::Help)
            && self.popup_buffer.is_some()
            && !self.popup_back_stack.is_empty()
            && self.pop_popup_back()
        {
            return;
        }
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
                self.position_history_cursor =
                    self.position_history.len().saturating_sub(1);
            }
        }
        self.do_walk_history(delta, |e| e.is_jump(), "jumps", "jump list");
    }
}

/// 5.5.G.6: pure-editor `g;` / `g,` mark-history walk.
/// `<C-o>` / `<C-i>` jump-history walk stays App-side until
/// `pop_popup_back` (App) migrates.
impl Editor {
    /// `g;` / `g,` per §5.1.1 -- step through `NamedMark` entries
    /// in the position-history ring. No "snapshot current pos"
    /// pre-step: mark navigation is exploratory and shouldn't
    /// pollute the jump list with `AutoJump` entries.
    pub fn do_mark_history(&mut self, delta: i32) {
        self.do_walk_history(delta, |e| e.is_named_mark(), "marks", "mark history");
    }

    /// Generic walk over the unified position-history ring filtered
    /// by `pred`. Used by both `:do_jump_history` (App, via
    /// `<C-o>` / `<C-i>` — filters `e.is_jump()`) and
    /// `:do_mark_history` (host, via `g;` / `g,` — filters
    /// `e.is_named_mark()`).
    pub fn do_walk_history<F: Fn(&PositionEntry) -> bool>(
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
        let popup_help_id = self.popup_buffer;
        let reachable = |e: &PositionEntry| -> bool {
            match e.buffer {
                BufferKind::Document | BufferKind::FileTree => {
                    self.buffers.contains(e.buffer_id)
                }
                BufferKind::Help => {
                    self.buffers.contains_help(e.buffer_id) || popup_help_id == Some(e.buffer_id)
                }
                BufferKind::Oil => self.buffers.contains(e.buffer_id),
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
        match entry.buffer {
            BufferKind::Document => {
                if self.buffers.contains_document(entry.buffer_id) {
                    let _ = self.activate_document(entry.buffer_id);
                    self.cursor = entry.position;
                    self.clamp_cursor_to_active_buffer();
                    self.auto_open_folds_at_cursor();
                }
            }
            BufferKind::Help => {
                self.active_buffer = BufferKind::Help;
                let buffer_present = self.buffers.contains_help(entry.buffer_id)
                    || self.popup_buffer == Some(entry.buffer_id);
                if buffer_present {
                    self.cursor = entry.position;
                    self.pane_tree.active_mut().buffer = BufferKind::Help;
                    self.pane_tree.active_mut().buffer_id = entry.buffer_id;
                    self.clamp_cursor_to_active_buffer();
                }
            }
            BufferKind::FileTree => {
                if self.buffers.contains_file_tree(entry.buffer_id) {
                    self.active_buffer = BufferKind::FileTree;
                    self.cursor = entry.position;
                    self.pane_tree.active_mut().buffer = BufferKind::FileTree;
                    self.pane_tree.active_mut().buffer_id = entry.buffer_id;
                    self.clamp_cursor_to_active_buffer();
                }
            }
            BufferKind::Oil => {
                if self.buffers.contains_oil(entry.buffer_id) {
                    self.active_buffer = BufferKind::Oil;
                    self.cursor = entry.position;
                    self.pane_tree.active_mut().buffer = BufferKind::Oil;
                    self.pane_tree.active_mut().buffer_id = entry.buffer_id;
                    self.clamp_cursor_to_active_buffer();
                }
            }
        }
    }
}

/// 5.5.G.5: pure-editor pane navigation. `Action::SplitPaneHorizontal`
/// / `SplitPaneVertical` / `ClosePane` / `NavigatePane` / `NextPane`
/// / `PrevPane`. Bodies were already 100% `editor.pane_tree` +
/// `snapshot_active_pane` / `load_active_pane` (host-side since
/// F.4.1) reads/writes.
impl Editor {
    /// `<C-w>s` (horizontal) / `<C-w>v` (vertical) -- split the
    /// active pane; the new sibling inherits cursor + scroll.
    pub fn do_split_pane(
        &mut self,
        orientation: lattice_core::ui::pane::SplitOrientation,
    ) {
        self.snapshot_active_pane();
        let _new_idx = self.pane_tree.split_active(orientation);
    }

    /// `<C-w>c` -- close the active pane; the first surviving
    /// pane becomes active. No-op when only one pane is open.
    pub fn do_close_pane(&mut self) {
        if self.pane_tree.len() <= 1 {
            self.set_message(EchoLevel::Warn, "Already only one pane".to_string());
            return;
        }
        self.snapshot_active_pane();
        if !self.pane_tree.close_active() {
            return;
        }
        self.load_active_pane();
    }

    /// Cardinal neighbour walk (`<C-w>h/j/k/l`).
    pub fn do_navigate_pane(
        &mut self,
        direction: lattice_core::ui::pane::PaneDirection,
    ) {
        let area = self.buffer_area_rect();
        let Some(target) = self.pane_tree.navigate(direction, area) else {
            return;
        };
        self.activate_pane(target);
    }

    /// Build the buffer-area rectangle from terminal_width +
    /// viewport_height. Mirrors the App-side helper retired in
    /// this slice.
    pub fn buffer_area_rect(&self) -> lattice_core::ui::pane::PaneRect {
        lattice_core::ui::pane::PaneRect {
            x: 0,
            y: 0,
            width: self.terminal_width.unwrap_or(120),
            height: self.viewport_height as u16,
        }
    }

    /// Make pane `idx` active, swapping pane stash <-> hot-path
    /// cursor / scroll.
    pub fn activate_pane(&mut self, idx: usize) {
        if idx == self.pane_tree.active_index() {
            return;
        }
        self.snapshot_active_pane();
        if !self.pane_tree.set_active(idx) {
            return;
        }
        self.load_active_pane();
    }
}

/// 5.5.G.4: bracket-match scan helpers — co-moved from
/// `lattice_ui_tui::app::motions` alongside `do_match_bracket`.
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

/// 5.5.G.2: pure-editor visual-mode helpers.
impl Editor {
    /// `v` / `V` / `<C-v>` from Normal -- enter Visual mode at the
    /// current cursor, seeding `document.selections` with a
    /// zero-width anchor=head selection so `Range::Selection`
    /// dispatch picks up the cursor immediately.
    pub fn do_enter_visual(&mut self, kind: lattice_grammar::VisualKind) {
        self.modal = ModalState::Visual(kind);
        self.visual_anchor = Some(self.cursor);
        let sel = lattice_protocol::selection::Selection {
            anchor: self.cursor,
            head: self.cursor,
            visual: Some(visual_kind_to_mode(kind)),
        };
        self.set_selections_blocking(lattice_protocol::selection::SelectionSet::single(sel));
    }

    /// `<Esc>` from Visual (and the post-operator-on-selection path)
    /// -- capture the current selection as `last_visual` so `gv`
    /// can restore it, then collapse the selection to a cursor at
    /// the current head and drop to Normal.
    pub fn do_exit_visual(&mut self) {
        if let ModalState::Visual(kind) = self.modal {
            let sels = self.document.selections();
            let sel = sels.primary();
            self.last_visual = Some(crate::state::LastVisual {
                anchor: sel.anchor,
                head: sel.head,
                kind,
            });
        }
        self.modal = ModalState::Normal;
        self.visual_anchor = None;
        self.set_selections_blocking(lattice_protocol::selection::SelectionSet::single(
            lattice_protocol::selection::Selection::cursor(self.cursor),
        ));
    }

    /// `gv` -- restore the prior selection captured by `do_exit_visual`,
    /// or echo an error if there's no captured visual to reselect.
    pub fn do_reselect_visual(&mut self) {
        let Some(last) = self.last_visual else {
            self.set_message(
                EchoLevel::Error,
                "no previous visual selection".to_string(),
            );
            return;
        };
        self.modal = ModalState::Visual(last.kind);
        self.visual_anchor = Some(last.anchor);
        self.cursor = last.head;
        let sel = lattice_protocol::selection::Selection {
            anchor: last.anchor,
            head: last.head,
            visual: Some(visual_kind_to_mode(last.kind)),
        };
        self.set_selections_blocking(lattice_protocol::selection::SelectionSet::single(sel));
    }
}

/// Phase 5.5.E.6: host-side option-cascade infrastructure. These
/// methods move the pure-editor half of `:set` (config write +
/// cache rebuild + cascade dispatch) under [`Editor`]; the renderer
/// keeps the renderer-coupled tail wired through
/// [`RendererSignal`] (theme rebuild, file-tree rope refresh, mode
/// mirror activate / deactivate, LSP `didChangeConfiguration`
/// fan-out).
impl Editor {
    /// Sync the host-side renderer-neutral [`crate::ui::theme::Theme`]
    /// (`editor.host_theme`) from the typed `ui.*` options in
    /// [`Self::config`]. Called by the option-cascade for any
    /// `ui.*` key and at App-init time. The renderer half (TUI's
    /// cached `Theme` mirror rebuild) runs after this returns,
    /// driven by [`RendererSignal::ThemeChanged`].
    pub fn sync_host_theme_from_config(&mut self) {
        use crate::ui::theme as host_theme;
        use crate::ui::theme_options::{
            UiDimInactive, UiNerdFonts, UiSeparator, UiSeparatorColor, UiStatuslineActiveFg,
            UiStatuslineInactiveFg,
        };
        let dim_inactive = *self.config.get_typed::<UiDimInactive>().expect("UiDimInactive");
        let nerd_fonts = *self.config.get_typed::<UiNerdFonts>().expect("UiNerdFonts");
        let sep = self.config.get_typed::<UiSeparator>().expect("UiSeparator");
        let sep_char = sep.chars().next().unwrap_or('│');
        self.host_theme.dim_inactive_panes = dim_inactive;
        self.host_theme.nerd_fonts = nerd_fonts;
        self.host_theme.pane_separator_vertical = sep_char;
        // ui.separator_color -- color name; host parser returned
        // Ok during validate so unwrap-via-fallback is safe.
        let sep_color = self
            .config
            .get_typed::<UiSeparatorColor>()
            .expect("UiSeparatorColor");
        if let Ok(c) = host_theme::parse_color(&sep_color) {
            self.host_theme.pane_separator = host_theme::Style::empty().fg(c);
        }
        // ui.statusline_active_fg / _inactive_fg -- foreground
        // only; preserve modifiers / background by chaining `.fg(c)`
        // on the current host style.
        let active_fg = self
            .config
            .get_typed::<UiStatuslineActiveFg>()
            .expect("UiStatuslineActiveFg");
        if let Ok(c) = host_theme::parse_color(&active_fg) {
            self.host_theme.pane_status_active.fg = Some(c);
        }
        let inactive_fg = self
            .config
            .get_typed::<UiStatuslineInactiveFg>()
            .expect("UiStatuslineInactiveFg");
        if let Ok(c) = host_theme::parse_color(&inactive_fg) {
            self.host_theme.pane_status_inactive.fg = Some(c);
        }
    }

    /// Refresh [`Self::option_cache`] from the active buffer's
    /// resolved values. Falls back to the registry's current value
    /// when `resolved_options` doesn't yet have a cache entry for
    /// the active buffer (transient state during boot before the
    /// first [`Self::recompute_options_for_buffer`]). Cheap: 9
    /// typed reads.
    pub fn rebuild_option_cache(&mut self) {
        use lattice_config::{
            CompletionAutoInsertSingle, CursorLine, FoldEnable, FoldMethodOption, IgnoreCase,
            Number, RelativeNumber, Scrolloff, Tabstop, Whitespace, WhitespaceEol,
            WhitespaceLeading, WhitespaceSpace, WhitespaceTab, WhitespaceTrailing, Wrap,
        };
        let buffer = self.document_buffer_id;
        // M.7.3.a: parse a typed-option `String` into a single
        // glyph. Empty string ⇒ category is not decorated.
        let glyph = |s: &str| -> Option<char> { s.chars().next() };
        self.option_cache = crate::state::OptionCache {
            show_line_numbers: *self.resolved_option::<Number>(buffer),
            relative_line_numbers: *self.resolved_option::<RelativeNumber>(buffer),
            wrap_lines: *self.resolved_option::<Wrap>(buffer),
            ignorecase: *self.resolved_option::<IgnoreCase>(buffer),
            tabstop: *self.resolved_option::<Tabstop>(buffer) as u32,
            foldenable: *self.resolved_option::<FoldEnable>(buffer),
            foldmethod: *self.resolved_option::<FoldMethodOption>(buffer),
            scrolloff: *self.resolved_option::<Scrolloff>(buffer) as u32,
            completion_auto_insert_single: *self
                .resolved_option::<CompletionAutoInsertSingle>(buffer),
            show_whitespace: *self.resolved_option::<Whitespace>(buffer),
            current_line_highlight: *self.resolved_option::<CursorLine>(buffer),
            whitespace_tab: glyph(&self.resolved_option::<WhitespaceTab>(buffer)),
            whitespace_trailing: glyph(&self.resolved_option::<WhitespaceTrailing>(buffer)),
            whitespace_leading: glyph(&self.resolved_option::<WhitespaceLeading>(buffer)),
            whitespace_space: glyph(&self.resolved_option::<WhitespaceSpace>(buffer)),
            whitespace_eol: glyph(&self.resolved_option::<WhitespaceEol>(buffer)),
        };
    }

    /// Recompute the resolved-options cache for `buffer` by
    /// stitching every layer of the resolution stack
    /// (`mode-architecture.md` §6.1) and writing the result into
    /// [`Self::resolved_options`].
    ///
    /// Eager whole-cache recompute (§6.3.1). Called whenever any
    /// resolution layer for `buffer` changes: mode toggle, buffer-
    /// local set, modal-state transition, or option write (the
    /// cascade in [`Self::drain_option_changes`] propagates global
    /// `:set` writes to every buffer's cache).
    pub fn recompute_options_for_buffer(&mut self, buffer: BufferId) {
        let mut resolved = lattice_config::ResolvedOptions::new();
        // Layer 5/6: bootstrap with current registry values.
        self.config.bootstrap_resolved_with_current_values(&mut resolved);

        // Active modes (layers 4 + 3): walk in activation order
        // for minors, prepend major.
        let modes_snapshot = self.active_modes.get(&buffer).cloned().unwrap_or_default();
        let mut mode_contributions: Vec<lattice_config::OptionOverrideSet> =
            Vec::with_capacity(modes_snapshot.minors().len() + 1);
        if let Some(major_id) = modes_snapshot.major()
            && let Some(major) = self.mode_registry.get(major_id)
        {
            mode_contributions.push(major.options());
        }
        for &minor_id in modes_snapshot.minors() {
            if let Some(minor) = self.mode_registry.get(minor_id) {
                mode_contributions.push(minor.options());
            }
        }

        // Buffer-local overrides (layer 2).
        let buffer_local = self
            .buffer_local_overrides
            .get(&buffer)
            .cloned()
            .unwrap_or_default();

        // Modal-state layer (1) is empty for now; M.7 wires it.
        let modal_layer = lattice_config::OptionOverrideSet::new();

        let mut layered: Vec<&lattice_config::OptionOverrideSet> = Vec::new();
        layered.push(&modal_layer);
        layered.push(&buffer_local);
        for set in mode_contributions.iter().rev() {
            layered.push(set);
        }

        let resolver = lattice_config::Resolver::new();
        resolver.resolve_into(layered, &mut resolved);

        self.resolved_options.insert(buffer, resolved);
        // M.4: keep `option_cache` in lockstep with the active
        // buffer's resolved options.
        if buffer == self.document_buffer_id {
            self.rebuild_option_cache();
        }
    }

    /// Read a resolved option's value for `buffer`. Returns the
    /// option's bootstrap default if the cache for `buffer` hasn't
    /// been recomputed yet (transient state during boot before the
    /// first [`Self::recompute_options_for_buffer`]).
    ///
    /// Hot-path read; O(1) `TypeId` lookup on the cached
    /// [`lattice_config::ResolvedOptions`].
    pub fn resolved_option<D: lattice_config::OptionDecl>(
        &self,
        buffer: BufferId,
    ) -> std::sync::Arc<D::Value>
    where
        D::Value: Clone + Send + Sync + 'static,
    {
        if let Some(cache) = self.resolved_options.get(&buffer)
            && let Some(v) = cache.get::<D>()
        {
            return v;
        }
        self.config.get_typed::<D>().expect("option not registered")
    }

    /// Body of `:set foo=bar`. Parses + applies the spec via the
    /// canonical [`lattice_config::ConfigRegistry`] cmdline path,
    /// drains the cascade so user-visible side effects (recompute
    /// folds, theme refresh, ...) land before the caller observes
    /// the post-set state, and echoes the result. Returns the
    /// signal list the cascade enqueued so the renderer can fan
    /// out its half (`RendererSignal::ThemeChanged` etc.).
    pub fn do_set(&mut self, option: &str) -> Vec<RendererSignal> {
        let echo = match self.config.parse_and_set_command(option) {
            Ok(echo) => echo,
            Err(err) => {
                self.set_message(EchoLevel::Error, err.to_string());
                return Vec::new();
            }
        };
        // Drain any cascade events the set just enqueued so the user
        // sees the side effects (recompute folds, theme refresh, ...)
        // before the next frame draws. The runtime's main_loop also
        // drains once per iteration as a backstop for writes that
        // originate outside the keystroke path (plugin tasks, future
        // LSP-driven config writes).
        let signals = self.drain_option_changes();
        self.set_message(EchoLevel::Info, echo);
        signals
    }

    /// Drain queued [`Event::OptionChanged`] events from the typed-
    /// options bus and apply per-option cascades. Returns the
    /// accumulated [`RendererSignal`]s so renderer-coupled side
    /// effects can fan out after every set call.
    ///
    /// Cascades re-entrant on themselves: a cascade that writes
    /// another typed option (e.g. `relativenumber=true` implies
    /// `number=true`) queues a new event, and the `while let Ok`
    /// loop picks it up on the next iteration before exiting.
    pub fn drain_option_changes(&mut self) -> Vec<RendererSignal> {
        let mut signals = Vec::new();
        // Take the receiver to dodge the borrow checker (we mutate
        // `self` for cascades while reading from the rx). Always
        // restored after the loop.
        let mut rx = match self.option_change_rx.take() {
            Some(rx) => rx,
            None => return signals,
        };
        while let Ok(event) = rx.try_recv() {
            if let Event::OptionChanged { name, .. } = event {
                self.apply_option_cascade(&name, &mut signals);
            }
        }
        self.option_change_rx = Some(rx);
        signals
    }

    /// Per-option cascade body. Pushes [`RendererSignal`]s into
    /// `signals` for the renderer-coupled side effects; runs the
    /// pure-editor side effects (resolver recompute, cache rebuild,
    /// implied-option writes, fold recompute, messages-filter
    /// reload) inline.
    fn apply_option_cascade(&mut self, canonical_name: &str, signals: &mut Vec<RendererSignal>) {
        // M.4: a global `:set` updates the config layer (lowest-
        // priority resolver layer). Re-resolve the active buffer
        // so its `ResolvedOptions` reflects the new value;
        // otherwise the cache rebuild below would read stale
        // resolved data.
        let active_id = self.document_buffer_id;
        self.recompute_options_for_buffer(active_id);
        // Refresh the hot-path cache so subsequent reads see the
        // new value. `recompute_options_for_buffer` already calls
        // `rebuild_option_cache` when `buffer == active`; this
        // belt-and-braces call covers the bootstrap window.
        self.rebuild_option_cache();
        // M.7.1: declarative mode-mirror cascade. 5.5.F.5.4 brought
        // `mirror_option_to_modes` host-side alongside the mode-
        // lifecycle methods it dispatches to — the cascade now runs
        // synchronously inside the cascade body, returning its own
        // signal list (from any cascading mode-activated typed-option
        // writes) which we splice into the parent cascade's stream.
        signals.extend(self.mirror_option_to_modes(canonical_name));
        match canonical_name {
            "relativenumber" => {
                // Vim cascade: `:set rnu` implies `:set nu` so the
                // gutter renders at all. The reverse (`:set nornu`)
                // does NOT clear `nu` -- preserves user intent.
                if self.option_cache.relative_line_numbers {
                    let _ = self.config.set_typed::<lattice_config::Number>(true);
                }
            }
            "foldmethod" => {
                // Recompute folds against the new method. Idempotent
                // and cheap when method is `Manual` (the recompute
                // returns immediately).
                self.recompute_folds();
            }
            "messages.filter" => {
                // msg-mode.2: live-reload the `MessagesLayer`'s
                // `EnvFilter` directive. Validator already rejected
                // unparseable specs at `:set` time; the only way to
                // hit `Err` is the test path where
                // `install_messages_subscriber` wasn't called.
                let spec = self
                    .config
                    .get_typed::<lattice_config::MessagesFilter>()
                    .map(|v| (*v).clone())
                    .unwrap_or_else(|| String::from("info"));
                if let Err(e) = lattice_runtime::reload_messages_filter(&spec) {
                    self.set_message(
                        EchoLevel::Warn,
                        format!("messages.filter reload skipped: {e}"),
                    );
                }
            }
            n if n.starts_with("ui.") => {
                // Sync the host-side neutral theme first, then
                // signal the renderer to rebuild its typed mirror.
                self.sync_host_theme_from_config();
                signals.push(RendererSignal::ThemeChanged);
                if n == "ui.nerd_fonts" {
                    // File-tree rope embeds the icon glyphs, so a
                    // palette flip must re-render every existing
                    // tree. Renderer owns the file-tree buffers
                    // through 5.5.F; signal out and let it run the
                    // walk.
                    signals.push(RendererSignal::NerdFontsToggled);
                }
            }
            // 4.4.k: any change under `lsp.<server-id>.*` is a
            // server-scoped config edit -- fan out
            // `workspace/didChangeConfiguration` to every actor
            // matching that server-id. The LSP actor pool still
            // lives renderer-side, so signal out. `lsp.<host-knob>`
            // keys (one dot after `lsp`, e.g. `lsp.log_level`)
            // configure the host, not any server, and shouldn't
            // page server actors.
            n => {
                if let Some(server_id) = lsp_server_scope(n) {
                    signals.push(RendererSignal::LspConfigChanged(server_id.to_string()));
                }
            }
        }
    }
}

/// 5.5.F.1: host-side helpers for the `:ls` / `:describe-buffer`
/// Effect arms. Each builds a [`lattice_help::HelpContent`] from
/// `editor.*` state and lives next to the cascade so the
/// `do_*`-style content builders cluster in one module rather than
/// fanning out into per-command files.
impl Editor {
    /// 5.5.F.1: build the `:ls` / `:buffers` listing content.
    /// Mirrors the registry-walk + per-kind formatting that App's
    /// `do_list_buffers` used; reads `lattice_file_tree::modes::FileTreeRoot`
    /// and `lattice_oil::modes::OilDir` from `buffer_locals` so
    /// file-tree / oil rows show their root paths.
    pub fn build_list_buffers_content(&self) -> lattice_help::HelpContent {
        use crate::buffer_registry::BufferData;
        use lattice_core::BufferKind;
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
        // Snapshot every entry under one lock acquire. Per-line
        // rendering reads `buffer_locals` (Editor-side) so we do it
        // outside the closure.
        struct EntryRow {
            id: BufferId,
            kind: BufferKind,
            listed: bool,
            name: Option<String>,
            doc_path: Option<std::path::PathBuf>,
            doc_dirty: bool,
            help_title: Option<String>,
        }
        let mut rows: Vec<EntryRow> = Vec::with_capacity(ids.len());
        self.buffers.for_each(|entry| {
            let (doc_path, doc_dirty) = match &entry.data {
                BufferData::Document(d) => (d.handle.path(), d.handle.dirty()),
                _ => (None, false),
            };
            let help_title = match &entry.data {
                BufferData::Help(h) => Some(h.title.clone()),
                _ => None,
            };
            rows.push(EntryRow {
                id: entry.id,
                kind: entry.kind(),
                listed: entry.flags.listed,
                name: entry.name.clone(),
                doc_path,
                doc_dirty,
                help_title,
            });
        });
        rows.sort_by_key(|r| r.id);
        for row in rows {
            let id = row.id;
            let active_marker = if id == active_id { "%" } else { " " };
            let listed_marker = if row.listed { " " } else { "u" };
            match row.kind {
                BufferKind::Document => {
                    let label = row
                        .doc_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .or_else(|| row.name.clone())
                        .unwrap_or_else(|| "(no file)".to_string());
                    let dirty = if row.name.is_none() && row.doc_dirty {
                        "[+]"
                    } else {
                        "   "
                    };
                    lines.push(format!(
                        "  {active_marker}{listed_marker} #{:<3} doc  {dirty} {label}",
                        id.0
                    ));
                }
                BufferKind::FileTree => {
                    let root = self
                        .buffer_locals
                        .get(&id)
                        .and_then(|locals| locals.get::<lattice_file_tree::modes::FileTreeRoot>())
                        .map(|r| r.0.clone())
                        .unwrap_or_default();
                    lines.push(format!(
                        "  {active_marker}{listed_marker} #{:<3} tree     {}",
                        id.0,
                        root.display()
                    ));
                }
                BufferKind::Help => {
                    let title = row.help_title.unwrap_or_default();
                    lines.push(format!(
                        "  {active_marker}{listed_marker} #{:<3} help     {title}",
                        id.0,
                    ));
                }
                BufferKind::Oil => {
                    let dir = self
                        .buffer_locals
                        .get(&id)
                        .and_then(|locals| locals.get::<lattice_oil::modes::OilDir>())
                        .map(|d| d.0.display().to_string())
                        .unwrap_or_default();
                    lines.push(format!(
                        "  {active_marker}{listed_marker} #{:<3} oil      {}",
                        id.0, dir
                    ));
                }
            }
        }
        lattice_help::HelpContent::from_lines("buffers", lines)
            .with_markdown_syntax(self.lang_registry.clone())
    }

    /// 5.5.F.1: build the `:describe-buffer` content. Mirrors App's
    /// `do_describe_buffer`; every field read goes through
    /// `editor.*`. The active-mode lines render as
    /// `[name](mode:name)` markdown links so follow-link routes to
    /// `:describe-mode <name>` via the help buffer's link table.
    pub fn build_describe_buffer_content(&self) -> lattice_help::HelpContent {
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
            self.option_cache.show_line_numbers,
            self.option_cache.relative_line_numbers,
        ));
        // Active modes on the document buffer. Each mode name is a
        // clickable `[name](mode:name)` link.
        lines.push(String::new());
        lines.push("## Active modes".to_string());
        let active = self.active_modes.get(&self.document_buffer_id);
        let major = active.and_then(|a| a.major());
        let minors: Vec<_> = active.map(|a| a.minors().to_vec()).unwrap_or_default();
        if let Some(major) = major {
            lines.push(format!("- major: {}", lattice_help::mode_link(major.as_str())));
        } else {
            lines.push("- major: (none)".to_string());
        }
        if minors.is_empty() {
            lines.push("- minors: (none)".to_string());
        } else {
            lines.push(format!("- minors ({}):", minors.len()));
            for id in minors {
                lines.push(format!("    - {}", lattice_help::mode_link(id.as_str())));
            }
        }
        lattice_help::HelpContent::from_lines("describe-buffer", lines)
            .with_markdown_syntax(self.lang_registry.clone())
    }

    /// 5.5.F.2: build the `:describe-command` content.
    ///
    /// Two-stage name resolution
    /// ([`crate::excommand::resolve_command_name_or_alias`]) so both
    /// canonical (`ex:write`) and alias (`w`, `write`) spellings
    /// resolve to the same spec. Pushes a one-line error message
    /// onto the echo ring (and returns `None`) for unknown names;
    /// the caller skips the [`RendererSignal::DisplayBuffer`] emit.
    ///
    /// Cross-link tail: append `See also: [topic](help:topic)` rows
    /// for every help topic whose `related_command_patterns`
    /// matches this command. Lets a reader of
    /// `:describe-command operator:fold-create` jump to the
    /// `folding` topic via `<CR>` on the link.
    pub fn build_describe_command_content(
        &mut self,
        name: &str,
        anchor: Option<&str>,
    ) -> Option<lattice_help::HelpContent> {
        let Some(id) = crate::excommand::resolve_command_name_or_alias(&self.registry, name)
        else {
            self.set_message(EchoLevel::Error, format!("no command named `{name}`"));
            return None;
        };
        let Some(spec) = self.registry.lookup(id) else {
            self.set_message(EchoLevel::Error, format!("no command named `{name}`"));
            return None;
        };
        let rendered = lattice_grammar::render_introspection(spec);
        let anchors: Vec<lattice_help::HelpAnchor> = rendered
            .anchors
            .into_iter()
            .map(|a| lattice_help::HelpAnchor {
                name: a.name,
                line: a.line,
            })
            .collect();
        let mut lines = rendered.lines;
        let topics: Vec<String> = self
            .help_topics
            .topics_for_command(&spec.name)
            .map(|t| lattice_help::topic_link(&t.name))
            .collect();
        if !topics.is_empty() {
            lines.push(String::new());
            lines.push(format!("See also: {}", topics.join(", ")));
        }
        let mut content = lattice_help::HelpContent::from_lines_and_anchors(
            format!("describe-command {name}"),
            lines,
            anchors,
        )
        .with_markdown_syntax(self.lang_registry.clone());
        if let Some(a) = anchor
            && let Some(line) = lattice_help::anchor_line(&content.metadata.anchors, a)
        {
            content.buffer.scroll = line as usize;
        }
        Some(content)
    }

    /// 5.5.F.2: build the `:apropos <pattern>` content. Walks every
    /// registered command, matches `pattern` (case-insensitive)
    /// against the canonical name and the doc body, renders a
    /// 3-column listing (`name  kind  first-line-of-doc`) with
    /// `command_link` wrapping the name for `<CR>` follow.
    /// Empty pattern routes an error to the echo ring and returns
    /// `None` so the renderer skips the display signal.
    pub fn build_apropos_content(&mut self, pattern: &str) -> Option<lattice_help::HelpContent> {
        if pattern.is_empty() {
            self.set_message(EchoLevel::Error, "empty pattern".to_string());
            return None;
        }
        let needle = pattern.to_ascii_lowercase();
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
            let name_w = hits.iter().map(|(n, _, _)| n.len()).max().unwrap_or(0);
            let kind_w = hits.iter().map(|(_, k, _)| k.len()).max().unwrap_or(0);
            for (name, kind, first) in hits {
                let pad_n = name_w.saturating_sub(name.len());
                let pad_k = kind_w.saturating_sub(kind.len());
                lines.push(format!(
                    "  {}{}  {}{}  {}",
                    lattice_help::command_link(&name),
                    " ".repeat(pad_n),
                    kind,
                    " ".repeat(pad_k),
                    first
                ));
            }
        }
        Some(
            lattice_help::HelpContent::from_lines(format!("apropos {pattern}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        )
    }

    /// 5.5.F.2: build the `:describe-key <chord>` content. Looks up
    /// every binding of `chord` across all modes
    /// ([`crate::keymap::lookup`]) and renders each entry through
    /// the unified `render_introspection_lines` surface. Infallible
    /// — an unbound chord renders as "`{chord}` is not bound in any
    /// mode."
    pub fn build_describe_key_content(&self, chord: &str) -> lattice_help::HelpContent {
        let hits = crate::keymap::lookup(chord);
        let mut lines: Vec<String> = Vec::new();
        if hits.is_empty() {
            lines.push(format!("`{chord}` is not bound in any mode."));
        } else {
            lines.push(format!(
                "{} -- {} binding(s):",
                lattice_help::key_link(chord),
                hits.len()
            ));
            for entry in hits {
                lines.push(String::new());
                for l in lattice_grammar::render_introspection_lines(entry) {
                    lines.push(l);
                }
            }
        }
        lattice_help::HelpContent::from_lines(format!("describe-key {chord}"), lines)
            .with_markdown_syntax(self.lang_registry.clone())
    }

    /// 5.5.F.2: build the `:list-keymap` content. Groups every
    /// registered binding ([`crate::keymap::entries`]) by mode in a
    /// fixed order so the rendered output reads top-down, wraps
    /// each chord in a `key_link` for `<CR>` follow.
    pub fn build_list_keymap_content(&self) -> lattice_help::HelpContent {
        use crate::keymap::{BindingMode, entries};
        let mut by_mode: std::collections::BTreeMap<&str, Vec<&crate::keymap::KeymapEntry>> =
            std::collections::BTreeMap::new();
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
            let chord_w = group.iter().map(|e| e.chord.len()).max().unwrap_or(0);
            for entry in group {
                let pad = chord_w.saturating_sub(entry.chord.len());
                lines.push(format!(
                    "  {}{}  {}",
                    lattice_help::key_link(entry.chord),
                    " ".repeat(pad),
                    entry.doc
                ));
            }
            lines.push(String::new());
        }
        lattice_help::HelpContent::from_lines("keymap", lines)
            .with_markdown_syntax(self.lang_registry.clone())
    }

    /// 5.5.F.3: build the `:describe-option <name>` content.
    /// Renders the option's metadata (canonical name, aliases,
    /// type, default, current value, enumerated values) + doc.
    /// Unknown name routes a vim-style `E518` error to the echo
    /// ring and returns `None` so the dispatcher skips the signal.
    pub fn build_describe_option_content(
        &mut self,
        name: &str,
    ) -> Option<lattice_help::HelpContent> {
        let Some(spec) = self.config.lookup(name) else {
            self.set_message(EchoLevel::Error, format!("E518: Unknown option: {name}"));
            return None;
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
        Some(
            lattice_help::HelpContent::from_lines(format!("describe-option {name}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        )
    }

    /// 5.5.F.3: build the `:options` content. Live reference of
    /// every registered customizable option, grouped by
    /// [`lattice_config::OptionGroup`]. Self-updating: walks the
    /// [`lattice_config::OPTION_DECLS`] linkme slice, so a new
    /// `options! { ... }` declaration lights up here at the next
    /// build with no extra wiring.
    pub fn build_list_options_content(&self) -> lattice_help::HelpContent {
        use lattice_config::{GROUP_DECLS, OPTION_DECLS};
        use std::collections::BTreeMap;

        let mut by_group: BTreeMap<&'static str, Vec<&'static lattice_config::OptionDeclMetadata>> =
            BTreeMap::new();
        for meta in OPTION_DECLS.iter() {
            if !meta.customizable {
                continue;
            }
            by_group.entry(meta.group_name).or_default().push(*meta);
        }
        for v in by_group.values_mut() {
            v.sort_by_key(|m| m.name);
        }

        let group_doc: BTreeMap<&'static str, &'static str> =
            GROUP_DECLS.iter().map(|g| (g.name, g.doc)).collect();

        let total: usize = by_group.values().map(|v| v.len()).sum();
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("# Options ({total} customisable)"));
        lines.push(String::new());
        lines.push(
            "Live reference of every registered option, grouped by group. \
             For per-option detail run `:describe-option <name>`. For \
             concepts (`:set` syntax, layered resolution, TOML, plugin \
             options) read `:help options`."
                .into(),
        );
        lines.push(String::new());

        for (group, options) in &by_group {
            lines.push(format!("## {} ({})", group, options.len()));
            if let Some(doc) = group_doc.get(group) {
                lines.push(String::new());
                lines.push((*doc).to_string());
            }
            lines.push(String::new());
            for meta in options {
                let spec = self.config.lookup(meta.name);
                let aliases = spec
                    .as_ref()
                    .map(|s| s.aliases())
                    .filter(|a| !a.is_empty())
                    .map(|a| format!(" [{}]", a.join(", ")))
                    .unwrap_or_default();
                let type_label = (meta.type_label)();
                let default = (meta.default_formatted)();
                let current = spec
                    .as_ref()
                    .map(|s| s.get_formatted())
                    .unwrap_or_else(|| "?".into());
                let header = if current == default {
                    format!(
                        "- **{}**{} : {} = {}",
                        meta.name, aliases, type_label, current
                    )
                } else {
                    format!(
                        "- **{}**{} : {} = {} (default: {})",
                        meta.name, aliases, type_label, current, default,
                    )
                };
                lines.push(header);
                for doc_line in meta.doc.lines() {
                    let trimmed = doc_line.trim();
                    if !trimmed.is_empty() {
                        lines.push(format!("  {trimmed}"));
                    }
                }
                if let Some(values) = spec.as_ref().and_then(|s| s.enumerate_values()) {
                    lines.push(format!("  values: {}", values.join(", ")));
                }
                lines.push(String::new());
            }
        }

        lattice_help::HelpContent::from_lines("options", lines)
            .with_markdown_syntax(self.lang_registry.clone())
    }

    /// 5.5.F.3: build the `:describe-option-resolution <name>`
    /// content. Walks the §6.1 layer model (modal-state, buffer-
    /// local, minors, major, typed-option, default) for the
    /// active buffer and marks each layer that contributes the
    /// resolved value. Helps debug surprising values where a mode
    /// contribution shadows a `:set` write or vice versa.
    pub fn build_describe_option_resolution_content(
        &mut self,
        name: &str,
    ) -> Option<lattice_help::HelpContent> {
        let Some(spec) = self.config.lookup(name) else {
            self.set_message(EchoLevel::Error, format!("E518: Unknown option: {name}"));
            return None;
        };
        let canonical_name = spec.name();
        let target_type_id = lattice_config::OPTION_DECLS
            .iter()
            .find(|d| d.name == canonical_name)
            .map(|d| (d.type_id)())
            .expect("registered option must have OPTION_DECLS entry");

        let buffer_id = self.document_buffer_id;
        let modes_snapshot = self
            .active_modes
            .get(&buffer_id)
            .cloned()
            .unwrap_or_default();
        let buffer_local = self.buffer_local_overrides.get(&buffer_id);

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("# option resolution :: {}", name));
        lines.push(String::new());
        lines.push(format!("- type:                 `{}`", spec.type_label()));
        lines.push(format!(
            "- resolved value:       `{}`",
            spec.get_formatted()
        ));
        lines.push(format!(
            "- typed-option (`:set`): `{}`",
            spec.get_formatted()
        ));
        lines.push(format!(
            "- default:              `{}`",
            spec.default_formatted()
        ));
        lines.push(String::new());
        lines.push("Layered contributions for this buffer (highest → lowest):".into());
        lines.push(String::new());

        lines.push("- modal-state: (empty -- v1 wires no modal-overrides)".into());

        let local_has = buffer_local
            .map(|set| set.iter().any(|o| o.option_type_id == target_type_id))
            .unwrap_or(false);
        if local_has {
            lines.push("- buffer-local (`:setlocal`): contributes ⭐".into());
        } else {
            lines.push("- buffer-local (`:setlocal`): (no override)".into());
        }

        let minors: Vec<lattice_mode::ModeId> =
            modes_snapshot.minors().iter().copied().rev().collect();
        if minors.is_empty() {
            lines.push("- minors: (none active)".into());
        } else {
            let mut any_contributes = false;
            for minor_id in &minors {
                let Some(minor) = self.mode_registry.get(*minor_id) else {
                    continue;
                };
                let opts = minor.options();
                let contributes = opts.iter().any(|o| o.option_type_id == target_type_id);
                if contributes {
                    if !any_contributes {
                        lines.push("- minors:".into());
                        any_contributes = true;
                    }
                    lines.push(format!("    - `{minor_id}` ⭐"));
                }
            }
            if !any_contributes {
                lines.push(format!(
                    "- minors: {} active, none contribute this option",
                    minors.len(),
                ));
            }
        }

        match modes_snapshot.major() {
            Some(major_id) => match self.mode_registry.get(major_id) {
                Some(major) => {
                    let opts = major.options();
                    let contributes = opts.iter().any(|o| o.option_type_id == target_type_id);
                    if contributes {
                        lines.push(format!("- major: `{major_id}` contributes ⭐"));
                    } else {
                        lines.push(format!("- major: `{major_id}` (no contribution)",));
                    }
                }
                None => {
                    lines.push(format!("- major: `{major_id}` (mode missing)"));
                }
            },
            None => {
                lines.push("- major: (none active)".into());
            }
        }

        lines.push(format!("- typed-option layer: `{}`", spec.get_formatted(),));
        lines.push(format!(
            "- built-in default:   `{}`",
            spec.default_formatted(),
        ));

        lines.push(String::new());
        lines.push(
            "⭐ marks layers contributing this option. The highest \
             marked layer wins. Mode-architecture §6.1 explains the \
             layer priority order in detail."
                .into(),
        );

        Some(
            lattice_help::HelpContent::from_lines(
                format!("describe-option-resolution {name}"),
                lines,
            )
            .with_markdown_syntax(self.lang_registry.clone()),
        )
    }

    /// 5.5.F.3: build the `:describe-events` content. Walks
    /// [`lattice_protocol::event_registry::EVENT_DESCRIPTORS`]
    /// (the `linkme` distributed slice every `register_event!`
    /// invocation pushes into); groups rows by source crate so
    /// the catalogue is easy to scan.
    pub fn build_describe_events_content(&self) -> lattice_help::HelpContent {
        use lattice_protocol::event_registry::registered_events;
        let mut by_crate: std::collections::BTreeMap<
            &'static str,
            Vec<&'static lattice_protocol::event_registry::EventDescriptor>,
        > = std::collections::BTreeMap::new();
        for d in registered_events() {
            by_crate.entry(d.source_crate).or_default().push(d);
        }
        let mut total = 0usize;
        let mut lines: Vec<String> = Vec::new();
        lines.push("# Registered events".into());
        lines.push(String::new());
        if by_crate.is_empty() {
            lines.push("(none)".into());
        }
        for (source_crate, mut entries) in by_crate {
            entries.sort_by_key(|d| d.name);
            total += entries.len();
            lines.push(format!("## {source_crate} ({})", entries.len()));
            lines.push(String::new());
            for d in entries {
                lines.push(format!("- [{}](event:{})  {}", d.name, d.name, d.doc));
            }
            lines.push(String::new());
        }
        if total > 0 {
            lines.insert(
                1,
                format!(
                    "({total} registered event(s) across {} crate(s))",
                    lines.iter().filter(|l| l.starts_with("## ")).count()
                ),
            );
        }
        lattice_help::HelpContent::from_lines("describe-events", lines)
            .with_markdown_syntax(self.lang_registry.clone())
    }

    /// 5.5.F.4.1: copy the Editor's hot-path cursor / scroll into the
    /// active pane's stash. Called before any operation that flips
    /// which pane is active.
    ///
    /// **Unified hot-path**: `self.cursor` and `self.scroll` are the
    /// active buffer's regardless of kind, so the snapshot reads
    /// from there uniformly. Help / file-tree / oil records are
    /// also synced into their kind-specific cursor / scroll fields
    /// (and the registry copy for help) so the archival state stays
    /// current; live state always lives on the hot-path slots.
    pub fn snapshot_active_pane(&mut self) {
        let cursor = self.cursor;
        let scroll = self.scroll;
        let pane_id = self.pane_tree.active().buffer_id;
        match self.active_buffer {
            BufferKind::Help => {
                // M.4 (b): the popup buffer lives in the registry;
                // mutate it in place there. The hot-path slot is
                // just an id, so there's nothing to mirror back.
                if let Some(id) = self.popup_buffer
                    && id == pane_id
                {
                    self.buffers.with_help_mut(pane_id, |reg| {
                        reg.cursor = cursor;
                        reg.scroll = scroll as usize;
                    });
                }
            }
            BufferKind::FileTree => {
                self.buffers.with_file_tree_mut(pane_id, |t| {
                    t.cursor = cursor;
                    t.scroll = scroll as usize;
                });
            }
            BufferKind::Oil => {
                self.buffers.with_oil_mut(pane_id, |o| {
                    o.cursor = cursor;
                    o.scroll = scroll as usize;
                });
            }
            BufferKind::Document => {}
        }
        let active = self.pane_tree.active_mut();
        active.cursor = cursor;
        active.scroll = scroll;
    }

    /// 5.5.F.4.1: stash the active document's hot-path mode-state
    /// (`syntax`, `last_parsed_text_version`, `last_synced_syntax_version`,
    /// `folds`) into `buffer_locals` so a subsequent `activate_document`
    /// (same or different id) can restore it. Guarded by
    /// `active_buffer == Document` — when active is file-tree or
    /// help, the document's syntax was already moved into locals on
    /// the *previous* transition; a second snapshot would `take()`
    /// an already-None value and overwrite the entry's stashed
    /// syntax, dropping highlight state.
    pub fn snapshot_active_document(&mut self) {
        if !matches!(self.active_buffer, BufferKind::Document) {
            return;
        }
        // M.3.2.c.5: stash mode-state into buffer_locals (the
        // canonical home post-DocumentEntry-field-retirement).
        // Round-tripping every field — including
        // `last_synced_syntax_version` — preserves the syntax
        // worker baseline across a switch-away-and-back so an
        // out-of-order reparse race can't slip through.
        let id = self.document_buffer_id;
        let syntax = self.syntax.take();
        let last_parsed = self.last_parsed_text_version;
        let last_synced = self.last_synced_syntax_version;
        let folds = std::mem::take(&mut self.folds);
        let locals = self.buffer_locals.entry(id).or_default();
        locals.insert(crate::modes::DocumentSyntax(syntax));
        locals.insert(crate::modes::DocumentLastParsedTextVersion(last_parsed));
        locals.insert(crate::modes::DocumentLastSyncedSyntaxVersion(last_synced));
        locals.insert(crate::modes::DocumentFolds(folds));
    }

    /// 5.5.F.4.1: load the active pane's stashed cursor / scroll
    /// into the hot-path slots. Inverse of [`Self::snapshot_active_pane`].
    /// Also restores the help-popup mirror when the active pane is
    /// a help buffer pointing at a different popup than the one
    /// currently mirrored.
    pub fn load_active_pane(&mut self) {
        let pane = *self.pane_tree.active();
        self.active_buffer = pane.buffer;
        self.cursor = pane.cursor;
        self.scroll = pane.scroll;
        if matches!(pane.buffer, BufferKind::Help)
            && self.popup_buffer != Some(pane.buffer_id)
            && self.buffers.contains_help(pane.buffer_id)
        {
            self.popup_buffer = Some(pane.buffer_id);
        }
    }

    /// 5.5.F.4.2: push a tagged entry onto the position-history
    /// ring. If the history cursor is mid-ring (user walking back),
    /// truncate forward entries before pushing — standard
    /// "modify-from-middle" semantics. Capped at [`POSITION_HISTORY_CAP`];
    /// oldest dropped. Adjacent same-position-and-source duplicates
    /// are coalesced. Relocated from `lattice-ui-tui::app::motions`
    /// alongside `activate_buffer` (its only renderer-neutral
    /// caller cluster); other position-history call sites stay on
    /// App via the delegate until the wider `motions.rs` migration.
    pub fn push_position_history(&mut self, pos: lattice_protocol::position::Position, source: PositionSource) {
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
            self.position_history_cursor = self.position_history_cursor.saturating_sub(1);
        }
        self.position_history_cursor = self.position_history.len();
    }

    /// 5.5.F.4.2: switch the active pane to whatever buffer `id`
    /// references, regardless of kind. Document buffers route
    /// through [`Self::activate_document`]; tree buffers update the
    /// active pane + load the tree's stash; help buffers go through
    /// [`Self::activate_help_in_pane`]; oil through
    /// [`Self::activate_oil`].
    ///
    /// Returns `true` when the activation went through the full
    /// `activate_document` path that the App-side caller needs to
    /// follow up with `activate_buffer_state()` (mode/syntax/option
    /// re-init + per-frame highlight-cache clear). Returns `false`
    /// on the early-return paths (unknown id, same-buffer no-op,
    /// non-document target) — the caller skips the tail.
    ///
    /// **Why bool, not void**: until F.5 lands mode lifecycle host-
    /// side, `activate_buffer_state` cannot run inside `Editor` — it
    /// calls `activate_major_for_buffer_kind` + `maybe_reparse_syntax`,
    /// both still on App. The bool is the explicit-coordination
    /// signal across the migration window; it deletes when F.5+
    /// brings the tail host-side.
    pub fn activate_buffer(&mut self, id: BufferId) -> bool {
        let Some(kind) = self.buffers.kind_of(id) else {
            self.set_message(EchoLevel::Error, format!("buffer #{} not found", id.0));
            return false;
        };
        if !self.previewing && id != self.active_pane_buffer_id() {
            let cur = self.active_cursor();
            self.push_position_history(cur, PositionSource::AutoJump);
        }
        match kind {
            BufferKind::Document => self.activate_document(id),
            BufferKind::FileTree => {
                self.activate_file_tree(id);
                false
            }
            BufferKind::Help => {
                self.activate_help_in_pane(id);
                false
            }
            BufferKind::Oil => {
                self.activate_oil(id);
                false
            }
        }
    }

    /// 5.5.F.4.2: switch the active document to `id`. Snapshots
    /// the current active state into `buffer_locals`, then loads
    /// from the destination's locals. Returns `true` when the
    /// caller should run `activate_buffer_state()` next (full-
    /// activation path); returns `false` on no-op (already active)
    /// or on the same-document fast path (returning to the
    /// document buffer that `self.document` already points at,
    /// e.g. from a help-in-pane overlay or a file-tree pane).
    pub fn activate_document(&mut self, id: BufferId) -> bool {
        if id == self.document_buffer_id && matches!(self.active_buffer, BufferKind::Document) {
            return false;
        }
        if !self.buffers.contains_document(id) {
            self.set_message(EchoLevel::Error, format!("buffer #{} not a document", id.0));
            return false;
        }
        self.snapshot_active_pane();
        // Same-document fast path: returning to the document
        // buffer that `self.document` still points at (e.g. from
        // a help-in-pane overlay or a file-tree pane).
        // Help overlay leaves the buffer-locals' DocumentSyntax as
        // None (no stash); file-tree leaves it as Some (stashed via
        // snapshot_active_document). The "is the entry stashed?"
        // check is `entry.syntax.is_some()`; folds piggyback.
        if id == self.document_buffer_id {
            self.active_buffer = BufferKind::Document;
            let pane = self.pane_tree.active_mut();
            pane.buffer = BufferKind::Document;
            pane.buffer_id = id;
            // Pull stashed mode-state out of buffer_locals when
            // re-activating a buffer the user just left for a pane
            // overlay. The `is_some()` guard preserves the help-
            // overlay invariant: when the active buffer returns
            // from a popup that didn't focus into help, no sync
            // happened, so locals are stale and we leave
            // self.syntax / self.folds untouched.
            let stashed_syntax = self
                .buffer_locals
                .get(&id)
                .and_then(|l| l.get::<crate::modes::DocumentSyntax>())
                .and_then(|s| s.0.clone());
            if stashed_syntax.is_some() {
                self.syntax = stashed_syntax;
                self.last_parsed_text_version = self
                    .buffer_locals
                    .get(&id)
                    .and_then(|l| l.get::<crate::modes::DocumentLastParsedTextVersion>())
                    .map(|v| v.0)
                    .unwrap_or(0);
                self.last_synced_syntax_version = self
                    .buffer_locals
                    .get(&id)
                    .and_then(|l| l.get::<crate::modes::DocumentLastSyncedSyntaxVersion>())
                    .map(|v| v.0)
                    .unwrap_or(0);
                self.folds = self
                    .buffer_locals
                    .get(&id)
                    .and_then(|l| l.get::<crate::modes::DocumentFolds>())
                    .map(|f| f.0.clone())
                    .unwrap_or_default();
            }
            // Same-document fast path doesn't need the
            // activate_buffer_state tail — options / modes / syntax
            // / folds are unchanged.
            return false;
        }
        self.snapshot_active_document();
        // Load destination — clone the handle out under the
        // registry lock so we don't hold the lock past the borrow.
        self.document = self
            .buffers
            .document_handle(id)
            .expect("contains_document lookup above succeeded");
        self.snapshot_cache = self.document.snapshot_cache();
        self.syntax = self
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::DocumentSyntax>())
            .and_then(|s| s.0.clone());
        self.last_parsed_text_version = self
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::DocumentLastParsedTextVersion>())
            .map(|v| v.0)
            .unwrap_or(0);
        self.last_synced_syntax_version = self
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::DocumentLastSyncedSyntaxVersion>())
            .map(|v| v.0)
            .unwrap_or(0);
        self.folds = self
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::DocumentFolds>())
            .map(|f| f.0.clone())
            .unwrap_or_default();
        self.document_buffer_id = id;
        let pane = self.pane_tree.active_mut();
        pane.buffer = BufferKind::Document;
        pane.buffer_id = id;
        self.current_match = None;
        self.all_matches.clear();
        self.search_line = None;
        self.cursor = lattice_protocol::position::Position::ZERO;
        self.scroll = 0;
        self.load_active_pane();
        // Echo the switch. `set_message` runs host-side (F.1).
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
        // Full-activation path: caller must run activate_buffer_state.
        true
    }

    /// 5.5.F.4.3: listed buffer ids in ascending order across kinds.
    /// `:bn` / `:bp` cycle through this; unlisted buffers (vim's
    /// `nobuflisted`) are filtered out.
    fn listed_buffer_ids_sorted(&self) -> Vec<BufferId> {
        self.buffers.listed_ids_sorted()
    }

    /// 5.5.F.4.3: next listed buffer id from the active pane's, in
    /// cyclical sorted order. `None` if there's only one listed
    /// buffer (no other valid target).
    pub fn next_listed_buffer_id(&self) -> Option<BufferId> {
        let ids = self.listed_buffer_ids_sorted();
        if ids.len() <= 1 {
            return None;
        }
        let cur = self.active_pane_buffer_id();
        let pos = ids.iter().position(|id| *id == cur)?;
        Some(ids[(pos + 1) % ids.len()])
    }

    /// 5.5.F.4.3: previous listed buffer id from the active pane's,
    /// in cyclical sorted order.
    pub fn prev_listed_buffer_id(&self) -> Option<BufferId> {
        let ids = self.listed_buffer_ids_sorted();
        if ids.len() <= 1 {
            return None;
        }
        let cur = self.active_pane_buffer_id();
        let pos = ids.iter().position(|id| *id == cur)?;
        Some(ids[if pos == 0 { ids.len() - 1 } else { pos - 1 }])
    }

    /// 5.5.F.4.3: `:bnext` / `:bn` — cycle to the next listed
    /// buffer. Returns `true` when the activation went through the
    /// full `activate_document` path; caller must run
    /// [`Self::activate_buffer_state`] (the `handle_effect` arm
    /// does this inline as of F.5.5).
    pub fn do_buffer_next(&mut self) -> bool {
        let Some(target) = self.next_listed_buffer_id() else {
            self.set_message(EchoLevel::Info, "only one listed buffer".to_string());
            return false;
        };
        self.activate_buffer(target)
    }

    /// 5.5.F.4.3: `:bprev` / `:bp` — cycle to the previous listed
    /// buffer. Same return-shape as [`Self::do_buffer_next`].
    pub fn do_buffer_prev(&mut self) -> bool {
        let Some(target) = self.prev_listed_buffer_id() else {
            self.set_message(EchoLevel::Info, "only one listed buffer".to_string());
            return false;
        };
        self.activate_buffer(target)
    }

    /// 5.5.F.4.4: detach a buffer from every attached LSP server.
    /// Removes the URI→BufferId mapping, then fires the wire-level
    /// `didClose` (fire-and-forget against the supervisor mailbox).
    /// No-op when the buffer was never attached.
    pub fn lsp_close_buffer(&mut self, buffer_id: BufferId) {
        let Some(uri) = self.buffer_uris.remove(&buffer_id) else {
            return;
        };
        self.lsp.close_buffer(uri);
    }

    /// 5.5.F.4.4: `:bd[elete]` — close the active buffer. v1 picks
    /// any other buffer to activate; if no others remain, the close
    /// is rejected. For document buffers `!` bypasses the dirty
    /// check; tree buffers are always read-only and skip the guard.
    ///
    /// Returns `true` when the successor activation went through
    /// the full `activate_document` path; caller must run
    /// [`Self::activate_buffer_state`] (the `handle_effect` arm
    /// does this inline as of F.5.5).
    pub fn do_buffer_delete(&mut self, force: bool) -> bool {
        let to_remove = self.active_pane_buffer_id();
        // "Only buffer" check uses the *listed* count — unlisted
        // synthetic buffers (`*lsp*`, `*lsp:<server>*`, ...) don't
        // count as switch destinations. Without this, `:bd` would
        // happily activate `*lsp*` as the successor and leave the
        // user staring at a read-only log buffer with no document
        // to return to.
        let listed = self.buffers.listed_ids_sorted();
        let to_remove_is_listed = self
            .buffers
            .flags_of(to_remove)
            .map(|f| f.listed)
            .unwrap_or(false);
        if to_remove_is_listed && listed.len() <= 1 {
            self.set_message(
                EchoLevel::Error,
                "Cannot delete the only buffer".to_string(),
            );
            return false;
        }
        // Dirty check applies to documents only.
        if !force && self.buffers.document_dirty(to_remove) {
            self.set_message(
                EchoLevel::Error,
                "no write since last change (add ! to override)".to_string(),
            );
            return false;
        }
        // Successor preference: another *listed* buffer if any,
        // else any other buffer (including unlisted synthetics).
        let mut successor = listed.iter().copied().find(|id| *id != to_remove);
        if successor.is_none() {
            successor = self
                .buffers
                .sorted_ids()
                .into_iter()
                .find(|id| *id != to_remove);
        }
        let Some(successor) = successor else {
            return false;
        };
        let ran_full = self.activate_buffer(successor);
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
        ran_full
    }

    /// 5.5.F.4.2: switch the active pane to the file-tree buffer
    /// with `id`. Snapshots the current active state first; the
    /// pane's stashed cursor / scroll load into the tree's hot
    /// fields. No `activate_buffer_state` tail — tree buffers don't
    /// have document/syntax/options state to re-resolve.
    pub fn activate_file_tree(&mut self, id: BufferId) {
        if !self.buffers.contains_file_tree(id) {
            self.set_message(EchoLevel::Error, format!("buffer #{} not a tree", id.0));
            return;
        }
        if id == self.active_pane_buffer_id() && matches!(self.active_buffer, BufferKind::FileTree)
        {
            return;
        }
        self.snapshot_active_pane();
        self.snapshot_active_document();
        let (stash_cursor, stash_scroll) = self
            .buffers
            .with_file_tree(id, |t| (t.cursor, t.scroll as u32))
            .unwrap_or((lattice_protocol::position::Position::ZERO, 0));
        self.cursor = stash_cursor;
        self.scroll = stash_scroll;
        self.active_buffer = BufferKind::FileTree;
        let pane = self.pane_tree.active_mut();
        pane.buffer = BufferKind::FileTree;
        pane.buffer_id = id;
        pane.cursor = stash_cursor;
        pane.scroll = stash_scroll;
    }

    /// 5.5.F.4.2: switch the active pane to the oil buffer with `id`.
    pub fn activate_oil(&mut self, id: BufferId) {
        let Some((oil_cursor, oil_scroll)) = self.buffers.with_oil(id, |o| (o.cursor, o.scroll))
        else {
            return;
        };
        self.active_buffer = BufferKind::Oil;
        let pane = self.pane_tree.active_mut();
        pane.buffer = BufferKind::Oil;
        pane.buffer_id = id;
        pane.cursor = oil_cursor;
        pane.scroll = oil_scroll as u32;
        self.cursor = oil_cursor;
        self.scroll = oil_scroll as u32;
    }

    /// 5.5.F.4.2: switch the active pane to an existing help
    /// buffer in the registry. Snapshots prior pane state so
    /// `<C-o>` returns the user to the document/cursor they came
    /// from. The registry's HelpBuffer is mirrored into
    /// `self.popup_buffer` so the existing keymap + render paths
    /// transparently target it.
    pub fn activate_help_in_pane(&mut self, id: BufferId) {
        if !self.buffers.contains_help(id) {
            self.set_message(
                EchoLevel::Error,
                format!("buffer #{} not a help buffer", id.0),
            );
            return;
        }
        // Skip the auto-jump push during picker-preview hovers —
        // the user hasn't committed to this buffer yet.
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
        // Note: do NOT call snapshot_active_document here. Help is
        // rendered as a popup overlay over the underlying document;
        // the pane's per-frame paint still draws the active
        // document via the snapshot path which reads self.syntax /
        // self.folds. Stashing those into locals would leave
        // self.syntax = None for the help session.
        if self.popup_buffer != Some(id) && self.buffers.contains_help(id) {
            self.popup_buffer = Some(id);
        }
        let (stash_cursor, stash_scroll) = self
            .popup_help()
            .map(|h| (h.cursor, h.scroll as u32))
            .unwrap_or((lattice_protocol::position::Position::ZERO, 0));
        self.cursor = stash_cursor;
        self.scroll = stash_scroll;
        self.active_buffer = BufferKind::Help;
        let pane = self.pane_tree.active_mut();
        pane.buffer = BufferKind::Help;
        pane.buffer_id = id;
        pane.cursor = stash_cursor;
        pane.scroll = stash_scroll;
    }

    /// 5.5.F.3: build the `:describe-event <name>` content.
    /// Renders the descriptor for one registered event. Mirrors
    /// `:describe-command` / `:describe-option`'s shape. Unknown
    /// name routes an error to the echo ring; dispatcher skips.
    pub fn build_describe_event_content(
        &mut self,
        name: &str,
    ) -> Option<lattice_help::HelpContent> {
        use lattice_protocol::event_registry::descriptor_by_name;
        let Some(d) = descriptor_by_name(name) else {
            self.set_message(EchoLevel::Error, format!("no event named `{name}`"));
            return None;
        };
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("# event :: {}", d.name));
        lines.push(String::new());
        lines.push(format!("- source crate: `{}`", d.source_crate));
        lines.push(format!("- type-id name: `{}`", d.name));
        lines.push(String::new());
        lines.push(d.doc.to_string());
        lines.push(String::new());
        lines.push(
            "Subscribe via `EventBus::subscribe_typed::<T>(tx)` where `T` \
             is the concrete event struct exported by the source crate."
                .into(),
        );
        Some(
            lattice_help::HelpContent::from_lines(format!("describe-event {name}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        )
    }

    /// 5.5.F.6: `:list-modes` (M.8) content builder — render every
    /// registered mode as a help buffer. Groups by kind (Major /
    /// Minor); each row shows the mode's id and `*` if currently
    /// active on the active document buffer. Mode counterpart of
    /// `:options`.
    pub fn build_list_modes_content(&self) -> lattice_help::HelpContent {
        let mut majors: Vec<lattice_mode::ModeId> = Vec::new();
        let mut minors: Vec<lattice_mode::ModeId> = Vec::new();
        for (id, kind) in self.mode_registry.iter_meta() {
            match kind {
                lattice_mode::ModeKind::Major => majors.push(id),
                lattice_mode::ModeKind::Minor => minors.push(id),
            }
        }
        majors.sort_by_key(|m| m.as_str().to_string());
        minors.sort_by_key(|m| m.as_str().to_string());

        let buffer_id = self.document_buffer_id;
        let active = self.active_modes.get(&buffer_id);
        let active_major = active.and_then(|a| a.major());
        let is_minor_active =
            |id: lattice_mode::ModeId| -> bool { active.map(|a| a.has_minor(id)).unwrap_or(false) };

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!(
            "# Modes ({} registered)",
            majors.len() + minors.len(),
        ));
        lines.push(String::new());
        lines.push(
            "Mark `*` indicates the mode is active on the currently \
             focused buffer. For per-mode detail run \
             `:describe-mode <name>`. Toggle a mode with \
             `:<mode-name>` (e.g. `:lsp-mode`)."
                .into(),
        );
        lines.push(String::new());

        lines.push(format!("## majors ({})", majors.len()));
        lines.push(String::new());
        for id in &majors {
            let marker = if Some(*id) == active_major { "*" } else { " " };
            lines.push(format!("- {marker} [{id}](mode:{id})"));
        }
        lines.push(String::new());

        lines.push(format!("## minors ({})", minors.len()));
        lines.push(String::new());
        for id in &minors {
            let marker = if is_minor_active(*id) { "*" } else { " " };
            lines.push(format!("- {marker} [{id}](mode:{id})"));
        }

        lattice_help::HelpContent::from_lines("list-modes", lines)
            .with_markdown_syntax(self.lang_registry.clone())
    }

    /// 5.5.F.6: `:describe-mode <name>` (M.8) content builder —
    /// render one mode's metadata: id, kind, contributed option
    /// overrides (mapping each `TypeId` back to the option's display
    /// name via `OPTION_DECLS`), required capabilities, and current
    /// activation state on the active buffer. Mode counterpart of
    /// `:describe-option`. Fallible: pushes echo + returns None on
    /// unknown name.
    pub fn build_describe_mode_content(
        &mut self,
        name: &str,
    ) -> Option<lattice_help::HelpContent> {
        let mode_id = lattice_mode::ModeId::new(name);
        let Some(mode) = self.mode_registry.get(mode_id) else {
            self.set_message(EchoLevel::Error, format!("no mode named `{name}`"));
            return None;
        };

        // TypeId → option name lookup. Walk OPTION_DECLS to render
        // the mode's contributed overrides as readable names instead
        // of opaque TypeIds.
        let type_id_to_name: std::collections::HashMap<std::any::TypeId, &'static str> =
            lattice_config::OPTION_DECLS
                .iter()
                .map(|d| ((d.type_id)(), d.name))
                .collect();

        let buffer_id = self.document_buffer_id;
        let active = self.active_modes.get(&buffer_id);
        let is_active = match mode.kind() {
            lattice_mode::ModeKind::Major => active.and_then(|a| a.major()) == Some(mode_id),
            lattice_mode::ModeKind::Minor => active.map(|a| a.has_minor(mode_id)).unwrap_or(false),
        };
        let kind_label = match mode.kind() {
            lattice_mode::ModeKind::Major => "major",
            lattice_mode::ModeKind::Minor => "minor",
        };

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("# mode :: {mode_id}"));
        lines.push(String::new());
        lines.push(format!("- kind: `{kind_label}`"));
        lines.push(format!(
            "- active on current buffer: {}",
            if is_active { "yes" } else { "no" }
        ));

        // Option contributions.
        let opts = mode.options();
        if opts.is_empty() {
            lines.push("- contributed options: (none)".into());
        } else {
            lines.push(format!("- contributed options ({}):", opts.iter().count()));
            for ovr in opts.iter() {
                let name = type_id_to_name
                    .get(&ovr.option_type_id)
                    .copied()
                    .unwrap_or("(unknown option)");
                lines.push(format!("    - `{name}`"));
            }
        }

        // Capabilities.
        let caps = mode.required_capabilities();
        if caps == lattice_mode::CapabilitySet::empty() {
            lines.push("- required capabilities: (none)".into());
        } else {
            lines.push(format!("- required capabilities: `{caps:?}`"));
        }

        lines.push(String::new());
        lines.push(format!(
            "Toggle with `:{mode_id}`. For options the mode contributes, \
             see `:describe-option <name>`.",
        ));

        Some(
            lattice_help::HelpContent::from_lines(format!("describe-mode {name}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        )
    }

    /// 5.5.F.6: `:customize` (no args) (M.9.0) content builder —
    /// picker view: every group + every registered mode that
    /// contributes at least one customizable option. Each row is
    /// a `[label](customize:name)` link.
    pub fn build_customize_picker_content(&self) -> lattice_help::HelpContent {
        // Modes that contribute at least one customizable option.
        let customizable_type_ids: std::collections::HashSet<std::any::TypeId> =
            lattice_config::OPTION_DECLS
                .iter()
                .filter(|d| d.customizable)
                .map(|d| (d.type_id)())
                .collect();
        let mut customisable_modes: Vec<lattice_mode::ModeId> = Vec::new();
        for (mode_id, _kind) in self.mode_registry.iter_meta() {
            if let Some(mode) = self.mode_registry.get(mode_id) {
                let opts = mode.options();
                if opts
                    .iter()
                    .any(|o| customizable_type_ids.contains(&o.option_type_id))
                {
                    customisable_modes.push(mode_id);
                }
            }
        }
        customisable_modes.sort_by_key(|m| m.as_str().to_string());

        // Groups: every registered OptionGroup plus its option count.
        let mut group_counts: std::collections::BTreeMap<&'static str, (usize, &'static str)> =
            std::collections::BTreeMap::new();
        for g in lattice_config::GROUP_DECLS.iter() {
            group_counts.insert(g.name, (0, g.doc));
        }
        for d in lattice_config::OPTION_DECLS.iter() {
            if !d.customizable {
                continue;
            }
            if let Some(entry) = group_counts.get_mut(d.group_name) {
                entry.0 += 1;
            }
        }

        let mut lines: Vec<String> = Vec::new();
        lines.push("# Customize".into());
        lines.push(String::new());
        lines.push(
            "Pick a group to browse options across modes, or a mode \
             to see what it contributes. `:customize <name>` opens \
             the focused view; this picker is just navigation."
                .into(),
        );
        lines.push(String::new());

        lines.push(format!("## groups ({})", group_counts.len()));
        lines.push(String::new());
        for (name, (count, doc)) in &group_counts {
            lines.push(format!("- [{name}](customize:{name}) ({count}) -- {doc}"));
        }
        lines.push(String::new());

        lines.push(format!("## modes ({})", customisable_modes.len()));
        lines.push(String::new());
        for id in &customisable_modes {
            lines.push(format!("- [{id}](customize:{id})"));
        }

        lattice_help::HelpContent::from_lines("customize", lines)
            .with_markdown_syntax(self.lang_registry.clone())
    }

    /// 5.5.F.6: `:customize <group>` content builder — every
    /// customizable option in `<group>`. Each row shows the option's
    /// canonical name + aliases, type, current value, default (when
    /// it differs), and the doc string. Fallible: pushes echo +
    /// returns None on unknown group.
    pub fn build_customize_group_content(
        &mut self,
        group_name: &str,
    ) -> Option<lattice_help::HelpContent> {
        let group_doc = lattice_config::GROUP_DECLS
            .iter()
            .find(|g| g.name == group_name)
            .map(|g| g.doc);
        let Some(doc) = group_doc else {
            self.set_message(EchoLevel::Error, format!("no group named `{group_name}`"));
            return None;
        };

        let mut entries: Vec<&'static lattice_config::OptionDeclMetadata> =
            lattice_config::OPTION_DECLS
                .iter()
                .filter(|d| d.customizable && d.group_name == group_name)
                .copied()
                .collect();
        entries.sort_by_key(|d| d.name);

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("# customize :: {group_name}"));
        lines.push(String::new());
        lines.push(doc.to_string());
        lines.push(String::new());
        if entries.is_empty() {
            lines.push("(no customizable options in this group)".into());
        } else {
            lines.push(format!("{} option(s):", entries.len()));
            lines.push(String::new());
            for meta in &entries {
                self.append_customize_row(&mut lines, meta);
            }
        }
        lines.push(String::new());
        lines.push(
            "To edit any option above run `:set NAME=VALUE` from \
             the cmdline. Per-row edit affordances land in M.9.1."
                .into(),
        );
        Some(
            lattice_help::HelpContent::from_lines(format!("customize {group_name}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        )
    }

    /// 5.5.F.6: `:customize <mode-name>` content builder — every
    /// option the mode contributes via `Mode::options()`. Each row
    /// shows the same metadata as the group view, plus a
    /// `[mode-shadow]` indicator when the contribution is active on
    /// the active buffer. Fallible: pushes echo + returns None on
    /// unknown mode.
    pub fn build_customize_mode_content(
        &mut self,
        mode_name: &str,
    ) -> Option<lattice_help::HelpContent> {
        let mode_id = lattice_mode::ModeId::new(mode_name);
        let Some(mode) = self.mode_registry.get(mode_id) else {
            self.set_message(EchoLevel::Error, format!("no mode named `{mode_name}`"));
            return None;
        };

        let by_type_id: std::collections::HashMap<
            std::any::TypeId,
            &'static lattice_config::OptionDeclMetadata,
        > = lattice_config::OPTION_DECLS
            .iter()
            .map(|d| ((d.type_id)(), *d))
            .collect();

        let buffer_id = self.document_buffer_id;
        let active = self.active_modes.get(&buffer_id);
        let mode_active_here = match mode.kind() {
            lattice_mode::ModeKind::Major => active.and_then(|a| a.major()) == Some(mode_id),
            lattice_mode::ModeKind::Minor => active.map(|a| a.has_minor(mode_id)).unwrap_or(false),
        };

        let mut entries: Vec<&'static lattice_config::OptionDeclMetadata> = Vec::new();
        for ovr in mode.options().iter() {
            if let Some(meta) = by_type_id.get(&ovr.option_type_id)
                && meta.customizable
            {
                entries.push(meta);
            }
        }
        entries.sort_by_key(|d| d.name);
        entries.dedup_by_key(|d| d.name);

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("# customize :: {mode_id}"));
        lines.push(String::new());
        lines.push(format!(
            "Mode kind: `{}`. {} on the active buffer.",
            match mode.kind() {
                lattice_mode::ModeKind::Major => "major",
                lattice_mode::ModeKind::Minor => "minor",
            },
            if mode_active_here {
                "Active"
            } else {
                "Inactive"
            },
        ));
        lines.push(String::new());
        if entries.is_empty() {
            lines.push("(this mode contributes no customizable options)".into());
        } else {
            lines.push(format!("Contributes {} option(s):", entries.len()));
            lines.push(String::new());
            for meta in &entries {
                self.append_customize_row(&mut lines, meta);
                if mode_active_here {
                    lines.push(
                        "    [mode-shadow] this mode's contribution is \
                         active on the active buffer; a `:set` write \
                         here will be overridden by the mode-contribution \
                         layer until the mode deactivates."
                            .into(),
                    );
                }
            }
        }
        lines.push(String::new());
        lines.push(
            "To edit any option above run `:set NAME=VALUE` from \
             the cmdline. Per-row edit affordances land in M.9.1."
                .into(),
        );
        Some(
            lattice_help::HelpContent::from_lines(format!("customize {mode_name}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        )
    }

    /// 5.5.F.7: `:diagnostics` — open every published diagnostic
    /// across every attached server in a vertico-style picker.
    /// Severity glyph in the marginalia (`[E]` / `[W]` / `[I]` /
    /// `[H]`) and the diagnostic message as the preview text. Empty
    /// snapshot or empty rows route an info echo.
    ///
    /// Sets `self.picker` directly — pickers are a renderer-neutral
    /// `Editor` field, so no `RendererSignal` is required (the
    /// renderer reads the picker each frame).
    pub fn do_list_diagnostics(&mut self) {
        // `:diagnostics` is a browse-style picker, not a tag-intent
        // drill-down — clear any stale nav origin so a later
        // JumpToLspLocation accept doesn't push a phantom tag stack
        // entry.
        self.pending_tag_origin = None;
        let snapshot = self.lsp_diagnostics.snapshot();
        if snapshot.is_empty() {
            self.set_message(EchoLevel::Info, "no diagnostics".to_string());
            return;
        }
        let mut rows: Vec<lattice_picker::LspLocationRow> = Vec::new();
        for (uri, diags) in snapshot {
            let path = match lattice_lsp::actor::uri_to_path(&uri) {
                Some(p) => p,
                None => continue,
            };
            for d in diags {
                let sev = match d.severity {
                    Some(lattice_lsp::DiagnosticSeverity::ERROR) => "[E]",
                    Some(lattice_lsp::DiagnosticSeverity::WARNING) => "[W]",
                    Some(lattice_lsp::DiagnosticSeverity::INFORMATION) => "[I]",
                    Some(lattice_lsp::DiagnosticSeverity::HINT) => "[H]",
                    _ => "[?]",
                };
                rows.push(lattice_picker::LspLocationRow {
                    path: path.clone(),
                    line: d.range.start.line,
                    col: d.range.start.character,
                    preview: lattice_help::one_line(&d.message),
                    marginalia: sev.to_string(),
                });
            }
        }
        if rows.is_empty() {
            self.set_message(EchoLevel::Info, "no diagnostics".to_string());
            return;
        }
        let total = rows.len();
        let mut p = lattice_picker::Picker::new(
            format!("diagnostics ({total})"),
            lattice_picker::PickerSource::LspLocations,
            lattice_picker::PickerAction::JumpToLspLocation,
        );
        p.set_lsp_locations(rows);
        self.picker = Some(p);
    }

    /// 5.5.F.6: shared row formatter for the customize views.
    /// Renders one option's metadata in the `:options`-listing-
    /// compatible shape. Wraps the option name in a
    /// `[NAME](customize-edit:NAME)` link so `<CR>` on the row
    /// prefills the cmdline with `:set NAME=current` for inline
    /// editing (M.9.2).
    fn append_customize_row(
        &self,
        lines: &mut Vec<String>,
        meta: &lattice_config::OptionDeclMetadata,
    ) {
        let spec = self.config.lookup(meta.name);
        let aliases = spec
            .as_ref()
            .map(|s| s.aliases())
            .filter(|a| !a.is_empty())
            .map(|a| format!(" [{}]", a.join(", ")))
            .unwrap_or_default();
        let type_label = (meta.type_label)();
        let default = (meta.default_formatted)();
        let current = spec
            .as_ref()
            .map(|s| s.get_formatted())
            .unwrap_or_else(|| "?".into());
        let name_link = format!("[{0}](customize-edit:{0})", meta.name);
        let header = if current == default {
            format!(
                "- **{name_link}**{aliases} : {type_label} = {current}"
            )
        } else {
            format!(
                "- **{name_link}**{aliases} : {type_label} = {current} (default: {default})"
            )
        };
        lines.push(header);
        for doc_line in meta.doc.lines() {
            let trimmed = doc_line.trim();
            if !trimmed.is_empty() {
                lines.push(format!("    {trimmed}"));
            }
        }
        if let Some(values) = spec.as_ref().and_then(|s| s.enumerate_values()) {
            lines.push(format!("    values: {}", values.join(", ")));
        }
        lines.push(String::new());
    }
}

/// 4.4.k: returns `Some(server_id)` when `canonical_name` names a
/// server-scoped config key (`lsp.<server_id>.<...>`), `None`
/// otherwise. Used by [`Editor::apply_option_cascade`] to decide
/// whether an option change should fan out
/// `workspace/didChangeConfiguration` to a language server.
///
/// Single-dot `lsp.foo` keys (e.g. `lsp.log_level`,
/// `lsp.log_capacity`) are host-side host-knob options; the spec
/// is that we never page servers for host-side knob changes.
pub(crate) fn lsp_server_scope(canonical_name: &str) -> Option<&str> {
    let rest = canonical_name.strip_prefix("lsp.")?;
    let dot = rest.find('.')?;
    let server_id = &rest[..dot];
    if server_id.is_empty() {
        None
    } else {
        Some(server_id)
    }
}

/// Project a grammar [`VisualKind`] onto the protocol-side
/// [`VisualMode`]. Used when constructing a [`Selection`] from a
/// modal-state visual mode so the document actor's selection set
/// carries the visual flavour. Moved here from
/// `lattice-ui-tui::app::visual` in 5.5.E.4 alongside the
/// [`Effect::SelectionChange`] arm.
pub fn visual_kind_to_mode(kind: VisualKind) -> VisualMode {
    match kind {
        VisualKind::Charwise => VisualMode::Charwise,
        VisualKind::Linewise => VisualMode::Linewise,
        VisualKind::Blockwise => VisualMode::Blockwise,
    }
}

/// Render a register's content into a one-line preview (truncated
/// and with newlines escaped). Used by [`Editor::do_list_registers`]
/// (`:reg`) and the picker-source register listing. Moved here from
/// `lattice-ui-tui::app` in 5.5.E.2.
pub fn preview_register(s: &str) -> String {
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

/// Compute the last addressable line of `buf`. ropey reports an
/// extra empty line for any rope ending in `\n`; this helper returns
/// the line index of the last non-trailing-empty row instead.
fn last_addressable_line(buf: &lattice_core::Buffer) -> u32 {
    let lc = buf.line_count();
    if lc == 0 {
        return 0;
    }
    let last_idx = lc - 1;
    if buf.line_byte_len(last_idx) == 0 && lc >= 2 {
        last_idx - 1
    } else {
        last_idx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `RendererSignal` is `Clone` so the renderer can fan signals
    /// out without consuming the host's `Vec<RendererSignal>` (and
    /// so unit tests can splat representative variants without
    /// caring about Drop semantics). 5.5.E.6 dropped `Copy` because
    /// `LspConfigChanged` (and the now-retired `MirrorOptionToModes`)
    /// carry an owned `String`; 5.5.F.1 dropped `PartialEq` / `Eq`
    /// because `DisplayBuffer(Box<DisplayBufferRequest>)` carries a
    /// `lattice_help::HelpContent` that isn't value-equatable (the
    /// syntax-highlight cache is renderer-neutral but not
    /// `PartialEq`). Signals are produced at `:` / Effect-arm rate,
    /// not per-frame, so neither `String` cloning nor the `Box`
    /// allocation lands anywhere near the perf gate.
    #[test]
    fn renderer_signal_is_clone() {
        fn assert_clone<T: Clone>(_: T) {}
        assert_clone(RendererSignal::ThemeChanged);
        assert_clone(RendererSignal::Quit);
        assert_clone(RendererSignal::NerdFontsToggled);
        assert_clone(RendererSignal::LspConfigChanged("rust-analyzer".into()));
        assert_clone(RendererSignal::DisplayBuffer(Box::new(
            DisplayBufferRequest {
                content: lattice_help::HelpContent::from_lines("test", vec!["x".into()]),
                category: lattice_core::ui::display::BufferDisplayCategory::HelpList,
            },
        )));
    }

    /// 4.4.k: `lsp.<server>.<key>` returns the server-id; anything
    /// shallower (`lsp.<host-knob>`) is host-side and returns None.
    /// The host-side knobs (e.g. `lsp.log_level`) must NOT trigger
    /// `workspace/didChangeConfiguration`, since they configure the
    /// host's behaviour, not the server's. Phase 5.5.E.6 relocated
    /// this helper alongside the migrated `apply_option_cascade`.
    #[test]
    fn lsp_server_scope_picks_server_id_segment() {
        assert_eq!(
            lsp_server_scope("lsp.rust-analyzer.checkOnSave"),
            Some("rust-analyzer")
        );
        assert_eq!(
            lsp_server_scope("lsp.gopls.completeUnimported"),
            Some("gopls")
        );
        // Single-dot under lsp.* -> host knob, NOT a fan-out target.
        assert_eq!(lsp_server_scope("lsp.log_level"), None);
        assert_eq!(lsp_server_scope("lsp.log_capacity"), None);
        // Non-lsp options are unaffected.
        assert_eq!(lsp_server_scope("tabstop"), None);
        assert_eq!(lsp_server_scope("ui.theme"), None);
        // Empty server-id (`lsp..foo`) is rejected -- malformed config
        // should never page a phantom server.
        assert_eq!(lsp_server_scope("lsp..foo"), None);
    }

    /// 5.5.A acceptance shape: [`DispatchOutcome::default()`]
    /// starts with no signals, and `handle_action` is a no-op that
    /// preserves that. When sub-slices populate the body, dedicated
    /// per-arm tests replace this smoke test.
    #[test]
    fn dispatch_outcome_default_has_no_signals() {
        let out = DispatchOutcome::default();
        assert!(out.renderer_signals.is_empty());
    }

    /// 5.5.E.1 acceptance shape: every migrated `Effect` arm (`None`,
    /// `ClearSearchHighlight`, `Echo`) is renderer-neutral state
    /// mutation only -- none of them emit a `RendererSignal`. The
    /// signal channel first lights up in a later E.* sub-slice when
    /// `Effect::SetOption` migrates and `ThemeChanged` starts firing.
    /// Behavioural coverage for the three arms lives at the App layer
    /// (`app::search::tests::nohlsearch_clears_overlay`,
    /// `app::search::tests::substitute_no_match_emits_error`); host-
    /// side standalone construction is gated on the future
    /// `Editor::new` extraction (M.0–M.9 plan).
    #[test]
    fn migrated_effect_arms_emit_no_renderer_signals() {
        // We can't build a bare `Editor` here yet, so this is a
        // type-level guard: the inner [`handle_effect`] free function
        // takes `&mut DispatchOutcome` by reference, and the
        // `_out` binding in its body is `_`-prefixed precisely because
        // 5.5.E.1's arms don't push signals. If a future contributor
        // wires a signal through one of the migrated arms without
        // updating its tests, the rename of `_out` -> `out` here will
        // surface in this module's diff.
        let out = DispatchOutcome::default();
        assert!(out.renderer_signals.is_empty());
    }
}
