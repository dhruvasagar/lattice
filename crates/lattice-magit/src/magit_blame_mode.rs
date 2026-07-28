//! MG.7: magit-blame major mode.
//!
//! Runs `git blame --line-porcelain <path>` on open, populates
//! buffer with annotated content. <CR> shows commit, p re-blames
//! at the parent of whatever revision is currently blamed.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_grammar::Effect;
use lattice_mode::{
    ActionContext, ActionHandlerContribution, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_vcs::Repository;

use crate::buffer_state::{BufferStateGuard, BufferStates};
use crate::headerline::{self, MagitHeaderlineHandle};

pub struct MagitBlameMode;

impl MagitBlameMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-blame-mode")
    }
}

fn magit_blame_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Show commit for blamed line", cmd: "action:magit-blame-show-commit" },
            keymap_entry! { mode: Normal, chord: "p", doc: "Re-blame at parent commit", cmd: "action:magit-blame-parent" },
        ]
    })
}

pub struct BlameState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    path: String,
    /// The revision currently being blamed — `p` walks this back to
    /// its parent. Starts at "HEAD" (equivalent to blaming the
    /// working tree's current checkout).
    rev: String,
    pending_highlights: Option<lattice_mode::PendingSyntheticHighlightsHandle>,
    /// MG.14: the buffer's headerline. `p` walks `rev` back to its
    /// parent; without the header there is no way to tell how far
    /// back you have walked.
    headerline: Option<MagitHeaderlineHandle>,
}

/// MG.13: service alias for this mode's per-buffer state
/// (`feedback_servicesregistry_arc_typeid`).
pub type BlameStatesHandle = Arc<BufferStates<BlameState>>;

fn state(ctx: &ActionContext<'_>) -> Option<Arc<Mutex<BlameState>>> {
    crate::buffer_state::state_for::<BlameState>(ctx)
}

impl Mode for MagitBlameMode {
    type Guard = BufferStateGuard<BlameState>;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    fn target_buffer_kind(&self) -> Option<lattice_core::BufferKind> {
        None
    }

    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
            lattice_config::Number = false,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_blame_keymap_entries())
    }

    /// MG.13: boot-registered — see `buffer_state`'s module docs.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            // <CR> — show the commit for the blamed line at cursor.
            ActionHandlerContribution {
                action_name: "action:magit-blame-show-commit",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let g = s.lock().ok()?;
                    let handle = g.store.handle_for(g.buffer_id)?;
                    let snap = handle.snapshot();
                    let line = snap.buffer.line(ctx.cursor.line)?;
                    let sha = line.get(0..8)?;
                    if sha.trim().is_empty() || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
                        return None;
                    }
                    Some(Effect::OpenSyntheticBuffer {
                        name: format!("*magit:commit:{sha}*"),
                        mode_id: "magit-revision-mode".to_string(),
                    })
                }),
            },
            // p — re-blame at the parent of the revision currently shown.
            ActionHandlerContribution {
                action_name: "action:magit-blame-parent",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let (handle, wd, path, rev, pending, buffer_id, hl) = {
                        let g = s.lock().ok()?;
                        (
                            g.store.handle_for(g.buffer_id)?,
                            g.workdir.clone(),
                            g.path.clone(),
                            g.rev.clone(),
                            g.pending_highlights.clone(),
                            g.buffer_id,
                            g.headerline.clone(),
                        )
                    };
                    let s2 = s.clone();
                    tokio::task::spawn(async move {
                        let wd2 = wd.clone();
                        let rev_for_lookup = rev.clone();
                        let parent = tokio::task::spawn_blocking(move || {
                            resolve_parent(&wd2, &rev_for_lookup)
                        })
                        .await
                        .ok()
                        .flatten();
                        let Some(parent) = parent else {
                            tracing::debug!(
                                target: "lattice_magit",
                                "blame: {rev} has no parent — already at the root commit",
                            );
                            return;
                        };
                        if let Ok(mut g) = s2.lock() {
                            g.rev = parent.clone();
                        }
                        // MG.14: the header is the only place the
                        // walked-to revision is visible — the blame
                        // body itself looks identical at every step.
                        headerline::publish(&hl, headerline::blame_fields(&path, &parent));
                        let wd3 = wd.clone();
                        let path2 = path.clone();
                        let parent2 = parent.clone();
                        let text =
                            tokio::task::spawn_blocking(move || run_blame(&wd3, &parent2, &path2))
                                .await
                                .unwrap_or_default();
                        let spans = crate::highlight::blame_styled_spans(&text);
                        apply_full_replace(&handle, text).await;
                        if let Some(ph) = pending {
                            ph.store_and_wake(buffer_id, spans);
                        }
                    });
                    None
                }),
            },
        ]
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let orphan = || BufferStateGuard::new(Arc::new(BufferStates::default()), buffer_id);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(orphan());
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(orphan());
            };
            let workdir = Repository::discover(".")
                .ok()
                .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                .unwrap_or_default();

            // Extract the blamed file path from the buffer name:
            // "*magit:blame:<path>*" → "<path>"
            let file_path = store
                .name_for(buffer_id)
                .and_then(|name| {
                    let s = name.strip_prefix("*magit:blame:")?;
                    Some(s.strip_suffix("*")?.to_string())
                })
                .unwrap_or_else(|| ".".to_string());

            let pending_highlights = ctx.service::<lattice_mode::PendingSyntheticHighlights>();

            // MG.14: path and revision are both known here, so the
            // header is complete before the blame itself runs.
            let (hl, hl_registration) =
                match headerline::install(&ctx, buffer_id, Self::mode_id().as_str()) {
                    Some((h, reg)) => (Some(h), Some(reg)),
                    None => (None, None),
                };
            headerline::publish(&hl, headerline::blame_fields(&file_path, "HEAD"));

            // MG.13: publish BEFORE the first `.await` — see the note
            // in `magit_branch_mode::on_activate`.
            let Some(states) = ctx.service::<BlameStatesHandle>() else {
                return Ok(orphan());
            };
            states.publish(
                buffer_id,
                BlameState {
                    buffer_id,
                    store: store.clone(),
                    workdir: workdir.clone(),
                    path: file_path.clone(),
                    rev: "HEAD".to_string(),
                    pending_highlights: pending_highlights.clone(),
                    headerline: hl.clone(),
                },
            );
            let guard = BufferStateGuard::new((*states).clone(), buffer_id)
                .with_headerline(hl_registration);

            // Populate blame: blocking I/O on spawn_blocking, then apply
            // edit on the current task (no Runtime::new()).
            let wd = workdir.clone();
            let fp = file_path.clone();
            let text = tokio::task::spawn_blocking(move || run_blame(&wd, "HEAD", &fp))
                .await
                .unwrap();
            let spans = crate::highlight::blame_styled_spans(&text);
            apply_full_replace(&handle, text).await;
            if let Some(ref ph) = pending_highlights {
                ph.store_and_wake(buffer_id, spans);
            }

            Ok(guard)
        })
    }
}

async fn apply_full_replace(handle: &Arc<dyn lattice_runtime::Document>, text: String) {
    let snap = handle.snapshot();
    let last = snap.buffer.line_count().saturating_sub(1);
    let last_line = snap.buffer.line(last).unwrap_or_default();
    let end = Position::new(last, last_line.len() as u32);
    let _ = handle
        .apply_edit_batch(vec![Edit::replace(Range::new(Position::ZERO, end), text)])
        .await;
}

/// Resolve `<rev>^`'s commit sha — `None` if `rev` has no parent
/// (the root commit) or resolution otherwise fails.
fn resolve_parent(workdir: &std::path::Path, rev: &str) -> Option<String> {
    let repo = Repository::discover(workdir).ok()?;
    repo.run_git_str(["rev-parse", &format!("{rev}^")])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn run_blame(workdir: &std::path::Path, rev: &str, path: &str) -> String {
    if path.is_empty() || path == "." {
        return "No file to blame — open :magit-blame <file> or run from a file buffer.\n"
            .to_string();
    }
    let output = std::process::Command::new("git")
        .args(["blame", "--line-porcelain", rev, "--", path])
        .current_dir(workdir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| format!("Could not blame {}\n", path));

    // Format blame output: extract author + SHA per line
    let mut result = String::new();
    for line in output.lines() {
        if line.len() < 40 {
            continue;
        }
        let sha: String = line.chars().take(8).collect();
        // porcelain format — the interesting lines are author, committer-time, filename
        if line.starts_with("author ") {
            let author = line.strip_prefix("author ").unwrap_or("?");
            result.push_str(&format!("{} {:>12}  ", sha, author));
        } else if line.starts_with("\t") {
            result.push_str(line.strip_prefix("\t").unwrap_or(""));
            result.push('\n');
        }
    }

    if result.is_empty() {
        format!("No blame data for {}\n", path)
    } else {
        result
    }
}
