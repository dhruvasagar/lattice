//! Compilation headerline — the compilation-mode-owned status bar
//! rendered as a sticky virtual row above the `*compilation*` buffer.
//!
//! Mirrors the project-search headerline pattern: shows the command
//! being run with emphasis highlighting (like search emphasises the
//! query), a spinner for running state, and a success/failure icon
//! with error/warning counts for the terminal state.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_cells::{Cell, Headerline, HeaderlineRow};

/// Shared state the compilation drain updates and the headerline
/// renderer reads.
#[derive(Debug, Clone)]
pub struct CompilationHeadlineState {
    /// The compile command being run, e.g. `cargo build --release`.
    pub command: String,
    /// `errors + warnings` from the last finished run. `None` while
    /// no run has completed.
    pub last_counts: Option<(usize, usize)>,
    /// `true` while a compile run is in progress (Reset published but
    /// no Finished yet).
    pub running: bool,
}

impl Default for CompilationHeadlineState {
    fn default() -> Self {
        Self {
            command: String::new(),
            last_counts: None,
            running: false,
        }
    }
}

/// A `Headerline` impl backed by `CompilationHeadlineState`.
///
/// Renders:
///   ` ⟳ cargo build --release … `          (running, command emphasised)
///   ` ◆ cargo build --release ✗ 3e 2w `    (finished with errors)
///   ` ◆ cargo build --release ✔ ok `       (finished clean)
pub struct CompilationHeaderline {
    state: Arc<std::sync::RwLock<CompilationHeadlineState>>,
    version: Arc<AtomicU64>,
    command_fg: u32,
    in_progress_fg: u32,
    success_fg: u32,
    failure_fg: u32,
}

impl CompilationHeaderline {
    pub fn new(
        state: Arc<std::sync::RwLock<CompilationHeadlineState>>,
        version: Arc<AtomicU64>,
        command_fg: u32,
        in_progress_fg: u32,
        success_fg: u32,
        failure_fg: u32,
    ) -> Self {
        Self { state, version, command_fg, in_progress_fg, success_fg, failure_fg }
    }
}

impl Headerline for CompilationHeaderline {
    fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    fn render(&self) -> Option<HeaderlineRow> {
        let state = self.state.read().ok()?;
        if state.command.is_empty() {
            return None;
        }
        let running = state.running;
        let (errors, warnings) = state.last_counts.unwrap_or((0, 0));
        let has_errors = errors > 0;

        let (prefix, fg) = if running {
            ("\u{27f3} ", self.in_progress_fg)
        } else if has_errors {
            ("\u{25c6} ", self.failure_fg)
        } else {
            ("\u{25c6} ", self.success_fg)
        };

        let status_text = if running {
            " \u{2026}".to_string()
        } else if has_errors {
            format!(" \u{2717} {errors}e {warnings}w")
        } else {
            " \u{2714} ok".to_string()
        };

        let text = format!("{prefix}{}{status_text}", state.command);

        let emphasis = if state.command.is_empty() {
            None
        } else {
            let prefix_len = prefix.chars().count();
            let cmd_len = state.command.chars().count();
            Some((prefix_len, prefix_len + cmd_len))
        };

        let cells: Arc<[Cell]> = text
            .chars()
            .enumerate()
            .map(|(i, c)| {
                let cell_fg = match emphasis {
                    Some((start, end)) if i >= start && i < end => self.command_fg,
                    _ => fg,
                };
                Cell::new(c as u32, cell_fg, 0, 0)
            })
            .collect::<Vec<_>>()
            .into();

        Some(HeaderlineRow { cells, bg: None })
    }
}

/// Provider id tag for the compilation headerline so re-activations
/// can `unregister` / `register` idempotently.
pub const COMPILATION_HEADERLINE_PROVIDER_ID: u64 =
    0x636f_6d70_686c_0300; // "comp-hl"
