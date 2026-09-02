//! Typed [`Option<T>`] spec + the [`OptionHandle<T>`] consumers
//! use for hot-path reads.
//!
//! Each option owns its current value behind an [`ArcSwap<T>`].
//! Reads are wait-free pointer loads; writes go through
//! [`Option::set`] (or, more commonly, via the registry's
//! `parse_and_set` driven by `:set foo=value`).

use std::borrow::Cow;
use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::option_type::OptionType;

/// Post-parse validator. Runs after [`OptionType::parse`] succeeds
/// and before commit; returning `Err(_)` cancels the set with
/// that message.
pub type ValidateFn<T> = fn(&T) -> Result<(), String>;

/// A typed config option. The user-visible name + metadata,
/// plus the cell holding the current value. Constructed once and
/// handed to [`crate::ConfigRegistry::register`], which returns a
/// typed [`OptionHandle`] consumers use to read / write.
pub struct Option<T: OptionType> {
    /// PL8.F: `Cow<'static, str>` so a builtin passes a zero-cost
    /// `Cow::Borrowed` string literal while a plugin-contributed option passes a
    /// `Cow::Owned(String)` that frees with the entry on
    /// `ConfigRegistry::unregister` — replacing the old `Box::leak` intern.
    pub(crate) name: Cow<'static, str>,
    pub(crate) aliases: &'static [&'static str],
    pub(crate) doc: Cow<'static, str>,
    /// Optional post-parse validator. Runs *after* `T::parse`
    /// succeeds and *before* the value is committed to the cell.
    /// Returning `Err(_)` cancels the set with that message;
    /// `Ok(())` commits. Used for range checks (`tabstop` 1..=32),
    /// invariants between options, etc.
    pub(crate) validate: std::option::Option<ValidateFn<T>>,
    /// Pre-formatted default value. Captured at construction time
    /// so `:describe-option` can show "default: X" without storing
    /// `T` separately or re-running `format()` against the cell
    /// (which holds the *current*, not the default, value).
    pub(crate) default_formatted: String,
    /// TC.3: a shape declared per OPTION rather than per type.
    ///
    /// `None` for every option whose type knows its own shape, which is all of
    /// them that are written in Rust — `OptionType::schema()` answers and this
    /// field stays empty. It exists for the case the type cannot cover: a
    /// PLUGIN's structured option, whose shape arrives at registration as data
    /// and therefore cannot be a static method on `ConfigValue`.
    ///
    /// The schema lives here rather than inside the value because a value that
    /// carried its own shape could not survive `OptionType::from_value`, which
    /// is a static function with no access to the option being set. Metadata
    /// about an option belongs beside its doc and its default.
    pub(crate) schema: std::option::Option<crate::ConfigSchema>,
    pub(crate) cell: ArcSwap<T>,
}

impl<T: OptionType + std::fmt::Debug> std::fmt::Debug for Option<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Option")
            .field("name", &self.name)
            .field("aliases", &self.aliases)
            .field("type_label", &T::type_label())
            .field("current", &**self.cell.load())
            .finish_non_exhaustive()
    }
}

impl<T: OptionType> Option<T> {
    /// Build a typed option with a default value. Most options use
    /// the [`OptionBuilder`] (see [`Option::builder`]) instead so
    /// optional fields don't need defaults restated.
    pub fn new(
        name: impl Into<Cow<'static, str>>,
        default: T,
        doc: impl Into<Cow<'static, str>>,
    ) -> Self {
        let default_formatted = default.format();
        Self {
            name: name.into(),
            aliases: &[],
            doc: doc.into(),
            validate: None,
            default_formatted,
            schema: None,
            cell: ArcSwap::from_pointee(default),
        }
    }

    /// Builder entry point. Pattern:
    /// ```ignore
    /// Option::<i64>::builder("tabstop", 8, "Tab visual width.")
    ///     .aliases(&["ts"])
    ///     .validate(|i| (1..=32).contains(i)
    ///         .then_some(())
    ///         .ok_or_else(|| format!("out of range: {i}")))
    ///     .build()
    /// ```
    pub fn builder(
        name: impl Into<Cow<'static, str>>,
        default: T,
        doc: impl Into<Cow<'static, str>>,
    ) -> OptionBuilder<T> {
        OptionBuilder {
            name: name.into(),
            aliases: &[],
            doc: doc.into(),
            default,
            validate: None,
        }
    }

    /// TC.3: an option whose shape is declared rather than derived — the
    /// plugin-contributed structured option.
    ///
    /// `default` is NOT validated here; the caller
    /// (`config_host::register_structured_option`) checks it against `schema`
    /// first, so a declaration that does not fit registers nothing at all
    /// rather than producing an option that exists and cannot hold a legal
    /// value.
    pub fn structured(
        name: impl Into<Cow<'static, str>>,
        schema: crate::ConfigSchema,
        default: T,
        doc: impl Into<Cow<'static, str>>,
    ) -> Self {
        let mut out = Self::new(name, default, doc);
        out.schema = Some(schema);
        out
    }

    /// The option's declared shape: what was given to [`Self::structured`], or
    /// the type's own [`crate::OptionType::schema`].
    pub fn declared_schema(&self) -> crate::ConfigSchema {
        self.schema.clone().unwrap_or_else(T::schema)
    }

    /// Borrows the option's name. PL8.F: was `-> &'static str` when the field was
    /// a leaked `&'static str`; a `Cow` field can only lend a `&str` bound to
    /// `&self`, so callers that need an owned name `.to_owned()` it (registry).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Wait-free read of the current value. The returned `Arc<T>`
    /// is stable for the caller's frame; concurrent writers may
    /// publish a newer value, observed by the next call.
    pub fn get(&self) -> Arc<T> {
        self.cell.load_full()
    }

    /// Borrow the current value through a closure. Avoids the
    /// `Arc::clone` cost of [`Self::get`] for one-shot reads --
    /// useful in render hot paths that read+drop in the same
    /// statement.
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.cell.load())
    }

    /// Set the current value, running the (private) `validate` closure first.
    /// Returns the validator's error verbatim on rejection;
    /// otherwise commits and returns `Ok(())`.
    pub fn set(&self, value: T) -> Result<(), String> {
        if let Some(v) = self.validate
            && let Err(e) = v(&value)
        {
            return Err(e);
        }
        self.cell.store(Arc::new(value));
        Ok(())
    }
}

/// Fluent constructor for [`Option<T>`]. See [`Option::builder`].
pub struct OptionBuilder<T: OptionType> {
    name: Cow<'static, str>,
    aliases: &'static [&'static str],
    doc: Cow<'static, str>,
    default: T,
    validate: std::option::Option<ValidateFn<T>>,
}

impl<T: OptionType> OptionBuilder<T> {
    pub fn aliases(mut self, aliases: &'static [&'static str]) -> Self {
        self.aliases = aliases;
        self
    }

    pub fn validate(mut self, f: ValidateFn<T>) -> Self {
        self.validate = Some(f);
        self
    }

    pub fn build(self) -> Option<T> {
        let default_formatted = self.default.format();
        Option {
            schema: None,
            name: self.name,
            aliases: self.aliases,
            doc: self.doc,
            validate: self.validate,
            default_formatted,
            cell: ArcSwap::from_pointee(self.default),
        }
    }
}

/// Typed pointer into the registry. Returned by
/// [`crate::ConfigRegistry::register`]; passed back to
/// [`crate::ConfigRegistry::get`] / [`crate::ConfigRegistry::set`]
/// for type-safe access without string lookups.
///
/// `Copy` so callers can stash handles as plain fields and pass
/// them around freely. Internally an opaque index +
/// [`std::marker::PhantomData<T>`] -- the registry validates the
/// index on each access.
pub struct OptionHandle<T: OptionType> {
    pub(crate) idx: usize,
    pub(crate) _ty: std::marker::PhantomData<fn() -> T>,
}

impl<T: OptionType> OptionHandle<T> {
    pub(crate) fn new(idx: usize) -> Self {
        Self {
            idx,
            _ty: std::marker::PhantomData,
        }
    }

    /// Raw index for telemetry / debugging only. Two handles of
    /// the same `T` with the same `idx` refer to the same option.
    pub fn raw(self) -> usize {
        self.idx
    }
}

impl<T: OptionType> Clone for OptionHandle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: OptionType> Copy for OptionHandle<T> {}

impl<T: OptionType> std::fmt::Debug for OptionHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OptionHandle<{}>({})", T::type_label(), self.idx)
    }
}

impl<T: OptionType> PartialEq for OptionHandle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.idx == other.idx
    }
}

impl<T: OptionType> Eq for OptionHandle<T> {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn option_builder_constructs_with_aliases_and_validator() {
        let o: Option<i64> = Option::<i64>::builder("tabstop", 8, "tab width")
            .aliases(&["ts"])
            .validate(|i| {
                if (1..=32).contains(i) {
                    Ok(())
                } else {
                    Err(format!("out of range: {i}"))
                }
            })
            .build();
        assert_eq!(o.name, "tabstop");
        assert_eq!(o.aliases, &["ts"]);
        assert_eq!(*o.get(), 8);
        assert!(o.set(4).is_ok());
        assert_eq!(*o.get(), 4);
    }

    #[test]
    fn validator_rejects_out_of_range() {
        let o: Option<i64> = Option::<i64>::builder("ts", 8, "")
            .validate(|i| {
                if (1..=32).contains(i) {
                    Ok(())
                } else {
                    Err(format!("out of range: {i}"))
                }
            })
            .build();
        let err = o.set(99).unwrap_err();
        assert!(err.contains("out of range"), "got `{err}`");
        assert_eq!(*o.get(), 8, "value must not change on validator reject");
    }

    #[test]
    fn with_closure_borrows_without_clone() {
        let o: Option<String> = Option::new("name", "hello".into(), "doc");
        let len = o.with(|s| s.len());
        assert_eq!(len, 5);
    }

    #[test]
    fn option_handle_is_copy_and_eq() {
        let h: OptionHandle<i64> = OptionHandle::new(7);
        let h2 = h;
        assert_eq!(h, h2);
        assert_eq!(h.raw(), 7);
    }
}
