//! Terminal IO loop. Sets up raw mode + alt screen, draws frames, polls
//! events, restores terminal state on exit.
//!
//! This is the only file in the crate that talks to the terminal directly.
//! Everything else is pure and unit-tested.

use std::io::Stdout;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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

/// Perf instrumentation (paired with `LATTICE_PERF_INPUT`): counts bytes
/// written to the terminal so the input timer can report **per-frame write
/// volume**. This is the real driver of terminal present time on a slow pty:
/// `terminal.draw()` returns as soon as the diff lands in the pty buffer, but
/// the terminal emulator then spends time parsing + presenting those bytes —
/// time our `input→glyph` timer can't see. vim writes a few bytes per
/// keystroke; if we write kilobytes (a whole-viewport rewrite) the same
/// terminal that renders vim instantly will visibly lag on us. Reset before
/// each `draw()`, read after.
static PERF_FRAME_BYTES: AtomicU64 = AtomicU64::new(0);

/// Wraps the terminal writer and tallies bytes into [`PERF_FRAME_BYTES`].
/// Forwards everything to the inner writer; the relaxed atomic add is
/// negligible. Always wired (so the byte count is available whenever
/// `LATTICE_PERF_INPUT` is set) — the only cost when the env is unset is the
/// add itself, which is in the noise next to the syscall it accompanies.
struct CountingWriter<W> {
    inner: W,
}

impl<W: std::io::Write> std::io::Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        PERF_FRAME_BYTES.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// The concrete terminal backend type, now wrapped in the byte counter.
type TermBackend = CrosstermBackend<CountingWriter<Stdout>>;

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

fn setup() -> Result<Terminal<TermBackend>> {
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
    let backend = CrosstermBackend::new(CountingWriter { inner: stdout });
    Terminal::new(backend).context("create terminal")
}

fn teardown(terminal: &mut Terminal<TermBackend>) -> Result<()> {
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
///
/// Terminal-mode T2.b (2026-05-25): when the active buffer is a
/// Terminal AND `terminal-insert-mode` is active, the shape
/// flips to `SteadyBar` even though `ModalState` stays `Normal`
/// (TerminalInsert is a minor mode, not a separate modal
/// variant). The Normal-in-terminal state keeps `SteadyBlock`.
fn cursor_style_for(modal: ModalState, terminal_insert_active: bool) -> SetCursorStyle {
    use lattice_host::cursor_shape::CursorShape;
    if terminal_insert_active {
        return SetCursorStyle::SteadyBar;
    }
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

fn main_loop(terminal: &mut Terminal<TermBackend>, mut app: App) -> Result<()> {
    // Terminal-mode T2.b (2026-05-25): cursor-style cache now keys
    // off `(modal, terminal_insert_active)` since TerminalInsert is
    // a minor mode that doesn't flip `ModalState`. Without the
    // minor-mode bit in the cache key, entering / leaving
    // Terminal-Insert wouldn't re-push the cursor style and the
    // user would see a stale block where a bar belongs.
    let mut last_cursor_inputs: Option<(ModalState, bool)> = None;
    // Diff cache for the per-frame terminal-width dispatch (see loop body):
    // the width only changes on a resize, so we dispatch only on change
    // rather than every frame.
    let mut last_terminal_width: Option<u16> = None;
    // Diff cache for the per-frame viewport-height push. `set_viewport_height`
    // was dispatched UNCONDITIONALLY every iteration → a publish (+ both worker
    // wakes) per loop tick even when nothing changed — the idle/per-keystroke
    // publish storm. Push only when the resolved height actually changes.
    let mut last_viewport_height: Option<u32> = None;
    // Opt-in keystroke→glyph timer (set env `LATTICE_PERF_INPUT=1`). Measures
    // OUR input-to-draw latency only — from the instant an input event was
    // applied to the instant `terminal.draw()` returns (i.e. we've written the
    // frame's diff to stdout). It does NOT include the terminal emulator /
    // pty / compositor present time, which on WSL2 is outside our process.
    // So: a tiny number here + felt lag ⇒ the lag is the WSL2 terminal, not us;
    // a large number here ⇒ the lag is our draw. Writes one clean line per
    // rendered keystroke straight to stderr (bypasses tracing, so it does not
    // add to the debug-log flood). Off by default — zero cost when unset.
    let perf_input = std::env::var_os("LATTICE_PERF_INPUT").is_some();
    let mut last_input_at: Option<Instant> = None;
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
        // Diff-then-push: only dispatch when the resolved viewport height
        // changed. Unconditional dispatch here was publishing (+ waking both
        // cells/virtual-rows workers) every loop iteration.
        let vh = app.active_pane_content_height(buffer_height);
        if last_viewport_height != Some(vh) {
            app.set_viewport_height(vh);
            last_viewport_height = Some(vh);
        }
        // 2026-05-27: fire `set_pane_viewport(idx, rows, cols)` per
        // leaf so the host writes the per-pane geometry onto
        // `PaneState` AND, for terminal panes, resizes alacritty +
        // PTY to match the allocated area. Without this, terminal
        // grids stayed at their spawn-time 80×24 default and never
        // wrapped to the actual TUI pane width — the GPUI peer
        // had this loop in its render path but the TUI never did.
        //
        // Diff-then-send: only send when the leaf's published
        // viewport differs from what compute_rects computed this
        // frame. Steady-state (no resize) fires zero commands.
        let panes_arc = app.panes();
        let area_for_panes = crate::pane::PaneRect {
            x: 0,
            y: 0,
            width: size.width,
            height: buffer_height as u16,
        };
        let rects = panes_arc.tree.compute_rects(area_for_panes);
        let multi = rects.len() > 1;
        let current_leaves: Vec<_> = panes_arc.tree.leaves().to_vec();
        for (idx, prect) in &rects {
            // Reserve a bottom row for the per-pane status line in
            // multi-pane setups so the host's viewport matches
            // what's actually drawn (the renderer carves the same
            // row off in `draw_panes`).
            let content_h = if multi && prect.height >= 2 {
                prect.height - 1
            } else {
                prect.height
            };
            let rows = u32::from(content_h).max(1);
            let cols = u32::from(prect.width).max(1);
            let needs = current_leaves
                .get(*idx)
                .map(|l| l.viewport_height != rows || l.viewport_width != cols)
                .unwrap_or(true);
            if needs {
                app.set_pane_viewport(*idx, rows, cols);
            }
        }
        // Slice 3c.final.C: terminal_width via Action. Diff-then-send
        // (mirrors the pane-viewport loop above): the width only changes
        // on a terminal resize, so dispatching every frame meant a blocking
        // actor RPC + a full `publish_render_state` on every frame (hence on
        // every keystroke's draw) for a no-op field write. Cache the
        // last-sent width UI-side; dispatch only on change.
        if last_terminal_width != Some(size.width) {
            app.apply(lattice_host::action::Action::SetTerminalWidth(size.width));
            last_terminal_width = Some(size.width);
        }
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
        // B4: disabled. This per-frame UI-thread `highlight_lines` recompute
        // (itself a goal-#1 violation) only fed inactive-pane `pane_highlights`;
        // the active body now reads the canonical `DisplayMatrix`. B4 migrates
        // the inactive-pane consumer onto DisplayMatrix, then deletes this.
        // app.refresh_pane_highlights();

        // Push the cursor shape only when the inputs change --
        // terminals accept these every frame, but emitting on every
        // iteration adds a few bytes of escape sequence to the
        // stream that isn't free. Cache key includes both `modal`
        // and `terminal_insert_active` so the bar / block flip on
        // entering / leaving TerminalInsert re-pushes.
        let cursor_inputs = (app.ad().modal, app.ad().terminal_insert_active);
        if last_cursor_inputs != Some(cursor_inputs) {
            execute!(
                terminal.backend_mut(),
                cursor_style_for(cursor_inputs.0, cursor_inputs.1)
            )
            .context("set cursor style")?;
            last_cursor_inputs = Some(cursor_inputs);
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
        let draw_t0 = if perf_input {
            // Reset the per-frame byte tally so we count only THIS draw's
            // write (the cursor-style execute! above already happened).
            PERF_FRAME_BYTES.store(0, Ordering::Relaxed);
            Some(Instant::now())
        } else {
            None
        };
        terminal
            .draw(|frame| draw_frame(frame, &app, &frame_snap))
            .context("draw frame")?;
        // Perf timer: this draw is the one that renders the keystroke applied
        // at the end of the PREVIOUS iteration. Report input→glyph (our side),
        // the bare draw duration, AND the bytes written to the terminal — the
        // last is the real driver of present time on a slow pty (vim writes a
        // handful; a whole-viewport rewrite is kilobytes). Clear so we only log
        // frames that actually rendered fresh input (not async repaints).
        if perf_input {
            if let (Some(input_at), Some(t0)) = (last_input_at.take(), draw_t0) {
                let done = Instant::now();
                eprintln!(
                    "[perf] input→glyph {:>7.3}ms  (draw {:>7.3}ms, {} bytes)",
                    done.duration_since(input_at).as_secs_f64() * 1e3,
                    done.duration_since(t0).as_secs_f64() * 1e3,
                    PERF_FRAME_BYTES.load(Ordering::Relaxed),
                );
            }
        }

        // Slice I.1 — input coalescing. The 100ms poll waits for the FIRST
        // event (keeps the loop responsive to resizes without spinning;
        // Slice I.3 makes this fully event-driven). Once an event is ready
        // we drain EVERY currently-buffered event before looping back to
        // draw: a typing burst of N queued keys collapses to N cheap
        // `apply`s + ONE draw instead of N full draw cycles, so the
        // displayed text never trails the keystrokes (the keystroke UX
        // contract). The translate context is rebuilt per event because
        // applying one event can change the modal state / mode stack that
        // governs how the next event translates. See
        // docs/dev/architecture/input-pipeline.md.
        if event::poll(Duration::from_millis(100)).context("poll events")? {
            loop {
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
                            terminal_esc_exits: ad.terminal_esc_exits,
                            terminal_app_cursor_keys: ad.terminal_app_cursor_keys,
                            terminal_insert_exit_pending: ad.terminal_insert_exit_pending,
                            terminal_visual_active: ad.terminal_visual_active,
                            keymap: &translator.keymap,
                            partial_chord: &translator.partial_chord,
                            active_minor_modes: &translator.active_minor_modes,
                        };
                        let action = translate(ctx, k);
                        app.apply(action);
                        if perf_input {
                            last_input_at = Some(Instant::now());
                        }
                    }
                    Event::Paste(text) => {
                        // Real bracketed-paste burst from the terminal's
                        // clipboard shortcut. Hand the payload to the app
                        // as a single edit; Ctrl+V keystrokes (the binding
                        // for blockwise visual) still arrive as Event::Key
                        // because they're not the terminal's paste path.
                        app.apply(Action::PasteText(text));
                        if perf_input {
                            last_input_at = Some(Instant::now());
                        }
                    }
                    Event::Resize(_, _) => {
                        // next iteration handles the new size
                    }
                    _ => {}
                }
                // Stop draining the instant a quit lands: don't apply later
                // buffered keys against a tearing-down app, and let the outer
                // `while !should_quit` exit without a final draw.
                if app.render_state.load().lifecycle.should_quit {
                    break;
                }
                // Drain only what is ALREADY buffered (zero-timeout poll).
                // When the queue empties, fall through to a single redraw
                // showing the coalesced final state.
                if !event::poll(Duration::ZERO).context("poll events")? {
                    break;
                }
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
            cursor_style_for(ModalState::Normal, false),
            SetCursorStyle::SteadyBlock
        ));
    }

    #[test]
    fn insert_mode_uses_bar_cursor() {
        assert!(matches!(
            cursor_style_for(ModalState::Insert, false),
            SetCursorStyle::SteadyBar
        ));
    }

    #[test]
    fn visual_charwise_uses_block_cursor() {
        assert!(matches!(
            cursor_style_for(ModalState::Visual(VisualKind::Charwise), false),
            SetCursorStyle::SteadyBlock
        ));
    }

    #[test]
    fn visual_linewise_uses_block_cursor() {
        assert!(matches!(
            cursor_style_for(ModalState::Visual(VisualKind::Linewise), false),
            SetCursorStyle::SteadyBlock
        ));
    }

    #[test]
    fn operator_pending_uses_block_cursor() {
        assert!(matches!(
            cursor_style_for(ModalState::OperatorPending, false),
            SetCursorStyle::SteadyBlock
        ));
    }

    #[test]
    fn replace_mode_uses_underscore_cursor() {
        assert!(matches!(
            cursor_style_for(ModalState::Replace, false),
            SetCursorStyle::SteadyUnderScore
        ));
    }

    #[test]
    fn command_mode_uses_bar_cursor() {
        assert!(matches!(
            cursor_style_for(ModalState::Command, false),
            SetCursorStyle::SteadyBar
        ));
    }

    #[test]
    fn search_mode_uses_bar_cursor() {
        assert!(matches!(
            cursor_style_for(ModalState::Search(SearchDirection::Forward), false),
            SetCursorStyle::SteadyBar
        ));
        assert!(matches!(
            cursor_style_for(ModalState::Search(SearchDirection::Backward), false),
            SetCursorStyle::SteadyBar
        ));
    }

    /// Terminal-mode T2.b: `terminal_insert_active` forces the
    /// shape to `SteadyBar` regardless of `ModalState` (which
    /// stays `Normal` because TerminalInsert is a minor mode).
    /// Normal-in-terminal (insert-active = false) stays
    /// `SteadyBlock` so the user can tell at a glance which
    /// sub-state they're in.
    #[test]
    fn terminal_insert_minor_mode_overrides_to_bar() {
        assert!(matches!(
            cursor_style_for(ModalState::Normal, true),
            SetCursorStyle::SteadyBar
        ));
        assert!(matches!(
            cursor_style_for(ModalState::Normal, false),
            SetCursorStyle::SteadyBlock
        ));
    }
}
