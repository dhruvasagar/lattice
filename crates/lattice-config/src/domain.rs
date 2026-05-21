//! [`OptionType`] impls for domain types defined in other crates.
//!
//! These impls live in `lattice-config` (rather than alongside the
//! types) because the alternative — having `lattice-core` depend
//! on `lattice-config` — would invert the dependency direction.
//! `lattice-config` already depends on `lattice-core` (transitively
//! through `lattice-completion`'s `Buffer`-aware traits), so adding
//! domain impls here is cycle-free.
//!
//! The orphan rule allows this: `OptionType` is local to
//! `lattice-config`; the types implemented here (`FoldMethod`, ...)
//! are foreign — local-trait-on-foreign-type is permitted.

use lattice_core::FoldMethod;
use lattice_core::ui::display::BufferDisplayPreference;

use crate::option_type::{EnumeratedValue, OptionType};

impl OptionType for BufferDisplayPreference {
    fn parse(s: &str) -> Result<Self, String> {
        BufferDisplayPreference::parse_label(s)
    }

    fn format(&self) -> String {
        self.label().to_string()
    }

    fn type_label() -> &'static str {
        "display-preference"
    }

    fn enumerate() -> Option<Vec<&'static str>> {
        Some(vec![
            "default",
            "popup-centered",
            "popup-cursor",
            "floating-cursor",
            "active-pane",
            "split-h",
            "split-v",
        ])
    }

    fn enumerate_with_docs() -> Option<Vec<EnumeratedValue>> {
        // Slice `3c.unify.option-docs-builtin`: per-value
        // marginalia pulled from the variant's own `doc()`
        // accessor in `lattice-core`. Adding a new variant
        // requires extending `label` / `doc` / `all` together;
        // this method picks the docs up automatically.
        Some(
            BufferDisplayPreference::all()
                .iter()
                .map(|v| EnumeratedValue {
                    form: v.label(),
                    doc: v.doc(),
                })
                .collect(),
        )
    }
}

impl OptionType for FoldMethod {
    fn parse(s: &str) -> Result<Self, String> {
        FoldMethod::parse_label(s)
    }

    fn format(&self) -> String {
        self.label().to_string()
    }

    fn type_label() -> &'static str {
        "foldmethod"
    }

    fn enumerate() -> Option<Vec<&'static str>> {
        // Order matches the legacy `gen:options` value list so
        // `:set foldmethod=<Tab>` shows the same candidates;
        // `lsp` (4.4.f) appended at the end.
        Some(vec!["manual", "indent", "markdown", "syntax", "lsp"])
    }

    fn enumerate_with_docs() -> Option<Vec<EnumeratedValue>> {
        // Slice `3c.unify.option-docs-builtin`: per-value
        // marginalia pulled from the variant's own `doc()`
        // accessor in `lattice-core`. Adding a new fold method
        // requires extending `label` / `doc` / `all` together;
        // this method picks the docs up automatically.
        Some(
            FoldMethod::all()
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
    fn foldmethod_parse_round_trip() {
        for fm in [
            FoldMethod::Manual,
            FoldMethod::Indent,
            FoldMethod::Markdown,
            FoldMethod::Syntax,
            FoldMethod::Lsp,
        ] {
            assert_eq!(FoldMethod::parse(&fm.format()), Ok(fm));
        }
    }

    #[test]
    fn foldmethod_enumerate_lists_every_variant() {
        let values = FoldMethod::enumerate().expect("enumeration available");
        assert_eq!(
            values,
            vec!["manual", "indent", "markdown", "syntax", "lsp"]
        );
    }

    #[test]
    fn foldmethod_parse_error_message_matches_legacy_wording() {
        // Error wording grew the `lsp` option in 4.4.f; check
        // for the new shape (the legacy-string requirement is
        // dropped because the option set itself grew).
        let err = FoldMethod::parse("xyz").unwrap_err();
        assert!(
            err.contains("expected `manual`, `indent`, `markdown`, `syntax`, or `lsp`")
                && err.contains("xyz"),
            "got `{err}`"
        );
    }
}
