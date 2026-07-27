//! `prompt-line-mode`: the generic one-line minibuffer text prompt
//! backing `Effect::OpenPrompt`.
//!
//! Structurally mirrors [`crate::command_line_mode::CommandLineMode`]
//! / [`crate::search_line_mode::SearchLineMode`] (buffer-backed,
//! Insert-editable, focus-swapped via `Editor::focus_editing_buffer`)
//! but is generic rather than tied to one purpose: the caller
//! supplies the prompt label, initial text, and which `action:*`
//! handler fires on submit (`Effect::OpenPrompt`'s fields), so this
//! mode's own keymap only needs submit/cancel — no history, no
//! completion, no purpose-specific logic. See
//! `Editor::open_prompt_line` / `do_prompt_line_submit`.

use lattice_core::BufferKind;
use lattice_mode::{Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind};

/// Default synthetic name used when `Effect::OpenPrompt`'s
/// `buffer_name` is `None`.
pub const PROMPT_LINE_BUFFER_NAME_DEFAULT: &str = "*prompt*";

pub const PROMPT_LINE_SUBMIT: &str = "action:prompt-line-submit";
pub const PROMPT_LINE_CANCEL: &str = "action:prompt-line-cancel";

pub struct PromptLineMode;

impl PromptLineMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("prompt-line-mode")
    }
}

impl Mode for PromptLineMode {
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
        Keymap::from_entries(prompt_line_entries())
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async { Ok(()) })
    }
}

fn prompt_line_entries() -> &'static [KeymapEntry] {
    use std::sync::OnceLock;
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            lattice_mode::keymap_entry! { mode: Insert, chord: "<CR>", doc: "Submit the prompt", cmd: "action:prompt-line-submit" },
            lattice_mode::keymap_entry! { mode: Insert, chord: "<Esc>", doc: "Cancel the prompt", cmd: "action:prompt-line-cancel" },
            lattice_mode::keymap_entry! { mode: Insert, chord: "<C-c>", doc: "Cancel the prompt", cmd: "action:prompt-line-cancel" },
        ]
    })
}
