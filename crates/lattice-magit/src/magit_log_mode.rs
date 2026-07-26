//! MG.6: magit-log major mode.
//!
//! Runs `git log --oneline --graph --decorate -50` on open,
//! populates buffer content. <CR> shows commit detail.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_grammar::{CommandRegistryHandle, Effect};
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerRegistration, ActionHandlerRegistryHandle,
    BufferStoreHandle, CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext,
    ModeId, ModeKind, OptionOverrideSet, keymap_entry,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_runtime::Document;
use lattice_vcs::Repository;

pub struct MagitLogMode;

impl MagitLogMode {
    pub fn mode_id() -> ModeId { ModeId::new("magit-log-mode") }
}

fn magit_log_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Show commit detail at cursor", cmd: "action:magit-log-show-commit" },
        ]
    })
}

struct LogState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
}

impl Mode for MagitLogMode {
    type Guard = ();

    fn id(&self) -> ModeId { Self::mode_id() }
    fn kind(&self) -> ModeKind { ModeKind::Major }
    fn target_buffer_kind(&self) -> Option<lattice_core::BufferKind> { None }

    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
            lattice_config::Number = false,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet { CapabilitySet::empty() }
    fn keymap(&self) -> Keymap { Keymap::from_entries(magit_log_keymap_entries()) }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else { return Ok(()); };
            let Some(handle) = store.handle_for(buffer_id) else { return Ok(()); };
            let workdir = Repository::discover(".")
                .ok()
                .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                .unwrap_or_default();

            // Populate log: blocking I/O on spawn_blocking, then apply edit
            // on the current task (no Runtime::new()).
            let wd = workdir.clone();
            let text = tokio::task::spawn_blocking(move || {
                run_log(&wd)
            }).await.unwrap();
            let snap = handle.snapshot();
            let last = snap.buffer.line_count().saturating_sub(1);
            let last_line = snap.buffer.line(last).unwrap_or_default();
            let end = Position::new(last, last_line.len() as u32);
            let _ = handle.apply_edit_batch(vec![
                Edit::replace(Range::new(Position::ZERO, end), text),
            ]).await;

            // Register <CR> handler
            let state = Arc::new(Mutex::new(LogState {
                buffer_id, store: store.clone(), workdir,
            }));

            let Some(cmd_arc) = ctx.service::<CommandRegistryHandle>() else { return Ok(()); };
            let Some(ah_arc) = ctx.service::<ActionHandlerRegistryHandle>() else { return Ok(()); };
            let registry = cmd_arc.load();
            let handlers = (*ah_arc).clone();

            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-log-show-commit") {
                    let h: ActionHandler = Arc::new(move |ctx: &ActionContext<'_>| {
                        let g = s.lock().ok()?;
                        let handle = g.store.handle_for(g.buffer_id)?;
                        let snap = handle.snapshot();
                        let line = snap.buffer.line(ctx.cursor.line)?;
                        let sha = line.split_whitespace().next()?;
                        let output = std::process::Command::new("git")
                            .args(["show", "--stat", "-p", sha])
                            .current_dir(&g.workdir)
                            .output().ok()?;
                        let text = String::from_utf8(output.stdout).ok()?;
                        // Write to a temp file and open it
                        let tmp = g.workdir.join(format!(".lattice_commit_{}", sha));
                        std::fs::write(&tmp, text).ok()?;
                        Some(Effect::OpenBuffer {
                            path: Some(tmp),
                            force: true,
                        })
                    });
                    handlers.register(cid, h.clone());
                }
            }

            Ok(())
        })
    }
}

fn run_log(workdir: &std::path::Path) -> String {
    std::process::Command::new("git")
        .args(["log", "--oneline", "--graph", "--decorate", "-50"])
        .current_dir(workdir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| "Not a git repository.\n".to_string())
}
