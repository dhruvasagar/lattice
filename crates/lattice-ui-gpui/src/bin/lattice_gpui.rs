//! `lattice-gpui` — placeholder GPUI window binary for the 5.7
//! scaffold slice. Behind the `window` Cargo feature so the
//! scaffold's lib + tests build everywhere; this binary is opt-in
//! for hosts with display libs installed:
//!
//! ```text
//! # Debian / Ubuntu / WSL2:
//! sudo apt install libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev
//! cargo run --features window -p lattice-ui-gpui --bin lattice-gpui
//! ```
//!
//! Opens a 720×480 native window with "Lattice (GPUI) — 5.7
//! scaffold" centred text. The purpose is end-to-end validation:
//! if the binary builds and runs, the host substrate
//! (`lattice-host`) is provably reusable from a non-TUI renderer
//! and Zed's GPUI links + initialises against it. Real dispatch +
//! paint wiring is the 5.8+ work; this is the smoke test before
//! that begins.

use gpui::{
    AppContext, Application, Bounds, Context, IntoElement, ParentElement, Render, Styled, Window,
    WindowBounds, WindowOptions, div, px, rgb, size,
};
use lattice_core::Document;
use lattice_ui_gpui::GpuiApp;

/// Minimal root view. Holds the [`GpuiApp`] composition root so the
/// editor + theme + registry remain reachable from inside the GPUI
/// event loop, even though no field is read yet. The 5.8+ paint
/// wiring will thread `&self.app.editor` through the render call.
struct PlaceholderView {
    _app: GpuiApp,
}

impl Render for PlaceholderView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_center()
            .size_full()
            .bg(rgb(0x1e1e2e))
            .text_color(rgb(0xcdd6f4))
            .text_xl()
            .child("Lattice (GPUI) — 5.7 scaffold")
    }
}

fn main() {
    Application::new().run(|cx| {
        let bounds = Bounds::centered(None, size(px(720.0), px(480.0)), cx);
        let window_result = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| {
                cx.new(|_cx| PlaceholderView {
                    // 5.7.B.2: `GpuiApp::new` now goes through
                    // `Editor::boot`. The placeholder binary
                    // boots an empty scratch document; the real
                    // CLI path (Phase 5.9) will route through
                    // `lattice-cli` and pass the user-supplied
                    // file.
                    _app: GpuiApp::new(Document::empty()),
                })
            },
        );
        if let Err(e) = window_result {
            tracing::error!(error = ?e, "lattice-gpui: failed to open placeholder window");
        }
        cx.activate(true);
    });
}
