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
    /// `true` when the compilation was explicitly killed (via
    /// `:compilation-kill` or `<C-c>`). The headerline shows a
    /// distinct killed icon instead of success/failure.
    pub killed: bool,
}

impl Default for CompilationHeadlineState {
    fn default() -> Self {
        Self {
            command: String::new(),
            last_counts: None,
            running: false,
            killed: false,
        }
    }
}

/// A `Headerline` impl backed by `CompilationHeadlineState`.
///
/// Renders:
///   ` Compiling ⟳ "cargo build --release" … `
///   ` Compiled ✔ "cargo build --release" ok `
///   ` Compiled ✗ "cargo build --release" 3e 2w `
///   ` Killed ■ "cargo build --release" `
///
/// The command is quoted for readability; the label prefix
/// ("Compiling", "Compiled", "Killed") gives the user an at-a-glance
/// summary of the build state. The command body uses `command_fg`
/// (warm yellow) for emphasis.
pub struct CompilationHeaderline {
    state: Arc<std::sync::RwLock<CompilationHeadlineState>>,
    version: Arc<AtomicU64>,
    command_fg: u32,
    in_progress_fg: u32,
    success_fg: u32,
    failure_fg: u32,
    dim_fg: u32,
}

impl CompilationHeaderline {
    pub fn new(
        state: Arc<std::sync::RwLock<CompilationHeadlineState>>,
        version: Arc<AtomicU64>,
        command_fg: u32,
        in_progress_fg: u32,
        success_fg: u32,
        failure_fg: u32,
        dim_fg: u32,
    ) -> Self {
        Self { state, version, command_fg, in_progress_fg, success_fg, failure_fg, dim_fg }
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
        let killed = state.killed;
        let (errors, warnings) = state.last_counts.unwrap_or((0, 0));
        let has_errors = errors > 0;

        // Label + icon + colon separator: the user-facing prefix that
        // tells them what state the build is in before they even read
        // the command.
        let (label, icon, label_fg) = if running {
            ("Compiling", "\u{27f3}", self.in_progress_fg)
        } else if killed {
            ("Killed", "\u{25a0}", self.failure_fg)
        } else if has_errors {
            ("Compiled", "\u{2717}", self.failure_fg)
        } else {
            ("Compiled", "\u{2714}", self.success_fg)
        };

        // Status badge on the right: count of errors/warnings, "ok",
        // "killed", or spinner dots.
        let status = if running {
            " \u{2026}"
        } else if killed {
            ""
        } else if has_errors {
            return Some(self.render_counts(label, icon, &label_fg, &state.command, errors, warnings));
        } else {
            " ok"
        };

        let quoted_cmd = format!("\"{}\"", state.command);
        let text = format!(" {label} {icon} {quoted_cmd}{status}");
        let cells = build_cells(&text, label_fg, &state.command, self.command_fg, self.dim_fg, icon);
        Some(HeaderlineRow { cells, bg: None })
    }
}

impl CompilationHeaderline {
    /// Render the counts variant (errors + warnings) with per-count
    /// colouring. Errors use failure_fg, warnings use dim_fg.
    fn render_counts(
        &self,
        label: &str,
        icon: &str,
        label_fg: &u32,
        command: &str,
        errors: usize,
        warnings: usize,
    ) -> HeaderlineRow {
        let label_prefix = format!(" {label} {icon} ");
        let quoted_cmd = format!("\"{}\"", command);
        let mut spans: Vec<(String, u32)> = Vec::new();
        spans.push((label_prefix, *label_fg));
        spans.push((quoted_cmd, self.command_fg));
        if errors > 0 {
            spans.push((format!(" {errors}e"), self.failure_fg));
        }
        if warnings > 0 {
            spans.push((format!(" {warnings}w"), self.dim_fg));
        }

        let cells: Arc<[Cell]> = spans
            .iter()
            .flat_map(|(s, fg)| s.chars().map(move |c| Cell::new(c as u32, *fg, 0, 0)))
            .collect::<Vec<_>>()
            .into();
        HeaderlineRow { cells, bg: None }
    }
}

/// Build cells for the non-counts variants. `label_fg` colours the
/// named prefix, `command_fg` colours the quoted command body, `dim_fg`
/// colours the status token ("ok" / "…"), and `icon` (already
/// coloured by `label_fg`) sits between label and command.
fn build_cells(
    text: &str,
    label_fg: u32,
    command: &str,
    command_fg: u32,
    dim_fg: u32,
    icon: &str,
) -> Arc<[Cell]> {
    let quoted_cmd = format!("\"{}\"", command);
    let icon_start = text.find(icon).unwrap_or(0);
    let icon_end = icon_start + icon.chars().count();
    let quote_start = text.find('"').unwrap_or(0);
    let quote_end = quote_start + quoted_cmd.len();

    text.chars()
        .enumerate()
        .map(|(i, c)| {
            let fg = if i >= icon_start && i < icon_end {
                label_fg
            } else if i >= quote_start && i < quote_end {
                command_fg
            } else if i > quote_end {
                dim_fg
            } else {
                label_fg
            };
            Cell::new(c as u32, fg, 0, 0)
        })
        .collect::<Vec<_>>()
        .into()
}

/// Provider id tag for the compilation headerline so re-activations
/// can `unregister` / `register` idempotently.
pub const COMPILATION_HEADERLINE_PROVIDER_ID: u64 =
    0x636f_6d70_686c_0300; // "comp-hl"
