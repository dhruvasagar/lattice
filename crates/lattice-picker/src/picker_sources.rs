//! First-party picker source generators -- renderer-neutral; live
//! in `lattice-picker` next to the `PickerSourceGenerator` trait
//! and the `PickerRegistry` they register against. Symmetric with
//! how feature crates already organise their sources
//! (`lattice_snippet::picker_sources`, future
//! `lattice_lsp::picker_sources`).
//!
//! Each source's state is reachable through `PickerContext` (the
//! snapshot passed to `init` / `accept`) or via an `Arc`-cloned
//! registry handle captured at construction (`CommandsSource` ->
//! `CommandRegistry`, `GrepSource` -> `ConfigRegistry`). The trait
//! surface stays state-handle-free.
//!
//! Slice 5.7.B.0 migrated this module out of `lattice-ui-tui`. The
//! `walk_files_for_picker` helper (file-system walk for `:picker
//! files`) lives here too -- it has no renderer dependency, and the
//! only consumer today is `FilesSource`; the earlier ui-tui
//! location was an accident of where the picker first landed.

use std::sync::Arc;

use lattice_completion::{CandidateKind, RawCandidate};
use lattice_config::ConfigRegistry;
use lattice_grammar::CommandRegistry;
use lattice_grammar::args::{ArgDefault, ArgSpec, Args};
use lattice_grammar::command::CommandKind;

use crate::{
    PickerAcceptOutcome, PickerContext, PickerInitResult, PickerSourceGenerator, PickerSourceSpec,
    RoutingPayload, SourceResult,
};

/// Format an ex-command's `args_schema` as the marginalia
/// args hint -- emacs-style `<arg>` for required, `[<arg>]`
/// for optional. Empty for no-arg commands. Used by
/// `:picker commands` to fill the args column.
fn format_args_hint(schema: &[ArgSpec]) -> String {
    schema
        .iter()
        .map(|arg| match arg.default {
            ArgDefault::Required => format!("<{}>", arg.name),
            _ => format!("[<{}>]", arg.name),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Clip `s` to at most `width` chars; longer strings get a
/// trailing ellipsis so the user knows truncation happened.
/// Cheap (single chars().count() walk) and stable across
/// utf-8 multi-byte characters.
fn clip_to(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if s.chars().count() <= width {
        return s.to_string();
    }
    let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Format unix mode bits like `ls -l` (`-rw-r--r--`,
/// `drwxr-xr-x`, `lrwxrwxrwx`). On platforms without unix
/// mode bits, falls back to a six-char `<file>` / `<ro>`
/// marker so the column stays width-aligned.
fn format_perms(meta: &std::fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode();
        let ft = meta.file_type();
        let kind = if ft.is_dir() {
            'd'
        } else if ft.is_symlink() {
            'l'
        } else {
            '-'
        };
        let bit = |mask, ch| if mode & mask != 0 { ch } else { '-' };
        format!(
            "{kind}{}{}{}{}{}{}{}{}{}",
            bit(0o400, 'r'),
            bit(0o200, 'w'),
            bit(0o100, 'x'),
            bit(0o040, 'r'),
            bit(0o020, 'w'),
            bit(0o010, 'x'),
            bit(0o004, 'r'),
            bit(0o002, 'w'),
            bit(0o001, 'x'),
        )
    }
    #[cfg(not(unix))]
    {
        if meta.permissions().readonly() {
            "<ro>      ".to_string()
        } else {
            "<rw>      ".to_string()
        }
    }
}

/// Format a byte size with a single-letter SI-ish suffix
/// (`72` / `1.4K` / `70k` / `12M` / `4.2G`), matching the
/// `ls -h` convention. Uses 1024-based units. Capped at 5
/// chars so the size column has a stable width.
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes < KB {
        format!("{bytes}")
    } else if bytes < MB {
        let k = bytes as f64 / KB as f64;
        if k < 10.0 {
            format!("{k:.1}K")
        } else {
            format!("{}K", bytes / KB)
        }
    } else if bytes < GB {
        let m = bytes as f64 / MB as f64;
        if m < 10.0 {
            format!("{m:.1}M")
        } else {
            format!("{}M", bytes / MB)
        }
    } else {
        let g = bytes as f64 / GB as f64;
        if g < 10.0 {
            format!("{g:.1}G")
        } else {
            format!("{}G", bytes / GB)
        }
    }
}

/// Format a `SystemTime` as a relative-to-now phrase
/// (`28 hours ago`, `3 days ago`, `just now`). Stable
/// across reasonable clock skew (negative durations clamp
/// to "just now" rather than producing nonsense). Returns
/// a fixed-format string so columns align.
fn format_mtime_relative(mtime: std::time::SystemTime) -> String {
    let now = std::time::SystemTime::now();
    let secs = match now.duration_since(mtime) {
        Ok(d) => d.as_secs(),
        Err(_) => return "just now".to_string(),
    };
    if secs < 60 {
        "just now".to_string()
    } else if secs < 60 * 60 {
        let m = secs / 60;
        if m == 1 {
            "1 minute ago".to_string()
        } else {
            format!("{m} minutes ago")
        }
    } else if secs < 60 * 60 * 36 {
        // Hours up to 36h, matching moment.js / emacs
        // marginalia convention (so a file edited yesterday
        // afternoon reads "28 hours ago" instead of
        // jumping to "1 day ago" at the 24h boundary).
        let h = secs / (60 * 60);
        if h == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{h} hours ago")
        }
    } else if secs < 60 * 60 * 24 * 30 {
        let d = secs / (60 * 60 * 24);
        if d == 1 {
            "1 day ago".to_string()
        } else {
            format!("{d} days ago")
        }
    } else if secs < 60 * 60 * 24 * 365 {
        let mo = secs / (60 * 60 * 24 * 30);
        if mo == 1 {
            "1 month ago".to_string()
        } else {
            format!("{mo} months ago")
        }
    } else {
        let y = secs / (60 * 60 * 24 * 365);
        if y == 1 {
            "1 year ago".to_string()
        } else {
            format!("{y} years ago")
        }
    }
}

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
                doc: "File picker rooted at the current working directory (recursive). Pass an explicit path to override.",
                args_hint: "[root]",
                args_schema: vec![ArgSpec {
                    name: "root",
                    kind: ArgKind::String,
                    doc: "Directory to walk recursively. Absent = current working directory.",
                    prompt: "root:",
                    default: ArgDefault::None,
                    completion: Some("gen:files"),
                }],
                live: false,
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

    fn init(&self, _ctx: &PickerContext<'_>, args: &[String]) -> SourceResult<PickerInitResult> {
        // Default to the process's current working directory
        // (recursive). This matches what users typically
        // expect from `:files` -- the same root `:e` paths
        // resolve against. Earlier slices defaulted to the
        // active document's parent dir which behaved
        // unintuitively for projects spread across many
        // subdirectories. Users who want a different root
        // pass it explicitly: `:picker files <path>`.
        let root: std::path::PathBuf = match args.first() {
            Some(p) if !p.is_empty() => std::path::PathBuf::from(p),
            _ => std::env::current_dir()
                .map_err(|e| format!("files: failed to read current directory: {e}"))?,
        };
        let canonical_root = std::fs::canonicalize(&root).unwrap_or(root.clone());
        let entries = walk_files_for_picker(&canonical_root);
        if entries.is_empty() {
            return Err(format!(
                "files: no files under {}",
                canonical_root.display()
            ));
        }
        // Stat each entry for marginalia (perms / size /
        // mtime). One syscall per file -- on a fast disk
        // O(N µs); the walker's 5000-entry cap keeps this
        // bounded to <100ms in the worst case. Files we
        // can't stat (permission denied mid-walk) get an
        // empty-metadata row -- the path still shows but
        // the marginalia columns blank out.
        struct Row {
            abs: std::path::PathBuf,
            rel_display: String,
            perms: String,
            size: String,
            mtime: String,
        }
        let rows: Vec<Row> = entries
            .into_iter()
            .map(|abs| {
                let rel = abs
                    .strip_prefix(&canonical_root)
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|_| abs.clone());
                let (perms, size, mtime) = match std::fs::metadata(&abs) {
                    Ok(m) => (
                        format_perms(&m),
                        format_size(m.len()),
                        m.modified().map(format_mtime_relative).unwrap_or_default(),
                    ),
                    Err(_) => (String::new(), String::new(), String::new()),
                };
                Row {
                    abs,
                    rel_display: rel.display().to_string(),
                    perms,
                    size,
                    mtime,
                }
            })
            .collect();
        // Per-column widths adapt to the longest row in each
        // column. Path column caps at 60 chars (clip with
        // ellipsis) so the marginalia stays visible on
        // narrow terminals. Size + mtime have fixed maximums
        // by formatter design (~5 / ~16 chars); perms is
        // 10 chars on unix.
        let path_width = rows
            .iter()
            .map(|r| r.rel_display.chars().count())
            .max()
            .unwrap_or(0)
            .min(60);
        let perms_width = rows.iter().map(|r| r.perms.len()).max().unwrap_or(0);
        let size_width = rows.iter().map(|r| r.size.len()).max().unwrap_or(0);
        let pairs = rows
            .into_iter()
            .map(|row| {
                let path_clipped = clip_to(&row.rel_display, path_width);
                let display = format!(
                    "{:<path$}  {:<perms$}  {:>size$}  {}",
                    path_clipped,
                    row.perms,
                    row.size,
                    row.mtime,
                    path = path_width,
                    perms = perms_width,
                    size = size_width,
                );
                let mut cand = RawCandidate::plain(display, CandidateKind::Plain);
                // Slice 7b.2: typed accept payload.
                cand.accept_action = Some(lattice_completion::AcceptAction::OpenFile {
                    path: row.abs.clone(),
                });
                (cand, RoutingPayload::OpenFile { path: row.abs })
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

    fn init(&self, ctx: &PickerContext<'_>, _args: &[String]) -> SourceResult<PickerInitResult> {
        if ctx.recent_files.is_empty() {
            return Err("no recent files".into());
        }
        let pairs = ctx
            .recent_files
            .iter()
            .map(|p| {
                let display = p.display().to_string();
                let mut cand = RawCandidate::plain(display, CandidateKind::Plain);
                // Slice 7b.2: typed accept payload.
                cand.accept_action = Some(lattice_completion::AcceptAction::OpenFile {
                    path: p.clone(),
                });
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

    fn init(&self, ctx: &PickerContext<'_>, _args: &[String]) -> SourceResult<PickerInitResult> {
        let active = ctx.active_buffer.buffer_id;
        // Float the active buffer to the bottom of the list
        // so the initial selection lands on the alternate.
        let mut entries: Vec<&crate::BufferEntry> = ctx.buffers.iter().collect();
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
                let mut cand = RawCandidate::plain(format!("#{}", e.id), CandidateKind::Buffer);
                cand.display = display;
                // Slice 7b.1: typed accept payload on the
                // candidate. Parallel to the existing
                // RoutingPayload (still emitted for the picker's
                // routing_meta lookup) — slice 7d's registry
                // cutover drops the parallel routing vec once
                // the host routes accept through
                // DefaultAcceptHandler.
                cand.accept_action = Some(lattice_completion::AcceptAction::SwitchBuffer {
                    id: lattice_core::BufferId(e.id),
                });
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

    fn init(&self, ctx: &PickerContext<'_>, _args: &[String]) -> SourceResult<PickerInitResult> {
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
            let mut cand = RawCandidate::plain(display, CandidateKind::Plain);
            // Slice 7b.4: typed accept payload.
            cand.accept_action = Some(lattice_completion::AcceptAction::JumpInBuffer {
                buffer_id: lattice_core::BufferId(buffer_id),
                line: line as u32,
                col: 0,
            });
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
            RoutingPayload::JumpInBuffer {
                buffer_id,
                line,
                col,
            } => Ok(PickerAcceptOutcome::JumpInBuffer {
                buffer_id: *buffer_id,
                line: *line,
                col: *col,
            }),
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

    fn init(&self, ctx: &PickerContext<'_>, _args: &[String]) -> SourceResult<PickerInitResult> {
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
                    crate::PositionSource::AutoJump => "auto".to_string(),
                    crate::PositionSource::ExplicitMark => "mark".to_string(),
                    crate::PositionSource::PluginPush => "plugin".to_string(),
                    crate::PositionSource::NamedMark(c) => format!("'{c}"),
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
                let mut cand = RawCandidate::plain(display, CandidateKind::Plain);
                // Slice 7b.4: typed accept payload.
                cand.accept_action = Some(lattice_completion::AcceptAction::JumpInBuffer {
                    buffer_id: lattice_core::BufferId(entry.buffer_id),
                    line: entry.line,
                    col: entry.col,
                });
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
            RoutingPayload::JumpInBuffer {
                buffer_id,
                line,
                col,
            } => Ok(PickerAcceptOutcome::JumpInBuffer {
                buffer_id: *buffer_id,
                line: *line,
                col: *col,
            }),
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

    fn init(&self, _ctx: &PickerContext<'_>, _args: &[String]) -> SourceResult<PickerInitResult> {
        // Walk registry names, keep ex-commands, project to a
        // row carrying every marginalia column. Emacs
        // `marginalia.el`-style: name | args-hint | doc |
        // latency-tag, all right-padded to align across rows.
        // Mode-toggle ex-commands like `buffer-words-mode`
        // register without the `ex:` prefix; the projection
        // handles both.
        struct Row {
            user_facing: String,
            canonical: String,
            args_hint: String,
            doc: String,
            latency: &'static str,
        }
        let mut rows: Vec<Row> = self
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
                let args_hint = format_args_hint(&spec.args_schema);
                let one_line_doc: String = spec
                    .doc
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect();
                Some(Row {
                    user_facing,
                    canonical: canonical.to_string(),
                    args_hint,
                    doc: one_line_doc,
                    latency: spec.latency_class.label(),
                })
            })
            .collect();
        // Sort by user-facing name so the popup matches the
        // alphabetic order users see.
        rows.sort_by(|a, b| a.user_facing.cmp(&b.user_facing));
        if rows.is_empty() {
            return Err("commands: no ex-commands registered".into());
        }
        // Per-column widths adapt to the longest row in each
        // column. Args column caps at 20 chars so a command
        // with many args doesn't push the doc column off the
        // visible width. Latency tag width is the fixed
        // `[background]` length (12 incl. brackets) so the
        // right edge stays stable.
        let name_width = rows.iter().map(|r| r.user_facing.len()).max().unwrap_or(0);
        let args_width = rows
            .iter()
            .map(|r| r.args_hint.len())
            .max()
            .unwrap_or(0)
            .min(20);
        const LATENCY_TAG_WIDTH: usize = 12;
        let pairs = rows
            .into_iter()
            .map(|row| {
                let args_clipped = clip_to(&row.args_hint, args_width);
                let tag = format!("[{}]", row.latency);
                let display = format!(
                    "{:<name$}  {:<args$}  {:<doc_max$}  {:>tag_w$}",
                    row.user_facing,
                    args_clipped,
                    row.doc,
                    tag,
                    name = name_width,
                    args = args_width,
                    doc_max = 60,
                    tag_w = LATENCY_TAG_WIDTH,
                );
                let mut cand = RawCandidate::plain(row.user_facing.clone(), CandidateKind::Plain);
                cand.display = display;
                // Slice 7b.3: typed accept payload.
                cand.accept_action = Some(lattice_completion::AcceptAction::InvokeCommand {
                    id: row.canonical.clone(),
                    args: Args::None,
                });
                (
                    cand,
                    RoutingPayload::InvokeCommand {
                        id: row.canonical,
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
            RoutingPayload::InvokeCommand { id, args } => Ok(PickerAcceptOutcome::InvokeCommand {
                id: id.clone(),
                args: args.clone(),
            }),
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

    fn init(&self, ctx: &PickerContext<'_>, _args: &[String]) -> SourceResult<PickerInitResult> {
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
                let mut cand = RawCandidate::plain(display, CandidateKind::Plain);
                // Slice 7b.5: typed accept payload.
                cand.accept_action =
                    Some(lattice_completion::AcceptAction::PasteRegister { name: ch });
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

/// `:picker marks`. Walks `ctx.marks` (sorted by name in
/// `build_picker_context`) and emits one row per set mark.
/// Accept emits `JumpToMark { name }` which the host
/// resolves through `do_jump_mark` -- same path the `` ` ``
/// motion uses, so cursor placement + position-history push
/// match keyboard-driven behavior. MRU will key on
/// `mark:<name>` automatically when slice 14 lands.
pub struct MarksSource {
    pub spec: PickerSourceSpec,
}

impl MarksSource {
    pub fn new() -> Self {
        Self {
            spec: PickerSourceSpec::no_args(
                "marks",
                "Vim-style marks. `<CR>` jumps to the mark via the same path as `` ` ``.",
            ),
        }
    }
}

impl Default for MarksSource {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerSourceGenerator for MarksSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(&self, ctx: &PickerContext<'_>, _args: &[String]) -> SourceResult<PickerInitResult> {
        if ctx.marks.is_empty() {
            return Err("marks: no marks set".into());
        }
        let pairs = ctx
            .marks
            .iter()
            .map(|(name, pos)| {
                let display = format!("'{name:<2} {}:{}", pos.line + 1, pos.byte + 1);
                let mut cand = RawCandidate::plain(display, CandidateKind::Plain);
                // Slice 7b.5: typed accept payload.
                cand.accept_action =
                    Some(lattice_completion::AcceptAction::JumpToMark { name: *name });
                (cand, RoutingPayload::JumpToMark { name: *name })
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
            RoutingPayload::JumpToMark { name } => {
                Ok(PickerAcceptOutcome::JumpToMark { name: *name })
            }
            other => Err(format!("marks: unexpected routing payload {other:?}")),
        }
    }
}

/// `:picker grep <pattern>`. Shells out to a configurable
/// backend (`rg`, `ag`, `grep`, or `auto`-detected at
/// invocation time) and walks its output line-by-line.
///
/// Sync subprocess for v1 (matches Files / Recent design --
/// users invoke explicitly; brief wait is acceptable). The
/// `:picker grep` ergonomic equivalent of vertico-buffer
/// live-grep with prescient ranking ships once the async
/// init seat path lands; until then this is the simplest
/// path that respects the configurable-backend requirement.
///
/// Captures `Arc<ConfigRegistry>` at construction so the
/// backend choice is read at every invocation (lets the user
/// `:set picker.grep.backend = "ag"` mid-session and see
/// it take effect on the next `:picker grep`).
pub struct GrepSource {
    pub spec: PickerSourceSpec,
    pub config: Arc<ConfigRegistry>,
}

impl GrepSource {
    pub fn new(config: Arc<ConfigRegistry>) -> Self {
        use lattice_grammar::args::{ArgDefault, ArgKind, ArgSpec};
        Self {
            spec: PickerSourceSpec {
                id: "grep",
                doc: "Live recursive text search via the configured backend (`rg`/`ag`/`grep`). Re-runs as you type; `<CR>` jumps to the chosen hit.",
                args_hint: "[pattern]",
                args_schema: vec![ArgSpec {
                    name: "pattern",
                    kind: ArgKind::String,
                    doc: "Optional initial pattern. When given, seeds the picker prompt; without it, picker opens empty and runs the first grep on the first keystroke.",
                    prompt: "pattern:",
                    default: ArgDefault::None,
                    completion: None,
                }],
                // Slice 3: live source. Picker bypasses fuzzy
                // refilter (`run_grep` IS the filter); host
                // calls `on_query_changed` on each debounced
                // keystroke.
                live: true,
            },
            config,
        }
    }

    /// Resolve backend choice + max-hits from the config. Shared
    /// by `init` and `on_query_changed` so both routes honour
    /// the same `:set picker.grep.*` options.
    fn resolve_settings(&self) -> SourceResult<(String, usize)> {
        let backend_choice = self
            .config
            .get_typed::<lattice_config::core_options::PickerGrepBackend>()
            .map(|s| (*s).clone())
            .unwrap_or_else(|| "auto".to_string());
        let max_hits = self
            .config
            .get_typed::<lattice_config::core_options::PickerGrepMaxHits>()
            .map(|n| *n as usize)
            .unwrap_or(2000)
            .max(1);
        let resolved = resolve_grep_backend(&backend_choice)?;
        Ok((resolved, max_hits))
    }

    /// Build a `CandidateFuture` that runs `run_grep` on
    /// tokio's blocking pool. Uses `spawn_blocking` because
    /// `run_grep` shells out via the std-sync `Command::output`
    /// API; running it on the async runtime's worker pool would
    /// pin a worker for the duration of the grep. The blocking
    /// pool is the right fit -- it's sized for exactly this
    /// kind of task.
    fn spawn_grep(
        binary: String,
        pattern: String,
        root: std::path::PathBuf,
        max_hits: usize,
    ) -> crate::CandidateFuture {
        Box::pin(async move {
            let join =
                tokio::task::spawn_blocking(move || run_grep(&binary, &pattern, &root, max_hits))
                    .await;
            match join {
                Ok(Ok(hits)) => Ok(hits_to_pairs(hits)),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(format!("grep: task panicked: {e}")),
            }
        })
    }
}

/// Convert raw grep hits into the picker's `(RawCandidate,
/// RoutingPayload)` pairs. Shared by the sync init() fast
/// path (no initial pattern → empty pairs) and the async
/// future path that the live grep flow drives. Empty input
/// → empty output; callers don't special-case.
fn hits_to_pairs(hits: Vec<GrepHit>) -> crate::CandidateBatch {
    hits.into_iter()
        .map(|hit| {
            let display = format!(
                "{}:{}:{}  {}",
                hit.path.display(),
                hit.line + 1,
                hit.col + 1,
                hit.preview.trim_start(),
            );
            let mut cand = RawCandidate::plain(display, CandidateKind::Plain);
            // Slice 7b.6: typed accept payload. Grep hits jump
            // to file:line:col — same shape as LSP references /
            // definitions / diagnostics → JumpToFileLocation.
            cand.accept_action = Some(lattice_completion::AcceptAction::JumpToFileLocation {
                path: hit.path.clone(),
                line: hit.line,
                col: hit.col,
            });
            (
                cand,
                RoutingPayload::LspLocation {
                    path: hit.path,
                    line: hit.line,
                    col: hit.col,
                },
            )
        })
        .collect()
}

impl PickerSourceGenerator for GrepSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    /// Slice 3: optional initial pattern. With no pattern the
    /// picker opens empty (no grep runs); the first keystroke
    /// triggers the live flow through `on_query_changed`.
    /// With an initial pattern the grep runs immediately --
    /// async via the Future variant so the UI thread doesn't
    /// park on the first invocation either. The host seeds
    /// `picker.query` with the initial pattern (live-source
    /// convention in `App::open_picker`), so subsequent
    /// keystrokes extend the same query.
    fn init(&self, ctx: &PickerContext<'_>, args: &[String]) -> SourceResult<PickerInitResult> {
        let pattern = args.first().map(|s| s.trim()).filter(|s| !s.is_empty());
        let Some(pattern) = pattern else {
            return Ok(PickerInitResult::Inline(Vec::new()));
        };
        let (binary, max_hits) = self.resolve_settings()?;
        let root = ctx.workspace_root.to_path_buf();
        let fut = GrepSource::spawn_grep(binary, pattern.to_string(), root, max_hits);
        Ok(PickerInitResult::Future(fut))
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        match routing {
            RoutingPayload::LspLocation { path, line, col } => {
                Ok(PickerAcceptOutcome::JumpToLocation {
                    path: path.clone(),
                    line: *line,
                    col: *col,
                })
            }
            other => Err(format!("grep: unexpected routing payload {other:?}")),
        }
    }

    /// Slice 3: live re-execution. The host's
    /// `drain_pending_live_picker_query` calls this every time
    /// the debounce expires; we trim, special-case the empty
    /// query (no grep, empty result -- clears the candidate
    /// list), and otherwise spawn the grep on the blocking
    /// pool. The Future variant lets the host cancel us if a
    /// newer keystroke fires before we finish.
    fn on_query_changed(
        &self,
        ctx: &PickerContext<'_>,
        query: &str,
    ) -> Option<SourceResult<PickerInitResult>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Some(Ok(PickerInitResult::Inline(Vec::new())));
        }
        let settings = match self.resolve_settings() {
            Ok(s) => s,
            Err(e) => return Some(Err(e)),
        };
        let (binary, max_hits) = settings;
        let root = ctx.workspace_root.to_path_buf();
        let fut = GrepSource::spawn_grep(binary, trimmed.to_string(), root, max_hits);
        Some(Ok(PickerInitResult::Future(fut)))
    }
}

/// One grep hit -- path + 0-based LSP-flavored line + 0-based
/// utf-8 byte column + the matching line's text (preview).
struct GrepHit {
    path: std::path::PathBuf,
    line: u32,
    col: u32,
    preview: String,
}

/// Picks the grep binary from the user's `picker.grep.backend`
/// option. `"auto"` walks rg / ag / grep, returning the first
/// on PATH. Explicit names check that single binary; missing
/// returns `Err` so the user can re-configure.
fn resolve_grep_backend(choice: &str) -> SourceResult<String> {
    fn on_path(name: &str) -> bool {
        std::env::var_os("PATH")
            .map(|p| {
                std::env::split_paths(&p).any(|dir| {
                    let bin = dir.join(name);
                    bin.is_file()
                })
            })
            .unwrap_or(false)
    }
    if choice == "auto" {
        for candidate in ["rg", "ag", "grep"] {
            if on_path(candidate) {
                return Ok(candidate.to_string());
            }
        }
        return Err("grep: no backend on PATH (tried rg, ag, grep). \
             Set `picker.grep.backend` to a binary name."
            .into());
    }
    if on_path(choice) {
        Ok(choice.to_string())
    } else {
        Err(format!(
            "grep: backend `{choice}` not found on PATH \
             (configured via `picker.grep.backend`)"
        ))
    }
}

/// Run `binary <pattern> <root>` with backend-appropriate
/// args and parse the output. Output formats:
/// - rg: `path:line:col:text`
/// - ag: `path:line:col:text`
/// - grep: `path:line:text` (no column; fall back to 0)
fn run_grep(
    binary: &str,
    pattern: &str,
    root: &std::path::Path,
    max_hits: usize,
) -> SourceResult<Vec<GrepHit>> {
    let mut cmd = std::process::Command::new(binary);
    match binary {
        "rg" => {
            cmd.args(["--no-heading", "--line-number", "--column", "--color=never"]);
        }
        "ag" => {
            cmd.args(["--noheading", "--column", "--nocolor"]);
        }
        "grep" => {
            cmd.args(["-rnH"]);
        }
        _ => {
            // Custom backend; assume an rg-compatible flag set.
            cmd.args(["--line-number", "--column"]);
        }
    }
    cmd.arg(pattern).arg(root);
    let output = cmd
        .output()
        .map_err(|e| format!("grep: spawning `{binary}` failed: {e}"))?;
    if !output.status.success() && output.stdout.is_empty() {
        // Some backends (`grep`, `rg`) return non-zero on
        // "no hits". Only treat as error when stderr has a
        // real message AND stdout is empty.
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            return Err(format!("grep: `{binary}` failed: {stderr}"));
        }
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut hits = Vec::new();
    for raw_line in stdout.lines() {
        if hits.len() >= max_hits {
            break;
        }
        if let Some(hit) = parse_grep_line(binary, raw_line) {
            hits.push(hit);
        }
    }
    Ok(hits)
}

/// Parse one output line. `path:line:col:text` for rg/ag,
/// `path:line:text` for grep. Path may itself contain colons
/// (Windows drive letters, or files with `:` in name); we
/// scan left-to-right for the first numeric `line` segment
/// and key off that rather than splitting blindly on colons.
fn parse_grep_line(binary: &str, raw: &str) -> Option<GrepHit> {
    let with_column = matches!(binary, "rg" | "ag") || binary.contains("rg");
    // Collect colon positions left-to-right; we'll walk pairs
    // looking for the first all-digits chunk between two
    // colons -- that's the line number, and everything before
    // is the path.
    let colon_idxs: Vec<usize> = raw
        .bytes()
        .enumerate()
        .filter_map(|(i, b)| (b == b':').then_some(i))
        .collect();
    for window in colon_idxs.windows(2) {
        let line_chunk = &raw[window[0] + 1..window[1]];
        if line_chunk.bytes().all(|b| b.is_ascii_digit()) && !line_chunk.is_empty() {
            let line: u32 = line_chunk.parse().ok()?;
            let path = &raw[..window[0]];
            if with_column {
                // Need a column next: look for another colon
                // after `window[1]` whose chunk between is all
                // digits.
                let after_line = window[1];
                let next_colon = colon_idxs.iter().find(|&&i| i > after_line)?;
                let col_chunk = &raw[after_line + 1..*next_colon];
                if col_chunk.bytes().all(|b| b.is_ascii_digit()) && !col_chunk.is_empty() {
                    let col: u32 = col_chunk.parse().ok()?;
                    let preview = raw[*next_colon + 1..].to_string();
                    return Some(GrepHit {
                        path: std::path::PathBuf::from(path),
                        line: line.saturating_sub(1),
                        col: col.saturating_sub(1),
                        preview,
                    });
                }
                continue;
            }
            // grep: path:line:text -- preview is everything
            // after the line's trailing colon.
            let preview = raw[window[1] + 1..].to_string();
            return Some(GrepHit {
                path: std::path::PathBuf::from(path),
                line: line.saturating_sub(1),
                col: 0,
                preview,
            });
        }
    }
    None
}

/// `:picker outline`. Tree-sitter-driven symbol outline for
/// the active buffer. Reads `ctx.active_buffer.syntax_symbols`
/// (pre-collected by the host via
/// `Syntax::collect_symbol_locations`) and emits one row per
/// symbol, sorted by source position. Accept jumps the
/// cursor to the symbol via `JumpInBuffer`.
///
/// The LSP-flavored counterpart (`textDocument/documentSymbol`)
/// lives in `lattice-lsp::picker_sources` once the async-init
/// seat path lands; for now this source provides a
/// language-agnostic outline that works for every language
/// with a tree-sitter symbols query (`rust`, `python`,
/// `javascript` today; more as queries register).
pub struct OutlineSource {
    pub spec: PickerSourceSpec,
}

impl OutlineSource {
    pub fn new() -> Self {
        Self {
            spec: PickerSourceSpec::no_args(
                "outline",
                "Tree-sitter symbol outline for the active buffer. `<CR>` jumps to the symbol.",
            ),
        }
    }
}

impl Default for OutlineSource {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerSourceGenerator for OutlineSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(&self, ctx: &PickerContext<'_>, _args: &[String]) -> SourceResult<PickerInitResult> {
        if ctx.active_buffer.syntax_symbols.is_empty() {
            let lang = ctx.active_buffer.language.unwrap_or("plain");
            return Err(format!(
                "outline: no symbols (language `{lang}` has no tree-sitter query, or the parse tree is empty)"
            ));
        }
        let buffer_id = ctx.active_buffer.buffer_id;
        // Width-pad the line-number column so the symbol name
        // column lines up.
        let max_line = ctx
            .active_buffer
            .syntax_symbols
            .iter()
            .map(|(_, l, _)| *l)
            .max()
            .unwrap_or(0)
            + 1;
        let line_width = ((max_line as f64).log10().floor() as usize) + 1;
        let pairs = ctx
            .active_buffer
            .syntax_symbols
            .iter()
            .map(|(name, line, col)| {
                let display = format!("{:>width$}: {name}", line + 1, width = line_width,);
                let mut cand = RawCandidate::plain(display, CandidateKind::Plain);
                // Slice 7b.4: typed accept payload.
                cand.accept_action = Some(lattice_completion::AcceptAction::JumpInBuffer {
                    buffer_id: lattice_core::BufferId(buffer_id),
                    line: *line,
                    col: *col,
                });
                (
                    cand,
                    RoutingPayload::JumpInBuffer {
                        buffer_id,
                        line: *line,
                        col: *col,
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
            RoutingPayload::JumpInBuffer {
                buffer_id,
                line,
                col,
            } => Ok(PickerAcceptOutcome::JumpInBuffer {
                buffer_id: *buffer_id,
                line: *line,
                col: *col,
            }),
            other => Err(format!("outline: unexpected routing payload {other:?}")),
        }
    }
}

/// Hard cap on the file-picker walker's emitted entry count.
/// At this scale the host's fuzzy matcher stays well inside the
/// per-keystroke frame budget; larger trees fall back to ripgrep-
/// style live filtering via `:grep` (P.10) or `:Filetree`'s
/// per-directory lazy walk.
pub const FILE_PICKER_MAX_ENTRIES: usize = 5000;

/// Walk `root` recursively (BFS) and return the absolute paths
/// of every regular file, capped at [`FILE_PICKER_MAX_ENTRIES`].
/// Skips the conventional ignore directories (`.git`, `target`,
/// `node_modules`, `dist`, `.cache`) and dotfiles at the top of
/// each directory entry. Symlinks aren't followed -- a cycle on
/// disk would silently consume the cap.
///
/// Errors are silently absorbed (unreadable directories show up
/// as gaps in the listing); the picker UX prefers "some results"
/// over a hard failure when the workspace has a permission
/// pocket somewhere.
///
/// Moved here from `lattice-ui-tui::app::picker` in slice 5.7.B.0;
/// the only consumer today is `FilesSource` below. Future
/// non-picker callers (file-tree, oil) can either pull this from
/// `lattice-picker` or get their own walker -- file-walk
/// traversal patterns diverge per use case, so co-location with
/// the current single consumer is honest until that second
/// caller appears.
pub fn walk_files_for_picker(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    const IGNORE_DIRS: &[&str] = &[".git", "target", "node_modules", "dist", ".cache"];
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if out.len() >= FILE_PICKER_MAX_ENTRIES {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut subdirs: Vec<std::path::PathBuf> = Vec::new();
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                if IGNORE_DIRS.contains(&name) {
                    continue;
                }
                subdirs.push(path);
            } else if ft.is_file() {
                files.push(path);
            }
        }
        // Stable order: alphabetic. Files first so they show up
        // before deep subdirs in the candidate list (relative-
        // path sort still scrambles them, but the matcher is
        // fuzzy so order isn't load-bearing).
        files.sort();
        subdirs.sort();
        for f in files {
            if out.len() >= FILE_PICKER_MAX_ENTRIES {
                break;
            }
            out.push(f);
        }
        // BFS-ish: push subdirs in reverse so pop() drains
        // alphabetically.
        for sub in subdirs.into_iter().rev() {
            stack.push(sub);
        }
    }
    out
}

/// Convenience: build the first-party source generators as
/// `Arc<dyn PickerSourceGenerator>` ready to register against
/// a `PickerRegistry`. Used by `App::new` (and a future host-
/// owned `Editor::boot`) to boot the registry. Sources that
/// need App-wide state captured at construction (e.g.
/// `CommandsSource` -> `CommandRegistry`, `GrepSource` ->
/// `ConfigRegistry`) take the relevant `Arc` here so the trait
/// surface stays state-handle-free.
pub fn first_party_generators(
    command_registry: Arc<CommandRegistry>,
    config: Arc<ConfigRegistry>,
) -> Vec<Arc<dyn PickerSourceGenerator>> {
    vec![
        Arc::new(FilesSource::new()),
        Arc::new(RecentFilesSource::new()),
        Arc::new(BuffersSource::new()),
        Arc::new(LinesSource::new()),
        Arc::new(JumpsSource::new()),
        Arc::new(CommandsSource::new(command_registry)),
        Arc::new(RegistersSource::new()),
        Arc::new(MarksSource::new()),
        Arc::new(GrepSource::new(config)),
        Arc::new(OutlineSource::new()),
    ]
}

#[cfg(test)]
mod tests {
    //! Unit tests for the pure private helpers (formatters,
    //! grep-line parser). The integration tests that need
    //! `app_with(...)` to build a real `PickerContext` snapshot
    //! stay in `lattice-ui-tui::picker_sources` -- they couple
    //! to the TUI's test-helper App constructor, not to the
    //! sources themselves. Slice 5.7.B.0 split the test layers
    //! so the renderer-neutral substrate's tests build without
    //! pulling ui-tui.

    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    /// Marginalia helpers: `format_size` matches the
    /// `ls -h` convention (bytes / K / M / G with one-decimal
    /// precision under 10 of each unit).
    #[test]
    fn format_size_humanizes_byte_counts() {
        assert_eq!(format_size(0), "0");
        assert_eq!(format_size(512), "512");
        assert_eq!(format_size(1024), "1.0K");
        assert_eq!(format_size(1024 * 9), "9.0K");
        assert_eq!(format_size(1024 * 10), "10K");
        assert_eq!(format_size(1024 * 70), "70K");
        assert_eq!(format_size(1024 * 1024), "1.0M");
        assert_eq!(format_size(1024 * 1024 * 12), "12M");
        assert_eq!(
            format_size(1024_u64.pow(3) * 4 + 1024_u64.pow(3) / 5),
            "4.2G"
        );
    }

    /// `format_mtime_relative` produces stable English-y
    /// relative phrases. We don't test the boundary
    /// transitions exactly (they depend on wall-clock); we
    /// test category dispatch through synthesised deltas.
    #[test]
    fn format_mtime_relative_categorises_durations() {
        use std::time::{Duration, SystemTime};

        let now = SystemTime::now();
        // 30 seconds ago -> "just now"
        let recent = now - Duration::from_secs(30);
        assert_eq!(format_mtime_relative(recent), "just now");
        // 3 minutes ago
        let mins = now - Duration::from_secs(3 * 60);
        assert_eq!(format_mtime_relative(mins), "3 minutes ago");
        // 1 minute ago (singular)
        let one_min = now - Duration::from_secs(70);
        assert_eq!(format_mtime_relative(one_min), "1 minute ago");
        // 28 hours ago (the user's example)
        let hours = now - Duration::from_secs(28 * 60 * 60);
        assert_eq!(format_mtime_relative(hours), "28 hours ago");
        // 5 days ago
        let days = now - Duration::from_secs(5 * 24 * 60 * 60);
        assert_eq!(format_mtime_relative(days), "5 days ago");
    }

    /// Unix permission bits format like `ls -l`.
    #[cfg(unix)]
    #[test]
    fn format_perms_matches_ls_l_shape() {
        // We can't easily construct a Metadata with arbitrary
        // bits; instead stat a real fixture file and confirm
        // the format shape.
        let tmp = std::env::temp_dir().join(format!("lattice-perms-{}", std::process::id()));
        std::fs::write(&tmp, b"x").unwrap();
        let meta = std::fs::metadata(&tmp).unwrap();
        let s = format_perms(&meta);
        assert_eq!(s.len(), 10, "expected 10-char permission string, got `{s}`");
        assert!(
            s.starts_with('-'),
            "file should render with `-` leader, got `{s}`"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    /// Helper smoke: `format_args_hint` matches the
    /// emacs-style `<arg>` / `[<arg>]` convention.
    #[test]
    fn format_args_hint_renders_required_vs_optional() {
        use lattice_grammar::args::{ArgDefault, ArgKind, ArgSpec};
        let required = ArgSpec {
            name: "path",
            kind: ArgKind::String,
            doc: "",
            prompt: "",
            default: ArgDefault::Required,
            completion: None,
        };
        let optional = ArgSpec {
            default: ArgDefault::None,
            ..required.clone()
        };
        assert_eq!(format_args_hint(&[required.clone()]), "<path>");
        assert_eq!(format_args_hint(&[optional.clone()]), "[<path>]");
        assert_eq!(format_args_hint(&[required, optional]), "<path> [<path>]");
        assert_eq!(format_args_hint(&[]), "");
    }

    /// `parse_grep_line` decodes rg / ag format
    /// (`path:line:col:text`). Paths with colons (Windows
    /// drive letters, files with `:` in names) still parse
    /// because we key off the first numeric line segment, not
    /// blind colon-split.
    #[test]
    fn parse_grep_line_rg_format() {
        let hit = parse_grep_line("rg", "src/main.rs:42:7:    let x = foo();").unwrap();
        assert_eq!(hit.path, std::path::PathBuf::from("src/main.rs"));
        assert_eq!(hit.line, 41);
        assert_eq!(hit.col, 6);
        assert_eq!(hit.preview, "    let x = foo();");
    }

    /// `parse_grep_line` decodes plain `grep -rn` format
    /// (`path:line:text`, no column).
    #[test]
    fn parse_grep_line_grep_format() {
        let hit = parse_grep_line("grep", "src/main.rs:42:    let x = foo();").unwrap();
        assert_eq!(hit.path, std::path::PathBuf::from("src/main.rs"));
        assert_eq!(hit.line, 41);
        assert_eq!(hit.col, 0);
        assert_eq!(hit.preview, "    let x = foo();");
    }

    /// `walk_files_for_picker` walks a temp tree, honouring
    /// the dotfile + ignore-dir filters. Co-located with the
    /// walker so the sibling test in ui-tui's app/picker.rs
    /// (which referenced `super::walk_files_for_picker`) can
    /// retire post-move.
    #[test]
    fn walk_files_for_picker_honours_dotfile_and_ignore_filters() {
        let tmp = std::env::temp_dir().join(format!("lattice-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.rs"), "").unwrap();
        std::fs::write(tmp.join("b.rs"), "").unwrap();
        std::fs::create_dir(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("sub").join("c.rs"), "").unwrap();
        // Ignored: dotfile and ignore-dir.
        std::fs::write(tmp.join(".secret"), "").unwrap();
        std::fs::create_dir(tmp.join("target")).unwrap();
        std::fs::write(tmp.join("target").join("d.rs"), "").unwrap();
        let entries = walk_files_for_picker(&tmp);
        let names: Vec<String> = entries
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().any(|n| n == "a.rs"));
        assert!(names.iter().any(|n| n == "b.rs"));
        assert!(names.iter().any(|n| n == "c.rs"));
        assert!(!names.iter().any(|n| n == ".secret"));
        assert!(!names.iter().any(|n| n == "d.rs"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
