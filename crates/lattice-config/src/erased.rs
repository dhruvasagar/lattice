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
    fn name(&self) -> &'static str;
    fn aliases(&self) -> &'static [&'static str];
    fn doc(&self) -> &'static str;
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
    fn parse_to_erased(
        &self,
        value: &str,
    ) -> Result<Arc<dyn Any + Send + Sync>, String>;

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
    fn format_erased_value(&self, value: &std::sync::Arc<dyn std::any::Any + Send + Sync>) -> std::option::Option<String>;

    /// Project back to the concrete type for typed-handle reads.

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
}

impl<T: OptionType> ErasedOption for Option<T> {
    fn name(&self) -> &'static str {
        self.name
    }

    fn aliases(&self) -> &'static [&'static str] {
        self.aliases
    }

    fn doc(&self) -> &'static str {
        self.doc
    }

    fn type_label(&self) -> &'static str {
        T::type_label()
    }

    fn parse_and_set(&self, value: &str) -> Result<(), String> {
        let parsed = T::parse(value)?;
        self.set(parsed)
    }

    fn parse_to_erased(
        &self,
        value: &str,
    ) -> Result<Arc<dyn Any + Send + Sync>, String> {
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
        T::name_forms(self.name)
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

    fn format_erased_value(&self, value: &std::sync::Arc<dyn std::any::Any + Send + Sync>) -> std::option::Option<String> {
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
