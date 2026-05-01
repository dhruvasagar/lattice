//! What a `CommandInvocation` produced once executed.
//!
//! `Effect::None` is for read-only or selection-only commands. `Effect::Edits`
//! carries the `AppliedEdit`s that the dispatcher applied to the document
//! (suitable for `Event::DocumentChanged`). `Effect::SelectionChange` carries
//! the new selection set (suitable for `Event::SelectionsChanged`). Effects
//! compose; a single command can yield multiple via `Effect::Many`.
//!
//! Ex-command effects (`SaveBuffer`, `QuitEditor`, `OpenBuffer`, `SetOption`,
//! `ClearSearchHighlight`, `Echo`, `EchoRegisters`, `EchoMarks`, `Substitute`,
//! `Global`) carry the typed intent of an ex-command. The host applies them
//! using its own state (registers, marks, view options, document loader);
//! the closure inside the registry only needs to package args into the
//! correct effect, which is what makes plugin- and built-in ex-commands
//! peers (DESIGN.md §5.2.1, §5.2.4).

use std::path::PathBuf;

use lattice_core::buffer::AppliedEdit;
use lattice_protocol::selection::SelectionSet;

use crate::modal::ModalState;
use crate::register::Register;

/// How a yank captured its content. Drives paste behavior:
/// charwise yanks land at the cursor, linewise yanks land on the next
/// line below, blockwise yanks paste each '\n'-separated row at the
/// same column on consecutive lines (vim's `Ctrl-V` selection then
/// `y`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum YankKind {
    Charwise,
    Linewise,
    Blockwise,
}

/// Severity tier for `Effect::Echo`. The host's echo-area renderer maps
/// these to its own colour scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EchoLevel {
    Info,
    Warn,
    Error,
}

/// Scope for `Effect::Substitute`. Mirrors vim's `:s/.../.../` (current
/// line) vs. `:%s/.../.../` (whole buffer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SubstituteScope {
    CurrentLine,
    Whole,
}

#[derive(Debug, Clone)]
pub enum Effect {
    None,
    Edits(Vec<AppliedEdit>),
    SelectionChange(SelectionSet),
    Yank {
        register: Register,
        content: String,
        kind: YankKind,
    },
    /// Transition the modal state machine. Used by operators that change
    /// modes after committing edits (vim's `c` -> Insert, future `s`,
    /// `gv` reselect Visual, etc.).
    EnterMode(ModalState),

    // --- Ex-command effects (DESIGN.md §5.2.1) ---
    /// `:w [path]` -- write the current buffer (to the given path, or the
    /// document's known path).
    SaveBuffer { path: Option<PathBuf> },
    /// `:q` / `:q!` -- quit the editor. `force = true` ignores dirty state.
    QuitEditor { force: bool },
    /// `:e[!] [path]` -- swap the current document for the file at `path`.
    /// With `path = None` reload from the document's existing path.
    /// `force = true` discards unsaved changes.
    OpenBuffer { path: Option<PathBuf>, force: bool },
    /// `:set <option>` -- the host parses the option spec; the closure
    /// just hands the raw text through.
    SetOption { spec: String },
    /// `:noh[lsearch]` -- clear the hlsearch overlay.
    ClearSearchHighlight,
    /// Display a one-line message in the echo area.
    Echo { level: EchoLevel, text: String },
    /// `:reg[isters]` -- the host formats and displays its own register
    /// state.
    EchoRegisters,
    /// `:marks` -- the host formats and displays its own mark state.
    EchoMarks,
    /// `:[%]s/pat/repl/[g]` -- run substitute over the given scope.
    Substitute {
        scope: SubstituteScope,
        pattern: String,
        replacement: String,
        global: bool,
    },
    /// `:g/pat/body` (and `:v/pat/body` with `inverted = true`).
    Global {
        pattern: String,
        inverted: bool,
        body: String,
    },
    /// `:d` -- delete the current line including its trailing newline.
    /// Distinct from the standard `delete` operator with a `CurrentLine`
    /// range, which preserves the newline (vim's `dd` semantics differ
    /// from `:d` -- §5.2.1).
    DeleteCurrentLine,
    /// `:describe-command <name>` (DESIGN.md §5.11). The host queries
    /// its `CommandRegistry` for the named entry and renders the
    /// metadata into a help overlay. Carried as a sentinel because
    /// the closure has no registry access.
    DescribeCommand { name: String },
    /// `:describe-buffer`. The host renders a snapshot of the current
    /// buffer's view-relevant state (path, language, modal, cursor,
    /// dirty, line count, ...).
    DescribeBuffer,
    /// `:apropos <pattern>`. The host runs a substring search over
    /// every registered `CommandSpec` (name + doc) and renders the
    /// matches.
    Apropos { pattern: String },

    Many(Vec<Effect>),
}

impl Effect {
    pub fn is_none(&self) -> bool {
        matches!(self, Effect::None)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn none_is_none() {
        assert!(Effect::None.is_none());
    }

    #[test]
    fn yank_carries_register_and_content() {
        let e = Effect::Yank {
            register: Register::Unnamed,
            content: "hello".into(),
            kind: YankKind::Charwise,
        };
        match e {
            Effect::Yank { register, content, kind } => {
                assert_eq!(register, Register::Unnamed);
                assert_eq!(content, "hello");
                assert_eq!(kind, YankKind::Charwise);
            }
            _ => panic!("expected Yank"),
        }
    }

    #[test]
    fn yank_kind_serializes() {
        let charwise = serde_json::to_string(&YankKind::Charwise).unwrap();
        let linewise = serde_json::to_string(&YankKind::Linewise).unwrap();
        assert!(charwise.contains("Charwise"));
        assert!(linewise.contains("Linewise"));
    }
}
