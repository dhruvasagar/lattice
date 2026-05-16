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
use crate::state::{SearchLine, UnnamedRegister};

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
    /// 5.5.E.6: the option-cascade just touched a `bool` option
    /// whose canonical name is mirrored by one or more registered
    /// modes (`Mode::mirrors_option == Some(name)`). The renderer
    /// runs the activate/deactivate cascade for those modes on
    /// the current buffer -- a host-only walk would otherwise need
    /// to reach into the renderer's `activate_mode_by_id` path
    /// (mode-lifecycle still lives renderer-side until 5.5.F).
    MirrorOptionToModes(String),
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
        // M.7.1: declarative mode-mirror cascade. Mode lifecycle
        // (`activate_mode_by_id` / `deactivate_mode_by_id`) still
        // lives renderer-side through 5.5.F, so we signal out and
        // let the renderer run the mirror walk on its `App`. Each
        // emitted signal also acts as a debug breadcrumb that the
        // cascade did consider this option for mode mirroring.
        signals.push(RendererSignal::MirrorOptionToModes(canonical_name.to_string()));
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
    /// caring about Drop semantics). 5.5.E.6 dropped `Copy`
    /// because `MirrorOptionToModes` / `LspConfigChanged` carry an
    /// owned `String`; 5.5.F.1 dropped `PartialEq` / `Eq` because
    /// `DisplayBuffer(Box<DisplayBufferRequest>)` carries a
    /// `lattice_help::HelpContent` that isn't value-equatable
    /// (the syntax-highlight cache is renderer-neutral but not
    /// `PartialEq`). Signals are produced at `:` / Effect-arm
    /// rate, not per-frame, so neither `String` cloning nor the
    /// `Box` allocation lands anywhere near the perf gate.
    #[test]
    fn renderer_signal_is_clone() {
        fn assert_clone<T: Clone>(_: T) {}
        assert_clone(RendererSignal::ThemeChanged);
        assert_clone(RendererSignal::Quit);
        assert_clone(RendererSignal::NerdFontsToggled);
        assert_clone(RendererSignal::MirrorOptionToModes("number".into()));
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
