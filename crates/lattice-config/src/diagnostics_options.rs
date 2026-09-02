//! Value types for the `ui.diagnostics.*` typed options (slice L4a).
//!
//! The inline diagnostic presentation (`lsp-architecture.md` §15) is
//! controlled by two typed options under the `diagnostics` group:
//! [`DiagnosticsInline`] (where the end-of-line summary renders) and
//! [`DiagnosticsSeverity`] (the least-severe level included). Both are
//! pure display policy read host-side, so — like [`crate::ModelineZone`]
//! — they live in `lattice-config` (no lattice-core-logic consumer) and
//! impl [`OptionType`] locally.

use crate::option_type::{EnumeratedValue, OptionType};

/// `ui.diagnostics.inline` — where the inline (end-of-line virtual-text)
/// diagnostic summary renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiagnosticsInline {
    /// No inline summary — gutter severity column + inline underline
    /// only.
    Off,
    /// The cursor line only (Helix's low-noise default), painted after
    /// a short idle once the cursor settles on a new line and suppressed
    /// in Insert mode.
    #[default]
    CursorLine,
    /// Every viewport line (noisier; the L5 opt-in). O(viewport) only.
    All,
}

impl DiagnosticsInline {
    pub fn label(&self) -> &'static str {
        match self {
            DiagnosticsInline::Off => "off",
            DiagnosticsInline::CursorLine => "cursor-line",
            DiagnosticsInline::All => "all",
        }
    }

    pub fn doc(&self) -> &'static str {
        match self {
            DiagnosticsInline::Off => "No inline summary (gutter + underline only)",
            DiagnosticsInline::CursorLine => "End-of-line summary on the cursor line, idle-gated",
            DiagnosticsInline::All => "End-of-line summary on every viewport line",
        }
    }

    pub fn all() -> [DiagnosticsInline; 3] {
        [
            DiagnosticsInline::Off,
            DiagnosticsInline::CursorLine,
            DiagnosticsInline::All,
        ]
    }

    pub fn parse_label(s: &str) -> Result<Self, String> {
        match s {
            "off" => Ok(DiagnosticsInline::Off),
            "cursor-line" => Ok(DiagnosticsInline::CursorLine),
            "all" => Ok(DiagnosticsInline::All),
            other => Err(format!(
                "ui.diagnostics.inline: expected `off`, `cursor-line`, or `all`, got `{other}`"
            )),
        }
    }
}

impl OptionType for DiagnosticsInline {
    fn parse(s: &str) -> Result<Self, String> {
        DiagnosticsInline::parse_label(s)
    }
    fn format(&self) -> String {
        self.label().to_string()
    }
    fn type_label() -> &'static str {
        "diagnostics-inline"
    }
    fn enumerate() -> Option<Vec<&'static str>> {
        Some(DiagnosticsInline::all().iter().map(|v| v.label()).collect())
    }

    /// TC.1: closed — `parse` accepts these forms and nothing else, so
    /// the schema is an `enum` and `:customize` can offer a picker.
    fn enumerate_is_exhaustive() -> bool {
        true
    }
    fn enumerate_with_docs() -> Option<Vec<EnumeratedValue>> {
        Some(
            DiagnosticsInline::all()
                .iter()
                .map(|v| EnumeratedValue {
                    form: v.label(),
                    doc: v.doc(),
                })
                .collect(),
        )
    }
}

/// `ui.diagnostics.inline-min-severity` — the least-severe level a
/// diagnostic must be to appear in the inline summary. A diagnostic is
/// included when its severity is **as severe or more severe** than this
/// (i.e. its rank ≤ this rank).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiagnosticsSeverity {
    Error,
    Warning,
    Info,
    /// The default: include everything down to hints.
    #[default]
    Hint,
}

impl DiagnosticsSeverity {
    /// Severity rank matching `lattice_lsp::diagnostics_layer`'s
    /// `severity_rank` (Error = 0 … Hint = 3) — lower is more severe.
    /// The host includes a diagnostic when its rank ≤ this rank.
    pub fn rank(&self) -> u8 {
        match self {
            DiagnosticsSeverity::Error => 0,
            DiagnosticsSeverity::Warning => 1,
            DiagnosticsSeverity::Info => 2,
            DiagnosticsSeverity::Hint => 3,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            DiagnosticsSeverity::Error => "error",
            DiagnosticsSeverity::Warning => "warning",
            DiagnosticsSeverity::Info => "info",
            DiagnosticsSeverity::Hint => "hint",
        }
    }

    pub fn doc(&self) -> &'static str {
        match self {
            DiagnosticsSeverity::Error => "Errors only",
            DiagnosticsSeverity::Warning => "Warnings and errors",
            DiagnosticsSeverity::Info => "Info, warnings, and errors",
            DiagnosticsSeverity::Hint => "All diagnostics (hints and up)",
        }
    }

    pub fn all() -> [DiagnosticsSeverity; 4] {
        [
            DiagnosticsSeverity::Error,
            DiagnosticsSeverity::Warning,
            DiagnosticsSeverity::Info,
            DiagnosticsSeverity::Hint,
        ]
    }

    pub fn parse_label(s: &str) -> Result<Self, String> {
        match s {
            "error" => Ok(DiagnosticsSeverity::Error),
            "warning" => Ok(DiagnosticsSeverity::Warning),
            "info" => Ok(DiagnosticsSeverity::Info),
            "hint" => Ok(DiagnosticsSeverity::Hint),
            other => Err(format!(
                "ui.diagnostics.inline-min-severity: expected `error`, `warning`, `info`, or `hint`, got `{other}`"
            )),
        }
    }
}

impl OptionType for DiagnosticsSeverity {
    fn parse(s: &str) -> Result<Self, String> {
        DiagnosticsSeverity::parse_label(s)
    }
    fn format(&self) -> String {
        self.label().to_string()
    }
    fn type_label() -> &'static str {
        "diagnostics-severity"
    }
    fn enumerate() -> Option<Vec<&'static str>> {
        Some(
            DiagnosticsSeverity::all()
                .iter()
                .map(|v| v.label())
                .collect(),
        )
    }

    /// TC.1: closed — `parse` accepts these forms and nothing else, so
    /// the schema is an `enum` and `:customize` can offer a picker.
    fn enumerate_is_exhaustive() -> bool {
        true
    }
    fn enumerate_with_docs() -> Option<Vec<EnumeratedValue>> {
        Some(
            DiagnosticsSeverity::all()
                .iter()
                .map(|v| EnumeratedValue {
                    form: v.label(),
                    doc: v.doc(),
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn inline_round_trips_and_default_is_cursor_line() {
        assert_eq!(DiagnosticsInline::default(), DiagnosticsInline::CursorLine);
        for v in DiagnosticsInline::all() {
            assert_eq!(DiagnosticsInline::parse(&v.format()).unwrap(), v);
        }
        assert!(DiagnosticsInline::parse("nope").is_err());
    }

    #[test]
    fn severity_round_trips_and_rank_orders_error_lowest() {
        assert_eq!(DiagnosticsSeverity::default(), DiagnosticsSeverity::Hint);
        for v in DiagnosticsSeverity::all() {
            assert_eq!(DiagnosticsSeverity::parse(&v.format()).unwrap(), v);
        }
        assert!(DiagnosticsSeverity::Error.rank() < DiagnosticsSeverity::Warning.rank());
        assert!(DiagnosticsSeverity::Warning.rank() < DiagnosticsSeverity::Info.rank());
        assert!(DiagnosticsSeverity::Info.rank() < DiagnosticsSeverity::Hint.rank());
        assert!(DiagnosticsSeverity::parse("fatal").is_err());
    }

    #[test]
    fn enumerate_with_docs_covers_every_variant() {
        assert_eq!(DiagnosticsInline::enumerate_with_docs().unwrap().len(), 3);
        assert_eq!(DiagnosticsSeverity::enumerate_with_docs().unwrap().len(), 4);
    }
}
