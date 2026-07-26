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

use crate::diff::subsystem::DocumentBufferResolver;

use self::baseline::GitBaseline;

pub mod baseline;
pub mod options;

/// Tracks an auto-registered diff session for a single buffer.
/// The `BufferId` is the map key; the struct exists as a future extension
/// point for per-session metadata (repo path, last-known HEAD, etc.).
#[allow(dead_code)]
struct TrackedSession {
    buffer_id: BufferId,
}

/// The VCS subsystem — watches for document open/close events and
/// auto-registers diff sessions against git HEAD.
pub struct VcsSubsystem {
    sessions: std::sync::Mutex<HashMap<BufferId, TrackedSession>>,
}

/// Dropped on editor teardown; unsubscribes the event bus and aborts
/// the drainer task.
#[derive(Debug)]
pub struct VcsSubscriptionGuard {
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

impl VcsSubsystem {
    pub fn new() -> Self {
        Self {
            sessions: std::sync::Mutex::new(HashMap::new()),
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
        resolver: Arc<dyn DocumentBufferResolver>,
    ) -> VcsSubscriptionGuard {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();

        let subscription = bus.subscribe(
            lattice_runtime::EventFilter::kinds(vec![
                EventKind::DocumentOpened,
                EventKind::DocumentClosed,
            ]),
            lattice_runtime::SubscriptionTarget::Channel(tx),
        );

        let drainer = tokio::task::spawn(async move {
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

                        let Some(buffer_id) = resolver.buffer_id_for(id) else {
                            continue;
                        };

                        // Discover git repo
                        let repo = match Repository::discover(&path) {
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

                        // Build GitBaseline source
                        let source = Arc::new(GitBaseline::new(workdir, "HEAD", rel_path));

                        // Register diff session
                        let descriptor = DiffDescriptor {
                            sources: vec![source],
                            watch: vec![buffer_id],
                            participants: vec![buffer_id],
                        };

                        diff_subsystem.register_with_sources(
                            buffer_id,
                            lattice_diff::DiffAlgorithm::Histogram,
                            descriptor,
                        );

                        // Track
                        if let Ok(mut sessions) = self.sessions.lock() {
                            sessions.insert(buffer_id, TrackedSession { buffer_id });
                        }
                    }
                    Event::DocumentClosed { id } => {
                        let Some(buffer_id) = resolver.buffer_id_for(id) else {
                            continue;
                        };
                        if let Ok(mut sessions) = self.sessions.lock() {
                            if sessions.remove(&buffer_id).is_some() {
                                diff_subsystem.drop_session(buffer_id);
                            }
                        }
                    }
                    _ => {}
                }
            }
        });

        VcsSubscriptionGuard {
            bus,
            subscription,
            drainer,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for the VCS subsystem live in `editor.rs` (integration)
    // alongside the diff subsystem tests, since they require a full
    // Editor + BufferRegistry + diff subsystem setup.
}
