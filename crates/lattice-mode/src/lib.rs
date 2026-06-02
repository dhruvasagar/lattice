//! Mode foundation: `Mode` trait, registry, lifecycle (M.1).
//!
//! The major / minor mode system is the primary customization
//! mechanism per DESIGN.md §5.8 / docs/dev/architecture/mode-architecture.md. This
//! crate is the foundation -- the trait surface, the activation
//! registry, the per-buffer `ActiveModes` set, and the typed
//! lifecycle event payloads. No actual modes are registered here;
//! M.3 lands the major modes for current buffer kinds, M.5 lands
//! `lsp-mode`, etc.
//!
//! ## What's in this slice (M.1)
//!
//! - [`Mode`] trait: declarative contributions (options, keymap,
//!   subscriptions, decorations) plus capabilities, conflicts,
//!   implies, and lifecycle hooks.
//! - [`ModeId`]: interned-string identity. Cross-crate uniqueness
//!   for free; `Copy + Eq + Hash` for hot-path lookups.
//! - [`ModeRegistry`]: register modes, look them up, drive
//!   activation / deactivation against a per-buffer
//!   [`ActiveModes`] set.
//! - [`ActiveModes`]: the major + ordered-minors set per buffer.
//! - [`ModeEvent`]: typed lifecycle event payloads matching
//!   DESIGN.md §5.10. The registry returns the events activation /
//!   deactivation produces; the caller forwards to the actual
//!   typed event bus (M.4 wires that). M.1 keeps the registry
//!   bus-agnostic so it can be tested in isolation.
//! - [`CapabilitySet`]: typed bitfield of buffer capabilities a
//!   mode may require (`BUFFER_URI`, `LSP`, `TREE_SITTER`, ...).
//! - [`ModeContext`]: read-only context passed to lifecycle
//!   hooks. Per `mode-architecture.md` §5.2 modes do not mutate
//!   the registry from `on_activate` / `on_deactivate`; the
//!   declarative contributions are applied by the registry, and
//!   the hook is for side effects (server connection, watcher,
//!   ...) only.
//!
//! ## Stub types still pending real impls
//!
//! `Keymap`, `Subscription`, and `DecorationProvider` remain
//! placeholders in `contributions.rs`. Real impls land in:
//!
//! - `Keymap` -- when the layered keymap registry from
//!   `keymap-architecture.md` exposes a public Keymap type for
//!   modes to contribute. Until then, the placeholder lets the
//!   trait surface be complete.
//! - `Subscription` -- when the typed event bus stabilises a
//!   subscription type (DESIGN.md §5.10).
//! - `DecorationProvider` -- M.4 / decoration registry.
//!
//! As of M.2.1, `OptionOverride` / `OptionOverrideSet` /
//! `OverridePriority` are real types in `overrides.rs`; the
//! resolver and `ResolvedOptions` cache live in `lattice-config`
//! (see `mode-architecture.md` §6.3 / §9.3 for why the split).

// M.10.1 (2026-06-02): action-handler registry — mode-
// contributed closures per `CommandId`. Required so modes own
// BOTH chord choice (already done via `keymap()`) AND handler
// body (this substrate), per `feedback_mode_owns_its_surface`
// + `mode-architecture.md` §5.3. Host's chord-resolved-action
// dispatcher consults via `lookup`; mode's `Guard` carries
// `ActionHandlerRegistration` tokens whose `Drop` unregisters.
pub mod action_handler_registry;
pub mod active;
pub mod activator;
pub mod binding_mode;
pub mod buffer_store;
pub mod capability;
pub mod context;
pub mod contributions;
pub mod error;
pub mod event;
pub mod guards;
// K.2.4.A.0.1 (2026-06-02): static keymap catalog + `keymap_entry!`
// macro relocated from `lattice-host::keymap` so mode crates can
// declare bindings without depending on `lattice-host`. The host
// retains `lattice-host::keymap` as a re-export shim so the
// dispatcher / TUI drift test / `:describe-key` / `:keymap` keep
// resolving verbatim.
pub mod keymap_entry;
pub mod locals;
pub mod mode;
pub mod modes;
pub mod registry;
pub mod services;

pub use crate::action_handler_registry::{
    ActionContext, ActionHandler, ActionHandlerRegistration, ActionHandlerRegistry,
    ActionHandlerRegistryHandle,
};
pub use crate::active::ActiveModes;
pub use crate::activator::ModeActivator;
pub use crate::binding_mode::BindingMode;
pub use crate::buffer_store::{BufferStore, BufferStoreHandle};
pub use crate::capability::CapabilitySet;
pub use crate::context::ModeContext;
pub use crate::contributions::{DecorationProvider, Keymap, KeymapBinding, Subscription};
pub use crate::keymap_entry::KeymapEntry;
pub use crate::error::ModeActivationError;
pub use crate::event::ModeEvent;
pub use crate::guards::{GuardStore, GuardStoreHandle};
pub use crate::locals::{BufferLocal, BufferLocals, LocalDescriptor};
pub use crate::mode::{DynMode, LifecycleFuture, Mode, ModeId, ModeKind};
pub use crate::modes::{
    ActiveCompletionSources, BufferWordsMode, CompletionMode, CompletionPopupMode, HelpMode,
    HoverMode, MessagesMode, PathCompletionMode, TextMode, register_foundation_modes,
};
pub use crate::services::ServiceRegistry;
// M.4 dep-inversion: layer-input types live in `lattice-config`
// now. Re-exported here for compatibility -- callers that
// imported from `lattice_mode` keep working.
pub use crate::registry::{ModeRegistry, RegistrationError};
pub use lattice_config::{OptionOverride, OptionOverrideSet, OverridePriority};
