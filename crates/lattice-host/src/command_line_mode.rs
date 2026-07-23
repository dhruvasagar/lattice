//! MB.1 (rich minibuffer, tier 1): `command-line-mode`.
//!
//! The `:` command line is a **buffer-backed, readline-grade editing
//! surface** (`docs/dev/architecture/rich-minibuffer.md` §2). Pressing
//! `:` creates / focuses the synthetic one-line `*command-line*`
//! `Document` through the mode-owned creation seam
//! ([`lattice_mode::ModeActivator::ensure_named_document`]) and
//! focus-swaps it in as the editing buffer
//! ([`crate::dispatch::Editor::focus_editing_buffer`]). Keys then flow
//! through the **universal Insert dispatcher**, so the readline chords
//! (`<C-a>`/`<C-e>`/`<C-b>`/`<C-f>`/`<C-w>`/`<C-u>`, cursor keys,
//! `<Del>`) edit it directly — no cmdline-specific editing code.
//!
//! `command-line-mode` is the **major mode** on that buffer. It owns the
//! small delta a command line adds over a plain Insert buffer, via an
//! Insert-mode keymap layer (`Mode::keymap`, resolved through
//! `keymap_mode_contributions`): `<CR>` submit, `<Esc>` / `<C-c>`
//! cancel, `<C-p>` / `<C-n>` / `<Up>` / `<Down>` history walk, `<Tab>` /
//! `<S-Tab>` completion, `<C-h>` describe-under-cursor. Because the
//! layer is keyed under `MajorMode(command-line-mode)`, the per-keystroke
//! `keymap_gated_ids` filter scopes it to the `*command-line*` buffer
//! only (mode-ownership; `mode-architecture.md` §5.3–5.4).
//!
//! The handler bodies stay as the (rewired) `Editor::do_command_line_*`
//! methods reached through the `AppEffect` host boundary — the same
//! pattern `action:enter-command-line` uses. The buffer is insert-only:
//! there is no Normal-mode entry, so `<Esc>` cancels cleanly.

use lattice_core::BufferKind;
use lattice_mode::{
    keymap_entry, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
};

/// Synthetic name of the `:` command-line buffer. `:ls` / `:b` can
/// reach it; `:bn` / `:bp` skip it (`listed = false`).
pub const COMMAND_LINE_BUFFER_NAME: &str = "*command-line*";

/// Command names bound by `command-line-mode`'s keymap. Registered as
/// `CommandId`s in `crate::actions` (each maps to an `AppEffect` that
/// drives the rewired `Editor::do_command_line_*` handler).
pub const CMDLINE_SUBMIT: &str = "action:command-line-submit";
pub const CMDLINE_CANCEL: &str = "action:command-line-cancel";
pub const CMDLINE_HISTORY_PREV: &str = "action:command-line-history-prev";
pub const CMDLINE_HISTORY_NEXT: &str = "action:command-line-history-next";
pub const CMDLINE_COMPLETE: &str = "action:command-line-complete";
pub const CMDLINE_COMPLETE_PREV: &str = "action:command-line-complete-prev";
pub const CMDLINE_DESCRIBE: &str = "action:command-line-describe-under-cursor";
pub const CMDLINE_TOGGLE_EXPAND: &str = "action:command-line-toggle-expand";

/// `command-line-mode`: the major mode of the `*command-line*` buffer.
pub struct CommandLineMode;

impl CommandLineMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("command-line-mode")
    }
}

impl Mode for CommandLineMode {
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
    /// never-on-disk) prompt buffer — otherwise `:q` would refuse with
    /// "no write since last change" for the `*command-line*` buffer. NOT
    /// `ReadOnly` (the prompt is the editing surface). Gutterless +
    /// no-wrap: a one-line prompt.
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
        // Insert-mode overrides that turn a plain one-line buffer into a
        // command line. Base Insert supplies the readline editing; these
        // rows only add submit / cancel / history / completion. The
        // conditional "accept-vs-submit" / "history-vs-complete" logic
        // lives in the handler bodies (they branch on `completion_state`).
        Keymap::from_entries(command_line_entries())
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async { Ok(()) })
    }
}

/// Static entry table for `command-line-mode`'s Insert-layer keymap.
fn command_line_entries() -> &'static [KeymapEntry] {
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
            keymap_entry! { mode: Insert, chord: "<C-x><C-e>", doc: "Expand the `:` line into the full-modal mini-buffer band (or collapse it)", cmd: "action:command-line-toggle-expand" },
        ]
    })
}
