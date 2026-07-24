//! Compilation headerline — the compilation-mode-owned status bar
//! rendered as a sticky virtual row above the `*compilation*` buffer.
//!
//! Renders the build command with a state icon leading and a status
//! badge trailing. The command is double-quoted for readability and
//! emphasised in `command_fg` (warm yellow).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_cells::{Cell, Headerline, HeaderlineRow};

/// Shared state the compilation drain updates and the headerline
/// renderer reads.
#[derive(Debug, Clone)]
pub struct CompilationHeadlineState {
    pub command: String,
    pub last_counts: Option<(usize, usize)>,
    pub running: bool,
    pub killed: bool,
}

impl Default for CompilationHeadlineState {
    fn default() -> Self {
        Self { command: String::new(), last_counts: None, running: false, killed: false }
    }
}

/// A `Headerline` impl backed by `CompilationHeadlineState`.
///
/// Renders:
///   ` ⟳ "cargo build --release" … `          (running)
///   ` ✔ "cargo build --release" ok `         (finished clean)
///   ` ✗ "cargo build --release" 3e 2w `      (finished with errors)
///   ` ■ "cargo build --release" killed `     (explicitly killed)
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

        let (icon, icon_fg) = if running {
            ("\u{27f3}", self.in_progress_fg)
        } else if killed {
            ("\u{25a0}", self.failure_fg)
        } else if has_errors {
            ("\u{2717}", self.failure_fg)
        } else {
            ("\u{2714}", self.success_fg)
        };

        let quoted = format!("\"{}\"", state.command);

        if has_errors && !running && !killed {
            // Per-count spans: error count in failure_fg, warning count in dim_fg.
            let mut parts: Vec<(String, u32)> = Vec::new();
            parts.push((format!(" {icon} "), icon_fg));
            parts.push((quoted, self.command_fg));
            if errors > 0 {
                parts.push((format!(" {errors}e"), self.failure_fg));
            }
            if warnings > 0 {
                parts.push((format!(" {warnings}w"), self.dim_fg));
            }
            let cells: Arc<[Cell]> = parts
                .iter()
                .flat_map(|(s, fg)| s.chars().map(move |c| Cell::new(c as u32, *fg, 0, 0)))
                .collect::<Vec<_>>()
                .into();
            return Some(HeaderlineRow { cells, bg: None });
        }

        let status = if running {
            " \u{2026}"
        } else if killed {
            " killed"
        } else {
            " ok"
        };

        let text = format!(" {icon} {quoted}{status}");
        let icon_len = icon.chars().count() + 2; // leading space + icon + trailing space
        let cmd_end = icon_len + quoted.len();
        let cells = text
            .chars()
            .enumerate()
            .map(|(i, c)| {
                let fg = if i < icon_len {
                    icon_fg
                } else if i < cmd_end {
                    self.command_fg
                } else {
                    self.dim_fg
                };
                Cell::new(c as u32, fg, 0, 0)
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
