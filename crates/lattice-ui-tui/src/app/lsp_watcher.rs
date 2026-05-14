//! 4.4.l.2 -- App-side file-watcher service backing
//! `workspace/didChangeWatchedFiles`.
//!
//! The pure piece (parsing dynamic-registration entries +
//! compiling globs + matching paths) lives in
//! `lattice-lsp::file_watcher`. This module owns the
//! `notify::RecommendedWatcher` and the per-tick glue:
//!
//! 1. **Subscription refresh.** Walks every running actor's
//!    `Capabilities.dynamic` registry, recompiles per-server
//!    [`WatcherSubscriptions`], and uses the `fingerprint()`
//!    on each to skip work when nothing changed. Buffer
//!    attachments or `client/registerCapability` updates cause
//!    a rebuild on the next tick.
//! 2. **Watcher lifecycle.** Lazily spawns a single
//!    `RecommendedWatcher` watching the workspace root
//!    recursively the first time any server registers at
//!    least one watcher. The watcher emits fs events to a
//!    tokio mpsc that the App's main loop drains.
//! 3. **Fan-out.** Each fs event is matched against every
//!    server's [`WatcherSubscriptions`]; a server with at
//!    least one matching watcher receives a
//!    `workspace/didChangeWatchedFiles` notification with the
//!    batched `FileEvent`s.
//!
//! ## Why watch the whole root recursively
//!
//! In principle we could compute the union of declared base
//! paths and watch only those. In practice servers register
//! a small number of broad patterns (`**/*.rs`,
//! `**/Cargo.toml`, ...) all anchored to the workspace root,
//! so the union IS the root. Watching once recursively is
//! simpler and avoids subscribing / unsubscribing as servers
//! register / unregister.
//!
//! ## What still hurts
//!
//! - `target/` and `node_modules/` get watched too. inotify
//!   watches are a finite OS resource; on Linux the default
//!   limit is 8192 watches per user. This can run out on
//!   monorepos; a follow-up adds per-path ignore globs.
//! - No debounce yet; a flurry of writes from a build tool
//!   produces N notifications. Most servers handle this
//!   gracefully (they coalesce internally) but a debouncer
//!   here would be cheap insurance.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use lsp_types::{FileChangeType, FileEvent};
use notify::{
    Event as NotifyEvent, EventKind as NotifyEventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{CreateKind, ModifyKind, RemoveKind},
};

use crate::app::App;
use lattice_lsp::{LogLevel, LogSource, WatcherSubscriptions};

/// One server's subscription snapshot + the fingerprint used to
/// detect changes. Keyed in [`LspFileWatcher`] by `server_id` so
/// fan-out can route batched `FileEvent`s back to the right
/// `ServerHandle`.
struct CachedSubscription {
    fingerprint: u64,
    subs: WatcherSubscriptions,
}

/// App-side file-watcher service. One per App instance; created
/// lazily on first use so apps that never run an LSP server pay
/// no cost.
pub struct LspFileWatcher {
    /// One recursive watch per unique workspace root across
    /// running actors. Kept on the struct so it doesn't drop;
    /// dropping the watcher tears down its background thread.
    watcher: RecommendedWatcher,
    /// Set of paths the watcher is currently watching. Refresh
    /// adds new roots / unwatches removed roots based on the
    /// running-actor set.
    watched_roots: HashSet<PathBuf>,
    /// Tokio mpsc the watcher thread sends into; the App drains
    /// from the App side. `try_recv` is non-blocking so the
    /// main loop never parks here.
    rx: tokio::sync::mpsc::UnboundedReceiver<NotifyEvent>,
    /// Per-server `WatcherSubscriptions` cache. `refresh` rebuilds
    /// only entries whose fingerprint changed; unchanged servers
    /// reuse their cached compile.
    by_server: HashMap<String, CachedSubscription>,
}

impl LspFileWatcher {
    /// Create a watcher with no roots attached yet. Callers use
    /// [`Self::sync_watched_roots`] to subscribe to the union of
    /// active workspace roots; `new()` only spawns the
    /// `RecommendedWatcher` itself (cheap -- one background
    /// thread).
    ///
    /// Returns `Err` if the OS refuses to construct the watcher
    /// (inotify-create failure on Linux, FSEvents init failure
    /// on macOS). Caller surfaces failures through the LSP
    /// logger and treats the service as absent until next
    /// attempt.
    pub fn new() -> Result<Self, notify::Error> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<NotifyEvent>();
        let watcher = RecommendedWatcher::new(
            move |res: notify::Result<NotifyEvent>| {
                if let Ok(ev) = res {
                    let _ = tx.send(ev);
                }
            },
            notify::Config::default(),
        )?;
        Ok(Self {
            watcher,
            watched_roots: HashSet::new(),
            rx,
            by_server: HashMap::new(),
        })
    }

    /// Sync the watcher's subscribed roots to match `target`.
    /// Adds new ones via `watch(..., RecursiveMode::Recursive)`
    /// and unwatches paths no longer in `target`. Failures on
    /// either side log + skip; the rest of the diff still
    /// applies.
    fn sync_watched_roots(&mut self, target: &HashSet<PathBuf>, logger: &lattice_lsp::LspLogger) {
        // Unwatch paths that fell out of scope.
        let stale: Vec<PathBuf> = self
            .watched_roots
            .iter()
            .filter(|p| !target.contains(*p))
            .cloned()
            .collect();
        for p in stale {
            if let Err(e) = self.watcher.unwatch(&p) {
                logger.log(
                    None,
                    LogLevel::Warn,
                    LogSource::Client,
                    format!("file-watcher unwatch {} failed: {e}", p.display()),
                );
            }
            self.watched_roots.remove(&p);
        }
        // Add new paths.
        let new: Vec<PathBuf> = target
            .iter()
            .filter(|p| !self.watched_roots.contains(*p))
            .cloned()
            .collect();
        for p in new {
            match self.watcher.watch(&p, RecursiveMode::Recursive) {
                Ok(()) => {
                    self.watched_roots.insert(p);
                }
                Err(e) => {
                    logger.log(
                        None,
                        LogLevel::Warn,
                        LogSource::Client,
                        format!("file-watcher watch {} failed: {e}", p.display()),
                    );
                }
            }
        }
    }

    /// Borrow the set of paths the watcher is currently
    /// subscribed to. Used by tests; production code goes
    /// through [`Self::sync_watched_roots`].
    pub fn watched_roots(&self) -> &HashSet<PathBuf> {
        &self.watched_roots
    }

    /// Drain queued fs events into a vec. Used by tests; the
    /// production drain (`App::drain_lsp_fs_events`) consumes the
    /// same path inline.
    fn drain_pending(&mut self) -> Vec<NotifyEvent> {
        let mut out: Vec<NotifyEvent> = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }
}

/// Translate one `notify::Event` into an LSP `FileChangeType` per
/// path. Notify lumps a wide range of fs ops into broad kinds;
/// we collapse those into the spec's three-value enum. Returns
/// `None` for kinds with no LSP analogue (e.g. metadata-only
/// chmods) so the dispatcher can short-circuit before glob
/// matching.
fn classify(event: &NotifyEvent) -> Option<FileChangeType> {
    match event.kind {
        NotifyEventKind::Create(CreateKind::File)
        | NotifyEventKind::Create(CreateKind::Folder)
        | NotifyEventKind::Create(CreateKind::Any)
        | NotifyEventKind::Create(CreateKind::Other) => Some(FileChangeType::CREATED),
        NotifyEventKind::Modify(ModifyKind::Data(_))
        | NotifyEventKind::Modify(ModifyKind::Metadata(_))
        | NotifyEventKind::Modify(ModifyKind::Any)
        | NotifyEventKind::Modify(ModifyKind::Other) => Some(FileChangeType::CHANGED),
        // A rename surfaces as a Modify(Name(_)); for LSP
        // purposes a rename's net effect from / to are best
        // expressed as a deletion of the old + a creation of
        // the new. The notify event records the `to` path only
        // in the `RenameMode::Both` variant; otherwise we get
        // one event per side. Treating any name-mode modify as
        // "changed" is the safe default that won't lose
        // information -- the server can re-parse and resolve.
        NotifyEventKind::Modify(ModifyKind::Name(_)) => Some(FileChangeType::CHANGED),
        NotifyEventKind::Remove(RemoveKind::File)
        | NotifyEventKind::Remove(RemoveKind::Folder)
        | NotifyEventKind::Remove(RemoveKind::Any)
        | NotifyEventKind::Remove(RemoveKind::Other) => Some(FileChangeType::DELETED),
        // Access events (read, open, close) don't map to any
        // LSP file-change type. Likewise `Any` and `Other`
        // catch-alls at the event level (rather than per-kind)
        // get dropped.
        _ => None,
    }
}

impl App {
    /// 4.4.l.2: ensure the file-watcher service is alive and its
    /// per-server subscription cache reflects the current
    /// dynamic registry. Idempotent + cheap when nothing has
    /// changed (fingerprint hit short-circuits per server).
    ///
    /// Called from the main loop's pre-render tick. The first
    /// call after any server registers at least one
    /// `workspace/didChangeWatchedFiles` watcher spawns the
    /// underlying `notify::RecommendedWatcher`; subsequent
    /// changes just refresh the per-server snapshots and
    /// subscribed paths.
    pub fn refresh_lsp_file_watcher(&mut self) {
        // (workspace_root, server_id, ServerHandle) triples for
        // every running actor; `running_actors` returns
        // `(ActorKey, ServerHandle)` where ActorKey is
        // (PathBuf, String).
        let actors = self.lsp.running_actors();
        let actors_with_watchers: Vec<_> = actors
            .into_iter()
            .filter(|(_, h)| {
                h.capabilities()
                    .dynamic
                    .has("workspace/didChangeWatchedFiles")
            })
            .collect();
        if actors_with_watchers.is_empty() {
            // Drop the watcher when no actor is interested.
            // OS watches are released; if a server re-registers
            // later we spawn a fresh one.
            self.lsp_file_watcher = None;
            return;
        }
        // Spawn lazily.
        if self.lsp_file_watcher.is_none() {
            match LspFileWatcher::new() {
                Ok(w) => self.lsp_file_watcher = Some(w),
                Err(e) => {
                    self.lsp_logger.log(
                        None,
                        LogLevel::Warn,
                        LogSource::Client,
                        format!("file watcher init failed: {e}"),
                    );
                    return;
                }
            }
        }
        let watcher = match self.lsp_file_watcher.as_mut() {
            Some(w) => w,
            None => return,
        };
        // Compute the union of workspace roots across actors;
        // sync the notify subscription set to match. notify
        // dedupes internally on identical paths, but we still
        // diff so unwatching is correct.
        let target_roots: HashSet<PathBuf> = actors_with_watchers
            .iter()
            .map(|((root, _), _)| root.clone())
            .collect();
        let logger = self.lsp_logger.clone();
        watcher.sync_watched_roots(&target_roots, &logger);
        // Per-server fingerprint compare + recompile. Each
        // actor's subscriptions anchor to its own workspace
        // root.
        let mut live_ids: HashSet<String> = HashSet::new();
        for ((root, _key_server_id), handle) in &actors_with_watchers {
            let server_id = handle.server_id().to_string();
            live_ids.insert(server_id.clone());
            let caps = handle.capabilities();
            let server_id_arc: Arc<str> = Arc::from(server_id.as_str());
            let subs = lattice_lsp::compile_with_workspace_root(&caps, server_id_arc, root);
            let fp = subs.fingerprint();
            let entry = watcher.by_server.entry(server_id);
            match entry {
                std::collections::hash_map::Entry::Occupied(mut o) => {
                    if o.get().fingerprint != fp {
                        o.insert(CachedSubscription {
                            fingerprint: fp,
                            subs,
                        });
                    }
                }
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(CachedSubscription {
                        fingerprint: fp,
                        subs,
                    });
                }
            }
        }
        // Evict cached entries for actors that have gone away.
        watcher.by_server.retain(|id, _| live_ids.contains(id));
    }

    /// 4.4.l.2: drain queued fs events, match each against every
    /// server's subscription set, fan out per-server
    /// `workspace/didChangeWatchedFiles` notifications.
    ///
    /// Per-tick batching: every event consumed this tick lands
    /// in one notification per server (the LSP spec allows
    /// multiple `FileEvent`s in a payload). Servers with zero
    /// matching events don't receive a notification at all.
    pub fn drain_lsp_fs_events(&mut self) {
        let Some(watcher) = self.lsp_file_watcher.as_mut() else {
            return;
        };
        // Pull every queued event. notify's mpsc is unbounded so
        // we drain to empty per tick; the per-server match cost
        // is O(globs) per event, so even a burst is cheap.
        let events = watcher.drain_pending();
        if events.is_empty() {
            return;
        }
        // Pre-flatten into (path, change) tuples. notify events
        // can list multiple paths (e.g. rename from/to); each
        // path produces its own LSP `FileEvent`.
        let mut classified: Vec<(PathBuf, FileChangeType)> = Vec::new();
        for ev in &events {
            let Some(kind) = classify(ev) else { continue };
            for p in &ev.paths {
                classified.push((p.clone(), kind));
            }
        }
        if classified.is_empty() {
            return;
        }
        // Bucket by server. Walk every cached subscription and
        // collect matched events; servers with no hits get no
        // notification.
        let mut per_server: HashMap<String, Vec<FileEvent>> = HashMap::new();
        for (server_id, cached) in &watcher.by_server {
            if cached.subs.is_empty() {
                continue;
            }
            let mut batch: Vec<FileEvent> = Vec::new();
            for (path, change) in &classified {
                let hits = cached.subs.matches(path, *change);
                if hits.is_empty() {
                    continue;
                }
                let uri = lattice_lsp::actor::uri_from_path(path);
                batch.push(FileEvent::new(uri, *change));
            }
            if !batch.is_empty() {
                per_server.insert(server_id.clone(), batch);
            }
        }
        if per_server.is_empty() {
            return;
        }
        // Fan out: one notification per server.
        for (_key, handle) in self.lsp.running_actors() {
            let server_id = handle.server_id().to_string();
            let Some(batch) = per_server.remove(&server_id) else {
                continue;
            };
            let params = lsp_types::DidChangeWatchedFilesParams { changes: batch };
            if let Err(e) = handle.did_change_watched_files(params) {
                let instance = handle.instance();
                self.lsp_logger.log(
                    Some(&instance),
                    LogLevel::Warn,
                    LogSource::Client,
                    format!("workspace/didChangeWatchedFiles fan-out failed: {e}"),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Notify's `EventKind::Create(_)` flavours map to LSP
    /// `CREATED`; `Modify(_)` to `CHANGED`; `Remove(_)` to
    /// `DELETED`. Access events drop out.
    #[test]
    fn classify_translates_notify_kinds_to_lsp_kinds() {
        use notify::event::{AccessKind, AccessMode};
        let mk = |kind: NotifyEventKind| NotifyEvent {
            kind,
            paths: Vec::new(),
            attrs: notify::event::EventAttributes::new(),
        };
        assert_eq!(
            classify(&mk(NotifyEventKind::Create(CreateKind::File))),
            Some(FileChangeType::CREATED),
        );
        assert_eq!(
            classify(&mk(NotifyEventKind::Modify(ModifyKind::Data(
                notify::event::DataChange::Content
            )))),
            Some(FileChangeType::CHANGED),
        );
        assert_eq!(
            classify(&mk(NotifyEventKind::Remove(RemoveKind::File))),
            Some(FileChangeType::DELETED),
        );
        // Access events: no LSP analogue.
        assert_eq!(
            classify(&mk(NotifyEventKind::Access(AccessKind::Open(
                AccessMode::Read
            )))),
            None,
        );
    }
}
