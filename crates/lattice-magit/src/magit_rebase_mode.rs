//! MG.9: magit-rebase major mode.
//!
//! Interactive rebase todo buffer. Editable pick/reword/squash/fixup/drop list.
//! C-c C-c runs rebase, C-c C-k aborts.

use std::sync::OnceLock;

use lattice_config;
use lattice_mode::{
    CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet, keymap_entry,
};

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
            keymap_entry! {
                mode: Insert, chord: "<C-c><C-c>",
                doc: "Execute rebase",
                cmd: "action:magit-rebase-confirm"
            },
            keymap_entry! {
                mode: Insert, chord: "<C-c><C-k>",
                doc: "Abort rebase",
                cmd: "action:magit-rebase-abort"
            },
        ]
    })
}

impl Mode for MagitRebaseMode {
    type Guard = ();

    fn id(&self) -> ModeId { Self::mode_id() }
    fn kind(&self) -> ModeKind { ModeKind::Major }
    fn target_buffer_kind(&self) -> Option<lattice_core::BufferKind> { None }

    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::NoFile = true,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet { CapabilitySet::empty() }
    fn keymap(&self) -> Keymap { Keymap::from_entries(magit_rebase_keymap_entries()) }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move { Ok(()) })
    }
}
