//! The mode system's foundation: the `Mode` trait, the mode registry, the
//! per-buffer set of active modes, and the typed lifecycle events — plus the
//! generic host seams a mode uses to own its whole surface (action handlers,
//! services, inbound wakes, buffer creation) and the foundation modes that
//! have no other owning crate.
//!
//! The major / minor mode system is the primary customization mechanism
//! (DESIGN.md §5.8, `docs/dev/architecture/mode-architecture.md`). A buffer
//! has exactly one **major** mode (content-type identity: `rust-mode`,
//! `help-mode`'s markdown major, `messages-mode`) and any number of **minor**
//! modes layered over it (`line-numbers-mode`, `table-mode`,
//! `emacs-keys-mode`). A mode contributes declaratively — option overrides,
//! a keymap layer, completion sources, gutter signs, action handlers — and
//! imperatively through one lifecycle hook whose returned Guard is the only
//! cleanup path.
//!
//! ## What this crate owns
//!
//! - **The contract.** [`Mode`] (and its object-safe adapter [`DynMode`]),
//!   [`ModeId`], [`ModeKind`], [`ActivationPolicy`], [`EditableTail`],
//!   [`CapabilitySet`], [`ModeContext`], [`LifecycleFuture`] and
//!   [`ModeActivationError`].
//! - **Activation.** [`ModeRegistry`] registers modes and drives activation /
//!   deactivation against a buffer's [`ActiveModes`], stashing each
//!   activation's Guard in a [`GuardStoreHandle`]. Observable transitions
//!   (`MajorEntered` / `MinorActivated` / …) ride the protocol `Event` enum;
//!   internal failures ride [`ModeEvent`].
//! - **The host seams a mode needs to own its surface without depending on
//!   the host.** [`ServiceRegistry`] (typed services),
//!   [`ActionHandlerRegistry`] (chord bodies), [`ModeActivator`] and
//!   [`BufferStore`] (buffer creation / lookup), [`SubsystemBoot`] (a
//!   subsystem's one-line `install`), [`inbound`] and
//!   [`TickCallbackRegistry`] (off-keystroke results that wake the editor),
//!   [`idle_gate`] (armed deadlines), [`ProviderViewRegistry`] (open a
//!   provider's view), [`ForegroundCancel`], and the producer registries
//!   for plugin-backed content ([`MediaSourceRegistry`],
//!   [`ContextSourceRegistry`], [`GutterDecorationSourceRegistry`],
//!   [`ScannedExcerptSourceRegistry`], [`BufferScopeSourceRegistry`]).
//! - **Shared render-facing vocabularies** a mode writes without seeing a
//!   renderer: gutter signs ([`SignRegistry`], [`GutterDecoration`]), the
//!   modeline element model ([`ModelineService`],
//!   [`ModelineElementUpdate`]), async highlight / inlay hand-off
//!   ([`PendingSyntheticHighlights`], [`PendingInlays`]), buffer-locals
//!   ([`BufferLocals`]).
//! - **Foundation modes** ([`modes`], registered by
//!   [`register_foundation_modes`]): `text-mode`, `help-mode`,
//!   `hover-mode`, `messages-mode`, `image-mode`, the completion and display
//!   minors, `table-mode`, `surround-mode`, `which-key-mode`, and the shared
//!   minors that own one chord for a whole class of view
//!   ([`RefreshableViewMode`] `gr`, [`FoldableViewMode`] `<Tab>`,
//!   [`ReplMode`], [`EmacsKeysMode`]). Feature-crate modes live with their
//!   feature (`lattice-lsp`, `lattice-listing`, `lattice-magit`, …).
//!
//! ## What it must not depend on, and why
//!
//! Nothing above it: not `lattice-host`, no renderer (`lattice-ui-tui`,
//! `lattice-ui-gpui`), no feature crate. Every feature crate depends on this
//! one to declare its modes, and the host depends on every feature crate, so
//! a dependency upward is a cycle — and, more to the point, it is the
//! structural guarantee that a mode can own its keymap, handler bodies,
//! buffers and async wakes **without an `Editor::` method or a host `Action`
//! variant** (the mode-ownership acid test). Where a mode needs the host, the
//! host implements a trait defined here ([`ModeActivator`], [`BufferStore`],
//! [`SubsystemBoot`]) or registers a service. Its own dependencies are the
//! substrate below: protocol, core, grammar, keymap, config, completion,
//! runtime, cells.
//!
//! ## Example: a minimal minor mode
//!
//! ```
//! use lattice_core::BufferKind;
//! use lattice_mode::{
//!     ActivationPolicy, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, ModeRegistry,
//!     OptionOverrideSet,
//! };
//!
//! /// Wraps long lines in prose buffers.
//! struct ProseMode;
//!
//! impl Mode for ProseMode {
//!     type Guard = (); // nothing to clean up
//!     fn id(&self) -> ModeId {
//!         ModeId::new("prose-mode")
//!     }
//!     fn kind(&self) -> ModeKind {
//!         ModeKind::Minor
//!     }
//!     fn options(&self) -> OptionOverrideSet {
//!         lattice_config::overrides! { lattice_config::Wrap = true, }
//!     }
//!     fn activation_policy(&self) -> ActivationPolicy {
//!         ActivationPolicy::Majors(vec![ModeId::new("markdown-mode")])
//!     }
//!     fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
//!         Box::pin(async { Ok(()) })
//!     }
//! }
//!
//! let mut registry = ModeRegistry::new();
//! let id = registry.register(ProseMode).unwrap();
//! // The host's minor resolver asks this when a buffer enters a major.
//! assert_eq!(registry.auto_activatable_minors("markdown-mode", BufferKind::Document), vec![id]);
//! assert!(registry.auto_activatable_minors("rust-mode", BufferKind::Document).is_empty());
//! ```
//!
//! The [`Mode`] docs carry the full lifecycle (registration → activation →
//! deactivation) as a runnable example; [`SubsystemBoot`] shows a whole
//! subsystem install with an off-thread producer.
//!
//! ## Design documents
//!
//! - `docs/dev/architecture/mode-architecture.md` — the mode model,
//!   activation, Guards, option layering (§5–§9).
//! - `docs/dev/architecture/boot-composition.md` — `SubsystemBoot`, the
//!   inbound primitive and why the wake lives in the sender (§3).
//! - `docs/dev/architecture/keymap-architecture.md` — keymap layers.
//! - `docs/dev/architecture/modeline.md` — the modeline element model.
//! - `docs/dev/architecture/cancellation.md` — [`ForegroundCancel`].

#![warn(missing_docs)]

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
pub mod operator_chord;
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
pub mod idle_gate;
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
pub mod language_server;
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
    BUILTIN_SIGN_COLUMNS,
    BuiltinSignIds,
    CompilationSeverityData,
    DIAGNOSTIC_ERROR_PRIORITY,
    DIAGNOSTIC_HINT_PRIORITY,
    DIAGNOSTIC_INFO_PRIORITY,
    DIAGNOSTIC_WARNING_PRIORITY,
    DIFF_SIGN_PRIORITY,
    DecorationCtx,
    DecorationProvider,
    DiagnosticGlyphs,
    GutterDecoration,
    GutterDiffKind,
    GutterSeverityLevel,
    Keymap,
    KeymapBinding,
    SIGN_COLUMN_DIFF,
    SIGN_COLUMN_MARK,
    SignDefinition,
    SignId,
    SignRegistry,
    SignRegistryHandle,
    Subscription, // MO.4.c: real RAII type; use in mode Guards
    register_builtin_signs,
    winning_sign,
};
pub use crate::decoration_source::{
    AsyncGutterDecorationSource, DecorationEpoch, DecorationEpochHandle, DecorationFuture,
    GutterDecorationSourceRegistry, GutterDecorationSourceRegistryHandle,
};
pub use crate::error::ModeActivationError;
pub use crate::event::ModeEvent;
pub use crate::guards::{GuardStore, GuardStoreHandle};
pub use crate::language_server::{
    LanguageServerRegistrar, LanguageServerRegistrarHandle, LanguageServerSpec,
};
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
    register_help_mode_actions,
};
pub use crate::operator_chord::{OperatorChordWirer, OperatorChordWirerHandle};
// TB.1: `table-mode` — the shared pipe-table minor. Re-exported beside the
// other shared minors so boot reaches it by the same path.
pub use crate::modes::table::mode::{TableMode, register_table_actions, register_table_mode};
pub use crate::plugin_meta_sink::{PluginMetaSink, PluginMetaSinkHandle};
pub use crate::provider_view::{
    ProviderViewOpener, ProviderViewOutcome, ProviderViewRegistry, ProviderViewRegistryHandle,
};
pub use crate::scanned_excerpt_source::{
    ClockSpan, RowAnnotation, ScanBeginFuture, ScanDescribeFuture, ScanFuture, ScanResult,
    ScannedExcerpt, ScannedExcerptSource, ScannedExcerptSourceRegistry,
    ScannedExcerptSourceRegistryHandle,
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
