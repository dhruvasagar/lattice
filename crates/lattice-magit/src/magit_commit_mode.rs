//! MG.4: magit-commit major mode.
//!
//! Synthetic buffer `*magit:commit*` for composing commit messages.
//! Staged diff preview + editable message region deferred.

use std::sync::OnceLock;

use lattice_config;
use lattice_mode::{
    CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet, keymap_entry,
};

pub struct MagitCommitMode;

impl MagitCommitMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-commit-mode")
    }
}

fn magit_commit_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Insert, chord: "<C-c><C-c>",
                doc: "Confirm commit",
                cmd: "action:magit-commit-confirm"
            },
            keymap_entry! {
                mode: Insert, chord: "<C-c><C-k>",
                doc: "Abort commit",
                cmd: "action:magit-commit-abort"
            },
        ]
    })
}

impl Mode for MagitCommitMode {
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
            lattice_config::Number = false,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_commit_keymap_entries())
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move { Ok(()) })
    }
}
