//! MG.9: magit-stash major mode.
//!
//! Stash list with apply/pop/drop/create operations.

use std::sync::OnceLock;

use lattice_config;
use lattice_mode::{
    CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet, keymap_entry,
};

pub struct MagitStashMode;

impl MagitStashMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-stash-mode")
    }
}

fn magit_stash_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Normal, chord: "a",
                doc: "Apply stash at cursor (keep in list)",
                cmd: "action:magit-stash-apply"
            },
            keymap_entry! {
                mode: Normal, chord: "p",
                doc: "Pop stash at cursor (apply + drop)",
                cmd: "action:magit-stash-pop"
            },
            keymap_entry! {
                mode: Normal, chord: "d",
                doc: "Drop stash at cursor",
                cmd: "action:magit-stash-drop"
            },
            keymap_entry! {
                mode: Normal, chord: "z",
                doc: "Create new stash",
                cmd: "action:magit-stash-create"
            },
        ]
    })
}

impl Mode for MagitStashMode {
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
    fn keymap(&self) -> Keymap { Keymap::from_entries(magit_stash_keymap_entries()) }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move { Ok(()) })
    }
}
