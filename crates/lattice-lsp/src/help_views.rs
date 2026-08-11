//! LSP-specific help-buffer factories (DESIGN.md §5.11).
//!
//! These produce [`lattice_help::HelpContent`] from LSP runtime state
//! (the diagnostics layer, supervisor, logger, etc.). They live in
//! `lattice-lsp` rather than `lattice-help` because they read LSP
//! types -- the help crate has no awareness of LSP and should stay
//! that way (one extension model, one substrate). Callers in the
//! editor App invoke these directly when servicing
//! `:diagnostics` / `:lsp-status` / `:lsp-log` / `:references`.
//!
//! Each function returns an unsyntaxed [`HelpContent`]; help buffers
//! receive their markdown syntax and link styling from the live
//! cells-worker `DisplayMatrix` once displayed.

use lattice_help::{HelpContent, one_line};

use crate::{
    Capabilities, DiagnosticSeverity, DiagnosticsLayer, LogRecord, LogSource, LspLogger,
    LspSupervisor, LspSupervisorHandle, actor::uri_to_path,
};

/// Build a help buffer listing every workspace diagnostic
/// (Phase 4.1.d.iv). Each diagnostic renders as one
/// `[severity] [path:line:col message](file:path:line)` row -- the
/// markdown link is parsed by `extract_links_and_clean` into a
/// `HelpLinkTarget::Source` that the existing `do_help_follow_link`
/// path knows how to dispatch (jumps to the file at the given line).
///
/// URIs sort alphabetically; diagnostics within a URI sort by
/// (line, column). Empty layer renders an explicit "no diagnostics"
/// message so the buffer is always useful as a status read.
pub fn diagnostics_help(layer: &DiagnosticsLayer) -> HelpContent {
    let snapshot = layer.snapshot();
    let counts = layer.severity_counts();
    let mut lines: Vec<String> = Vec::new();
    if snapshot.is_empty() {
        lines.push("# Workspace diagnostics".to_string());
        lines.push(String::new());
        lines.push("(none)".to_string());
        return HelpContent::from_lines("diagnostics", lines);
    }
    lines.push(format!(
        "# Workspace diagnostics ({} total: {} errors, {} warnings, {} info, {} hints)",
        counts.total(),
        counts.errors,
        counts.warnings,
        counts.info,
        counts.hints
    ));
    lines.push(String::new());
    for (uri, diags) in snapshot {
        let path = uri_to_path(&uri)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| uri.as_str().to_string());
        lines.push(format!("## {} ({})", path, diags.len()));
        lines.push(String::new());
        for d in diags {
            let sev = match d.severity {
                Some(DiagnosticSeverity::ERROR) => "E",
                Some(DiagnosticSeverity::WARNING) => "W",
                Some(DiagnosticSeverity::INFORMATION) => "I",
                Some(DiagnosticSeverity::HINT) => "H",
                _ => "?",
            };
            let line0 = d.range.start.line;
            let col0 = d.range.start.character;
            let label = format!(
                "{}:{}:{} {}",
                path,
                line0 + 1,
                col0 + 1,
                one_line(&d.message)
            );
            lines.push(format!("[{sev}] [{label}](file:{path}:{line0})"));
        }
        lines.push(String::new());
    }
    HelpContent::from_lines("diagnostics", lines)
}

/// Build the `*lsp*` subsystem-wide log view (Phase 4.1.g).
/// Snapshots `logger.snapshot_global()` and renders one row per
/// record: `<timestamp> <level> <source> <message>`.
pub fn lsp_global_log_help(logger: &LspLogger) -> HelpContent {
    let records = logger.snapshot_global();
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "# *lsp* (subsystem-wide, {} records)",
        records.len()
    ));
    lines.push(String::new());
    if records.is_empty() {
        lines.push("(no records)".to_string());
    } else {
        for r in records {
            lines.push(format_log_record(&r));
        }
    }
    HelpContent::from_lines("lsp", lines)
}

/// Build a per-instance log view (`*lsp:<server>:<workspace>*`).
/// Filters out trace records (those land in `lsp_server_trace_help`).
pub fn lsp_server_log_help(
    logger: &LspLogger,
    instance: &crate::logging::InstanceKey,
) -> HelpContent {
    let records = logger.snapshot_instance(instance);
    let body: Vec<&LogRecord> = records
        .iter()
        .filter(|r| r.source != LogSource::Trace)
        .collect();
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "# *lsp:{}:{}* ({} records, trace excluded)",
        instance.server_id,
        instance.workspace.display(),
        body.len()
    ));
    lines.push(String::new());
    if body.is_empty() {
        lines.push("(no records)".to_string());
    } else {
        for r in body {
            lines.push(format_log_record(r));
        }
    }
    HelpContent::from_lines(
        format!(
            "lsp:{}:{}",
            instance.server_id,
            instance.workspace.display()
        ),
        lines,
    )
}

/// Build the JSON-RPC trace view
/// (`*lsp:<server>:<workspace>:trace*`). Filters to
/// `LogSource::Trace` records only. Empty when trace mode hasn't
/// been on.
pub fn lsp_server_trace_help(
    logger: &LspLogger,
    instance: &crate::logging::InstanceKey,
) -> HelpContent {
    let records = logger.snapshot_instance(instance);
    let body: Vec<&LogRecord> = records
        .iter()
        .filter(|r| r.source == LogSource::Trace)
        .collect();
    let mut lines: Vec<String> = Vec::new();
    let trace_on = logger.is_tracing(instance);
    lines.push(format!(
        "# *lsp:{}:{}:trace* ({} records, trace currently {})",
        instance.server_id,
        instance.workspace.display(),
        body.len(),
        if trace_on { "ON" } else { "OFF" }
    ));
    lines.push(String::new());
    if body.is_empty() {
        lines.push("(no trace records; toggle with `:lsp-trace <server>`)".to_string());
    } else {
        for r in body {
            lines.push(format_log_record(r));
        }
    }
    HelpContent::from_lines(
        format!(
            "lsp:{}:{}:trace",
            instance.server_id,
            instance.workspace.display()
        ),
        lines,
    )
}

/// Build the `:lsp-server-log` picker -- one row per running actor
/// with workspace root + buffer count + capability summary in the
/// margin, each row carrying an `exec:` link to its log + trace.
/// Use `/query` (vim regex search) to filter; press `<CR>` on a
/// link to open. A real fuzzy picker arrives with the bundled
/// fuzzy-finder plugin (Phase 8b); for now this listing keeps
/// everything reachable through the existing help-buffer machinery.
pub fn lsp_server_log_listing_help(supervisor: &LspSupervisor) -> HelpContent {
    let mut actors = supervisor.running_actors();
    actors.sort_by(|a, b| a.0.1.cmp(&b.0.1).then_with(|| a.0.0.cmp(&b.0.0)));
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "# :lsp-server-log ({} server actor(s) running)",
        actors.len()
    ));
    lines.push(String::new());
    if actors.is_empty() {
        lines.push(
            "(no LSP servers running; open a file with a matching language to attach. \
             see :lsp-status for the full overview.)"
                .to_string(),
        );
        return HelpContent::from_lines("lsp-server-log", lines);
    }
    lines.push(
        "Each row links to the per-server log (`*lsp:<server>*`) and trace \
         (`*lsp:<server>:trace*`). Press `<CR>` on a link to open. Use \
         `/query` to filter rows by id or workspace path."
            .to_string(),
    );
    lines.push(String::new());
    for ((workspace, server_id), handle) in &actors {
        let buffer_count = supervisor.buffer_count_for(&(workspace.clone(), server_id.clone()));
        let caps = handle.capabilities();
        let cap_summary = summarise_capabilities(&caps);
        lines.push(format!("## [{server_id}](exec:lsp-log {server_id})"));
        lines.push(format!("- workspace:    `{}`", workspace.display()));
        lines.push(format!("- buffers:      {buffer_count} attached"));
        lines.push(format!("- capabilities: {cap_summary}"));
        lines.push(format!(
            "- trace:        [open / toggle](exec:lsp-trace {server_id})"
        ));
        lines.push(String::new());
    }
    HelpContent::from_lines("lsp-server-log", lines)
}

/// Build the `:lsp-status` view -- one row per running actor (id,
/// workspace root, server-side capability summary).
pub fn lsp_status_help(supervisor: &LspSupervisorHandle) -> HelpContent {
    let actors = supervisor.running_actors();
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "# :lsp-status ({} server(s), {} attached buffer(s))",
        actors.len(),
        supervisor.attached_buffer_count()
    ));
    lines.push(String::new());
    if actors.is_empty() {
        lines.push(
            "(no LSP servers running; open a file with a matching language to attach)".to_string(),
        );
    } else {
        for ((workspace, server_id), handle) in actors {
            let caps = handle.capabilities();
            lines.push(format!("## {server_id}"));
            lines.push(format!("- workspace root: `{}`", workspace.display()));
            lines.push(format!("- position encoding: {:?}", caps.position_encoding));
            lines.push(format!("- supports hover: {}", caps.supports_hover()));
            lines.push(format!(
                "- supports definition: {}",
                caps.supports_definition()
            ));
            lines.push(format!(
                "- diagnostics subscribers: {}",
                handle.diagnostics_subscriber_count()
            ));
            lines.push(String::new());
        }
    }
    HelpContent::from_lines("lsp-status", lines)
}

/// One-line summary of a server's negotiated capabilities. Used in
/// the `:lsp-server-log` picker margin so a glance tells the user
/// "this server has hover + completion but not references" without
/// having to dig into `:lsp-status`.
pub fn summarise_capabilities(caps: &Capabilities) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if caps.supports_hover() {
        parts.push("hover");
    }
    if caps.supports_definition() {
        parts.push("definition");
    }
    if caps.supports_references() {
        parts.push("references");
    }
    if caps.supports_document_symbol() {
        parts.push("document-symbol");
    }
    if caps.supports_workspace_symbol() {
        parts.push("workspace-symbol");
    }
    if caps.supports_completion() {
        parts.push("completion");
    }
    if parts.is_empty() {
        "(none advertised)".into()
    } else {
        parts.join(", ")
    }
}

/// Render one log record as a one-line entry:
/// `HH:MM:SS.mmm <level> <source>: <message>`. Used by the four log
/// buffer builders above (Phase 4.1.g).
fn format_log_record(r: &LogRecord) -> String {
    use std::time::SystemTime;
    let elapsed = r.timestamp.duration_since(SystemTime::UNIX_EPOCH).ok();
    let secs = elapsed.map(|d| d.as_secs()).unwrap_or(0);
    let ms = elapsed.map(|d| d.subsec_millis()).unwrap_or(0);
    let hh = (secs / 3600) % 24;
    let mm = (secs / 60) % 60;
    let ss = secs % 60;
    format!(
        "{:02}:{:02}:{:02}.{:03} {} {:>6}: {}",
        hh,
        mm,
        ss,
        ms,
        r.level.short(),
        r.source.tag(),
        one_line(&r.message)
    )
}
