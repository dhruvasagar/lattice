//! MG.7: magit-blame major mode.
//!
//! Per-line git blame annotations. Runs `git blame --line-porcelain`
//! on spawn_blocking. `<CR>` shows commit, `p` re-blames at parent.

use std::sync::OnceLock;

use lattice_config;
use lattice_mode::{
    CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet, keymap_entry,
};

pub struct MagitBlameMode;

impl MagitBlameMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-blame-mode")
    }
}

fn magit_blame_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Normal, chord: "<CR>",
                doc: "Show commit for the blamed line",
                cmd: "action:magit-blame-show-commit"
            },
            keymap_entry! {
                mode: Normal, chord: "p",
                doc: "Re-blame at the parent commit",
                cmd: "action:magit-blame-parent"
            },
        ]
    })
}

impl Mode for MagitBlameMode {
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
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
            lattice_config::Number = false,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_blame_keymap_entries())
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move { Ok(()) })
    }
}
