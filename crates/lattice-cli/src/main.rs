//! `lattice` -- the editor binary.
//!
//! Phase 2: opens a single file, renders it in a terminal viewer with cursor
//! motion. Subsequent phases wire the modal engine, tree-sitter, LSP, and
//! ultimately the GPU UI.
//!
//! ## Why `#[tokio::main]`
//!
//! Slice C.1: `main` is async so a tokio runtime is current from the
//! very first instruction of program execution.
//! `tokio::runtime::Handle::try_current()` succeeds anywhere
//! downstream -- including in `App::new` which constructs
//! `SyntaxHandle`s and (separately) the LSP supervisor handle.
//!
//! Pre-C.1, `main` was synchronous and the runtime didn't exist
//! yet at construction time, so `try_current()` silently failed.
//! Two specific subsystems noticed: LSP papered over with
//! explicit-handle plumbing
//! (`lattice_ui_tui::runtime::lsp_runtime()` + `LspSupervisor::spawn(handle)`);
//! syntax did *not* paper over and the worker was silently
//! never-spawned for the entire lifetime of Option B's
//! incremental-reparse pipeline -- producing the user-visible
//! "highlighting stuck to byte positions" symptom regardless of
//! how correct the algorithm was. C.1 makes that class of bug
//! impossible by ensuring a runtime is always current from start.
//!
//! `lattice_ui_tui::run` stays sync and is called directly from
//! the async main body. It blocks the executor thread for its
//! lifetime; spawned tasks (LSP, syntax, document actor) run on
//! other workers (the multi-thread runtime has `num_cpus` worker
//! threads). `block_on` calls inside the editor route through
//! `lattice_runtime::block_on`, which handles nested-runtime
//! contexts via `block_in_place` (slice C.1.a).

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

use lattice_core::Document;

#[derive(Debug, Parser)]
#[command(version, about = "lattice editor", long_about = None)]
struct Cli {
    /// Path to the file to open. If omitted, an empty buffer is opened.
    file: Option<PathBuf>,

    /// Route to the GPUI renderer (requires the `gui` build feature).
    /// Default until phase 5.last lands; from then on `--gui` becomes
    /// the default and `--tui` opts in to the terminal renderer. Mutually
    /// exclusive with `--tui`.
    #[arg(long, conflicts_with = "tui")]
    gui: bool,

    /// Route to the TUI renderer (terminal). Default until phase 5.last.
    /// Mutually exclusive with `--gui`.
    #[arg(long, conflicts_with = "gui")]
    tui: bool,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    // Tracing subscriber: per-keystroke debug output lands on stderr when
    // `RUST_LOG=lattice_ui_gpui=debug` (or any module-targeted filter) is
    // set. With no env var the default filter (`info`) keeps the binary
    // quiet for normal use. `try_init` is idempotent — safe if a hook /
    // test framework already installed a subscriber.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let cli = Cli::parse();

    let document = match cli.file {
        Some(path) => {
            Document::open(&path).with_context(|| format!("opening {}", path.display()))?
        }
        None => Document::empty(),
    };

    // Phase 5.9 / 5.8.M: single-binary entry. `--gui` routes to the
    // GPUI peer (feature-gated by `gui`). `--tui` (or no flag) routes
    // to the TUI peer. After phase 5.last achieves full feature parity
    // the default flips to GUI; until then TUI stays the default.
    // `clap`'s `conflicts_with` enforces mutual exclusivity at parse
    // time.
    if cli.gui {
        run_gui(document)
    } else {
        lattice_ui_tui::run(document)
    }
}

/// Route to the GPUI peer. Feature-gated: the `gui` Cargo feature
/// pulls in `lattice-ui-gpui` (with its `window` feature) and
/// exposes the real entry. Without the feature, `--gui` produces a
/// helpful error so users know to rebuild with `--features gui`.
#[cfg(feature = "gui")]
fn run_gui(document: Document) -> Result<()> {
    lattice_ui_gpui::run(document)
}

#[cfg(not(feature = "gui"))]
fn run_gui(_document: Document) -> Result<()> {
    anyhow::bail!(
        "GUI renderer not compiled in. Rebuild with `cargo build --features gui` \
         (Linux: requires `libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev`)."
    )
}
