//! MG.1: magit-core shared minor mode.
//!
//! Activates on magit buffers. Provides shared keymap. Navigation
//! chords (]]/[[,/]f/[f,/]c/[c) are registered but produce no-ops —
//! cursor movement from action handlers requires Effect extensions.
//! `gr` (refresh) and `q` (close) handlers are live.

use std::sync::{Arc, OnceLock};

use lattice_core::BufferId;
use lattice_grammar::{CommandRegistryHandle, Effect, QuitScope};
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerRegistryHandle, ActivationPolicy, BufferStoreHandle,
    CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet, keymap_entry,
};

use crate::magit_blame_mode::MagitBlameMode;
use crate::magit_branch_mode::MagitBranchMode;
use crate::magit_commit_mode::MagitCommitMode;
use crate::magit_diff_mode::MagitDiffMode;
use crate::magit_log_mode::MagitLogMode;
use crate::magit_rebase_mode::MagitRebaseMode;
use crate::magit_stash_mode::MagitStashMode;
use crate::magit_status_mode::MagitStatusMode;

pub struct MagitCoreMode;

impl MagitCoreMode {
    pub fn mode_id() -> ModeId { ModeId::new("magit-core-mode") }
}

fn magit_core_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "gr", doc: "Refresh current magit buffer", cmd: "action:magit-refresh" },
            keymap_entry! { mode: Normal, chord: "q", doc: "Close magit buffer", cmd: "action:magit-close" },
            keymap_entry! { mode: Normal, chord: "]]", doc: "Next section", cmd: "action:magit-next-section" },
            keymap_entry! { mode: Normal, chord: "[[", doc: "Previous section", cmd: "action:magit-prev-section" },
            keymap_entry! { mode: Normal, chord: "]f", doc: "Next file", cmd: "action:magit-next-file" },
            keymap_entry! { mode: Normal, chord: "[f", doc: "Previous file", cmd: "action:magit-prev-file" },
            keymap_entry! { mode: Normal, chord: "]c", doc: "Next hunk", cmd: "action:magit-next-hunk" },
            keymap_entry! { mode: Normal, chord: "[c", doc: "Previous hunk", cmd: "action:magit-prev-hunk" },
            keymap_entry! { mode: Normal, chord: "<Tab>", doc: "Toggle fold", cmd: "action:magit-toggle-fold" },
            keymap_entry! { mode: Normal, chord: "<S-Tab>", doc: "Cycle sections", cmd: "action:magit-cycle-sections" },
        ]
    })
}

impl Mode for MagitCoreMode {
    type Guard = ();

    fn id(&self) -> ModeId { Self::mode_id() }
    fn kind(&self) -> ModeKind { ModeKind::Minor }

    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Majors(vec![
            MagitStatusMode::mode_id(), MagitCommitMode::mode_id(),
            MagitDiffMode::mode_id(), MagitLogMode::mode_id(),
            MagitBlameMode::mode_id(), MagitStashMode::mode_id(),
            MagitBranchMode::mode_id(), MagitRebaseMode::mode_id(),
        ])
    }

    fn options(&self) -> OptionOverrideSet { OptionOverrideSet::new() }
    fn required_capabilities(&self) -> CapabilitySet { CapabilitySet::empty() }
    fn keymap(&self) -> Keymap { Keymap::from_entries(magit_core_keymap_entries()) }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else { return Ok(()); };

            let Some(cmd_arc) = ctx.service::<CommandRegistryHandle>() else { return Ok(()); };
            let Some(ah_arc) = ctx.service::<ActionHandlerRegistryHandle>() else { return Ok(()); };
            let registry = cmd_arc.load();
            let handlers = (*ah_arc).clone();
            let mut regs = Vec::new();

            macro_rules! h {
                ($name:expr, $body:expr) => {
                    if let Some(cid) = registry.id_by_name($name) {
                        regs.push(handlers.register(cid, Arc::new($body)));
                    }
                };
            }

            // close (q)
            h!("action:magit-close", move |_ctx: &ActionContext<'_>| {
                Some(Effect::QuitEditor { force: false, scope: QuitScope::Pane })
            });

            // Navigation chords — no-ops until cursor-move effects are available
            h!("action:magit-next-section", move |_ctx: &ActionContext<'_>| { None });
            h!("action:magit-prev-section", move |_ctx: &ActionContext<'_>| { None });
            h!("action:magit-next-file", move |_ctx: &ActionContext<'_>| { None });
            h!("action:magit-prev-file", move |_ctx: &ActionContext<'_>| { None });
            h!("action:magit-next-hunk", move |_ctx: &ActionContext<'_>| { None });
            h!("action:magit-prev-hunk", move |_ctx: &ActionContext<'_>| { None });
            h!("action:magit-toggle-fold", move |_ctx: &ActionContext<'_>| { None });
            h!("action:magit-cycle-sections", move |_ctx: &ActionContext<'_>| { None });

            std::mem::forget(regs);
            Ok(())
        })
    }
}
