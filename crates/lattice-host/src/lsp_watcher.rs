//! LSP file-watcher subsystem (Phase 5.8.AF.5 / Slice 1).
//!
//! Owns a `notify::RecommendedWatcher` and the per-server LSP
//! subscription map. Lives entirely on a dedicated tokio task on
//! the LSP runtime — paramount goal #4 (CLAUDE.md): nothing that
//! does I/O, classification, or LSP fan-out runs on the renderer's
//! per-tick loop.
//!
//! ## Shape
//!
//! - `LspFileWatcherHandle` — what `Editor` holds. Just a
//!   `cmd_tx`. Cheap to construct, cheap to send through, never
//!   blocks.
//! - `spawn_lsp_file_watcher_task` — spawns the task on
//!   `lsp_runtime`. The task owns the watcher, the event rx, and
//!   a `HashMap<server_id, CachedSubscription>`. It `select!`s
//!   between commands from `Editor` and `notify` events.
//! - Editor pushes a `SyncSubscriptions` command whenever the
//!   actor roster or per-server caps change. The task installs/
//!   tears down recursive watches and replaces its in-memory
//!   subscription map atomically.
//! - When notify fires an event, the task classifies it, matches
//!   it against every server's `WatcherSubscriptions`, and fans
//!   out one `workspace/didChangeWatchedFiles` per interested
//!   server via the cloned `LspSupervisorHandle`. The supervisor
//!   handle's `did_change_watched_files` notification is
//!   non-blocking (channel send on the per-server actor).
//!
//! Constructing the `notify` watcher and calling `watcher.watch`/
//! `watcher.unwatch` are sync APIs; they're invoked from inside
//! the task via [`tokio::task::block_in_place`] so the LSP
//! runtime (multi-thread) keeps polling other futures while a
//! large recursive walk is in flight.
//!
//! ## What's NOT here (deferred to Slice 2)
//!
//! - `.gitignore` / `.ignore` aware filtering at the notify
//!   callback so events for `target/`, `.git/`, `node_modules/`,
//!   etc. never enter the channel.
//! - Per-server registered-glob filtering at the source so we
//!   don't pay the inotify wakeup cost for irrelevant paths.
//!
//! Both are pure-additive improvements that bolt onto this
//! task's existing structure.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use lattice_lsp::lsp_types::FileChangeType;
use notify::{
    Event as NotifyEvent, EventKind as NotifyEventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{CreateKind, ModifyKind, RemoveKind},
};
use tokio::sync::mpsc;

use lattice_lsp::WatcherSubscriptions;

/// One server's subscription snapshot + the fingerprint used to
/// detect changes. `Editor` computes the fingerprint, stores its
/// own `server_id → fingerprint` map, and only sends a
/// [`WatcherCommand::SyncSubscriptions`] when at least one server's
/// fingerprint flipped (or the actor roster changed).
#[derive(Clone)]
pub struct CachedSubscription {
    pub fingerprint: u64,
    pub subs: WatcherSubscriptions,
}

impl std::fmt::Debug for CachedSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedSubscription")
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

/// Editor → watcher-task control commands.
pub enum WatcherCommand {
    /// Replace the watched-root set + per-server subscription map
    /// atomically. The task diffs the roots against its current
    /// set, installs/tears down notify watches, and swaps the
    /// subscription map.
    SyncSubscriptions {
        target_roots: HashSet<PathBuf>,
        subscriptions: HashMap<String, CachedSubscription>,
    },
    /// Tear down everything and exit the task loop. Sent on
    /// editor shutdown; the task also exits when `cmd_rx` is
    /// closed (handle dropped).
    Shutdown,
}

/// Handle held by `Editor`. Owning this is cheap; sending through
/// it is non-blocking. Drop the handle to tear down the task
/// (the watcher itself drops + every inotify watch is released).
pub struct LspFileWatcherHandle {
    cmd_tx: mpsc::UnboundedSender<WatcherCommand>,
}

impl std::fmt::Debug for LspFileWatcherHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspFileWatcherHandle")
            .finish_non_exhaustive()
    }
}

impl LspFileWatcherHandle {
    /// Send a new target-root + subscription snapshot to the
    /// task. Best-effort: a dropped task (shouldn't happen in
    /// production) silently no-ops.
    pub fn sync(
        &self,
        target_roots: HashSet<PathBuf>,
        subscriptions: HashMap<String, CachedSubscription>,
    ) {
        let _ = self.cmd_tx.send(WatcherCommand::SyncSubscriptions {
            target_roots,
            subscriptions,
        });
    }

    /// Tell the task to exit. Idempotent.
    pub fn shutdown(&self) {
        let _ = self.cmd_tx.send(WatcherCommand::Shutdown);
    }
}

/// Spawn the watcher task on the LSP runtime. Returns the handle
/// `Editor` should keep. Returns `Err` only if `notify` itself
/// fails to construct the OS watcher (out of inotify slots, etc.).
pub fn spawn_lsp_file_watcher_task(
    supervisor: lattice_lsp::LspSupervisorHandle,
    logger: lattice_lsp::LspLogger,
) -> Result<LspFileWatcherHandle, notify::Error> {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<WatcherCommand>();
    let (event_tx, event_rx) = mpsc::unbounded_channel::<NotifyEvent>();
    // Build the OS watcher up front. Its callback is invoked from
    // notify's own worker thread; we just fan the event into our
    // tokio channel.
    let watcher = RecommendedWatcher::new(
        move |res: notify::Result<NotifyEvent>| {
            if let Ok(ev) = res {
                let _ = event_tx.send(ev);
            }
        },
        notify::Config::default(),
    )?;

    lattice_runtime::runtime::spawn_on_lsp_runtime(async move {
        let mut state = WatcherTaskState {
            watcher,
            watched_roots: HashSet::new(),
            by_server: HashMap::new(),
            supervisor,
            logger,
        };
        state.run(cmd_rx, event_rx).await;
    });

    Ok(LspFileWatcherHandle { cmd_tx })
}

/// In-task state. Owned exclusively by the spawned task — no
/// locks, no shared mutability. Editor talks to it only through
/// [`WatcherCommand`]s.
struct WatcherTaskState {
    watcher: RecommendedWatcher,
    watched_roots: HashSet<PathBuf>,
    by_server: HashMap<String, CachedSubscription>,
    supervisor: lattice_lsp::LspSupervisorHandle,
    logger: lattice_lsp::LspLogger,
}

impl WatcherTaskState {
    async fn run(
        &mut self,
        mut cmd_rx: mpsc::UnboundedReceiver<WatcherCommand>,
        mut event_rx: mpsc::UnboundedReceiver<NotifyEvent>,
    ) {
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => match cmd {
                    Some(WatcherCommand::SyncSubscriptions { target_roots, subscriptions }) => {
                        self.sync_roots(&target_roots);
                        self.by_server = subscriptions;
                    }
                    Some(WatcherCommand::Shutdown) | None => break,
                },
                ev = event_rx.recv() => match ev {
                    Some(event) => self.dispatch_event(event),
                    None => break,
                },
            }
        }
    }

    /// Diff `target` against `self.watched_roots`. Add inotify
    /// watches for new ones, remove for stale. Both `notify`
    /// calls are sync; wrap in `block_in_place` so a large
    /// recursive walk on a giant workspace doesn't stall the LSP
    /// runtime's reactor for other tasks.
    fn sync_roots(&mut self, target: &HashSet<PathBuf>) {
        let stale: Vec<PathBuf> = self
            .watched_roots
            .iter()
            .filter(|p| !target.contains(*p))
            .cloned()
            .collect();
        for p in stale {
            tracing::info!(path = %p.display(), "lsp_watcher: unwatching stale root");
            let _ = tokio::task::block_in_place(|| self.watcher.unwatch(&p));
            self.watched_roots.remove(&p);
        }
        let new: Vec<PathBuf> = target
            .iter()
            .filter(|p| !self.watched_roots.contains(*p))
            .cloned()
            .collect();
        for p in new {
            // notify's `Recursive` watch walks the tree
            // synchronously on Linux. `block_in_place` flags this
            // to the multi-thread runtime so other tasks on
            // sibling workers keep making progress.
            tracing::info!(
                path = %p.display(),
                "lsp_watcher: installing recursive watch (may block on large trees)"
            );
            let started = std::time::Instant::now();
            let result = tokio::task::block_in_place(|| {
                self.watcher.watch(&p, RecursiveMode::Recursive)
            });
            match result {
                Ok(()) => {
                    tracing::info!(
                        path = %p.display(),
                        elapsed_ms = started.elapsed().as_millis(),
                        "lsp_watcher: recursive watch installed"
                    );
                    self.watched_roots.insert(p);
                }
                Err(e) => {
                    tracing::info!(
                        path = %p.display(),
                        elapsed_ms = started.elapsed().as_millis(),
                        error = %e,
                        "lsp_watcher: recursive watch failed"
                    );
                    self.logger.log(
                        None,
                        lattice_lsp::LogLevel::Warn,
                        lattice_lsp::LogSource::Client,
                        format!("file-watcher watch {} failed: {e}", p.display()),
                    );
                }
            }
        }
    }

    /// Translate a `notify::Event` into per-server batched
    /// `workspace/didChangeWatchedFiles` notifications and fan
    /// them out. Notification-only on the LSP side; the actor's
    /// `did_change_watched_files` is a channel-send.
    fn dispatch_event(&mut self, event: NotifyEvent) {
        let Some(kind) = classify(&event) else {
            return;
        };
        // (path, kind) tuples. One event may cover several paths
        // (notify::Event::paths is a Vec).
        let classified: Vec<(&Path, FileChangeType)> =
            event.paths.iter().map(|p| (p.as_path(), kind)).collect();
        if classified.is_empty() {
            return;
        }
        // Per-server: match the classified paths against the
        // server's compiled subscription set; build the batch.
        let mut per_server: HashMap<String, Vec<lattice_lsp::lsp_types::FileEvent>> =
            HashMap::new();
        for (server_id, cached) in &self.by_server {
            if cached.subs.is_empty() {
                continue;
            }
            let mut batch: Vec<lattice_lsp::lsp_types::FileEvent> = Vec::new();
            for (path, change) in &classified {
                let hits = cached.subs.matches(path, *change);
                if hits.is_empty() {
                    continue;
                }
                let uri = lattice_lsp::actor::uri_from_path(path);
                batch.push(lattice_lsp::lsp_types::FileEvent::new(uri, *change));
            }
            if !batch.is_empty() {
                per_server.insert(server_id.clone(), batch);
            }
        }
        if per_server.is_empty() {
            return;
        }
        // Fan out. The supervisor handle's `running_actors` is a
        // wait-free ArcSwap load; the per-handle notify is a
        // channel send.
        for (_key, handle) in self.supervisor.running_actors() {
            let server_id = handle.server_id().to_string();
            let Some(batch) = per_server.remove(&server_id) else {
                continue;
            };
            let params = lattice_lsp::lsp_types::DidChangeWatchedFilesParams { changes: batch };
            if let Err(e) = handle.did_change_watched_files(params) {
                let instance = handle.instance();
                self.logger.log(
                    Some(&instance),
                    lattice_lsp::LogLevel::Warn,
                    lattice_lsp::LogSource::Client,
                    format!("workspace/didChangeWatchedFiles fan-out failed: {e}"),
                );
            }
        }
    }
}

/// Translate one `notify::Event` into an LSP `FileChangeType`.
pub fn classify(event: &NotifyEvent) -> Option<FileChangeType> {
    match event.kind {
        NotifyEventKind::Create(CreateKind::File)
        | NotifyEventKind::Create(CreateKind::Folder)
        | NotifyEventKind::Create(CreateKind::Any)
        | NotifyEventKind::Create(CreateKind::Other) => Some(FileChangeType::CREATED),
        NotifyEventKind::Modify(ModifyKind::Data(_))
        | NotifyEventKind::Modify(ModifyKind::Metadata(_))
        | NotifyEventKind::Modify(ModifyKind::Any)
        | NotifyEventKind::Modify(ModifyKind::Other)
        | NotifyEventKind::Modify(ModifyKind::Name(_)) => Some(FileChangeType::CHANGED),
        NotifyEventKind::Remove(RemoveKind::File)
        | NotifyEventKind::Remove(RemoveKind::Folder)
        | NotifyEventKind::Remove(RemoveKind::Any)
        | NotifyEventKind::Remove(RemoveKind::Other) => Some(FileChangeType::DELETED),
        _ => None,
    }
}
