//! MG.5: magit-diff major mode.
//!
//! Dedicated side-by-side diff view. Reuses the diff subsystem's
//! D.4 pane groups. Hunk staging (s/u/x) via same action handlers
//! as magit-status.

use std::sync::OnceLock;

use lattice_config;
use lattice_mode::{
    CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet, keymap_entry,
};

pub struct MagitDiffMode;

impl MagitDiffMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-diff-mode")
    }
}

fn magit_diff_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Normal, chord: "s",
                doc: "Stage hunk at cursor",
                cmd: "action:magit-stage"
            },
            keymap_entry! {
                mode: Normal, chord: "u",
                doc: "Unstage hunk at cursor",
                cmd: "action:magit-unstage"
            },
        ]
    })
}

impl Mode for MagitDiffMode {
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
        Keymap::from_entries(magit_diff_keymap_entries())
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move { Ok(()) })
    }
}
