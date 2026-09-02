//! Per-type metadata + parse / format / enumerate hooks
//! (DESIGN.md §5.12).
//!
//! Each option's value type implements [`OptionType`]. The trait is
//! the single source of truth: `parse` validates user input,
//! `format` round-trips it back to a string, `enumerate` enumerates
//! valid string forms for completion (`:set foo=<Tab>`),
//! `name_forms` enumerates *option-name* alternates for completion
//! of the option NAME itself (vim's `noNAME` for booleans).
//!
//! Foreign primitive types (`bool`, `i64`, `String`) impl this in
//! the crate's own module to avoid orphan-rule problems. Renderer-
//! specific types (`FoldMethod`, `Color`) impl `OptionType` from
//! their owning crate using the trait re-exported here.

/// One enumerated value of an [`OptionType`], paired with its
/// short help text. Returned by [`OptionType::enumerate_with_docs`]
/// for the cmdline-completion marginalia column (slice
/// `3c.unify.option-doc-annotator`). `doc` is the empty string
/// when the type doesn't provide per-value documentation; the
/// default impl of `enumerate_with_docs` returns one of these per
/// `enumerate()` form with an empty doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumeratedValue {
    pub form: &'static str,
    pub doc: &'static str,
}

/// Surface contract for any value a typed [`crate::option::Option`] can
/// hold. Implementors describe how their type round-trips through
/// the user-facing `:set foo=value` syntax.
pub trait OptionType: Sized + Clone + Send + Sync + 'static {
    /// Parse the right-hand side of `:set name=value`. Errors are
    /// surfaced verbatim through the cmdline echo, so the message
    /// should read well to the user (mention valid forms when the
    /// space is finite). The parser MUST round-trip with `format`:
    /// `T::parse(&v.format()) == Ok(v)` for every `v`.
    fn parse(s: &str) -> Result<Self, String>;

    /// Render the current value back to a string (`:set foo?` echo,
    /// the customize buffer view, value comparison). Round-trips
    /// with [`Self::parse`].
    fn format(&self) -> String;

    /// Short type label for `:describe-option` (`"boolean"`,
    /// `"foldmethod"`, etc.). Used in help bodies and the customize
    /// buffer view.
    fn type_label() -> &'static str;

    /// Optional: enumerate valid string forms for `:set foo=<Tab>`
    /// completion. `None` means "free-form" (no enumerable space --
    /// integers, file paths, free strings). The values are
    /// `&'static str` to keep the completion source allocation-free
    /// in the common case.
    fn enumerate() -> Option<Vec<&'static str>> {
        None
    }

    /// Optional: enumerate valid string forms WITH per-value doc
    /// strings for the cmdline-completion marginalia column.
    /// Default impl wraps [`Self::enumerate`] with empty docs;
    /// types with rich help override.
    ///
    /// Slice `3c.unify.option-doc-annotator`: cmdline completion
    /// surfaces these as right-aligned marginalia. Example
    /// (foldmethod):
    ///   `marker        Fold by markers ({{{...}}})`
    ///   `indent        Fold by indent level`
    ///   `manual        User-defined folds only`
    ///   `syntax        Folds from tree-sitter syntax tree`
    fn enumerate_with_docs() -> Option<Vec<EnumeratedValue>> {
        Self::enumerate().map(|forms| {
            forms
                .into_iter()
                .map(|form| EnumeratedValue { form, doc: "" })
                .collect()
        })
    }

    /// Optional: alternative *name* forms this type accepts for the
    /// option name itself. Used by completion to surface the name
    /// alongside its negation (`:set nu` / `:set nonu`). Default:
    /// no extra forms. Booleans return `[format!("no{canonical}")]`.
    fn name_forms(_canonical: &str) -> Vec<String> {
        Vec::new()
    }

    /// Whether this type's option supports `:set noFOO`. `bool`
    /// returns `true`; everything else `false`. Drives the negate
    /// path in the cmdline parser.
    fn is_bool() -> bool {
        false
    }

    /// Whether this option's value is list-shaped — i.e. a TOML
    /// **array** at config-load time should be joined into the
    /// delimited string [`Self::parse`] accepts, rather than rejected
    /// as "not applicable to a scalar option". `false` for every
    /// scalar type (the default); `true` for list types like
    /// [`crate::ModelineZone`]. Drives `loader::apply_array` (ML.5).
    /// The `:set` cmdline path is unaffected — it always passes a
    /// string and never sees a TOML array.
    fn accepts_list() -> bool {
        false
    }

    /// Negation value (only meaningful for [`Self::is_bool`] true).
    /// Default: returns `Err` -- the registry guards by checking
    /// `is_bool()` first, so callers that respect the contract
    /// won't observe this. The `bool` impl returns `Ok(false)`.
    fn try_negation_value() -> Result<Self, String> {
        Err(format!(
            "option type `{}` does not support negation",
            Self::type_label()
        ))
    }

    // ── TC.1: the shape of this type's value, as data ─────────────
    //
    // Defaulted, so no existing option declaration changes. A type
    // that already enumerates its forms for `:set foo=<Tab>` gets an
    // `enum` schema for free — which is most of the renderer-owned
    // types — and everything else falls back to the string form it
    // already round-trips through. Only `bool` and `i64` need to say
    // anything, and composite types override all three together.
    //
    // See `typed-configuration.md` §2.1 for why this is a re-base of
    // every option rather than a fourth option kind.

    /// Whether [`Self::enumerate`] is the COMPLETE set of valid values
    /// (a closed enum) or a completion hint over an open space.
    ///
    /// Default `false`, and the default is the point.
    /// [`Self::enumerate`]'s contract is "enumerate valid string forms
    /// for `:set foo=<Tab>` completion", which several types read as a
    /// *hint*: [`crate::ModelineZone`] advertises `auto` while
    /// accepting any comma-separated list, [`crate::ExpandHeight`]
    /// advertises `half`/`full` while also accepting a bare number, and
    /// [`crate::RootMarkers`] advertises the default markers while
    /// accepting any of them. That ambiguity was harmless while
    /// `enumerate` only fed a completion popup; TC.1 gave it a second
    /// consumer that draws a conclusion from it, so it has to be named.
    ///
    /// Deriving the schema from `enumerate` alone would have described
    /// three open-ended types as closed sets, and `:customize` would
    /// then offer a picker of one value for an option that accepts
    /// arbitrary lists. Opting IN is one line per type and cannot be
    /// wrong by omission.
    fn enumerate_is_exhaustive() -> bool {
        false
    }

    /// The declared shape of this type's values.
    ///
    /// Default: `enum(...)` for a type whose enumeration is closed
    /// ([`Self::enumerate_is_exhaustive`]), else `scalar(string)` — the
    /// honest description of a type whose only contract is
    /// `parse`/`format`.
    fn schema() -> crate::ConfigSchema {
        match Self::enumerate() {
            Some(forms) if Self::enumerate_is_exhaustive() => {
                crate::ConfigSchema::Enum(forms.into_iter().map(str::to_string).collect())
            }
            _ => crate::ConfigSchema::string(),
        }
    }

    /// This value as a schema-shaped tree.
    ///
    /// Default: the `format()` string, which matches the default
    /// `schema()`. MUST agree with `schema()` — a type that overrides
    /// one and not the other is a silently lossy option, which is what
    /// the round-trip test in this module exists to catch.
    fn to_value(&self) -> crate::ConfigValue {
        crate::ConfigValue::Str(self.format())
    }

    /// Rebuild from a tree. Default: the inverse of [`Self::to_value`],
    /// deferring to `parse` so a type's validation rules apply on this
    /// path exactly as they do on `:set`.
    fn from_value(value: &crate::ConfigValue) -> Result<Self, String> {
        match value.as_str() {
            Some(s) => Self::parse(s),
            None => Err(format!(
                "expected {}, got {}",
                Self::type_label(),
                value.kind_label()
            )),
        }
    }
}

// --------------------------------------------------------------
// Primitive impls. These live here (rather than in their owning
// crate) because the orphan rule forbids `impl OptionType for bool`
// elsewhere -- we own the trait, the std types are foreign.
// --------------------------------------------------------------

impl OptionType for bool {
    fn parse(s: &str) -> Result<bool, String> {
        // Accept the legacy `on`/`off` / `1`/`yes` / `0`/`no`
        // forms so existing config files keep parsing; the
        // user-facing surface (error message, completion)
        // advertises only `true`/`false` to keep the typing
        // surface minimal.
        match s {
            "true" | "on" | "1" | "yes" => Ok(true),
            "false" | "off" | "0" | "no" => Ok(false),
            other => Err(format!("expected boolean (`true`/`false`), got `{other}`")),
        }
    }

    fn format(&self) -> String {
        // Match the existing `:set foo?` echo so the migration to
        // typed options doesn't change user-visible cmdline output.
        // `bool::to_string` returns `"true"`/`"false"`; we keep that
        // exact wording. Vim's actual convention (`number` /
        // `nonumber` with no `=value` echo) is a follow-up choice
        // when we restructure echo output.
        self.to_string()
    }

    fn type_label() -> &'static str {
        "boolean"
    }

    fn enumerate() -> Option<Vec<&'static str>> {
        // `:set foo=<Tab>` shows only the canonical forms.
        // The parser still accepts `on`/`off`/`1`/`0`/`yes`/`no`
        // for back-compat with hand-written config files, but
        // surfacing four equivalent forms in completion was
        // confusing -- the popup pretended each was a distinct
        // value.
        Some(vec!["true", "false"])
    }

    fn enumerate_with_docs() -> Option<Vec<EnumeratedValue>> {
        // Slice `3c.unify.option-docs-builtin`: per-value
        // marginalia for boolean options. Negation via
        // `:set noNAME` is handled by `name_forms` above.
        Some(vec![
            EnumeratedValue {
                form: "true",
                doc: "Enable this option",
            },
            EnumeratedValue {
                form: "false",
                doc: "Disable this option",
            },
        ])
    }

    fn name_forms(canonical: &str) -> Vec<String> {
        vec![format!("no{canonical}")]
    }

    fn is_bool() -> bool {
        true
    }

    fn try_negation_value() -> Result<Self, String> {
        Ok(false)
    }

    // TC.1: a boolean is a boolean, not the string "true". The default
    // would have described it as an enum of its two completion forms,
    // which reads fine and would make `:customize` offer a two-item
    // picker where a checkbox belongs.
    fn schema() -> crate::ConfigSchema {
        crate::ConfigSchema::bool()
    }

    fn to_value(&self) -> crate::ConfigValue {
        crate::ConfigValue::Bool(*self)
    }

    fn from_value(value: &crate::ConfigValue) -> Result<Self, String> {
        value
            .as_bool()
            .ok_or_else(|| format!("expected boolean, got {}", value.kind_label()))
    }
}

impl OptionType for i64 {
    fn parse(s: &str) -> Result<i64, String> {
        s.parse::<i64>()
            .map_err(|e| format!("expected integer, got `{s}`: {e}"))
    }

    fn format(&self) -> String {
        self.to_string()
    }

    fn type_label() -> &'static str {
        "integer"
    }

    // TC.1: an integer crosses as an integer. The TOML loader is the
    // caller that cares — `tabstop = 4` is a TOML integer, and turning
    // it into "4" only to parse it back was the round-trip this removes.
    fn schema() -> crate::ConfigSchema {
        crate::ConfigSchema::int()
    }

    fn to_value(&self) -> crate::ConfigValue {
        crate::ConfigValue::Int(*self)
    }

    fn from_value(value: &crate::ConfigValue) -> Result<Self, String> {
        value
            .as_int()
            .ok_or_else(|| format!("expected integer, got {}", value.kind_label()))
    }
}

impl OptionType for String {
    fn parse(s: &str) -> Result<String, String> {
        Ok(s.to_string())
    }

    fn format(&self) -> String {
        self.clone()
    }

    fn type_label() -> &'static str {
        "string"
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn bool_parse_accepts_synonyms() {
        for s in ["on", "true", "1", "yes"] {
            assert_eq!(bool::parse(s), Ok(true));
        }
        for s in ["off", "false", "0", "no"] {
            assert_eq!(bool::parse(s), Ok(false));
        }
    }

    #[test]
    fn bool_parse_rejects_garbage_with_helpful_message() {
        let e = bool::parse("maybe").unwrap_err();
        assert!(e.contains("expected boolean"), "got `{e}`");
        assert!(e.contains("maybe"), "got `{e}`");
    }

    #[test]
    fn bool_round_trip_through_format_and_parse() {
        assert_eq!(bool::parse(&true.format()), Ok(true));
        assert_eq!(bool::parse(&false.format()), Ok(false));
    }

    #[test]
    fn bool_format_matches_legacy_true_false_text() {
        // Migration constraint: `:set foo?` echo must read
        // identically to the pre-migration cmdline output.
        assert_eq!(true.format(), "true");
        assert_eq!(false.format(), "false");
    }

    #[test]
    fn bool_name_forms_includes_negation() {
        assert_eq!(bool::name_forms("number"), vec!["nonumber"]);
    }

    #[test]
    fn bool_is_bool_marker() {
        assert!(bool::is_bool());
        assert!(!i64::is_bool());
        assert!(!String::is_bool());
    }

    #[test]
    fn bool_negation_value_is_false() {
        assert_eq!(<bool as OptionType>::try_negation_value(), Ok(false));
    }

    #[test]
    fn int_negation_value_returns_err() {
        assert!(<i64 as OptionType>::try_negation_value().is_err());
    }

    #[test]
    fn int_parse_round_trip() {
        assert_eq!(i64::parse("42"), Ok(42));
        assert_eq!(i64::parse(&(-7i64).format()), Ok(-7));
        assert!(i64::parse("not-a-number").is_err());
    }

    #[test]
    fn int_no_enumeration() {
        assert!(i64::enumerate().is_none());
        assert!(i64::name_forms("tabstop").is_empty());
    }

    #[test]
    fn string_pass_through() {
        assert_eq!(String::parse("hello"), Ok("hello".into()));
        assert_eq!("hello".to_string().format(), "hello");
        assert!(String::enumerate().is_none());
    }
}
