//! IDE-protocol I3: the write-request payload + per-item handler.
//!
//! Writes mutate the editor, which must happen on the editor (actor) thread.
//! The WS task hands a [`ClaudeCodeInboundRequest`] to the editor thread over
//! the generic inbound bus ([`lattice_mode::inbound`], via
//! [`SubsystemBoot::inbound`](lattice_mode::SubsystemBoot::inbound)) and awaits
//! a oneshot reply.
//!
//! BC.3b: the bespoke `ClaudeCodeInboundBus` + per-tick `make_drain` were
//! replaced by the generic `InboundBus<ClaudeCodeInboundRequest>` primitive,
//! whose `send` wakes the actor off-keystroke (the wake is baked into the
//! sender — structurally impossible to forget, paramount #4) and whose per-tick
//! drain runs each request through [`make_handler`]. This module now owns only
//! the claude-specific payload + mapping logic; the channel + drain + wake are
//! the shared primitive.
//!
//! [`make_handler`] validates + maps each request to an EXISTING `Effect` and
//! resolves its oneshot — optimistic-ack: `ok=true` on a valid map, `ok=false`
//! on an unknown / non-active target (option C, design §2: per-buffer save/close
//! targeting lands with the diff/tab work).

use std::path::{Path, PathBuf};

use lattice_grammar::effect::Effect;
use lattice_grammar::Utf16Pos;
use tokio::sync::oneshot;

use crate::snapshot::ReadStateHandle;

/// A write request's payload.
#[derive(Debug)]
pub enum InboundKind {
    /// Open `path`. `column` (a UTF-16 cursor position from the agent's
    /// `selection.start`, `None` when absent) is carried unconverted — the host
    /// resolves it to a byte offset against the opened line. BC.8c follow-up:
    /// maps to the HOST-APPLIED `OpenBufferAtColumn`, not the peer-applied
    /// `OpenBufferAt` (which the inbound tick path discards, so openFile never
    /// actually opened before this fix).
    OpenFile {
        path: PathBuf,
        column: Option<Utf16Pos>,
    },
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

/// Build the per-item handler for the generic inbound primitive
/// ([`SubsystemBoot::inbound`](lattice_mode::SubsystemBoot::inbound)). Maps one
/// request to its `Effect` (or none, on `ok=false`), resolves the oneshot, and
/// returns the effect(s) for the host to apply. The generic bus owns the
/// channel, the per-tick `try_recv` loop, and the off-keystroke wake.
pub fn make_handler(
    cache: ReadStateHandle,
) -> impl FnMut(ClaudeCodeInboundRequest) -> Vec<Effect> + Send + 'static {
    move |req| {
        let (effect, reply) = map_request(&req.kind, &cache);
        // A dropped response receiver (agent gone) is fine — log-and-skip.
        let _ = req.response.send(reply);
        effect.into_iter().collect()
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
        InboundKind::OpenFile { path, column } => (
            // BC.8c follow-up: host-applied open (works on the inbound tick
            // path, where peer-applied `OpenBufferAt` is discarded). The host
            // does do_edit + the UTF-16→byte cursor conversion against the
            // opened line; `column = None` opens without forcing the cursor.
            Some(Effect::OpenBufferAtColumn {
                path: Some(path.clone()),
                column: *column,
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
    use lattice_mode::inbound::make_inbound;
    use lattice_protocol::ids::DocumentId;
    use lattice_protocol::{Event, SelectionSet};
    use std::sync::{Arc, Mutex};
    use tokio::sync::Notify;

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
    fn open_file_maps_to_host_applied_open_ok() {
        // No selection → column None (open only). Maps to the HOST-APPLIED
        // OpenBufferAtColumn so it actually opens on the inbound tick path.
        let (e, r) = map_request(
            &InboundKind::OpenFile {
                path: PathBuf::from("/a.rs"),
                column: None,
            },
            &empty_cache(),
        );
        assert!(matches!(
            e,
            Some(Effect::OpenBufferAtColumn { column: None, .. })
        ));
        assert!(r.ok);
    }

    #[test]
    fn open_file_with_selection_carries_utf16_column() {
        let (e, _r) = map_request(
            &InboundKind::OpenFile {
                path: PathBuf::from("/a.rs"),
                column: Some(Utf16Pos { line: 3, col: 7 }),
            },
            &empty_cache(),
        );
        assert!(matches!(
            e,
            Some(Effect::OpenBufferAtColumn {
                column: Some(Utf16Pos { line: 3, col: 7 }),
                ..
            })
        ));
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
    fn handler_maps_request_and_resolves_oneshot() {
        // The generic inbound primitive (`make_inbound`) owns the channel + the
        // per-tick drain + the wake (those are pinned by lattice-mode's inbound
        // tests); this pins claude's per-item handler — it maps the request to
        // an Effect and resolves the oneshot.
        let (bus, mut drain) = make_inbound(Arc::new(Notify::new()), make_handler(empty_cache()));

        let (resp_tx, mut resp_rx) = oneshot::channel();
        bus.send(ClaudeCodeInboundRequest {
            kind: InboundKind::OpenFile {
                path: PathBuf::from("/a.rs"),
                column: None,
            },
            response: resp_tx,
        })
        .expect("send ok");

        let effects = drain();
        assert_eq!(effects.len(), 1);
        let reply = resp_rx.try_recv().expect("oneshot resolved");
        assert!(reply.ok);
    }
}
