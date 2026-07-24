//! MB.2 (rich minibuffer, tier 2): `command-line-expand-mode`.
//!
//! When the user presses `<C-x><C-e>` in the `:` line, the one-row
//! `*command-line*` buffer grows into a full-modal mini-buffer band.
//! This mode replaces `command-line-mode` as the buffer's major for
//! the duration of the expand, giving the expanded band its own isolated
//! option stack (`Number = false`, `SignColumn = No`, etc.) and keymap
//! surface.
//!
//! Collapsing (`<C-x><C-e>` again) reactivates `command-line-mode`.
//!
//! The mode is intentionally a separate `ModeId` so that:
//! - option overrides (gutter, line numbers, wrap) are scoped to the
//!   expanded band and never leak to the document pane;
//! - future expand-specific features (full-text syntax highlighting,
//!   per-line decorations, band-shaped render path) have a dedicated
//!   mode to attach to.

use lattice_core::BufferKind;
use lattice_mode::{
    keymap_entry, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
};

/// `command-line-expand-mode`: the major mode of the `*command-line*`
/// buffer while expanded into the full-modal band.
pub struct CommandLineExpandMode;

impl CommandLineExpandMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("command-line-expand-mode")
    }
}

impl Mode for CommandLineExpandMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    fn target_buffer_kind(&self) -> Option<BufferKind> {
        None
    }

    /// Same option overrides as `command-line-mode`: gutterless, no
    /// line numbers, no wrap, no cursor line. These are scoped to
    /// the `*command-line*` buffer only (per-buffer option cascade).
    fn options(&self) -> lattice_config::OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::NoFile = true,
            lattice_config::Wrap = false,
            lattice_config::Number = false,
            lattice_config::SignColumnOption = lattice_config::SignColumn::No,
            lattice_config::CursorLine = false,
        }
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(expand_entries())
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async { Ok(()) })
    }
}

/// Static entry table for `command-line-expand-mode`. Mirrors
/// `command-line-mode`'s keymap: the same Insert-layer chords
/// (submit/cancel/history/completion/describe) plus the Normal-layer
/// `<C-x><C-e>` for collapse.
fn expand_entries() -> &'static [KeymapEntry] {
    use std::sync::OnceLock;
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Insert, chord: "<CR>", doc: "Submit the command line", cmd: "action:command-line-submit" },
            keymap_entry! { mode: Insert, chord: "<Esc>", doc: "Cancel the command line", cmd: "action:command-line-cancel" },
            keymap_entry! { mode: Insert, chord: "<C-c>", doc: "Cancel the command line", cmd: "action:command-line-cancel" },
            keymap_entry! { mode: Insert, chord: "<C-p>", doc: "Previous history entry", cmd: "action:command-line-history-prev" },
            keymap_entry! { mode: Insert, chord: "<C-n>", doc: "Next history entry", cmd: "action:command-line-history-next" },
            keymap_entry! { mode: Insert, chord: "<Up>", doc: "Previous history entry", cmd: "action:command-line-history-prev" },
            keymap_entry! { mode: Insert, chord: "<Down>", doc: "Next history entry", cmd: "action:command-line-history-next" },
            keymap_entry! { mode: Insert, chord: "<Tab>", doc: "Complete / next candidate", cmd: "action:command-line-complete" },
            keymap_entry! { mode: Insert, chord: "<S-Tab>", doc: "Previous candidate", cmd: "action:command-line-complete-prev" },
            keymap_entry! { mode: Insert, chord: "<C-h>", doc: "Describe command / arg under cursor", cmd: "action:command-line-describe-under-cursor" },
            keymap_entry! { mode: Insert, chord: "<C-x><C-e>", doc: "Collapse the mini-buffer band back to the one-row `:` line", cmd: "action:command-line-toggle-expand" },
            keymap_entry! { mode: Normal, chord: "<C-x><C-e>", doc: "Collapse the mini-buffer band back to the one-row `:` line", cmd: "action:command-line-toggle-expand" },
        ]
    })
}
