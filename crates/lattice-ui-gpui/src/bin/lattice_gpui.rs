//! `lattice-gpui` — thin shim binary that opens a GPUI window
//! via [`lattice_ui_gpui::run`]. Behind the `window` Cargo
//! feature so the lib + tests build on hosts without display
//! libs.
//!
//! Phase 5.9 migration: the previous 800+-line `main` body
//! (EditorView, CursorShape, render impl, etc.) was lifted into
//! `lattice_ui_gpui::window`. This binary now just sets up
//! tracing, opens the document from the first CLI arg, and
//! hands off to [`lattice_ui_gpui::run`]. The same entry is
//! available via `lattice --gpu <file>` once
//! `lattice-cli` gains the `gpu` feature.
//!
//! ```text
//! # Debian / Ubuntu / WSL2:
//! sudo apt install libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev
//! cargo run --features window -p lattice-ui-gpui --bin lattice-gpui
//! cargo run --features window -p lattice-ui-gpui --bin lattice-gpui -- src/lib.rs
//! ```
//!
//! On WSLg (WSL2 GUI host) gpui-0.2.2's bundled Wayland
//! compositor stack panics on protocol-version negotiation
//! (requires `xdg_wm_base v2+`; WSLg ships v1). Force the X11
//! backend by unsetting `WAYLAND_DISPLAY`:
//!
//! ```text
//! unset WAYLAND_DISPLAY
//! cargo run --features window -p lattice-ui-gpui --bin lattice-gpui
//! ```

use anyhow::Result;

fn main() -> Result<()> {
    // Tracing subscriber: per-keystroke debug output lands on
    // stderr when `RUST_LOG=lattice_gpui=debug` is set. With no
    // env var the default filter (`info`) keeps the binary quiet
    // for normal use.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let document = lattice_ui_gpui::document_from_first_arg();
    lattice_ui_gpui::run(document)
}
