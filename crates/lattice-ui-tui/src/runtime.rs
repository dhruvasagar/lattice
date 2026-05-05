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
    // LSP boot. Spawns matching language servers for the
    // initial document (if it has a path) + attaches them.
    // Async because the LSP handshake awaits an `initialize`
    // response. Failure is logged through the supervisor's
    // logger -- never blocks editor startup.
    initialize_lsp_blocking(&mut app);
    let result = main_loop(&mut terminal, app);
    teardown(&mut terminal).context("teardown terminal")?;
    result
}

/// Drive [`App::initialize_lsp`] from the synchronous `run`
/// entry point. We're not yet inside a tokio runtime here; the
/// editor's main loop is a sync `crossterm::event::poll` loop
/// that uses [`lattice_runtime::block_on`] for its async
/// boundaries. For LSP boot we need a full tokio context (the
/// supervisor spawns actor tasks), so we stand up a multi-thread
/// runtime, drive the boot future to completion, and let the
/// runtime live as a background reactor for the actor tasks.
fn initialize_lsp_blocking(app: &mut App) {
    let rt = lsp_runtime();
    rt.block_on(app.initialize_lsp());
}

/// Drive [`App::drain_pending_lsp_opens`] from the synchronous
/// main loop. Reuses the same boot runtime so spawned actor
/// tasks share one reactor. Cheap when the queue is empty;
/// invoked once per main-loop iteration.
fn drain_pending_lsp_opens_blocking(app: &mut App) {
    let rt = lsp_runtime();
    rt.block_on(app.drain_pending_lsp_opens());
}

/// Shared tokio multi-thread runtime that hosts every LSP
/// task (actor + read/write loops + diagnostic pumps + the
/// debounced flush task). Survives for the editor's lifetime.
fn lsp_runtime() -> &'static tokio::runtime::Runtime {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("lattice-lsp")
            .build()
            .expect("LSP tokio runtime should build")
    })
}

/// Spawn a fire-and-forget future on the shared LSP runtime
/// (Phase 4.2). Used by the App's per-feature dispatchers
/// (hover, definition, references, ...) so the request awaits
/// the actor's response *off* the main UI thread; the result
/// flows back through a per-feature mpsc channel that the App
/// drains before each draw.
///
/// Returning a `JoinHandle` lets the caller cancel by dropping
/// it -- though for LSP cooperative cancellation runs through
/// the `CancellationToken` plumbed into the typed wrappers,
/// so the handle is mostly informational.
pub fn spawn_on_lsp_runtime<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    lsp_runtime().spawn(future)
}

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
    while !app.should_quit {
        // Drain pending LSP opens (Phase 4.1.i.2). `:e <path>`
        // queues; we attach asynchronously on the boot runtime
        // so the input thread doesn't await the actor handshake.
        // Cheap when the queue is empty (the common case).
        if !app.pending_lsp_opens.is_empty() {
            drain_pending_lsp_opens_blocking(&mut app);
        }
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
        // Drain queued Event::LspLogPushed events (Phase 4) and
        // refresh any open log / trace buffers from the logger
        // snapshot. Cheap on an idle channel; cheap when no log
        // buffer is open (the refresh path short-circuits on
        // missing-by-title).
        app.drain_lsp_log_events();
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
            .picker
            .as_ref()
            .map(|p| popup_height_for(p.candidates.len().max(1)))
            .unwrap_or(0)
            .max(
                app.completion_state
                    .as_ref()
                    .map(|s| popup_height_for(s.candidates.len()))
                    .unwrap_or(0),
            );
        let buffer_height = size
            .height
            .saturating_sub(2)
            .saturating_sub(extra_rows as u16) as u32;
        app.set_viewport_height(app.active_pane_content_height(buffer_height));
        app.terminal_width = Some(size.width);
        // `<C-l>` (RedrawScreen) sets `pending_redraw`; honour it
        // by clearing the terminal buffer so the next draw repaints
        // every cell instead of letting ratatui's diff engine
        // assume the previous frame's contents are intact.
        if app.pending_redraw {
            terminal.clear().context("clear terminal for redraw")?;
            app.pending_redraw = false;
        }
        app.refresh_highlights();
        app.refresh_pane_highlights();

        // Push the cursor shape only when modal changes -- terminals
        // accept these every frame, but emitting on every iteration adds
        // a few bytes of escape sequence to the stream that isn't free.
        if last_modal != Some(app.modal) {
            execute!(terminal.backend_mut(), cursor_style_for(app.modal))
                .context("set cursor style")?;
            last_modal = Some(app.modal);
        }

        // §5.6.8: one Cache::load per frame for the active document.
        // Steady-state ~300ps when the actor hasn't published since
        // last frame; ~16ns on the first read after a publish.
        // The Arc keeps the snapshot alive for the entire frame --
        // the actor is free to publish concurrently and the new
        // pointer is observed by next frame's load.
        let frame_snap = app.snapshot_cache.load_arc();
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
                        modal: app.modal,
                        pending: app.pending,
                        builtins: &app.builtins,
                        pending_count: app.pending_count,
                        recording_macro: app.macro_recording.is_some(),
                        active_buffer: app.active_buffer,
                        completion_open: app.completion_state.is_some(),
                        chord_capture: app.chord_capture_active(),
                        picker_open: app.picker.is_some(),
                        insert_completion_open: app.insert_completion.is_some(),
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
