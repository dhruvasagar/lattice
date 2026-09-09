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
use lattice_core::IndentMethod;
use lattice_core::ui::display::BufferDisplayPreference;
use lattice_core::{AutoWrap, ProviderChain};

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

    /// TC.1: closed — `parse` accepts these forms and nothing else, so
    /// the schema is an `enum` and `:customize` can offer a picker.
    fn enumerate_is_exhaustive() -> bool {
        true
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

    /// TC.1: closed — `parse` accepts these forms and nothing else, so
    /// the schema is an `enum` and `:customize` can offer a picker.
    fn enumerate_is_exhaustive() -> bool {
        true
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

// IN.0: `:set indentmethod=none|keep|syntax`.
impl OptionType for IndentMethod {
    fn parse(s: &str) -> Result<Self, String> {
        IndentMethod::parse_label(s)
    }

    fn format(&self) -> String {
        self.label().to_string()
    }

    fn type_label() -> &'static str {
        "indentmethod"
    }

    fn enumerate() -> Option<Vec<&'static str>> {
        // Derived from `all()` rather than hand-listed. `FoldMethod`
        // above hardcodes its list to preserve a legacy completion
        // order; a new option has no such constraint, so deriving is
        // strictly better -- adding a variant cannot leave the
        // completion list behind.
        Some(IndentMethod::all().iter().map(|v| v.label()).collect())
    }

    /// TC.1: closed — `parse` accepts these forms and nothing else, so
    /// the schema is an `enum` and `:customize` can offer a picker.
    fn enumerate_is_exhaustive() -> bool {
        true
    }

    fn enumerate_with_docs() -> Option<Vec<EnumeratedValue>> {
        Some(
            IndentMethod::all()
                .iter()
                .map(|v| EnumeratedValue {
                    form: v.label(),
                    doc: v.doc(),
                })
                .collect(),
        )
    }
}

// Issue #29 (2026-05-22): tabline.show enum option.
impl OptionType for lattice_core::ui::tab::TablineShow {
    fn parse(s: &str) -> Result<Self, String> {
        lattice_core::ui::tab::TablineShow::parse_label(s)
    }

    fn format(&self) -> String {
        self.label().to_string()
    }

    fn type_label() -> &'static str {
        "tabline-show"
    }

    fn enumerate() -> Option<Vec<&'static str>> {
        Some(
            lattice_core::ui::tab::TablineShow::all()
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
            lattice_core::ui::tab::TablineShow::all()
                .iter()
                .map(|v| EnumeratedValue {
                    form: v.label(),
                    doc: v.doc(),
                })
                .collect(),
        )
    }
}

// RF.0: `:set autowrap=off|comments|all`.
impl OptionType for AutoWrap {
    fn parse(s: &str) -> Result<Self, String> {
        AutoWrap::parse_label(s)
    }

    fn format(&self) -> String {
        self.label().to_string()
    }

    fn type_label() -> &'static str {
        "autowrap"
    }

    fn enumerate() -> Option<Vec<&'static str>> {
        Some(AutoWrap::all().iter().map(|v| v.label()).collect())
    }

    /// Closed: `parse` accepts the three canonical labels plus the
    /// boolean-ish aliases, and nothing else — so `:customize` offers a
    /// picker rather than a text field.
    fn enumerate_is_exhaustive() -> bool {
        true
    }

    fn enumerate_with_docs() -> Option<Vec<EnumeratedValue>> {
        Some(
            AutoWrap::all()
                .iter()
                .map(|v| EnumeratedValue {
                    form: v.label(),
                    doc: v.doc(),
                })
                .collect(),
        )
    }
}

// RF.0: `:set format.reformat=lsp,lang-default` and peers.
//
// The first LIST-valued option in the editor. Comma-separated rather
// than a TOML array because the value has to survive `:set name=value`,
// where vim's own convention for a list is a comma-separated string —
// so this needs no new config machinery beyond the impl.
impl OptionType for ProviderChain {
    fn parse(s: &str) -> Result<Self, String> {
        ProviderChain::parse(s)
    }

    fn format(&self) -> String {
        self.label()
    }

    fn type_label() -> &'static str {
        "formatter-chain"
    }

    /// The bare rungs only. `external:` and `plugin:` carry a payload,
    /// so they cannot be completion candidates — offering `external:`
    /// alone would complete to a value that fails to parse.
    fn enumerate() -> Option<Vec<&'static str>> {
        Some(vec!["native", "lsp", "lang-default"])
    }

    /// **Open**, unlike every other enumerated option here: the value is
    /// a list, and two of its rungs take arbitrary payloads. A
    /// `:customize` picker over these three forms would be actively
    /// wrong — it would hide `external:` and offer no way to write a
    /// chain of more than one.
    fn enumerate_is_exhaustive() -> bool {
        false
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
