//! MG.9: magit-branch major mode.
//!
//! Lists local branches with checkout/create/delete/merge operations.

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
use lattice_vcs::{Branch, Repository};

use crate::magit_core_mode::ActionRegsGuard;

pub struct MagitBranchMode;

impl MagitBranchMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-branch-mode")
    }
}

fn magit_branch_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Checkout branch", cmd: "action:magit-branch-checkout" },
            keymap_entry! { mode: Normal, chord: "c", doc: "Create branch", cmd: "action:magit-branch-create" },
            keymap_entry! { mode: Normal, chord: "d", doc: "Delete branch", cmd: "action:magit-branch-delete" },
            keymap_entry! { mode: Normal, chord: "m", doc: "Merge branch", cmd: "action:magit-branch-merge" },
        ]
    })
}

struct BranchState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    pending_highlights: Option<lattice_mode::PendingSyntheticHighlightsHandle>,
}

impl Mode for MagitBranchMode {
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
        Keymap::from_entries(magit_branch_keymap_entries())
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

            // Populate branch list: blocking I/O on spawn_blocking, then
            // apply edit on the current task (no Runtime::new()).
            let wd = workdir.clone();
            let text = tokio::task::spawn_blocking(move || build_branch_list(&wd))
                .await
                .unwrap();
            let spans = crate::highlight::branch_styled_spans(&text);
            apply_full_replace(&handle, text).await;
            let pending_highlights = ctx.service::<lattice_mode::PendingSyntheticHighlights>();
            if let Some(ref ph) = pending_highlights {
                ph.store_and_wake(buffer_id, spans);
            }

            let state = Arc::new(Mutex::new(BranchState {
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

            // refresh (gr) — re-list branches. Previously only
            // magit-status supported `gr`; every other magit buffer
            // silently no-op'd on it.
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-refresh") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |_ctx: &ActionContext<'_>| refresh(s.clone())),
                    ));
                }
            }

            // checkout (<CR>)
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-branch-checkout") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |ctx: &ActionContext<'_>| {
                            let (name, workdir) = {
                                let g = s.lock().ok()?;
                                (branch_name_at_cursor(&g, ctx.cursor)?, g.workdir.clone())
                            };
                            spawn_mutation_and_refresh(s.clone(), move || {
                                if let Ok(repo) = Repository::discover(&workdir) {
                                    let _ = Branch::checkout(&repo, &name);
                                }
                            })
                        }),
                    ));
                }
            }

            // delete (d)
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-branch-delete") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |ctx: &ActionContext<'_>| {
                            let (name, workdir) = {
                                let g = s.lock().ok()?;
                                (branch_name_at_cursor(&g, ctx.cursor)?, g.workdir.clone())
                            };
                            spawn_mutation_and_refresh(s.clone(), move || {
                                if let Ok(repo) = Repository::discover(&workdir) {
                                    let _ = Branch::delete(&repo, &name);
                                }
                            })
                        }),
                    ));
                }
            }

            // merge (m)
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-branch-merge") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |ctx: &ActionContext<'_>| {
                            let (name, workdir) = {
                                let g = s.lock().ok()?;
                                (branch_name_at_cursor(&g, ctx.cursor)?, g.workdir.clone())
                            };
                            spawn_mutation_and_refresh(s.clone(), move || {
                                if let Ok(repo) = Repository::discover(&workdir) {
                                    let _ = repo.run_git(["merge", &name]);
                                }
                            })
                        }),
                    ));
                }
            }

            // create (c) — Emacs-magit-style two-step wizard: pick an
            // existing branch as the base via the picker, then a
            // follow-up prompt asks for the new branch's name (see
            // `picker_sources::BranchPickBaseSource` +
            // `action:magit-branch-create-finish` in
            // `magit_global_mode`). The direct `:magit-branch-create
            // <name>` ex-command (creates from HEAD, no base choice)
            // stays available for the scriptable/quick path.
            {
                if let Some(cid) = registry.id_by_name("action:magit-branch-create") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |_ctx: &ActionContext<'_>| {
                            Some(Effect::OpenPicker {
                                source: "magit-branch-pick-base".to_string(),
                                args: Vec::new(),
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

/// `gr` — re-list branches without a prior mutation.
fn refresh(s: Arc<Mutex<BranchState>>) -> Option<Effect> {
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
        let text = tokio::task::spawn_blocking(move || build_branch_list(&wd))
            .await
            .unwrap_or_default();
        let spans = crate::highlight::branch_styled_spans(&text);
        apply_full_replace(&handle, text).await;
        if let Some(ph) = pending {
            ph.store_and_wake(buffer_id, spans);
        }
    });
    None
}

/// Run `mutate` (a blocking git call) on `spawn_blocking`, off the
/// actor thread, then re-list branches — the shape every mutating
/// handler above uses instead of calling git synchronously inline.
fn spawn_mutation_and_refresh(
    s: Arc<Mutex<BranchState>>,
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
        let text = tokio::task::spawn_blocking(move || build_branch_list(&wd))
            .await
            .unwrap_or_default();
        let spans = crate::highlight::branch_styled_spans(&text);
        apply_full_replace(&handle, text).await;
        if let Some(ph) = pending {
            ph.store_and_wake(buffer_id, spans);
        }
    });
    None
}

fn branch_name_at_cursor(state: &BranchState, cursor: Position) -> Option<String> {
    let handle = state.store.handle_for(state.buffer_id)?;
    let snap = handle.snapshot();
    let line = snap.buffer.line(cursor.line)?;
    // Format: "  branch-name" or "* branch-name (current)"
    let name = line
        .trim()
        .trim_start_matches("* ")
        .split_whitespace()
        .next()?;
    Some(name.to_string())
}

fn build_branch_list(workdir: &std::path::Path) -> String {
    let repo = match Repository::discover(workdir) {
        Ok(r) => r,
        Err(_) => return "Not a git repository.\n".to_string(),
    };
    let branches = Branch::list(&repo).unwrap_or_default();
    if branches.is_empty() {
        return "No branches.\n".to_string();
    }

    // Determine current branch
    let current = repo
        .run_git_str(["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let mut out = format!("Branches ({})\n", branches.len());
    for b in &branches {
        let marker = if *b == current { "* " } else { "  " };
        out.push_str(&format!("{}{}\n", marker, b));
    }
    out.push('\n');
    out
}
