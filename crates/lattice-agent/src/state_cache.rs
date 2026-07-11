//! AG-2a: the protocol-neutral editor read-state cache.
//!
//! Per mode-ownership ([[feedback_mode_owns_its_surface]]), the read state
//! the agent read tools answer from belongs to **this crate**, not the host
//! and not any one agent-protocol adapter. Adapters subscribe to the generic
//! event bus (`DocumentOpened` / `DocumentClosed` / `SelectionsChanged`) and
//! fold those events into an `EditorStateCache`. A dedicated updater task
//! owns the writes; agent tasks read the cache off the editor thread. The
//! editor thread pays nothing new — it already `publish`es these events.
//!
//! On-demand text / path / dirty come from the generic `BufferStore`
//! service at read time, not this cache — the cache only tracks the
//! open-editor *set* and the active selection, which aren't otherwise
//! queryable off-thread.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lattice_protocol::ids::DocumentId;
use lattice_protocol::{Event, EventKind, SelectionSet};
use lattice_runtime::{EventBus, EventFilter, SubscriptionTarget};

/// One open editor buffer, tracked from `DocumentOpened`.
#[derive(Debug, Clone, Default)]
pub struct OpenBuffer {
    /// Filesystem path, if the buffer is a real file editor (`None` for
    /// scratch / unsaved buffers).
    pub path: Option<PathBuf>,
    /// Latest known text version.
    pub version: u64,
}

/// The active buffer's selection, tracked from `SelectionsChanged`.
#[derive(Debug, Clone)]
pub struct ActiveSelection {
    /// The buffer whose selection this is (the most recently active one).
    pub buffer: DocumentId,
    /// Text version the selection was reported against.
    pub version: u64,
    /// The selection set (usually one cursor / range).
    pub selections: SelectionSet,
}

/// Crate-owned snapshot of the editor state the read tools answer from.
/// Holds **no host types** — only protocol-level ids / selections / paths.
/// Mutated solely by the updater task (draining generic events); read by
/// consumers under the [`EditorStateHandle`] mutex.
#[derive(Debug, Default)]
pub struct EditorStateCache {
    /// Open editor buffers keyed by id.
    pub open_buffers: HashMap<DocumentId, OpenBuffer>,
    /// The active buffer + selection, if any buffer is active.
    pub active: Option<ActiveSelection>,
}

impl EditorStateCache {
    /// Fold one generic editor event into the cache. The subscription
    /// filters to the three relevant kinds; the catch-all keeps this a
    /// total, defensive match.
    pub fn apply_event(&mut self, event: &Event) {
        match event {
            Event::DocumentOpened {
                id, path, version, ..
            } => {
                self.open_buffers.insert(
                    *id,
                    OpenBuffer {
                        path: path.clone(),
                        version: *version,
                    },
                );
            }
            Event::DocumentClosed { id } => {
                self.open_buffers.remove(id);
                if self.active.as_ref().is_some_and(|a| a.buffer == *id) {
                    self.active = None;
                }
            }
            Event::SelectionsChanged {
                id,
                version,
                selections,
            } => {
                if let Some(b) = self.open_buffers.get_mut(id) {
                    b.version = *version;
                }
                self.active = Some(ActiveSelection {
                    buffer: *id,
                    version: *version,
                    selections: selections.clone(),
                });
            }
            _ => {}
        }
    }
}

/// Thread-safe handle to the read cache, shared between the updater task
/// (writer) and agent tasks (readers). A plain `Mutex` — updates are O(1)
/// in-place (no per-event clone) and reads are rare (agent-initiated), so
/// there's no contention worth a wait-free structure.
pub type EditorStateHandle = Arc<Mutex<EditorStateCache>>;

/// Subscribe to the generic event bus and spawn the cache-updater task.
/// Returns the shared cache handle. Fully crate-owned: the host only
/// publishes the (generic) events; this task + cache live here.
pub fn spawn_read_cache(bus: &Arc<EventBus>, rt: &tokio::runtime::Handle) -> EditorStateHandle {
    let cache: EditorStateHandle = Arc::new(Mutex::new(EditorStateCache::default()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    bus.subscribe(
        EventFilter::kinds(vec![
            EventKind::DocumentOpened,
            EventKind::DocumentClosed,
            EventKind::SelectionsChanged,
        ]),
        SubscriptionTarget::Channel(tx),
    );
    let cache_for_task = Arc::clone(&cache);
    rt.spawn(async move {
        while let Some(event) = rx.recv().await {
            cache_for_task
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .apply_event(&event);
        }
    });
    cache
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn doc(id: u64) -> DocumentId {
        DocumentId::new(id)
    }

    fn opened(id: u64, path: &str) -> Event {
        Event::DocumentOpened {
            id: doc(id),
            path: Some(PathBuf::from(path)),
            version: 1,
            text: String::new(),
        }
    }

    #[test]
    fn document_opened_adds_to_open_set_with_path() {
        let mut s = EditorStateCache::default();
        s.apply_event(&opened(1, "/work/a.rs"));
        assert_eq!(s.open_buffers.len(), 1);
        assert_eq!(
            s.open_buffers[&doc(1)].path.as_deref(),
            Some(std::path::Path::new("/work/a.rs"))
        );
    }

    #[test]
    fn selections_changed_sets_active_buffer_and_version() {
        let mut s = EditorStateCache::default();
        s.apply_event(&opened(1, "/work/a.rs"));
        s.apply_event(&Event::SelectionsChanged {
            id: doc(1),
            version: 7,
            selections: SelectionSet::default(),
        });
        let active = s.active.as_ref().expect("active set");
        assert_eq!(active.buffer, doc(1));
        assert_eq!(active.version, 7);
    }

    #[test]
    fn document_closed_removes_from_set_and_clears_active() {
        let mut s = EditorStateCache::default();
        s.apply_event(&opened(1, "/work/a.rs"));
        s.apply_event(&Event::SelectionsChanged {
            id: doc(1),
            version: 2,
            selections: SelectionSet::default(),
        });
        assert!(s.active.is_some());
        s.apply_event(&Event::DocumentClosed { id: doc(1) });
        assert!(s.open_buffers.is_empty());
        assert!(
            s.active.is_none(),
            "closing the active buffer clears active"
        );
    }

    #[test]
    fn closing_a_non_active_buffer_keeps_active() {
        let mut s = EditorStateCache::default();
        s.apply_event(&opened(1, "/work/a.rs"));
        s.apply_event(&opened(2, "/work/b.rs"));
        s.apply_event(&Event::SelectionsChanged {
            id: doc(2),
            version: 3,
            selections: SelectionSet::default(),
        });
        s.apply_event(&Event::DocumentClosed { id: doc(1) });
        assert_eq!(s.open_buffers.len(), 1);
        assert!(
            s.active.is_some(),
            "closing a non-active buffer leaves active"
        );
    }

    #[test]
    fn unrelated_event_is_a_noop() {
        let mut s = EditorStateCache::default();
        s.apply_event(&Event::DocumentSaved {
            id: doc(1),
            path: PathBuf::from("/work/a.rs"),
        });
        assert!(s.open_buffers.is_empty());
        assert!(s.active.is_none());
    }
}
