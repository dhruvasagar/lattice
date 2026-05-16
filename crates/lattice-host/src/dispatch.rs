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
//!   on `ui.*` keys).
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
use lattice_protocol::Event;
use lattice_runtime::MessagePushed;

use crate::action::{Action, EchoLevel};
use crate::buffers::BufferId;
use crate::editor::Editor;
use crate::state::SearchLine;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererSignal {
    /// The host's neutral [`crate::ui::theme::Theme`] changed
    /// (typically via a `:set ui.*` cascade). The renderer should
    /// rebuild its cached typed theme mirror.
    ThemeChanged,
    /// Quit requested. The renderer should begin its shutdown
    /// sequence. `editor.should_quit` is also set for back-compat
    /// with renderers that poll per-tick.
    Quit,
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

    /// `RendererSignal` is `Copy + Eq` so renderers can match on it
    /// without cloning and dedupe a signal list cheaply. Pinning the
    /// derives now keeps a future contributor from adding a non-Copy
    /// variant without thinking about hot-path call sites.
    #[test]
    fn renderer_signal_is_copy_eq() {
        fn assert_copy_eq<T: Copy + Eq>(_: T) {}
        assert_copy_eq(RendererSignal::ThemeChanged);
        assert_copy_eq(RendererSignal::Quit);
        assert_eq!(RendererSignal::Quit, RendererSignal::Quit);
        assert_ne!(RendererSignal::Quit, RendererSignal::ThemeChanged);
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
}
