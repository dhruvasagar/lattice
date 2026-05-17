//! `lattice-gpui` — GPUI window binary for the 5.7.B phase
//! progression. Behind the `window` Cargo feature so the
//! scaffold's lib + tests build everywhere; this binary is
//! opt-in for hosts with display libs installed:
//!
//! ```text
//! # Debian / Ubuntu / WSL2:
//! sudo apt install libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev
//! cargo run --features window -p lattice-ui-gpui --bin lattice-gpui
//! ```
//!
//! On WSLg (WSL2 GUI host) gpui-0.2.2's bundled Wayland
//! compositor stack panics on protocol-version negotiation
//! (it requires `xdg_wm_base v2+`; WSLg ships v1). Force the
//! X11 backend by unsetting `WAYLAND_DISPLAY` before running:
//!
//! ```text
//! unset WAYLAND_DISPLAY
//! cargo run --features window -p lattice-ui-gpui --bin lattice-gpui
//! ```
//!
//! ## What this binary does today (5.7.B.4)
//!
//! Opens a 720×480 native window with a minimal editor surface:
//!
//! - **Top region**: the active document's text. Today the
//!   binary boots an empty scratch document and a placeholder
//!   line of instructions; the 5.9 CLI path (`lattice --gpu
//!   <file>`) wires user-supplied files in.
//!
//! - **Bottom region**: a status line showing the current
//!   `ModalState` (Normal / Insert / Visual / ...) and the
//!   `(line, byte)` cursor position. Read live from
//!   `editor.modal` + `editor.cursor` each frame.
//!
//! - **Key events**: every keystroke flows through
//!   [`GpuiApp::dispatch_keystroke`] (Phase 5.7.B.3) so
//!   `i` → Insert, `Esc` → Normal, `j` / `k` move the cursor,
//!   etc. Tested today against `editor.modal` transitions; the
//!   visible cursor and document mutations land as soon as
//!   paint-side text-shaping support lands (post-5.7.B.4
//!   refinements).
//!
//! What's missing vs. the TUI peer: text shaping with proper
//! cursor overlay, per-frame syntax highlights, status-line
//! widgets (LSP / mode hints / diagnostics counts), pane
//! splits, picker overlay, command-line minibuffer. Each lands
//! in its own slice once paint helpers stabilise on the host
//! side.

use gpui::{
    App, AppContext, Application, Bounds, Context, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Render, Styled, Window, WindowBounds, WindowOptions,
    div, px, rgb, size,
};
use lattice_core::Document;
use lattice_grammar::ModalState;
use lattice_ui_gpui::GpuiApp;

/// The renderer-side composition root rendered as a GPUI
/// `Entity`. Holds the [`GpuiApp`] + a [`FocusHandle`] so the
/// window's key events actually route to our dispatcher.
struct EditorView {
    app: GpuiApp,
    focus_handle: FocusHandle,
}

impl EditorView {
    fn new(document: Document, cx: &mut Context<Self>) -> Self {
        Self {
            app: GpuiApp::new(document),
            focus_handle: cx.focus_handle(),
        }
    }

    /// Key-event handler. Destructures GPUI's `Keystroke` into
    /// the primitive shape [`GpuiApp::dispatch_keystroke`] takes
    /// (so the lib doesn't have to link gpui), then ticks the
    /// host pipeline. `cx.notify()` schedules a repaint so the
    /// status line + (future) document view re-reads the
    /// post-dispatch editor state.
    ///
    /// `cx.stop_propagation()` prevents gpui's own keybinding
    /// system from claiming the key after us — we want every
    /// chord to flow through `lattice_host::input::translate`,
    /// not through the platform's default action map.
    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        tracing::debug!(
            key = %ks.key,
            ctrl = ks.modifiers.control,
            alt = ks.modifiers.alt,
            shift = ks.modifiers.shift,
            platform = ks.modifiers.platform,
            "lattice-gpui: key down"
        );
        let outcome = self.app.dispatch_keystroke(
            &ks.key,
            ks.modifiers.control,
            ks.modifiers.alt,
            ks.modifiers.shift,
            ks.modifiers.platform,
        );
        tracing::debug!(
            modal = ?self.app.editor.modal,
            cursor_line = self.app.editor.cursor.line,
            cursor_byte = self.app.editor.cursor.byte,
            dispatched = outcome.is_some(),
            "lattice-gpui: post-dispatch state"
        );
        cx.stop_propagation();
        cx.notify();
        // 5.7.B.7: `:q` / `:qa` / `<C-c>` / `Action::Quit` all
        // set `editor.should_quit` (and emit `RendererSignal::Quit`,
        // which `GpuiApp::handle_renderer_signal` logs but can't
        // act on without a `gpui::App` context). The binary
        // observes the flag here and tears down the application.
        if self.app.editor.should_quit {
            tracing::info!("lattice-gpui: editor.should_quit set; closing application");
            cx.quit();
        }
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Read editor state for this frame. `snapshot()` is
        // wait-free (`ArcSwap::load`); the buffer is a ropey
        // rope that re-renders cheaply.
        let snapshot = self.app.editor.document.snapshot();
        let text = snapshot.text();
        let cursor = self.app.editor.cursor;
        let modal = self.app.editor.modal;

        let modal_label = match modal {
            ModalState::Normal => "NORMAL",
            ModalState::Insert => "INSERT",
            ModalState::Visual(_) => "VISUAL",
            ModalState::OperatorPending => "PENDING",
            ModalState::Command => "COMMAND",
            ModalState::Search(_) => "SEARCH",
            ModalState::Replace => "REPLACE",
        };
        // 5.7.B.4: cursor coordinates are 1-based for display
        // (vim convention) so users reading the status line see
        // the same numbers `:set ruler` shows in the TUI peer.
        let status = format!(
            "  {}   L:{}  C:{}",
            modal_label,
            cursor.line + 1,
            cursor.byte,
        );

        // 5.7.B.8: real cursor block on monospace text. Each
        // character renders as its own div under a `flex_row`
        // line; the cursor character has inverted bg/fg, giving
        // a vim-style block cursor aligned to glyph metrics.
        // Setting `font_family("monospace")` on the outer
        // container makes every cell the same width so the
        // block stacks predictably across rows.
        //
        // Tabs / CRLF / wide-glyph alignment are TODO -- the
        // approach handles ASCII + most BMP glyphs correctly via
        // `char_indices()` (byte-indexed; matches `cursor.byte`).
        // A future slice swaps this for a proper `Element` impl
        // using `window.text_system().shape_line` so the cursor
        // can sit on a sub-character glyph position and tabs
        // expand to their configured width.
        let cursor_line = cursor.line as usize;
        let cursor_byte = cursor.byte as usize;
        let cursor_fg = rgb(0x1e1e2e);
        let cursor_bg = rgb(0xcdd6f4);
        let raw_lines: Vec<&str> = text.split('\n').collect();
        let cursor_past_last_line = cursor_line >= raw_lines.len();

        let mut rows: Vec<gpui::Div> = raw_lines
            .iter()
            .enumerate()
            .map(|(line_idx, line)| {
                let is_cursor_line = line_idx == cursor_line;
                let mut cells: Vec<gpui::Div> = line
                    .char_indices()
                    .map(|(byte_idx, c)| {
                        let is_cursor = is_cursor_line && byte_idx == cursor_byte;
                        if is_cursor {
                            div()
                                .bg(cursor_bg)
                                .text_color(cursor_fg)
                                .child(c.to_string())
                        } else {
                            div().child(c.to_string())
                        }
                    })
                    .collect();
                // Trailing block: cursor at EOL (`cursor_byte ==
                // line.len()`) or on an empty line (`line.len() ==
                // 0`). The block falls on a synthetic space so
                // there's a visible cell where the next insert
                // will land.
                if is_cursor_line && cursor_byte >= line.len() {
                    cells.push(div().bg(cursor_bg).text_color(cursor_fg).child(" "));
                }
                div().flex().flex_row().children(cells)
            })
            .collect();

        if cursor_past_last_line {
            // Cursor sits on a row beyond the document's last
            // line (e.g. a doc ending in `\n` with cursor on the
            // synthetic post-newline row). Append a marker-only
            // row so the user can see the target.
            rows.push(
                div()
                    .flex()
                    .flex_row()
                    .child(div().bg(cursor_bg).text_color(cursor_fg).child(" ")),
            );
        }

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x1e1e2e))
            .text_color(rgb(0xcdd6f4))
            .text_sm()
            // Monospace family: gpui falls through to the next
            // available font if the named family doesn't match.
            // "DejaVu Sans Mono" is present on Debian/Ubuntu/WSLg
            // by default; if not, the fallback chain picks
            // whatever monospace gpui's font system registered.
            .font_family("DejaVu Sans Mono")
            .child(div().flex_grow().p_3().flex().flex_col().children(rows))
            .child(
                div()
                    .bg(rgb(0x313244))
                    .text_color(rgb(0xa6e3a1))
                    .px_2()
                    .py_1()
                    .child(status),
            )
    }
}

fn main() {
    // Tracing subscriber: per-keystroke debug output lands on
    // stderr when `RUST_LOG=lattice_gpui=debug` is set. With no
    // env var the default filter (`info`) keeps the binary
    // quiet for normal use.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();
    Application::new().run(|cx| {
        let bounds = Bounds::centered(None, size(px(720.0), px(480.0)), cx);
        let window = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| cx.new(|cx| EditorView::new(Document::empty(), cx)),
        );
        let window = match window {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(error = ?e, "lattice-gpui: failed to open editor window");
                return;
            }
        };
        // Focus + activation has to run AFTER `open_window`
        // returns -- calling `window.focus(...)` from inside the
        // builder closure runs before gpui has fully initialised
        // the window's focus tree, and the focus call is silently
        // dropped. The `window.update(cx, ...)` re-entry hands
        // back a real window context where focus actually sticks
        // (this is the pattern gpui's own `examples/input.rs`
        // uses). Without this, the editor view never receives
        // key events -- the on_key_down listener is attached but
        // gpui delivers events only to the focused element, and
        // nothing is focused.
        let focus_result = window.update(cx, |view, window, cx| {
            window.focus(&view.focus_handle.clone());
            cx.activate(true);
        });
        if let Err(e) = focus_result {
            tracing::error!(error = ?e, "lattice-gpui: failed to focus editor window");
        }
    });
}
