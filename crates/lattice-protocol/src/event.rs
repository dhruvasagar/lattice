//! Events published by the core to subscribed clients.
//!
//! Per DESIGN.md §5.10: every meaningful editor state transition
//! publishes a typed event. Vim's `autocmd` and emacs's hooks both
//! desugar to the same `EventBus::subscribe` call (filter +
//! sink). The `Event` enum is the catalog; `EventKind` is the
//! discriminator used by filter dispatch.
//!
//! This is the *closed* catalogue: editor-core transitions the host owns.
//! Feature crates declare their own events as types through
//! [`crate::event_registry`] instead of growing this enum, and plugins
//! publish theirs through the one open arm, [`Event::Plugin`].
//!
//! The bus itself (`lattice_runtime::EventBus`) is not here — this crate has
//! no runtime. Publishing is fire-and-forget unless a variant says otherwise.
//! Most variants are also delivered to WASM plugins (mirrored in WIT by
//! `lattice-plugin-host`); the ones marked *host-internal* below are refused
//! at that boundary.
//!
//! Versions: `version` fields carry `lattice_core::Document`'s counters.
//! [`Event::DocumentOpened`] carries the *text* version (bumps on text
//! changes only); [`Event::DocumentChanged`] and [`Event::SelectionsChanged`]
//! carry the whole-document version, which also bumps on selection changes.
//!
//! # Examples
//!
//! ```
//! use lattice_protocol::{DocumentId, Event, EventKind};
//! use std::path::PathBuf;
//!
//! let saved = Event::DocumentSaved {
//!     id: DocumentId::new(3),
//!     path: PathBuf::from("src/lib.rs"),
//! };
//! // Filters bucket on the payload-free discriminator.
//! assert_eq!(saved.kind(), EventKind::DocumentSaved);
//! assert_eq!(Event::BeforeQuit.kind(), EventKind::BeforeQuit);
//! ```

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::{BufferId, DocumentId};
use crate::position::Range;
use crate::selection::SelectionSet;

/// One editor-core state transition, as published on the event bus.
///
/// See the [module docs](crate::event) for how this relates to typed events and the
/// plugin boundary; each variant says when it fires, who publishes it, and
/// what its fields carry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    /// Fired when a document buffer opens. Subscribers (the
    /// LSP attach driver, future plugin hooks, project-watcher,
    /// completion warmer) react asynchronously; the publisher
    /// (`Editor::publish_document_opened_for_active`, run at boot for
    /// the initial document and after each `:e <path>` open) returns
    /// immediately. The
    /// event-driven design keeps the UI thread off the LSP
    /// `initialize` round-trip -- aligned with paramount goal
    /// #4 (asynchronicity).
    ///
    /// `path` is `None` for unsaved scratch buffers (no LSP
    /// attach work to drive). `text` carries the buffer's
    /// initial content so subscribers don't have to reach back
    /// through a document handle on the publish path -- LSP
    /// hands it straight to `didOpen`.
    ///
    /// Caveat: the current publisher builds `id` from the raw value of the
    /// buffer-registry id (`DocumentId::new(buffer_id.0)`), whereas every
    /// other document event carries the document's own [`DocumentId`]. The
    /// two number spaces are not guaranteed to agree.
    DocumentOpened {
        /// The opened document (see the caveat above).
        id: DocumentId,
        /// Its file path; `None` for a scratch buffer.
        path: Option<PathBuf>,
        /// The document's *text* version at open — the version LSP's
        /// `didOpen` starts from.
        version: u64,
        /// The full initial content.
        text: String,
    },
    /// A document was closed. Subscribers drop per-document state keyed by
    /// `id` (the LSP references provider, multibuffer excerpts, diff and VCS
    /// caches all do).
    ///
    /// Note: the variant is subscribed to and mirrored in WIT, but no
    /// production code path publishes it yet.
    DocumentClosed {
        /// The closed document.
        id: DocumentId,
    },
    /// Fired before [`Self::DocumentSaved`]. Observation-only in
    /// v1; future revisions may carry a payload that handlers can
    /// mutate (formatters rewriting buffer content) or veto
    /// (return Err to abort the save).
    ///
    /// Published by the host's save paths (`:w` and background saves)
    /// immediately before the write.
    BeforeSave {
        /// The document about to be written.
        id: DocumentId,
        /// Where it is about to be written.
        path: PathBuf,
    },
    /// A document was written to disk successfully (after
    /// [`Self::BeforeSave`]; a failed write publishes nothing further).
    /// Published by the host's save paths, including background saves of
    /// buffers the user is not looking at.
    DocumentSaved {
        /// The saved document.
        id: DocumentId,
        /// The path actually written.
        path: PathBuf,
    },
    /// A document's text changed. Published by the host after each applied
    /// edit (and by `lattice-multibuffer` when an edit through a multibuffer
    /// lands in its source document). LSP's `didChange`, the multibuffer's
    /// excerpt refresh and the diff subsystem feed on it.
    DocumentChanged {
        /// The changed document.
        id: DocumentId,
        /// The buffer's filesystem path, if it has one. Carried so
        /// subscribers can resolve URIs without holding their own
        /// DocumentId -> path map. `None` for scratch / unsaved
        /// buffers.
        path: Option<PathBuf>,
        /// The document's whole version after the change (see the module
        /// docs on versions).
        version: u64,
        /// The edits, in the order they were applied; each one's ranges are
        /// in the coordinates of the buffer as the previous one left it.
        /// Today's publishers send one edit per event.
        edits: Vec<AppliedEdit>,
    },
    /// The selection set of a document changed — visual extension, a
    /// selection-changing effect, `gv`. Published by
    /// `Editor::publish_selections_changed`. Carries the complete new set,
    /// not a delta.
    SelectionsChanged {
        /// The document whose selections changed.
        id: DocumentId,
        /// The document's whole version after the change.
        version: u64,
        /// The full new selection set.
        selections: SelectionSet,
    },
    /// Fired when the modal state transitions
    /// (Normal -> Insert, Insert -> Normal, ...). Carries the
    /// previous and next state as opaque labels; the App owns the
    /// `ModalState` type so the protocol layer keeps it as String.
    ///
    /// Published by the host's modal-state setter, only on a real
    /// transition. The labels are the `Debug` rendering of the host's
    /// `ModalState` (`"Normal"`, `"Insert"`, ...).
    ModalModeChanged {
        /// The state being left.
        from: String,
        /// The state being entered.
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
    ///
    /// Published by `lattice_config::ConfigRegistry` through its injected
    /// publisher, after the write and outside the registry lock (so a
    /// handler may read other options).
    OptionChanged {
        /// The option's canonical name (`tabstop`, not `ts`).
        name: String,
        /// The previous value, formatted; `None` on the first publish.
        old: Option<String>,
        /// The new value, formatted as `:set` would print it.
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
        /// The buffer the major mode is now active on.
        buffer: BufferId,
        /// The major mode's canonical name.
        major: String,
    },
    /// The active major mode on `buffer` is about to be deactivated
    /// (published *before* the mode's Guard drops, so subscribers can
    /// inspect what's being torn down). Pairs with
    /// [`Self::MajorEntered`] for minor-mode teardown. `major` is the
    /// canonical name of the major being torn down.
    MajorExiting {
        /// The buffer the major mode is leaving.
        buffer: BufferId,
        /// The major mode's canonical name.
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
        /// The buffer the minor mode is now active on.
        buffer: BufferId,
        /// The minor mode's canonical name.
        minor: String,
    },
    /// A minor mode was deactivated on `buffer` (published *before*
    /// its Guard drops). `minor` is the minor mode's canonical name.
    MinorDeactivated {
        /// The buffer the minor mode is leaving.
        buffer: BufferId,
        /// The minor mode's canonical name.
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
        /// The plugin-declared event name (`my-plugin.file-indexed`).
        name: String,
        /// Opaque MessagePack bytes, owned and interpreted only by plugins.
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
    ///
    /// Host-internal: refused at the plugin boundary, never delivered to a
    /// guest.
    PluginCrashed {
        /// The host-issued numeric id of the quarantined plugin instance.
        plugin: u32,
        /// The export that trapped (`"on-event"`, `"spec"`, ...).
        func: String,
        /// Why: `"fuel"`, `"epoch"` or `"trap"`.
        kind: String,
    },
    /// A named plugin is ABOUT to run the load-time exports that read its own
    /// options (OA.14d). Published by the loader mid-load: after the plugin's
    /// `config` seam drained — so every option it declares EXISTS and can be
    /// set — and before every seam that consumes one.
    ///
    /// It exists because [`Self::PluginLoaded`] is too late for a value the
    /// plugin reads at load. org derives its per-keyword theme elements and its
    /// generated highlight-query rules from `org.todo-keywords` inside
    /// `register-theme-elements`; a handler that runs after the load sets an
    /// option nothing will read again until a restart.
    ///
    /// `name` is the manifest id, and it is carried rather than left implicit
    /// so a handler discriminates: config for a plugin you know is legible,
    /// config invited to run for every plugin on the disk is not.
    ///
    /// **Delivery is awaited.** The loader publishes this via
    /// `EventBus::publish_awaited` and does not continue the load until every
    /// guest handler has returned — an unawaited publish would leave the
    /// handler racing the very export it exists to precede. It is the one event
    /// with that property; everything else on the bus is fire-and-forget.
    PrePluginLoaded {
        /// The loading plugin's manifest id. (No numeric id: guests receive
        /// the name only.)
        name: String,
    },
    /// A plugin finished loading (CI.1): every seam drained, its modes /
    /// options / commands all registered. Published by the loader at
    /// `load_discovered` completion. UNLIKE [`Self::PluginCrashed`], this IS
    /// delivered to guests — an `init.rs` subscribes (filtered by `name`) to run
    /// deferred config against a now-present plugin (`with-eval-after-load`;
    /// config-and-init.md). `name` is the manifest id; `id` the host-issued
    /// numeric plugin id.
    PluginLoaded {
        /// The plugin's manifest id.
        name: String,
        /// The host-issued numeric plugin id.
        id: u32,
    },
    /// A plugin was unloaded (CI.1): teardown reversed its contributions
    /// (`:plugin-unload` / crash-teardown). Delivered to guests so a handler can
    /// tear down its own dependent setup. Fields mirror [`Self::PluginLoaded`].
    PluginUnloaded {
        /// The plugin's manifest id.
        name: String,
        /// The host-issued numeric plugin id it had while loaded.
        id: u32,
    },
    /// A request to enable/disable a minor mode globally (CI.4) — the
    /// guest-to-Editor bridge for `enable-mode` / `disable-mode`. A plugin
    /// (init.rs) calls the modes-seam `enable-mode` from an `on-plugin-loaded`
    /// handler; the host publishes THIS, and the Editor (which owns the mode
    /// registry + the open-buffer set + the activator) flips the enablement and
    /// re-activates open buffers. Host-internal — NOT delivered back to guests
    /// (like [`Self::PluginCrashed`]). `mode` is the mode id.
    ModeEnablementRequested {
        /// The minor mode's id.
        mode: String,
        /// `true` to enable globally, `false` to disable.
        enabled: bool,
    },
    /// A request to set an option for ONE buffer — the guest-to-Editor bridge
    /// for `set-option-in-buffer`, and the peer of
    /// [`Self::ModeEnablementRequested`] in both shape and reason.
    ///
    /// The config seam's `set-option` writes the GLOBAL layer (it is the
    /// `:set` path). A handler that wants "wrap in org buffers" cannot use it:
    /// it would wrap everything, and nothing would unwrap on leaving org. The
    /// buffer-local layer is what expresses that, and it lives on the Editor
    /// (`buffer_local_overrides`) rather than in the `ConfigRegistry` the
    /// plugin host holds — hence the bridge.
    ///
    /// Host-internal, NOT delivered back to guests: a plugin observing every
    /// other plugin's option writes is a surveillance seam nobody asked for,
    /// and `option-changed` already reports the outcome.
    BufferOptionOverrideRequested {
        /// The buffer the override applies to.
        buffer: BufferId,
        /// `name=value` in `:set` syntax, parsed by the same
        /// `parse_for_buffer_local` the `:setlocal` path uses — so a guest
        /// cannot express anything `:setlocal` could not, and a bad value is
        /// rejected with the same message.
        option: String,
    },
    /// MG.41g: a long-running background operation finished.
    ///
    /// The decoupling seam between *producers* of async work (magit's
    /// git invocations, LSP requests, a plugin's task) and whatever
    /// *reports* completion. Producers publish this and never mention
    /// notifications; the notification layer is one subscriber, so the
    /// policy — which levels surface, whether to notify at all, rate
    /// limiting — lives in one place and is configurable later without
    /// touching a single producer.
    ///
    /// Replaces threading a `NotificationStoreHandle` into every
    /// spawner, which was opt-in and therefore already forgotten in
    /// five of magit's ten (`spawn_git`, the generic one, among them).
    ///
    /// Published today by magit's git spawners; `lattice-notify` is the
    /// subscriber that turns it into a notification. Not yet mirrored in WIT,
    /// so it is not delivered to guests.
    BackgroundTaskFinished {
        /// Subsystem that ran it — `"magit"`, `"lsp"`, a plugin id.
        /// Lets a subscriber filter without parsing `label`.
        source: String,
        /// NC.2: what the work was done *in* — a repository name, a
        /// project, a server. `None` when the work has no such place.
        ///
        /// A field, not part of `label`, so every producer's scope
        /// lands in the same place on screen: several notifications at
        /// once are told apart by reading one column, not by parsing
        /// each producer's own phrasing.
        scope: Option<String>,
        /// What was done, as an imperative phrase naming its object:
        /// `"push main → origin/main"`, `"drop stash@{2}"`. The outcome
        /// is appended by whoever reports it, so the label must read
        /// correctly before "failed" as well as before a summary.
        label: String,
        /// How it ended.
        outcome: TaskOutcome,
    },
    /// OR.2: files under a directory a plugin asked the host to watch
    /// (`host-services.watch`) changed on disk.
    ///
    /// **Addressed, not broadcast.** `plugin` is the host-issued numeric id of
    /// the plugin whose watch produced this, and the event-delivery actor drops
    /// any delivery whose `plugin` is not its own. The bus is a broadcast and a
    /// watch is a capability: a plugin granted `fs:read` over one directory
    /// must not learn which files under *another* plugin's watched directory
    /// changed, and it would if this rode [`Self::Plugin`] — every
    /// `EventKind::Plugin` subscriber sees every plugin event. The id never
    /// crosses to a guest, because by the time it does it is always the guest's
    /// own.
    ///
    /// **Many paths, one event.** A `git pull` that rewrites two hundred files
    /// produces one delivery carrying two hundred paths, not two hundred
    /// deliveries: the host coalesces a burst behind a quiet window before
    /// publishing. A consumer re-reads what changed, so ordering within the
    /// batch carries no meaning and the paths are deduplicated and sorted.
    ///
    /// Published by `lattice-plugin-host`'s watch host.
    FilesChanged {
        /// The host-issued numeric plugin id that armed the watch.
        plugin: u32,
        /// Absolute paths that changed, created or were removed. A removal is
        /// reported as a change — the consumer stats the path — because an
        /// index that cannot see deletions offers destinations that no longer
        /// exist.
        paths: Vec<std::path::PathBuf>,
    },
    /// LH.0: a long-running host job a plugin started has made progress.
    ///
    /// A *job* is what a host-service returns an id for when its work outlasts
    /// the call: a download (`host-services.http-download`) today. The event
    /// is the same for every kind — see `lattice-plugin-host`'s `job` module
    /// for why adding a kind must not add a variant here.
    ///
    /// **Addressed**, exactly as [`Self::FilesChanged`] is and for its reason:
    /// the bus is a broadcast, and what a plugin is fetching or running is
    /// nobody else's business. The delivery actor drops any delivery whose
    /// `plugin` is not its own, and the id never crosses to a guest.
    ///
    /// **Coalesced.** At most one per quiet interval per job, so fast work is
    /// a handful of guest calls. A job that finishes inside one interval
    /// publishes none — a consumer must not wait for progress before expecting
    /// [`Self::JobFinished`].
    JobProgress {
        /// The host-issued numeric plugin id that started the job.
        plugin: u32,
        /// The id the host-service returned.
        id: u64,
        /// Units done so far. The seam that started the job says what a unit
        /// is — bytes, for a download.
        done: u64,
        /// The total in the same units, when it is known.
        total: Option<u64>,
    },
    /// LH.0.3: a host job produced output — the lines a subprocess wrote
    /// (`host-services.spawn-process`), stdout and stderr interleaved as they
    /// arrived.
    ///
    /// **Batched**, not one event per line: a quiet interval's worth at a
    /// time, bounded in size, with nothing dropped. Addressed like
    /// [`Self::JobProgress`].
    JobOutput {
        /// The host-issued numeric plugin id that started the job.
        plugin: u32,
        /// The id the host-service returned.
        id: u64,
        /// Whole lines, without their terminators, in arrival order.
        lines: Vec<String>,
    },
    /// LH.0: a host job ended. Exactly one per job, always, including a
    /// cancelled one: a consumer drives a state machine off this and a job
    /// that could end silently would strand it.
    ///
    /// Addressed like [`Self::JobProgress`].
    JobFinished {
        /// The host-issued numeric plugin id that started the job.
        plugin: u32,
        /// The id the host-service returned.
        id: u64,
        /// `Ok` once the job did what it was asked; otherwise why not, in
        /// words a user can act on.
        result: Result<(), String>,
    },
}

/// MG.41g: how a [`Event::BackgroundTaskFinished`] ended.
///
/// Deliberately two variants rather than a `Result<String, String>`:
/// the payload crosses the plugin boundary, where a typed variant
/// mirrors cleanly and a `Result` does not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskOutcome {
    /// Finished cleanly. `summary` is a short human line — the full
    /// output belongs in a log, not a notification.
    Succeeded {
        /// A short human-readable result line.
        summary: String,
    },
    /// NC.2: ended cleanly but **not done** — a rebase paused on an
    /// `edit`, a merge left uncommitted, a conflict waiting for the
    /// user. Reporting these as success says "finished" about work the
    /// user still has to finish. `message` says what is waiting.
    Stopped {
        /// What is waiting on the user.
        message: String,
    },
    /// Failed. `message` is the reason, already truncated for display.
    Failed {
        /// Why it failed, display-ready.
        message: String,
    },
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
    /// by the runtime event bus's filter dispatch to bucket
    /// subscriptions without string-matching variant names.
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
            Event::PrePluginLoaded { .. } => EventKind::PrePluginLoaded,
            Event::PluginLoaded { .. } => EventKind::PluginLoaded,
            Event::PluginUnloaded { .. } => EventKind::PluginUnloaded,
            Event::ModeEnablementRequested { .. } => EventKind::ModeEnablementRequested,
            Event::BufferOptionOverrideRequested { .. } => EventKind::BufferOptionOverrideRequested,
            Event::BackgroundTaskFinished { .. } => EventKind::BackgroundTaskFinished,
            Event::FilesChanged { .. } => EventKind::FilesChanged,
            Event::JobProgress { .. } => EventKind::JobProgress,
            Event::JobOutput { .. } => EventKind::JobOutput,
            Event::JobFinished { .. } => EventKind::JobFinished,
        }
    }
}

/// Discriminator for [`Event`] variants. Stored in
/// `EventFilter::kinds` and used by the bus to bucket
/// subscriptions per kind so publish does no global iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventKind {
    /// Discriminator for [`Event::DocumentOpened`].
    DocumentOpened,
    /// Discriminator for [`Event::DocumentClosed`].
    DocumentClosed,
    /// Discriminator for [`Event::BeforeSave`].
    BeforeSave,
    /// Discriminator for [`Event::DocumentSaved`].
    DocumentSaved,
    /// Discriminator for [`Event::DocumentChanged`].
    DocumentChanged,
    /// Discriminator for [`Event::SelectionsChanged`].
    SelectionsChanged,
    /// Discriminator for [`Event::ModalModeChanged`].
    ModalModeChanged,
    /// Discriminator for [`Event::BeforeQuit`].
    BeforeQuit,
    /// Discriminator for [`Event::OptionChanged`].
    OptionChanged,
    /// Discriminator for [`Event::MajorEntered`].
    MajorEntered,
    /// Discriminator for [`Event::MajorExiting`].
    MajorExiting,
    /// Discriminator for [`Event::MinorActivated`].
    MinorActivated,
    /// Discriminator for [`Event::MinorDeactivated`].
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
    /// Discriminator for [`Event::PrePluginLoaded`] (OA.14d) — the awaited
    /// signal an `init.rs` subscribes to for config a plugin reads at LOAD.
    PrePluginLoaded,
    /// Discriminator for [`Event::PluginLoaded`] (CI.1) — the plugin-load
    /// lifecycle signal an `init.rs` subscribes to for deferred config.
    PluginLoaded,
    /// Discriminator for [`Event::PluginUnloaded`] (CI.1).
    PluginUnloaded,
    /// Discriminator for [`Event::ModeEnablementRequested`] (CI.4) — the
    /// host-internal enable/disable-minor-mode bridge the Editor handles.
    ModeEnablementRequested,
    /// Discriminator for [`Event::BufferOptionOverrideRequested`] — like its
    /// neighbour above, host-internal and never deliverable to a guest.
    BufferOptionOverrideRequested,
    /// Discriminator for [`Event::BackgroundTaskFinished`] (MG.41g).
    BackgroundTaskFinished,
    /// Discriminator for [`Event::FilesChanged`] (OR.2) — a plugin's
    /// `host-services.watch` fired. One kind for every watch; the delivery
    /// actor, not the filter, is what scopes a batch to the plugin that armed
    /// it (see the variant's doc).
    FilesChanged,
    /// Discriminator for [`Event::JobProgress`] (LH.0). Addressed by the
    /// delivery actor, like [`Self::FilesChanged`].
    JobProgress,
    /// Discriminator for [`Event::JobOutput`] (LH.0.3).
    JobOutput,
    /// Discriminator for [`Event::JobFinished`] (LH.0).
    JobFinished,
}

/// An edit as actually applied to the buffer (the original `Edit` plus the
/// resulting range, useful for clients that want to know what changed).
///
/// The bus-level copy of `lattice_core::AppliedEdit`, without its
/// tree-sitter [`EditDelta`](crate::EditDelta). Positions are
/// (line, UTF-8 byte), as everywhere in this crate.
///
/// `inserted_text` carries the text that was placed into `inserted_range`.
/// Together with `original_range` this is exactly what an LSP
/// `textDocument/didChange` payload needs, which lets the
/// `lattice-lsp` fan-in synthesise a `lattice_protocol::edit::Edit`
/// from this event without re-reading the buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedEdit {
    /// The range the edit targeted, in pre-edit coordinates.
    pub original_range: Range,
    /// Where the inserted text now sits, in post-edit coordinates: starts at
    /// `original_range.start`; empty for a pure delete.
    pub inserted_range: Range,
    /// The text removed from `original_range` (empty for a pure insert).
    pub replaced_text: String,
    /// The text placed at `inserted_range` (empty for a pure delete).
    pub inserted_text: String,
}
