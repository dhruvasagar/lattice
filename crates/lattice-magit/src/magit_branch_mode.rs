//! MG.9: magit-branch major mode.
//!
//! Lists local branches with checkout/create/delete/merge operations.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_grammar::{CommandRegistryHandle, Effect};
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerRegistryHandle, BufferStoreHandle, CapabilitySet,
    Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_runtime::Document;
use lattice_vcs::{Branch, Repository};

pub struct MagitBranchMode;

impl MagitBranchMode {
    pub fn mode_id() -> ModeId { ModeId::new("magit-branch-mode") }
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
}

impl Mode for MagitBranchMode {
    type Guard = ();

    fn id(&self) -> ModeId { Self::mode_id() }
    fn kind(&self) -> ModeKind { ModeKind::Major }
    fn target_buffer_kind(&self) -> Option<lattice_core::BufferKind> { None }

    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet { CapabilitySet::empty() }
    fn keymap(&self) -> Keymap { Keymap::from_entries(magit_branch_keymap_entries()) }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else { return Ok(()); };
            let Some(handle) = store.handle_for(buffer_id) else { return Ok(()); };
            let Ok(runtime) = tokio::runtime::Handle::try_current() else { return Ok(()); };

            let workdir = Repository::discover(".")
                .ok()
                .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                .unwrap_or_default();

            let h = handle.clone();
            let wd = workdir.clone();
            runtime.spawn_blocking(move || {
                let text = build_branch_list(&wd);
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    let snap = h.snapshot();
                    let last = snap.buffer.line_count().saturating_sub(1);
                    let last_line = snap.buffer.line(last).unwrap_or_default();
                    let end = Position::new(last, last_line.len() as u32);
                    let _ = h.apply_edit_batch(vec![
                        Edit::replace(Range::new(Position::ZERO, end), text)
                    ]).await;
                });
            });

            let state = Arc::new(Mutex::new(BranchState {
                buffer_id, store: store.clone(), workdir: workdir.clone(),
            }));

            let Some(cmd_arc) = ctx.service::<CommandRegistryHandle>() else { return Ok(()); };
            let Some(ah_arc) = ctx.service::<ActionHandlerRegistryHandle>() else { return Ok(()); };
            let registry = cmd_arc.load();
            let handlers = (*ah_arc).clone();
            let mut regs = Vec::new();

            // checkout (<CR>)
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-branch-checkout") {
                    regs.push(handlers.register(cid, Arc::new(move |ctx: &ActionContext<'_>| {
                        let g = s.lock().ok()?;
                        let name = branch_name_at_cursor(&g, ctx.cursor)?;
                        Branch::checkout(&Repository::discover(&g.workdir).ok()?, &name).ok()?;
                        None
                    })));
                }
            }

            // delete (d)
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-branch-delete") {
                    regs.push(handlers.register(cid, Arc::new(move |ctx: &ActionContext<'_>| {
                        let g = s.lock().ok()?;
                        let name = branch_name_at_cursor(&g, ctx.cursor)?;
                        Branch::delete(&Repository::discover(&g.workdir).ok()?, &name).ok()?;
                        None
                    })));
                }
            }

            // merge (m)
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-branch-merge") {
                    regs.push(handlers.register(cid, Arc::new(move |ctx: &ActionContext<'_>| {
                        let g = s.lock().ok()?;
                        let name = branch_name_at_cursor(&g, ctx.cursor)?;
                        let repo = Repository::discover(&g.workdir).ok()?;
                        repo.run_git(["merge", &name]).ok()?;
                        None
                    })));
                }
            }

            // create (c) — not yet implemented (needs minibuffer prompt)
            {
                if let Some(cid) = registry.id_by_name("action:magit-branch-create") {
                    regs.push(handlers.register(cid, Arc::new(move |_ctx: &ActionContext<'_>| {
                        None
                    })));
                }
            }

            std::mem::forget(regs);
            Ok(())
        })
    }
}

fn branch_name_at_cursor(state: &BranchState, cursor: Position) -> Option<String> {
    let handle = state.store.handle_for(state.buffer_id)?;
    let snap = handle.snapshot();
    let line = snap.buffer.line(cursor.line)?;
    // Format: "  branch-name" or "* branch-name (current)"
    let name = line.trim().trim_start_matches("* ").split_whitespace().next()?;
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
