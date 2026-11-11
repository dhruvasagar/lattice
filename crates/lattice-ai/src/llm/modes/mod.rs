//! The LLM layer's modes.

pub mod chat;
pub mod context;
pub mod llm_mode;
pub mod requests;
pub mod system;
pub mod tools;

use std::sync::OnceLock;

use lattice_config::OptionOverrideSet;
use lattice_mode::ModeId;

/// The options of a read-only listing buffer (`*llm-context*`,
/// `*llm-requests*`, `*llm-tools*`): no typing, no file.
///
/// One of TWO declarations a read-only major needs; the other is
/// [`read_only_implied`]. Kept here, once, so a listing cannot ship with one
/// and not the other, which is how magit and `*messages*` shipped editable.
fn read_only_options() -> OptionOverrideSet {
    lattice_config::overrides! {
        lattice_config::ReadOnly = true,
        lattice_config::NoFile = true,
    }
}

/// The second declaration: `ReadOnly` gates typing only; `read-only-mode`
/// carries the runner that refuses `x`, `p` and the other operators.
fn read_only_implied() -> &'static [ModeId] {
    static IMPLIED: OnceLock<Vec<ModeId>> = OnceLock::new();
    IMPLIED.get_or_init(|| vec![lattice_mode::modes::ReadOnlyMode::mode_id()])
}
