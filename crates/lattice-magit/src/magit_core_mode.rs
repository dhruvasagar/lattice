//! MG.1: magit-core shared minor mode definition.
//!
//! Activates on any buffer whose major mode matches `magit-*`.
//! Provides the shared navigation + close keymap that every
//! magit buffer inherits.

use std::sync::OnceLock;

use lattice_mode::{
    ActivationPolicy, CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext,
    ModeId, ModeKind, OptionOverrideSet, keymap_entry,
};

use crate::magit_blame_mode::MagitBlameMode;
use crate::magit_branch_mode::MagitBranchMode;
use crate::magit_commit_mode::MagitCommitMode;
use crate::magit_diff_mode::MagitDiffMode;
use crate::magit_log_mode::MagitLogMode;
use crate::magit_rebase_mode::MagitRebaseMode;
use crate::magit_stash_mode::MagitStashMode;
use crate::magit_status_mode::MagitStatusMode;

/// Minor mode active on every magit buffer.
pub struct MagitCoreMode;

impl MagitCoreMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-core-mode")
    }
}

/// Shared keymap for all magit buffers — navigation, refresh, close.
fn magit_core_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Normal, chord: "gr",
                doc: "Refresh current magit buffer",
                cmd: "action:magit-refresh"
            },
            keymap_entry! {
                mode: Normal, chord: "q",
                doc: "Close magit buffer (bury)",
                cmd: "action:magit-close"
            },
            keymap_entry! {
                mode: Normal, chord: "]]",
                doc: "Next top-level section",
                cmd: "action:magit-next-section"
            },
            keymap_entry! {
                mode: Normal, chord: "[[",
                doc: "Previous top-level section",
                cmd: "action:magit-prev-section"
            },
            keymap_entry! {
                mode: Normal, chord: "]f",
                doc: "Next file/entry within current section",
                cmd: "action:magit-next-file"
            },
            keymap_entry! {
                mode: Normal, chord: "[f",
                doc: "Previous file/entry within current section",
                cmd: "action:magit-prev-file"
            },
            keymap_entry! {
                mode: Normal, chord: "]c",
                doc: "Next hunk",
                cmd: "action:magit-next-hunk"
            },
            keymap_entry! {
                mode: Normal, chord: "[c",
                doc: "Previous hunk",
                cmd: "action:magit-prev-hunk"
            },
            keymap_entry! {
                mode: Normal, chord: "<Tab>",
                doc: "Toggle section/hunk fold at cursor",
                cmd: "action:magit-toggle-fold"
            },
            keymap_entry! {
                mode: Normal, chord: "<S-Tab>",
                doc: "Cycle section visibility",
                cmd: "action:magit-cycle-sections"
            },
        ]
    })
}

impl Mode for MagitCoreMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Majors(vec![
            MagitStatusMode::mode_id(),
            MagitCommitMode::mode_id(),
            MagitDiffMode::mode_id(),
            MagitLogMode::mode_id(),
            MagitBlameMode::mode_id(),
            MagitStashMode::mode_id(),
            MagitBranchMode::mode_id(),
            MagitRebaseMode::mode_id(),
        ])
    }

    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::new()
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_core_keymap_entries())
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move { Ok(()) })
    }
}
