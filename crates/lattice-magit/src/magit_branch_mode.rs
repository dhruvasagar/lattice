//! MG.9: magit-branch major mode.
//!
//! Branch list with checkout/create/delete/merge operations.

use std::sync::OnceLock;

use lattice_config;
use lattice_mode::{
    CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet, keymap_entry,
};

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
            keymap_entry! {
                mode: Normal, chord: "<CR>",
                doc: "Check out branch at cursor",
                cmd: "action:magit-branch-checkout"
            },
            keymap_entry! {
                mode: Normal, chord: "c",
                doc: "Create new branch",
                cmd: "action:magit-branch-create"
            },
            keymap_entry! {
                mode: Normal, chord: "d",
                doc: "Delete branch at cursor",
                cmd: "action:magit-branch-delete"
            },
            keymap_entry! {
                mode: Normal, chord: "m",
                doc: "Merge branch at cursor into current",
                cmd: "action:magit-branch-merge"
            },
        ]
    })
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
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move { Ok(()) })
    }
}
