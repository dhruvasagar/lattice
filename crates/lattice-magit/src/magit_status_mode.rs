//! MG.1: magit-status major mode definition.
//!
//! The empty scaffold — buffer creation + keymap stubs.
//! Real status rendering arrives in MG.2; actions in MG.3.

use std::sync::OnceLock;

use lattice_config;
use lattice_mode::{
    BufferStoreHandle, CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext,
    ModeId, ModeKind, OptionOverrideSet, keymap_entry,
};

/// Major mode for the `*magit:status*` buffer.
pub struct MagitStatusMode;

impl MagitStatusMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-status-mode")
    }
}

/// Static keymap for `magit-status-mode`.
///
/// Stubs only — real handler bodies land in MG.3. The host's
/// `translate_mode_keymaps` pass auto-pushes these as a
/// `MajorMode(magit-status-mode)` layer.
fn magit_status_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Normal, chord: "s",
                doc: "Stage hunk or file at cursor",
                cmd: "action:magit-stage"
            },
            keymap_entry! {
                mode: Normal, chord: "u",
                doc: "Unstage hunk or file at cursor",
                cmd: "action:magit-unstage"
            },
            keymap_entry! {
                mode: Normal, chord: "x",
                doc: "Discard hunk or file at cursor",
                cmd: "action:magit-discard"
            },
            keymap_entry! {
                mode: Normal, chord: "cc",
                doc: "Open commit buffer",
                cmd: "action:magit-commit"
            },
            keymap_entry! {
                mode: Normal, chord: "ca",
                doc: "Amend previous commit",
                cmd: "action:magit-commit-amend"
            },
            keymap_entry! {
                mode: Normal, chord: "=",
                doc: "Toggle inline diff at cursor",
                cmd: "action:magit-toggle-diff"
            },
            keymap_entry! {
                mode: Normal, chord: "p",
                doc: "Stage hunk interactively (git add -p)",
                cmd: "action:magit-stage-patch"
            },
            keymap_entry! {
                mode: Normal, chord: "<CR>",
                doc: "Context-aware open/visit at cursor",
                cmd: "action:magit-visit"
            },
        ]
    })
}

/// MG.1: empty guard — no subscriptions or state yet.
/// MG.2 populates with refresh task + event subscriptions.
#[derive(Default)]
pub struct MagitStatusGuard;

impl Mode for MagitStatusMode {
    type Guard = MagitStatusGuard;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    fn target_buffer_kind(&self) -> Option<lattice_core::BufferKind> {
        None // plain Document — no kind-specific logic
    }

    fn options(&self) -> OptionOverrideSet {
        // Read-only synthetic buffer. Owner writes (mgit-status
        // refresh) go through apply_edit_batch which bypasses the
        // read-only gate at the dispatcher's Insert/operator path.
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
            lattice_config::Number = false,
            lattice_config::SignColumnOption = lattice_config::SignColumn::Yes,
            lattice_config::CursorLine = false,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_status_keymap_entries())
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            // MG.1: empty buffer. MG.2 spawns refresh task here.
            let _buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(MagitStatusGuard::default());
            };
            let Some(_handle) = store.handle_for(_buffer_id) else {
                return Ok(MagitStatusGuard::default());
            };

            tracing::debug!(
                target: "lattice_magit",
                "magit-status-mode activated on buffer {_buffer_id:?}",
            );

            Ok(MagitStatusGuard::default())
        })
    }
}
