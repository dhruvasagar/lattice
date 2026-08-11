//! CM.3a (2026-07-22): the error **entry** value type, lowered
//! from `lattice-host` to the protocol floor so the below-host
//! compilation parser (`lattice-compilation`) and the effect payload
//! (`lattice_grammar::AppEffect::SetErrorList`) share ONE type.
//!
//! Only the entry + its severity live here — the *list* (`ErrorList`,
//! with its navigation index) stays in `lattice-host` as core/host
//! state alongside `position_history` (its consumer is generic host
//! dispatch; see `docs/dev/architecture/compilation-mode.md` §3).
//! `lattice-host` re-exports these two types so existing callers
//! (`lattice_host::error_list::ErrorEntry`, the CM.2 tests) are
//! unchanged.
//!
//! `Serialize` / `Deserialize` are added here (the host-local
//! definitions did not carry them) because the type now rides inside
//! the serde-derived `AppEffect` enum.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Severity of one error entry.
///
/// Deliberately independent of LSP's `DiagnosticSeverity`: the
/// error list is core substrate and must not depend on the LSP
/// crate. Producers map their own severity onto this small set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorSeverity {
    Error,
    Warning,
    Info,
    Note,
}

/// Which producer a run of error entries came from.
///
/// EP.1 (2026-08-10): the error list is shared by more than one
/// producer, and a write replaces only its own source's slice. Without
/// the tag a write is a clobber: the language server republishes on
/// every edit-debounce, so a live diagnostic feed would overwrite a
/// compile run's entries *while the user is walking them*.
///
/// Deliberately a small closed enum. A `Plugin(id)` variant lands with
/// the plugin path — the boundary currently refuses `SetErrorList`
/// outright, and tagging is the precondition for lifting that, not a
/// reason to speculate about the shape now.
///
/// See `docs/dev/architecture/error-list.md` §3.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorSource {
    /// `lattice-compilation` — `:compile` / `:recompile` output parsed
    /// into entries.
    Compilation,
    /// `lattice-lsp` — `publishDiagnostics`, coalesced.
    Lsp,
}

impl ErrorSource {
    /// Fixed presentation order. Slices concatenate in this order and
    /// each keeps its producer's own ordering — producer order carries
    /// meaning (rustc emits the root cause ahead of the errors it
    /// cascades into), so sorting the merged list would destroy it.
    pub const PRESENTATION_ORDER: [ErrorSource; 2] = [ErrorSource::Compilation, ErrorSource::Lsp];

    /// Short label for echoes and filters.
    pub fn label(self) -> &'static str {
        match self {
            ErrorSource::Compilation => "compile",
            ErrorSource::Lsp => "lsp",
        }
    }
}

/// One navigable location on the error list. `line` / `col` are
/// 0-based (the convention `Editor::jump_to_file_line_col` expects),
/// matching LSP diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEntry {
    pub path: PathBuf,
    /// 0-based line.
    pub line: u32,
    /// 0-based byte column.
    pub col: u32,
    pub severity: ErrorSeverity,
    pub message: String,
}
