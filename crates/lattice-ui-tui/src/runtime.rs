//! Terminal IO loop. Sets up raw mode + alt screen, draws frames, polls
//! events, restores terminal state on exit.
//!
//! This is the only file in the crate that talks to the terminal directly.
//! Everything else is pure and unit-tested.

use std::io::Stdout;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use lattice_grammar::ModalState;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use lattice_core::Document;

use crate::app::{Action, App};
use crate::input::{TranslateContext, translate};
use crate::render::draw_frame;

pub fn run(document: Document) -> Result<()> {
    let mut terminal = setup().context("setup terminal")?;
    let mut app = App::new(document);
    // Load persistent config (TOML) before LSP attach work so
    // any overrides that affect LSP behaviour land before the
    // attach driver processes the initial document's
    // `Event::DocumentOpened`. Workspace root is the CWD
    // walked up to the first `.git` / `.lattice/` marker (or
    // the CWD itself if neither is found). Failures are
    // surfaced via the App's echo, never abort startup.
    let workspace_root = workspace_root_from_cwd();
    app.load_persistent_config(workspace_root.as_deref());
    // Drain `[completion.per-language.<lang>]` sections the
    // loader bucketed -- spec defaults seed the map at App
    // init; TOML overrides layer on top here.
    app.apply_per_language_toml_overrides();
    // LSP attach is event-driven: `App::new` already
    // published `Event::DocumentOpened` for the initial
    // document (if path-bearing). The attach driver wired in
    // `build_lsp_subsystem` runs on the LSP runtime and
    // submits the open to the supervisor *off* the UI thread.
    // The first frame can draw immediately without waiting on
    // the LSP `initialize` round-trip -- paramount goal #4
    // (asynchronicity).
    let result = main_loop(&mut terminal, app);
    teardown(&mut terminal).context("teardown terminal")?;
    result
}

/// Walk up from `std::env::current_dir()` looking for a `.git`
/// directory or a `.lattice/` directory. Returns the first
/// match, or the CWD itself if neither marker is found. The
/// workspace root is the path the project-config TOML lookup
/// (`<root>/.lattice/config.toml`) joins onto. `None` only when
/// the CWD itself is unreadable -- a rare boot failure mode in
/// which case the loader falls back to user config alone.
fn workspace_root_from_cwd() -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let mut cursor = cwd.as_path();
    loop {
        if cursor.join(".git").exists() || cursor.join(".lattice").exists() {
            return Some(cursor.to_path_buf());
        }
        match cursor.parent() {
            Some(parent) => cursor = parent,
            None => return Some(cwd),
        }
    }
}

// Phase 5.5.LSP.1: the shared LSP runtime + spawn helper now
// live in `lattice_runtime::runtime` so host-side dispatchers
// can fire LSP requests without taking a back-edge through this
// (renderer-specific) crate. Re-exported here so the existing
// `crate::runtime::*` call sites (34 inside `App`) keep
// compiling unchanged. Both names point at the single
// `lattice_runtime::LSP_RUNTIME` OnceLock -- no behaviour change.
pub(crate) use lattice_runtime::runtime::lsp_runtime;
pub use lattice_runtime::runtime::spawn_on_lsp_runtime;

fn setup() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen).context("enter alt screen")?;
    // Bracketed paste tells the terminal to wrap clipboard pastes in
    // ESC[200~ ... ESC[201~ markers. Crossterm decodes those as
    // `Event::Paste(String)`, which we hand to the App as a single
    // bracketed-paste burst (one undo unit). Without this, terminals
    // that bind Ctrl+V to clipboard paste (Konsole, Windows Terminal,
    // tmux configs) replay the clipboard contents as a stream of
    // raw key events -- which Normal mode then interprets as commands
    // and the user gets unexpected behaviour instead of a paste.
    execute!(stdout, EnableBracketedPaste).context("enable bracketed paste")?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).context("create terminal")
}

fn teardown(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    // Restore the user's default cursor shape before tearing down the
    // alt screen -- otherwise the shell prompt inherits whatever the
    // editor was rendering.
    execute!(terminal.backend_mut(), SetCursorStyle::DefaultUserShape)
        .context("restore cursor style")?;
    execute!(terminal.backend_mut(), DisableBracketedPaste).context("disable bracketed paste")?;
    disable_raw_mode().context("disable raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen).context("leave alt screen")?;
    terminal.show_cursor().context("show cursor")?;
    Ok(())
}

/// Map the App's modal state to a terminal cursor shape. Block for
/// command-language modes (Normal, Visual, Operator-Pending), bar for
/// Insert (matches the pre-modal feel users expect when typing text),
/// underscore for Replace (vim convention), bar for the prompt rows
/// (Command / Search) since the cursor lives in the bottom row there.
fn cursor_style_for(modal: ModalState) -> SetCursorStyle {
    match modal {
        ModalState::Normal | ModalState::Visual(_) | ModalState::OperatorPending => {
            SetCursorStyle::SteadyBlock
        }
        ModalState::Insert => SetCursorStyle::SteadyBar,
        ModalState::Replace => SetCursorStyle::SteadyUnderScore,
        ModalState::Command | ModalState::Search(_) => SetCursorStyle::SteadyBar,
    }
}

/// Mirror of the renderer's `popup_height` so the runtime can
/// subtract the candidate-list rows from the buffer-area height
/// before computing the active pane's viewport. Kept in sync by
/// hand for now; if either drifts the cursor / scroll math
/// goes off by `extra_rows` in pickers / completion. See
/// `render::popup_height` for the canonical formula.
fn popup_height_for(candidate_count: usize) -> usize {
    const MAX_ROWS: usize = 10;
    candidate_count.min(MAX_ROWS).max(1)
}

fn main_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, mut app: App) -> Result<()> {
    let mut last_modal: Option<ModalState> = None;
    while !app.editor.should_quit {
        // No queue-and-drain step for LSP opens any more --
        // `Event::DocumentOpened` flows directly from the
        // publisher (`App::new` / `App::do_edit`) into the
        // attach driver task on the LSP runtime. The UI thread
        // never parks on the LSP `initialize` round-trip.
        //
        // Drain queued `Event::OptionChanged` cascades (DESIGN.md
        // §5.10 + §5.12). Backstop for option writes that happened
        // outside a keystroke -- e.g. plugin tasks, future
        // LSP-driven config writes. The cmdline path's `do_set`
        // already drains synchronously after each `:set`, so the
        // common case here is a no-op pull on an empty channel.
        app.drain_option_changes();
        // Drain queued LSP hover responses (Phase 4.2.b). The K
        // keystroke spawns a request on the LSP runtime; the
        // response flows back through `App.pending_hover_rx` and
        // surfaces here, before the next draw, via the existing
        // hover popup. Cheap on an idle channel.
        app.drain_pending_hover();
        // Drain queued LSP goto-definition responses (Phase 4.2.c).
        // `gd` spawns a request; single-result jumps in-place,
        // multi-result echoes a count + jumps to first.
        app.drain_pending_definitions();
        // Drain queued LSP references responses (Phase 4.2.d).
        // `gr` spawns a request; the merged + deduped list opens
        // as a vertico picker.
        app.drain_pending_references();
        // Drain queued documentSymbol / workspaceSymbol responses
        // (Phase 4.2.e / 4.2.f). Both surfaces share one channel
        // since there's never more than one symbol request in
        // flight; a follow-up `:lsp-symbols` cancels its
        // predecessor.
        app.drain_pending_symbols();
        // Drain queued formatting responses (Phase 4.3). Applies
        // the returned edits as one undo unit; echoes when no
        // provider was found or no changes were needed.
        app.drain_pending_format();
        // Drain queued signatureHelp responses (Phase 4.3).
        // Renders the active signature into the hover popup
        // pipeline.
        app.drain_pending_signature_help();
        // Drain queued completion responses (Phase 4.2.g).
        // Opens a picker over the merged item list.
        app.drain_pending_completion();
        // Drain queued rename responses (Phase 4.3). Applies
        // the WorkspaceEdit per-file (one undo unit per
        // affected buffer in v1) and echoes a summary.
        app.drain_pending_rename();
        // Drain queued code-action responses (Phase 4.3).
        // Items open a picker; resolve responses (post-accept)
        // apply directly.
        app.drain_pending_code_actions();
        // Drain queued LSP insert-completion responses (Phase
        // 4.2.g.2). Merge into the active popup's `raw` set
        // and refilter; close the popup if everything dropped.
        app.drain_pending_insert_completion_lsp();
        // Drain queued completionItem/resolve responses
        // (Phase 4.2.g.3). Update the focused candidate's
        // metadata + the docs popup body in place.
        app.drain_pending_completion_resolve();
        // Drain async picker init futures (slice 14d). When a
        // source returns `PickerInitResult::Future`, the
        // resolved batch lands here; the picker seats with the
        // results, same code path as inline. Cheap on an idle
        // channel (try_recv -> Empty).
        app.drain_pending_picker_init();
        // Drain live-picker debounce + in-flight (slice 2).
        // Fires `on_query_changed` when the debounce elapses
        // and seats new candidates when the source's future
        // resolves. Cheap on an idle live picker (single
        // `Instant::now()` compare, no allocations).
        app.drain_pending_live_picker_query();
        // Drain queued Event::LspLogPushed events (Phase 4) and
        // refresh any open log / trace buffers from the logger
        // snapshot. Cheap on an idle channel; cheap when no log
        // buffer is open (the refresh path short-circuits on
        // missing-by-title).
        app.drain_lsp_log_events();
        // Drain queued `LspProgressUpdate` events (4.4.c) into
        // the App's progress accumulator so the modeline shows
        // the freshest state on the next render tick.
        app.drain_lsp_progress_events();
        // `LspBufferDetached` subscriber: `LspMode::on_deactivate`
        // publishes the event; this drain calls
        // `lsp_close_buffer` per event so the wire-level
        // `didClose` runs after the mode lifecycle without
        // the App's `deactivate_mode_by_id` knowing anything
        // about `lsp-mode`.
        app.drain_lsp_detach_events();
        // M-async.3 rollback drain: the mode dispatcher's
        // spawned lifecycle task publishes `ModeEvent::
        // ModeActivationFailed` when `on_activate.await`
        // returns `Err` (or when a cascade abort marks a
        // child as unrun); the App rolls back `active_modes`
        // + `mode_guards` for each.
        app.drain_mode_lifecycle_events();
        // Drain server-initiated `workspace/applyEdit` requests
        // (Phase 4.3). Each is applied via the existing
        // workspace-edit flatten + per-file batch path, then
        // the LSP response (`applied`/`failure_reason`) ferries
        // back to the originating server through the embedded
        // oneshot.
        app.drain_inbound_apply_edits();
        // Drain server-initiated `workspace/configuration`
        // requests (Phase 4.1 follow-up). Walk each requested
        // section in the cached TOML tree at `lsp.<section>`
        // and reply with the per-section value.
        app.drain_inbound_configuration_requests();
        // 4.4.l.2: refresh the file-watcher subscription set +
        // drain any fs events the watcher emitted since the
        // last tick. Refresh is cheap when nothing changed
        // (per-server fingerprint short-circuit); the drain
        // fans out `workspace/didChangeWatchedFiles` per server
        // for matching events.
        app.refresh_lsp_file_watcher();
        app.drain_lsp_fs_events();
        // 4.5.g: drain pending `:lsp-moniker` response (if any)
        // and echo the summary line. Cheap when the channel is
        // empty; no-op when no request is in flight.
        app.drain_pending_moniker();
        // 4.5.c: per-tick documentLink pump + drain. The pump
        // fires on document-version change (cheap short-circuit
        // otherwise); the drain seats fresh link ranges into
        // the per-buffer cache that `gx` consults.
        app.maybe_request_document_link();
        app.drain_pending_document_link();
        // 4.5.d: codeLens refresh + pump + drain. The refresh
        // drain evicts cached lenses for servers that emitted
        // `workspace/codeLens/refresh` (so the next pump
        // refetches); the pump fires on doc-version change OR
        // a fresh eviction; the drain seats the response into
        // the per-buffer cache that `:lsp-code-lens` reads.
        app.drain_code_lens_refresh();
        app.maybe_request_code_lens();
        app.drain_pending_code_lens();
        // 4.5.e: documentColor pump + drain. Per-tick pump on
        // doc-version change caches color literal ranges; the
        // cache feeds `:lsp-color-presentation` (and a future
        // renderer swatch overlay).
        app.maybe_request_document_color();
        app.drain_pending_document_color();
        // 4.4.b: server-initiated window/showDocument requests
        // (open URI in buffer / external handler).
        app.drain_inbound_show_documents();
        // 4.4.b: server-initiated window/showMessageRequest
        // (modal action picker; user's selection ferries back).
        app.drain_inbound_show_message_requests();
        // 4.4.e: outstanding selectionRange response, if any
        // expand/shrink invocation hit the wire path. Seats
        // the chain + applies the step.
        app.drain_pending_selection_range();
        // 4.4.e: documentHighlight pump. Fires a fresh
        // request when the cursor has moved off the last
        // issue point; cancels any in-flight predecessor.
        // The drain consumes the most recent response and
        // seats the cache for the renderer overlay.
        app.maybe_request_document_highlight();
        app.drain_pending_document_highlight();
        // 4.4.f: foldingRange pump. Only fires when
        // `:set foldmethod=lsp` is active AND the buffer's
        // document version has changed; the drain seats the
        // cache and triggers `recompute_folds`.
        app.maybe_request_folding_range();
        app.drain_pending_folding_range();
        // 4.4.g: server-initiated inlay-hint refresh. Drain
        // before the pump so a refresh that arrived this tick
        // invalidates the cache before the pump checks it
        // (otherwise the pump sees the cache as up-to-date for
        // the current doc version and skips the re-fetch).
        app.drain_inlay_hint_refresh();
        // 4.4.g: inlayHint pump. Fires when
        // `lsp-inlay-hint-mode` is on AND the buffer's
        // document version has changed; the renderer overlay
        // splices each hint as virtual text mid-line.
        app.maybe_request_inlay_hint();
        app.drain_pending_inlay_hint();
        // `*messages*` live tail: coalesce queued
        // MessagePushed events and rebuild the buffer view
        // once per tick. Cheap (rebuild only fires when at
        // least one event landed AND the buffer is open).
        app.drain_message_events();
        // 4.4.j: server-initiated pull-diagnostic refresh.
        // Drained ahead of the pull pump so a refresh this
        // tick evicts the cache before the pump's version
        // check sees it as fresh.
        app.drain_diagnostic_refresh();
        // 4.4.j: textDocument/diagnostic pull pump. Fires when
        // `lsp-diagnostics-mode` is on AND the active server
        // advertises pull AND the document version changed.
        // The server can answer either `Full` (apply to the
        // layer same as push) or `Unchanged` (no-op, just
        // refresh the cached result_id). Pump is single-
        // flight: each new request cancels its predecessor.
        app.maybe_request_pull_diagnostics();
        app.drain_pending_pull_diagnostics();
        // 4.4.i: server-initiated semantic-tokens refresh.
        // Drained before the pump for the same reason as the
        // inlay-hint refresh -- a refresh this tick must
        // invalidate the cache before the pump's version check
        // sees it as fresh.
        app.drain_semantic_tokens_refresh();
        // 4.4.h: semanticTokens/full pump. Fires when
        // `lsp-semantic-tokens-mode` is on AND the document
        // version has changed; the renderer overlay overrides
        // tree-sitter styling within each token's byte range.
        // 4.4.i: when the cache has a `result_id` and the server
        // advertises delta support, the pump issues
        // `semanticTokens/full/delta` and the drain splices the
        // returned edit script into the cached raw vec.
        app.maybe_request_semantic_tokens();
        app.drain_pending_semantic_tokens();
        // Update viewport height. The buffer-area band is the
        // terminal minus the mode line + cmdline/echo row (and the
        // candidate-list row band, when a picker / completion popup
        // is up). Within that band, the active *pane* gets only its
        // share -- horizontal/vertical splits divide the area, and
        // multi-pane layouts reserve the bottom row of each pane
        // for its status line. The viewport must reflect the active
        // pane's content height, not the full buffer band, so
        // motions / scroll / cursor visibility agree with what the
        // renderer actually paints.
        let size = terminal.size().context("query terminal size")?;
        let extra_rows = app
            .editor.picker
            .as_ref()
            .map(|p| popup_height_for(p.candidates.len().max(1)))
            .unwrap_or(0)
            .max(
                app.editor.completion_state
                    .as_ref()
                    .map(|s| popup_height_for(s.candidates.len()))
                    .unwrap_or(0),
            );
        let buffer_height = size
            .height
            .saturating_sub(2)
            .saturating_sub(extra_rows as u16) as u32;
        app.set_viewport_height(app.active_pane_content_height(buffer_height));
        app.editor.terminal_width = Some(size.width);
        // `<C-l>` (RedrawScreen) sets `pending_redraw`; honour it
        // by clearing the terminal buffer so the next draw repaints
        // every cell instead of letting ratatui's diff engine
        // assume the previous frame's contents are intact.
        if app.editor.pending_redraw {
            terminal.clear().context("clear terminal for redraw")?;
            app.editor.pending_redraw = false;
        }
        app.refresh_highlights();
        app.refresh_pane_highlights();

        // Push the cursor shape only when modal changes -- terminals
        // accept these every frame, but emitting on every iteration adds
        // a few bytes of escape sequence to the stream that isn't free.
        if last_modal != Some(app.editor.modal) {
            execute!(terminal.backend_mut(), cursor_style_for(app.editor.modal))
                .context("set cursor style")?;
            last_modal = Some(app.editor.modal);
        }

        // §5.6.8: one Cache::load per frame for the active document.
        // Steady-state ~300ps when the actor hasn't published since
        // last frame; ~16ns on the first read after a publish.
        // The Arc keeps the snapshot alive for the entire frame --
        // the actor is free to publish concurrently and the new
        // pointer is observed by next frame's load.
        let frame_snap = app.editor.snapshot_cache.load_arc();
        terminal
            .draw(|frame| draw_frame(frame, &app, &frame_snap))
            .context("draw frame")?;

        // 100ms poll keeps the loop responsive to terminal resizes without
        // spinning. We only consume KeyEvents; resizes naturally re-render
        // on the next iteration.
        if event::poll(Duration::from_millis(100)).context("poll events")? {
            match event::read().context("read event")? {
                Event::Key(k) => {
                    let ctx = TranslateContext {
                        modal: app.editor.modal,
                        builtins: &app.editor.builtins,
                        pending_count: app.editor.pending_count,
                        op_count: app.editor.op_count,
                        recording_macro: app.editor.macro_recording.is_some(),
                        active_buffer: app.editor.active_buffer,
                        completion_open: app.editor.completion_state.is_some(),
                        chord_capture: app.chord_capture_active(),
                        picker_open: app.editor.picker.is_some(),
                        insert_completion_open: app.completion_popup_active(),
                        snippet_active: app.editor.active_snippet.is_some(),
                        keymap: &app.editor.keymap,
                        partial_chord: &app.editor.partial_chord,
                    };
                    let action = translate(ctx, k);
                    app.apply(action);
                }
                Event::Paste(text) => {
                    // Real bracketed-paste burst from the terminal's
                    // clipboard shortcut. Hand the payload to the app
                    // as a single edit; Ctrl+V keystrokes (the binding
                    // for blockwise visual) still arrive as Event::Key
                    // because they're not the terminal's paste path.
                    app.apply(Action::PasteText(text));
                }
                Event::Resize(_, _) => {
                    // next iteration handles the new size
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_grammar::{SearchDirection, VisualKind};

    #[test]
    fn normal_mode_uses_block_cursor() {
        assert!(matches!(
            cursor_style_for(ModalState::Normal),
            SetCursorStyle::SteadyBlock
        ));
    }

    #[test]
    fn insert_mode_uses_bar_cursor() {
        assert!(matches!(
            cursor_style_for(ModalState::Insert),
            SetCursorStyle::SteadyBar
        ));
    }

    #[test]
    fn visual_charwise_uses_block_cursor() {
        assert!(matches!(
            cursor_style_for(ModalState::Visual(VisualKind::Charwise)),
            SetCursorStyle::SteadyBlock
        ));
    }

    #[test]
    fn visual_linewise_uses_block_cursor() {
        assert!(matches!(
            cursor_style_for(ModalState::Visual(VisualKind::Linewise)),
            SetCursorStyle::SteadyBlock
        ));
    }

    #[test]
    fn operator_pending_uses_block_cursor() {
        assert!(matches!(
            cursor_style_for(ModalState::OperatorPending),
            SetCursorStyle::SteadyBlock
        ));
    }

    #[test]
    fn replace_mode_uses_underscore_cursor() {
        assert!(matches!(
            cursor_style_for(ModalState::Replace),
            SetCursorStyle::SteadyUnderScore
        ));
    }

    #[test]
    fn command_mode_uses_bar_cursor() {
        assert!(matches!(
            cursor_style_for(ModalState::Command),
            SetCursorStyle::SteadyBar
        ));
    }

    #[test]
    fn search_mode_uses_bar_cursor() {
        assert!(matches!(
            cursor_style_for(ModalState::Search(SearchDirection::Forward)),
            SetCursorStyle::SteadyBar
        ));
        assert!(matches!(
            cursor_style_for(ModalState::Search(SearchDirection::Backward)),
            SetCursorStyle::SteadyBar
        ));
    }
}
