//! MB.5a (rich minibuffer): `search-line-mode`.
//!
//! The `/`·`?` search line is a **buffer-backed, readline-grade editing
//! surface** (`docs/dev/architecture/rich-minibuffer.md` §6). Pressing
//! `/` or `?` creates / focuses the synthetic one-line `*search-line*`
//! `Document` through the mode-owned creation seam
//! ([`lattice_mode::ModeActivator::ensure_named_document`]) and
//! focus-swaps it in as the editing buffer
//! ([`crate::dispatch::Editor::focus_editing_buffer`]). Keys then flow
//! through the **universal Insert dispatcher**, so the readline chords
//! (`<C-a>`/`<C-e>`/`<C-b>`/`<C-f>`/`<C-w>`/`<C-u>`, cursor keys,
//! `<Del>`) edit it directly — no search-specific editing code.
//!
//! `search-line-mode` is the **major mode** on that buffer. It owns the
//! small delta a search line adds over a plain Insert buffer, via an
//! Insert-mode keymap layer (`Mode::keymap`, resolved through
//! `keymap_mode_contributions`): `<CR>` submit, `<Esc>` / `<C-c>`
//! cancel. Because the layer is keyed under `MajorMode(search-line-mode)`,
//! the per-keystroke `keymap_gated_ids` filter scopes it to the
//! `*search-line*` buffer only (mode-ownership; `mode-architecture.md`
//! §5.3–5.4).
//!
//! The buffer is insert-only: there is no Normal-mode entry, so `<Esc>`
//! cancels cleanly.

use lattice_core::BufferKind;
use lattice_mode::{
    Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, keymap_entry,
};

/// Synthetic name of the `/`·`?` search-line buffer. `ephemeral`
/// keeps it out of `:ls` and the `:bn`/`:bp` cycle.
pub const SEARCH_LINE_BUFFER_NAME: &str = "*search-line*";

/// Command names bound by `search-line-mode`'s keymap. Registered as
/// `CommandId`s in `crate::actions` (each maps to an `AppEffect` that
/// drives the `Editor::do_search_line_*` handler).
pub const SEARCH_LINE_SUBMIT: &str = "action:search-line-submit";
pub const SEARCH_LINE_CANCEL: &str = "action:search-line-cancel";
pub const SEARCH_LINE_TOGGLE_EXPAND: &str = "action:search-line-toggle-expand";

/// `search-line-mode`: the major mode of the `*search-line*` buffer.
pub struct SearchLineMode;

impl SearchLineMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("search-line-mode")
    }
}

impl Mode for SearchLineMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    /// Not auto-activated by on-disk language detection — the mode is
    /// activated *by id* through `ensure_named_document`. Returning
    /// `None` keeps it out of the kind→major lookup so it never claims
    /// ordinary `Document` buffers.
    fn target_buffer_kind(&self) -> Option<BufferKind> {
        None
    }

    /// `NoFile = true` so `:q`'s dirty guard skips the (always-dirty,
    /// never-on-disk) prompt buffer. Gutterless + no-wrap: a one-line
    /// prompt.
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
        Keymap::from_entries(search_line_entries())
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async { Ok(()) })
    }
}

/// Static entry table for `search-line-mode`'s Insert-layer keymap.
fn search_line_entries() -> &'static [KeymapEntry] {
    use std::sync::OnceLock;
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Insert, chord: "<CR>", doc: "Submit the search pattern", cmd: "action:search-line-submit" },
            keymap_entry! { mode: Insert, chord: "<Esc>", doc: "Cancel the search line", cmd: "action:search-line-cancel" },
            keymap_entry! { mode: Insert, chord: "<C-c>", doc: "Cancel the search line", cmd: "action:search-line-cancel" },
            keymap_entry! { mode: Insert, chord: "<C-p>", doc: "Previous search history entry", cmd: "action:search-line-history-prev" },
            keymap_entry! { mode: Insert, chord: "<C-n>", doc: "Next search history entry", cmd: "action:search-line-history-next" },
            keymap_entry! { mode: Insert, chord: "<Up>", doc: "Previous search history entry", cmd: "action:search-line-history-prev" },
            keymap_entry! { mode: Insert, chord: "<Down>", doc: "Next search history entry", cmd: "action:search-line-history-next" },
            keymap_entry! { mode: Insert, chord: "<C-x><C-e>", doc: "Expand the `/`·`?` line into the full-modal mini-buffer band (or collapse it)", cmd: "action:search-line-toggle-expand" },
            // MB.5c: also from the expanded band's Normal mode, so collapse
            // works without first re-entering Insert.
            keymap_entry! { mode: Normal, chord: "<C-x><C-e>", doc: "Collapse the mini-buffer band back to the one-row `/`·`?` line", cmd: "action:search-line-toggle-expand" },
        ]
    })
}
