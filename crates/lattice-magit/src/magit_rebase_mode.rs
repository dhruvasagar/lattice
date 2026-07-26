//! MG.9: magit-rebase major mode.
//!
//! Editable interactive rebase todo buffer. C-c C-c runs rebase,
//! C-c C-k aborts.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_grammar::{CommandRegistryHandle, Effect, QuitScope};
use lattice_mode::{
    ActionContext, ActionHandlerRegistryHandle, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_vcs::Repository;

pub struct MagitRebaseMode;

impl MagitRebaseMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-rebase-mode")
    }
}

fn magit_rebase_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Insert, chord: "<C-c><C-c>", doc: "Execute rebase", cmd: "action:magit-rebase-confirm" },
            keymap_entry! { mode: Insert, chord: "<C-c><C-k>", doc: "Abort rebase", cmd: "action:magit-rebase-abort" },
            keymap_entry! { mode: Normal, chord: "<C-c><C-c>", doc: "Execute rebase", cmd: "action:magit-rebase-confirm" },
            keymap_entry! { mode: Normal, chord: "<C-c><C-k>", doc: "Abort rebase", cmd: "action:magit-rebase-abort" },
        ]
    })
}

struct RebaseState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
}

impl Mode for MagitRebaseMode {
    type Guard = ();

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
            lattice_config::NoFile = true,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_rebase_keymap_entries())
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(());
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(());
            };

            let workdir = Repository::discover(".")
                .ok()
                .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                .unwrap_or_default();

            // Populate with sample todo list
            let initial = "pick abc1234 First commit\n\
                           pick def5678 Second commit\n\
                           pick ghi9012 Third commit\n\
                           \n\
                           # Rebase todo — edit the list above, then C-c C-c\n\
                           # Commands: pick, reword, squash, fixup, drop\n";
            let snap = handle.snapshot();
            let last = snap.buffer.line_count().saturating_sub(1);
            let last_line = snap.buffer.line(last).unwrap_or_default();
            let end = Position::new(last, last_line.len() as u32);
            let _ = handle
                .apply_edit_batch(vec![Edit::replace(
                    Range::new(Position::ZERO, end),
                    initial.to_string(),
                )])
                .await;

            let state = Arc::new(Mutex::new(RebaseState {
                buffer_id,
                store: store.clone(),
                workdir,
            }));

            let Some(cmd_arc) = ctx.service::<CommandRegistryHandle>() else {
                return Ok(());
            };
            let Some(ah_arc) = ctx.service::<ActionHandlerRegistryHandle>() else {
                return Ok(());
            };
            let registry = cmd_arc.load();
            let handlers = (*ah_arc).clone();
            let mut regs = Vec::new();

            // confirm (C-c C-c)
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-rebase-confirm") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |_ctx: &ActionContext<'_>| {
                            let g = s.lock().ok()?;
                            let handle = g.store.handle_for(g.buffer_id)?;
                            let snap = handle.snapshot();
                            // Collect todo lines, skipping comments
                            let mut todo = String::new();
                            for l in 0..snap.buffer.line_count() as u32 {
                                let text = snap.buffer.line(l).unwrap_or_default();
                                if text.starts_with('#') {
                                    continue;
                                }
                                if text.trim().is_empty() {
                                    continue;
                                }
                                todo.push_str(&text);
                                todo.push('\n');
                            }
                            let repo = Repository::discover(&g.workdir).ok()?;
                            // Write todo to GIT_DIR/rebase-merge/git-rebase-todo
                            let gitdir = repo.gitdir().to_path_buf();
                            let todo_path = gitdir.join("rebase-merge").join("git-rebase-todo");
                            if let Some(parent) = todo_path.parent() {
                                std::fs::create_dir_all(parent).ok()?;
                            }
                            std::fs::write(&todo_path, todo).ok()?;
                            // Run rebase --continue
                            repo.run_git(["rebase", "--continue"]).ok()?;
                            Some(Effect::QuitEditor {
                                force: false,
                                scope: QuitScope::Pane,
                            })
                        }),
                    ));
                }
            }

            // abort (C-c C-k)
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-rebase-abort") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |_ctx: &ActionContext<'_>| {
                            let g = s.lock().ok()?;
                            let repo = Repository::discover(&g.workdir).ok()?;
                            repo.run_git(["rebase", "--abort"]).ok()?;
                            Some(Effect::QuitEditor {
                                force: false,
                                scope: QuitScope::Pane,
                            })
                        }),
                    ));
                }
            }

            std::mem::forget(regs);
            Ok(())
        })
    }
}
