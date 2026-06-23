//! IDE-protocol I2.2: the five read tools.
//!
//! Crate-owned logic. Each tool composes *generic* services — the
//! crate-owned read cache ([`crate::snapshot`]), the generic `BufferStore`
//! (on-demand text / dirty), the generic `DiagnosticsQuery` (lattice-lsp),
//! and the server's workspace config — into an MCP result `Value`. No host
//! claude-code trait is involved.
//!
//! Shape: pure *result-builders* (`*_result`, unit-tested with plain data)
//! plus thin tool entry points (`get_*` / `check_*`) that fetch from a
//! [`ReadContext`] and call the builders. Reads run on the WS task off the
//! editor thread — `BufferStore::handle_for(id)` → `Document` snapshot reads
//! are wait-free `ArcSwap` loads, and the cache is a brief `Mutex`.
//!
//! Result JSON shapes are PROVISIONAL (like the lockfile schema) until
//! validated against a live `claude` CLI in the I0–I2 walking skeleton.

use std::path::Path;

use serde_json::{Value, json};

use lattice_protocol::ids::DocumentId;
use lattice_protocol::{Position, Selection};

use crate::snapshot::ReadStateHandle;

/// The generic services the read tools consume. Built once at server spawn
/// from boot-provided handles and held behind an `Arc` in the dispatch
/// context. Any field may be absent (headless / test harness), in which
/// case the dependent tool degrades to an empty result, never an error.
pub struct ReadContext {
    /// Crate-owned read-state cache (open set + active selection).
    pub cache: ReadStateHandle,
    /// Generic buffer-store service for on-demand text / dirty.
    pub buffer_store: Option<lattice_mode::BufferStoreHandle>,
    /// Generic diagnostics query (lattice-lsp) for `getDiagnostics`.
    pub diagnostics: Option<lattice_lsp::modes::DiagnosticsQueryHandle>,
    /// Workspace folders from the server config.
    pub workspace_folders: Vec<String>,
}

/// `DocumentId` → the `BufferStore`'s core `BufferId` (same underlying id,
/// distinct newtypes — mirrors `HostDiagnosticsQuery`).
fn core_id(id: DocumentId) -> lattice_core::BufferId {
    lattice_core::BufferId(id.raw() as u32)
}

fn path_to_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// `character` carries the byte offset within the line — lattice's
/// `Position` is byte-based. PROVISIONAL vs the VS Code UTF-16 contract.
fn pos_json(p: Position) -> Value {
    json!({ "line": p.line, "character": p.byte })
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

/// `(start, end)` in document order.
fn ordered(sel: &Selection) -> (Position, Position) {
    if sel.anchor <= sel.head {
        (sel.anchor, sel.head)
    } else {
        (sel.head, sel.anchor)
    }
}

// ---- pure result builders -------------------------------------------------

/// `getCurrentSelection` result. `None` selection → an empty result (the
/// agent's "nothing selected" case), not an error.
pub fn selection_result(
    file_path: Option<&Path>,
    selected_text: &str,
    selection: Option<&Selection>,
) -> Value {
    let Some(sel) = selection else {
        return json!({ "text": "", "filePath": Value::Null, "selection": Value::Null });
    };
    let (start, end) = ordered(sel);
    json!({
        "text": selected_text,
        "filePath": file_path.map(|p| p.display().to_string()),
        "fileUrl": file_path.map(path_to_uri),
        "selection": {
            "start": pos_json(start),
            "end": pos_json(end),
            "isEmpty": sel.is_cursor(),
        }
    })
}

/// `getOpenEditors` result from `(filePath, isActive)` rows (file buffers
/// only; synthetic / unsaved buffers are excluded by the caller).
pub fn open_editors_result(editors: &[(String, bool)]) -> Value {
    let tabs: Vec<Value> = editors
        .iter()
        .map(|(path, active)| {
            json!({
                "filePath": path,
                "uri": format!("file://{path}"),
                "isActive": active,
            })
        })
        .collect();
    json!({ "editors": tabs })
}

/// `getWorkspaceFolders` result.
pub fn workspace_folders_result(folders: &[String]) -> Value {
    let list: Vec<Value> = folders
        .iter()
        .map(|f| {
            json!({
                "path": f,
                "uri": format!("file://{f}"),
                "name": Path::new(f).file_name().and_then(|n| n.to_str()).unwrap_or(f),
            })
        })
        .collect();
    json!({ "folders": list })
}

/// `getDiagnostics` result. Each `(uri, diagnostics)` group serializes its
/// diagnostics in LSP wire shape (`lsp_types::Diagnostic` is `Serialize`).
pub fn diagnostics_result(files: &[(String, Vec<lattice_lsp::Diagnostic>)]) -> Value {
    let out: Vec<Value> = files
        .iter()
        .map(|(uri, diags)| {
            json!({
                "uri": uri,
                "diagnostics": serde_json::to_value(diags).unwrap_or_else(|_| json!([])),
            })
        })
        .collect();
    json!({ "diagnostics": out })
}

/// `checkDocumentDirty` result.
pub fn dirty_result(is_dirty: bool) -> Value {
    json!({ "isDirty": is_dirty })
}

// ---- tool entry points ----------------------------------------------------

/// `getCurrentSelection`: the active buffer's path + selection (+ selected
/// text via the buffer store, when available).
pub fn get_current_selection(ctx: &ReadContext) -> Value {
    let cache = ctx.cache.lock().unwrap_or_else(|e| e.into_inner());
    let Some(active) = cache.active.as_ref() else {
        return selection_result(None, "", None);
    };
    let path = cache
        .open_buffers
        .get(&active.buffer)
        .and_then(|b| b.path.clone());
    let sel = *active.selections.primary();
    let selected_text = ctx
        .buffer_store
        .as_ref()
        .and_then(|bs| bs.handle_for(core_id(active.buffer)))
        .map(|doc| slice_selection(&doc.text(), &sel))
        .unwrap_or_default();
    selection_result(path.as_deref(), &selected_text, Some(&sel))
}

/// `getOpenEditors`: the open file-editor buffers (skips synthetic / unsaved
/// buffers with no path).
pub fn get_open_editors(ctx: &ReadContext) -> Value {
    let cache = ctx.cache.lock().unwrap_or_else(|e| e.into_inner());
    let active = cache.active.as_ref().map(|a| a.buffer);
    let editors: Vec<(String, bool)> = cache
        .open_buffers
        .iter()
        .filter_map(|(id, b)| {
            b.path
                .as_ref()
                .map(|p| (p.display().to_string(), Some(*id) == active))
        })
        .collect();
    open_editors_result(&editors)
}

/// `getWorkspaceFolders`: from the server config.
pub fn get_workspace_folders(ctx: &ReadContext) -> Value {
    workspace_folders_result(&ctx.workspace_folders)
}

/// `getDiagnostics`: all diagnostics, or just the requested `uri` argument.
pub fn get_diagnostics(ctx: &ReadContext, arguments: &Value) -> Value {
    let Some(dq) = ctx.diagnostics.as_ref() else {
        return diagnostics_result(&[]);
    };
    let files: Vec<(String, Vec<lattice_lsp::Diagnostic>)> =
        match arguments.get("uri").and_then(|v| v.as_str()) {
            Some(uri) => vec![(uri.to_string(), dq.for_uri(uri))],
            None => dq
                .uris_with_diagnostics()
                .into_iter()
                .map(|uri| {
                    let diags = dq.for_uri(&uri);
                    (uri, diags)
                })
                .collect(),
        };
    diagnostics_result(&files)
}

/// `checkDocumentDirty`: dirty flag for the `filePath` argument. Unknown /
/// absent path → `isDirty: false` (not an error).
pub fn check_document_dirty(ctx: &ReadContext, arguments: &Value) -> Value {
    let Some(path) = arguments.get("filePath").and_then(|v| v.as_str()) else {
        return dirty_result(false);
    };
    let id = {
        let cache = ctx.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .open_buffers
            .iter()
            .find(|(_, b)| b.path.as_deref() == Some(Path::new(path)))
            .map(|(id, _)| *id)
    };
    let dirty = id
        .and_then(|id| {
            ctx.buffer_store
                .as_ref()
                .and_then(|bs| bs.handle_for(core_id(id)))
        })
        .map(|doc| doc.dirty())
        .unwrap_or(false);
    dirty_result(dirty)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::snapshot::ClaudeCodeReadState;
    use lattice_protocol::Event;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn ctx_with(state: ClaudeCodeReadState, workspace: Vec<String>) -> ReadContext {
        ReadContext {
            cache: Arc::new(Mutex::new(state)),
            buffer_store: None,
            diagnostics: None,
            workspace_folders: workspace,
        }
    }

    #[test]
    fn selection_result_none_is_empty_not_error() {
        let v = selection_result(None, "", None);
        assert_eq!(v["text"], "");
        assert!(v["selection"].is_null());
    }

    #[test]
    fn selection_result_orders_start_before_end() {
        let sel = Selection {
            anchor: Position::new(5, 2),
            head: Position::new(1, 0),
            visual: None,
        };
        let v = selection_result(Some(Path::new("/a.rs")), "x", Some(&sel));
        assert_eq!(v["selection"]["start"]["line"], 1);
        assert_eq!(v["selection"]["end"]["line"], 5);
        assert_eq!(v["filePath"], "/a.rs");
        assert_eq!(v["fileUrl"], "file:///a.rs");
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

    #[test]
    fn open_editors_lists_only_file_buffers_and_marks_active() {
        let mut s = ClaudeCodeReadState::default();
        s.apply_event(&Event::DocumentOpened {
            id: DocumentId::new(1),
            path: Some(PathBuf::from("/a.rs")),
            version: 1,
            text: String::new(),
        });
        s.apply_event(&Event::DocumentOpened {
            id: DocumentId::new(2),
            path: None,
            version: 1,
            text: String::new(),
        });
        s.apply_event(&Event::SelectionsChanged {
            id: DocumentId::new(1),
            version: 2,
            selections: lattice_protocol::SelectionSet::default(),
        });
        let v = get_open_editors(&ctx_with(s, vec![]));
        let editors = v["editors"].as_array().unwrap();
        assert_eq!(editors.len(), 1, "synthetic (no-path) buffer excluded");
        assert_eq!(editors[0]["filePath"], "/a.rs");
        assert_eq!(editors[0]["isActive"], true);
    }

    #[test]
    fn workspace_folders_from_config() {
        let v = get_workspace_folders(&ctx_with(
            ClaudeCodeReadState::default(),
            vec!["/work/project".to_string()],
        ));
        let folders = v["folders"].as_array().unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0]["path"], "/work/project");
        assert_eq!(folders[0]["name"], "project");
    }

    #[test]
    fn diagnostics_with_no_service_is_empty_not_error() {
        let v = get_diagnostics(&ctx_with(ClaudeCodeReadState::default(), vec![]), &json!({}));
        assert_eq!(v["diagnostics"].as_array().map(|a| a.len()), Some(0));
    }

    #[test]
    fn check_dirty_unknown_path_is_false() {
        let v = check_document_dirty(
            &ctx_with(ClaudeCodeReadState::default(), vec![]),
            &json!({ "filePath": "/nope.rs" }),
        );
        assert_eq!(v["isDirty"], false);
    }

    #[test]
    fn get_current_selection_no_active_is_empty() {
        let v = get_current_selection(&ctx_with(ClaudeCodeReadState::default(), vec![]));
        assert_eq!(v["text"], "");
        assert!(v["selection"].is_null());
    }
}
