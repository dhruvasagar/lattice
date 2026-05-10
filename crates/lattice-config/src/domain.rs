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

use crate::option_type::OptionType;

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
        // `:set foldmethod=<Tab>` shows the same candidates.
        Some(vec!["manual", "indent", "markdown", "syntax"])
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
        ] {
            assert_eq!(FoldMethod::parse(&fm.format()), Ok(fm));
        }
    }

    #[test]
    fn foldmethod_enumerate_lists_every_variant() {
        let values = FoldMethod::enumerate().expect("enumeration available");
        assert_eq!(values, vec!["manual", "indent", "markdown", "syntax"]);
    }

    #[test]
    fn foldmethod_parse_error_message_matches_legacy_wording() {
        // Migration constraint: error message must match the
        // pre-typed-options string for byte-identical user output.
        let err = FoldMethod::parse("xyz").unwrap_err();
        assert!(
            err.contains("expected `manual`, `indent`, `markdown`, or `syntax`")
                && err.contains("xyz"),
            "got `{err}`"
        );
    }
}
