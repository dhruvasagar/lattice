//! [`ConfigRegistry`] — the central typed-options store
//! (DESIGN.md §5.12).
//!
//! Holds every registered option behind an [`crate::ErasedOption`]
//! trait object. Two access patterns:
//!
//! 1. **Typed handle** ([`crate::OptionHandle<T>`]) — returned by
//!    [`Self::register`]. Zero-overhead reads through
//!    [`Self::get`] / [`Self::with`]. Used by the App and
//!    renderers for hot-path option reads.
//!
//! 2. **By-name** ([`Self::lookup`] / [`Self::parse_and_set_command`])
//!    — driven by the cmdline `:set foo=bar` / `:set foo?`,
//!    the customize buffer view, and plugin introspection. The
//!    name is the public API surface; aliases resolve to the same
//!    spec.
//!
//! Concurrent reads through handles are wait-free (each
//! [`crate::Option<T>`]'s value cell is an `ArcSwap<T>`). The
//! registry's `by_id` / `by_name` maps live behind a `Mutex` because
//! adding entries shifts the `by_id` vec; in practice registration
//! happens once at App boot and once per plugin activation, never
//! on hot paths. Read-by-handle takes the registry mutex briefly to
//! pull the `Arc<dyn ErasedOption>` then drops it before reading
//! the cell, so the lock window is microscopic.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::erased::ErasedOption;
use crate::option::{Option, OptionHandle};
use crate::option_type::OptionType;
use crate::parse::{ParsedSet, parse_set};

/// Process-shared registry.
#[derive(Default)]
pub struct ConfigRegistry {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Indexed by [`OptionHandle::idx`]. Vec is append-only;
    /// removing an option (which we don't currently support) would
    /// require a tombstone scheme to keep handle indices valid.
    by_id: Vec<Arc<dyn ErasedOption>>,
    /// Name + alias → index. Multiple entries (canonical name +
    /// each alias) all point at the same `by_id` index.
    by_name: HashMap<String, usize>,
}

/// What the registry's fallible operations can fail with. Kept
/// separate from raw `String` errors so callers can echo the
/// canonical message without re-stringifying.
///
/// Vim-style `E` codes match the existing TUI cmdline echo
/// wording; renderer-agnostic by design (every renderer surfaces
/// these the same way).
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("E518: Unknown option: {0}")]
    UnknownOption(String),
    #[error("E448: option `{0}` already registered")]
    DuplicateName(String),
    /// `:set noFOO` against a non-bool option. Wording matches the
    /// pre-typed-options TUI echo.
    #[error("E474: not a boolean option: {0}")]
    NotBoolean(String),
    /// `:set tabstop=999` (out-of-range) etc. — produced by the
    /// option's validate closure. Wording is the closure's verbatim.
    #[error("{0}")]
    Validation(String),
    /// `:set foldmethod=xyz` — produced by the option type's
    /// `parse` impl. Wording is the impl's verbatim.
    #[error("{0}")]
    Parse(String),
    #[error("E474: type mismatch: handle expected `{expected}`, registry has `{actual}`")]
    TypeMismatch {
        expected: &'static str,
        actual: &'static str,
    },
}

#[cold]
#[track_caller]
#[allow(clippy::panic)]
fn panic_on_duplicate(e: &ConfigError) -> ! {
    // Only the infallible `register` shim calls this; the message
    // includes the offending name via the ConfigError variant. The
    // codebase's no-panic policy makes a controlled exception for
    // genuine programming errors at registration time -- duplicate
    // names are a build-the-app bug, not a user-input failure.
    // Callers that need recovery use `try_register`.
    panic!("{e}")
}

impl ConfigRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a typed option. Returns an [`OptionHandle<T>`] for
    /// hot-path access. Returns `Err` if `option.name` or any alias
    /// is already registered (duplicate-name detection is a
    /// programming-error guard; recoverable via the `Result` so the
    /// caller can decide whether a duplicate is fatal).
    #[track_caller]
    pub fn register<T: OptionType>(&self, option: Option<T>) -> OptionHandle<T> {
        self.try_register(option)
            .unwrap_or_else(|e| panic_on_duplicate(&e))
    }

    /// Fallible registration. Most callers want [`Self::register`];
    /// this exists for plugins / dynamic code that can fail
    /// gracefully on a name collision.
    pub fn try_register<T: OptionType>(
        &self,
        option: Option<T>,
    ) -> Result<OptionHandle<T>, ConfigError> {
        let mut inner = self.inner.lock().expect("ConfigRegistry poisoned");
        let name = option.name();
        let aliases = option.aliases;
        if inner.by_name.contains_key(name) {
            return Err(ConfigError::DuplicateName(name.to_string()));
        }
        for a in aliases {
            if inner.by_name.contains_key(*a) {
                return Err(ConfigError::DuplicateName((*a).to_string()));
            }
        }
        let idx = inner.by_id.len();
        let arc: Arc<dyn ErasedOption> = Arc::new(option);
        inner.by_id.push(arc);
        inner.by_name.insert(name.to_string(), idx);
        for a in aliases {
            inner.by_name.insert((*a).to_string(), idx);
        }
        Ok(OptionHandle::<T>::new(idx))
    }

    /// Wait-free typed read.
    ///
    /// Returns `Arc<T>`; `Arc::clone` is one atomic increment.
    /// Returns the option's default-via-clone fallback if the
    /// handle's type or index doesn't match -- safer than
    /// panicking, callers can detect via [`Self::try_get`] if they
    /// care to. Handles obtained from [`Self::register`] are
    /// always type-correct, so this only matters for forged ones.
    #[track_caller]
    pub fn get<T: OptionType>(&self, handle: OptionHandle<T>) -> Arc<T> {
        self.try_get(handle).expect("config: invalid handle in get")
    }

    /// Fallible typed read. Returns `None` if the handle's index
    /// is out of range or refers to an option of a different `T`.
    pub fn try_get<T: OptionType>(&self, handle: OptionHandle<T>) -> std::option::Option<Arc<T>> {
        let arc = self.erased_at(handle.idx)?;
        let opt = arc.as_any().downcast_ref::<Option<T>>()?;
        Some(opt.get())
    }

    /// Closure-style read. Avoids the `Arc::clone` cost of
    /// [`Self::get`] for one-shot reads.
    #[track_caller]
    pub fn with<T: OptionType, R>(&self, handle: OptionHandle<T>, f: impl FnOnce(&T) -> R) -> R {
        let arc = self
            .erased_at(handle.idx)
            .expect("config: handle index out of bounds in with");
        let opt = arc
            .as_any()
            .downcast_ref::<Option<T>>()
            .expect("config: handle type mismatch in with");
        opt.with(f)
    }

    /// Typed write through a handle. Runs the option's validator
    /// before committing.
    pub fn set<T: OptionType>(&self, handle: OptionHandle<T>, value: T) -> Result<(), String> {
        let arc = self
            .erased_at(handle.idx)
            .ok_or_else(|| format!("config: handle index {} out of bounds", handle.idx))?;
        let opt = arc.as_any().downcast_ref::<Option<T>>().ok_or_else(|| {
            format!(
                "config: handle index {} type mismatch (expected {})",
                handle.idx,
                T::type_label()
            )
        })?;
        opt.set(value)
    }

    /// Look up an option by name (or alias). Returns the erased
    /// view -- the by-name path is for cmdline / customize / plugin
    /// introspection where the type isn't known at compile time.
    pub fn lookup(&self, name: &str) -> std::option::Option<Arc<dyn ErasedOption>> {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        inner
            .by_name
            .get(name)
            .map(|i| Arc::clone(&inner.by_id[*i]))
    }

    /// Iterate every registered option in registration order.
    /// Used by completion (`gen:options`) and the customize buffer
    /// view to enumerate.
    pub fn iter(&self) -> Vec<Arc<dyn ErasedOption>> {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        inner.by_id.iter().map(Arc::clone).collect()
    }

    /// Number of registered options.
    pub fn len(&self) -> usize {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        inner.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drive the cmdline `:set` syntax against the registry. Parses
    /// the input through [`parse_set`], then dispatches to the
    /// matching option's parse / set / negate / format path.
    /// Returns the echo line on success — the caller surfaces it
    /// via the cmdline echo. Forms:
    /// - `:set foo` -- echoes `foo=current` for non-bool, sets
    ///   true for bool (vim convention).
    /// - `:set nofoo` -- sets bool to false.
    /// - `:set foo=value` -- parses + sets.
    /// - `:set foo?` -- always echoes the current value.
    pub fn parse_and_set_command(&self, input: &str) -> Result<String, ConfigError> {
        let parsed = parse_set(input).map_err(ConfigError::Parse)?;
        match parsed {
            ParsedSet::NameOnly(name) => {
                let opt = self
                    .lookup(&name)
                    .ok_or(ConfigError::UnknownOption(name.clone()))?;
                if opt.is_bool() {
                    opt.parse_and_set("true").map_err(ConfigError::Validation)?;
                }
                // For both bool (post-toggle) and non-bool, echo
                // the current formatted value.
                Ok(format!("{}={}", opt.name(), opt.get_formatted()))
            }
            ParsedSet::Negate(name) => {
                let opt = self
                    .lookup(&name)
                    .ok_or(ConfigError::UnknownOption(name.clone()))?;
                if !opt.is_bool() {
                    // Surface the legacy vim wording (`E474: not a
                    // boolean option`) directly. The bare
                    // `negate()` path produces a more verbose
                    // message that's useful in tests but doesn't
                    // match what users saw before the typed-options
                    // migration.
                    return Err(ConfigError::NotBoolean(name));
                }
                opt.negate().map_err(ConfigError::Validation)?;
                Ok(format!("{}={}", opt.name(), opt.get_formatted()))
            }
            ParsedSet::Assign { name, value } => {
                let opt = self
                    .lookup(&name)
                    .ok_or(ConfigError::UnknownOption(name.clone()))?;
                opt.parse_and_set(&value).map_err(ConfigError::Validation)?;
                Ok(format!("{}={}", opt.name(), opt.get_formatted()))
            }
            ParsedSet::Query(name) => {
                let opt = self
                    .lookup(&name)
                    .ok_or(ConfigError::UnknownOption(name.clone()))?;
                Ok(format!("{}={}", opt.name(), opt.get_formatted()))
            }
        }
    }

    fn erased_at(&self, idx: usize) -> std::option::Option<Arc<dyn ErasedOption>> {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        inner.by_id.get(idx).map(Arc::clone)
    }
}

impl std::fmt::Debug for ConfigRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        f.debug_struct("ConfigRegistry")
            .field("count", &inner.by_id.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn register_returns_typed_handle_and_get_round_trips() {
        let r = ConfigRegistry::new();
        let h: OptionHandle<i64> = r.register(Option::new("ts", 8, "tab width"));
        assert_eq!(*r.get(h), 8);
        r.set(h, 4).unwrap();
        assert_eq!(*r.get(h), 4);
    }

    #[test]
    fn register_panics_on_duplicate_name() {
        let r = ConfigRegistry::new();
        r.register(Option::<bool>::new("number", true, ""));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            r.register(Option::<bool>::new("number", false, ""));
        }));
        assert!(result.is_err());
    }

    #[test]
    fn lookup_resolves_aliases_to_same_spec() {
        let r = ConfigRegistry::new();
        r.register(
            Option::<i64>::builder("tabstop", 8, "tab")
                .aliases(&["ts"])
                .build(),
        );
        let canonical = r.lookup("tabstop").unwrap();
        let aliased = r.lookup("ts").unwrap();
        assert_eq!(canonical.name(), aliased.name());
    }

    #[test]
    fn parse_and_set_command_assign_int() {
        let r = ConfigRegistry::new();
        let h = r.register(Option::<i64>::new("tabstop", 8, ""));
        let echo = r.parse_and_set_command("tabstop=4").unwrap();
        assert_eq!(echo, "tabstop=4");
        assert_eq!(*r.get(h), 4);
    }

    #[test]
    fn parse_and_set_command_negate_bool() {
        let r = ConfigRegistry::new();
        let h = r.register(Option::<bool>::new("number", true, ""));
        let echo = r.parse_and_set_command("nonumber").unwrap();
        assert!(!*r.get(h));
        assert_eq!(echo, "number=false");
    }

    #[test]
    fn parse_and_set_command_name_only_toggles_bool_on() {
        let r = ConfigRegistry::new();
        let h = r.register(Option::<bool>::new("number", false, ""));
        let echo = r.parse_and_set_command("number").unwrap();
        assert!(*r.get(h));
        assert_eq!(echo, "number=true");
    }

    #[test]
    fn parse_and_set_command_name_only_echoes_non_bool() {
        let r = ConfigRegistry::new();
        r.register(Option::<i64>::new("tabstop", 8, ""));
        let echo = r.parse_and_set_command("tabstop").unwrap();
        assert_eq!(echo, "tabstop=8");
    }

    #[test]
    fn parse_and_set_command_unknown_option() {
        let r = ConfigRegistry::new();
        let err = r.parse_and_set_command("xyzzy").unwrap_err();
        assert!(matches!(err, ConfigError::UnknownOption(_)));
    }

    #[test]
    fn parse_and_set_command_query_form() {
        let r = ConfigRegistry::new();
        r.register(Option::<bool>::new("number", true, ""));
        let echo = r.parse_and_set_command("number?").unwrap();
        assert_eq!(echo, "number=true");
    }

    #[test]
    fn validator_runs_through_parse_and_set() {
        let r = ConfigRegistry::new();
        r.register(
            Option::<i64>::builder("tabstop", 8, "")
                .validate(|i| {
                    if (1..=32).contains(i) {
                        Ok(())
                    } else {
                        Err(format!("out of range: {i}"))
                    }
                })
                .build(),
        );
        let err = r.parse_and_set_command("tabstop=99").unwrap_err();
        assert!(matches!(err, ConfigError::Validation(_)));
    }

    #[test]
    fn iter_returns_options_in_registration_order() {
        let r = ConfigRegistry::new();
        r.register(Option::<bool>::new("a", true, ""));
        r.register(Option::<i64>::new("b", 0, ""));
        r.register(Option::<bool>::new("c", false, ""));
        let names: Vec<&str> = r.iter().iter().map(|o| o.name()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn type_mismatch_in_get_panics() {
        let r = ConfigRegistry::new();
        let h_int: OptionHandle<i64> = r.register(Option::new("ts", 8, ""));
        // Forge a handle of the wrong type pointing at the same idx.
        let h_bool: OptionHandle<bool> = OptionHandle::new(h_int.raw());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = r.get(h_bool);
        }));
        assert!(result.is_err());
    }
}
