//! MG.9: magit-stash major mode.
//!
//! Lists stash entries with apply/pop/drop/create operations.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_grammar::{CommandRegistryHandle, Effect};
use lattice_mode::{
    ActionContext, ActionHandlerRegistryHandle, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_vcs::{Repository, Stash};

use crate::magit_core_mode::ActionRegsGuard;

pub struct MagitStashMode;

impl MagitStashMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-stash-mode")
    }
}

fn magit_stash_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "a", doc: "Apply stash", cmd: "action:magit-stash-apply" },
            keymap_entry! { mode: Normal, chord: "p", doc: "Pop stash", cmd: "action:magit-stash-pop" },
            keymap_entry! { mode: Normal, chord: "d", doc: "Drop stash", cmd: "action:magit-stash-drop" },
            keymap_entry! { mode: Normal, chord: "z", doc: "Create stash", cmd: "action:magit-stash-create" },
        ]
    })
}

struct StashState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    pending_highlights: Option<lattice_mode::PendingSyntheticHighlightsHandle>,
}

impl Mode for MagitStashMode {
    type Guard = ActionRegsGuard;

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
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_stash_keymap_entries())
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(ActionRegsGuard::default());
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(ActionRegsGuard::default());
            };
            let workdir = Repository::discover(".")
                .ok()
                .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                .unwrap_or_default();

            // Populate stash list: blocking I/O on spawn_blocking, then
            // apply edit on the current task (no Runtime::new()).
            let wd = workdir.clone();
            let text = tokio::task::spawn_blocking(move || build_stash_list(&wd))
                .await
                .unwrap();
            let spans = crate::highlight::stash_styled_spans(&text);
            apply_full_replace(&handle, text).await;
            let pending_highlights = ctx.service::<lattice_mode::PendingSyntheticHighlights>();
            if let Some(ref ph) = pending_highlights {
                ph.store_and_wake(buffer_id, spans);
            }

            let state = Arc::new(Mutex::new(StashState {
                buffer_id,
                store: store.clone(),
                workdir: workdir.clone(),
                pending_highlights,
            }));

            let Some(cmd_arc) = ctx.service::<CommandRegistryHandle>() else {
                return Ok(ActionRegsGuard::default());
            };
            let Some(ah_arc) = ctx.service::<ActionHandlerRegistryHandle>() else {
                return Ok(ActionRegsGuard::default());
            };
            let registry = cmd_arc.load();
            let handlers = (*ah_arc).clone();
            let mut regs = Vec::new();

            // refresh (gr) — re-list stashes. Previously only
            // magit-status supported `gr`.
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-refresh") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |_ctx: &ActionContext<'_>| refresh(s.clone())),
                    ));
                }
            }

            // apply (a)
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-stash-apply") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |ctx: &ActionContext<'_>| {
                            let (idx, workdir) = {
                                let g = s.lock().ok()?;
                                (stash_index_at_cursor(&g, ctx.cursor)?, g.workdir.clone())
                            };
                            spawn_mutation_and_refresh(s.clone(), move || {
                                if let Ok(repo) = Repository::discover(&workdir) {
                                    let _ = Stash::apply(&repo, idx);
                                }
                            })
                        }),
                    ));
                }
            }

            // pop (p)
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-stash-pop") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |ctx: &ActionContext<'_>| {
                            let (idx, workdir) = {
                                let g = s.lock().ok()?;
                                (stash_index_at_cursor(&g, ctx.cursor)?, g.workdir.clone())
                            };
                            spawn_mutation_and_refresh(s.clone(), move || {
                                if let Ok(repo) = Repository::discover(&workdir) {
                                    let _ = Stash::pop(&repo, idx);
                                }
                            })
                        }),
                    ));
                }
            }

            // drop (d)
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-stash-drop") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |ctx: &ActionContext<'_>| {
                            let (idx, workdir) = {
                                let g = s.lock().ok()?;
                                (stash_index_at_cursor(&g, ctx.cursor)?, g.workdir.clone())
                            };
                            spawn_mutation_and_refresh(s.clone(), move || {
                                if let Ok(repo) = Repository::discover(&workdir) {
                                    let _ = Stash::drop(&repo, idx);
                                }
                            })
                        }),
                    ));
                }
            }

            // create (z)
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-stash-create") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |_ctx: &ActionContext<'_>| {
                            let workdir = { s.lock().ok()?.workdir.clone() };
                            spawn_mutation_and_refresh(s.clone(), move || {
                                if let Ok(repo) = Repository::discover(&workdir) {
                                    let _ = Stash::create(&repo, None, false);
                                }
                            })
                        }),
                    ));
                }
            }

            Ok(ActionRegsGuard(regs))
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

/// `gr` — re-list stashes without a prior mutation.
fn refresh(s: Arc<Mutex<StashState>>) -> Option<Effect> {
    let (handle, wd, pending, buffer_id) = {
        let g = s.lock().ok()?;
        (
            g.store.handle_for(g.buffer_id)?,
            g.workdir.clone(),
            g.pending_highlights.clone(),
            g.buffer_id,
        )
    };
    tokio::task::spawn(async move {
        let text = tokio::task::spawn_blocking(move || build_stash_list(&wd))
            .await
            .unwrap_or_default();
        let spans = crate::highlight::stash_styled_spans(&text);
        apply_full_replace(&handle, text).await;
        if let Some(ph) = pending {
            ph.store_and_wake(buffer_id, spans);
        }
    });
    None
}

/// Run `mutate` (a blocking git call) on `spawn_blocking`, off the
/// actor thread, then re-list stashes — the shape every mutating
/// handler above uses instead of calling git synchronously inline.
fn spawn_mutation_and_refresh(
    s: Arc<Mutex<StashState>>,
    mutate: impl FnOnce() + Send + 'static,
) -> Option<Effect> {
    let (handle, wd, pending, buffer_id) = {
        let g = s.lock().ok()?;
        (
            g.store.handle_for(g.buffer_id)?,
            g.workdir.clone(),
            g.pending_highlights.clone(),
            g.buffer_id,
        )
    };
    tokio::task::spawn(async move {
        let _ = tokio::task::spawn_blocking(mutate).await;
        let text = tokio::task::spawn_blocking(move || build_stash_list(&wd))
            .await
            .unwrap_or_default();
        let spans = crate::highlight::stash_styled_spans(&text);
        apply_full_replace(&handle, text).await;
        if let Some(ph) = pending {
            ph.store_and_wake(buffer_id, spans);
        }
    });
    None
}

fn stash_index_at_cursor(state: &StashState, cursor: Position) -> Option<usize> {
    let handle = state.store.handle_for(state.buffer_id)?;
    let snap = handle.snapshot();
    let line = snap.buffer.line(cursor.line)?;
    // Format: "  stash@{N} message"
    let trimmed = line.trim();
    if let Some(idx_str) = trimmed
        .strip_prefix("stash@{")
        .and_then(|s| s.split('}').next())
    {
        idx_str.parse().ok()
    } else {
        None
    }
}

fn build_stash_list(workdir: &std::path::Path) -> String {
    let repo = match Repository::discover(workdir) {
        Ok(r) => r,
        Err(_) => return "Not a git repository.\n".to_string(),
    };
    let stashes = Stash::list(&repo).unwrap_or_default();
    if stashes.is_empty() {
        return "No stashes.\n".to_string();
    }
    let mut out = format!("Stashes ({})\n", stashes.len());
    for s in &stashes {
        out.push_str(&format!("  {}\n", s.message));
    }
    out.push('\n');
    out
}
