//! Layer 2 VCS subsystem — auto-inline-diff against HEAD.
//!
//! Automatically registers a `DiffSession` with a `GitBaseline`
//! (file content at HEAD) when a file is opened inside a git
//! repository. Gutter signs appear immediately, showing changes
//! since the last commit.
//!
//! Controlled by the `git.auto-head-diff` typed option.
//!
//! See `docs/dev/architecture/magit.md` §3 (three-layer model)
//! and `docs/dev/operations/slice-plans/magit.md` VCS.2.

use std::collections::HashMap;
use std::sync::Arc;

use lattice_core::BufferId;
use lattice_diff::subsystem::{DiffDescriptor, DiffSubsystem};
use lattice_protocol::event::{Event, EventKind};
use lattice_runtime::EventBus;
use lattice_vcs::Repository;

use self::baseline::GitBaseline;

pub mod baseline;
pub mod options;

/// A buffer this subsystem diffs against HEAD. The `BufferId` is the map
/// key.
///
/// An explicit diff (`:diffsplit`, magit's `dv`, a conflict opened for
/// resolution) needs the buffer to itself: a buffer can be in one session,
/// and the HEAD session occupies it from the moment the file opens. So
/// the HEAD session steps aside ([`VcsSubsystem::yield_buffer`]) and is
/// re-armed from what is kept here once the explicit diff ends
/// ([`VcsSubsystem::reclaim`]).
struct TrackedSession {
    workdir: std::path::PathBuf,
    rel_path: std::path::PathBuf,
    /// The HEAD session is not registered: an explicit diff holds the
    /// buffer.
    yielded: bool,
}

/// The buffer a document event is about.
///
/// The host publishes these events with the BUFFER's id in the `id` slot
/// (`Editor::publish_document_opened_for_active`), and every consumer —
/// LSP, the agent's state cache, plugins over WIT — reads it that way.
/// This subsystem used to resolve it as a document-handle id instead,
/// which matched only while the two counters happened to agree: a file
/// opened once they had drifted apart (any synthetic buffer, or a second
/// editor in the same process) silently got no gutter signs.
fn buffer_of(id: lattice_protocol::ids::DocumentId) -> Option<BufferId> {
    u32::try_from(id.raw()).ok().map(BufferId)
}

/// The descriptor of `buffer_id`'s diff against HEAD.
fn head_descriptor(
    buffer_id: BufferId,
    workdir: &std::path::Path,
    rel_path: &std::path::Path,
) -> DiffDescriptor {
    let source = Arc::new(GitBaseline::new(
        workdir.to_path_buf(),
        "HEAD",
        rel_path.to_path_buf(),
    ));
    DiffDescriptor {
        sources: vec![source],
        watch: vec![buffer_id],
        participants: vec![buffer_id],
    }
}

/// The VCS subsystem — watches for document open/close events and
/// auto-registers diff sessions against git HEAD.
pub struct VcsSubsystem {
    sessions: std::sync::Mutex<HashMap<BufferId, TrackedSession>>,
}

type Sessions = HashMap<BufferId, TrackedSession>;

/// The subsystem held still while an explicit diff takes buffers over.
///
/// The drainer registers a HEAD session on its own thread when a file
/// opens, and an explicit diff opens the file it diffs against just before
/// claiming it. Without a hold the drainer can land between the claim and
/// the registration: the file then has a HEAD session the claim never saw,
/// and the explicit diff is refused as overlapping. The drainer decides and
/// registers under the same lock, so while a hold lives it either finished
/// first (its session is yielded here) or runs after (and sees the explicit
/// session, so it steps aside on its own).
pub struct VcsHold<'a> {
    sessions: std::sync::MutexGuard<'a, Sessions>,
}

impl VcsHold<'_> {
    /// [`VcsSubsystem::yield_buffer`], under the hold.
    pub fn yield_buffer(&mut self, buffer_id: BufferId, diff_subsystem: &DiffSubsystem) -> bool {
        yield_in(&mut self.sessions, buffer_id, diff_subsystem)
    }

    /// [`VcsSubsystem::reclaim`], under the hold.
    pub fn reclaim(&mut self, buffer_id: BufferId, diff_subsystem: &DiffSubsystem) {
        reclaim_in(&mut self.sessions, buffer_id, diff_subsystem);
    }
}

fn yield_in(sessions: &mut Sessions, buffer_id: BufferId, diff_subsystem: &DiffSubsystem) -> bool {
    match sessions.get_mut(&buffer_id) {
        Some(tracked) if !tracked.yielded => {
            tracked.yielded = true;
            diff_subsystem.drop_session(buffer_id);
            true
        }
        _ => false,
    }
}

fn reclaim_in(sessions: &mut Sessions, buffer_id: BufferId, diff_subsystem: &DiffSubsystem) {
    let Some(tracked) = sessions.get_mut(&buffer_id) else {
        return;
    };
    if !tracked.yielded || diff_subsystem.lookup_session_for(buffer_id).is_some() {
        return;
    }
    tracked.yielded = false;
    diff_subsystem.register_with_sources(
        buffer_id,
        lattice_diff::DiffAlgorithm::Histogram,
        head_descriptor(buffer_id, &tracked.workdir, &tracked.rel_path),
    );
}

/// Dropped on editor teardown; unsubscribes the event bus and aborts
/// the drainer task.
pub struct VcsSubscriptionGuard {
    subsystem: Arc<VcsSubsystem>,
    bus: Arc<EventBus>,
    subscription: lattice_runtime::SubscriptionId,
    drainer: tokio::task::JoinHandle<()>,
}

impl Drop for VcsSubscriptionGuard {
    fn drop(&mut self) {
        self.bus.unsubscribe(self.subscription);
        self.drainer.abort();
    }
}

impl std::fmt::Debug for VcsSubscriptionGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VcsSubscriptionGuard")
            .field("subscription", &self.subscription)
            .finish_non_exhaustive()
    }
}

impl VcsSubscriptionGuard {
    /// The subsystem this guard binds.
    pub fn subsystem(&self) -> &VcsSubsystem {
        &self.subsystem
    }

    /// The subsystem, shared: for a [`VcsSubsystem::hold`] that must
    /// outlive a borrow of whatever owns this guard.
    pub fn subsystem_handle(&self) -> &Arc<VcsSubsystem> {
        &self.subsystem
    }
}

impl Default for VcsSubsystem {
    fn default() -> Self {
        Self::new()
    }
}

impl VcsSubsystem {
    pub fn new() -> Self {
        Self {
            sessions: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Let an explicit diff have `buffer_id`: drop its HEAD session, and
    /// remember to re-arm it. False when the subsystem does not diff the
    /// buffer, or has already stepped aside.
    pub fn yield_buffer(&self, buffer_id: BufferId, diff_subsystem: &DiffSubsystem) -> bool {
        self.hold()
            .is_some_and(|mut hold| hold.yield_buffer(buffer_id, diff_subsystem))
    }

    /// Hold the subsystem still across an explicit diff's claim and its
    /// registration; see [`VcsHold`]. `None` when the lock is poisoned.
    pub fn hold(&self) -> Option<VcsHold<'_>> {
        self.sessions
            .lock()
            .ok()
            .map(|sessions| VcsHold { sessions })
    }

    /// True when `buffer_id`'s session is this subsystem's diff against
    /// HEAD, one an explicit diff may take over.
    pub fn holds_head_diff(&self, buffer_id: BufferId) -> bool {
        self.sessions
            .lock()
            .is_ok_and(|s| s.get(&buffer_id).is_some_and(|t| !t.yielded))
    }

    /// Re-arm `buffer_id`'s HEAD session, if it stepped aside and nothing
    /// else holds the buffer now. Safe to call on any buffer.
    pub fn reclaim(&self, buffer_id: BufferId, diff_subsystem: &DiffSubsystem) {
        if let Some(mut hold) = self.hold() {
            hold.reclaim(buffer_id, diff_subsystem);
        }
    }

    /// Bind the subsystem to the event bus. Spawns a drainer task
    /// that listens for `DocumentOpened` / `DocumentClosed` events
    /// and auto-registers / tears down diff sessions.
    ///
    /// Returns a guard whose `Drop` cleans up the subscription + task.
    pub fn bind(
        self: Arc<Self>,
        bus: Arc<EventBus>,
        diff_subsystem: Arc<DiffSubsystem>,
        config: Arc<lattice_config::ConfigRegistry>,
    ) -> VcsSubscriptionGuard {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();

        let subscription = bus.subscribe(
            lattice_runtime::EventFilter::kinds(vec![
                EventKind::DocumentOpened,
                EventKind::DocumentClosed,
            ]),
            lattice_runtime::SubscriptionTarget::Channel(tx),
        );

        let this = Arc::clone(&self);
        let drainer = tokio::task::spawn(async move {
            let this = this;
            while let Some(event) = rx.recv().await {
                match event {
                    Event::DocumentOpened { id, path, .. } => {
                        let Some(path) = path else { continue };

                        // Check option
                        let auto = config
                            .get_typed::<options::GitAutoHeadDiff>()
                            .map(|v| *v)
                            .unwrap_or(true);
                        if !auto {
                            continue;
                        }

                        let Some(buffer_id) = buffer_of(id) else {
                            continue;
                        };

                        // Discover git repo. `gix::discover` requires a
                        // directory — `path` is the opened file itself, so
                        // start from its parent (bug fix: this previously
                        // always failed with "not a directory", meaning
                        // auto-head-diff gutter signs never appeared for
                        // any file).
                        let Some(parent) = path.parent() else {
                            continue;
                        };
                        let repo = match Repository::discover(parent) {
                            Ok(r) => r,
                            Err(_) => continue, // not in a git repo
                        };

                        let workdir = match repo.workdir() {
                            Some(w) => w.to_path_buf(),
                            None => continue,
                        };

                        // Compute path relative to repo root
                        let rel_path = match path.strip_prefix(&workdir) {
                            Ok(p) => p.to_path_buf(),
                            Err(_) => continue,
                        };

                        // A file opened BY an explicit diff (`:diffsplit`
                        // opening its other side, a conflict opening the
                        // working file) is already in that session by the
                        // time this event lands. Registering here would
                        // replace that session's descriptor with HEAD's, so
                        // the file is tracked as stepped aside instead, and
                        // re-armed once the explicit diff ends.
                        //
                        // Decided and registered under the lock, so an
                        // explicit diff's claim ([`VcsHold`]) sees either
                        // all of it or none.
                        let Ok(mut sessions) = this.sessions.lock() else {
                            continue;
                        };
                        let yielded = diff_subsystem.lookup_session_for(buffer_id).is_some();
                        if !yielded {
                            diff_subsystem.register_with_sources(
                                buffer_id,
                                lattice_diff::DiffAlgorithm::Histogram,
                                head_descriptor(buffer_id, &workdir, &rel_path),
                            );
                        }
                        sessions.insert(
                            buffer_id,
                            TrackedSession {
                                workdir,
                                rel_path,
                                yielded,
                            },
                        );
                    }
                    Event::DocumentClosed { id } => {
                        let Some(buffer_id) = buffer_of(id) else {
                            continue;
                        };
                        // A stepped-aside buffer's session is the explicit
                        // diff's, not ours to drop.
                        if let Ok(mut sessions) = this.sessions.lock()
                            && sessions.remove(&buffer_id).is_some_and(|t| !t.yielded)
                        {
                            diff_subsystem.drop_session(buffer_id);
                        }
                    }
                    _ => {}
                }
            }
        });

        VcsSubscriptionGuard {
            subsystem: self,
            bus,
            subscription,
            drainer,
        }
    }
}

#[cfg(test)]
mod tests {
    // The editor-level behaviour (the gutter signs, `:diffsplit` taking a
    // file over) is tested through the editor; this is the subsystem's own
    // concurrency contract.
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use std::time::Duration;

    /// A file opened just before an explicit diff claims it: the drainer's
    /// HEAD registration must not land between the claim and the
    /// registration. With the hold alive the drainer waits; once it runs it
    /// finds the explicit diff and steps aside, leaving that diff alone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_held_claim_keeps_the_drainer_out_until_the_diff_registers() {
        let dir = tempfile::tempdir().unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(init.success());
        let workdir = Repository::discover(dir.path())
            .unwrap()
            .workdir()
            .unwrap()
            .to_path_buf();
        std::fs::write(workdir.join("f.txt"), "text\n").unwrap();

        let bus = Arc::new(EventBus::new());
        let diff = Arc::new(DiffSubsystem::default());
        let vcs = Arc::new(VcsSubsystem::new());
        let _guard = Arc::clone(&vcs).bind(
            Arc::clone(&bus),
            Arc::clone(&diff),
            Arc::new(lattice_config::ConfigRegistry::new()),
        );
        let buffer = BufferId(7);

        let hold = vcs.hold().unwrap();
        bus.publish(Event::DocumentOpened {
            id: lattice_protocol::ids::DocumentId::new(7),
            path: Some(workdir.join("f.txt")),
            version: 0,
            text: "text\n".into(),
        });
        // Long enough for an unheld drainer to discover the repository
        // and register; a held one cannot, however long this is.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            diff.lookup_session_for(buffer).is_none(),
            "the drainer registered HEAD while the claim was held"
        );
        // The explicit diff registers (its descriptor's shape is not the
        // point), then lets go.
        let explicit = diff.register_with_sources(
            buffer,
            lattice_diff::DiffAlgorithm::Histogram,
            head_descriptor(buffer, &workdir, std::path::Path::new("f.txt")),
        );
        drop(hold);

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !vcs.sessions.lock().unwrap().contains_key(&buffer) {
            assert!(
                std::time::Instant::now() < deadline,
                "the drainer never ran"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            !vcs.holds_head_diff(buffer),
            "the drainer saw the explicit diff and stepped aside"
        );
        let now = diff.lookup_session_for(buffer).unwrap();
        assert!(
            Arc::ptr_eq(&now, &explicit),
            "the explicit diff is untouched"
        );
    }
}
