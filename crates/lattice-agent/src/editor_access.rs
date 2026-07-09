//! AG-2a: the concrete editor-capability handle for the read surface.
//!
//! `EditorAccess` composes *generic* services — the crate-owned
//! [`EditorStateCache`] and the generic `BufferStore` (on-demand text /
//! dirty) — into Rust-typed answers for `current_selection` /
//! `open_editors` / `workspace_folders` / `document_dirty`. It carries no
//! wire-protocol shape (no JSON, no MCP envelope): that translation is each
//! adapter's job (`lattice_ai::mcp::reads`, and `lattice-ai`'s ACP
//! mapping).
//!
//! Not a trait — one implementation exists. Tests construct it over
//! in-memory seams (an `EditorStateCache` built directly from events),
//! mirroring the pre-port `ctx_with` test helper.

use std::path::{Path, PathBuf};
use std::time::Duration;

use lattice_grammar::Utf16Pos;
use lattice_mode::inbound::InboundBus;
use lattice_protocol::ids::DocumentId;
use lattice_protocol::{Position, Selection};
use tokio::sync::oneshot;

use crate::error::{AgentError, Result};
use crate::state_cache::EditorStateHandle;
use crate::write_bus::{EditorWriteRequest, InboundKind};

/// Backstop so a write can never hang the caller even if the editor never
/// resolves the oneshot (it always should — the drain resolves synchronously
/// once the actor wakes).
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// The active selection plus the text it covers.
#[derive(Debug, Clone)]
pub struct SelectionInfo {
    pub file_path: Option<std::path::PathBuf>,
    pub selected_text: String,
    pub selection: Option<Selection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenEditor {
    pub path: String,
    pub is_active: bool,
}

/// The concrete editor-capability handle. Clone-able (all fields are cheap
/// handles), shared between agent tasks off the editor thread.
#[derive(Clone)]
pub struct EditorAccess {
    cache: EditorStateHandle,
    buffer_store: Option<lattice_mode::BufferStoreHandle>,
    workspace_folders: Vec<String>,
    writes: Option<InboundBus<EditorWriteRequest>>,
}

impl EditorAccess {
    pub fn new(
        cache: EditorStateHandle,
        buffer_store: Option<lattice_mode::BufferStoreHandle>,
        workspace_folders: Vec<String>,
        writes: Option<InboundBus<EditorWriteRequest>>,
    ) -> Self {
        Self {
            cache,
            buffer_store,
            workspace_folders,
            writes,
        }
    }

    /// The active buffer's path + selection + covered text, if any buffer
    /// is active. `None` when nothing is active (the "nothing selected"
    /// case — callers map this to their own empty-result shape).
    pub fn current_selection(&self) -> Option<SelectionInfo> {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let active = cache.active.as_ref()?;
        let path = cache
            .open_buffers
            .get(&active.buffer)
            .and_then(|b| b.path.clone());
        let sel = *active.selections.primary();
        let selected_text = self
            .buffer_store
            .as_ref()
            .and_then(|bs| bs.handle_for(core_id(active.buffer)))
            .map(|doc| slice_selection(&doc.text(), &sel))
            .unwrap_or_default();
        Some(SelectionInfo {
            file_path: path,
            selected_text,
            selection: Some(sel),
        })
    }

    /// The open file-editor buffers (skips synthetic / unsaved buffers with
    /// no path).
    pub fn open_editors(&self) -> Vec<OpenEditor> {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let active = cache.active.as_ref().map(|a| a.buffer);
        cache
            .open_buffers
            .iter()
            .filter_map(|(id, b)| {
                b.path.as_ref().map(|p| OpenEditor {
                    path: p.display().to_string(),
                    is_active: Some(*id) == active,
                })
            })
            .collect()
    }

    pub fn workspace_folders(&self) -> &[String] {
        &self.workspace_folders
    }

    /// Dirty flag for `path`. An unknown path (not open, or no buffer-store
    /// service) is `false`, never an error.
    pub fn document_dirty(&self, path: &str) -> bool {
        let id = {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache
                .open_buffers
                .iter()
                .find(|(_, b)| b.path.as_deref() == Some(Path::new(path)))
                .map(|(id, _)| *id)
        };
        id.and_then(|id| {
            self.buffer_store
                .as_ref()
                .and_then(|bs| bs.handle_for(core_id(id)))
        })
        .map(|doc| doc.dirty())
        .unwrap_or(false)
    }

    /// Open `path`, optionally forcing the cursor to `column` (a UTF-16
    /// position; `None` opens without moving the cursor — re-opening an
    /// already-open file keeps its position).
    pub async fn open_file(&self, path: PathBuf, column: Option<Utf16Pos>) -> Result<()> {
        self.run_write(InboundKind::OpenFile { path, column }).await
    }

    /// Save the document for `path` (only when it is the active buffer —
    /// option C, the I3 limitation).
    pub async fn save_document(&self, path: PathBuf) -> Result<()> {
        self.run_write(InboundKind::SaveDocument { path }).await
    }

    /// Close the tab named `tab_name`, scoped to connection
    /// `origin_session` — the host rejects that connection's diff
    /// session(s), falling back to the active-buffer file-close.
    pub async fn close_tab(&self, origin_session: u64, tab_name: String) -> Result<()> {
        self.run_write(InboundKind::CloseTab {
            origin_session,
            tab_name,
        })
        .await
    }

    /// Reject every programmatic diff connection `origin_session` opened.
    pub async fn close_session_diffs(&self, origin_session: u64) -> Result<()> {
        self.run_write(InboundKind::CloseAllDiffTabs { origin_session })
            .await
    }

    /// Send `kind` on the write bus, await the reply, and map it onto
    /// [`AgentError`]. The single graceful path for all four write methods —
    /// a missing bus, a dropped receiver, a timeout, and an `ok: false` reply
    /// all map to a distinct `AgentError` carrying the ORIGINAL message text
    /// (never re-wrapped through `Display`, so adapters that emit
    /// `e.to_string()` don't double-prefix).
    async fn run_write(&self, kind: InboundKind) -> Result<()> {
        let Some(bus) = &self.writes else {
            return Err(AgentError::Bus(
                "write unavailable: IDE server not fully initialized".to_string(),
            ));
        };
        let (tx, rx) = oneshot::channel();
        if bus.send(EditorWriteRequest { kind, response: tx }).is_err() {
            // Receiver dropped — the editor/server is gone.
            return Err(AgentError::Bus(
                "write failed: editor not reachable".to_string(),
            ));
        }
        match tokio::time::timeout(WRITE_TIMEOUT, rx).await {
            Ok(Ok(reply)) if reply.ok => Ok(()),
            Ok(Ok(reply)) => Err(AgentError::Io(reply.message.unwrap_or_default())),
            // Sender dropped without replying, or timed out.
            _ => Err(AgentError::Io(
                "write failed: editor did not respond".to_string(),
            )),
        }
    }
}

/// `DocumentId` → the `BufferStore`'s core `BufferId` (same underlying id,
/// distinct newtypes — mirrors `HostDiagnosticsQuery`).
fn core_id(id: DocumentId) -> lattice_core::BufferId {
    lattice_core::BufferId(id.raw() as u32)
}

/// A `file://` URI for `path`. Used by adapters building a selection result
/// (`lattice_ai::mcp::reads::selection_result`) alongside `file_path`.
pub fn path_to_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// Absolute byte offset of `pos` within `text` (clamped to `text.len()`).
fn abs_offset(text: &str, pos: Position) -> usize {
    let mut offset = 0usize;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        if i as u32 == pos.line {
            return (offset + pos.byte as usize).min(text.len());
        }
        offset += line.len();
    }
    (offset + pos.byte as usize).min(text.len())
}

/// The text covered by `sel` within `text` (empty for a cursor). Defensive:
/// a non-char-boundary range yields `""` rather than a panic.
fn slice_selection(text: &str, sel: &Selection) -> String {
    if sel.is_cursor() {
        return String::new();
    }
    let (start, end) = ordered(sel);
    let (s, e) = (abs_offset(text, start), abs_offset(text, end));
    text.get(s..e).unwrap_or("").to_string()
}

/// `(start, end)` in document order. Used by adapters building a selection
/// result (`lattice_ai::mcp::reads::selection_result`) as well as
/// internally by [`slice_selection`].
pub fn ordered(sel: &Selection) -> (Position, Position) {
    if sel.anchor <= sel.head {
        (sel.anchor, sel.head)
    } else {
        (sel.head, sel.anchor)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::state_cache::EditorStateCache;
    use lattice_protocol::Event;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn access(state: EditorStateCache, workspace: Vec<String>) -> EditorAccess {
        EditorAccess::new(Arc::new(Mutex::new(state)), None, workspace, None)
    }

    #[test]
    fn current_selection_none_when_no_active_buffer() {
        let ea = access(EditorStateCache::default(), vec![]);
        assert!(ea.current_selection().is_none());
    }

    #[test]
    fn current_selection_some_when_active_buffer_present() {
        let mut s = EditorStateCache::default();
        s.apply_event(&Event::DocumentOpened {
            id: DocumentId::new(1),
            path: Some(PathBuf::from("/a.rs")),
            version: 1,
            text: String::new(),
        });
        s.apply_event(&Event::SelectionsChanged {
            id: DocumentId::new(1),
            version: 2,
            selections: lattice_protocol::SelectionSet::default(),
        });
        let ea = access(s, vec![]);
        let sel = ea.current_selection().expect("active buffer set");
        assert_eq!(sel.file_path.as_deref(), Some(Path::new("/a.rs")));
        assert!(sel.selection.is_some());
        // No buffer-store service wired — selected text degrades to empty
        // rather than erroring.
        assert_eq!(sel.selected_text, "");
    }

    #[test]
    fn document_dirty_false_when_path_not_open() {
        let ea = access(EditorStateCache::default(), vec![]);
        assert!(!ea.document_dirty("/nope.rs"));
    }

    #[test]
    fn document_dirty_false_when_open_but_no_buffer_store() {
        let mut s = EditorStateCache::default();
        s.apply_event(&Event::DocumentOpened {
            id: DocumentId::new(1),
            path: Some(PathBuf::from("/a.rs")),
            version: 1,
            text: String::new(),
        });
        let ea = access(s, vec![]);
        // The path is known to the cache, but with no buffer-store service
        // wired there is no dirty flag to read — degrades to false.
        assert!(!ea.document_dirty("/a.rs"));
    }

    #[test]
    fn slice_selection_extracts_between_positions() {
        let text = "hello\nworld\n";
        let sel = Selection {
            anchor: Position::new(0, 0),
            head: Position::new(0, 5),
            visual: None,
        };
        assert_eq!(slice_selection(text, &sel), "hello");
        let cur = Selection::cursor(Position::new(1, 2));
        assert_eq!(slice_selection(text, &cur), "");
    }

    // --- write half: the two failure paths with no direct coverage before
    // the port move (AG-2b). ---

    fn access_with_writes(writes: InboundBus<EditorWriteRequest>) -> EditorAccess {
        EditorAccess::new(
            Arc::new(Mutex::new(EditorStateCache::default())),
            None,
            vec![],
            Some(writes),
        )
    }

    #[tokio::test]
    async fn open_file_with_dropped_receiver_is_bus_error() {
        // Build a raw bus and immediately drop the receiver — mirrors the
        // "server stopped" case: nothing is draining the channel.
        let (bus, rx) = lattice_mode::inbound::make_inbound_raw::<EditorWriteRequest>(Arc::new(
            tokio::sync::Notify::new(),
        ));
        drop(rx);
        let ea = access_with_writes(bus);
        let err = ea
            .open_file(PathBuf::from("/a.rs"), None)
            .await
            .expect_err("dropped receiver must fail the write");
        assert!(
            matches!(err, AgentError::Bus(ref m) if m == "write failed: editor not reachable"),
            "expected Bus(\"write failed: editor not reachable\"), got {err:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn open_file_times_out_when_nothing_replies() {
        // A live receiver that never replies — the request sits in the
        // channel forever, so `run_write`'s 5s backstop must fire.
        let (bus, _rx) = lattice_mode::inbound::make_inbound_raw::<EditorWriteRequest>(Arc::new(
            tokio::sync::Notify::new(),
        ));
        let ea = access_with_writes(bus);
        let call = tokio::spawn(async move { ea.open_file(PathBuf::from("/a.rs"), None).await });
        tokio::time::advance(Duration::from_secs(6)).await;
        let err = call
            .await
            .expect("task join")
            .expect_err("no reply within WRITE_TIMEOUT must fail the write");
        assert!(
            matches!(err, AgentError::Io(ref m) if m == "write failed: editor did not respond"),
            "expected Io(\"write failed: editor did not respond\"), got {err:?}"
        );
    }
}
