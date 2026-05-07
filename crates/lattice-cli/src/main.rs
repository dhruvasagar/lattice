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
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let document = match cli.file {
        Some(path) => {
            Document::open(&path).with_context(|| format!("opening {}", path.display()))?
        }
        None => Document::empty(),
    };

    lattice_ui_tui::run(document)?;
    Ok(())
}
