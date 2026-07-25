//! MG.1: magit-global universal minor mode — entry-point chords.
//!
//! Activates on every buffer (Universal policy) so `C-x g`,
//! `C-c g`, and `C-c f` work from any buffer kind — document,
//! help, file tree, oil, terminal, etc.

use std::sync::OnceLock;

use lattice_mode::{
    ActivationPolicy, CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext,
    ModeId, ModeKind, OptionOverrideSet, keymap_entry,
};

pub struct MagitGlobalMode;

impl MagitGlobalMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-global-mode")
    }
}

fn magit_global_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Normal, chord: "C-x g",
                doc: "Open magit-status for the current repo",
                cmd: "magit-status"
            },
            keymap_entry! {
                mode: Normal, chord: "C-c g",
                doc: "Open magit dispatch transient (repo-level)",
                cmd: "magit-dispatch"
            },
            keymap_entry! {
                mode: Normal, chord: "C-c f",
                doc: "Open magit file-dispatch transient",
                cmd: "magit-file-dispatch"
            },
        ]
    })
}

impl Mode for MagitGlobalMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    fn activation_policy(&self) -> ActivationPolicy {
        // Universal: activate on every buffer kind so the entry
        // chords work from help, file tree, oil, terminal, etc.
        ActivationPolicy::Universal
    }

    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::new()
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_global_keymap_entries())
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move { Ok(()) })
    }
}
