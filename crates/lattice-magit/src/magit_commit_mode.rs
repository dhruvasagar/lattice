//! MG.4: magit-commit major mode.
//!
//! Shows the staged diff (read-only top region) and an editable
//! message region below. C-c C-c commits, C-c C-k aborts.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_grammar::{AppEffect, CommandRegistryHandle, Effect, QuitScope};
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerRegistration, ActionHandlerRegistryHandle,
    BufferStoreHandle, CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext,
    ModeId, ModeKind, OptionOverrideSet, keymap_entry,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_runtime::Document;
use lattice_vcs::{Commit, Repository};

pub struct MagitCommitMode;

impl MagitCommitMode {
    pub fn mode_id() -> ModeId { ModeId::new("magit-commit-mode") }
}

fn magit_commit_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Insert, chord: "<C-c><C-c>", doc: "Confirm commit", cmd: "action:magit-commit-confirm" },
            keymap_entry! { mode: Insert, chord: "<C-c><C-k>", doc: "Abort commit", cmd: "action:magit-commit-abort" },
            keymap_entry! { mode: Normal, chord: "<C-c><C-c>", doc: "Confirm commit", cmd: "action:magit-commit-confirm" },
            keymap_entry! { mode: Normal, chord: "<C-c><C-k>", doc: "Abort commit", cmd: "action:magit-commit-abort" },
        ]
    })
}

struct CommitState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    amend: bool,
}

impl Mode for MagitCommitMode {
    type Guard = ();

    fn id(&self) -> ModeId { Self::mode_id() }
    fn kind(&self) -> ModeKind { ModeKind::Major }
    fn target_buffer_kind(&self) -> Option<lattice_core::BufferKind> { None }

    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::NoFile = true,
            lattice_config::Number = false,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet { CapabilitySet::empty() }
    fn keymap(&self) -> Keymap { Keymap::from_entries(magit_commit_keymap_entries()) }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else { return Ok(()); };
            let Some(handle) = store.handle_for(buffer_id) else { return Ok(()); };

            let workdir = Repository::discover(".")
                .ok()
                .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                .unwrap_or_default();

            // Populate the buffer: staged diff + message area
            let staged = run_staged_diff(&workdir);
            let initial = format!(
                "--- Staged diff (review before committing) ---\n\
                 {}\n\
                 --- Commit message (edit below) ---\n\
                 \n\
                 \n",
                if staged.is_empty() { "(nothing staged)" } else { &staged }
            );
            let snap = handle.snapshot();
            let last = snap.buffer.line_count().saturating_sub(1);
            let last_line = snap.buffer.line(last).unwrap_or_default();
            let end = Position::new(last, last_line.len() as u32);
            let _ = handle.apply_edit_batch(vec![
                Edit::replace(Range::new(Position::ZERO, end), initial)
            ]).await;

            // Register action handlers
            let state = Arc::new(Mutex::new(CommitState {
                buffer_id, store: store.clone(), workdir, amend: false,
            }));

            let Some(cmd_arc) = ctx.service::<CommandRegistryHandle>() else { return Ok(()); };
            let Some(ah_arc) = ctx.service::<ActionHandlerRegistryHandle>() else { return Ok(()); };
            let registry = cmd_arc.load();
            let handlers = (*ah_arc).clone();
            let mut regs = Vec::new();

            // ── confirm (C-c C-c) ──────────────────────
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-commit-confirm") {
                    regs.push(handlers.register(cid, Arc::new(move |_ctx: &ActionContext<'_>| {
                        let g = s.lock().ok()?;
                        let handle = g.store.handle_for(g.buffer_id)?;
                        let snap = handle.snapshot();
                        // Read the message: lines after the "--- Commit message" marker
                        let mut message = String::new();
                        let mut in_message = false;
                        for l in 0..snap.buffer.line_count() as u32 {
                            let text = snap.buffer.line(l).unwrap_or_default();
                            if text.contains("--- Commit message") {
                                in_message = true;
                                continue;
                            }
                            if in_message && !text.trim().is_empty() {
                                message.push_str(&text);
                                message.push('\n');
                            }
                        }
                        if message.trim().is_empty() { return None; }
                        let repo = Repository::discover(&g.workdir).ok()?;
                        if g.amend {
                            Commit::amend(&repo, message.trim()).ok()?;
                        } else {
                            Commit::create(&repo, message.trim()).ok()?;
                        }
                        Some(Effect::QuitEditor { force: false, scope: QuitScope::Pane })
                    })));
                }
            }

            // ── abort (C-c C-k) ─────────────────────────
            {
                if let Some(cid) = registry.id_by_name("action:magit-commit-abort") {
                    regs.push(handlers.register(cid, Arc::new(move |_ctx: &ActionContext<'_>| {
                        Some(Effect::QuitEditor { force: false, scope: QuitScope::Pane })
                    })));
                }
            }

            std::mem::forget(regs);
            Ok(())
        })
    }
}

fn run_staged_diff(workdir: &std::path::Path) -> String {
    std::process::Command::new("git")
        .args(["diff", "--cached"])
        .current_dir(workdir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default()
}
