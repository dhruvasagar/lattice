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

use lsp_types::Uri;

use crate::actor::{self, ServerHandle};
use crate::config::{ServerConfig, resolve_workspace_root};
use crate::diagnostics_layer::{DiagnosticsLayer, pump_diagnostics};
use crate::error::LspResult;
use crate::logging::{LogLevel, LogSource, LspLogger};
use lattice_protocol::edit::Edit;

/// Stable key for an actor: workspace root + server id.
type ActorKey = (PathBuf, String);

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

impl std::fmt::Debug for LspSupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspSupervisor")
            .field("configs", &self.configs.len())
            .field("actors", &self.actors.len())
            .field("attached_buffers", &self.attachments.len())
            .finish_non_exhaustive()
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
