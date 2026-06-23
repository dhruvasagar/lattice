//! IDE-protocol I3: the write-request inbound bus + per-tick drain.
//!
//! Writes mutate the editor, which must happen on the editor (actor) thread.
//! The WS task hands a request to the editor thread over this bus and awaits a
//! oneshot reply.
//!
//! CRITICAL (paramount #4 — async-correct by construction, not by discipline):
//! [`ClaudeCodeInboundBus::send`] **wakes the actor** (`async_landed.notify_one`)
//! so the per-tick drain runs WITHOUT a keystroke. The wake is baked into the
//! sender here so it is structurally impossible to forget — the bug class
//! `boot-composition.md` §3 designs out, and the migration-ready shape for that
//! `inbound::<T>` primitive.
//!
//! The drain (registered via the I1 tick-callback registry) `try_recv`s pending
//! requests, validates + maps each to an EXISTING `Effect`, returns the effects
//! for the host to apply, and resolves each oneshot — optimistic-ack: `ok=true`
//! on a valid map, `ok=false` on an unknown / non-active target (option C,
//! design §2: per-buffer save/close targeting lands with the diff/tab work).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lattice_grammar::effect::Effect;
use lattice_protocol::Position;
use tokio::sync::{Notify, mpsc, oneshot};

use crate::snapshot::ReadStateHandle;

/// A write request's payload.
#[derive(Debug)]
pub enum InboundKind {
    /// Open `path`, placing the cursor at `position`.
    OpenFile { path: PathBuf, position: Position },
    /// Save the document for `path` (option C: only when it's the active buffer).
    SaveDocument { path: PathBuf },
    /// Close the tab named `tab_name` (option C: only the active buffer in I3;
    /// `tab_name` is treated as a file path).
    CloseTab { tab_name: String },
}

/// The drain's reply to the WS task. Optimistic-ack: `ok` reflects whether the
/// request mapped to a valid Effect, not the eventual apply result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundReply {
    pub ok: bool,
    pub message: Option<String>,
}

impl InboundReply {
    fn ok() -> Self {
        Self {
            ok: true,
            message: None,
        }
    }
    fn fail(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: Some(msg.into()),
        }
    }
}

/// One write request: payload + the oneshot the drain resolves. Mirrors LSP's
/// `InboundShowDocument`.
#[derive(Debug)]
pub struct ClaudeCodeInboundRequest {
    pub kind: InboundKind,
    pub response: oneshot::Sender<InboundReply>,
}

/// Sender half — held by the WS task (write tools). `send` wakes the actor so
/// the drain runs off-keystroke.
#[derive(Clone)]
pub struct ClaudeCodeInboundBus {
    tx: mpsc::UnboundedSender<ClaudeCodeInboundRequest>,
    wake: Arc<Notify>,
}

impl ClaudeCodeInboundBus {
    /// Build the bus + its receiver. `wake` is the editor's `async_landed`.
    pub fn new(wake: Arc<Notify>) -> (Self, mpsc::UnboundedReceiver<ClaudeCodeInboundRequest>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx, wake }, rx)
    }

    /// Send a request and wake the actor. Returns the request back on failure
    /// (receiver dropped — server stopped), so the caller reports a graceful
    /// error instead of awaiting a oneshot that will never resolve.
    pub fn send(
        &self,
        req: ClaudeCodeInboundRequest,
    ) -> Result<(), ClaudeCodeInboundRequest> {
        self.tx.send(req).map_err(|e| e.0)?;
        self.wake.notify_one();
        Ok(())
    }
}

/// Build the per-tick drain closure registered with the tick-callback registry.
/// Drains all pending requests, maps each to an Effect (or `ok=false`), resolves
/// oneshots, and returns the Effects for the host to apply.
pub fn make_drain(
    mut rx: mpsc::UnboundedReceiver<ClaudeCodeInboundRequest>,
    cache: ReadStateHandle,
) -> impl FnMut() -> Vec<Effect> + Send + 'static {
    move || {
        let mut effects = Vec::new();
        while let Ok(req) = rx.try_recv() {
            let (effect, reply) = map_request(&req.kind, &cache);
            if let Some(e) = effect {
                effects.push(e);
            }
            // A dropped response receiver (agent gone) is fine — log-and-skip.
            let _ = req.response.send(reply);
        }
        effects
    }
}

/// The active buffer's path, if any (option-C targeting).
fn active_path(cache: &ReadStateHandle) -> Option<PathBuf> {
    let g = cache.lock().unwrap_or_else(|e| e.into_inner());
    let active = g.active.as_ref()?;
    g.open_buffers.get(&active.buffer).and_then(|b| b.path.clone())
}

/// Map a write request to an existing Effect + an optimistic reply.
fn map_request(kind: &InboundKind, cache: &ReadStateHandle) -> (Option<Effect>, InboundReply) {
    match kind {
        InboundKind::OpenFile { path, position } => (
            Some(Effect::OpenBufferAt {
                path: Some(path.clone()),
                position: *position,
                force: false,
            }),
            InboundReply::ok(),
        ),
        InboundKind::SaveDocument { path } => {
            if active_path(cache).as_deref() == Some(path.as_path()) {
                (Some(Effect::SaveBuffer { path: None }), InboundReply::ok())
            } else {
                (
                    None,
                    InboundReply::fail(
                        "saveDocument: target is not the active buffer (I3 limitation)",
                    ),
                )
            }
        }
        InboundKind::CloseTab { tab_name } => {
            if active_path(cache).as_deref() == Some(Path::new(tab_name)) {
                (Some(Effect::BufferDelete { force: false }), InboundReply::ok())
            } else {
                (
                    None,
                    InboundReply::fail("close_tab: only the active buffer can be closed in I3"),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::snapshot::ClaudeCodeReadState;
    use lattice_protocol::ids::DocumentId;
    use lattice_protocol::{Event, SelectionSet};
    use std::sync::Mutex;
    use std::time::Duration;

    fn cache_with_active(path: &str) -> ReadStateHandle {
        let mut s = ClaudeCodeReadState::default();
        s.apply_event(&Event::DocumentOpened {
            id: DocumentId::new(1),
            path: Some(PathBuf::from(path)),
            version: 1,
            text: String::new(),
        });
        s.apply_event(&Event::SelectionsChanged {
            id: DocumentId::new(1),
            version: 1,
            selections: SelectionSet::default(),
        });
        Arc::new(Mutex::new(s))
    }

    fn empty_cache() -> ReadStateHandle {
        Arc::new(Mutex::new(ClaudeCodeReadState::default()))
    }

    #[test]
    fn open_file_maps_to_open_buffer_at_ok() {
        let (e, r) = map_request(
            &InboundKind::OpenFile {
                path: PathBuf::from("/a.rs"),
                position: Position::ZERO,
            },
            &empty_cache(),
        );
        assert!(matches!(e, Some(Effect::OpenBufferAt { .. })));
        assert!(r.ok);
    }

    #[test]
    fn save_active_buffer_maps_to_save_ok() {
        let (e, r) = map_request(
            &InboundKind::SaveDocument {
                path: PathBuf::from("/a.rs"),
            },
            &cache_with_active("/a.rs"),
        );
        assert!(matches!(e, Some(Effect::SaveBuffer { .. })));
        assert!(r.ok);
    }

    #[test]
    fn save_non_active_buffer_is_ok_false_no_effect() {
        let (e, r) = map_request(
            &InboundKind::SaveDocument {
                path: PathBuf::from("/other.rs"),
            },
            &cache_with_active("/a.rs"),
        );
        assert!(e.is_none());
        assert!(!r.ok);
    }

    #[test]
    fn close_active_maps_to_delete_else_ok_false() {
        let (e, r) = map_request(
            &InboundKind::CloseTab {
                tab_name: "/a.rs".to_string(),
            },
            &cache_with_active("/a.rs"),
        );
        assert!(matches!(e, Some(Effect::BufferDelete { .. })));
        assert!(r.ok);

        let (e2, r2) = map_request(
            &InboundKind::CloseTab {
                tab_name: "/nope.rs".to_string(),
            },
            &cache_with_active("/a.rs"),
        );
        assert!(e2.is_none());
        assert!(!r2.ok);
    }

    #[test]
    fn drain_applies_effect_and_resolves_oneshot() {
        let wake = Arc::new(Notify::new());
        let (bus, rx) = ClaudeCodeInboundBus::new(wake);
        let mut drain = make_drain(rx, empty_cache());

        let (resp_tx, mut resp_rx) = oneshot::channel();
        bus.send(ClaudeCodeInboundRequest {
            kind: InboundKind::OpenFile {
                path: PathBuf::from("/a.rs"),
                position: Position::ZERO,
            },
            response: resp_tx,
        })
        .expect("send ok");

        let effects = drain();
        assert_eq!(effects.len(), 1);
        let reply = resp_rx.try_recv().expect("oneshot resolved");
        assert!(reply.ok);
    }

    #[tokio::test]
    async fn send_wakes_the_actor() {
        let wake = Arc::new(Notify::new());
        let (bus, _rx) = ClaudeCodeInboundBus::new(wake.clone());
        let (resp_tx, _resp_rx) = oneshot::channel();
        bus.send(ClaudeCodeInboundRequest {
            kind: InboundKind::OpenFile {
                path: PathBuf::from("/a.rs"),
                position: Position::ZERO,
            },
            response: resp_tx,
        })
        .expect("send ok");
        // The wake permit stored by `notify_one` must let a `notified()` resolve
        // promptly — i.e. the actor would wake off-keystroke.
        let woke = tokio::time::timeout(Duration::from_millis(200), wake.notified()).await;
        assert!(woke.is_ok(), "send must wake the actor");
    }

    #[test]
    fn dropped_receiver_makes_send_fail_gracefully() {
        let wake = Arc::new(Notify::new());
        let (bus, rx) = ClaudeCodeInboundBus::new(wake);
        drop(rx); // server stopped
        let (resp_tx, _resp_rx) = oneshot::channel();
        let result = bus.send(ClaudeCodeInboundRequest {
            kind: InboundKind::OpenFile {
                path: PathBuf::from("/a.rs"),
                position: Position::ZERO,
            },
            response: resp_tx,
        });
        assert!(result.is_err(), "dropped receiver → send returns the request");
    }
}
