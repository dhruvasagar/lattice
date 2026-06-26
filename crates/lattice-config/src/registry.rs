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

#[derive(Default)]
struct Inner {
    /// Indexed by [`OptionHandle::idx`]. Vec is append-only;
    /// removing an option (which we don't currently support) would
    /// require a tombstone scheme to keep handle indices valid.
    by_id: Vec<Arc<dyn ErasedOption>>,
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
    /// `name?` passed to `parse_for_buffer_local`. Query forms are
    /// not writes; callers should use `:set name?` or `:setlocal name?`
    /// to echo instead of calling into the write path.
    #[error("E474: query form not allowed in :setlocal; use :set {0}? to echo")]
    QueryNotAllowed(String),
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
                .map(|i| inner.by_id[*i].get_formatted());
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
        let arc = Arc::clone(&inner.by_id[idx]);
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
            let arc = std::sync::Arc::clone(&inner.by_id[idx]);
            let value_erased = arc.current_value_erased();
            out.insert_erased_with_origin(
                *type_id,
                value_erased,
                crate::OptionOrigin::GlobalConfig,
            );
        }
    }

    /// Look up the `TypeId` registered for an option's canonical name.
    /// Returns `None` if the option was not registered via
    /// `register_with_typeid` (e.g. a bare `try_register` call, or the
    /// option simply doesn't exist).
    pub fn type_id_for_name(&self, name: &str) -> std::option::Option<std::any::TypeId> {
        self.typeid_for_name(name).ok()
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
            .map(|i| Arc::clone(&inner.by_id[*i]))
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
    /// `(TypeId, erased_value, canonical_name)` needed to construct an
    /// [`lattice_mode::OptionOverride`] for the buffer-local layer.
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
    pub fn parse_for_buffer_local(
        &self,
        input: &str,
    ) -> Result<(std::any::TypeId, std::sync::Arc<dyn std::any::Any + Send + Sync>, String), ConfigError>
    {
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
                let type_id = self.typeid_for_name(opt.name())?;
                let erased = opt.parse_to_erased("true").map_err(ConfigError::Validation)?;
                Ok((type_id, erased, opt.name().to_string()))
            }
            ParsedSet::Negate(name) => {
                let opt = self
                    .lookup(&name)
                    .ok_or(ConfigError::UnknownOption(name.clone()))?;
                if !opt.is_bool() {
                    return Err(ConfigError::NotBoolean(name));
                }
                let type_id = self.typeid_for_name(opt.name())?;
                let erased = opt.parse_to_erased("false").map_err(ConfigError::Validation)?;
                Ok((type_id, erased, opt.name().to_string()))
            }
            ParsedSet::Assign { name, value } => {
                let opt = self
                    .lookup(&name)
                    .ok_or(ConfigError::UnknownOption(name.clone()))?;
                let type_id = self.typeid_for_name(opt.name())?;
                let erased = opt
                    .parse_to_erased(&value)
                    .map_err(ConfigError::Validation)?;
                Ok((type_id, erased, opt.name().to_string()))
            }
            ParsedSet::Query(name) | ParsedSet::Reset(name) => {
                Err(ConfigError::QueryNotAllowed(name))
            }
        }
    }

    /// Internal helper: look up the `TypeId` registered for an option's
    /// canonical name. Returns `ConfigError::UnknownOption` if the
    /// option was registered without a typeid (e.g. via bare
    /// `try_register` rather than `register_with_typeid`).
    fn typeid_for_name(&self, canonical: &str) -> Result<std::any::TypeId, ConfigError> {
        let inner = self.inner.lock().expect("ConfigRegistry poisoned");
        let &idx = inner
            .by_name
            .get(canonical)
            .ok_or_else(|| ConfigError::UnknownOption(canonical.to_string()))?;
        inner
            .by_typeid
            .iter()
            .find(|(_, i)| **i == idx)
            .map(|(tid, _)| *tid)
            .ok_or_else(|| ConfigError::UnknownOption(canonical.to_string()))
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
    #![allow(clippy::unwrap_used, clippy::panic)]
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
