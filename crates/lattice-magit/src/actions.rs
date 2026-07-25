//! MG.3: magit-status action handlers.
//!
//! Registered per-buffer in `on_activate`. Each handler captures
//! shared state (`buffer_id`, `BufferStoreHandle`, `workdir`) from
//! the mode's Guard so it can read the cursor line, resolve the repo,
//! and invoke git operations at chord-press time.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lattice_core::BufferId;
use lattice_grammar::{Effect, QuitScope, CommandRegistryHandle};
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerRegistration, ActionHandlerRegistryHandle,
    BufferStoreHandle,
};
use lattice_protocol::position::Position;
use lattice_runtime::Document;
use lattice_vcs::{Index, Repository};

use crate::refresh;

/// Shared state each action handler reads at chord-press time.
pub struct StatusBufferState {
    pub buffer_id: BufferId,
    pub store: Arc<BufferStoreHandle>,
    pub workdir: PathBuf,
}

/// Parse a file path from a section entry line.
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
    if path_str.is_empty() { return None; }
    Some(PathBuf::from(path_str))
}

fn file_path_at_cursor(state: &StatusBufferState, cursor: Position) -> Option<PathBuf> {
    let handle = state.store.handle_for(state.buffer_id)?;
    let snap = handle.snapshot();
    parse_file_path(&snap.buffer.line(cursor.line)?)
}

/// Register all action handlers for the status buffer.
pub fn register_action_handlers(
    state: Arc<Mutex<StatusBufferState>>,
    cmd_registry_arc: &Arc<CommandRegistryHandle>,
    action_handlers_arc: &Arc<ActionHandlerRegistryHandle>,
) -> Vec<ActionHandlerRegistration> {
    let mut registrations = Vec::new();
    let registry = cmd_registry_arc.load();
    let handlers = (*action_handlers_arc).clone();

    macro_rules! handler {
        ($name:expr, $body:expr) => {
            if let Some(cmd_id) = registry.id_by_name($name) {
                let h: ActionHandler = Arc::new($body);
                registrations.push(handlers.register(cmd_id, h));
            }
        };
    }

    // ── stage (s) ──────────────────────────────────────
    {
        let s = state.clone();
        handler!("action:magit-stage", move |ctx: &ActionContext<'_>| {
            let g = s.lock().ok()?;
            let path = file_path_at_cursor(&g, ctx.cursor)?;
            let repo = Repository::discover(&g.workdir).ok()?;
            Index::stage_path(&repo, &path).ok()?;
            None
        });
    }

    // ── unstage (u) ───────────────────────────────────
    {
        let s = state.clone();
        handler!("action:magit-unstage", move |ctx: &ActionContext<'_>| {
            let g = s.lock().ok()?;
            let path = file_path_at_cursor(&g, ctx.cursor)?;
            let repo = Repository::discover(&g.workdir).ok()?;
            Index::unstage_path(&repo, &path).ok()?;
            None
        });
    }

    // ── discard (x) ───────────────────────────────────
    {
        let s = state.clone();
        handler!("action:magit-discard", move |ctx: &ActionContext<'_>| {
            let g = s.lock().ok()?;
            let path = file_path_at_cursor(&g, ctx.cursor)?;
            let repo = Repository::discover(&g.workdir).ok()?;
            repo.run_git(["checkout", "--", &path.to_string_lossy()]).ok()?;
            None
        });
    }

    // ── visit (<CR>) ──────────────────────────────────
    {
        let s = state.clone();
        handler!("action:magit-visit", move |ctx: &ActionContext<'_>| {
            let g = s.lock().ok()?;
            let path = file_path_at_cursor(&g, ctx.cursor)?;
            let full = if g.workdir.is_absolute() {
                g.workdir.join(&path)
            } else {
                std::env::current_dir().ok()?.join(&g.workdir).join(&path)
            };
            if full.exists() {
                Some(Effect::OpenBuffer { path: Some(full), force: false })
            } else {
                None
            }
        });
    }

    // ── commit (cc) ───────────────────────────────────
    {
        handler!("action:magit-commit", move |_ctx: &ActionContext<'_>| {
            Some(Effect::OpenSyntheticBuffer {
                name: "*magit:commit*".to_string(),
                mode_id: "magit-commit-mode".to_string(),
            })
        });
    }

    // ── refresh (gr) ──────────────────────────────────
    {
        let s = state.clone();
        handler!("action:magit-refresh", move |_ctx: &ActionContext<'_>| {
            let (buffer_id, handle, wd) = {
                let g = s.lock().ok()?;
                let h = g.store.handle_for(g.buffer_id)?;
                (g.buffer_id, h, g.workdir.clone())
            };
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn_blocking(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(refresh::refresh_status(buffer_id, handle, wd));
                });
            }
            None
        });
    }

    // ── toggle diff (=) ───────────────────────────────
    {
        let s = state.clone();
        handler!("action:magit-toggle-diff", move |_ctx: &ActionContext<'_>| {
            let (buffer_id, handle, wd) = {
                let g = s.lock().ok()?;
                let h = g.store.handle_for(g.buffer_id)?;
                (g.buffer_id, h, g.workdir.clone())
            };
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn_blocking(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(refresh::refresh_status(buffer_id, handle, wd));
                });
            }
            None
        });
    }

    // ── close (q) ─────────────────────────────────────
    {
        handler!("action:magit-close", move |_ctx: &ActionContext<'_>| {
            Some(Effect::QuitEditor { force: false, scope: QuitScope::Pane })
        });
    }

    registrations
}
