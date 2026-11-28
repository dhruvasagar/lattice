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

use std::any::TypeId;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lattice_protocol::Event;

use crate::erased::ErasedOption;
use crate::option::{Option, OptionHandle};
use crate::option_decl::{OPTION_DECLS, OptionDecl};
use crate::option_type::OptionType;
use crate::parse::{ParsedSet, parse_set};

/// Sink the registry calls after every successful set so consumers
/// can react to typed-option changes through the §5.10 event bus.
/// Stored as a `Box<dyn Fn>` so `lattice-config` doesn't depend on
/// `lattice-runtime`'s `EventBus` directly -- the App wires
/// `event_bus.publish(event)` as the closure body at boot.
///
/// The publisher is invoked synchronously from the same thread
/// that drove the set; downstream subscribers should not assume
/// any particular thread context.
pub type EventPublisher = Arc<dyn Fn(Event) + Send + Sync>;

/// Process-shared registry.
#[derive(Default)]
pub struct ConfigRegistry {
    inner: Mutex<Inner>,
}

/// OC.11c: one failed assignment to a named option.
///
/// A failed assignment is a NO-OP — vim's rule, which lattice keeps — so the
/// option keeps whatever it had, and for one never successfully set that is
/// its registered default. Reading the value therefore cannot tell "the user
/// configured this and it did not parse" from "the user never configured
/// this". This record is what can.
///
/// **It is not a state the option carries.** The option has no such state; an
/// assignment errored, which is an event, and this is the record of that
/// event. It is dropped the moment a later assignment to the same option
/// succeeds, because at that point it describes something untrue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDiagnostic {
    /// The message the loader or this registry produced, verbatim. For a
    /// composite it carries the schema PATH (`[2].target.file: expected
    /// string, got integer`), which is the whole reason this is worth
    /// surfacing rather than a bare "it failed".
    pub message: String,
    /// The config file it came from, or `None` for a runtime `:set`. That is
    /// the difference between "go fix your config" and "what you just typed
    /// did not take".
    pub source: std::option::Option<std::path::PathBuf>,
}

#[derive(Default)]
struct Inner {
    /// OC.11c: failed assignments, keyed by CANONICAL option name.
    ///
    /// Here rather than on `Editor` because its lifetime is the registry's:
    /// it is written by the two paths that assign options and invalidated by
    /// the same events that change them. Holding it a layer up meant the
    /// clear-on-success had to be remembered by every caller, which is the
    /// shape that gets forgotten.
    diagnostics: HashMap<String, ConfigDiagnostic>,
    /// Indexed by [`OptionHandle::idx`]. A slot is `Some` while the
    /// option is live and `None` once unregistered (PH7.12b): the
    /// index IS the handle, so a slot can never shift or be reused
    /// under a live handle without invalidating it — hence a tombstone
    /// (set the slot to `None`) rather than `Vec::remove`. Freed
    /// indices are recycled through [`Inner::free_list`] so a plugin
    /// reload re-registering the same options reuses the slots instead
    /// of growing this vec unbounded across reloads (audit F6).
    by_id: Vec<std::option::Option<Arc<dyn ErasedOption>>>,
    /// Tombstoned `by_id` indices, available for reuse by the next
    /// registration (PH7.12b). Keeps `by_id` bounded across plugin
    /// reload cycles; empty in the common register-only lifetime.
    free_list: Vec<usize>,
    /// Name + alias → index. Multiple entries (canonical name +
    /// each alias) all point at the same `by_id` index.
    by_name: HashMap<String, usize>,
    /// `TypeId` of an [`crate::OptionDecl`] type → index. Populated
    /// by [`ConfigRegistry::register_with_typeid`] (called from
    /// the proc-macro's self-registration thunk during
    /// [`ConfigRegistry::init_from_linkme`]). The hot-path
    /// type-keyed read [`ConfigRegistry::get_typed`] looks up
    /// here; absent entries (option not registered) return `None`.
    by_typeid: HashMap<TypeId, usize>,
    /// Optional sink for [`Event::OptionChanged`] publishing
    /// (DESIGN.md §5.10 / §5.12). `None` means "no event publish",
    /// useful in tests and for embedded uses that don't run an
    /// event bus. The App wires this at boot via
    /// [`ConfigRegistry::set_event_publisher`] -- the closure
    /// body calls `event_bus.publish(event)`.
    event_publisher: std::option::Option<EventPublisher>,
    /// PMF.16: values the user's configuration gave options, by full name and
    /// source — see [`ConfigRegistry::hold`].
    held: HashMap<String, std::collections::BTreeMap<HeldSource, HeldValue>>,
    /// D1: options a project's `.lattice/config.toml` may not set — see
    /// [`ConfigRegistry::refuse_from_project`].
    project_refused: std::collections::HashSet<String>,
    /// The value a config file last applied to each option, by canonical
    /// name, formatted as the option held it right after — see
    /// [`ConfigRegistry::record_file_applied`].
    file_applied: HashMap<String, String>,
    /// Options read once at startup — see
    /// [`ConfigRegistry::mark_startup_only`].
    startup_only: std::collections::BTreeSet<String>,
    /// Values plugins contributed to other plugins' options, by canonical
    /// option name, in the order each contributor first contributed — see
    /// [`ConfigRegistry::contribute`].
    ///
    /// Keyed by name rather than stored on the option so they outlive the
    /// owner's unregister: an owner reloading comes back to the
    /// contributions its dependents made, rather than to none.
    contributions: HashMap<String, Vec<OptionContribution>>,
    /// Canonical names of the options whose owner accepts contributions —
    /// see [`ConfigRegistry::accept_contributions`]. Cleared for an option
    /// when it is unregistered, so a contribution reaches only a live owner
    /// that asked for it.
    accepting: std::collections::HashSet<String>,
}

/// One plugin's contribution to another plugin's option.
///
/// The host stores these and never merges them into the option's value: the
/// owner reads them beside the user's value and combines them by its own
/// rules. Design: `docs/dev/architecture/plugin-extension.md` §3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionContribution {
    /// The contributing plugin's manifest id: the provenance a user sees in
    /// `:describe-option`, and the key its unload withdraws by.
    pub contributor: String,
    /// The contributed value, already validated against the option's schema.
    pub value: crate::ConfigValue,
}

/// A value held for an option that does not exist yet, kept in the shape the
/// configuration wrote it in.
///
/// The shape is kept because only the option can say what it means. A list or
/// a table written for a plugin's option (`[[org.capture-templates]]`,
/// `agenda-files = ["~/a", "~/b"]`) is the option's whole value, and flattening
/// it to text before the plugin declares the option would lose the structure
/// its schema checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeldValue {
    /// A scalar, in the text form `:set` takes.
    Text(String),
    /// A list or a table, applied whole and checked against the option's
    /// schema when it registers.
    Tree(crate::ConfigValue),
}

/// PMF.16: which part of the configuration a held value came from.
///
/// The order is the precedence: values are applied in this order, so a later
/// source wins — `lattice.toml`, then a project's `.lattice/config.toml`, then
/// `init.rs`, matching the order they are read in. Each source is cleared on
/// its own when it is read again, so re-reading one never drops another's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HeldSource {
    /// The user's `lattice.toml`.
    ConfigFile,
    /// D1: a project's `.lattice/config.toml`. Not the user's: it can arrive
    /// in a cloned repository, so it may not set what
    /// [`ConfigRegistry::refuse_from_project`] names.
    ProjectFile,
    /// The user's `init.rs`.
    Init,
}

impl HeldSource {
    /// How a report names the source.
    pub fn label(self) -> &'static str {
        match self {
            HeldSource::ConfigFile => "lattice.toml",
            HeldSource::ProjectFile => ".lattice/config.toml",
            HeldSource::Init => "init.rs",
        }
    }
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
    /// No option (or alias) is registered under this name. Also
    /// returned by [`ConfigRegistry::parse_for_buffer_local`] for an
    /// option registered without a `TypeId` (it cannot be layered).
    #[error("E518: Unknown option: {0}")]
    UnknownOption(String),
    /// [`ConfigRegistry::try_register`] found this name or alias
    /// already taken. Nothing was registered.
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
    /// `name?` passed to `parse_for_buffer_local`. Query forms are
    /// not writes; callers should use `:set name?` or `:setlocal name?`
    /// to echo instead of calling into the write path.
    #[error("E474: query form not allowed in :setlocal; use :set {0}? to echo")]
    QueryNotAllowed(String),
    /// A typed access named a value type the registered option does
    /// not have. Currently constructed nowhere in the workspace — the
    /// typed paths report a mismatch as `None` ([`ConfigRegistry::try_get`])
    /// or a `String` error ([`ConfigRegistry::set`]) instead.
    #[error("E474: type mismatch: handle expected `{expected}`, registry has `{actual}`")]
    TypeMismatch {
        /// The accessor's [`OptionType::type_label`].
        expected: &'static str,
        /// The registered option's type label.
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
    /// An empty registry with no event publisher. Options arrive via
    /// [`Self::init_from_linkme`] (every `options!` declaration linked
    /// into the binary) and [`Self::register`] (runtime / plugin
    /// options).
    ///
    /// # Examples
    ///
    /// Declared options, read and written by type:
    ///
    /// ```
    /// use lattice_config::{ConfigRegistry, Tabstop};
    ///
    /// let reg = ConfigRegistry::new();
    /// assert!(reg.get_typed::<Tabstop>().is_none()); // nothing registered yet
    /// reg.init_from_linkme();
    ///
    /// assert_eq!(*reg.get_typed::<Tabstop>().unwrap(), 4); // the declared default
    /// reg.set_typed::<Tabstop>(2).unwrap();
    /// assert_eq!(*reg.get_typed::<Tabstop>().unwrap(), 2);
    /// // The option's validator (1..=32) rejects the write; the old value stays.
    /// assert!(reg.set_typed::<Tabstop>(0).is_err());
    /// assert_eq!(*reg.get_typed::<Tabstop>().unwrap(), 2);
    /// ```
    ///
    /// A runtime-registered option, read through its handle:
    ///
    /// ```
    /// use lattice_config::ConfigRegistry;
    /// use lattice_config::option::Option;
    ///
    /// let reg = ConfigRegistry::new();
    /// let depth = reg.register(
    ///     Option::<i64>::builder("myplugin.depth", 3, "Search depth.")
    ///         .aliases(&["mpd"])
    ///         .validate(|d| if *d > 0 { Ok(()) } else { Err(format!("depth must be > 0, got {d}")) })
    ///         .build(),
    /// );
    /// assert_eq!(*reg.get(depth), 3);
    /// assert_eq!(reg.parse_and_set_command("mpd=5").unwrap(), "myplugin.depth=5");
    /// assert_eq!(reg.with(depth, |d| *d), 5);
    /// assert!(reg.set(depth, -1).is_err());
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the [`Event::OptionChanged`] sink. Idempotent
    /// replacement -- calling twice swaps the closure. Designed to
    /// be called once at boot from the consumer that owns the
    /// `EventBus` (the App today; future plugin-host paths route
    /// through the same closure).
    pub fn set_event_publisher(&self, publisher: EventPublisher) {
        let mut inner = self.inner.lock().expect("ConfigRegistry poisoned");
        inner.event_publisher = Some(publisher);
    }

    /// Publish helper: capture old + new and dispatch to the
    /// registered publisher (if any). Old value is captured
    /// *before* the set; new is captured *after*. We re-look-up
    /// the spec under the lock to read both sides atomically wrt
    /// the publisher fan-out -- but the publisher itself runs
    /// outside the lock so subscribers can re-enter the registry
    /// safely (e.g. read another option).
    fn publish_change(&self, name: &str, old: std::option::Option<String>) {
        let (publisher, new) = {
            let inner = self.inner.lock().expect("ConfigRegistry poisoned");
            let publisher = inner.event_publisher.clone();
            let new = inner
                .by_name
                .get(name)
                .and_then(|i| inner.by_id[*i].as_ref())
                .map(|o| o.get_formatted());
            (publisher, new)
        };
        if let (Some(publisher), Some(new)) = (publisher, new) {
            publisher(Event::OptionChanged {
                name: name.to_string(),
                old,
                new,
            });
        }
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
        // PL8.F: own the name up front — `option.name()` now borrows `option`
        // (a `Cow` field), and `option` is moved into the `Arc` below, so the
        // key insert at the tail would otherwise be a borrow-after-move.
        let name = option.name().to_owned();
        let aliases = option.aliases;
        if inner.by_name.contains_key(name.as_str()) {
            return Err(ConfigError::DuplicateName(name));
        }
        for a in aliases {
            if inner.by_name.contains_key(*a) {
                return Err(ConfigError::DuplicateName((*a).to_string()));
            }
        }
        let arc: Arc<dyn ErasedOption> = Arc::new(option);
        // Reuse a tombstoned slot if one is free (a prior unregister) so
        // `by_id` stays bounded across plugin reloads (PH7.12b); otherwise
        // append. Either way the index is stable for the returned handle.
        let idx = match inner.free_list.pop() {
            Some(reused) => {
                inner.by_id[reused] = Some(arc);
                reused
            }
            None => {
                inner.by_id.push(Some(arc));
                inner.by_id.len() - 1
            }
        };
        let held = inner.held.get(&name).cloned();
        let registered = inner.by_id[idx].clone();
        inner.by_name.insert(name.clone(), idx);
        for a in aliases {
            inner.by_name.insert((*a).to_string(), idx);
        }
        drop(inner);
        // PMF.16: the user's configuration may have named this option before
        // it existed (a plugin's option, set from `lattice.toml` or `init.rs`);
        // it starts with their value, not its default. In precedence order.
        let refused = self.refuses_from_project(&name);
        for (source, value) in held.into_iter().flatten() {
            if refused && source == HeldSource::ProjectFile {
                // User-actionable: the project file named it, and nothing
                // else will say why it had no effect.
                tracing::warn!(
                    option = %name,
                    "`{name}` in .lattice/config.toml was not applied: it decides what a plugin \
                     may reach or whether it runs, so only your own configuration can set it"
                );
                continue;
            }
            let applied = match &value {
                HeldValue::Text(text) => self
                    .parse_and_set_command(&format!("{name}={text}"))
                    .map(|_| ())
                    .map_err(|err| format!("`{name}`: {err}")),
                HeldValue::Tree(tree) => match registered.as_deref() {
                    Some(opt) => self.apply_held_tree(&name, opt, tree),
                    None => Ok(()),
                },
            };
            if let Err(err) = applied {
                // User-actionable, and the only trace: the config was read
                // long before this option appeared. The text carries the
                // option's name and, for a list or a table, the field that
                // failed, because `*messages*` shows the line, not the fields.
                tracing::warn!(
                    option = %name,
                    source = source.label(),
                    "{err} (set in {}, which this option's plugin declares as something else)",
                    source.label()
                );
            }
        }
        Ok(OptionHandle::<T>::new(idx))
    }

    /// PMF.16: record what the user's configuration says `name` should be.
    ///
    /// A plugin's options exist only once it has loaded, and the user's
    /// configuration is read before any plugin loads — `lattice.toml` at boot,
    /// `init.rs` first among plugins. A held value is applied when the option
    /// registers (in [`Self::try_register`]) and again whenever it registers
    /// anew after a plugin reload, so the plugin starts with the user's value
    /// rather than its default. Holding does not set anything: a caller whose
    /// option is already registered sets it too.
    pub fn hold(&self, name: &str, value: &str, source: HeldSource) {
        self.hold_value(name, HeldValue::Text(value.to_string()), source);
    }

    /// [`Self::hold`] for a list or a table: kept whole, and checked against
    /// the option's schema when the option registers.
    pub fn hold_tree(&self, name: &str, value: crate::ConfigValue, source: HeldSource) {
        self.hold_value(name, HeldValue::Tree(value), source);
    }

    fn hold_value(&self, name: &str, value: HeldValue, source: HeldSource) {
        self.inner
            .lock()
            .expect("ConfigRegistry poisoned")
            .held
            .entry(name.to_string())
            .or_default()
            .insert(source, value);
    }

    /// Apply a held list or table to the option it was written for.
    ///
    /// A composite option takes it whole, validated first so a failure names
    /// the field (`[2].target.file: expected string, got integer`). A
    /// list-of-scalars option that predates schemas (`ui.modeline.left`) takes
    /// it joined, the way the loader joins an array for a registered one.
    /// Anything else is a list or a table written for a scalar.
    fn apply_held_tree(
        &self,
        name: &str,
        opt: &dyn ErasedOption,
        tree: &crate::ConfigValue,
    ) -> Result<(), String> {
        let schema = opt.schema();
        if schema.is_composite() {
            crate::schema::validate(&schema, tree).map_err(|err| {
                format!(
                    "`{name}{}`: {}",
                    crate::schema::dot_path(&err.path),
                    err.message
                )
            })?;
            return opt
                .set_value(tree)
                .map_err(|err| format!("`{name}`: {err}"));
        }
        if let crate::ConfigValue::List(items) = tree
            && opt.accepts_list()
        {
            let parts: std::option::Option<Vec<String>> = items
                .iter()
                .map(|item| match item {
                    crate::ConfigValue::Str(s) => Some(s.clone()),
                    crate::ConfigValue::Int(i) => Some(i.to_string()),
                    crate::ConfigValue::Bool(b) => Some(b.to_string()),
                    _ => None,
                })
                .collect();
            if let Some(parts) = parts {
                return self
                    .parse_and_set_command(&format!("{name}={}", parts.join(",")))
                    .map(|_| ())
                    .map_err(|err| format!("`{name}`: {err}"));
            }
        }
        Err(format!(
            "`{name}`: a list or a table does not fit this option"
        ))
    }

    /// PMF.16: the value the user's configuration holds for `name`, highest
    /// precedence first — what the option will be once it registers. For a
    /// caller that must decide before then (a plugin's file grant). Text only:
    /// a grant follows a path, and a held list or table names none.
    pub fn held_value(&self, name: &str) -> std::option::Option<String> {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        let refused = inner.project_refused.contains(name);
        inner
            .held
            .get(name)
            .and_then(|by_source| {
                by_source
                    .iter()
                    .rev()
                    .find(|(source, _)| !(refused && **source == HeldSource::ProjectFile))
                    .map(|(_, value)| value)
            })
            .and_then(|value| match value {
                HeldValue::Text(text) => Some(text.clone()),
                HeldValue::Tree(_) => None,
            })
    }

    /// OB.1: [`Self::held_value`] in the option's own shape — a held list or
    /// table whole, text as a string. For a grant that follows a list option
    /// (`fs:write:{org.babel.tangle-directories}`), where flattening the list
    /// to text before the option registers would lose every entry.
    pub fn held_config_value(&self, name: &str) -> std::option::Option<crate::ConfigValue> {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        let refused = inner.project_refused.contains(name);
        inner
            .held
            .get(name)
            .and_then(|by_source| {
                by_source
                    .iter()
                    .rev()
                    .find(|(source, _)| !(refused && **source == HeldSource::ProjectFile))
                    .map(|(_, value)| value)
            })
            .map(|value| match value {
                HeldValue::Text(text) => crate::ConfigValue::Str(text.clone()),
                HeldValue::Tree(tree) => tree.clone(),
            })
    }

    /// D1: a project's `.lattice/config.toml` may not set `name`.
    ///
    /// For an option that decides what a plugin may reach or whether it runs:
    /// one a capability grant follows (`fs:write:{org.directory|~/org}`,
    /// `proc:spawn:{org.babel.languages}`), and a plugin's mode switches. The
    /// project file is not the user's. It arrives with a cloned repository, so
    /// a value there would be a stranger choosing what the user's plugins may
    /// run. The plugin host names them as it computes each grant, which is
    /// before the plugin registers its options; from then on a project value
    /// for one is neither applied when the option registers, nor read by
    /// [`Self::held_value`], nor set by a later load of the file. The user's
    /// own `lattice.toml`, `init.rs` and `:set` are untouched.
    pub fn refuse_from_project(&self, name: &str) {
        self.inner
            .lock()
            .expect("ConfigRegistry poisoned")
            .project_refused
            .insert(name.to_string());
    }

    /// D1: is `name` one [`Self::refuse_from_project`] named?
    pub fn refuses_from_project(&self, name: &str) -> bool {
        self.inner
            .lock()
            .expect("ConfigRegistry poisoned")
            .project_refused
            .contains(name)
    }

    /// PMF.16: forget what `source` holds, before it is read again — a line the
    /// user deleted must not keep applying. Other sources are untouched.
    pub fn clear_held(&self, source: HeldSource) {
        let mut inner = self.inner.lock().expect("ConfigRegistry poisoned");
        for by_source in inner.held.values_mut() {
            by_source.remove(&source);
        }
        inner.held.retain(|_, by_source| !by_source.is_empty());
    }

    /// Note that `name` is read once, at startup, by its owner, so a later
    /// change to it waits for the next start.
    ///
    /// The owner marks it, because only the owner knows: the GUI reads the
    /// window's decorations before the window opens, the terminal peer asks
    /// the terminal for key reporting once. A reload that changes a marked
    /// option says so instead of appearing to have applied it.
    pub fn mark_startup_only(&self, name: &str) {
        let canonical = self
            .lookup(name)
            .map_or_else(|| name.to_string(), |o| o.name().to_string());
        self.inner
            .lock()
            .expect("ConfigRegistry poisoned")
            .startup_only
            .insert(canonical);
    }

    /// The options marked with [`Self::mark_startup_only`], in name order.
    pub fn startup_only(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("ConfigRegistry poisoned")
            .startup_only
            .iter()
            .cloned()
            .collect()
    }

    /// Note that a config file just set `name`, recording the value the option
    /// now holds.
    ///
    /// A reload compares against this: an option the files no longer mention
    /// reverts to its default only while it still holds the value a file gave
    /// it, so a `:set` made since is not undone. Keyed by canonical name, so a
    /// file that switched to an alias's spelling is still the same option. A
    /// name no registered option answers to records nothing.
    pub fn record_file_applied(&self, name: &str) {
        let Some(opt) = self.lookup(name) else {
            return;
        };
        let value = opt.get_formatted();
        self.inner
            .lock()
            .expect("ConfigRegistry poisoned")
            .file_applied
            .insert(opt.name().to_string(), value);
    }

    /// The value a config file last applied to `name`, if one did.
    pub fn file_applied(&self, name: &str) -> std::option::Option<String> {
        let canonical = self.lookup(name).map(|o| o.name().to_string());
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        inner
            .file_applied
            .get(canonical.as_deref().unwrap_or(name))
            .cloned()
    }

    /// Take the whole record, leaving it empty — the start of a fresh reading
    /// of the files, which records anew what it applies.
    pub(crate) fn take_file_applied(&self) -> HashMap<String, String> {
        std::mem::take(
            &mut self
                .inner
                .lock()
                .expect("ConfigRegistry poisoned")
                .file_applied,
        )
    }

    /// Put back a record this reading did not replace: the file still names the
    /// option, but the new value failed, so the old one stands and is still the
    /// file's.
    pub(crate) fn restore_file_applied(&self, name: String, value: String) {
        self.inner
            .lock()
            .expect("ConfigRegistry poisoned")
            .file_applied
            .entry(name)
            .or_insert(value);
    }

    /// Is `name` recorded as set by a config file in the current reading?
    pub(crate) fn has_file_applied(&self, name: &str) -> bool {
        self.inner
            .lock()
            .expect("ConfigRegistry poisoned")
            .file_applied
            .contains_key(name)
    }

    /// PMF.16: held names no registered option answers to, with the source that
    /// set each — once every plugin has loaded, a misspelt name or a plugin
    /// that did not load. Sorted, for a stable report.
    ///
    /// A table held for a plugin that has not loaded is read both ways, since
    /// nothing can tell which it is until the plugin declares its options: as
    /// one option's value, and as a namespace of options. So a held name is
    /// also claimed when a registered option sits under it (the table was a
    /// namespace) or above it (it was a field of a table option's value).
    pub fn unclaimed_held(&self) -> Vec<(String, HeldSource)> {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        let claimed = |name: &str| {
            inner.by_name.contains_key(name)
                || inner.by_name.keys().any(|registered| {
                    registered
                        .strip_prefix(name)
                        .is_some_and(|rest| rest.starts_with('.'))
                        || name
                            .strip_prefix(registered.as_str())
                            .is_some_and(|rest| rest.starts_with('.'))
                })
        };
        let mut out: Vec<(String, HeldSource)> = inner
            .held
            .iter()
            .filter(|(name, _)| !claimed(name.as_str()))
            .flat_map(|(name, by_source)| by_source.keys().map(|s| (name.clone(), *s)))
            .collect();
        out.sort();
        out
    }

    /// PMF.16: is `namespace` already taken — a registered option lives under
    /// `namespace.`, or is named `namespace` itself? Decides, for a config file
    /// naming an unknown option, between "a plugin's, not loaded yet" (hold it)
    /// and "a mistake about something that is here" (say so now): a typo in a
    /// namespace that exists, or a table written at a scalar option's name
    /// (`[tabstop]`).
    pub fn namespace_in_use(&self, namespace: &str) -> bool {
        let prefix = format!("{namespace}.");
        self.inner
            .lock()
            .expect("ConfigRegistry poisoned")
            .by_name
            .keys()
            .any(|name| name == namespace || name.starts_with(&prefix))
    }

    /// Variant of [`Self::register`] that additionally records a
    /// `TypeId` ↔ option-index mapping for type-keyed reads via
    /// [`Self::get_typed`]. Called from the
    /// [`crate::options!`] macro's `register_fn` thunk during
    /// [`Self::init_from_linkme`]; not generally called by hand.
    ///
    /// `type_id` is `TypeId::of::<D>()` where `D` is the
    /// [`crate::OptionDecl`] type. Two declarations with the same
    /// `TypeId` cannot exist (Rust's type system enforces it
    /// cross-crate), so duplicate-typeid is a programming error
    /// and panics; the macro never produces it.
    #[track_caller]
    pub fn register_with_typeid<T: OptionType>(
        &self,
        option: Option<T>,
        type_id: TypeId,
    ) -> OptionHandle<T> {
        let handle = self.register(option);
        let mut inner = self.inner.lock().expect("ConfigRegistry poisoned");
        if inner.by_typeid.insert(type_id, handle.idx).is_some() {
            // Two registrations for the same type id is a build
            // bug -- shouldn't be reachable from the macro. We
            // use `unreachable!` (which clippy permits) rather
            // than `panic!` because the path is structurally
            // impossible to reach in correct callers.
            #[allow(clippy::panic)]
            {
                panic!("config: duplicate TypeId in register_with_typeid");
            }
        }
        handle
    }

    /// Type-keyed read: returns the resolved value for the
    /// [`OptionDecl`] type `D`. `D::Value` is the value type.
    /// Returns `None` if the option has not been registered (no
    /// `register_with_typeid` call seen for this `TypeId`); this
    /// is legitimate transient state during boot before
    /// [`Self::init_from_linkme`] runs.
    pub fn get_typed<D: OptionDecl>(&self) -> std::option::Option<Arc<D::Value>>
    where
        D::Value: Clone + Send + Sync + 'static,
    {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        let idx = *inner.by_typeid.get(&TypeId::of::<D>())?;
        let arc = Arc::clone(inner.by_id[idx].as_ref()?);
        drop(inner);
        let opt = arc.as_any().downcast_ref::<Option<D::Value>>()?;
        Some(opt.get())
    }

    /// Type-keyed handle lookup: recover the legacy
    /// [`OptionHandle<D::Value>`] from an [`OptionDecl`] type.
    /// Used by the M.2.0b backwards-compatibility shim in
    /// `core_options::register_core_options` to populate the
    /// `CoreOptions` struct with handles after
    /// [`Self::init_from_linkme`] has run.
    ///
    /// Returns `None` if `D` was not registered. M.2.0c retires
    /// the `OptionHandle<T>` API and this method along with it.
    pub fn handle_for_decl<D: OptionDecl>(&self) -> std::option::Option<OptionHandle<D::Value>> {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        let idx = *inner.by_typeid.get(&TypeId::of::<D>())?;
        Some(OptionHandle::<D::Value>::new(idx))
    }

    /// Type-keyed write: set the option declared by `D` to `value`.
    /// Runs the option's validator before committing, publishes
    /// [`Event::OptionChanged`] on success, returns the validator
    /// error (or a missing-registration error) on failure.
    ///
    /// Hot-path-equivalent to the legacy `set(handle, value)`
    /// shape; one TypeId lookup per write. Writes are rare (user
    /// `:set foo=bar` cmdline, programmatic toggles), so the
    /// extra hash bears no measurable cost.
    pub fn set_typed<D: OptionDecl>(&self, value: D::Value) -> Result<(), String>
    where
        D::Value: Clone + Send + Sync + 'static,
    {
        let handle = self
            .handle_for_decl::<D>()
            .ok_or_else(|| format!("config: option `{}` not registered", D::NAME))?;
        self.set(handle, value)
    }

    /// Bootstrap a [`crate::ResolvedOptions`] cache with every
    /// registered option's *current* value (layer 5 + 6 of the
    /// resolution stack -- `mode-architecture.md` §6.1). Walks
    /// the typeid map and writes each option's
    /// [`crate::ErasedOption::current_value_erased`] into the
    /// cache, keyed by `TypeId`.
    ///
    /// Used by [`crate::Resolver::resolve_into`] callers that
    /// want a fully-populated resolved cache without manually
    /// chaining a "default" layer. After this returns, the
    /// caller layers any mode / buffer-local / modal overrides
    /// on top via the resolver.
    pub fn bootstrap_resolved_with_current_values(&self, out: &mut crate::ResolvedOptions) {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        for (type_id, &idx) in inner.by_typeid.iter() {
            let Some(arc) = inner.by_id[idx].as_ref() else {
                continue;
            };
            let arc = std::sync::Arc::clone(arc);
            let value_erased = arc.current_value_erased();
            out.insert_erased_with_origin(
                crate::OptionKey::Native(*type_id),
                value_erased,
                crate::OptionOrigin::GlobalConfig,
            );
        }
    }

    /// The [`crate::OptionKey`] an option is known by in overrides and
    /// resolved snapshots (RF.4e′): its declaration `TypeId` when it has one,
    /// else its canonical name — a plugin option registered through the
    /// `config` seam. Aliases resolve to the canonical name, so `:setlocal`
    /// by alias and a mode override by full name land on one key. `None`
    /// for a name nothing registered.
    pub fn key_for_name(&self, name: &str) -> std::option::Option<crate::OptionKey> {
        self.key_for(name).ok()
    }

    /// Boot loop: walk the [`OPTION_DECLS`] linkme slice and
    /// register every option declared anywhere in the workspace.
    /// Idempotent: calling more than once is a no-op (the second
    /// call observes that every option is already registered and
    /// returns without doing anything). Boot panics on a true
    /// programming error (e.g. two distinct `OptionDecl` types
    /// happen to declare the same display name across crates).
    ///
    /// After this returns, every `options! { ... }` declaration
    /// is reachable via [`Self::get_typed`] / [`Self::lookup`] /
    /// the cmdline `:set` / TOML loader. Boot is the moment when
    /// "the option exists" goes from "compile-time fact in the
    /// declaring crate" to "runtime fact in the registry."
    pub fn init_from_linkme(&self) {
        let already_registered = {
            let inner = self.inner.lock().expect("ConfigRegistry poisoned");
            !inner.by_typeid.is_empty()
        };
        if already_registered {
            // Caller invoked us a second time (e.g. App::new
            // called init, then a per-feature helper also called
            // it). Skip the walk -- every option already lives
            // in the registry from the first call.
            return;
        }
        for decl in OPTION_DECLS.iter() {
            (decl.register_fn)(self);
        }
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
    /// before committing. Publishes [`Event::OptionChanged`] on
    /// success.
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
        let old = opt.with(|v| v.format());
        let name = opt.name();
        opt.set(value)?;
        self.publish_change(name, Some(old));
        Ok(())
    }

    /// Look up an option by name (or alias). Returns the erased
    /// view -- the by-name path is for cmdline / customize / plugin
    /// introspection where the type isn't known at compile time.
    pub fn lookup(&self, name: &str) -> std::option::Option<Arc<dyn ErasedOption>> {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        inner
            .by_name
            .get(name)
            .and_then(|i| inner.by_id[*i].as_ref().map(Arc::clone))
    }

    /// Read a `bool`-typed option's current value by name.
    /// Returns `None` if the option doesn't exist, isn't boolean,
    /// or its erased current value fails to downcast to `bool`
    /// (last case is defense-in-depth -- shouldn't happen given
    /// `is_bool()` agreement). Used by the host's mode-mirror
    /// cascade (see `Mode::mirrors_option`) so display modes can
    /// stay in sync with their typed-option counterparts without
    /// hardcoded per-mode special cases in the cascade handler.
    pub fn get_bool_by_name(&self, name: &str) -> std::option::Option<bool> {
        let spec = self.lookup(name)?;
        if !spec.is_bool() {
            return None;
        }
        let erased = spec.current_value_erased();
        erased.downcast_ref::<bool>().copied()
    }

    /// Read an `i64`-typed option's current value by name. Returns
    /// `None` if the option doesn't exist or its erased current value
    /// isn't an `i64` (the downcast IS the type check — there's no
    /// `is_int` predicate, and a wrong-type option simply fails the
    /// downcast). The `i64` shape covers every integer option (the
    /// `options!` macro stores counts / sizes as `i64`). Used by
    /// subsystems that read a numeric option by name without importing
    /// its decl type — e.g. `lattice-diff`'s `UnchangedFoldSource`
    /// reading `ui.diff.context`.
    pub fn get_int_by_name(&self, name: &str) -> std::option::Option<i64> {
        let spec = self.lookup(name)?;
        let erased = spec.current_value_erased();
        erased.downcast_ref::<i64>().copied()
    }

    /// Read a `String`-typed option's current value by name. The downcast IS
    /// the type check, as with [`Self::get_int_by_name`]. Used by subsystems
    /// that read an enum-ish option by name without importing its decl type —
    /// e.g. the host reading a plugin's `trim-scope`, which is registered
    /// dynamically and therefore has no decl type to import.
    pub fn get_string_by_name(&self, name: &str) -> std::option::Option<String> {
        let spec = self.lookup(name)?;
        let erased = spec.current_value_erased();
        erased.downcast_ref::<String>().cloned()
    }

    /// Iterate every registered option in registration order.
    /// Used by completion (`gen:options`) and the customize buffer
    /// view to enumerate.
    pub fn iter(&self) -> Vec<Arc<dyn ErasedOption>> {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        // `flatten` skips tombstoned (unregistered) slots (PH7.12b).
        inner.by_id.iter().flatten().map(Arc::clone).collect()
    }

    /// Number of registered options. Counts live options only — a
    /// tombstoned (unregistered) slot does not count (PH7.12b).
    pub fn len(&self) -> usize {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        inner.by_id.iter().filter(|o| o.is_some()).count()
    }

    /// `true` when no live option is registered (tombstoned slots do
    /// not count).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Remove an option by name (or any alias), the teardown seam for a plugin
    /// reload / unload (PH7.12b). Drops every name+alias mapping pointing at the
    /// option's slot plus any `TypeId` mapping, tombstones the `by_id` slot
    /// (freeing the `Arc`), and recycles the index through the free-list so a
    /// re-register reuses it. Idempotent: `false` if nothing is registered under
    /// `name`. Driven by the plugin host's teardown bundle with the option names
    /// a plugin registered (`PluginState::config_contributions`); built-in
    /// options have no unload path in practice. Any live [`OptionHandle`] for
    /// the removed option is left dangling by contract — its slot is `None`, so
    /// a by-handle read returns nothing rather than another option's value; the
    /// index is only ever reused by a *new* registration, never silently
    /// re-pointed under an existing handle.
    pub fn unregister(&self, name: &str) -> bool {
        let mut inner = self.inner.lock().expect("ConfigRegistry poisoned");
        let idx = match inner.by_name.get(name) {
            Some(&i) => i,
            None => return false,
        };
        if let Some(canonical) = inner.by_id[idx].as_ref().map(|o| o.name().to_string()) {
            inner.accepting.remove(&canonical);
        }
        inner.by_name.retain(|_, v| *v != idx);
        inner.by_typeid.retain(|_, v| *v != idx);
        inner.by_id[idx] = None;
        inner.free_list.push(idx);
        true
    }

    /// Let other plugins contribute values to `name` (see
    /// [`contribute`](Self::contribute)). `false` when no such option is
    /// registered.
    ///
    /// Who may call this is the caller's business: the plugin host lets an
    /// option's owner accept for its own options only, and refuses an option
    /// a capability grant follows.
    pub fn accept_contributions(&self, name: &str) -> bool {
        let mut inner = self.inner.lock().expect("ConfigRegistry poisoned");
        let Some(canonical) = canonical_name(&inner, name) else {
            return false;
        };
        inner.accepting.insert(canonical);
        true
    }

    /// Whether `name`'s owner accepts contributions.
    pub fn accepts_contributions(&self, name: &str) -> bool {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        canonical_name(&inner, name).is_some_and(|c| inner.accepting.contains(&c))
    }

    /// Record `contributor`'s contribution to `name`, replacing its earlier
    /// one in place (so the order stays "first contributed"), or withdraw it
    /// when `value` is `None`.
    ///
    /// Refused when the option does not exist, does not accept contributions,
    /// or `value` does not fit its schema; the error says which, with the
    /// schema path for a misfit. Withdrawing a contribution that was never
    /// made is not an error.
    pub fn contribute(
        &self,
        name: &str,
        contributor: &str,
        value: std::option::Option<crate::ConfigValue>,
    ) -> Result<(), String> {
        let mut inner = self.inner.lock().expect("ConfigRegistry poisoned");
        let option = inner
            .by_name
            .get(name)
            .and_then(|i| inner.by_id[*i].clone())
            .ok_or_else(|| format!("no option `{name}`"))?;
        let canonical = option.name().to_string();
        if !inner.accepting.contains(&canonical) {
            return Err(format!("`{canonical}` does not accept contributions"));
        }
        let list = inner.contributions.entry(canonical).or_default();
        let Some(value) = value else {
            list.retain(|c| c.contributor != contributor);
            return Ok(());
        };
        crate::schema::validate(&option.schema(), &value).map_err(|e| e.to_string())?;
        match list.iter_mut().find(|c| c.contributor == contributor) {
            Some(existing) => existing.value = value,
            None => list.push(OptionContribution {
                contributor: contributor.to_string(),
                value,
            }),
        }
        Ok(())
    }

    /// Every contribution to `name`, in the order each was first made. Empty
    /// when the option's owner does not accept contributions, even if some
    /// are held for it: they come back into effect when it accepts again.
    pub fn contributions(&self, name: &str) -> Vec<OptionContribution> {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        let Some(canonical) = canonical_name(&inner, name) else {
            return Vec::new();
        };
        if !inner.accepting.contains(&canonical) {
            return Vec::new();
        }
        inner
            .contributions
            .get(&canonical)
            .cloned()
            .unwrap_or_default()
    }

    /// The `:describe-option` section listing what other plugins contributed
    /// to `name`, one line per plugin with its value as TOML, or `None` when
    /// nothing is. Here rather than in the help builder so a user's "why is
    /// this folder in my agenda" gets the same answer wherever it is shown.
    pub fn describe_contributions(&self, name: &str) -> std::option::Option<Vec<String>> {
        let contributions = self.contributions(name);
        if contributions.is_empty() {
            return None;
        }
        let mut lines = vec![
            "## Contributed by plugins".to_string(),
            String::new(),
            "Read by the option's owner beside the value above; removed when the \
             contributing plugin unloads."
                .to_string(),
            String::new(),
        ];
        lines.extend(contributions.iter().map(|c| {
            format!(
                "- {}: {}",
                c.contributor,
                crate::schema::config_value_to_toml(&c.value)
            )
        }));
        Some(lines)
    }

    /// Withdraw every contribution `contributor` made, to any option. The
    /// plugin host's teardown calls this when the contributor unloads.
    /// Returns how many were withdrawn.
    pub fn withdraw_contributions(&self, contributor: &str) -> usize {
        let mut inner = self.inner.lock().expect("ConfigRegistry poisoned");
        let mut withdrawn = 0;
        for list in inner.contributions.values_mut() {
            let before = list.len();
            list.retain(|c| c.contributor != contributor);
            withdrawn += before - list.len();
        }
        inner.contributions.retain(|_, list| !list.is_empty());
        withdrawn
    }

    /// OC.11c: record that an assignment to `name` failed.
    ///
    /// `name` is canonicalised here so an alias (`:set ts=999`) records under
    /// the name the option was declared with — a plugin asking about its own
    /// config would never think to ask about an alias, and an alias-keyed
    /// record is invisible to every reader.
    pub fn record_failed_assignment(
        &self,
        name: &str,
        message: String,
        source: std::option::Option<std::path::PathBuf>,
    ) {
        let canonical = self
            .lookup(name)
            .map(|o| o.name().to_string())
            .unwrap_or_else(|| name.to_string());
        let mut inner = self.inner.lock().expect("ConfigRegistry poisoned");
        inner
            .diagnostics
            .insert(canonical, ConfigDiagnostic { message, source });
    }

    /// OC.11c: forget any failure recorded for `name` — an assignment to it
    /// has since succeeded, so the record describes something untrue.
    pub fn clear_failed_assignment(&self, name: &str) {
        let canonical = self
            .lookup(name)
            .map(|o| o.name().to_string())
            .unwrap_or_else(|| name.to_string());
        let mut inner = self.inner.lock().expect("ConfigRegistry poisoned");
        inner.diagnostics.remove(&canonical);
    }

    /// OC.11c: drop every recorded failure.
    ///
    /// Called at the start of a config LOAD, which is a fresh reading of the
    /// whole file: an option whose failing line the user deleted produces no
    /// message at all, so a per-message update would leave its diagnostic
    /// behind forever.
    pub fn clear_all_failed_assignments(&self) {
        let mut inner = self.inner.lock().expect("ConfigRegistry poisoned");
        inner.diagnostics.clear();
    }

    /// OC.11c: the failure recorded against `name`, if the last assignment to
    /// it failed.
    ///
    /// `None` means the last assignment succeeded or there never was one —
    /// not distinguished, deliberately: the caller's question is "can I trust
    /// this value", and both answers are yes.
    pub fn failed_assignment(&self, name: &str) -> std::option::Option<ConfigDiagnostic> {
        let canonical = self
            .lookup(name)
            .map(|o| o.name().to_string())
            .unwrap_or_else(|| name.to_string());
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        inner.diagnostics.get(&canonical).cloned()
    }

    /// Drive the cmdline `:set` syntax against the registry. Parses
    /// the input through [`parse_set`], then dispatches to the
    /// matching option's parse / set / negate / format path.
    /// Returns the echo line on success (`canonical-name=value`) — the
    /// caller surfaces it via the cmdline echo. Forms:
    /// - `:set foo` -- echoes `foo=current` for non-bool, sets
    ///   true for bool (vim convention).
    /// - `:set nofoo` -- sets bool to false; [`ConfigError::NotBoolean`]
    ///   for any other type.
    /// - `:set foo=value` -- parses + validates + sets.
    /// - `:set foo?` -- always echoes the current value.
    /// - `:set foo&` -- resets to the registered default.
    ///
    /// Every successful write publishes [`Event::OptionChanged`]; a
    /// query does not. The outcome also maintains the failed-assignment
    /// record ([`Self::failed_assignment`]): a failure on a known option
    /// is recorded (with no source file), a success clears it, and an
    /// unknown name records nothing.
    ///
    /// # Examples
    ///
    /// ```
    /// use lattice_config::{ConfigError, ConfigRegistry};
    ///
    /// let reg = ConfigRegistry::new();
    /// reg.init_from_linkme();
    ///
    /// assert_eq!(reg.parse_and_set_command("ts=8").unwrap(), "tabstop=8");
    /// assert_eq!(reg.parse_and_set_command("wrap").unwrap(), "wrap=true");
    /// assert_eq!(reg.parse_and_set_command("nowrap").unwrap(), "wrap=false");
    /// assert_eq!(reg.parse_and_set_command("tabstop&").unwrap(), "tabstop=4");
    ///
    /// assert!(matches!(
    ///     reg.parse_and_set_command("notabstop"),
    ///     Err(ConfigError::NotBoolean(_))
    /// ));
    /// assert!(matches!(
    ///     reg.parse_and_set_command("nosuch=1"),
    ///     Err(ConfigError::UnknownOption(_))
    /// ));
    ///
    /// // A rejected write is remembered against the canonical name...
    /// assert!(reg.parse_and_set_command("ts=0").is_err());
    /// assert!(reg.failed_assignment("tabstop").is_some());
    /// // ...until a later write succeeds.
    /// reg.parse_and_set_command("tabstop=2").unwrap();
    /// assert!(reg.failed_assignment("tabstop").is_none());
    /// ```
    pub fn parse_and_set_command(&self, input: &str) -> Result<String, ConfigError> {
        let parsed = parse_set(input).map_err(ConfigError::Parse)?;
        // OC.11c: the outcome of THIS assignment replaces whatever was
        // recorded for the option, at the one chokepoint every `:set` goes
        // through — so no caller has to remember to clear it. An
        // `UnknownOption` records nothing: there is no option for a diagnostic
        // to be about, and keying one under a typo would let it shadow the
        // real option a plugin later asks about.
        let touched = match &parsed {
            ParsedSet::Assign { name, .. } => Some(name.clone()),
            ParsedSet::Negate(name) | ParsedSet::NameOnly(name) | ParsedSet::Reset(name) => {
                Some(name.clone())
            }
            ParsedSet::Query(_) => None,
        }
        .filter(|n| !n.is_empty());
        let outcome = self.parse_and_set_command_inner(parsed);
        if let Some(name) = touched {
            match &outcome {
                Ok(_) => self.clear_failed_assignment(&name),
                Err(ConfigError::UnknownOption(_)) => {}
                Err(err) => self.record_failed_assignment(&name, err.to_string(), None),
            }
        }
        outcome
    }

    fn parse_and_set_command_inner(&self, parsed: ParsedSet) -> Result<String, ConfigError> {
        match parsed {
            ParsedSet::NameOnly(name) => {
                let opt = self
                    .lookup(&name)
                    .ok_or(ConfigError::UnknownOption(name.clone()))?;
                if opt.is_bool() {
                    let canonical = opt.name();
                    let old = opt.get_formatted();
                    opt.parse_and_set("true").map_err(ConfigError::Validation)?;
                    self.publish_change(canonical, Some(old));
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
                let canonical = opt.name();
                let old = opt.get_formatted();
                opt.negate().map_err(ConfigError::Validation)?;
                self.publish_change(canonical, Some(old));
                Ok(format!("{}={}", opt.name(), opt.get_formatted()))
            }
            ParsedSet::Assign { name, value } => {
                let opt = self
                    .lookup(&name)
                    .ok_or(ConfigError::UnknownOption(name.clone()))?;
                let canonical = opt.name();
                let old = opt.get_formatted();
                opt.parse_and_set(&value).map_err(ConfigError::Validation)?;
                self.publish_change(canonical, Some(old));
                Ok(format!("{}={}", opt.name(), opt.get_formatted()))
            }
            ParsedSet::Query(name) => {
                let opt = self
                    .lookup(&name)
                    .ok_or(ConfigError::UnknownOption(name.clone()))?;
                Ok(format!("{}={}", opt.name(), opt.get_formatted()))
            }
            ParsedSet::Reset(name) => {
                let opt = self
                    .lookup(&name)
                    .ok_or(ConfigError::UnknownOption(name.clone()))?;
                let canonical = opt.name();
                let old = opt.get_formatted();
                // Reset to the registered default (the string stored at
                // registration time from `default_formatted`).
                opt.parse_and_set(opt.default_formatted())
                    .map_err(ConfigError::Validation)?;
                self.publish_change(canonical, Some(old));
                Ok(format!("{}={}", opt.name(), opt.get_formatted()))
            }
        }
    }

    /// Parse an option spec string for use as a buffer-local override,
    /// without writing to the global registry. Returns the triple
    /// `(key, erased_value, canonical_name)` needed to construct an
    /// [`crate::OptionOverride`] for the buffer-local layer. The key is
    /// [`Self::key_for_name`]'s, so a plugin option is as settable per
    /// buffer as a native one (RF.4e′).
    ///
    /// - `NameOnly(name)` — for bool options, returns erased `true`.
    ///   For non-bool, returns `Err` (callers echo the value instead).
    /// - `Negate(name)` — returns erased `false` (rejects non-bool via
    ///   the option's own error message).
    /// - `Assign { name, value }` — parses `value` against the option's
    ///   [`crate::OptionType`] and validates without writing.
    /// - `Query(name)` — always returns [`ConfigError::QueryNotAllowed`];
    ///   callers use `:setlocal name?` echo path instead.
    /// - `Reset(name)` — returns [`ConfigError::QueryNotAllowed`]; the
    ///   caller's `:setlocal name&` clear-override path handles it before
    ///   reaching here.
    ///
    /// # Examples
    ///
    /// ```
    /// use lattice_config::{ConfigRegistry, OptionKey, OptionOverride, Tabstop};
    ///
    /// let reg = ConfigRegistry::new();
    /// reg.init_from_linkme();
    ///
    /// let (key, value, name) = reg.parse_for_buffer_local("ts=2").unwrap();
    /// assert_eq!(key, OptionKey::of::<Tabstop>());
    /// assert_eq!(name, "tabstop");
    /// assert_eq!(value.downcast_ref::<i64>(), Some(&2));
    /// // The global value is untouched.
    /// assert_eq!(*reg.get_typed::<Tabstop>().unwrap(), 4);
    ///
    /// let ov = OptionOverride { key, value, priority: Default::default() };
    /// assert_eq!(ov.downcast_value::<i64>(), Some(&2));
    /// ```
    pub fn parse_for_buffer_local(
        &self,
        input: &str,
    ) -> Result<
        (
            crate::OptionKey,
            std::sync::Arc<dyn std::any::Any + Send + Sync>,
            String,
        ),
        ConfigError,
    > {
        let parsed = parse_set(input).map_err(ConfigError::Parse)?;
        match parsed {
            ParsedSet::NameOnly(name) => {
                let opt = self
                    .lookup(&name)
                    .ok_or(ConfigError::UnknownOption(name.clone()))?;
                if !opt.is_bool() {
                    return Err(ConfigError::Parse(format!(
                        "E474: use :setlocal {name}=value to set a non-boolean option"
                    )));
                }
                let key = self.key_for(opt.name())?;
                let erased = opt
                    .parse_to_erased("true")
                    .map_err(ConfigError::Validation)?;
                Ok((key, erased, opt.name().to_string()))
            }
            ParsedSet::Negate(name) => {
                let opt = self
                    .lookup(&name)
                    .ok_or(ConfigError::UnknownOption(name.clone()))?;
                if !opt.is_bool() {
                    return Err(ConfigError::NotBoolean(name));
                }
                let key = self.key_for(opt.name())?;
                let erased = opt
                    .parse_to_erased("false")
                    .map_err(ConfigError::Validation)?;
                Ok((key, erased, opt.name().to_string()))
            }
            ParsedSet::Assign { name, value } => {
                let opt = self
                    .lookup(&name)
                    .ok_or(ConfigError::UnknownOption(name.clone()))?;
                let key = self.key_for(opt.name())?;
                let erased = opt
                    .parse_to_erased(&value)
                    .map_err(ConfigError::Validation)?;
                Ok((key, erased, opt.name().to_string()))
            }
            ParsedSet::Query(name) | ParsedSet::Reset(name) => {
                Err(ConfigError::QueryNotAllowed(name))
            }
        }
    }

    /// RF.2e: `name`'s value as `resolved` holds it, formatted the way
    /// `:set name?` prints it — the per-buffer view of [`Self::lookup`] +
    /// `get_formatted`, for a reader holding a buffer's resolved snapshot
    /// (`config.get-option-in-buffer`).
    ///
    /// Falls back to the global value when `resolved` is `None` or holds no
    /// entry for the option (an option registered after the snapshot was
    /// taken): nothing buffer-local is known to apply. `None` only for an
    /// option no one registered.
    pub fn format_resolved(
        &self,
        name: &str,
        resolved: std::option::Option<&crate::ResolvedOptions>,
    ) -> std::option::Option<String> {
        let opt = self.lookup(name)?;
        let local = resolved.and_then(|r| {
            let key = self.key_for(opt.name()).ok()?;
            opt.format_erased_value(r.get_erased(&key)?)
        });
        Some(local.unwrap_or_else(|| opt.get_formatted()))
    }

    /// Internal helper for [`Self::key_for_name`]: the native key when the
    /// option was registered with a declaration type
    /// (`register_with_typeid`), else the plugin key by canonical name.
    /// `ConfigError::UnknownOption` for a name nothing registered.
    fn key_for(&self, name: &str) -> Result<crate::OptionKey, ConfigError> {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        let &idx = inner
            .by_name
            .get(name)
            .ok_or_else(|| ConfigError::UnknownOption(name.to_string()))?;
        if let Some((tid, _)) = inner.by_typeid.iter().find(|(_, i)| **i == idx) {
            return Ok(crate::OptionKey::Native(*tid));
        }
        let canonical = inner
            .by_id
            .get(idx)
            .and_then(|slot| slot.as_ref())
            .map(|o| o.name().to_string())
            .ok_or_else(|| ConfigError::UnknownOption(name.to_string()))?;
        Ok(crate::OptionKey::Plugin(canonical.into()))
    }

    fn erased_at(&self, idx: usize) -> std::option::Option<Arc<dyn ErasedOption>> {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        inner
            .by_id
            .get(idx)
            .and_then(|slot| slot.as_ref())
            .map(Arc::clone)
    }
}

impl std::fmt::Debug for ConfigRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        f.debug_struct("ConfigRegistry")
            .field("count", &inner.by_id.iter().filter(|o| o.is_some()).count())
            .finish_non_exhaustive()
    }
}

/// The name `name` (or an alias of it) was registered under, if it was.
fn canonical_name(inner: &Inner, name: &str) -> std::option::Option<String> {
    inner
        .by_name
        .get(name)
        .and_then(|i| inner.by_id[*i].as_ref())
        .map(|o| o.name().to_string())
}

/// Runtime-registered options that have no compile-time declaration — i.e.
/// plugin options — grouped by their dotted namespace.
///
/// `:customize` and its completion are both built on the compile-time decl
/// slices, which only native options join. A plugin option reaches `:set` and
/// `:describe-option` (both read this registry) but would otherwise be absent
/// from every browse surface. The namespace IS the group, which is what native
/// groups already are (`ai.log` + `ai.log_level` -> `ai`); the host assigns the
/// prefix from the plugin id, so a plugin can neither collide with another nor
/// squat a native group.
pub fn plugin_option_groups(
    reg: &ConfigRegistry,
) -> std::collections::BTreeMap<String, Vec<Arc<dyn ErasedOption>>> {
    let declared: std::collections::HashSet<&'static str> =
        crate::OPTION_DECLS.iter().map(|d| d.name).collect();
    let mut out: std::collections::BTreeMap<String, Vec<Arc<dyn ErasedOption>>> =
        std::collections::BTreeMap::new();
    for opt in reg.iter() {
        let name = opt.name().to_string();
        if declared.contains(name.as_str()) {
            continue;
        }
        // Un-namespaced runtime options are not plugin contributions; skip
        // rather than invent a group for them.
        let Some((ns, _)) = name.split_once('.') else {
            continue;
        };
        out.entry(ns.to_string()).or_default().push(opt);
    }
    for opts in out.values_mut() {
        opts.sort_by_key(|o| o.name().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    /// A structured `list<string>` option, the shape of `org.agenda-files`.
    fn list_option(r: &ConfigRegistry, name: &str) {
        r.try_register(Option::<crate::ConfigValue>::structured(
            name.to_string(),
            crate::ConfigSchema::list(crate::ConfigSchema::string()),
            crate::ConfigValue::List(Vec::new()),
            "Files.".to_string(),
        ))
        .unwrap();
    }

    fn strings(items: &[&str]) -> crate::ConfigValue {
        crate::ConfigValue::List(
            items
                .iter()
                .map(|s| crate::ConfigValue::Str((*s).to_string()))
                .collect(),
        )
    }

    /// The owner sees each contributor's value, in first-contributed order; a
    /// re-contribution replaces in place, `None` withdraws, and the user's
    /// own value is untouched throughout.
    #[test]
    fn contributions_are_kept_per_contributor_beside_the_value() {
        let r = ConfigRegistry::new();
        list_option(&r, "org.agenda-files");
        assert!(r.accept_contributions("org.agenda-files"));

        r.contribute(
            "org.agenda-files",
            "org-clients",
            Some(strings(&["~/org/clients"])),
        )
        .unwrap();
        r.contribute("org.agenda-files", "other", Some(strings(&["~/x"])))
            .unwrap();
        r.contribute(
            "org.agenda-files",
            "org-clients",
            Some(strings(&["~/org/c2"])),
        )
        .unwrap();
        let got = r.contributions("org.agenda-files");
        assert_eq!(
            got.iter()
                .map(|c| c.contributor.as_str())
                .collect::<Vec<_>>(),
            ["org-clients", "other"],
            "a replacement keeps its place"
        );
        assert_eq!(got[0].value, strings(&["~/org/c2"]));
        assert_eq!(
            r.lookup("org.agenda-files").unwrap().get_value(),
            strings(&[]),
            "the user's value is not touched"
        );

        r.contribute("org.agenda-files", "org-clients", None)
            .unwrap();
        assert_eq!(r.contributions("org.agenda-files").len(), 1);
        r.contribute("org.agenda-files", "never", None)
            .expect("withdrawing nothing is not an error");
    }

    /// Nothing is accepted for an option whose owner did not ask, for an
    /// unknown option, or in a shape the option cannot hold — and each
    /// refusal says which.
    #[test]
    fn a_contribution_is_refused_unless_accepted_and_well_shaped() {
        let r = ConfigRegistry::new();
        list_option(&r, "org.agenda-files");
        let err = r
            .contribute("org.agenda-files", "org-clients", Some(strings(&["a"])))
            .unwrap_err();
        assert!(err.contains("does not accept"), "{err}");
        assert!(!r.accept_contributions("no.such"));
        assert!(
            r.contribute("no.such", "org-clients", None)
                .unwrap_err()
                .contains("no option")
        );

        r.accept_contributions("org.agenda-files");
        let err = r
            .contribute(
                "org.agenda-files",
                "org-clients",
                Some(crate::ConfigValue::Str("~/org".into())),
            )
            .unwrap_err();
        assert!(err.contains("expected"), "a misfit names the schema: {err}");
        assert!(r.contributions("org.agenda-files").is_empty());
    }

    /// `:describe-option` names each contributing plugin with its value, and
    /// has no section at all when nothing is contributed.
    #[test]
    fn describe_contributions_names_each_plugin() {
        let r = ConfigRegistry::new();
        list_option(&r, "org.agenda-files");
        r.accept_contributions("org.agenda-files");
        assert_eq!(r.describe_contributions("org.agenda-files"), None);
        r.contribute(
            "org.agenda-files",
            "org-clients",
            Some(strings(&["~/org/clients"])),
        )
        .unwrap();
        let lines = r.describe_contributions("org.agenda-files").unwrap();
        assert_eq!(lines[0], "## Contributed by plugins");
        assert_eq!(lines.last().unwrap(), r#"- org-clients: ["~/org/clients"]"#);
    }

    /// An owner that unloads stops seeing contributions, and one that comes
    /// back and accepts again sees the ones its dependents left — while a
    /// contributor's own unload takes back everything it made.
    #[test]
    fn contributions_survive_the_owner_and_go_with_the_contributor() {
        let r = ConfigRegistry::new();
        list_option(&r, "org.agenda-files");
        list_option(&r, "org.other");
        r.accept_contributions("org.agenda-files");
        r.accept_contributions("org.other");
        r.contribute("org.agenda-files", "org-clients", Some(strings(&["a"])))
            .unwrap();
        r.contribute("org.other", "org-clients", Some(strings(&["b"])))
            .unwrap();
        r.contribute("org.other", "keep", Some(strings(&["c"])))
            .unwrap();

        r.unregister("org.agenda-files");
        assert!(r.contributions("org.agenda-files").is_empty());
        list_option(&r, "org.agenda-files");
        assert!(
            r.contributions("org.agenda-files").is_empty(),
            "re-registered but not yet accepting"
        );
        r.accept_contributions("org.agenda-files");
        assert_eq!(
            r.contributions("org.agenda-files").len(),
            1,
            "back in effect"
        );

        assert_eq!(r.withdraw_contributions("org-clients"), 2);
        assert!(r.contributions("org.agenda-files").is_empty());
        assert_eq!(
            r.contributions("org.other")
                .iter()
                .map(|c| c.contributor.as_str())
                .collect::<Vec<_>>(),
            ["keep"]
        );
    }

    /// RF.2e: the buffer's value where the snapshot has one, the global
    /// value where it does not, and nothing for an unknown name.
    #[test]
    fn format_resolved_reads_the_snapshot_then_falls_back_to_global() {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        let mut resolved = crate::ResolvedOptions::new();
        resolved.insert::<crate::ExpandTab>(false);
        assert_eq!(
            r.format_resolved("expandtab", Some(&resolved)).as_deref(),
            Some("false")
        );
        assert_eq!(
            r.format_resolved("expandtab", None).as_deref(),
            Some("true")
        );
        assert_eq!(
            r.format_resolved("shiftwidth", Some(&resolved)).as_deref(),
            Some("4"),
            "absent from the snapshot: the global value"
        );
        assert_eq!(r.format_resolved("no-such-option", Some(&resolved)), None);
    }

    /// RF.4e′: an option with no declaration type — a plugin's — has a key
    /// too, its canonical name, so it can be overridden per buffer. The
    /// override resolves through the same layers a native one does, aliases
    /// land on the same key, and a snapshot without the override reads the
    /// global value.
    #[test]
    fn a_runtime_option_is_keyed_by_name_and_resolves_per_buffer() {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        r.register(
            Option::<String>::builder("myplugin.style", "plain".to_string(), "Style.")
                .aliases(&["mps"])
                .build(),
        );
        let key = r.key_for_name("mps").expect("registered");
        assert_eq!(key, crate::OptionKey::Plugin("myplugin.style".into()));
        assert_eq!(
            r.key_for_name("expandtab"),
            Some(crate::OptionKey::of::<crate::ExpandTab>())
        );
        assert_eq!(r.key_for_name("no-such-option"), None);

        let (key, value, name) = r.parse_for_buffer_local("myplugin.style=fancy").unwrap();
        assert_eq!(name, "myplugin.style");
        let local: crate::OptionOverrideSet = [crate::OptionOverride {
            key,
            value,
            priority: Default::default(),
        }]
        .into_iter()
        .collect();
        let mut resolved = crate::ResolvedOptions::new();
        crate::Resolver::new()
            .resolve_into_with_origins([(&local, crate::OptionOrigin::BufferLocal)], &mut resolved);
        assert_eq!(
            r.format_resolved("mps", Some(&resolved)).as_deref(),
            Some("fancy")
        );
        assert_eq!(
            resolved.get_origin_for(&crate::OptionKey::Plugin("myplugin.style".into())),
            crate::OptionOrigin::BufferLocal
        );
        assert_eq!(
            r.format_resolved("myplugin.style", Some(&crate::ResolvedOptions::new()))
                .as_deref(),
            Some("plain"),
            "no override in this buffer: the global value"
        );
        assert_eq!(
            r.format_resolved("myplugin.style", None).as_deref(),
            Some("plain"),
            "the global value is untouched"
        );
    }

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
    fn unregister_removes_name_and_aliases_and_recycles_the_slot() {
        let r = ConfigRegistry::new();
        // Two options, one with an alias; capture the aliased option's slot idx.
        let keep = r.register(Option::<i64>::new("keep", 1, "sibling"));
        let doomed = r.register(
            Option::<i64>::builder("plugin.opt", 8, "a plugin option")
                .aliases(&["po"])
                .build(),
        );
        let doomed_idx = doomed.idx;
        assert_eq!(r.len(), 2);
        assert!(r.lookup("plugin.opt").is_some());
        assert!(r.lookup("po").is_some());

        // Unregister by ALIAS: canonical name + alias both go, the slot frees,
        // len drops, the sibling is untouched.
        assert!(r.unregister("po"));
        assert_eq!(r.len(), 1);
        assert!(r.lookup("plugin.opt").is_none());
        assert!(r.lookup("po").is_none());
        assert_eq!(*r.get(keep), 1);

        // Idempotent: a second unload (canonical or alias) removes nothing.
        assert!(!r.unregister("plugin.opt"));
        assert!(!r.unregister("po"));

        // Reload: re-registering reuses the freed slot rather than growing by_id.
        let reloaded = r.register(Option::<i64>::new("plugin.opt", 9, "reloaded"));
        assert_eq!(reloaded.idx, doomed_idx, "freed slot is recycled");
        assert_eq!(r.len(), 2);
        assert_eq!(*r.get(reloaded), 9);
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
        // PL8.F: bind the `iter()` temp so the `&str` names (now borrowed from
        // each Arc's `Cow` field, not `'static`) outlive the collect.
        let opts = r.iter();
        let names: Vec<&str> = opts.iter().map(|o| o.name()).collect();
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

    // ---- Event::OptionChanged publish (DESIGN.md §5.10 + §5.12) ----

    fn capture_events() -> (
        EventPublisher,
        Arc<std::sync::Mutex<Vec<lattice_protocol::Event>>>,
    ) {
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let cap = captured.clone();
        let publisher: EventPublisher = Arc::new(move |event| {
            cap.lock().expect("captured poisoned").push(event);
        });
        (publisher, captured)
    }

    #[test]
    fn typed_set_publishes_option_changed_event() {
        use lattice_protocol::Event;
        let r = ConfigRegistry::new();
        let h = r.register(Option::<bool>::new("number", true, ""));
        let (publisher, captured) = capture_events();
        r.set_event_publisher(publisher);
        r.set(h, false).unwrap();
        let events = captured.lock().unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::OptionChanged { name, old, new } => {
                assert_eq!(name, "number");
                assert_eq!(old.as_deref(), Some("true"));
                assert_eq!(new, "false");
            }
            other => panic!("expected OptionChanged, got {other:?}"),
        }
    }

    #[test]
    fn parse_and_set_command_publishes_for_assign() {
        use lattice_protocol::Event;
        let r = ConfigRegistry::new();
        r.register(Option::<i64>::new("tabstop", 8, ""));
        let (publisher, captured) = capture_events();
        r.set_event_publisher(publisher);
        r.parse_and_set_command("tabstop=4").unwrap();
        let events = captured.lock().unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::OptionChanged { name, old, new } => {
                assert_eq!(name, "tabstop");
                assert_eq!(old.as_deref(), Some("8"));
                assert_eq!(new, "4");
            }
            other => panic!("expected OptionChanged, got {other:?}"),
        }
    }

    #[test]
    fn parse_and_set_command_publishes_for_negate() {
        use lattice_protocol::Event;
        let r = ConfigRegistry::new();
        r.register(Option::<bool>::new("number", true, ""));
        let (publisher, captured) = capture_events();
        r.set_event_publisher(publisher);
        r.parse_and_set_command("nonumber").unwrap();
        let events = captured.lock().unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::OptionChanged { name, old, new } => {
                assert_eq!(name, "number");
                assert_eq!(old.as_deref(), Some("true"));
                assert_eq!(new, "false");
            }
            other => panic!("expected OptionChanged, got {other:?}"),
        }
    }

    #[test]
    fn parse_and_set_command_publishes_for_bool_toggle() {
        use lattice_protocol::Event;
        let r = ConfigRegistry::new();
        r.register(Option::<bool>::new("number", false, ""));
        let (publisher, captured) = capture_events();
        r.set_event_publisher(publisher);
        r.parse_and_set_command("number").unwrap();
        let events = captured.lock().unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::OptionChanged { name, old, new } => {
                assert_eq!(name, "number");
                assert_eq!(old.as_deref(), Some("false"));
                assert_eq!(new, "true");
            }
            other => panic!("expected OptionChanged, got {other:?}"),
        }
    }

    #[test]
    fn parse_and_set_command_query_does_not_publish() {
        let r = ConfigRegistry::new();
        r.register(Option::<bool>::new("number", true, ""));
        let (publisher, captured) = capture_events();
        r.set_event_publisher(publisher);
        r.parse_and_set_command("number?").unwrap();
        assert!(captured.lock().unwrap().is_empty());
    }

    #[test]
    fn no_publisher_set_means_no_events() {
        let r = ConfigRegistry::new();
        let h = r.register(Option::<bool>::new("number", true, ""));
        // Don't set a publisher.
        r.set(h, false).unwrap();
        // No panic, no event capture; just a silent set.
        assert!(!*r.get(h));
    }

    #[test]
    fn alias_set_publishes_under_canonical_name() {
        use lattice_protocol::Event;
        // `:set ts=4` (alias) should publish OptionChanged with
        // canonical name "tabstop", not "ts".
        let r = ConfigRegistry::new();
        r.register(
            Option::<i64>::builder("tabstop", 8, "")
                .aliases(&["ts"])
                .build(),
        );
        let (publisher, captured) = capture_events();
        r.set_event_publisher(publisher);
        r.parse_and_set_command("ts=4").unwrap();
        let events = captured.lock().unwrap();
        assert_eq!(events.len(), 1);
        if let Event::OptionChanged { name, .. } = &events[0] {
            assert_eq!(name, "tabstop", "expected canonical name");
        } else {
            panic!("expected OptionChanged");
        }
    }

    #[test]
    fn validation_error_does_not_publish() {
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
        let (publisher, captured) = capture_events();
        r.set_event_publisher(publisher);
        let _ = r.parse_and_set_command("tabstop=999");
        assert!(captured.lock().unwrap().is_empty());
    }
}
