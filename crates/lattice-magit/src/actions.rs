//! MG.3: magit-status action handlers.
//!
//! Each handler captures shared state from the mode's Guard so it
//! can read the cursor line, resolve the repo, and invoke git
//! operations. Async operations (diff expansion, refresh) use the
//! stored tokio handle — no `Runtime::new()`, no `block_on`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lattice_core::BufferId;
use lattice_grammar::{Effect, QuitScope, CommandRegistryHandle};
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerRegistration, ActionHandlerRegistryHandle,
    BufferStoreHandle, PendingSyntheticHighlights,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_runtime::Document;
use lattice_vcs::{Index, Repository};

use crate::refresh;

pub struct StatusBufferState {
    pub buffer_id: BufferId,
    pub store: Arc<BufferStoreHandle>,
    pub workdir: PathBuf,
    pub runtime: tokio::runtime::Handle,
    /// MG.2: optional handle to store styled spans after async edit
    /// lands, so highlights appear without a keystroke.
    pub pending_highlights: Option<std::sync::Arc<PendingSyntheticHighlights>>,
}

// ── cursor helpers ──────────────────────────────────────

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
    if parts.len() < 2 { return None; }
    let path_str = parts[1].trim();
    if path_str.is_empty() { return None; }
    Some(PathBuf::from(path_str))
}

fn file_path_at_cursor(state: &StatusBufferState, cursor: Position) -> Option<PathBuf> {
    let handle = state.store.handle_for(state.buffer_id)?;
    let snap = handle.snapshot();
    parse_file_path(&snap.buffer.line(cursor.line)?)
}

/// True if the line after `file_line` is diff content.
fn diff_is_expanded(state: &StatusBufferState, file_line: u32) -> Option<bool> {
    let handle = state.store.handle_for(state.buffer_id)?;
    let snap = handle.snapshot();
    let next = snap.buffer.line(file_line + 1)?;
    let t = next.trim();
    Some(t.starts_with("diff --git") || t.starts_with("@@") || t.starts_with("---"))
}

fn section_header_above(state: &StatusBufferState, line: u32) -> Option<String> {
    let handle = state.store.handle_for(state.buffer_id)?;
    let snap = handle.snapshot();
    for l in (0..=line).rev() {
        let text = snap.buffer.line(l)?;
        let t = text.trim();
        if t.starts_with("Staged changes") || t.starts_with("Unstaged changes") {
            return Some(t.to_string());
        }
    }
    None
}

fn diff_line_count(state: &StatusBufferState, file_line: u32) -> Option<usize> {
    let handle = state.store.handle_for(state.buffer_id)?;
    let snap = handle.snapshot();
    let total = snap.buffer.line_count() as u32;
    let mut count = 0usize;
    for l in (file_line + 1)..total {
        let text = snap.buffer.line(l)?;
        let t = text.trim();
        if t.is_empty()
            || t.starts_with("Staged changes")
            || t.starts_with("Unstaged changes")
            || t.starts_with("Untracked files")
            || t.starts_with("Stashes")
            || t.starts_with("Recent commits")
            || parse_file_path(&text).is_some()
        {
            break;
        }
        count += 1;
    }
    Some(count)
}

fn run_diff(workdir: &PathBuf, path: &PathBuf, staged: bool) -> Option<String> {
    let mut args: Vec<&str> = vec!["diff"];
    if staged { args.push("--cached"); }
    args.push("--");
    let ps = path.to_string_lossy();
    let ps: &str = &ps;
    args.push(ps);
    let output = std::process::Command::new("git")
        .args(&args)
        .current_dir(workdir)
        .output()
        .ok()?;
    if output.status.success() {
        String::from_utf8(output.stdout).ok()
    } else {
        None
    }
}

// ── registration ────────────────────────────────────────

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
                registrations.push(handlers.register(cmd_id, Arc::new($body)));
            }
        };
    }

    /// MG.2: helper that reads state, spawns blocking I/O on
    /// spawn_blocking, then applies the edit + stores highlights on a
    /// tokio task. No `Runtime::new()` — the current runtime is used.
    let trigger_refresh = |s: Arc<Mutex<StatusBufferState>>| {
        let (handle, wd, pending, bid) = {
            let g = s.lock().ok()?;
            let h = g.store.handle_for(g.buffer_id)?;
            (h, g.workdir.clone(), g.pending_highlights.clone(), g.buffer_id)
        };
        tokio::task::spawn(async move {
            let (text, spans) = tokio::task::spawn_blocking(move || {
                refresh::build_and_format(&wd)
            })
            .await
            .expect("spawn_blocking");
            refresh::apply_and_highlight(handle, text, spans, pending, bid).await;
        });
        None::<Effect>
    };

    // ── stage (s) ──────────────────────────────────────
    {
        let s = state.clone();
        handler!("action:magit-stage", move |ctx: &ActionContext<'_>| {
            let g = s.lock().ok()?;
            let path = file_path_at_cursor(&g, ctx.cursor)?;
            let repo = Repository::discover(&g.workdir).ok()?;
            Index::stage_path(&repo, &path).ok()?;
            drop(g);
            trigger_refresh(s.clone())
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
            drop(g);
            trigger_refresh(s.clone())
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
            drop(g);
            trigger_refresh(s.clone())
        });
    }

    // ── visit (<CR>) ──────────────────────────────────
    {
        let s = state.clone();
        handler!("action:magit-visit", move |ctx: &ActionContext<'_>| {
            let g = s.lock().ok()?;
            let path = file_path_at_cursor(&g, ctx.cursor)?;
            let full = g.workdir.join(&path);
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

    // ── commit amend (ca) ─────────────────────────────
    {
        handler!("action:magit-commit-amend", move |_ctx: &ActionContext<'_>| {
            Some(Effect::OpenSyntheticBuffer {
                name: "*magit:amend*".to_string(),
                mode_id: "magit-commit-mode".to_string(),
            })
        });
    }

    // ── stage patch (p) ───────────────────────────────
    {
        let s = state.clone();
        handler!("action:magit-stage-patch", move |ctx: &ActionContext<'_>| {
            let g = s.lock().ok()?;
            let path = file_path_at_cursor(&g, ctx.cursor)?;
            let repo = Repository::discover(&g.workdir).ok()?;
            repo.run_git(["add", "-p", "--", &path.to_string_lossy()]).ok()?;
            drop(g);
            trigger_refresh(s.clone())
        });
    }

    // ── refresh (gr) ──────────────────────────────────
    {
        let s = state.clone();
        handler!("action:magit-refresh", move |_ctx: &ActionContext<'_>| {
            trigger_refresh(s.clone())
        });
    }

    // ── toggle diff (=) ───────────────────────────────
    {
        let s = state.clone();
        handler!("action:magit-toggle-diff", move |ctx: &ActionContext<'_>| {
            let (handle, wd, rt, expanded_opt, pending) = {
                let g = s.lock().ok()?;
                let h = g.store.handle_for(g.buffer_id)?;
                let expanded = diff_is_expanded(&g, ctx.cursor.line).unwrap_or(false);
                (h, g.workdir.clone(), g.runtime.clone(), expanded, g.pending_highlights.clone())
            };
            let path = {
                let g = s.lock().ok()?;
                file_path_at_cursor(&g, ctx.cursor)?
            };

            if expanded_opt {
                let count = {
                    let g = s.lock().ok()?;
                    diff_line_count(&g, ctx.cursor.line).unwrap_or(0)
                };
                if count > 0 {
                    let snap = handle.snapshot();
                    let start = Position::new(ctx.cursor.line + 1, 0);
                    let end_line = (ctx.cursor.line + 1 + count as u32)
                        .min(snap.buffer.line_count().saturating_sub(1) as u32);
                    let end_line_text = snap.buffer.line(end_line).unwrap_or_default();
                    let end = Position::new(end_line, end_line_text.len() as u32);
                    rt.spawn(async move {
                        let _ = handle.apply_edit_batch(vec![
                            Edit::replace(Range::new(start, end), String::new()),
                        ]).await;
                        if let Some(ref ph) = pending {
                            ph.wake();
                        }
                    });
                }
            } else {
                let staged = {
                    let g = s.lock().ok()?;
                    section_header_above(&g, ctx.cursor.line)
                        .map(|h| h.contains("Staged"))
                        .unwrap_or(false)
                };
                let diff = run_diff(&wd, &path, staged).unwrap_or_default();
                if !diff.trim().is_empty() {
                    let pos = Position::new(ctx.cursor.line + 1, 0);
                    rt.spawn(async move {
                        let _ = handle
                            .apply_edit_batch(vec![
                                Edit::insert(pos, format!("\n{}\n", diff.trim())),
                            ])
                            .await;
                        if let Some(ref ph) = pending {
                            ph.wake();
                        }
                    });
                }
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
