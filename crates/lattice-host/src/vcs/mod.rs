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
        let Ok(mut sessions) = self.sessions.lock() else {
            return false;
        };
        match sessions.get_mut(&buffer_id) {
            Some(tracked) if !tracked.yielded => {
                tracked.yielded = true;
                diff_subsystem.drop_session(buffer_id);
                true
            }
            _ => false,
        }
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
        let Ok(mut sessions) = self.sessions.lock() else {
            return;
        };
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
                        let yielded = diff_subsystem.lookup_session_for(buffer_id).is_some();
                        if !yielded {
                            diff_subsystem.register_with_sources(
                                buffer_id,
                                lattice_diff::DiffAlgorithm::Histogram,
                                head_descriptor(buffer_id, &workdir, &rel_path),
                            );
                        }
                        if let Ok(mut sessions) = this.sessions.lock() {
                            sessions.insert(
                                buffer_id,
                                TrackedSession {
                                    workdir,
                                    rel_path,
                                    yielded,
                                },
                            );
                        }
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
    // Tests for the VCS subsystem live in `editor.rs` (integration)
    // alongside the diff subsystem tests, since they require a full
    // Editor + BufferRegistry + diff subsystem setup.
}
