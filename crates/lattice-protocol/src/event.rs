//! Events published by the core to subscribed clients.
//!
//! Per DESIGN.md §5.10: every meaningful editor state transition
//! publishes a typed event. Vim's `autocmd` and emacs's hooks both
//! desugar to the same `EventBus::subscribe` call (filter +
//! sink). The `Event` enum is the catalog; `EventKind` is the
//! discriminator used by filter dispatch.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::{BufferId, DocumentId};
use crate::position::Range;
use crate::selection::SelectionSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    /// Fired when a document buffer opens. Subscribers (the
    /// LSP attach driver, future plugin hooks, project-watcher,
    /// completion warmer) react asynchronously; the publisher
    /// (`App::new` for the initial document, `App::do_edit` for
    /// subsequent `:e <path>` opens) returns immediately. The
    /// event-driven design keeps the UI thread off the LSP
    /// `initialize` round-trip -- aligned with paramount goal
    /// #4 (asynchronicity).
    ///
    /// `path` is `None` for unsaved scratch buffers (no LSP
    /// attach work to drive). `text` carries the buffer's
    /// initial content so subscribers don't have to reach back
    /// through a document handle on the publish path -- LSP
    /// hands it straight to `didOpen`.
    DocumentOpened {
        id: DocumentId,
        path: Option<PathBuf>,
        version: u64,
        text: String,
    },
    DocumentClosed {
        id: DocumentId,
    },
    /// Fired before [`Self::DocumentSaved`]. Observation-only in
    /// v1; future revisions may carry a payload that handlers can
    /// mutate (formatters rewriting buffer content) or veto
    /// (return Err to abort the save).
    BeforeSave {
        id: DocumentId,
        path: PathBuf,
    },
    DocumentSaved {
        id: DocumentId,
        path: PathBuf,
    },
    DocumentChanged {
        id: DocumentId,
        /// The buffer's filesystem path, if it has one. Carried so
        /// subscribers can resolve URIs without holding their own
        /// DocumentId -> path map. `None` for scratch / unsaved
        /// buffers.
        path: Option<PathBuf>,
        version: u64,
        edits: Vec<AppliedEdit>,
    },
    SelectionsChanged {
        id: DocumentId,
        version: u64,
        selections: SelectionSet,
    },
    /// Fired when the modal state transitions
    /// (Normal -> Insert, Insert -> Normal, ...). Carries the
    /// previous and next state as opaque labels; the App owns the
    /// `ModalState` type so the protocol layer keeps it as String.
    ModalModeChanged {
        from: String,
        to: String,
    },
    /// Fired before the editor exits. Observation-only in v1; the
    /// veto path (a handler returning Err to abort quit) layers on
    /// once the bus grows the Before-event mutation semantics.
    BeforeQuit,
    /// Fired after a typed-options registry value changes
    /// (DESIGN.md §5.12). Carries the option's canonical name plus
    /// the formatted old / new value strings -- string-formatted
    /// (rather than `Box<dyn Any>`) because most subscribers just
    /// react to the change signal and don't need the typed value.
    /// Subscribers that need the typed value re-read through the
    /// registry (`config.with(handle, |v| ...)`).
    ///
    /// `old` is `None` for the very first publish after registration
    /// (when the option is initialised to its default and no prior
    /// value exists); subsequent edits always carry both sides.
    OptionChanged {
        name: String,
        old: Option<String>,
        new: String,
    },
    /// A major mode became the active major on `buffer` (published
    /// *after* the mode's `on_activate` resolved, so subscribers see
    /// a consistent state). `major` is the major mode's canonical
    /// name (e.g. `rust-mode`) -- carried as a `String` so the
    /// protocol layer stays free of the `ModeId` type, mirroring
    /// [`Self::ModalModeChanged`].
    ///
    /// This is the event minor-mode activation triggers filter on:
    /// `EventFilter.major_modes` matches against `major`
    /// (mode-architecture.md §7.4). Published by the mode
    /// dispatcher's cascade task (MA.1); supersedes the prior typed
    /// `ModeEvent::MajorEntered` so the EF.1 filter machinery applies.
    MajorEntered {
        buffer: BufferId,
        major: String,
    },
    /// The active major mode on `buffer` is about to be deactivated
    /// (published *before* the mode's Guard drops, so subscribers can
    /// inspect what's being torn down). Pairs with
    /// [`Self::MajorEntered`] for minor-mode teardown. `major` is the
    /// canonical name of the major being torn down.
    MajorExiting {
        buffer: BufferId,
        major: String,
    },
    /// A minor mode was activated on `buffer` (published *after* its
    /// `on_activate` resolved). `minor` is the minor mode's canonical
    /// name. The full observable mode-lifecycle quartet
    /// (`MajorEntered`/`MajorExiting`/`MinorActivated`/`MinorDeactivated`)
    /// lives on this `Event` enum (design.md §5.10.1) so hooks /
    /// `EventFilter` apply uniformly; only the internal
    /// `ModeActivationFailed` / `OptionConflict` cascade signals stay
    /// on the typed `lattice_mode::ModeEvent` bus.
    MinorActivated {
        buffer: BufferId,
        minor: String,
    },
    /// A minor mode was deactivated on `buffer` (published *before*
    /// its Guard drops). `minor` is the minor mode's canonical name.
    MinorDeactivated {
        buffer: BufferId,
        minor: String,
    },
    /// A plugin-DEFINED event (PH7.8b). Unlike every arm above -- each a
    /// closed, host-owned editor-core transition -- this arm is the OPEN
    /// escape hatch a runtime-loaded plugin publishes through
    /// (`host-services emit-event`). The host is a thin router: `name` is
    /// the plugin's event identifier (declared via `register-event`, surfaced
    /// in the runtime event registry, `event_registry`); `payload` is opaque
    /// MessagePack the *plugin* owns and the host NEVER interprets -- the
    /// boundary discipline the plugin host rests on. Every plugin event shares
    /// this one variant + [`EventKind::Plugin`]; subscribers filter by `name`
    /// inside their handler (the bus discriminates only to `Plugin`, not
    /// per-name), so a new plugin event needs no enum/WIT change.
    Plugin {
        name: String,
        payload: Vec<u8>,
    },
    /// A plugin instance crashed (a lifecycle / callback export trapped: fuel
    /// exhaustion, epoch deadline, a guest panic, or any wasm trap) and was
    /// quarantined by the host (PH7.12). Unlike [`Self::Plugin`] -- the OPEN
    /// escape hatch a *live* plugin publishes through -- this is a CLOSED,
    /// host-owned lifecycle transition the host itself originates, mirroring the
    /// mode-lifecycle quartet ([`Self::MinorDeactivated`] et al.): the host is
    /// the sole publisher, and subscribers (a future crash-notification surface,
    /// the Phase-8 plugin manager's reload/health UI) filter it by *kind*, not by
    /// string-matching a name.
    ///
    /// Fired exactly once per instance -- on the first trap that trips
    /// quarantine. A component trap taints its instance irrecoverably (wasmtime
    /// offers no rollback), so the instance is dead-until-reinstantiation: every
    /// later call short-circuits without re-entering the dead `Store`, and no
    /// further `PluginCrashed` fires until a reload (PH7.12b) mints a fresh
    /// instance. The guarantee is **isolation**: the editor, actor, other
    /// plugins, LSP, and UI are untouched.
    ///
    /// `plugin` is the host-issued numeric plugin id (the same id inside
    /// `SourceLayer::Plugin(id)`); `func` is the export that trapped
    /// (`"on-event"` / `"spec"` / `"generate"` / `"apply-motion"` / ...); `kind`
    /// is a stable machine label (`"fuel"` / `"epoch"` / `"trap"`) -- a `String`
    /// (not the host's `TrapKind`) so the protocol layer stays free of the
    /// plugin-host type, mirroring [`Self::ModalModeChanged`].
    PluginCrashed {
        plugin: u32,
        func: String,
        kind: String,
    },
    /// A plugin finished loading (CI.1): every seam drained, its modes /
    /// options / commands all registered. Published by the loader at
    /// `load_discovered` completion. UNLIKE [`Self::PluginCrashed`], this IS
    /// delivered to guests — an `init.rs` subscribes (filtered by `name`) to run
    /// deferred config against a now-present plugin (`with-eval-after-load`;
    /// config-and-init.md). `name` is the manifest id; `id` the host-issued
    /// numeric plugin id.
    PluginLoaded { name: String, id: u32 },
    /// A plugin was unloaded (CI.1): teardown reversed its contributions
    /// (`:plugin-unload` / crash-teardown). Delivered to guests so a handler can
    /// tear down its own dependent setup. Fields mirror [`Self::PluginLoaded`].
    PluginUnloaded { name: String, id: u32 },
    /// A request to enable/disable a minor mode globally (CI.4) — the
    /// guest-to-Editor bridge for `enable-mode` / `disable-mode`. A plugin
    /// (init.rs) calls the modes-seam `enable-mode` from an `on-plugin-loaded`
    /// handler; the host publishes THIS, and the Editor (which owns the mode
    /// registry + the open-buffer set + the activator) flips the enablement and
    /// re-activates open buffers. Host-internal — NOT delivered back to guests
    /// (like [`Self::PluginCrashed`]). `mode` is the mode id.
    ModeEnablementRequested { mode: String, enabled: bool },
}

// M.5.3.b: `LspLogPushed`, `LspBufferAttached`, and
// `LspBufferDetached` moved out of this enum and into
// `lattice-lsp::events` as concrete types implementing
// [`crate::event_registry::Event`]. They publish via the
// typed-bus path (`EventBus::publish_typed`); subscribers
// use `EventBus::subscribe_typed::<T>`. Future cleanup will
// migrate the rest of the enum the same way.

impl Event {
    /// Project the event to its [`EventKind`] discriminator. Used
    /// by [`crate::cancel`]-style filter dispatch in the runtime
    /// layer's event bus to avoid string-matching variant names.
    pub fn kind(&self) -> EventKind {
        match self {
            Event::DocumentOpened { .. } => EventKind::DocumentOpened,
            Event::DocumentClosed { .. } => EventKind::DocumentClosed,
            Event::BeforeSave { .. } => EventKind::BeforeSave,
            Event::DocumentSaved { .. } => EventKind::DocumentSaved,
            Event::DocumentChanged { .. } => EventKind::DocumentChanged,
            Event::SelectionsChanged { .. } => EventKind::SelectionsChanged,
            Event::ModalModeChanged { .. } => EventKind::ModalModeChanged,
            Event::BeforeQuit => EventKind::BeforeQuit,
            Event::OptionChanged { .. } => EventKind::OptionChanged,
            Event::MajorEntered { .. } => EventKind::MajorEntered,
            Event::MajorExiting { .. } => EventKind::MajorExiting,
            Event::MinorActivated { .. } => EventKind::MinorActivated,
            Event::MinorDeactivated { .. } => EventKind::MinorDeactivated,
            Event::Plugin { .. } => EventKind::Plugin,
            Event::PluginCrashed { .. } => EventKind::PluginCrashed,
            Event::PluginLoaded { .. } => EventKind::PluginLoaded,
            Event::PluginUnloaded { .. } => EventKind::PluginUnloaded,
            Event::ModeEnablementRequested { .. } => EventKind::ModeEnablementRequested,
        }
    }
}

/// Discriminator for [`Event`] variants. Stored in
/// `EventFilter::kinds` and used by the bus to bucket
/// subscriptions per kind so publish does no global iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventKind {
    DocumentOpened,
    DocumentClosed,
    BeforeSave,
    DocumentSaved,
    DocumentChanged,
    SelectionsChanged,
    ModalModeChanged,
    BeforeQuit,
    OptionChanged,
    MajorEntered,
    MajorExiting,
    MinorActivated,
    MinorDeactivated,
    /// Discriminator for every plugin-defined event ([`Event::Plugin`]). All
    /// plugin events share this one kind; the per-event `name` is NOT a bus
    /// discriminator (subscribers filter by name in their handler, PH7.8b).
    Plugin,
    /// Discriminator for [`Event::PluginCrashed`] -- the host-originated
    /// crash/quarantine lifecycle transition (PH7.12). A single kind so a
    /// crash-notification surface or the Phase-8 plugin manager subscribes to
    /// every plugin crash with one filter.
    PluginCrashed,
    /// Discriminator for [`Event::PluginLoaded`] (CI.1) — the plugin-load
    /// lifecycle signal an `init.rs` subscribes to for deferred config.
    PluginLoaded,
    /// Discriminator for [`Event::PluginUnloaded`] (CI.1).
    PluginUnloaded,
    /// Discriminator for [`Event::ModeEnablementRequested`] (CI.4) — the
    /// host-internal enable/disable-minor-mode bridge the Editor handles.
    ModeEnablementRequested,
}

/// An edit as actually applied to the buffer (the original `Edit` plus the
/// resulting range, useful for clients that want to know what changed).
///
/// `inserted_text` carries the text that was placed into `inserted_range`.
/// Together with `original_range` this is exactly what an LSP
/// `textDocument/didChange` payload needs, which lets the
/// `lattice-lsp` fan-in synthesise a `lattice_protocol::edit::Edit`
/// from this event without re-reading the buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedEdit {
    pub original_range: Range,
    pub inserted_range: Range,
    pub replaced_text: String,
    pub inserted_text: String,
}
