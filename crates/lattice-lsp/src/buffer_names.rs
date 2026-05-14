//! Canonical synthetic buffer names for the LSP log family.
//!
//! Three flavours of LSP-owned Document buffer, one canonical
//! name format each:
//!
//! - `*lsp*` — subsystem-wide log. Captures every record with
//!   `server_id.is_none()` (lifecycle, attach driver, supervisor
//!   chatter). [`LSP_SUBSYSTEM_LOG_NAME`].
//! - `*lsp:<server>:<workspace>*` — per-instance log. One per
//!   `InstanceKey`, so two `rust-analyzer` actors on different
//!   workspaces get distinct buffers. [`lsp_server_log_name`].
//! - `*lsp:<server>:<workspace>:trace*` — per-instance JSON-RPC
//!   wire trace. Mirrors the log buffer but only carries
//!   `LogSource::Trace` records. [`lsp_server_trace_log_name`].
//!
//! The inverse helpers ([`parse_lsp_server_log_name`],
//! [`parse_lsp_trace_log_name`]) extract the originating
//! [`InstanceKey`] from a synthetic name so a mode can derive
//! its identity from its buffer's name alone — no buffer-local
//! seeding required (B'.7).
//!
//! ## Invariant
//!
//! Server ids never contain `:`. The parsers split on the first
//! colon after `*lsp:`; everything between that and the trailing
//! `*` (or `:trace*`) is the workspace path. Workspaces with
//! literal colons in their path (rare on POSIX, non-existent on
//! Windows drive letters with `\\?\C:\…` because the registry
//! normalises) survive because the workspace segment is the
//! *remainder* — the parser does not split it further.

use std::path::Path;
use std::sync::Arc;

use crate::logging::InstanceKey;

/// Synthetic name for the subsystem-wide `*lsp*` buffer.
///
/// Created eagerly at App boot so `:b *lsp*` works the moment
/// the editor starts. The matching major mode is
/// [`crate::modes::LspLogMode`].
pub const LSP_SUBSYSTEM_LOG_NAME: &str = "*lsp*";

/// Build the synthetic name for the per-instance LSP log buffer
/// owned by [`crate::modes::LspServerLogMode`].
///
/// Format: `*lsp:<server_id>:<workspace_path>*`. The workspace
/// path is rendered via `Path::display()` (matches the canonical
/// form the registry uses for buffer lookups).
pub fn lsp_server_log_name(instance: &InstanceKey) -> String {
    format!(
        "*lsp:{}:{}*",
        instance.server_id,
        instance.workspace.display(),
    )
}

/// Build the synthetic name for the per-instance LSP trace
/// buffer owned by [`crate::modes::LspTraceLogMode`].
///
/// Format: `*lsp:<server_id>:<workspace_path>:trace*`.
pub fn lsp_server_trace_log_name(instance: &InstanceKey) -> String {
    format!(
        "*lsp:{}:{}:trace*",
        instance.server_id,
        instance.workspace.display(),
    )
}

/// Recover the [`InstanceKey`] from a per-instance log buffer
/// name. Returns `None` when `name` does not match the
/// canonical `*lsp:<server>:<workspace>*` shape.
///
/// Rejects the trace variant — a name ending in `:trace*` parses
/// via [`parse_lsp_trace_log_name`] instead, so the two flavours
/// stay distinct at the type-of-buffer level.
pub fn parse_lsp_server_log_name(name: &str) -> Option<InstanceKey> {
    let body = strip_lsp_wrapping(name)?;
    if body.ends_with(":trace") {
        return None;
    }
    let (server, workspace) = body.split_once(':')?;
    if server.is_empty() || workspace.is_empty() {
        return None;
    }
    Some(InstanceKey::new(
        Arc::<str>::from(server),
        Arc::<Path>::from(Path::new(workspace)),
    ))
}

/// Recover the [`InstanceKey`] from a per-instance trace buffer
/// name. Returns `None` when `name` does not match
/// `*lsp:<server>:<workspace>:trace*`.
pub fn parse_lsp_trace_log_name(name: &str) -> Option<InstanceKey> {
    let body = strip_lsp_wrapping(name)?;
    let trimmed = body.strip_suffix(":trace")?;
    let (server, workspace) = trimmed.split_once(':')?;
    if server.is_empty() || workspace.is_empty() {
        return None;
    }
    Some(InstanceKey::new(
        Arc::<str>::from(server),
        Arc::<Path>::from(Path::new(workspace)),
    ))
}

/// Strip the `*lsp:` prefix + closing `*` wrapper. Returns the
/// inner `<server>:<workspace>[:trace]` body. None if the name
/// is not LSP-shaped or is the subsystem-wide `*lsp*`.
fn strip_lsp_wrapping(name: &str) -> Option<&str> {
    let body = name.strip_prefix("*lsp:")?.strip_suffix('*')?;
    if body.is_empty() {
        return None;
    }
    Some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(server: &str, workspace: &str) -> InstanceKey {
        InstanceKey::new(
            Arc::<str>::from(server),
            Arc::<Path>::from(Path::new(workspace)),
        )
    }

    #[test]
    fn server_log_name_round_trips() {
        let k = key("rust", "/home/u/proj");
        let name = lsp_server_log_name(&k);
        assert_eq!(name, "*lsp:rust:/home/u/proj*");
        let parsed = parse_lsp_server_log_name(&name).expect("parse");
        assert_eq!(parsed, k);
    }

    #[test]
    fn trace_log_name_round_trips() {
        let k = key("python", "/home/u/code");
        let name = lsp_server_trace_log_name(&k);
        assert_eq!(name, "*lsp:python:/home/u/code:trace*");
        let parsed = parse_lsp_trace_log_name(&name).expect("parse");
        assert_eq!(parsed, k);
    }

    #[test]
    fn parsers_reject_subsystem_name() {
        // `*lsp*` has no body — not a per-instance buffer.
        assert!(parse_lsp_server_log_name(LSP_SUBSYSTEM_LOG_NAME).is_none());
        assert!(parse_lsp_trace_log_name(LSP_SUBSYSTEM_LOG_NAME).is_none());
    }

    #[test]
    fn server_parser_rejects_trace_variant() {
        let trace = "*lsp:rust:/p:trace*";
        assert!(parse_lsp_server_log_name(trace).is_none());
    }

    #[test]
    fn trace_parser_rejects_log_variant() {
        let log = "*lsp:rust:/p*";
        assert!(parse_lsp_trace_log_name(log).is_none());
    }

    #[test]
    fn parsers_reject_garbage_names() {
        assert!(parse_lsp_server_log_name("*foo*").is_none());
        assert!(parse_lsp_server_log_name("*lsp:*").is_none());
        assert!(parse_lsp_server_log_name("*lsp::/p*").is_none());
        assert!(parse_lsp_trace_log_name("not-lsp").is_none());
    }
}
