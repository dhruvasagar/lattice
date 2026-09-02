//! Type-erased view of [`crate::option::Option<T>`] for the registry to
//! store heterogeneous specs in a single `Vec`.
//!
//! Consumers that know the type at compile time use
//! [`crate::option::OptionHandle<T>`] for direct typed access. Consumers
//! that only have a runtime name (cmdline `:set foo=bar`, the
//! customize buffer view, plugin introspection) go through
//! [`ErasedOption`].

use std::any::Any;
use std::sync::Arc;

use crate::option::Option;
use crate::option_type::OptionType;

/// Type-erased operations every typed [`Option<T>`] supports. The
/// registry stores `Arc<dyn ErasedOption>` in a `Vec` indexed by
/// the private `idx` field of [`crate::option::OptionHandle`]; `parse_and_set_by_name`
/// drives this trait when the user types `:set name=value`.
///
/// `as_any` is the canonical Rust idiom for downcasting back to
/// the concrete `Option<T>` when a typed handle reads. Required
/// rather than auto-derived so the bound stays explicit.
pub trait ErasedOption: Send + Sync {
    fn name(&self) -> &str;
    fn aliases(&self) -> &'static [&'static str];
    fn doc(&self) -> &str;
    fn type_label(&self) -> &'static str;

    /// Parse `value` against the option's [`OptionType`], run the
    /// post-parse validator, store the new value. Returns the
    /// concatenated error if either stage fails.
    fn parse_and_set(&self, value: &str) -> Result<(), String>;

    /// Parse `value` against the option's [`OptionType`] and run the
    /// validator, but **do not write to the option's storage**. Returns
    /// the typed value erased as `Arc<dyn Any + Send + Sync>`. Used by
    /// `ConfigRegistry::parse_for_buffer_local` to produce an
    /// `OptionOverride` for the buffer-local layer (BL.1) without
    /// touching the global registry.
    fn parse_to_erased(&self, value: &str) -> Result<Arc<dyn Any + Send + Sync>, String>;

    /// Render the current value (`:set foo?` echo, customize
    /// buffer view).
    fn get_formatted(&self) -> String;

    /// Render the *default* value (the value the option held at
    /// registration time). Used by `:describe-option` to show
    /// "default: X" alongside "current: Y".
    fn default_formatted(&self) -> &str;

    /// Enumerate valid string values for `:set foo=<Tab>`. `None`
    /// means free-form (no completion).
    fn enumerate_values(&self) -> std::option::Option<Vec<&'static str>>;

    /// Enumerate valid string values + per-value doc strings.
    /// Slice `3c.unify.option-doc-annotator` — drives the
    /// marginalia column on `:set foo=<Tab>` completion. `None`
    /// means free-form. Default impl wraps `enumerate_values`
    /// with empty docs; rich-help types override on
    /// `OptionType::enumerate_with_docs`.
    fn enumerate_values_with_docs(
        &self,
    ) -> std::option::Option<Vec<crate::option_type::EnumeratedValue>>;

    /// Alternate name forms the cmdline accepts (`noNAME` for
    /// booleans). Drives `:set <Tab>` enumeration.
    fn name_forms(&self) -> Vec<String>;

    /// Whether this option supports `:set noNAME`. Booleans true,
    /// everything else false.
    fn is_bool(&self) -> bool;

    /// Whether this option's value is list-shaped (ML.5). The config
    /// loader consults this to decide whether a TOML **array** for this
    /// key should be joined into the option's delimited parse form
    /// (`true`) or rejected as a scalar/array mismatch (`false`).
    /// Forwards to [`OptionType::accepts_list`].
    fn accepts_list(&self) -> bool;

    /// Set the option to its negation value (only meaningful when
    /// [`Self::is_bool`] is true). Used by the `:set noNAME` path.
    /// Returns `Err` if called on a non-bool option (registry
    /// guards this; this is defense in depth).
    fn negate(&self) -> Result<(), String>;

    /// Format an already-erased value of this option's type. Used by
    /// the query echo path (`:set name?`) to display the resolved value
    /// when the concrete type isn't known at the call site. Returns
    /// `None` if `value` doesn't downcast to this option's `Value` type
    /// (caller falls back to `get_formatted()` in that case).
    fn format_erased_value(
        &self,
        value: &std::sync::Arc<dyn std::any::Any + Send + Sync>,
    ) -> std::option::Option<String>;

    /// Project back to the concrete type for typed-handle reads.
    ///
    /// Implementors return `self`. Crate-private trait methods
    /// can't be expressed cleanly across the boundary, so we keep
    /// this in the public surface but document it as
    /// implementation-detail.
    fn as_any(&self) -> &dyn Any;

    /// Return the option's *current* value as an erased
    /// `Arc<dyn Any + Send + Sync>`. Used by
    /// [`crate::ConfigRegistry::bootstrap_resolved_with_current_values`]
    /// to seed the [`crate::ResolvedOptions`] cache with each
    /// option's current registry value (resolution layer 5)
    /// before mode/buffer-local layers overlay on top.
    fn current_value_erased(&self) -> Arc<dyn Any + Send + Sync>;

    // ── TC.1: the shape, and values in that shape ─────────────────
    //
    // The runtime-name surface every consumer that does not know the
    // type at compile time already goes through — `:set`, the TOML
    // loader, plugin introspection, `:describe-option`. Before this it
    // offered a `type_label(): &str` and a formatted `String`, which is
    // not enough to render a composite, validate a field, or write a
    // value back to TOML. See `typed-configuration.md`.

    /// This option's declared shape.
    fn schema(&self) -> crate::ConfigSchema;

    /// The current value as a schema-shaped tree.
    fn get_value(&self) -> crate::ConfigValue;

    /// Validate `value` against [`Self::schema`], then commit it.
    ///
    /// Validation runs against the SCHEMA first, so the error carries a
    /// path (`target.file: expected string, got integer`), and only
    /// then through the type's own conversion and post-parse validator
    /// — the same two stages `parse_and_set` runs, in the same order,
    /// so the tree path and the string path cannot disagree about
    /// whether a value is acceptable.
    fn set_value(&self, value: &crate::ConfigValue) -> Result<(), String>;
}

impl<T: OptionType> ErasedOption for Option<T> {
    fn name(&self) -> &str {
        &self.name
    }

    fn aliases(&self) -> &'static [&'static str] {
        self.aliases
    }

    fn doc(&self) -> &str {
        &self.doc
    }

    fn type_label(&self) -> &'static str {
        T::type_label()
    }

    fn parse_and_set(&self, value: &str) -> Result<(), String> {
        match T::parse(value) {
            Ok(parsed) => self.set(parsed),
            // TC.7: a `list<string>` option typed at the command line.
            //
            // `:set org.agenda-files=~/org` is a natural thing to type and was
            // a working one before those options declared list schemas; after,
            // the text is not TOML and `parse` refuses it. Requiring
            // `value = ["~/org"]` on a command line would be an implementation
            // detail leaking into the one surface that is meant to be terse.
            //
            // So a failed parse on a string-list option falls back to the ML.5
            // rule its predecessors used: split on commas. Only on the FAILURE
            // path, so a well-formed TOML array still means what it says, and
            // only for `list<string>` — a list of records has no delimited
            // spelling worth inventing, and the original error is the honest
            // answer there.
            //
            // Newlines separate as well as commas, because a `:set` spec can
            // arrive from somewhere other than a command line and splitting
            // only on commas would make one of those two spellings silently
            // produce a single element containing newlines.
            Err(err) => match self.declared_schema() {
                crate::ConfigSchema::List(inner)
                    if matches!(
                        inner.as_ref(),
                        crate::ConfigSchema::Scalar(crate::ScalarKind::Str)
                    ) =>
                {
                    let items: Vec<crate::ConfigValue> = value
                        .split([',', '\n'])
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(|s| crate::ConfigValue::Str(s.to_string()))
                        .collect();
                    self.set_value(&crate::ConfigValue::List(items))
                }
                _ => Err(err),
            },
        }
    }

    fn parse_to_erased(&self, value: &str) -> Result<Arc<dyn Any + Send + Sync>, String> {
        let parsed = T::parse(value)?;
        // Run the validator (if any) without writing to storage.
        if let Some(v) = self.validate
            && let Err(e) = v(&parsed)
        {
            return Err(e);
        }
        Ok(Arc::new(parsed) as Arc<dyn Any + Send + Sync>)
    }

    fn get_formatted(&self) -> String {
        self.with(|v| v.format())
    }

    fn default_formatted(&self) -> &str {
        &self.default_formatted
    }

    fn enumerate_values(&self) -> std::option::Option<Vec<&'static str>> {
        T::enumerate()
    }

    fn enumerate_values_with_docs(
        &self,
    ) -> std::option::Option<Vec<crate::option_type::EnumeratedValue>> {
        T::enumerate_with_docs()
    }

    fn name_forms(&self) -> Vec<String> {
        T::name_forms(self.name.as_ref())
    }

    fn is_bool(&self) -> bool {
        T::is_bool()
    }

    fn accepts_list(&self) -> bool {
        T::accepts_list()
    }

    fn negate(&self) -> Result<(), String> {
        let neg = T::try_negation_value()
            .map_err(|e| format!("option `{}` does not support `:set noNAME`: {e}", self.name))?;
        self.set(neg)
    }

    fn format_erased_value(
        &self,
        value: &std::sync::Arc<dyn std::any::Any + Send + Sync>,
    ) -> std::option::Option<String> {
        value.clone().downcast::<T>().ok().map(|v| v.format())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn current_value_erased(&self) -> Arc<dyn Any + Send + Sync> {
        // ArcSwap::load_full returns Arc<T>; coerce to
        // Arc<dyn Any + Send + Sync> via Rust's unsized
        // coercion (T: 'static + Send + Sync from OptionType
        // bounds satisfies Any + Send + Sync).
        let v: Arc<T> = self.cell.load_full();
        v
    }

    fn schema(&self) -> crate::ConfigSchema {
        // The option's DECLARED shape: what a plugin gave at registration, or
        // the type's own answer. Not `T::schema()` directly — a structured
        // plugin option's shape is data, and a static method cannot know it.
        self.declared_schema()
    }

    fn get_value(&self) -> crate::ConfigValue {
        self.with(|v| v.to_value())
    }

    fn set_value(&self, value: &crate::ConfigValue) -> Result<(), String> {
        // Schema first: it is the stage that knows WHERE a composite
        // went wrong. `from_value` can only say that the whole tree is
        // the wrong shape, which for a list of records is no better
        // than the hand-rolled parsers this replaces.
        crate::schema::validate(&self.declared_schema(), value).map_err(|e| e.to_string())?;
        let parsed = T::from_value(value)?;
        self.set(parsed)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn erased_view_dispatches_through_trait_object() {
        let o: Option<bool> = Option::new("number", true, "doc");
        let erased: &dyn ErasedOption = &o;
        assert_eq!(erased.name(), "number");
        assert_eq!(erased.type_label(), "boolean");
        assert!(erased.is_bool());
        assert_eq!(erased.get_formatted(), "true");
        erased.parse_and_set("off").unwrap();
        assert_eq!(erased.get_formatted(), "false");
        erased.negate().unwrap();
        assert_eq!(erased.get_formatted(), "false");
    }

    #[test]
    fn erased_negate_rejects_non_bool() {
        let o: Option<i64> = Option::new("tabstop", 8, "");
        let erased: &dyn ErasedOption = &o;
        let err = erased.negate().unwrap_err();
        assert!(
            err.contains("does not support `:set noNAME`"),
            "got `{err}`"
        );
    }

    #[test]
    fn erased_parse_and_set_surfaces_type_errors() {
        let o: Option<bool> = Option::new("number", true, "");
        let erased: &dyn ErasedOption = &o;
        let err = erased.parse_and_set("maybe").unwrap_err();
        assert!(err.contains("expected boolean"));
    }

    #[test]
    fn erased_as_any_downcasts_back_to_concrete_option() {
        let o: Option<bool> = Option::new("number", true, "");
        let erased: &dyn ErasedOption = &o;
        let typed = erased.as_any().downcast_ref::<Option<bool>>();
        assert!(typed.is_some());
        let bad = erased.as_any().downcast_ref::<Option<i64>>();
        assert!(bad.is_none());
    }
}

#[cfg(test)]
mod string_list_set_tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::option::Option as ConfigOption;
    use crate::{ConfigSchema, ConfigValue};

    fn paths() -> ConfigOption<ConfigValue> {
        ConfigOption::structured(
            "org.agenda-files",
            ConfigSchema::list(ConfigSchema::string()),
            ConfigValue::List(Vec::new()),
            "Which files the agenda scans.",
        )
    }

    #[test]
    fn a_bare_path_is_a_one_element_list() {
        // `:set org.agenda-files=~/org` is a natural thing to type and was a
        // working one before the option declared a list schema. Requiring
        // `value = ["~/org"]` on a command line would be an implementation
        // detail leaking into the surface meant to be terse.
        let o = paths();
        let erased: &dyn ErasedOption = &o;
        erased.parse_and_set("~/org").unwrap();
        assert_eq!(
            erased.get_value(),
            ConfigValue::List(vec![ConfigValue::Str("~/org".into())])
        );
    }

    #[test]
    fn commas_or_newlines_separate_and_blanks_are_dropped() {
        let o = paths();
        let erased: &dyn ErasedOption = &o;
        erased.parse_and_set("~/org, ~/notes.org ,,").unwrap();
        assert_eq!(
            erased.get_value(),
            ConfigValue::List(vec![
                ConfigValue::Str("~/org".into()),
                ConfigValue::Str("~/notes.org".into()),
            ])
        );
        // A spec can arrive from somewhere other than a command line.
        erased.parse_and_set("~/org\n~/notes.org\n").unwrap();
        assert_eq!(
            erased.get_value(),
            ConfigValue::List(vec![
                ConfigValue::Str("~/org".into()),
                ConfigValue::Str("~/notes.org".into()),
            ])
        );
    }

    #[test]
    fn a_well_formed_toml_array_still_means_what_it_says() {
        // The fallback is on the FAILURE path only. A value that parses as
        // TOML must never be reinterpreted — `value = [...]` is the form
        // `format` emits, so a round-trip through `:set foo?` has to survive.
        let o = paths();
        let erased: &dyn ErasedOption = &o;
        erased
            .parse_and_set("value = [\"~/a\", \"~/b\"]")
            .expect("the TOML form still works");
        assert_eq!(
            erased.get_value(),
            ConfigValue::List(vec![
                ConfigValue::Str("~/a".into()),
                ConfigValue::Str("~/b".into()),
            ])
        );
        // …and the round-trip closes: whatever `format` writes, `parse_and_set`
        // reads back to the same value.
        let text = erased.get_formatted();
        let o2 = paths();
        let erased2: &dyn ErasedOption = &o2;
        erased2
            .parse_and_set(&text)
            .expect("its own output re-reads");
        assert_eq!(erased2.get_value(), erased.get_value());
    }

    #[test]
    fn a_record_list_keeps_the_parse_error_rather_than_being_split() {
        // A list of records has no delimited spelling worth inventing, and
        // splitting one on commas would turn a typo into a list of nonsense
        // strings that then fails validation somewhere else. The original
        // error is the honest answer.
        let o = ConfigOption::<ConfigValue>::structured(
            "org.capture-templates",
            ConfigSchema::list(ConfigSchema::record([crate::SchemaField::new(
                "key",
                ConfigSchema::string(),
                "",
            )])),
            ConfigValue::List(Vec::new()),
            "",
        );
        let erased: &dyn ErasedOption = &o;
        let err = erased
            .parse_and_set("t, n, r")
            .expect_err("no delimited form");
        assert!(err.contains("TOML"), "the parse error survives: {err}");
    }
}
