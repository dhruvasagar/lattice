//! IDE-protocol I2.2: the five read tools.
//!
//! Crate-owned logic. Each tool fetches Rust-typed data from the
//! protocol-neutral [`lattice_agent::EditorAccess`] port (open-buffer set +
//! active selection, on-demand text / dirty via the generic `BufferStore`)
//! plus, for `getDiagnostics` only, the generic `DiagnosticsQuery`
//! (lattice-lsp) — and wraps it into an MCP result `Value`. No host
//! claude-code trait is involved.
//!
//! Shape: pure *result-builders* (`*_result`, unit-tested with plain data)
//! plus thin tool entry points (`get_*` / `check_*`) that fetch from a
//! [`ReadContext`] and call the builders. Reads run on the WS task off the
//! editor thread — `BufferStore::handle_for(id)` → `Document` snapshot reads
//! are wait-free `ArcSwap` loads, and the port's cache is a brief `Mutex`.
//!
//! Result JSON shapes are PROVISIONAL (like the lockfile schema) until
//! validated against a live `claude` CLI in the I0–I2 walking skeleton.

use std::path::Path;

use serde_json::{Value, json};

use lattice_agent::EditorAccess;
use lattice_agent::editor_access::{ordered, path_to_uri};
use lattice_protocol::{Position, Selection};

/// The generic services the read tools consume. Built once at server spawn
/// from boot-provided handles and held behind an `Arc` in the dispatch
/// context. Any field may be absent (headless / test harness), in which
/// case the dependent tool degrades to an empty result, never an error.
#[derive(Clone)]
pub struct ReadContext {
    /// The protocol-neutral editor-read port (open set + active selection +
    /// on-demand text / dirty).
    pub editor: EditorAccess,
    /// Generic diagnostics query (lattice-lsp) for `getDiagnostics`.
    pub diagnostics: Option<lattice_lsp::modes::DiagnosticsQueryHandle>,
}

/// `character` carries the byte offset within the line — lattice's
/// `Position` is byte-based. PROVISIONAL vs the VS Code UTF-16 contract.
fn pos_json(p: Position) -> Value {
    json!({ "line": p.line, "character": p.byte })
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
    match ctx.editor.current_selection() {
        Some(sel) => selection_result(
            sel.file_path.as_deref(),
            &sel.selected_text,
            sel.selection.as_ref(),
        ),
        None => selection_result(None, "", None),
    }
}

/// `getOpenEditors`: the open file-editor buffers (skips synthetic / unsaved
/// buffers with no path).
pub fn get_open_editors(ctx: &ReadContext) -> Value {
    let editors: Vec<(String, bool)> = ctx
        .editor
        .open_editors()
        .into_iter()
        .map(|e| (e.path, e.is_active))
        .collect();
    open_editors_result(&editors)
}

/// `getWorkspaceFolders`: from the server config.
pub fn get_workspace_folders(ctx: &ReadContext) -> Value {
    workspace_folders_result(ctx.editor.workspace_folders())
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
    dirty_result(ctx.editor.document_dirty(path))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_agent::EditorStateCache;
    use lattice_protocol::Event;
    use lattice_protocol::ids::DocumentId;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn ctx_with(state: EditorStateCache, workspace: Vec<String>) -> ReadContext {
        ReadContext {
            editor: EditorAccess::new(Arc::new(Mutex::new(state)), None, workspace, None),
            diagnostics: None,
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
    fn open_editors_lists_only_file_buffers_and_marks_active() {
        let mut s = EditorStateCache::default();
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
            EditorStateCache::default(),
            vec!["/work/project".to_string()],
        ));
        let folders = v["folders"].as_array().unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0]["path"], "/work/project");
        assert_eq!(folders[0]["name"], "project");
    }

    #[test]
    fn diagnostics_with_no_service_is_empty_not_error() {
        let v = get_diagnostics(&ctx_with(EditorStateCache::default(), vec![]), &json!({}));
        assert_eq!(v["diagnostics"].as_array().map(|a| a.len()), Some(0));
    }

    #[test]
    fn check_dirty_unknown_path_is_false() {
        let v = check_document_dirty(
            &ctx_with(EditorStateCache::default(), vec![]),
            &json!({ "filePath": "/nope.rs" }),
        );
        assert_eq!(v["isDirty"], false);
    }

    #[test]
    fn get_current_selection_no_active_is_empty() {
        let v = get_current_selection(&ctx_with(EditorStateCache::default(), vec![]));
        assert_eq!(v["text"], "");
        assert!(v["selection"].is_null());
    }
}
