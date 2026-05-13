//! Picker accept outcomes -- typed effects the source
//! generator emits on `<CR>` for the host to apply.
//!
//! Bounded enum, scoped to "what a picker can ask the host
//! to do." Source generators emit one outcome; the host's
//! translator (`App::apply_picker_outcome`) pattern-matches
//! into the appropriate `Effect` / App-state mutation.
//! Plugin sources (Phase 7) emit the same enum over WIT --
//! the variant set is the trait surface plugin authors
//! design against.

use std::path::PathBuf;

use lattice_grammar::args::Args;

/// Result of `PickerSourceGenerator::accept`. The host
/// pattern-matches and runs the corresponding mutation.
///
/// New variants are added when (and only when) a concrete
/// picker source needs an action the existing set can't
/// express. Resist re-using `Effect` directly: a tighter
/// outcome set is easier to audit, easier to mirror in WIT,
/// and stops source generators from emitting arbitrary
/// grammar effects that bypass picker conventions.
#[derive(Debug, Clone)]
pub enum PickerAcceptOutcome {
    /// Edit the file at `path`. Host routes through
    /// `App::do_edit`, which handles the "already-open" +
    /// new-file branches uniformly.
    OpenFile { path: PathBuf },
    /// Switch the active pane to an existing buffer by id.
    SwitchBuffer { buffer_id: u32 },
    /// Move the cursor within `buffer_id`. If `buffer_id`
    /// is the active pane's buffer, only the cursor moves;
    /// otherwise the host activates that buffer first.
    /// Picker sources use this for in-buffer jumps where
    /// the buffer is already loaded (`:picker lines`,
    /// `:picker marks` against the active doc).
    JumpInBuffer { buffer_id: u32, line: u32, col: u32 },
    /// Jump to a named mark. The host resolves the mark's
    /// position itself -- the source doesn't need to.
    JumpToMark { name: char },
    /// Jump to `path:line:col`. If `path` isn't the active
    /// buffer, the host opens it first. Used by LSP
    /// locations, grep hits, outline jumps -- anywhere the
    /// destination might or might not already be open.
    JumpToLocation { path: PathBuf, line: u32, col: u32 },
    /// Dispatch an ex-command by id with the provided
    /// args. The command palette (`:picker commands`) uses
    /// this; future plugin sources may emit it for chained
    /// behaviors ("pick a thing, then run a command on it").
    InvokeCommand { id: String, args: Args },
    /// Paste a register's contents at the current cursor.
    /// `name` is the single-char register identifier
    /// (`a`-`z`, `0`-`9`, `"`, `+`, etc.).
    PasteRegister { name: char },
    /// Expand a snippet by id at the current cursor.
    ExpandSnippet { id: String },
    /// Open the per-server LSP log buffer.
    OpenLspLog { server_id: String },
    /// Open the per-server LSP trace-log buffer (distinct
    /// from the regular log; carries the protocol trace).
    OpenLspTraceLog { server_id: String },
    /// Apply a resolved LSP code action by index into the
    /// host's `pending_code_action_items` snapshot.
    /// `handle` is the cancellation token / request handle
    /// the action was registered against (carried by the
    /// host for resolve-then-apply correlation).
    ApplyLspCodeAction { handle: u64, index: u32 },
    /// Apply an LSP completion item by index into the
    /// host's `pending_completion_items` snapshot.
    ApplyLspCompletion { index: u32 },
    /// Picker dismissed without action -- source-side
    /// abort, accept-on-empty filter, etc. Host applies no
    /// mutation. Distinct from `Err` returned from `accept`
    /// (which echoes an error message); `NoOp` is the
    /// silent "nothing to do here" path.
    NoOp,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn open_file_clone_preserves_path() {
        let o = PickerAcceptOutcome::OpenFile {
            path: "/tmp/x.rs".into(),
        };
        let clone = o.clone();
        match clone {
            PickerAcceptOutcome::OpenFile { path } => assert_eq!(path, PathBuf::from("/tmp/x.rs")),
            other => panic!("expected OpenFile, got {other:?}"),
        }
    }

    #[test]
    fn jump_in_buffer_carries_coordinates() {
        let o = PickerAcceptOutcome::JumpInBuffer {
            buffer_id: 7,
            line: 41,
            col: 3,
        };
        match o {
            PickerAcceptOutcome::JumpInBuffer {
                buffer_id,
                line,
                col,
            } => {
                assert_eq!(buffer_id, 7);
                assert_eq!(line, 41);
                assert_eq!(col, 3);
            }
            other => panic!("expected JumpInBuffer, got {other:?}"),
        }
    }

    #[test]
    fn noop_compares_equal_via_format() {
        // `PickerAcceptOutcome` is intentionally not `PartialEq`
        // (Args isn't), so use Debug shape as a sanity check.
        let o = PickerAcceptOutcome::NoOp;
        assert_eq!(format!("{o:?}"), "NoOp");
    }
}
