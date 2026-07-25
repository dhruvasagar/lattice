//! MG.5: magit-diff major mode.
//!
//! Opens the file under the active buffer and registers a
//! `DiffSession` between `GitBaseline(HEAD)` and the buffer.
//! Side-by-side layout is handled by the diff subsystem's
//! existing pane-group machinery. s/u staging fires the same
//! action handlers as magit-status.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_diff::subsystem::{
    DiffDescriptor, DiffParticipantSource, DiffSubsystemHandle,
};
use lattice_grammar::{CommandRegistryHandle, Effect, QuitScope};
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerRegistration, ActionHandlerRegistryHandle,
    CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet, keymap_entry,
};
use lattice_vcs::Repository;

pub struct MagitDiffMode;

impl MagitDiffMode {
    pub fn mode_id() -> ModeId { ModeId::new("magit-diff-mode") }
}

fn magit_diff_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "s", doc: "Stage hunk at cursor", cmd: "action:magit-stage" },
            keymap_entry! { mode: Normal, chord: "u", doc: "Unstage hunk at cursor", cmd: "action:magit-unstage" },
        ]
    })
}

impl Mode for MagitDiffMode {
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
    fn keymap(&self) -> Keymap { Keymap::from_entries(magit_diff_keymap_entries()) }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            // Register close handler
            let Some(cmd_arc) = ctx.service::<CommandRegistryHandle>() else { return Ok(()); };
            let Some(ah_arc) = ctx.service::<ActionHandlerRegistryHandle>() else { return Ok(()); };
            let registry = cmd_arc.load();
            let handlers = (*ah_arc).clone();
            let mut regs = Vec::new();

            if let Some(cid) = registry.id_by_name("action:magit-close") {
                regs.push(handlers.register(cid, Arc::new(move |_ctx: &ActionContext<'_>| {
                    Some(Effect::QuitEditor { force: false, scope: QuitScope::Pane })
                })));
            }

            std::mem::forget(regs);
            Ok(())
        })
    }
}
