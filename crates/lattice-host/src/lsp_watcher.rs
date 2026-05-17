//! App-side file watcher service type moved to host.
//! Provides a thin wrapper over `notify::RecommendedWatcher`
//! and caches per-server compiled watcher subscriptions.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use lsp_types::FileChangeType;
use notify::{
    Event as NotifyEvent, EventKind as NotifyEventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{CreateKind, ModifyKind, RemoveKind},
};

use lattice_lsp::WatcherSubscriptions;

/// One server's subscription snapshot + the fingerprint used to
/// detect changes. Keyed in [`LspFileWatcher`] by `server_id` so
/// fan-out can route batched `FileEvent`s back to the right
/// `ServerHandle`.
pub struct CachedSubscription {
    pub fingerprint: u64,
    pub subs: WatcherSubscriptions,
}

/// Host-side file-watcher service. One per editor instance; created
/// lazily on first use so editors that never run an LSP server pay
/// no cost.
pub struct LspFileWatcher {
    /// Underlying OS watcher.
    watcher: RecommendedWatcher,
    /// Paths currently watched.
    watched_roots: HashSet<PathBuf>,
    /// Event queue from the watcher thread.
    rx: tokio::sync::mpsc::UnboundedReceiver<NotifyEvent>,
    /// Per-server subscription cache.
    pub by_server: HashMap<String, CachedSubscription>,
}

impl LspFileWatcher {
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

    /// Sync the watched roots to match `target`.
    pub fn sync_watched_roots(
        &mut self,
        target: &HashSet<PathBuf>,
        logger: &lattice_lsp::LspLogger,
    ) {
        // Unwatch removed.
        let stale: Vec<PathBuf> = self
            .watched_roots
            .iter()
            .filter(|p| !target.contains(*p))
            .cloned()
            .collect();
        for p in stale {
            let _ = self.watcher.unwatch(&p);
            self.watched_roots.remove(&p);
        }
        // Watch new.
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
                Err(e) => logger.log(
                    None,
                    lattice_lsp::LogLevel::Warn,
                    lattice_lsp::LogSource::Client,
                    format!("file-watcher watch {} failed: {e}", p.display()),
                ),
            }
        }
    }

    pub fn watched_roots(&self) -> &HashSet<PathBuf> {
        &self.watched_roots
    }

    /// Drain queued fs events (used by ui-tui drain path).
    pub fn drain_pending(&mut self) -> Vec<NotifyEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
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
