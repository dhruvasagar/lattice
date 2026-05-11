//! First-party picker source generators that live in the host
//! crate.
//!
//! Each source's state is reachable from `App` -- they don't
//! need feature-crate facades. Sources whose state lives in a
//! dedicated crate (LSP-flavored sources, snippets) implement
//! `PickerSourceGenerator` in their own crate instead.
//!
//! Slice 13bc: `files`, `recent`, `buffers`. Future slices add
//! `lines`, `marks`, `registers`, `jumps`, `commands`, `grep`.

use std::sync::Arc;

use lattice_completion::{CandidateKind, RawCandidate};
use lattice_picker::{
    PickerAcceptOutcome, PickerContext, PickerInitResult, PickerSourceGenerator,
    PickerSourceSpec, RoutingPayload, SourceResult,
};

/// `:picker files [root]`. Walks `root` (or the workspace
/// root from the context) and emits one row per regular file
/// under the standard ignore set (`.git`, `target`,
/// `node_modules`, `dist`, `.cache`). Capped at
/// `FILE_PICKER_MAX_ENTRIES` (5000) -- larger workspaces fall
/// back to `:picker grep`.
pub struct FilesSource {
    pub spec: PickerSourceSpec,
}

impl FilesSource {
    pub fn new() -> Self {
        use lattice_grammar::args::{ArgDefault, ArgKind, ArgSpec};
        Self {
            spec: PickerSourceSpec {
                id: "files",
                doc: "Workspace file picker. Walks the current root (or supplied path) and emits one row per regular file.",
                args_hint: "[root]",
                args_schema: vec![ArgSpec {
                    name: "root",
                    kind: ArgKind::String,
                    doc: "Directory to walk. Absent = current document's parent / cwd.",
                    prompt: "root:",
                    default: ArgDefault::None,
                    completion: Some("gen:files"),
                }],
            },
        }
    }
}

impl Default for FilesSource {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerSourceGenerator for FilesSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(
        &self,
        ctx: &PickerContext<'_>,
        args: &[String],
    ) -> SourceResult<PickerInitResult> {
        let root: std::path::PathBuf = args
            .first()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| ctx.workspace_root.to_path_buf());
        let canonical_root = std::fs::canonicalize(&root).unwrap_or(root.clone());
        let entries = crate::app::picker::walk_files_for_picker(&canonical_root);
        if entries.is_empty() {
            return Err(format!(
                "files: no files under {}",
                canonical_root.display()
            ));
        }
        let pairs = entries
            .into_iter()
            .map(|abs| {
                let rel = abs
                    .strip_prefix(&canonical_root)
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|_| abs.clone());
                let display = rel.display().to_string();
                let cand = RawCandidate::plain(display, CandidateKind::Plain);
                (cand, RoutingPayload::OpenFile { path: abs })
            })
            .collect();
        Ok(PickerInitResult::Inline(pairs))
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        match routing {
            RoutingPayload::OpenFile { path } => {
                Ok(PickerAcceptOutcome::OpenFile { path: path.clone() })
            }
            other => Err(format!("files: unexpected routing payload {other:?}")),
        }
    }
}

/// `:picker recent`. Walks `ctx.recent_files` (MRU, newest
/// first) and emits one row per path. Empty MRU returns
/// `Err("no recent files")` which the host echoes.
pub struct RecentFilesSource {
    pub spec: PickerSourceSpec,
}

impl RecentFilesSource {
    pub fn new() -> Self {
        Self {
            spec: PickerSourceSpec::no_args(
                "recent",
                "Recently-edited files (MRU). Walks `App.recent_files`; accept edits the chosen path.",
            ),
        }
    }
}

impl Default for RecentFilesSource {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerSourceGenerator for RecentFilesSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(
        &self,
        ctx: &PickerContext<'_>,
        _args: &[String],
    ) -> SourceResult<PickerInitResult> {
        if ctx.recent_files.is_empty() {
            return Err("no recent files".into());
        }
        let pairs = ctx
            .recent_files
            .iter()
            .map(|p| {
                let display = p.display().to_string();
                let cand = RawCandidate::plain(display, CandidateKind::Plain);
                (cand, RoutingPayload::OpenFile { path: p.clone() })
            })
            .collect();
        Ok(PickerInitResult::Inline(pairs))
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        match routing {
            RoutingPayload::OpenFile { path } => {
                Ok(PickerAcceptOutcome::OpenFile { path: path.clone() })
            }
            other => Err(format!("recent: unexpected routing payload {other:?}")),
        }
    }
}

/// `:picker buffers`. Walks `ctx.buffers` and emits one row
/// per registered buffer, with `(current)` marginalia on the
/// active one. Active buffer floats to the bottom of the
/// list so the alternate-buffer convention (`<C-^>`-style)
/// keeps working: the initial selection lands on the
/// alternate, not on the buffer the user already sees.
pub struct BuffersSource {
    pub spec: PickerSourceSpec,
}

impl BuffersSource {
    pub fn new() -> Self {
        Self {
            spec: PickerSourceSpec::no_args(
                "buffers",
                "Live buffer switcher. Walks every entry in BufferRegistry; accept activates the chosen buffer.",
            ),
        }
    }
}

impl Default for BuffersSource {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerSourceGenerator for BuffersSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(
        &self,
        ctx: &PickerContext<'_>,
        _args: &[String],
    ) -> SourceResult<PickerInitResult> {
        let active = ctx.active_buffer.buffer_id;
        // Float the active buffer to the bottom of the list
        // so the initial selection lands on the alternate.
        let mut entries: Vec<&lattice_picker::BufferEntry> = ctx.buffers.iter().collect();
        entries.sort_by_key(|e| (e.id == active, e.id));
        let pairs = entries
            .into_iter()
            .map(|e| {
                let active_marker = if e.id == active { " (current)" } else { "" };
                let path_display = e
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| e.title.clone());
                let dirty = if e.dirty { " [+]" } else { "" };
                let display = format!(
                    "#{:<3} {path_display}{dirty}  {}{active_marker}",
                    e.id, e.kind_label,
                );
                let mut cand = RawCandidate::plain(
                    format!("#{}", e.id),
                    CandidateKind::Buffer,
                );
                cand.display = display;
                (cand, RoutingPayload::Buffer { id: e.id })
            })
            .collect();
        Ok(PickerInitResult::Inline(pairs))
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        match routing {
            RoutingPayload::Buffer { id } => {
                Ok(PickerAcceptOutcome::SwitchBuffer { buffer_id: *id })
            }
            other => Err(format!("buffers: unexpected routing payload {other:?}")),
        }
    }
}

/// Convenience: build the three first-party source
/// generators as `Arc<dyn PickerSourceGenerator>` ready to
/// register against a `PickerRegistry`. Used by `App::new` to
/// boot the registry.
pub fn first_party_generators() -> Vec<Arc<dyn PickerSourceGenerator>> {
    vec![
        Arc::new(FilesSource::new()),
        Arc::new(RecentFilesSource::new()),
        Arc::new(BuffersSource::new()),
    ]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::app::test_helpers::app_with;

    /// Files source emits `OpenFile { path }` routing
    /// payloads pointing under the supplied root.
    #[test]
    fn files_source_inline_init_emits_open_file_routing() {
        let tmp = std::env::temp_dir()
            .join(format!("lattice-files-src-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.rs"), "").unwrap();
        std::fs::write(tmp.join("b.rs"), "").unwrap();
        let app = app_with("hi\n", 5);
        let snap = app.document.snapshot();
        let ctx = app.build_picker_context(&snap);
        let source = FilesSource::new();
        let result = source
            .init(&ctx, std::slice::from_ref(&tmp.display().to_string()))
            .expect("inline result");
        match result {
            PickerInitResult::Inline(pairs) => {
                assert_eq!(pairs.len(), 2);
                for (_cand, routing) in &pairs {
                    match routing {
                        RoutingPayload::OpenFile { path } => {
                            assert!(path.starts_with(std::fs::canonicalize(&tmp).unwrap()));
                        }
                        other => panic!("expected OpenFile, got {other:?}"),
                    }
                }
            }
            other => panic!("expected Inline, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Empty workspace makes the files source's init return
    /// `Err("files: no files under ...")` which the host
    /// echoes verbatim.
    #[test]
    fn files_source_empty_root_errors() {
        let tmp = std::env::temp_dir()
            .join(format!("lattice-files-src-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let app = app_with("hi\n", 5);
        let snap = app.document.snapshot();
        let ctx = app.build_picker_context(&snap);
        let source = FilesSource::new();
        let err = source
            .init(&ctx, std::slice::from_ref(&tmp.display().to_string()))
            .unwrap_err();
        assert!(err.starts_with("files: no files under"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Recent source: empty MRU returns `Err("no recent files")`.
    #[test]
    fn recent_source_empty_mru_errors() {
        let app = app_with("hi\n", 5);
        let snap = app.document.snapshot();
        let ctx = app.build_picker_context(&snap);
        let source = RecentFilesSource::new();
        let err = source.init(&ctx, &[]).unwrap_err();
        assert_eq!(err, "no recent files");
    }

    /// Buffers source emits one row per registry entry; the
    /// active buffer carries the `(current)` marker and floats
    /// to the bottom.
    #[test]
    fn buffers_source_inline_init_floats_active_to_bottom() {
        let app = app_with("hi\n", 5);
        let snap = app.document.snapshot();
        let ctx = app.build_picker_context(&snap);
        let source = BuffersSource::new();
        let result = source.init(&ctx, &[]).expect("inline");
        let PickerInitResult::Inline(pairs) = result else {
            panic!("expected Inline");
        };
        assert!(!pairs.is_empty());
        // Last row is the active buffer (floated to bottom).
        let last = pairs.last().unwrap();
        match &last.1 {
            RoutingPayload::Buffer { id } => assert_eq!(*id, ctx.active_buffer.buffer_id),
            other => panic!("expected Buffer routing, got {other:?}"),
        }
        assert!(
            last.0.display.contains("(current)"),
            "active row missing (current) marker: {}",
            last.0.display
        );
    }

    #[test]
    fn first_party_generators_returns_three_sources() {
        let gens = first_party_generators();
        let ids: Vec<&'static str> = gens.iter().map(|g| g.spec().id).collect();
        assert_eq!(ids, vec!["files", "recent", "buffers"]);
    }
}
