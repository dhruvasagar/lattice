//! MG.3: magit-status action handlers.
//!
//! Each chord (s, u, x, cc, ca, =, p, <CR>, gr, q) is registered
//! as a per-buffer action handler in `on_activate`. The handler
//! reads the cursor line from the buffer, parses the file path,
//! resolves the repo, and invokes the appropriate git operation.
//!
//! Mode-ownership acid test: every chord AND handler body lives
//! in `lattice-magit`. Zero `Editor::do_magit_*` methods.

use std::path::PathBuf;
use std::sync::Arc;

use lattice_core::BufferId;
use lattice_grammar::{Effect, QuitScope, CommandRegistryHandle};
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerRegistration, ActionHandlerRegistryHandle,
    BufferStoreHandle,
};
use lattice_protocol::position::Position;
use lattice_vcs::{Index, Repository};

/// Parse a file path from a section entry line.
/// Format: `  <status-label>  <path>`
fn parse_file_path(line: &str) -> Option<PathBuf> {
    let trimmed = line.trim();
    if trimmed.is_empty()
        || trimmed.ends_with(')')
        || trimmed.starts_with("stash@")
        || trimmed.contains("No changes")
        || trimmed.contains("Not a git repository")
    {
        return None;
    }
    let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
    if parts.len() < 2 {
        return None;
    }
    let path_str = parts[1].trim();
    if path_str.is_empty() {
        return None;
    }
    Some(PathBuf::from(path_str))
}

/// Read the cursor line from the buffer, parse a file path from it.
fn file_path_at_cursor(
    store: &BufferStoreHandle,
    buffer_id: BufferId,
    cursor: Position,
) -> Option<PathBuf> {
    let handle = store.handle_for(buffer_id)?;
    let snap = handle.snapshot();
    let line = snap.buffer.line(cursor.line)?;
    parse_file_path(&line)
}

/// Register all magit-status (+ magit-core) action handlers for
/// the given buffer. Returns RAII registrations that auto-unregister
/// on drop.
pub fn register_action_handlers(
    buffer_id: BufferId,
    store: &BufferStoreHandle,
    cmd_registry_arc: &Arc<CommandRegistryHandle>,
    action_handlers_arc: &Arc<ActionHandlerRegistryHandle>,
    workdir: PathBuf,
) -> Vec<ActionHandlerRegistration> {
    let mut registrations = Vec::new();
    let registry = cmd_registry_arc.load();
    let handlers = (*action_handlers_arc).clone();

    // ── stage (s) ──────────────────────────────────────
    {
        let store = (*store).clone();
        let wd = workdir.clone();
        if let Some(cmd_id) = registry.id_by_name("action:magit-stage") {
            let handler: ActionHandler = Arc::new(move |ctx: &ActionContext<'_>| {
                let path = file_path_at_cursor(&store, buffer_id, ctx.cursor)?;
                let repo = Repository::discover(&wd).ok()?;
                Index::stage_path(&repo, &path).ok()?;
                None
            });
            registrations.push(handlers.register(cmd_id, handler));
        }
    }

    // ── unstage (u) ───────────────────────────────────
    {
        let store = (*store).clone();
        let wd = workdir.clone();
        if let Some(cmd_id) = registry.id_by_name("action:magit-unstage") {
            let handler: ActionHandler = Arc::new(move |ctx: &ActionContext<'_>| {
                let path = file_path_at_cursor(&store, buffer_id, ctx.cursor)?;
                let repo = Repository::discover(&wd).ok()?;
                Index::unstage_path(&repo, &path).ok()?;
                None
            });
            registrations.push(handlers.register(cmd_id, handler));
        }
    }

    // ── discard (x) ───────────────────────────────────
    {
        let store = (*store).clone();
        let wd = workdir.clone();
        if let Some(cmd_id) = registry.id_by_name("action:magit-discard") {
            let handler: ActionHandler = Arc::new(move |ctx: &ActionContext<'_>| {
                let path = file_path_at_cursor(&store, buffer_id, ctx.cursor)?;
                let repo = Repository::discover(&wd).ok()?;
                repo.run_git(["checkout", "--", &path.to_string_lossy()])
                    .ok()?;
                None
            });
            registrations.push(handlers.register(cmd_id, handler));
        }
    }

    // ── visit (<CR>) ──────────────────────────────────
    {
        let store = (*store).clone();
        let wd = workdir.clone();
        if let Some(cmd_id) = registry.id_by_name("action:magit-visit") {
            let handler: ActionHandler = Arc::new(move |ctx: &ActionContext<'_>| {
                let path = file_path_at_cursor(&store, buffer_id, ctx.cursor)?;
                let full = if wd.is_absolute() {
                    wd.join(&path)
                } else {
                    std::env::current_dir().ok()?.join(&wd).join(&path)
                };
                if full.exists() {
                    Some(Effect::OpenBuffer {
                        path: Some(full),
                        force: false,
                    })
                } else {
                    None
                }
            });
            registrations.push(handlers.register(cmd_id, handler));
        }
    }

    // ── refresh (gr) ──────────────────────────────────
    {
        if let Some(cmd_id) = registry.id_by_name("action:magit-refresh") {
            let handler: ActionHandler = Arc::new(move |_ctx: &ActionContext<'_>| {
                // No-op for now — the buffer will refresh on next
                // manual :magit-status or auto-refresh event.
                None
            });
            registrations.push(handlers.register(cmd_id, handler));
        }
    }

    // ── close (q) ─────────────────────────────────────
    {
        if let Some(cmd_id) = registry.id_by_name("action:magit-close") {
            let handler: ActionHandler = Arc::new(move |_ctx: &ActionContext<'_>| {
                Some(Effect::QuitEditor {
                    force: false,
                    scope: QuitScope::Pane,
                })
            });
            registrations.push(handlers.register(cmd_id, handler));
        }
    }

    // ── commit (cc) — MG.4 ────────────────────────────
    {
        if let Some(cmd_id) = registry.id_by_name("action:magit-commit") {
            let handler: ActionHandler =
                Arc::new(move |_ctx: &ActionContext<'_>| {
                    // MG.4: opens *magit:commit* buffer
                    Some(Effect::OpenSyntheticBuffer {
                        name: "*magit:commit*".to_string(),
                        mode_id: "magit-commit-mode".to_string(),
                    })
                });
            registrations.push(handlers.register(cmd_id, handler));
        }
    }

    registrations
}
