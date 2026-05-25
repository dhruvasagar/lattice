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
    // Phase 5.8.AA.u: workspace-root discovery + persistent-config
    // loading both live on the host so GPUI gets the same boot
    // behaviour. The TUI runtime is now a thin wrapper.
    let workspace_root = lattice_host::editor::Editor::workspace_root_from_cwd();
    app.load_persistent_config(workspace_root.as_deref());
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

// Phase 5.8.AA.u: `workspace_root_from_cwd` migrated to
// `lattice_host::dispatch::Editor::workspace_root_from_cwd`.

// Phase 5.5.LSP.1: the shared LSP runtime + spawn helper now
// live in `lattice_runtime::runtime` so host-side dispatchers
// can fire LSP requests without taking a back-edge through this
// (renderer-specific) crate. Re-exported here so the existing
// `crate::runtime::*` call sites (34 inside `App`) keep
// compiling unchanged. Both names point at the single
// `lattice_runtime::LSP_RUNTIME` OnceLock -- no behaviour change.
// Phase 5.8.AD.2: `lsp_runtime` re-export retired -- last
// caller (`do_lsp_restart`) migrated host-side. `spawn_on_lsp_runtime`
// is still publicly re-exported for the few App-resident async
// helpers that haven't migrated yet.
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

/// Map the App's modal state to a terminal cursor shape via the
/// renderer-neutral `host::cursor_shape::CursorShape`. The vim
/// convention (Block for command-language modes, Bar for Insert /
/// Command-line, Underscore for Replace) lives once on the host
/// (5.8.N); this peer just maps to crossterm's primitive.
fn cursor_style_for(modal: ModalState) -> SetCursorStyle {
    use lattice_host::cursor_shape::CursorShape;
    match CursorShape::for_mode(modal) {
        CursorShape::Block => SetCursorStyle::SteadyBlock,
        CursorShape::Bar => SetCursorStyle::SteadyBar,
        CursorShape::Underline => SetCursorStyle::SteadyUnderScore,
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
    // Slice 3c.final.B (group 6): lifecycle read via published
    // substate. `should_quit` flips from inside dispatch (`:q`,
    // `:wq`, `:qa!`) which republishes at its tail, so the next
    // iteration's load sees the new value.
    // Slice 3c.final.E.4: read through App-cached `render_state`.
    while !app.render_state.load().lifecycle.should_quit {
        // Phase 5.8.AF.5 / Slice X1: `run_tick_pending` no longer
        // runs per-frame here. It moved to `App::apply`'s tail
        // (`crates/lattice-ui-tui/src/app/dispatch.rs`) so the
        // drain happens on the keystroke that caused the work,
        // not in the renderer's per-frame body. Per paramount
        // goal #1, the UI thread does no I/O / event drain; the
        // ~30-channel aggregator we used to call here is exactly
        // the kind of work the spec forbids on this thread.
        //
        // Idle LSP arrivals (response with no keystroke in
        // flight) are pending X1b. See
        // `docs/dev/operations/render-thread-discipline-remediation.md`.
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
        // Slice 3c.final.E.5j: picker / completion popup rows via
        // published `picker_state()` / `completion()` sub-states.
        let extra_rows = app
            .picker_state()
            .state
            .as_deref()
            .map(|p| popup_height_for(p.candidates.len().max(1)))
            .unwrap_or(0)
            .max(
                app.completion()
                    .state
                    .as_deref()
                    .map(|s| popup_height_for(s.candidates.len()))
                    .unwrap_or(0),
            );
        let buffer_height = size
            .height
            .saturating_sub(2)
            .saturating_sub(extra_rows as u16) as u32;
        app.set_viewport_height(app.active_pane_content_height(buffer_height));
        // Slice 3c.final.C: terminal_width via Action.
        app.apply(lattice_host::action::Action::SetTerminalWidth(size.width));
        // `<C-l>` (RedrawScreen) sets `pending_redraw`; honour it
        // by clearing the terminal buffer so the next draw repaints
        // every cell instead of letting ratatui's diff engine
        // assume the previous frame's contents are intact.
        // Slice 3c.final.B (group 6) + 3c.final.C: pending_redraw
        // read via the published substate; acknowledge-write goes
        // through `Action::AcknowledgeRedraw` so the renderer no
        // longer mutates editor state directly. Dispatch tail
        // republishes RS so the next iteration's load observes
        // the cleared flag.
        // Slice 3c.final.E.4: read through App-cached `render_state`.
        if app.render_state.load().lifecycle.pending_redraw {
            terminal.clear().context("clear terminal for redraw")?;
            app.apply(lattice_host::action::Action::AcknowledgeRedraw);
        }
        // Phase 5.8.AF.5 / Slice X2.5: removed
        // `app.refresh_highlights()` from the per-frame body.
        // Active-pane highlights now come from the background
        // highlights worker (`lattice_host::highlights_worker`),
        // which subscribes to `Editor::highlight_wake` and
        // publishes results into
        // `render_state.syntax.visible_spans`. `FrameView::from_app`
        // reads through that cell. Pre-X2 cost: 200–600µs per
        // frame on scroll cache miss (tree-sitter walk on UI
        // thread); post-X2: zero UI-thread parse cost. Goal #1
        // violation B1 closed for the TUI peer.
        app.refresh_pane_highlights();

        // Push the cursor shape only when modal changes -- terminals
        // accept these every frame, but emitting on every iteration adds
        // a few bytes of escape sequence to the stream that isn't free.
        if last_modal != Some(app.ad().modal) {
            execute!(terminal.backend_mut(), cursor_style_for(app.ad().modal))
                .context("set cursor style")?;
            last_modal = Some(app.ad().modal);
        }

        // §5.6.8: one Cache::load per frame for the active document.
        // Steady-state ~300ps when the actor hasn't published since
        // last frame; ~16ns on the first read after a publish.
        // The Arc keeps the snapshot alive for the entire frame --
        // the actor is free to publish concurrently and the new
        // pointer is observed by next frame's load.
        // Slice 3c.final.E.5j: per-frame snapshot read via the
        // published `ad().snapshot` mirror (Arc-bump clone off the
        // same source as `snapshot_cache.load_arc()`).
        let frame_snap = app.ad().snapshot.clone();
        terminal
            .draw(|frame| draw_frame(frame, &app, &frame_snap))
            .context("draw frame")?;

        // 100ms poll keeps the loop responsive to terminal resizes without
        // spinning. We only consume KeyEvents; resizes naturally re-render
        // on the next iteration.
        if event::poll(Duration::from_millis(100)).context("poll events")? {
            match event::read().context("read event")? {
                Event::Key(k) => {
                    // Slice 3c.final.B (group 5): translator
                    // inputs read through `rs.translator` instead
                    // of `&app.editor.{builtins,keymap,partial_chord}`.
                    // The Arc-bound substate keeps the borrows
                    // valid for the duration of the translate call
                    // without tying them to `Editor`'s lifetime —
                    // sets up the slice-E thread split.
                    let ad = app.ad();
                    // Slice 3c.final.E.4: via App-cached render_state.
                    let translator = app.render_state.load().translator.clone();
                    // Investigation 2026-05-22: trace partial_chord
                    // observed by ctx-build. Pairs with the
                    // ABSORB/CLEAR traces in handle_action — the
                    // three together show the full chord-stack
                    // lifecycle across keystrokes.
                    tracing::info!(
                        "[chord-trace] KEY {:?} partial_chord_from_rs={:?}",
                        k.code,
                        translator.partial_chord,
                    );
                    let ctx = TranslateContext {
                        modal: ad.modal,
                        builtins: &translator.builtins,
                        pending_count: ad.pending_count,
                        op_count: ad.op_count,
                        recording_macro: ad.macro_recording,
                        active_buffer: ad.buffer_kind,
                        completion_open: ad.completion_open,
                        chord_capture: app.chord_capture_active(),
                        picker_open: ad.picker_open,
                        insert_completion_open: app.completion_popup_active(),
                        snippet_active: ad.snippet_active,
                        terminal_insert_active: ad.terminal_insert_active,
                        keymap: &translator.keymap,
                        partial_chord: &translator.partial_chord,
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
