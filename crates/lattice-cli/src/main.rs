//! `lattice` -- the editor binary.
//!
//! Phase 2: opens a single file, renders it in a terminal viewer with cursor
//! motion. Subsequent phases wire the modal engine, tree-sitter, LSP, and
//! ultimately the GPU UI.

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

fn main() -> Result<()> {
    let cli = Cli::parse();

    let document = match cli.file {
        Some(path) => Document::open(&path)
            .with_context(|| format!("opening {}", path.display()))?,
        None => Document::empty(),
    };

    lattice_ui_tui::run(document)?;
    Ok(())
}
