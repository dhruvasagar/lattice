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
