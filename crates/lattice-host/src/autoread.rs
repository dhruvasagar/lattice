//! Autoread — external-change detection + refresh for file-backed
//! Document buffers (vim's `autoread`).
//!
//! See `docs/dev/architecture/autoread.md` for the design and
//! `docs/dev/operations/slice-plans/autoread.md` (AR.*) for sequencing.
//!
//! AR.0 (this file, first slice) lands the **on-disk fingerprint** only —
//! the seam every later slice gates on. No watcher yet. The fingerprint is
//! stamped when a buffer loads and after the editor's own `:w`; the live
//! `notify` watcher (AR.2) compares an incoming filesystem event's post-read
//! fingerprint against the stored one to (a) suppress the event its own save
//! produced and (b) skip no-op `touch`es.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use notify::{
    Event as NotifyEvent, EventKind as NotifyEventKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use tokio::sync::mpsc;

/// A fast, non-cryptographic hash of a buffer's text. Not stable across
/// process runs (that's fine — fingerprints are session-scoped) and not
/// collision-proof against an adversary (irrelevant — the input is the
/// user's own file, and a collision at worst suppresses one real reload).
fn hash_text(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// The on-disk identity of a file-backed buffer at the moment the editor
/// last synced with disk — a load, or its own `:w`.
///
/// Two comparison surfaces, deliberately distinct:
///
/// - [`Self::same_content`] (content hash) is the **authoritative** "is this
///   the same file we already have" test. A `touch` that bumps mtime without
///   changing bytes must compare equal, so mtime/size are *not* part of it.
/// - [`Self::stat_unchanged`] is the cheap `(mtime, size)` **pre-gate** the
///   watcher uses to decide whether it even needs to read + hash the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnDiskFingerprint {
    /// Last-modified time from `stat`, or `None` on platforms / filesystems
    /// where it's unavailable (then detection leans on `content_hash` alone).
    pub mtime: Option<SystemTime>,
    /// Byte length from `stat` (`0` when metadata is unavailable).
    pub size: u64,
    /// Hash of the text the editor holds for this file — the precise check
    /// that survives mtime-only touches and identifies self-writes.
    pub content_hash: u64,
}

impl OnDiskFingerprint {
    /// Build a fingerprint from `path`'s current metadata plus the `text`
    /// the editor holds for it. `stat` failure degrades to
    /// `mtime = None` / `size = 0` rather than erroring — a missing stat
    /// must never break a load or a save (paramount: never panic on the
    /// hot path; recover + lean on the content hash).
    pub fn from_path_and_text(path: &Path, text: &str) -> Self {
        let meta = std::fs::metadata(path).ok();
        let mtime = meta.as_ref().and_then(|m| m.modified().ok());
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        Self {
            mtime,
            size,
            content_hash: hash_text(text),
        }
    }

    /// True when `self` and `other` denote the same on-disk *content*.
    /// Content hash is authoritative; mtime/size are ignored so a bare
    /// `touch` is correctly treated as "no change".
    pub fn same_content(&self, other: &Self) -> bool {
        self.content_hash == other.content_hash
    }

    /// Cheap pre-gate: `true` when `path`'s current `(mtime, size)` still
    /// match this fingerprint, i.e. the file almost certainly hasn't
    /// changed and the watcher can skip the read + hash entirely. A `stat`
    /// failure returns `false` (fall through to the authoritative read),
    /// as does a `None` stored mtime (we never had a baseline to gate on).
    pub fn stat_unchanged(&self, path: &Path) -> bool {
        let Some(stored_mtime) = self.mtime else {
            return false;
        };
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        meta.len() == self.size && meta.modified().ok() == Some(stored_mtime)
    }
}

// ---------------------------------------------------------------------------
// AR.2 — the `notify` watcher runtime task.
//
// A tokio task on the LSP runtime owns a `notify::RecommendedWatcher` and a
// dir→basenames map. It watches the **parent directories** of open file-backed
// buffers **non-recursively** (never a tree — that's what keeps cost tied to
// open buffers, not project size; see `autoread.md` §3), filters events to the
// watched basenames, and emits `AutoreadChange`s the host drains (AR.4).
//
// Deliberately no task-side debounce: the host's fingerprint gate (`stat`
// pre-gate + content-hash) already coalesces — a burst of events for one save
// costs a few cheap host-side `stat`s, the first reloads, the rest are no-ops
// once the stored fingerprint matches disk. Mirrors `lsp_watcher.rs`.
// ---------------------------------------------------------------------------

/// What kind of external change the watcher detected for a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoreadChangeKind {
    /// Created, or content/metadata modified — a reload candidate. The host's
    /// fingerprint gate decides whether it's a real change.
    Modified,
    /// Removed or renamed away — the host keeps the buffer and warns (AR.4).
    Deleted,
}

/// A detected external change to a watched file. Emitted by the watcher task,
/// drained by the host, which maps `path` back to a `BufferId`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoreadChange {
    pub path: PathBuf,
    pub kind: AutoreadChangeKind,
}

/// Classify a `notify` event for autoread. `Create`/`Modify` ⇒ `Modified`;
/// `Remove` ⇒ `Deleted`; access/other ⇒ ignored. The classification is only a
/// hint — the host re-`stat`s on receipt, so a rename mis-labelled `Modified`
/// still resolves correctly (the host finds the file missing and treats it as
/// a delete).
pub fn classify_autoread(event: &NotifyEvent) -> Option<AutoreadChangeKind> {
    match event.kind {
        NotifyEventKind::Create(_) | NotifyEventKind::Modify(_) => {
            Some(AutoreadChangeKind::Modified)
        }
        NotifyEventKind::Remove(_) => Some(AutoreadChangeKind::Deleted),
        _ => None,
    }
}

/// True when `path` names a watched file: its parent directory is a watch root
/// AND its file name is in that directory's watched set. Pure — the O(1) filter
/// the task applies to every event so a busy shared parent directory costs only
/// a cheap discard, never a spurious change.
fn path_is_watched(path: &Path, watches: &HashMap<PathBuf, HashSet<String>>) -> bool {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
    else {
        return false;
    };
    watches
        .get(parent)
        .is_some_and(|names| names.contains(name))
}

/// Editor → watcher-task control commands.
pub enum AutoreadWatcherCommand {
    /// Atomically replace the watched set: parent-dir → the file names in that
    /// dir the editor cares about. The task installs a **non-recursive** watch
    /// per new dir and removes watches for dirs no longer present.
    Sync {
        watches: HashMap<PathBuf, HashSet<String>>,
    },
    /// Tear down every watch and exit the loop. The task also exits when
    /// `cmd_rx` closes (handle dropped).
    Shutdown,
}

/// Handle held by `Editor`. Cheap to own; sends are non-blocking. Drop it (or
/// send `Shutdown`) to tear the task down — the watcher drops and every OS
/// watch is released.
pub struct AutoreadWatcherHandle {
    cmd_tx: mpsc::UnboundedSender<AutoreadWatcherCommand>,
}

impl std::fmt::Debug for AutoreadWatcherHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AutoreadWatcherHandle")
            .finish_non_exhaustive()
    }
}

impl AutoreadWatcherHandle {
    /// Send the desired watched set. Best-effort: a dropped task silently
    /// no-ops (shouldn't happen in production).
    pub fn sync(&self, watches: HashMap<PathBuf, HashSet<String>>) {
        let _ = self.cmd_tx.send(AutoreadWatcherCommand::Sync { watches });
    }

    /// Tell the task to exit. Idempotent.
    pub fn shutdown(&self) {
        let _ = self.cmd_tx.send(AutoreadWatcherCommand::Shutdown);
    }
}

/// Spawn the autoread watcher task on the LSP runtime. Returns the handle
/// `Editor` keeps plus the change receiver the host drains (AR.4). `Err` only
/// if `notify` itself fails to construct the OS watcher.
pub fn spawn_autoread_watcher_task() -> Result<
    (
        AutoreadWatcherHandle,
        mpsc::UnboundedReceiver<AutoreadChange>,
    ),
    notify::Error,
> {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<AutoreadWatcherCommand>();
    let (event_tx, event_rx) = mpsc::unbounded_channel::<NotifyEvent>();
    let (change_tx, change_rx) = mpsc::unbounded_channel::<AutoreadChange>();
    // The callback runs on notify's own worker thread; forward every event to
    // the tokio channel and filter inside the task (which holds the map).
    let watcher = RecommendedWatcher::new(
        move |res: notify::Result<NotifyEvent>| {
            if let Ok(ev) = res {
                let _ = event_tx.send(ev);
            }
        },
        notify::Config::default(),
    )?;
    lattice_runtime::runtime::spawn_on_lsp_runtime(async move {
        let mut task = AutoreadWatcherTask {
            watcher,
            watched_dirs: HashSet::new(),
            watches: HashMap::new(),
            change_tx,
        };
        task.run(cmd_rx, event_rx).await;
    });
    Ok((AutoreadWatcherHandle { cmd_tx }, change_rx))
}

/// In-task state, owned exclusively by the spawned task — no locks. The editor
/// talks to it only through [`AutoreadWatcherCommand`]s.
struct AutoreadWatcherTask {
    watcher: RecommendedWatcher,
    /// Directories with a live OS watch.
    watched_dirs: HashSet<PathBuf>,
    /// dir → the basenames in that dir the editor cares about (the event
    /// filter).
    watches: HashMap<PathBuf, HashSet<String>>,
    change_tx: mpsc::UnboundedSender<AutoreadChange>,
}

impl AutoreadWatcherTask {
    async fn run(
        &mut self,
        mut cmd_rx: mpsc::UnboundedReceiver<AutoreadWatcherCommand>,
        mut event_rx: mpsc::UnboundedReceiver<NotifyEvent>,
    ) {
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => match cmd {
                    Some(AutoreadWatcherCommand::Sync { watches }) => self.sync(watches),
                    Some(AutoreadWatcherCommand::Shutdown) | None => break,
                },
                ev = event_rx.recv() => match ev {
                    Some(event) => self.dispatch(event),
                    None => break,
                },
            }
        }
    }

    /// Diff `target` against the live watch set: install a non-recursive watch
    /// per new dir, drop watches for dirs no longer wanted. Both `notify` calls
    /// are sync, so wrap them in `block_in_place` to avoid stalling the LSP
    /// runtime's reactor for sibling tasks.
    fn sync(&mut self, target: HashMap<PathBuf, HashSet<String>>) {
        // Canonicalize dir keys so they match the paths `notify` reports.
        // macOS FSEvents resolves symlinks (`/var` → `/private/var`, and the
        // temp dir lives under one); inotify echoes the path passed to
        // `watch()`. Watching the canonical dir — and keying `watches` by it —
        // keeps event-parent lookups consistent on both. A dir that fails to
        // canonicalize (vanished) is dropped; its buffers fall back to the
        // host's on-activate `stat` (AR.3).
        let target: HashMap<PathBuf, HashSet<String>> = target
            .into_iter()
            .filter_map(|(dir, names)| std::fs::canonicalize(&dir).ok().map(|c| (c, names)))
            .collect();
        let target_dirs: HashSet<&PathBuf> = target.keys().collect();
        let stale: Vec<PathBuf> = self
            .watched_dirs
            .iter()
            .filter(|d| !target_dirs.contains(*d))
            .cloned()
            .collect();
        for d in stale {
            let _ = tokio::task::block_in_place(|| self.watcher.unwatch(&d));
            self.watched_dirs.remove(&d);
        }
        let new: Vec<PathBuf> = target
            .keys()
            .filter(|d| !self.watched_dirs.contains(*d))
            .cloned()
            .collect();
        for d in new {
            // NON-recursive: autoread watches individual parent dirs, never a
            // tree — this is what bounds cost to open buffers, not project size.
            match tokio::task::block_in_place(|| {
                self.watcher.watch(&d, RecursiveMode::NonRecursive)
            }) {
                Ok(()) => {
                    self.watched_dirs.insert(d);
                }
                Err(e) => {
                    // A failed watch downgrades that dir's buffers to the host's
                    // on-activate `stat` fallback (AR.3); log + skip, never
                    // panic. `debug!` — watch churn can burst.
                    tracing::debug!(dir = %d.display(), error = %e, "autoread: watch install failed");
                }
            }
        }
        self.watches = target;
    }

    /// Filter one event to the watched basenames and emit a change per match.
    fn dispatch(&self, event: NotifyEvent) {
        let Some(kind) = classify_autoread(&event) else {
            return;
        };
        for path in &event.paths {
            if path_is_watched(path, &self.watches) {
                let _ = self.change_tx.send(AutoreadChange {
                    path: path.clone(),
                    kind,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "lattice-autoread-{}-{}-{}",
            tag,
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn same_content_ignores_mtime_and_size() {
        // Two fingerprints with identical content hash but different
        // mtime/size compare equal-by-content — a `touch` is not a change.
        let a = OnDiskFingerprint {
            mtime: Some(SystemTime::UNIX_EPOCH),
            size: 10,
            content_hash: hash_text("hello"),
        };
        let b = OnDiskFingerprint {
            mtime: Some(SystemTime::now()),
            size: 999,
            content_hash: hash_text("hello"),
        };
        assert!(a.same_content(&b), "same bytes ⇒ same content");
    }

    #[test]
    fn same_content_differs_on_real_edit() {
        let a = OnDiskFingerprint::from_path_and_text(Path::new("/nonexistent"), "one");
        let b = OnDiskFingerprint::from_path_and_text(Path::new("/nonexistent"), "two");
        assert!(!a.same_content(&b), "different bytes ⇒ different content");
    }

    #[test]
    fn self_write_is_suppressible_by_content_hash() {
        // Simulate: we save text T (stamp F), then read disk back (F').
        // Even though the on-disk mtime moved, F'.same_content(&F) holds,
        // so the watcher can recognise its own write.
        let path = temp_path("selfwrite");
        std::fs::write(&path, "saved text\n").unwrap();
        let stamped = OnDiskFingerprint::from_path_and_text(&path, "saved text\n");
        // A later read of the unchanged file yields the same content hash.
        let reread = OnDiskFingerprint::from_path_and_text(&path, "saved text\n");
        assert!(stamped.same_content(&reread));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn stat_unchanged_true_when_untouched_then_false_after_write() {
        let path = temp_path("stat");
        std::fs::write(&path, "v1\n").unwrap();
        let fp = OnDiskFingerprint::from_path_and_text(&path, "v1\n");
        assert!(fp.stat_unchanged(&path), "freshly stamped ⇒ stat unchanged");

        // Rewrite with different length + (almost certainly) newer mtime.
        std::fs::write(&path, "v2-longer\n").unwrap();
        assert!(
            !fp.stat_unchanged(&path),
            "size/mtime moved ⇒ stat gate opens"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn stat_unchanged_false_when_no_baseline_mtime_or_missing_file() {
        let no_mtime = OnDiskFingerprint {
            mtime: None,
            size: 0,
            content_hash: 0,
        };
        assert!(!no_mtime.stat_unchanged(Path::new("/nonexistent")));

        let fp = OnDiskFingerprint::from_path_and_text(Path::new("/definitely/missing"), "x");
        assert!(!fp.stat_unchanged(Path::new("/definitely/missing")));
    }

    // ---- AR.2: watcher decision logic + one fs integration test ----

    #[test]
    fn classify_maps_create_modify_remove_and_ignores_access() {
        use notify::EventKind;
        use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind};
        assert_eq!(
            classify_autoread(&NotifyEvent::new(EventKind::Create(CreateKind::File))),
            Some(AutoreadChangeKind::Modified)
        );
        assert_eq!(
            classify_autoread(&NotifyEvent::new(EventKind::Modify(ModifyKind::Any))),
            Some(AutoreadChangeKind::Modified)
        );
        assert_eq!(
            classify_autoread(&NotifyEvent::new(EventKind::Remove(RemoveKind::File))),
            Some(AutoreadChangeKind::Deleted)
        );
        assert_eq!(
            classify_autoread(&NotifyEvent::new(EventKind::Access(AccessKind::Read))),
            None
        );
    }

    #[test]
    fn path_is_watched_matches_dir_and_basename_only() {
        let mut names = HashSet::new();
        names.insert("main.rs".to_string());
        let mut watches = HashMap::new();
        watches.insert(PathBuf::from("/proj/src"), names);

        assert!(path_is_watched(Path::new("/proj/src/main.rs"), &watches));
        // Right dir, wrong basename (the O(1) filter that discards a busy
        // shared directory's other files).
        assert!(!path_is_watched(Path::new("/proj/src/other.rs"), &watches));
        // Right basename, wrong (unwatched) dir.
        assert!(!path_is_watched(Path::new("/proj/other/main.rs"), &watches));
    }

    #[test]
    fn watcher_emits_change_on_external_write() {
        use std::time::Duration;
        let dir = temp_path("watch-integ");
        std::fs::create_dir_all(&dir).unwrap();
        // Canonicalize so the expected path matches what notify reports
        // (macOS FSEvents resolves the temp dir's symlink); the watcher
        // canonicalizes its keys the same way.
        let dir = std::fs::canonicalize(&dir).unwrap();
        let file = dir.join("watched.txt");
        std::fs::write(&file, "v1\n").unwrap();

        let (handle, mut rx) = spawn_autoread_watcher_task().expect("spawn watcher");
        let mut names = HashSet::new();
        names.insert("watched.txt".to_string());
        let mut watches = HashMap::new();
        watches.insert(dir.clone(), names);
        handle.sync(watches);

        // Let the Sync command install the OS watch before writing.
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(&file, "v2-changed\n").unwrap();

        let got = lattice_runtime::block_on(async move {
            tokio::time::timeout(Duration::from_secs(5), rx.recv()).await
        });
        handle.shutdown();
        std::fs::remove_dir_all(&dir).ok();

        let change = got
            .expect("watcher emitted a change before the timeout")
            .expect("change channel open");
        assert_eq!(change.path, file);
        assert_eq!(change.kind, AutoreadChangeKind::Modified);
    }
}
