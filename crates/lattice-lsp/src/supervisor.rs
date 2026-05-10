//! `LspSupervisor` -- the per-buffer attachment manager
//! (Phase 4.1.h).
//!
//! Glues the wire-side primitives ([`ServerHandle`], [`DocSync`],
//! [`DiagnosticsBus`], [`DiagnosticsLayer`], [`LspLogger`]) into
//! one editor-facing facade. The App holds exactly one
//! `LspSupervisor`; everything else flows through it.
//!
//! ## What the supervisor owns
//!
//! - **Config registry.** A `Vec<Arc<ServerConfig>>` -- the
//!   curated builtins plus user overrides.
//! - **Per-(workspace, server-id) actors.** One spawned actor
//!   per pair. Reused across buffers in the same workspace.
//! - **Per-(workspace, server-id) DocSync.** One per actor.
//!   Tracks every URI the actor cares about.
//! - **Per-URI attachments.** Which `(workspace, server-id)`
//!   actors care about each URI. A buffer can have multiple
//!   attachments (rust-analyzer + a clippy bridge for `.rs`).
//! - **Shared logger** (cloned into every actor; cloned for
//!   subsystem-level events like supervisor decisions).
//! - **Shared diagnostics layer** (every actor's
//!   `DiagnosticsBus` is pumped into it via a tokio task
//!   spawned at actor-creation time).
//!
//! ## Why URI as the key (not `BufferId`)
//!
//! `lattice-lsp` is below the UI layer in the crate graph; it
//! has no concept of `BufferId` (which lives in
//! `lattice-ui-tui`). URIs are the LSP-native identifier and
//! map 1:1 to file paths the editor cares about. The App
//! maintains its own `BufferId → Uri` mapping and threads URIs
//! into the supervisor's API.
//!
//! ## Lifecycle
//!
//! ```text
//!   App::open_file(path, text)
//!     -> supervisor.open_buffer(path, text).await
//!         -> match_configs(path) -> [ServerConfig...]
//!         -> for each config:
//!              ensure_actor(workspace_root, &config).await
//!              ensure_doc_sync(actor)
//!              doc_sync.open(uri, language_id, text)
//!              record (workspace, server_id) in attachments[uri]
//!         -> return list of attached ServerHandles
//!
//!   App::apply_edit(uri, edit)
//!     -> supervisor.record_edit(uri, edit)
//!         -> for each attached (workspace, server_id):
//!              syncs[(workspace, server_id)].record_edit(uri, edit)
//!
//!   App::idle_flush(uri)  // 50ms after last edit
//!     -> supervisor.flush(uri)
//!         -> for each attached server: doc_sync.flush(uri)
//!
//!   App::close_buffer(uri)
//!     -> supervisor.close_buffer(uri).await
//!         -> for each attached server: doc_sync.close(uri)
//!         -> drop the URI from attachments
//!         -> diagnostics.clear_uri(uri)
//! ```
//!
//! ## Server reuse
//!
//! Two `.rs` files in the same Cargo workspace share one
//! rust-analyzer actor. Two `.rs` files in different workspaces
//! get two actors (different roots = different indexed views).
//! Two distinct languages in the same workspace get two actors
//! (different ids).
//!
//! ## What the supervisor does NOT do (yet)
//!
//! - Per-feature dispatch (hover / goto-def / references /
//!   ...). The supervisor's job is attachment and lifecycle;
//!   the App calls `servers_for(uri)` and walks the list to
//!   issue feature requests. Per-feature merging (ranking,
//!   first-non-empty, union-dedupe) lands in 4.2 alongside the
//!   nav features.
//! - Server crash recovery. Today's actor detects pipe close
//!   and resolves pending with `ActorGone`; the supervisor's
//!   restart-with-backoff logic lands in 4.4.
//! - Reading `lsp.toml` -- that's the App's responsibility
//!   (lattice-lsp has no parser dependencies). The App calls
//!   `add_config()` for each entry.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwap;
use lsp_types::Uri;
use tokio::sync::{mpsc, oneshot};

use crate::actor::{self, ServerHandle};
use crate::config::{ServerConfig, resolve_workspace_root};
use crate::diagnostics_layer::{DiagnosticsLayer, pump_diagnostics};
use crate::error::{LspError, LspResult};
use crate::logging::{LogLevel, LogSource, LspLogger};
use lattice_protocol::edit::Edit;

/// Stable key for an actor: workspace root + server id. Public
/// so [`SupervisorSnapshot`] consumers can pattern-match on the
/// actor map without re-typing the tuple.
pub type ActorKey = (PathBuf, String);

/// One LSP subsystem per editor instance.
pub struct LspSupervisor {
    /// Curated config registry. Order is preference order: when
    /// multiple configs match a path, all of them attach (we
    /// don't disambiguate by priority at attachment time --
    /// priority resolves *feature-dispatch* ties only).
    configs: Vec<Arc<ServerConfig>>,
    /// Per-(workspace, server-id) actors, lazily spawned.
    /// `ServerHandle` is itself `Arc`-wrapped internally so
    /// cloning is cheap; no need to wrap it again.
    actors: HashMap<ActorKey, ServerHandle>,
    /// Per-URI attachments: which actors are tracking this URI.
    /// Stored as ActorKey so feature dispatch can resolve
    /// `uri -> [ServerHandle, ...]` in O(handles).
    ///
    /// DocSync no longer lives here -- each actor owns its own
    /// mirror. The supervisor stays out of the edit path entirely
    /// so the UI thread cannot stall behind a flush.
    attachments: HashMap<Uri, Vec<ActorKey>>,
    /// Shared logger. Every actor gets a clone; subsystem
    /// events (supervisor decisions, attach / detach) emit
    /// directly through this.
    logger: LspLogger,
    /// Shared diagnostics layer. Every actor's
    /// `DiagnosticsBus` is pumped into this.
    diagnostics: DiagnosticsLayer,
    /// Server-initiated `workspace/applyEdit` bus (Phase 4.3).
    /// `Some` once the App has called
    /// [`Self::set_apply_edit_bus`] at startup; cloned into
    /// every actor at spawn so they all forward inbound
    /// applyEdit requests through the same channel. `None`
    /// disables the feature -- the actor falls back to a
    /// METHOD_NOT_FOUND response (matches the pre-4.3
    /// behaviour).
    apply_edit_bus: Option<crate::apply_edit::ApplyEditBus>,
    /// Server-initiated `workspace/configuration` bus (Phase
    /// 4.1 follow-up). `Some` once the App has called
    /// [`Self::set_configuration_bus`] at startup; cloned into
    /// every actor at spawn. `None` falls back to
    /// `Vec<null>`-per-item replies (the pre-this-commit
    /// stub).
    configuration_bus: Option<crate::configuration::ConfigurationBus>,
    /// Editor-wide event bus. `Some` after the App calls
    /// [`Self::set_event_bus`] (early in startup, before any
    /// buffer opens). When set, every spawned actor gets a
    /// per-actor [`crate::fan_in`] task subscribed to
    /// `DocumentChanged` events; that task forwards each
    /// applied edit straight into the actor's `cmd_tx` --
    /// keeping the UI thread out of the LSP edit path
    /// entirely.
    event_bus: Option<Arc<lattice_runtime::EventBus>>,
    /// Per-actor fan-in subscription ids. Stored alongside the
    /// actor so shutdown can call `unsubscribe` and stop the
    /// bus from holding a dead sender.
    fan_in_subs: HashMap<ActorKey, lattice_runtime::SubscriptionId>,
}

impl LspSupervisor {
    /// Construct an empty supervisor with shared logger +
    /// diagnostics layer. Use `add_config` to populate the
    /// config registry.
    pub fn new(logger: LspLogger) -> Self {
        let diagnostics = DiagnosticsLayer::new(logger.clone());
        Self {
            configs: Vec::new(),
            actors: HashMap::new(),
            attachments: HashMap::new(),
            logger,
            diagnostics,
            apply_edit_bus: None,
            configuration_bus: None,
            event_bus: None,
            fan_in_subs: HashMap::new(),
        }
    }

    /// Install the editor event bus. Must be called once at
    /// startup before any buffer opens; after the call every
    /// actor spawned by the supervisor gets a per-actor fan-in
    /// task (see [`crate::fan_in`]) that turns
    /// `Event::DocumentChanged` into [`crate::actor::ServerHandle::record_edit`]
    /// without ever taking the supervisor's mutex.
    ///
    /// Calling twice replaces the bus reference; existing actors
    /// keep their original fan-in subscriptions (we don't
    /// re-spawn them on bus swap because that would race with
    /// in-flight events).
    pub fn set_event_bus(&mut self, bus: Arc<lattice_runtime::EventBus>) {
        self.event_bus = Some(bus.clone());
        // Spawn fan-ins for any actors that pre-date the bus
        // (in practice none -- the App calls this before opening
        // files -- but the path is here for symmetry with
        // attach_handle).
        let pending: Vec<(ActorKey, ServerHandle)> = self
            .actors
            .iter()
            .filter(|(k, _)| !self.fan_in_subs.contains_key(*k))
            .map(|(k, h)| (k.clone(), h.clone()))
            .collect();
        for (key, handle) in pending {
            let id = crate::fan_in::spawn(handle, bus.clone());
            self.fan_in_subs.insert(key, id);
        }
    }

    /// Install the apply-edit bus (Phase 4.3). The App calls
    /// this once at startup with the sender side of the channel
    /// it created; every actor spawned after this point gets a
    /// clone and forwards inbound `workspace/applyEdit`
    /// requests through it. Calling twice replaces the bus;
    /// any actors already spawned keep their original clone
    /// (we don't track them for retro-fitting in v1).
    pub fn set_apply_edit_bus(&mut self, bus: crate::apply_edit::ApplyEditBus) {
        self.apply_edit_bus = Some(bus);
    }

    /// Install the configuration bus (Phase 4.1 follow-up).
    /// Same shape as [`Self::set_apply_edit_bus`]: cloned into
    /// every actor spawned after the call so server-initiated
    /// `workspace/configuration` requests reach the App's
    /// drain. `None` falls back to per-item `null` replies.
    pub fn set_configuration_bus(
        &mut self,
        bus: crate::configuration::ConfigurationBus,
    ) {
        self.configuration_bus = Some(bus);
    }

    /// Borrow the shared logger (so callers can register their
    /// own subsystem-level events).
    pub fn logger(&self) -> &LspLogger {
        &self.logger
    }

    /// Borrow the shared diagnostics layer.
    pub fn diagnostics(&self) -> &DiagnosticsLayer {
        &self.diagnostics
    }

    /// Add a server config to the registry. The App calls this
    /// for every builtin + user-override config at startup.
    pub fn add_config(&mut self, config: ServerConfig) {
        self.configs.push(Arc::new(config));
    }

    /// Set the registry from an iterator (e.g. the curated
    /// builtins). Replaces any prior contents.
    pub fn set_configs<I: IntoIterator<Item = ServerConfig>>(&mut self, configs: I) {
        self.configs = configs.into_iter().map(Arc::new).collect();
    }

    /// All registered configs (read-only; for `:lsp-status`).
    pub fn configs(&self) -> &[Arc<ServerConfig>] {
        &self.configs
    }

    /// True iff at least one configured server's `file_patterns`
    /// matches `path`. M.5.2 uses this from the App's
    /// `MajorEntered` hook to decide whether to auto-activate
    /// `lsp-mode` on the buffer; if no server cares about the
    /// path, there's nothing to gate on.
    pub fn has_server_for_path(&self, path: &Path) -> bool {
        self.configs
            .iter()
            .any(|c| matches_any_pattern(path, &c.file_patterns))
    }

    /// Every actor currently running. Used by `:lsp-status`.
    pub fn running_actors(&self) -> Vec<(ActorKey, ServerHandle)> {
        self.actors
            .iter()
            .map(|(k, h)| (k.clone(), h.clone()))
            .collect()
    }

    /// Snapshot of every running actor's `ServerHandle`. Used by
    /// workspace-scoped LSP requests (e.g. `workspace/symbol`)
    /// that fan out across every server, not just servers
    /// attached to one buffer.
    pub fn all_running_handles(&self) -> Vec<ServerHandle> {
        self.actors.values().cloned().collect()
    }

    /// Number of buffers currently attached to the actor at
    /// `key`. Cheap walk over `attachments`; used by
    /// `:lsp-server-log` to surface per-server buffer counts in
    /// the picker margin.
    pub fn buffer_count_for(&self, key: &ActorKey) -> usize {
        self.attachments
            .values()
            .filter(|keys| keys.contains(key))
            .count()
    }

    /// Open a buffer. Walks the config registry, spawns
    /// matching actors as needed, attaches the buffer to each,
    /// and emits `didOpen` per server. Returns the list of
    /// attached `ServerHandle`s -- the App stores this so
    /// feature dispatch knows where to issue requests.
    ///
    /// `path` is the buffer's filesystem path; `text` is the
    /// initial buffer text.
    pub async fn open_buffer(
        &mut self,
        path: PathBuf,
        text: String,
    ) -> LspResult<Vec<ServerHandle>> {
        let uri = crate::actor::uri_from_path(&path);
        // Already open under another path? If the URI is
        // already in attachments, surface a no-op rather than
        // re-issuing didOpen (servers reject duplicate
        // didOpens).
        if self.attachments.contains_key(&uri) {
            return Ok(self.servers_for(&uri));
        }

        let matches: Vec<Arc<ServerConfig>> = self
            .configs
            .iter()
            .filter(|c| matches_any_pattern(&path, &c.file_patterns))
            .cloned()
            .collect();

        if matches.is_empty() {
            // No server cares about this buffer; not an error.
            // App still tracks the URI; we just don't store
            // attachments for it.
            return Ok(Vec::new());
        }

        let mut handles: Vec<ServerHandle> = Vec::new();
        let mut keys: Vec<ActorKey> = Vec::new();

        for config in matches {
            let workspace = resolve_workspace_root(
                path.parent().unwrap_or(&path),
                &config.root_markers,
            );
            let key: ActorKey = (workspace.clone(), config.id.clone());
            let handle = match self.actors.get(&key) {
                Some(existing) => existing.clone(),
                None => {
                    self.logger.log(
                        None,
                        LogLevel::Info,
                        LogSource::Client,
                        format!(
                            "supervisor: spawning {} for workspace {}",
                            config.id,
                            workspace.display()
                        ),
                    );
                    let h = match actor::spawn(
                        (*config).clone(),
                        workspace.clone(),
                        self.logger.clone(),
                        self.apply_edit_bus.clone(),
                        self.configuration_bus.clone(),
                    )
                    .await
                    {
                        Ok(h) => h,
                        Err(e) => {
                            self.logger.log(
                                None,
                                LogLevel::Warn,
                                LogSource::Client,
                                format!(
                                    "supervisor: spawn failed for {} ({}): {}",
                                    config.id,
                                    workspace.display(),
                                    e
                                ),
                            );
                            // Skip this server; continue with
                            // others. Server-not-on-PATH is the
                            // common case and shouldn't sink the
                            // open.
                            continue;
                        }
                    };
                    // Spawn the diagnostics pump.
                    let rx = h.subscribe_diagnostics();
                    let layer = self.diagnostics.clone();
                    tokio::spawn(pump_diagnostics(layer, rx));
                    self.actors.insert(key.clone(), h.clone());
                    // Spawn the per-actor edit fan-in if the
                    // editor's event bus is wired up. Stored
                    // SubscriptionId is unsubscribed on shutdown
                    // so the bus doesn't keep dead senders.
                    if let Some(bus) = self.event_bus.clone() {
                        let sub = crate::fan_in::spawn(h.clone(), bus);
                        self.fan_in_subs.insert(key.clone(), sub);
                    }
                    h
                }
            };

            // Drive the DocSync inside the actor: single writer
            // means no contention with the edit / flush path.
            handle.open_doc(
                uri.clone(),
                config.language_id.clone(),
                text.clone(),
            )?;

            handles.push(handle);
            keys.push(key);
        }

        if !keys.is_empty() {
            self.attachments.insert(uri.clone(), keys);
            self.logger.log(
                None,
                LogLevel::Info,
                LogSource::Client,
                format!(
                    "supervisor: opened {} attached to {} server(s)",
                    uri.as_str(),
                    handles.len()
                ),
            );
        }
        Ok(handles)
    }

    /// Attach a buffer to a pre-built `ServerHandle`. Used by:
    ///
    /// - **Tests** -- the in-process `MockServer` returns a
    ///   handle that the supervisor wouldn't normally spawn.
    /// - **Custom transports** (future) -- a TCP / named-pipe
    ///   server that bypasses `ChildTransport`.
    ///
    /// Identical effect to the actor-spawning branch of
    /// `open_buffer`: registers the actor under
    /// `(workspace_root, server_id)`, builds a `DocSync`,
    /// emits `didOpen`, records the attachment, and starts the
    /// diagnostics pump if it isn't already running.
    pub fn attach_handle(
        &mut self,
        uri: Uri,
        workspace_root: PathBuf,
        server_id: String,
        language_id: String,
        text: String,
        handle: ServerHandle,
    ) -> LspResult<()> {
        let key: ActorKey = (workspace_root, server_id);
        // Insert / reuse the actor.
        let was_new = !self.actors.contains_key(&key);
        if was_new {
            self.actors.insert(key.clone(), handle.clone());
            // Spawn the diagnostics pump for the new actor.
            let rx = handle.subscribe_diagnostics();
            let layer = self.diagnostics.clone();
            tokio::spawn(pump_diagnostics(layer, rx));
            // Spawn the per-actor edit fan-in if the bus is wired.
            if let Some(bus) = self.event_bus.clone() {
                let sub = crate::fan_in::spawn(handle.clone(), bus);
                self.fan_in_subs.insert(key.clone(), sub);
            }
        }
        // Open inside the actor; single-writer DocSync.
        handle.open_doc(uri.clone(), language_id, text)?;
        // Record attachment.
        self.attachments.entry(uri).or_default().push(key);
        Ok(())
    }

    /// Close a buffer. Flushes pending changes per attached
    /// server, sends `didClose`, and drops the URI from
    /// attachments + the diagnostics layer.
    pub fn close_buffer(&mut self, uri: &Uri) -> LspResult<()> {
        let keys = match self.attachments.remove(uri) {
            Some(k) => k,
            None => return Ok(()),
        };
        for key in &keys {
            // The actor owns the DocSync, so it pairs the final
            // flush + didClose internally and sends them in
            // order.
            let Some(handle) = self.actors.get(key) else {
                continue;
            };
            let _ = handle.close_doc(uri.clone());
        }
        self.diagnostics.clear_uri(uri);
        self.logger.log(
            None,
            LogLevel::Info,
            LogSource::Client,
            format!("supervisor: closed {}", uri.as_str()),
        );
        Ok(())
    }

    /// Forward an edit to every attached actor's mailbox.
    ///
    /// **Not on the editor's hot path.** Production edits flow
    /// through the editor event bus to a per-actor fan-in (see
    /// [`crate::fan_in`]), which means the UI thread never
    /// takes the supervisor mutex on a keystroke. This method
    /// remains for tests + admin tooling that drive the
    /// supervisor directly without standing up an event bus.
    pub fn record_edit(&mut self, uri: &Uri, edit: &Edit) -> LspResult<()> {
        let keys = match self.attachments.get(uri) {
            Some(k) => k.clone(),
            None => return Ok(()),
        };
        for key in &keys {
            let Some(handle) = self.actors.get(key) else {
                continue;
            };
            if let Err(e) = handle.record_edit(uri.clone(), edit.clone()) {
                self.logger.log(
                    Some(&Arc::from(key.1.as_str())),
                    LogLevel::Warn,
                    LogSource::Client,
                    format!("record_edit on {}: {}", uri.as_str(), e),
                );
            }
        }
        Ok(())
    }

    /// Force-flush queued changes for `uri` across every
    /// attached server, bypassing the per-actor debounce. Used
    /// before synchronous requests that require the server to
    /// have seen the latest text -- notably `willSaveWaitUntil`
    /// and `:lsp-flush`.
    pub fn flush(&mut self, uri: &Uri) -> LspResult<()> {
        let keys = match self.attachments.get(uri) {
            Some(k) => k.clone(),
            None => return Ok(()),
        };
        for key in &keys {
            let Some(handle) = self.actors.get(key) else {
                continue;
            };
            if let Err(e) = handle.flush(uri.clone()) {
                self.logger.log(
                    Some(&Arc::from(key.1.as_str())),
                    LogLevel::Warn,
                    LogSource::Client,
                    format!("flush on {}: {}", uri.as_str(), e),
                );
            }
        }
        Ok(())
    }

    /// Force-flush every open URI's queued changes across every
    /// attached server. Used at editor shutdown so each server
    /// sees a coherent final state before `didClose`. Per-edit
    /// flushing is debounced inside each actor; this is the
    /// only "drain everything now" affordance.
    pub fn flush_all(&mut self) -> LspResult<()> {
        for (key, handle) in &self.actors {
            if let Err(e) = handle.flush_all() {
                self.logger.log(
                    Some(&Arc::from(key.1.as_str())),
                    LogLevel::Warn,
                    LogSource::Client,
                    format!("flush_all: {}", e),
                );
            }
        }
        Ok(())
    }

    /// Every server attached to `uri`. The App walks this list
    /// to dispatch features (hover, goto-definition, ...).
    pub fn servers_for(&self, uri: &Uri) -> Vec<ServerHandle> {
        let keys = match self.attachments.get(uri) {
            Some(k) => k,
            None => return Vec::new(),
        };
        keys.iter()
            .filter_map(|k| self.actors.get(k).cloned())
            .collect()
    }

    /// Number of currently-attached buffers.
    pub fn attached_buffer_count(&self) -> usize {
        self.attachments.len()
    }

    /// Number of currently-running actors.
    pub fn running_actor_count(&self) -> usize {
        self.actors.len()
    }

    /// Detach every buffer + drop every actor. Used at editor
    /// exit. Each attached buffer's `didClose` is fired.
    pub async fn shutdown(&mut self) -> LspResult<()> {
        // Snapshot URIs first to avoid borrow issues.
        let uris: Vec<Uri> = self.attachments.keys().cloned().collect();
        for uri in uris {
            let _ = self.close_buffer(&uri);
        }
        // Drop the per-actor fan-in subscriptions before
        // dropping the actors -- otherwise the bus would briefly
        // hold a sender pointing at an actor that is on its way
        // out.
        if let Some(bus) = self.event_bus.as_ref() {
            for (_key, sub) in self.fan_in_subs.drain() {
                bus.unsubscribe(sub);
            }
        } else {
            self.fan_in_subs.clear();
        }
        // Then shut down each actor gracefully.
        let actors: Vec<ServerHandle> = self.actors.values().cloned().collect();
        self.actors.clear();
        for handle in actors {
            let _ = handle.shutdown().await;
        }
        self.logger.log(
            None,
            LogLevel::Info,
            LogSource::Client,
            "supervisor: shutdown complete",
        );
        Ok(())
    }
}

impl LspSupervisor {
    /// Snapshot the current attachment + actor state for the
    /// handle's `ArcSwap`. Pre-resolves attachments to
    /// `Vec<ServerHandle>` so `LspSupervisorHandle::servers_for`
    /// is one map lookup + one Vec clone -- no second walk.
    pub(crate) fn build_snapshot(&self) -> SupervisorSnapshot {
        let attachments = self
            .attachments
            .iter()
            .map(|(uri, keys)| {
                let handles = keys
                    .iter()
                    .filter_map(|k| self.actors.get(k).cloned())
                    .collect::<Vec<_>>();
                (uri.clone(), handles)
            })
            .collect();
        SupervisorSnapshot {
            configs: self.configs.clone(),
            actors: self.actors.clone(),
            attachments,
        }
    }

    /// Consume the configured supervisor and start the supervisor
    /// task on `runtime_handle`. Returns the
    /// [`LspSupervisorHandle`] App-side code uses for the rest of
    /// the editor's lifetime.
    ///
    /// After this call, all mutating operations route through the
    /// returned handle's mailbox; reads come from the wait-free
    /// `ArcSwap<SupervisorSnapshot>`. The `LspSupervisor` itself
    /// is owned exclusively by the spawned task -- no `Arc`, no
    /// `Mutex`, no contention possible with the UI thread.
    ///
    /// Configuration calls (`set_event_bus`, `set_apply_edit_bus`,
    /// `set_configuration_bus`, `add_config`, `set_configs`) must
    /// happen *before* `spawn` -- the post-spawn handle does not
    /// expose them. This matches the editor's actual lifecycle:
    /// the App configures the supervisor at startup before any
    /// buffer opens.
    ///
    /// `runtime_handle` is mandatory: the supervisor's command-
    /// mailbox semantics only make sense against a live tokio
    /// task, and a missing runtime is a programming error.
    /// Production callers (`App::new` →
    /// `build_lsp_subsystem`) pass the editor's shared LSP
    /// runtime handle (`runtime::lsp_runtime()`); tests that
    /// exercise the write path build an ad-hoc runtime via
    /// `tokio::runtime::Builder` and pass its handle. Earlier
    /// revisions used `tokio::runtime::Handle::try_current()`
    /// with a silent fallback that dropped `cmd_rx`; that path
    /// surfaced as `LspError::ActorGone` on every write whenever
    /// the caller didn't happen to be inside a tokio context,
    /// which proved to be a real footgun (e.g. `App::new` runs
    /// before `runtime::run` has entered any context).
    pub fn spawn(self, runtime_handle: &tokio::runtime::Handle) -> LspSupervisorHandle {
        let initial_snapshot = Arc::new(self.build_snapshot());
        let snapshot_cell = Arc::new(ArcSwap::from(initial_snapshot));
        let diagnostics = self.diagnostics.clone();
        let logger = self.logger.clone();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SupervisorCmd>();
        let snapshot_for_task = snapshot_cell.clone();
        runtime_handle.spawn(supervisor_main(self, cmd_rx, snapshot_for_task));
        LspSupervisorHandle {
            snapshot: snapshot_cell,
            cmd_tx,
            diagnostics,
            logger,
        }
    }
}

impl std::fmt::Debug for LspSupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspSupervisor")
            .field("configs", &self.configs.len())
            .field("actors", &self.actors.len())
            .field("attached_buffers", &self.attachments.len())
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------
// Snapshot + handle (the public, post-spawn API).
//
// The audit (docs/dev/architecture/lsp-architecture.md §11 + post-fix audit) found
// that 14+ App methods called `try_lock` on
// `Arc<tokio::sync::Mutex<LspSupervisor>>` from the UI thread
// (modeline render, Insert-mode trigger probes, etc.) and silently
// dropped work whenever an async path -- typically `:e <path>`
// holding the mutex across the LSP `initialize` handshake -- was
// in flight. Same class of bug as the pre-fan-in LSP-edit drop.
//
// Resolution: split the supervisor's surface in two.
//
//   - `SupervisorSnapshot` carries everything readers need
//     (configs, actors, pre-resolved per-URI attachments). It
//     lives in an `ArcSwap` cell; readers do one wait-free
//     `load_full()` and clone what they need from the returned
//     `Arc`.
//   - `LspSupervisorHandle` is what the App holds. Reads return
//     directly from the snapshot (lock-free); writes send a
//     typed `SupervisorCmd` on the task's mailbox.
//   - The supervisor task owns `LspSupervisor` exclusively; it
//     processes cmds, mutates state, then publishes a fresh
//     snapshot via `ArcSwap::store`. No shared mutex anywhere.
//
// This makes "UI thread takes no LSP-related lock" a structural
// guarantee, not a discipline -- you can't write the contended
// pattern even by accident.
// ---------------------------------------------------------------

/// Wait-free read view of supervisor state.
///
/// Built by [`LspSupervisor::build_snapshot`] after every cmd
/// that mutates state; held in [`LspSupervisorHandle::snapshot`]
/// inside an `ArcSwap` cell. Readers `load_full()` and clone
/// per-field as needed. All fields are public so callers can
/// project freely without growing the handle's API.
#[derive(Debug, Clone, Default)]
pub struct SupervisorSnapshot {
    /// Curated config registry. Order is preference order: when
    /// multiple configs match a path, all of them attach.
    pub configs: Vec<Arc<ServerConfig>>,
    /// Per-(workspace, server-id) live actor handles. Cloning
    /// is cheap (`ServerHandle` is `Arc<HandleInner>` internally).
    pub actors: HashMap<ActorKey, ServerHandle>,
    /// Per-URI attachments, pre-resolved to `ServerHandle`s so
    /// `servers_for(uri)` is one map lookup + one small clone.
    pub attachments: HashMap<Uri, Vec<ServerHandle>>,
}

/// Commands the [`LspSupervisorHandle`] sends to the supervisor
/// task. Every state-mutating operation flows through here so the
/// task is the single writer to `LspSupervisor` state.
enum SupervisorCmd {
    /// Open a buffer + spawn / reuse matching actors.
    OpenBuffer {
        path: PathBuf,
        text: String,
        reply: oneshot::Sender<LspResult<Vec<ServerHandle>>>,
    },
    /// Attach a buffer to a pre-built [`ServerHandle`] (tests +
    /// custom transports).
    AttachHandle {
        uri: Uri,
        workspace_root: PathBuf,
        server_id: String,
        language_id: String,
        text: String,
        handle: ServerHandle,
        reply: oneshot::Sender<LspResult<()>>,
    },
    /// Detach a buffer + emit `didClose` per server. Reply
    /// resolves once the supervisor has processed (used by tests
    /// that need ordering).
    CloseBuffer {
        uri: Uri,
        reply: oneshot::Sender<LspResult<()>>,
    },
    /// Force-flush queued changes for `uri`.
    Flush { uri: Uri },
    /// Force-flush every URI; reply resolves after all flushes
    /// have been issued (used by `shutdown`).
    FlushAll { reply: oneshot::Sender<()> },
    /// Drive a single edit through the per-actor mailbox. Used by
    /// tests + admin tooling; production edits flow through the
    /// event-bus fan-in (see `lattice_lsp::fan_in`).
    RecordEdit { uri: Uri, edit: Edit },
    /// Editor exit: close every buffer, drop fan-ins, shut down
    /// every actor. Reply resolves after the supervisor task
    /// itself is exiting (next iteration drops `cmd_rx`).
    Shutdown {
        reply: oneshot::Sender<LspResult<()>>,
    },
}

/// Editor-facing handle to the LSP subsystem. Cheap to clone
/// (every field is `Arc`-shaped internally); the App holds one
/// instance and shares it with helpers that need to read or
/// mutate supervisor state.
///
/// **Reads are wait-free.** [`Self::servers_for`],
/// [`Self::running_actors`], [`Self::configs`], and the count
/// helpers all go through `ArcSwap::load`. The UI thread can
/// call them on the keystroke / render path without ever
/// blocking.
///
/// **Writes are mailbox-routed.** [`Self::open_buffer`],
/// [`Self::attach_handle`], [`Self::shutdown`], and
/// [`Self::flush_all`] are async (await processing); the
/// fire-and-forget variants ([`Self::close_buffer`],
/// [`Self::flush`], [`Self::record_edit`]) send the cmd and
/// return immediately, with the effect observable on the next
/// snapshot publish.
#[derive(Clone)]
pub struct LspSupervisorHandle {
    snapshot: Arc<ArcSwap<SupervisorSnapshot>>,
    cmd_tx: mpsc::UnboundedSender<SupervisorCmd>,
    diagnostics: DiagnosticsLayer,
    logger: LspLogger,
}

impl LspSupervisorHandle {
    // ----- read API (wait-free) --------------------------------

    /// One wait-free `ArcSwap::load_full` returning the current
    /// snapshot. Callers that need many fields project from the
    /// returned `Arc`; one-off readers should prefer the
    /// projection helpers below to avoid retaining the snapshot
    /// across awaits.
    pub fn snapshot(&self) -> Arc<SupervisorSnapshot> {
        self.snapshot.load_full()
    }

    /// Every server attached to `uri`. Empty when the URI is not
    /// open or no config matched its path. Cheap clone.
    pub fn servers_for(&self, uri: &Uri) -> Vec<ServerHandle> {
        self.snapshot
            .load()
            .attachments
            .get(uri)
            .cloned()
            .unwrap_or_default()
    }

    /// Snapshot of every running actor's `(key, handle)` pair.
    pub fn running_actors(&self) -> Vec<(ActorKey, ServerHandle)> {
        self.snapshot
            .load()
            .actors
            .iter()
            .map(|(k, h)| (k.clone(), h.clone()))
            .collect()
    }

    /// Snapshot of every running actor's `ServerHandle`. Used by
    /// workspace-scoped LSP requests (e.g. `workspace/symbol`)
    /// that fan out across every server, not just servers
    /// attached to one buffer.
    pub fn all_running_handles(&self) -> Vec<ServerHandle> {
        self.snapshot.load().actors.values().cloned().collect()
    }

    /// Number of buffers currently attached to the actor at `key`.
    /// Used by `:lsp-server-log` for the picker margin.
    pub fn buffer_count_for(&self, key: &ActorKey) -> usize {
        let snap = self.snapshot.load();
        let handle = match snap.actors.get(key) {
            Some(h) => h,
            None => return 0,
        };
        snap.attachments
            .values()
            .filter(|handles| {
                handles.iter().any(|h| h.server_id() == handle.server_id())
            })
            .count()
    }

    /// Curated config registry (preference-ordered). Cheap clone.
    pub fn configs(&self) -> Vec<Arc<ServerConfig>> {
        self.snapshot.load().configs.clone()
    }

    /// True iff at least one configured server's `file_patterns`
    /// matches `path`. M.5.2 uses this from the App's
    /// `MajorEntered` hook to decide whether to auto-activate
    /// `lsp-mode` on the buffer.
    pub fn has_server_for_path(&self, path: &Path) -> bool {
        self.snapshot
            .load()
            .configs
            .iter()
            .any(|c| matches_any_pattern(path, &c.file_patterns))
    }

    /// Number of currently-attached buffers.
    pub fn attached_buffer_count(&self) -> usize {
        self.snapshot.load().attachments.len()
    }

    /// Number of currently-running actors.
    pub fn running_actor_count(&self) -> usize {
        self.snapshot.load().actors.len()
    }

    /// Borrow the shared logger. Cloning is cheap (`LspLogger`
    /// wraps an `Arc` internally) but most callers just want a
    /// borrow to call `.log(...)`.
    pub fn logger(&self) -> &LspLogger {
        &self.logger
    }

    /// Borrow the shared diagnostics layer.
    pub fn diagnostics(&self) -> &DiagnosticsLayer {
        &self.diagnostics
    }

    // ----- write API (async; await mailbox processing) ---------

    /// Open a buffer. Returns the list of attached
    /// `ServerHandle`s; empty if no config matched the path.
    /// May spawn new actors (each one pays the LSP `initialize`
    /// handshake cost). The supervisor task runs the open;
    /// awaiting here parks the caller while it does, but does
    /// NOT block any *other* read of the supervisor.
    pub async fn open_buffer(
        &self,
        path: PathBuf,
        text: String,
    ) -> LspResult<Vec<ServerHandle>> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCmd::OpenBuffer { path, text, reply })
            .map_err(|_| LspError::ActorGone)?;
        rx.await.map_err(|_| LspError::ActorGone)?
    }

    /// Attach a buffer to a pre-built handle (tests + custom
    /// transports).
    pub async fn attach_handle(
        &self,
        uri: Uri,
        workspace_root: PathBuf,
        server_id: String,
        language_id: String,
        text: String,
        handle: ServerHandle,
    ) -> LspResult<()> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCmd::AttachHandle {
                uri,
                workspace_root,
                server_id,
                language_id,
                text,
                handle,
                reply,
            })
            .map_err(|_| LspError::ActorGone)?;
        rx.await.map_err(|_| LspError::ActorGone)?
    }

    // ----- write API (fire-and-forget; effect via snapshot) ----

    /// Close a buffer. Fire-and-forget: the supervisor task
    /// processes the cmd asynchronously, fires `didClose` per
    /// attached server, and publishes a fresh snapshot. Tests
    /// that need to assert post-close state should call
    /// [`Self::close_buffer_ack`].
    pub fn close_buffer(&self, uri: Uri) {
        let (reply, _rx) = oneshot::channel();
        let _ = self.cmd_tx.send(SupervisorCmd::CloseBuffer { uri, reply });
    }

    /// Close a buffer + await acknowledgement. Used by tests +
    /// shutdown paths that need to observe the next snapshot.
    pub async fn close_buffer_ack(&self, uri: Uri) -> LspResult<()> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCmd::CloseBuffer { uri, reply })
            .map_err(|_| LspError::ActorGone)?;
        rx.await.map_err(|_| LspError::ActorGone)?
    }

    /// Force-flush queued changes for `uri`. Fire-and-forget.
    pub fn flush(&self, uri: Uri) {
        let _ = self.cmd_tx.send(SupervisorCmd::Flush { uri });
    }

    /// Force-flush every URI; awaits the supervisor task's ack.
    pub async fn flush_all(&self) -> LspResult<()> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCmd::FlushAll { reply })
            .map_err(|_| LspError::ActorGone)?;
        rx.await.map_err(|_| LspError::ActorGone)?;
        Ok(())
    }

    /// Drive an edit through the supervisor (test / admin path;
    /// production edits ride the event-bus fan-in).
    pub fn record_edit(&self, uri: Uri, edit: Edit) {
        let _ = self.cmd_tx.send(SupervisorCmd::RecordEdit { uri, edit });
    }

    /// Editor exit: close every buffer, drop fan-ins, shut down
    /// every actor. Awaits the supervisor task's final ack.
    pub async fn shutdown(&self) -> LspResult<()> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCmd::Shutdown { reply })
            .map_err(|_| LspError::ActorGone)?;
        rx.await.map_err(|_| LspError::ActorGone)?
    }
}

impl std::fmt::Debug for LspSupervisorHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let snap = self.snapshot.load();
        f.debug_struct("LspSupervisorHandle")
            .field("configs", &snap.configs.len())
            .field("actors", &snap.actors.len())
            .field("attached_buffers", &snap.attachments.len())
            .finish_non_exhaustive()
    }
}

/// The supervisor task. Owns `LspSupervisor` exclusively;
/// processes `SupervisorCmd`s in FIFO order; publishes a fresh
/// snapshot via `ArcSwap::store` after every state mutation.
///
/// Exits when `cmd_rx` returns `None` (every handle dropped) or
/// after a successful `Shutdown` cmd. The mailbox is unbounded;
/// publishers never block on a full queue, and the supervisor
/// always drains in order so e.g. `OpenBuffer` -> `CloseBuffer`
/// is processed open-first regardless of timing.
async fn supervisor_main(
    mut state: LspSupervisor,
    mut cmd_rx: mpsc::UnboundedReceiver<SupervisorCmd>,
    snapshot: Arc<ArcSwap<SupervisorSnapshot>>,
) {
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            SupervisorCmd::OpenBuffer { path, text, reply } => {
                let result = state.open_buffer(path, text).await;
                snapshot.store(Arc::new(state.build_snapshot()));
                let _ = reply.send(result);
            }
            SupervisorCmd::AttachHandle {
                uri,
                workspace_root,
                server_id,
                language_id,
                text,
                handle,
                reply,
            } => {
                let result = state.attach_handle(
                    uri,
                    workspace_root,
                    server_id,
                    language_id,
                    text,
                    handle,
                );
                snapshot.store(Arc::new(state.build_snapshot()));
                let _ = reply.send(result);
            }
            SupervisorCmd::CloseBuffer { uri, reply } => {
                let result = state.close_buffer(&uri);
                snapshot.store(Arc::new(state.build_snapshot()));
                let _ = reply.send(result);
            }
            SupervisorCmd::Flush { uri } => {
                // `flush` returns Ok unless the URI is unknown;
                // either way no state changes, so no snapshot
                // republish needed.
                let _ = state.flush(&uri);
            }
            SupervisorCmd::FlushAll { reply } => {
                let _ = state.flush_all();
                let _ = reply.send(());
            }
            SupervisorCmd::RecordEdit { uri, edit } => {
                let _ = state.record_edit(&uri, &edit);
            }
            SupervisorCmd::Shutdown { reply } => {
                let result = state.shutdown().await;
                snapshot.store(Arc::new(state.build_snapshot()));
                let _ = reply.send(result);
                // Drain anything still in the queue (likely
                // empty), then exit. Closing cmd_rx here would
                // be racey -- the runtime is the one dropping
                // senders.
                break;
            }
        }
    }
}

/// Match `path` against any of `patterns`. Supports `*.<ext>`
/// (extension match) and bare basename equality (e.g.
/// `Cargo.toml`). More elaborate globs are deferred until a
/// real use case appears.
pub(crate) fn matches_any_pattern(path: &Path, patterns: &[String]) -> bool {
    patterns.iter().any(|p| matches_pattern(path, p))
}

fn matches_pattern(path: &Path, pattern: &str) -> bool {
    if let Some(ext) = pattern.strip_prefix("*.") {
        return path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(std::ffi::OsStr::new(ext)));
    }
    // Bare pattern: basename equality.
    path.file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case(std::ffi::OsStr::new(pattern)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_pattern_handles_extension_globs() {
        let path = std::path::PathBuf::from("/tmp/x.rs");
        assert!(matches_pattern(&path, "*.rs"));
        assert!(!matches_pattern(&path, "*.py"));
        // Case-insensitive on extension.
        assert!(matches_pattern(&std::path::PathBuf::from("X.RS"), "*.rs"));
    }

    #[test]
    fn matches_pattern_handles_basename_equality() {
        let path = std::path::PathBuf::from("/tmp/Cargo.toml");
        assert!(matches_pattern(&path, "Cargo.toml"));
        assert!(!matches_pattern(&path, "package.json"));
    }

    #[test]
    fn matches_any_pattern_walks_list() {
        let path = std::path::PathBuf::from("/tmp/main.go");
        let patterns = vec!["*.rs".to_string(), "*.go".to_string(), "go.mod".to_string()];
        assert!(matches_any_pattern(&path, &patterns));
    }

    #[test]
    fn supervisor_has_empty_state_at_construction() {
        let logger = LspLogger::with_defaults();
        let sup = LspSupervisor::new(logger);
        assert_eq!(sup.attached_buffer_count(), 0);
        assert_eq!(sup.running_actor_count(), 0);
        assert_eq!(sup.configs().len(), 0);
        assert_eq!(sup.diagnostics().count(), 0);
    }

    #[test]
    fn add_config_registers_in_order() {
        let mut sup = LspSupervisor::new(LspLogger::with_defaults());
        sup.add_config(ServerConfig::new("rust", "rust-analyzer", "rust"));
        sup.add_config(ServerConfig::new("python", "pyright", "python"));
        assert_eq!(sup.configs().len(), 2);
        assert_eq!(sup.configs()[0].id, "rust");
        assert_eq!(sup.configs()[1].id, "python");
    }

    #[test]
    fn set_configs_replaces_registry() {
        let mut sup = LspSupervisor::new(LspLogger::with_defaults());
        sup.add_config(ServerConfig::new("rust", "rust-analyzer", "rust"));
        sup.set_configs([ServerConfig::new("go", "gopls", "go")]);
        assert_eq!(sup.configs().len(), 1);
        assert_eq!(sup.configs()[0].id, "go");
    }
}
