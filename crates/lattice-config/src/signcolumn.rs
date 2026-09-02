//! Value type for the `signcolumn` typed option.
//!
//! `signcolumn` controls whether the renderer reserves the gutter
//! **sign columns** — the diagnostics-severity column and the
//! diff-sign column. It is pure display policy read by the renderers
//! (no lattice-core-logic consumer), so — like
//! [`crate::DiagnosticsInline`] / [`crate::ModelineZone`] — the value
//! type lives in `lattice-config` and impls [`OptionType`] locally.
//!
//! Default is [`SignColumn::Yes`]: the columns are reserved
//! unconditionally so the buffer layout never shifts when a sign
//! appears or clears (the no-flicker contract). Modes that render
//! gutterless content — help-mode and other synthetic buffers — set
//! `signcolumn=no`. The renderer derives the column layout from the
//! resolved option alone and never branches on buffer kind / popup /
//! pane / tab, so a regular buffer with `:set signcolumn=no` renders
//! identically to a help popup.

use crate::option_type::{EnumeratedValue, OptionType};

/// `signcolumn` — whether to reserve the gutter sign columns.
///
/// Only `yes` / `no` are wired today. Vim's `auto` (reserve only when
/// a sign is present) is intentionally deferred: it re-introduces the
/// layout shift the unconditional reserve exists to avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SignColumn {
    /// Always reserve the severity + diff-sign columns, so the buffer
    /// layout never shifts when a sign appears or clears. The default.
    #[default]
    Yes,
    /// Never reserve them — content abuts the line-number gutter.
    /// Help / synthetic buffers set this for clean, gutterless render.
    No,
}

impl SignColumn {
    pub fn label(&self) -> &'static str {
        match self {
            SignColumn::Yes => "yes",
            SignColumn::No => "no",
        }
    }

    pub fn doc(&self) -> &'static str {
        match self {
            SignColumn::Yes => {
                "Always reserve the diagnostics + diff sign columns (no layout shift)"
            }
            SignColumn::No => "Never reserve the sign columns (gutterless)",
        }
    }

    /// Whether the sign columns should be reserved for this value.
    /// The single predicate every renderer reads to gate the two
    /// sign-column cells.
    pub fn reserved(&self) -> bool {
        matches!(self, SignColumn::Yes)
    }

    pub fn all() -> [SignColumn; 2] {
        [SignColumn::Yes, SignColumn::No]
    }

    pub fn parse_label(s: &str) -> Result<Self, String> {
        match s {
            "yes" => Ok(SignColumn::Yes),
            "no" => Ok(SignColumn::No),
            other => Err(format!("signcolumn: expected `yes` or `no`, got `{other}`")),
        }
    }
}

impl OptionType for SignColumn {
    fn parse(s: &str) -> Result<Self, String> {
        SignColumn::parse_label(s)
    }
    fn format(&self) -> String {
        self.label().to_string()
    }
    fn type_label() -> &'static str {
        "signcolumn"
    }
    fn enumerate() -> Option<Vec<&'static str>> {
        Some(SignColumn::all().iter().map(|v| v.label()).collect())
    }

    /// TC.1: closed — `parse` accepts these forms and nothing else, so
    /// the schema is an `enum` and `:customize` can offer a picker.
    fn enumerate_is_exhaustive() -> bool {
        true
    }
    fn enumerate_with_docs() -> Option<Vec<EnumeratedValue>> {
        Some(
            SignColumn::all()
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
    use super::*;

    #[test]
    fn default_is_yes_and_reserved() {
        assert_eq!(SignColumn::default(), SignColumn::Yes);
        assert!(SignColumn::default().reserved());
    }

    #[test]
    fn parse_round_trips_every_value() {
        for v in SignColumn::all() {
            assert_eq!(SignColumn::parse_label(v.label()).unwrap(), v);
            assert_eq!(SignColumn::parse(v.label()).unwrap(), v);
            assert_eq!(v.format(), v.label());
        }
    }

    #[test]
    fn parse_rejects_unknown() {
        // `auto` is deliberately not wired yet.
        assert!(SignColumn::parse_label("auto").is_err());
        assert!(SignColumn::parse_label("true").is_err());
    }

    #[test]
    fn no_is_not_reserved() {
        assert!(!SignColumn::No.reserved());
    }

    #[test]
    fn enumerate_lists_both_forms() {
        assert_eq!(SignColumn::enumerate().unwrap(), vec!["yes", "no"]);
    }
}
