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
pub mod activator;
pub mod active;
pub mod binding_mode;
// OM.A1: the native seam a WASM agenda-row producer implements. Sibling of
// `media_source` — async, host-driven off the keystroke path, once per file of
// a project walk. The source declares which extensions it wants offered, which
// is what keeps a filetype out of the host's walk.
pub mod buffer_store;
pub mod capability;
pub mod context;
pub mod scanned_excerpt_source;
// TC.2: the native seam a WASM sticky-context producer implements. Sibling of
// `decoration_source` — async, host-driven off the render path, result cached.
pub mod context_source;
pub mod contributions;
pub mod decoration_source;
pub mod media_source;
// BC.5: `emacs-keys-mode` — a default-on universal builtin minor mode (the
// `<C-x>` leader tribute). Moved here from `lattice-host`; registered with the
// foundation modes. The host keeps only the keymap-layer push (config + the
// live `KeymapHandle`).
pub mod emacs_keys_mode;
// CG.2 (2026-08-08): foreground cancellation as a registered service,
// so a provider can enrol work from any `&self` context (action
// handlers, event subscriptions) and not just where `&mut Editor` is
// reachable. See `docs/dev/architecture/cancellation.md`.
pub mod error;
pub mod event;
pub mod foldable_view_mode;
pub mod foreground_cancel;
pub mod guards;
pub mod refreshable_view_mode;
pub mod repl_mode;
// Boot-composition BC.1: the generic *inbound* primitive — a channel whose
// `send` wakes the editor (`async_landed`) and whose items drain per-tick
// through a handler. Pairs with `tick_callback`; generalizes the I3
// `ClaudeCodeInboundBus` + LSP's hand-rolled inbound buses. The wake is baked
// into the sender so it cannot be forgotten (`boot-composition.md` §3).
pub mod inbound;
// K.3 (2026-06-07): `KeymapEntry` + `keymap_entry!` live in
// `lattice-keymap::keymap_entry`. lattice-mode re-exports the MODULE and
// the `#[macro_export]` macro with a single `pub use` — the name
// `keymap_entry` resolves in both the type namespace (the module) and the
// macro namespace, so `lattice_mode::keymap_entry! { … }` AND
// `lattice_mode::keymap_entry::{KeymapEntry, default_keymap, …}` keep
// working for callers in `lattice-multibuffer`, `lattice-host`, and
// `lattice-ui-tui` WITHOUT duplicating the macro body. The macro's
// `$crate` resolves to `lattice_keymap` regardless of the re-export path
// (so callers need no direct `lattice-keymap` dep). See
// `project_keymap_entry_macro_dual_copy` — the former duplicate is gone.
pub use lattice_keymap::keymap_entry;
pub mod locals;
pub mod mode;
pub mod modeline;
// MG.2: pending synthetic-buffer highlights service, shared between
// lattice-host (drain) and lattice-magit (async refresh tasks).
pub mod modes;
pub mod pending_inlays;
pub mod pending_synthetic_highlights;
pub mod plugin_meta_sink;
// PV.1 (2026-08-12): the generic provider-view seam — one host primitive
// for "open the multibuffer view a provider owns", replacing the
// per-provider `AppEffect` variant + host arm + plugin-boundary arm.
pub mod provider_view;
pub mod registry;
pub mod services;
// DB.5 (design.md §9.1): the generic `Startup` boot-completion typed event.
// Declared here (alongside `ModeEvent`) so subsystem `install(&mut boot)`
// fns can subscribe without a `lattice-host` dependency.
pub mod startup;
// IDE-protocol I1.1: the one generic host primitive — a per-tick drain
// closure registry. Generalizes the host's hardcoded `drain_<x>` methods
// so a mode owns its channel + drain body (`feedback_mode_owns_its_surface`).
pub mod tick_callback;
// Boot-composition BC.3b: the capability surface a subsystem's `install(boot)`
// wires against. Lives here (below every subsystem crate) so subsystems name
// the capability, not the host's concrete `BootContext` (which would cycle).
pub mod subsystem_boot;

pub use crate::action_handler_registry::{
    ActionContext, ActionHandler, ActionHandlerContribution, ActionHandlerRegistration,
    ActionHandlerRegistry, ActionHandlerRegistryHandle,
};
pub use crate::activator::{ModeActivator, VirtualRowRegistrar};
pub use crate::active::ActiveModes;
pub use crate::binding_mode::BindingMode;
pub use crate::buffer_store::{BufferStore, BufferStoreHandle};
pub use crate::capability::CapabilitySet;
pub use crate::context::ModeContext;
pub use crate::context_source::{
    AsyncContextSource, ContextFuture, ContextSourceRegistry, ContextSourceRegistryHandle,
};
pub use crate::contributions::{
    CompilationSeverityData,
    DecorationCtx,
    DecorationProvider,
    GutterDecoration,
    GutterDiffKind,
    GutterSeverityLevel,
    Keymap,
    KeymapBinding,
    Subscription, // MO.4.c: real RAII type; use in mode Guards
};
pub use crate::decoration_source::{
    AsyncGutterDecorationSource, DecorationFuture, GutterDecorationSourceRegistry,
    GutterDecorationSourceRegistryHandle,
};
pub use crate::error::ModeActivationError;
pub use crate::event::ModeEvent;
pub use crate::guards::{GuardStore, GuardStoreHandle};
pub use crate::locals::{
    BufferLocal, BufferLocals, BufferScopeDir, BufferScopeSource, BufferScopeSourceRegistry,
    BufferScopeSourceRegistryHandle, LocalDescriptor,
};
pub use crate::media_source::{
    AsyncMediaSource, MediaBlockRequest, MediaFuture, MediaSourceRegistry,
    MediaSourceRegistryHandle,
};
pub use crate::mode::{
    ActivationPolicy, DynMode, EditableTail, LifecycleFuture, Mode, ModeId, ModeKind,
};
pub use crate::modes::{
    ActiveCompletionSources, BufferWordsMode, CompletionMode, CompletionPopupMode, HelpMode,
    HoverMode, MessagesMode, PathCompletionMode, TextMode, register_foundation_modes,
};
// TB.1: `table-mode` — the shared pipe-table minor. Re-exported beside the
// other shared minors so boot reaches it by the same path.
pub use crate::modes::table::mode::{TableMode, register_table_actions, register_table_mode};
pub use crate::plugin_meta_sink::{PluginMetaSink, PluginMetaSinkHandle};
pub use crate::provider_view::{
    ProviderViewOpener, ProviderViewOutcome, ProviderViewRegistry, ProviderViewRegistryHandle,
};
pub use crate::scanned_excerpt_source::{
    ClockSpan, ScanBeginFuture, ScanDescribeFuture, ScanFuture, ScanResult, ScannedExcerpt,
    ScannedExcerptSource, ScannedExcerptSourceRegistry, ScannedExcerptSourceRegistryHandle,
};
pub use crate::services::ServiceRegistry;
pub use crate::startup::Startup;
pub use crate::subsystem_boot::SubsystemBoot;
pub use crate::tick_callback::{
    TickCallback, TickCallbackRegistration, TickCallbackRegistry, TickCallbackRegistryHandle,
};
pub use lattice_keymap::KeymapEntry;
// BC.5: the host pushes the `<C-x>` leader layer (it owns the `KeymapHandle` +
// config), calling `emacs_keys_layer_bindings`; `EmacsKeysMode::mode_id` keys
// the layer + the K.1.c per-keystroke gate.
pub use crate::emacs_keys_mode::{EmacsKeysMode, emacs_keys_layer_bindings};
pub use crate::repl_mode::{ReplMode, register_repl_mode, register_repl_mode_actions};
// RV.1: the one place `gr` means "refresh this view" — the chord lives
// here, each view's mode declares its own `refresh_action()` target.
pub use crate::refreshable_view_mode::{
    RefreshableViewMode, VIEW_REFRESH_ACTION, register_refreshable_view_actions,
    register_refreshable_view_mode,
};
// OA.4b: the one place `<Tab>` folds the block at point — the chord lives
// here, each view's mode declares its own `fold_toggle_action()` target.
pub use crate::foldable_view_mode::{
    FOLD_TOGGLE_DEFAULT_ACTION, FoldableViewMode, VIEW_FOLD_CYCLE_ACTION, VIEW_FOLD_TOGGLE_ACTION,
    register_foldable_view_actions, register_foldable_view_mode,
};
// ML.0a: configurable-modeline element model + descriptor registry.
pub use crate::foreground_cancel::{ForegroundCancel, ForegroundCancelHandle};
pub use crate::modeline::{
    ElementContent, ElementId, HoverSpec, Interaction, ModelineElement, ModelineElementUpdate,
    ModelineKey, ModelineRegistry, ModelineRole, ModelineService, ModelineServiceHandle,
    ModelineSnapshot, Scope, Span, Zone,
};
pub use crate::pending_inlays::{InlayRow, PendingInlays, PendingInlaysHandle};
pub use crate::pending_synthetic_highlights::{
    HighlightsOp, PendingSyntheticHighlights, PendingSyntheticHighlightsHandle,
};
// M.4 dep-inversion: layer-input types live in `lattice-config`
// now. Re-exported here for compatibility -- callers that
// imported from `lattice_mode` keep working.
pub use crate::registry::{ModeRegistry, ModeRegistryHandle, RegistrationError};
pub use lattice_config::{OptionOverride, OptionOverrideSet, OverridePriority};
