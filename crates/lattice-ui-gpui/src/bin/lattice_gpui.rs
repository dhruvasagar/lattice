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

/// Vim-style cursor shape derived from [`ModalState`].
///
/// Phase 5.7.B.11: lets the GPUI peer render the same three
/// cursor flavours users expect from the TUI peer / canonical
/// vim — block in Normal/Visual/OperatorPending, bar in
/// Insert/Command/Search, underline in Replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorShape {
    /// Full inverted-cell block (Normal, Visual, OperatorPending).
    Block,
    /// Thin left-side vertical bar (Insert, Command, Search).
    Bar,
    /// Thin bottom-side horizontal underline (Replace).
    Underline,
}

impl CursorShape {
    fn for_mode(modal: ModalState) -> Self {
        match modal {
            ModalState::Insert | ModalState::Command | ModalState::Search(_) => Self::Bar,
            ModalState::Replace => Self::Underline,
            ModalState::Normal | ModalState::Visual(_) | ModalState::OperatorPending => Self::Block,
        }
    }
}

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
        // 5.7.B.10: when a help popup is showing, intercept Esc
        // for dismissal before the keystroke reaches
        // `editor.dispatch`. Without this pre-empt, Esc would
        // also fire its normal Normal-mode-entry behaviour
        // while the popup stays visible.
        if self.app.popup_content.is_some()
            && (ks.key.eq_ignore_ascii_case("escape") || ks.key.eq_ignore_ascii_case("esc"))
        {
            tracing::debug!("lattice-gpui: dismissing popup overlay (Esc)");
            self.app.dismiss_popup();
            cx.stop_propagation();
            cx.notify();
            return;
        }
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

        // 5.7.B.8/11: vim-style mode-aware cursor on monospace
        // text. Each character renders as its own div under a
        // `flex_row` line. The cursor cell is styled three
        // different ways depending on `modal`:
        //
        //   - Normal / Visual / OperatorPending: block (full
        //     cell inverted bg/fg). The classic vim block cursor.
        //   - Insert / Command / Search: left bar (a 2px left
        //     border colored as the cursor). The vim Insert
        //     convention -- the cursor sits BEFORE the next
        //     character.
        //   - Replace: underline (a 2px bottom border). Signals
        //     overwrite mode.
        //
        // Tabs / CRLF / wide-glyph alignment are still TODO; a
        // future slice swaps this for a proper `Element` impl
        // using `window.text_system().shape_line` so the cursor
        // can sit on sub-character glyph positions.
        let cursor_line = cursor.line as usize;
        let cursor_byte = cursor.byte as usize;
        // 5.7.B.12: cursor colors come from the cached GpuiTheme
        // (stored as `0xRRGGBB` u32; convert to gpui Rgba here)
        // so theme cascades propagate without binary-side
        // changes.
        let cursor_fg = rgb(self.app.theme.cursor_foreground);
        let cursor_bg = rgb(self.app.theme.cursor_background);
        let cursor_shape = CursorShape::for_mode(modal);
        let raw_lines: Vec<&str> = text.split('\n').collect();
        let cursor_past_last_line = cursor_line >= raw_lines.len();

        // Style a cursor cell with the current shape (closure
        // so EOL trailing + past-last-line trailing share the
        // logic with the in-line cursor cell).
        let style_cursor_cell = |c: &str| -> gpui::Div {
            let cell = div().child(c.to_string());
            match cursor_shape {
                CursorShape::Block => cell.bg(cursor_bg).text_color(cursor_fg),
                CursorShape::Bar => cell.border_l_2().border_color(cursor_bg),
                CursorShape::Underline => cell.border_b_2().border_color(cursor_bg),
            }
        };

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
                            style_cursor_cell(&c.to_string())
                        } else {
                            div().child(c.to_string())
                        }
                    })
                    .collect();
                // Trailing cursor: cursor at EOL (`cursor_byte ==
                // line.len()`) or on an empty line (`line.len() ==
                // 0`). The cursor falls on a synthetic space so
                // there's a visible cell where the next insert
                // will land.
                if is_cursor_line && cursor_byte >= line.len() {
                    cells.push(style_cursor_cell(" "));
                }
                div().flex().flex_row().children(cells)
            })
            .collect();

        if cursor_past_last_line {
            // Cursor sits on a row beyond the document's last
            // line (e.g. a doc ending in `\n` with cursor on the
            // synthetic post-newline row). Append a cursor-only
            // row so the user can see the target.
            rows.push(div().flex().flex_row().child(style_cursor_cell(" ")));
        }

        // 5.7.B.12: surface colors come from the cached
        // GpuiTheme so theme cascades propagate. The theme
        // stores `0xRRGGBB` u32 packed; `rgb(...)` converts
        // to `gpui::Rgba` at render time.
        let theme = self.app.theme;
        // 5.7.B.10: build the document area. Wrapped in a
        // separate variable so the popup overlay (if any) can
        // stack on top of it via `relative` positioning.
        let document_area = div().flex_grow().p_3().flex().flex_col().children(rows);

        // 5.7.B.10: when a help popup is showing, render it as
        // a centered overlay above the document area. The
        // overlay reads the help buffer's content + title and
        // displays them in a bordered panel; pressing `Esc`
        // (binary-side pre-empt in on_key_down) dismisses.
        let popup_overlay: Option<gpui::Div> = self.app.popup_content.as_ref().map(|content| {
            let title = content.buffer.title.clone();
            let body_text = content.buffer.content.as_string();
            let popup_lines: Vec<gpui::Div> = body_text
                .split('\n')
                .map(|line| div().child(line.to_string()))
                .collect();
            div()
                .flex()
                .flex_col()
                .max_w(px(640.0))
                .max_h(px(400.0))
                .p_4()
                .bg(rgb(theme.popup_background))
                .text_color(rgb(theme.foreground))
                .border_2()
                .border_color(rgb(theme.popup_border))
                .child(
                    div()
                        .text_color(rgb(theme.popup_border))
                        .pb_2()
                        .child(format!(" {title} "))
                        .child(div().child("───────────────".to_string())),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .children(popup_lines),
                )
                .child(
                    div()
                        .pt_2()
                        .text_color(rgb(theme.popup_border))
                        .child("[ Esc to dismiss ]".to_string()),
                )
        });

        let mut root = div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(theme.background))
            .text_color(rgb(theme.foreground))
            .text_sm()
            // Monospace family: gpui falls through to the next
            // available font if the named family doesn't match.
            // "DejaVu Sans Mono" is present on Debian/Ubuntu/WSLg
            // by default; if not, the fallback chain picks
            // whatever monospace gpui's font system registered.
            .font_family("DejaVu Sans Mono")
            .child(document_area)
            .child(
                div()
                    .bg(rgb(theme.status_background))
                    .text_color(rgb(theme.status_foreground))
                    .px_2()
                    .py_1()
                    .child(status),
            );

        if let Some(overlay) = popup_overlay {
            // Center the overlay across the window. The simplest
            // gpui pattern is to absolutely-position a wrapping
            // div that fills the screen + uses flex centering;
            // gpui-0.2.2 supports `.absolute().inset_0()` +
            // `.justify_center().items_center()`.
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .justify_center()
                    .items_center()
                    .child(overlay),
            );
        }
        root
    }
}

#[cfg(test)]
mod cursor_shape_tests {
    use super::CursorShape;
    use lattice_grammar::{ModalState, SearchDirection, VisualKind};

    #[test]
    fn normal_uses_block() {
        assert_eq!(
            CursorShape::for_mode(ModalState::Normal),
            CursorShape::Block
        );
    }

    #[test]
    fn visual_uses_block() {
        assert_eq!(
            CursorShape::for_mode(ModalState::Visual(VisualKind::Charwise)),
            CursorShape::Block
        );
        assert_eq!(
            CursorShape::for_mode(ModalState::Visual(VisualKind::Linewise)),
            CursorShape::Block
        );
        assert_eq!(
            CursorShape::for_mode(ModalState::Visual(VisualKind::Blockwise)),
            CursorShape::Block
        );
    }

    #[test]
    fn operator_pending_uses_block() {
        assert_eq!(
            CursorShape::for_mode(ModalState::OperatorPending),
            CursorShape::Block
        );
    }

    #[test]
    fn insert_uses_bar() {
        assert_eq!(CursorShape::for_mode(ModalState::Insert), CursorShape::Bar);
    }

    #[test]
    fn command_uses_bar() {
        assert_eq!(CursorShape::for_mode(ModalState::Command), CursorShape::Bar);
    }

    #[test]
    fn search_uses_bar() {
        assert_eq!(
            CursorShape::for_mode(ModalState::Search(SearchDirection::Forward)),
            CursorShape::Bar
        );
        assert_eq!(
            CursorShape::for_mode(ModalState::Search(SearchDirection::Backward)),
            CursorShape::Bar
        );
    }

    #[test]
    fn replace_uses_underline() {
        assert_eq!(
            CursorShape::for_mode(ModalState::Replace),
            CursorShape::Underline
        );
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
