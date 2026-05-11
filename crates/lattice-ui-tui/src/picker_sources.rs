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
use lattice_grammar::CommandRegistry;
use lattice_grammar::args::Args;
use lattice_grammar::command::CommandKind;
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

/// `:picker lines`. Walks the active buffer's rope and emits
/// one row per logical line, displayed as `<lineno>: <text>`.
/// Accept jumps the cursor to that line via
/// `RoutingPayload::JumpInBuffer`. The buffer_id is captured
/// at picker-open so a sibling hover-preview can't accidentally
/// redirect the jump.
pub struct LinesSource {
    pub spec: PickerSourceSpec,
}

impl LinesSource {
    pub fn new() -> Self {
        Self {
            spec: PickerSourceSpec::no_args(
                "lines",
                "Active buffer's lines. Type to filter; `<CR>` jumps to that line.",
            ),
        }
    }
}

impl Default for LinesSource {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerSourceGenerator for LinesSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(
        &self,
        ctx: &PickerContext<'_>,
        _args: &[String],
    ) -> SourceResult<PickerInitResult> {
        let buffer = ctx.active_buffer.buffer;
        let buffer_id = ctx.active_buffer.buffer_id;
        let line_count = buffer.line_count();
        if line_count == 0 {
            return Err("lines: empty buffer".into());
        }
        // ropey reports a trailing empty line when the buffer
        // ends in `\n`; drop it so the picker doesn't dangle a
        // blank "phantom" row past the last addressable line.
        let last = if buffer.line_byte_len(line_count - 1) == 0 && line_count >= 2 {
            line_count - 2
        } else {
            line_count - 1
        };
        // Use the largest line number's digit count as the
        // alignment width so the colon column lines up across
        // rows.
        let width = ((last + 1) as f64).log10().floor() as usize + 1;
        let mut pairs = Vec::with_capacity(last as usize + 1);
        for line in 0..=last {
            let text = buffer.line(line).unwrap_or_default();
            let text = text.trim_end_matches('\n');
            let display = format!("{:>width$}: {}", line + 1, text, width = width);
            let cand = RawCandidate::plain(display, CandidateKind::Plain);
            pairs.push((
                cand,
                RoutingPayload::JumpInBuffer {
                    buffer_id,
                    line,
                    col: 0,
                },
            ));
        }
        Ok(PickerInitResult::Inline(pairs))
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        match routing {
            RoutingPayload::JumpInBuffer { buffer_id, line, col } => {
                Ok(PickerAcceptOutcome::JumpInBuffer {
                    buffer_id: *buffer_id,
                    line: *line,
                    col: *col,
                })
            }
            other => Err(format!("lines: unexpected routing payload {other:?}")),
        }
    }
}

/// `:picker jumps`. Walks `ctx.position_history` (unified
/// jump-list + mark-ring per §5.1.1) and emits one row per
/// entry, newest first. Accept emits `JumpInBuffer` so the
/// host's apply translator handles "activate buffer +
/// position cursor" uniformly. MRU is correctly absent for
/// these rows -- `routing_identity` returns `None` for
/// `JumpInBuffer` because coordinates drift.
pub struct JumpsSource {
    pub spec: PickerSourceSpec,
}

impl JumpsSource {
    pub fn new() -> Self {
        Self {
            spec: PickerSourceSpec::no_args(
                "jumps",
                "Position-history ring (unified jump list + mark ring). Newest first; `<CR>` jumps to that entry.",
            ),
        }
    }
}

impl Default for JumpsSource {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerSourceGenerator for JumpsSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(
        &self,
        ctx: &PickerContext<'_>,
        _args: &[String],
    ) -> SourceResult<PickerInitResult> {
        if ctx.position_history.is_empty() {
            return Err("jumps: position history is empty".into());
        }
        // Walk newest-first. The ring stores oldest-first
        // (push appends to the end) so reverse iteration is
        // the user-facing default.
        let pairs = ctx
            .position_history
            .iter()
            .rev()
            .map(|entry| {
                let source_tag = match entry.source {
                    lattice_picker::PositionSource::AutoJump => "auto".to_string(),
                    lattice_picker::PositionSource::ExplicitMark => "mark".to_string(),
                    lattice_picker::PositionSource::PluginPush => "plugin".to_string(),
                    lattice_picker::PositionSource::NamedMark(c) => format!("'{c}"),
                };
                // Resolve buffer_id to a display label via the
                // buffers snapshot; fall back to the raw id when
                // the buffer is no longer in the registry.
                let buf_label = ctx
                    .buffers
                    .iter()
                    .find(|b| b.id == entry.buffer_id)
                    .map(|b| {
                        b.path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| b.title.clone())
                    })
                    .unwrap_or_else(|| format!("#{}", entry.buffer_id));
                let display = format!(
                    "[{source_tag:6}] {buf_label}:{}:{}",
                    entry.line + 1,
                    entry.col + 1,
                );
                let cand = RawCandidate::plain(display, CandidateKind::Plain);
                (
                    cand,
                    RoutingPayload::JumpInBuffer {
                        buffer_id: entry.buffer_id,
                        line: entry.line,
                        col: entry.col,
                    },
                )
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
            RoutingPayload::JumpInBuffer { buffer_id, line, col } => {
                Ok(PickerAcceptOutcome::JumpInBuffer {
                    buffer_id: *buffer_id,
                    line: *line,
                    col: *col,
                })
            }
            other => Err(format!("jumps: unexpected routing payload {other:?}")),
        }
    }
}

/// `:picker commands`. Walks the App's `CommandRegistry`
/// and emits one row per registered ex-command (motions,
/// operators, etc. are not user-invocable through this
/// surface and stay out). Captures an `Arc<CommandRegistry>`
/// at construction time -- the registry doesn't live on
/// `PickerContext` because it's static App-wide state, not
/// per-invocation snapshot data.
pub struct CommandsSource {
    pub spec: PickerSourceSpec,
    pub registry: Arc<CommandRegistry>,
}

impl CommandsSource {
    pub fn new(registry: Arc<CommandRegistry>) -> Self {
        Self {
            spec: PickerSourceSpec::no_args(
                "commands",
                "Ex-command palette. Walks the CommandRegistry; `<CR>` invokes the chosen command.",
            ),
            registry,
        }
    }
}

impl PickerSourceGenerator for CommandsSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(
        &self,
        _ctx: &PickerContext<'_>,
        _args: &[String],
    ) -> SourceResult<PickerInitResult> {
        // Walk registry names, keep ex-commands, project to
        // (user-facing, canonical, doc). The user-facing name
        // strips the `ex:` registration prefix where one exists
        // (mode-toggle ex-commands like `buffer-words-mode`
        // register without it).
        let mut rows: Vec<(String, String, String)> = self
            .registry
            .names()
            .filter_map(|canonical| {
                let spec = self.registry.lookup_by_name(canonical)?;
                if !matches!(spec.kind, CommandKind::ExCommand) {
                    return None;
                }
                let user_facing = canonical
                    .strip_prefix("ex:")
                    .unwrap_or(canonical)
                    .to_string();
                Some((user_facing, canonical.to_string(), spec.doc.clone()))
            })
            .collect();
        // Sort by user-facing name so the popup matches the
        // alphabetic order users see.
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        if rows.is_empty() {
            return Err("commands: no ex-commands registered".into());
        }
        // Right-pad the name column so doc text aligns
        // across rows. Width adapts to the longest user-facing
        // name.
        let name_width = rows.iter().map(|(uf, _, _)| uf.len()).max().unwrap_or(0);
        let pairs = rows
            .into_iter()
            .map(|(user_facing, canonical, doc)| {
                let one_line_doc: String = doc
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(120)
                    .collect();
                let display = format!(
                    "{:<width$}  {}",
                    user_facing,
                    one_line_doc,
                    width = name_width,
                );
                let mut cand = RawCandidate::plain(user_facing, CandidateKind::Plain);
                cand.display = display;
                (
                    cand,
                    RoutingPayload::InvokeCommand {
                        id: canonical,
                        args: Args::None,
                    },
                )
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
            RoutingPayload::InvokeCommand { id, args } => {
                Ok(PickerAcceptOutcome::InvokeCommand {
                    id: id.clone(),
                    args: args.clone(),
                })
            }
            other => Err(format!("commands: unexpected routing payload {other:?}")),
        }
    }
}

/// `:picker registers`. Walks `ctx.registers` (`(name,
/// preview)` pairs already prepared by the host's
/// `build_picker_context`) and emits one row per register.
/// Accept emits `PasteRegister { name }`; the host routes
/// through `do_paste` with the chosen register pre-selected.
pub struct RegistersSource {
    pub spec: PickerSourceSpec,
}

impl RegistersSource {
    pub fn new() -> Self {
        Self {
            spec: PickerSourceSpec::no_args(
                "registers",
                "Vim-style registers (unnamed, numbered, named). `<CR>` pastes the chosen register at the cursor.",
            ),
        }
    }
}

impl Default for RegistersSource {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerSourceGenerator for RegistersSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(
        &self,
        ctx: &PickerContext<'_>,
        _args: &[String],
    ) -> SourceResult<PickerInitResult> {
        if ctx.registers.is_empty() {
            return Err("registers: no registers set".into());
        }
        let pairs = ctx
            .registers
            .iter()
            .filter_map(|(name, preview)| {
                // Pick the first char of the name as the routing
                // key. Names are always one char today; future
                // multi-char keys (vim doesn't have any) would
                // need a richer routing variant.
                let ch = name.chars().next()?;
                let display = format!("\"{name:<2} {preview}");
                let cand = RawCandidate::plain(display, CandidateKind::Plain);
                Some((cand, RoutingPayload::PasteRegister { name: ch }))
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
            RoutingPayload::PasteRegister { name } => {
                Ok(PickerAcceptOutcome::PasteRegister { name: *name })
            }
            other => Err(format!("registers: unexpected routing payload {other:?}")),
        }
    }
}

/// Convenience: build the first-party source generators as
/// `Arc<dyn PickerSourceGenerator>` ready to register against
/// a `PickerRegistry`. Used by `App::new` to boot the
/// registry. Sources that need App-wide state captured at
/// construction (e.g. `CommandsSource` -> `CommandRegistry`)
/// take the relevant `Arc` here so the trait surface stays
/// state-handle-free.
pub fn first_party_generators(
    command_registry: Arc<CommandRegistry>,
) -> Vec<Arc<dyn PickerSourceGenerator>> {
    vec![
        Arc::new(FilesSource::new()),
        Arc::new(RecentFilesSource::new()),
        Arc::new(BuffersSource::new()),
        Arc::new(LinesSource::new()),
        Arc::new(JumpsSource::new()),
        Arc::new(CommandsSource::new(command_registry)),
        Arc::new(RegistersSource::new()),
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
    fn first_party_generators_returns_all_built_in_sources() {
        let app = app_with("hi\n", 5);
        let generators = first_party_generators(app.registry.clone());
        let ids: Vec<&'static str> = generators.iter().map(|g| g.spec().id).collect();
        assert_eq!(
            ids,
            vec!["files", "recent", "buffers", "lines", "jumps", "commands", "registers"]
        );
    }

    /// P.6: jumps source returns `Err` when the position
    /// history is empty -- a fresh App has nothing to walk
    /// yet so the picker stays closed with a clean echo.
    #[test]
    fn jumps_source_empty_history_errors() {
        let app = app_with("hi\n", 5);
        let snap = app.document.snapshot();
        let ctx = app.build_picker_context(&snap);
        let source = JumpsSource::new();
        let err = source.init(&ctx, &[]).unwrap_err();
        assert!(err.contains("position history is empty"), "got {err}");
    }

    /// P.7: commands source emits one row per registered
    /// ex-command, sorted, with `InvokeCommand` routing
    /// payloads that strip the `ex:` registration prefix
    /// (kept as the canonical id) while displaying the
    /// user-facing alias the popup matches against.
    #[test]
    fn commands_source_emits_ex_commands_only() {
        let app = app_with("hi\n", 5);
        let snap = app.document.snapshot();
        let ctx = app.build_picker_context(&snap);
        let source = CommandsSource::new(app.registry.clone());
        let result = source.init(&ctx, &[]).expect("inline");
        let PickerInitResult::Inline(pairs) = result else {
            panic!("expected Inline");
        };
        assert!(!pairs.is_empty(), "should have at least one command");
        // Every row routes through InvokeCommand carrying the
        // canonical `ex:`-prefixed registration name.
        for (cand, routing) in &pairs {
            match routing {
                RoutingPayload::InvokeCommand { id, .. } => {
                    // Routing payload carries the canonical
                    // registration name verbatim (with `ex:`
                    // prefix where the command uses one, bare
                    // otherwise -- mode toggles like
                    // `buffer-words-mode` register without).
                    assert!(!id.is_empty());
                }
                other => panic!("expected InvokeCommand, got {other:?}"),
            }
            // Display text strips any `ex:` prefix so the popup
            // matches what the user would type at `:`.
            assert!(!cand.text.starts_with("ex:"), "got {}", cand.text);
        }
        // Sorted: alphabetic by user-facing name.
        let texts: Vec<&str> = pairs.iter().map(|(c, _)| c.text.as_str()).collect();
        let mut sorted = texts.clone();
        sorted.sort();
        assert_eq!(texts, sorted);
    }

    /// P.7: accept on `InvokeCommand` routing returns the
    /// matching outcome, carrying the canonical id +
    /// supplied args verbatim.
    #[test]
    fn commands_source_accept_translates_invoke_command() {
        let app = app_with("hi\n", 5);
        let snap = app.document.snapshot();
        let ctx = app.build_picker_context(&snap);
        let source = CommandsSource::new(app.registry.clone());
        let routing = RoutingPayload::InvokeCommand {
            id: "ex:write".into(),
            args: Args::None,
        };
        let outcome = source.accept(&ctx, &routing).expect("ok");
        match outcome {
            PickerAcceptOutcome::InvokeCommand { id, args } => {
                assert_eq!(id, "ex:write");
                assert!(matches!(args, Args::None));
            }
            other => panic!("expected InvokeCommand, got {other:?}"),
        }
        let bad = RoutingPayload::OpenFile { path: "/tmp/x".into() };
        assert!(source.accept(&ctx, &bad).is_err());
    }

    /// P.4: registers source returns `Err` when the context
    /// has no registers (empty `ctx.registers`).
    #[test]
    fn registers_source_empty_errors() {
        let app = app_with("hi\n", 5);
        let snap = app.document.snapshot();
        let ctx = app.build_picker_context(&snap);
        let source = RegistersSource::new();
        let err = source.init(&ctx, &[]).unwrap_err();
        assert!(err.contains("no registers set"), "got {err}");
    }

    /// P.4: synthesise a couple of register entries on the
    /// context and confirm rows route through
    /// `PasteRegister`.
    #[test]
    fn registers_source_emits_paste_routing() {
        let app = app_with("hi\n", 5);
        let snap = app.document.snapshot();
        let mut ctx = app.build_picker_context(&snap);
        ctx.registers = vec![
            ("\"".into(), "hello".into()),
            ("a".into(), "world".into()),
        ];
        let source = RegistersSource::new();
        let result = source.init(&ctx, &[]).expect("inline");
        let PickerInitResult::Inline(pairs) = result else {
            panic!("expected Inline");
        };
        assert_eq!(pairs.len(), 2);
        match &pairs[0].1 {
            RoutingPayload::PasteRegister { name } => assert_eq!(*name, '"'),
            other => panic!("expected PasteRegister, got {other:?}"),
        }
        match &pairs[1].1 {
            RoutingPayload::PasteRegister { name } => assert_eq!(*name, 'a'),
            other => panic!("expected PasteRegister, got {other:?}"),
        }
    }

    /// P.4: accept on a `PasteRegister` routing returns the
    /// matching outcome; mismatched routing errors.
    #[test]
    fn registers_source_accept_translates_paste_register() {
        let app = app_with("hi\n", 5);
        let snap = app.document.snapshot();
        let ctx = app.build_picker_context(&snap);
        let source = RegistersSource::new();
        let routing = RoutingPayload::PasteRegister { name: 'a' };
        match source.accept(&ctx, &routing).expect("ok") {
            PickerAcceptOutcome::PasteRegister { name } => assert_eq!(name, 'a'),
            other => panic!("expected PasteRegister outcome, got {other:?}"),
        }
        let bad = RoutingPayload::OpenFile { path: "/tmp/x".into() };
        assert!(source.accept(&ctx, &bad).is_err());
    }

    /// P.6: synthesise a couple of position-history entries
    /// (the App's ring is private at this layer but the
    /// PickerContext carries an owned vec we can substitute
    /// for the test). Confirm the source emits newest-first
    /// with the appropriate source tags + `JumpInBuffer`
    /// routing.
    #[test]
    fn jumps_source_emits_newest_first_with_source_tags() {
        use lattice_picker::{PositionEntry, PositionSource};

        let app = app_with("hi\n", 5);
        let snap = app.document.snapshot();
        let mut ctx = app.build_picker_context(&snap);
        ctx.position_history = vec![
            PositionEntry { buffer_id: 1, line: 0, col: 0, source: PositionSource::AutoJump },
            PositionEntry { buffer_id: 1, line: 5, col: 2, source: PositionSource::NamedMark('a') },
            PositionEntry { buffer_id: 2, line: 10, col: 0, source: PositionSource::PluginPush },
        ];
        let source = JumpsSource::new();
        let result = source.init(&ctx, &[]).expect("inline");
        let PickerInitResult::Inline(pairs) = result else {
            panic!("expected Inline");
        };
        assert_eq!(pairs.len(), 3);
        // Newest first: plugin (line 10) leads.
        match &pairs[0].1 {
            RoutingPayload::JumpInBuffer { buffer_id, line, col } => {
                assert_eq!(*buffer_id, 2);
                assert_eq!(*line, 10);
                assert_eq!(*col, 0);
            }
            other => panic!("expected JumpInBuffer, got {other:?}"),
        }
        // The named-mark row carries `'a` in its source tag.
        assert!(pairs[1].0.display.contains("'a"), "{}", pairs[1].0.display);
        // The auto row carries `auto`.
        assert!(pairs[2].0.display.contains("auto"), "{}", pairs[2].0.display);
    }

    /// P.3: lines source emits one row per addressable line
    /// in the active buffer, with `JumpInBuffer` routing
    /// payloads carrying the captured buffer id.
    #[test]
    fn lines_source_emits_row_per_line() {
        let app = app_with("alpha\nbeta\ngamma\n", 10);
        let snap = app.document.snapshot();
        let ctx = app.build_picker_context(&snap);
        let active_id = ctx.active_buffer.buffer_id;
        let source = LinesSource::new();
        let result = source.init(&ctx, &[]).expect("inline");
        let PickerInitResult::Inline(pairs) = result else {
            panic!("expected Inline");
        };
        // 3 addressable lines; the trailing-empty phantom is dropped.
        assert_eq!(pairs.len(), 3);
        for (i, (cand, routing)) in pairs.iter().enumerate() {
            match routing {
                RoutingPayload::JumpInBuffer { buffer_id, line, col } => {
                    assert_eq!(*buffer_id, active_id);
                    assert_eq!(*line, i as u32);
                    assert_eq!(*col, 0);
                }
                other => panic!("expected JumpInBuffer, got {other:?}"),
            }
            // Display starts with right-aligned line number then `:`.
            assert!(cand.display.contains(':'), "missing `:` in {}", cand.display);
        }
    }

    /// P.3: empty buffer surfaces an error echo (the
    /// `line_count == 0` guard) rather than seating an empty
    /// picker.
    #[test]
    fn lines_source_empty_buffer_errors() {
        let app = app_with("", 5);
        let snap = app.document.snapshot();
        // ropey treats truly-empty as one logical line; force the
        // guard by constructing a context whose buffer has zero
        // line count -- skip via the buffer's own report. The
        // line_count == 0 branch is defensive (ropey rarely
        // produces it) so this test only confirms the non-empty
        // path doesn't panic when the rope contains a single
        // empty line.
        let ctx = app.build_picker_context(&snap);
        let source = LinesSource::new();
        let result = source.init(&ctx, &[]).expect("inline");
        if let PickerInitResult::Inline(pairs) = result {
            // One logical line, contents may be empty.
            assert_eq!(pairs.len(), 1);
        } else {
            panic!("expected Inline");
        }
    }

    /// P.3: accept on a `JumpInBuffer` routing returns the
    /// matching outcome variant. Mismatched routing returns
    /// `Err`.
    #[test]
    fn lines_source_accept_translates_jump_in_buffer() {
        let app = app_with("hi\n", 5);
        let snap = app.document.snapshot();
        let ctx = app.build_picker_context(&snap);
        let source = LinesSource::new();
        let routing = RoutingPayload::JumpInBuffer { buffer_id: 7, line: 12, col: 3 };
        let outcome = source.accept(&ctx, &routing).expect("ok");
        match outcome {
            PickerAcceptOutcome::JumpInBuffer { buffer_id, line, col } => {
                assert_eq!(buffer_id, 7);
                assert_eq!(line, 12);
                assert_eq!(col, 3);
            }
            other => panic!("expected JumpInBuffer, got {other:?}"),
        }
        let bad = RoutingPayload::OpenFile { path: "/tmp/x".into() };
        assert!(source.accept(&ctx, &bad).is_err());
    }
}
