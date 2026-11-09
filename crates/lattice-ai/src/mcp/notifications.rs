//! IDE-protocol I6: server-initiated notifications (server → agent).
//!
//! A task subscribes to the generic event bus (`SelectionsChanged`), coalesces
//! bursts (cursor moves fire at ~30 Hz under a held key), and broadcasts
//! `selection_changed` — plus a `didChangeActiveEditor` when the active buffer
//! changes — to every connected agent through the server's broadcast sender
//! ([`crate::mcp::server::ClaudeCodeServerHandle::notify_sender`]). Each connection's
//! forwarder relays the frame to its WS writer; a lagged connection skips
//! dropped frames (coalescing — latest wins).
//!
//! Crate-owned per `feedback_mode_owns_its_surface`: the host only publishes
//! the generic events. The editor thread pays nothing new — it already
//! `publish`es `SelectionsChanged`; the coalescing + framing run off-thread.
//!
//! **The frame SHAPE is PROVISIONAL** until validated against a live `claude`
//! CLI — server→agent notification formats are less documented than the tool
//! replies. In particular `selection.start/end.character` carries the editor's
//! **byte** offset within the line (lattice's `Position.byte`); the VS Code
//! contract is a UTF-16 character offset. Carrying it verbatim mirrors the I3
//! selection-encoding caveat; the conversion lands once a live CLI pins it.

use std::path::Path;
use std::sync::Arc;

use lattice_agent::EditorStateHandle;
use lattice_protocol::ids::DocumentId;
use lattice_protocol::position::Position;
use lattice_protocol::{Event, EventKind, SelectionSet};
use lattice_runtime::{EventBus, EventFilter, SubscriptionTarget};
use serde_json::json;
use tokio::sync::broadcast;

/// Subscribe to `SelectionsChanged` and spawn the notification task. The task
/// coalesces bursts and broadcasts notification frames through `notify_tx`.
pub fn spawn_notifier(
    bus: &Arc<EventBus>,
    notify_tx: broadcast::Sender<String>,
    cache: EditorStateHandle,
    rt: &tokio::runtime::Handle,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    bus.subscribe(
        EventFilter::kinds(vec![EventKind::SelectionsChanged]),
        SubscriptionTarget::Channel(tx),
    );
    rt.spawn(async move {
        let mut last_active: Option<DocumentId> = None;
        while let Some(first) = rx.recv().await {
            // Coalesce a burst — keep only the latest selection (cursor moves
            // fire per keystroke; the agent only needs where the cursor is now).
            let mut latest = first;
            while let Ok(next) = rx.try_recv() {
                latest = next;
            }
            let Event::SelectionsChanged { id, selections, .. } = &latest else {
                continue; // the filter guarantees this, but stay total
            };

            // Resolve the buffer's path from the read cache (the same cache the
            // read tools answer from).
            let path = {
                let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
                guard.open_buffers.get(id).and_then(|b| b.path.clone())
            };

            // On an active-buffer change, announce it first.
            if last_active != Some(*id) {
                last_active = Some(*id);
                let _ = notify_tx.send(did_change_active_editor_frame(path.as_deref()));
            }
            let _ = notify_tx.send(selection_changed_frame(selections, path.as_deref()));
        }
    });
}

/// Build the `selection_changed` notification frame (PROVISIONAL shape).
pub fn selection_changed_frame(selections: &SelectionSet, path: Option<&Path>) -> String {
    selection_frame(selections.primary(), path)
}

/// The frames a newly connected agent is sent once it has initialized: the
/// active editor and its selection as they are now. The notifier announces
/// an active editor only when it changes, so an agent that connects while
/// the user stays in one buffer would otherwise not know which file is open
/// until they moved to another. Empty when no buffer has been active.
pub fn current_state_frames(editor: &lattice_agent::EditorAccess) -> Vec<String> {
    let Some(info) = editor.current_selection() else {
        return Vec::new();
    };
    let path = info.file_path.as_deref();
    let mut frames = vec![did_change_active_editor_frame(path)];
    if let Some(sel) = info.selection {
        frames.push(selection_frame(&sel, path));
    }
    frames
}

fn selection_frame(sel: &lattice_protocol::Selection, path: Option<&Path>) -> String {
    let (start, end) = ordered(sel.anchor, sel.head);
    json!({
        "jsonrpc": "2.0",
        "method": "selection_changed",
        "params": {
            "filePath": path.map(display),
            "selection": {
                "start": { "line": start.line, "character": start.byte },
                "end": { "line": end.line, "character": end.byte },
                "isEmpty": start == end,
            }
        }
    })
    .to_string()
}

/// Build the `didChangeActiveEditor` notification frame (PROVISIONAL shape).
pub fn did_change_active_editor_frame(path: Option<&Path>) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": "didChangeActiveEditor",
        "params": { "filePath": path.map(display) }
    })
    .to_string()
}

/// Build the `at_mentioned` notification frame (PROVISIONAL shape) — the user
/// pushing the current file + selected line range into the agent's context via
/// `:claude-send` / `@`.
pub fn at_mentioned_frame(selections: &SelectionSet, path: Option<&Path>) -> String {
    let sel = selections.primary();
    let (start, end) = ordered(sel.anchor, sel.head);
    at_mentioned_lines_frame(path, start.line, end.line)
}

/// The `at_mentioned` frame for lines `start..=end` (0-based), as a ranged
/// `:'<,'>claude-send` names them.
pub fn at_mentioned_lines_frame(path: Option<&Path>, start: u32, end: u32) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": "at_mentioned",
        "params": {
            "filePath": path.map(display),
            "lineStart": start,
            "lineEnd": end,
        }
    })
    .to_string()
}

fn display(p: &Path) -> String {
    p.display().to_string()
}

/// Order two positions so `start <= end` (a selection may be anchored either
/// way — the head can sit before the anchor).
fn ordered(a: Position, b: Position) -> (Position, Position) {
    if (a.line, a.byte) <= (b.line, b.byte) {
        (a, b)
    } else {
        (b, a)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn sel(anchor: (u32, u32), head: (u32, u32)) -> SelectionSet {
        SelectionSet::single(lattice_protocol::selection::Selection {
            anchor: Position {
                line: anchor.0,
                byte: anchor.1,
            },
            head: Position {
                line: head.0,
                byte: head.1,
            },
            visual: None,
        })
    }

    #[test]
    fn selection_changed_frame_carries_method_path_and_ordered_range() {
        let frame = selection_changed_frame(&sel((1, 2), (3, 4)), Some(Path::new("/work/a.rs")));
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(v["method"], "selection_changed");
        assert_eq!(v["params"]["filePath"], "/work/a.rs");
        assert_eq!(v["params"]["selection"]["start"]["line"], 1);
        assert_eq!(v["params"]["selection"]["end"]["line"], 3);
        assert_eq!(v["params"]["selection"]["isEmpty"], false);
    }

    #[test]
    fn selection_range_is_ordered_even_when_anchored_backwards() {
        // head before anchor → start must still be the earlier position.
        let frame = selection_changed_frame(&sel((5, 0), (2, 0)), None);
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(v["params"]["selection"]["start"]["line"], 2);
        assert_eq!(v["params"]["selection"]["end"]["line"], 5);
        assert_eq!(v["params"]["filePath"], serde_json::Value::Null);
    }

    #[test]
    fn empty_selection_is_flagged() {
        let frame = selection_changed_frame(&sel((1, 1), (1, 1)), None);
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(v["params"]["selection"]["isEmpty"], true);
    }

    #[test]
    fn did_change_active_editor_frame_carries_method_and_path() {
        let frame = did_change_active_editor_frame(Some(Path::new("/work/b.rs")));
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(v["method"], "didChangeActiveEditor");
        assert_eq!(v["params"]["filePath"], "/work/b.rs");
    }

    #[test]
    fn at_mentioned_frame_carries_method_path_and_line_range() {
        let frame = at_mentioned_frame(&sel((2, 0), (6, 0)), Some(Path::new("/work/c.rs")));
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(v["method"], "at_mentioned");
        assert_eq!(v["params"]["filePath"], "/work/c.rs");
        assert_eq!(v["params"]["lineStart"], 2);
        assert_eq!(v["params"]["lineEnd"], 6);
    }
}
