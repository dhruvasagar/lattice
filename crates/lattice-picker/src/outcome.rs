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

/// YR.3: where a [`PickerAcceptOutcome::FillCaller`] puts its text.
///
/// **Captured when the picker is opened, never resolved when it
/// accepts.** By accept time the picker has been dismissed and the
/// modal state that identified the caller is gone; resolving then reads
/// whatever context is current. In the single-level case that is
/// usually the right answer, which is precisely the trap — it passes a
/// naive test and fails in the picker-inside-a-prompt case the feature
/// exists for. `Effect::CursorMoveIn` (name the buffer the position was
/// computed in) and MG.32's `<CR>` (ask the view before resolving the
/// path) are the same shape, arrived at the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FillTarget {
    /// Insert at the cursor in the buffer that was focused at open.
    Document,
    /// The `:` line.
    CommandLine,
    /// The `/` or `?` line.
    SearchLine,
    /// A one-line minibuffer prompt, named so a later prompt cannot
    /// receive text meant for the one that opened this picker.
    Prompt { buffer: u32 },
    /// The query of the picker that was showing when this one opened —
    /// the `M-y`-inside-a-picker case.
    PickerQuery,
    /// A transient menu's argument, parked while this picker is up.
    /// MG.53.e's case, folded in here rather than kept as a second
    /// mechanism: "which argument" is already recorded in the parked
    /// `PendingTransientArgument`, so this variant carries nothing.
    TransientArgument,
}

/// Issue #32 (2026-05-22): where a picker's file-opening
/// outcome should land. `<CR>` uses `Default` and the host's
/// preference (typically the active pane). `<C-s>` / `<C-v>` /
/// `<C-t>` override to a split / vsplit / new tab respectively.
///
/// Only the file-targeting outcome arms (`OpenFile`,
/// `SwitchBuffer`, `JumpInBuffer`, `JumpToLocation`) honor
/// this. Non-file outcomes (commands, registers, snippets,
/// LSP code actions) ignore it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OpenTarget {
    /// `<CR>` — host preference (active pane by default).
    #[default]
    Default,
    /// `<C-s>` — open in a new horizontal split below.
    Split,
    /// `<C-v>` — open in a new vertical split to the right.
    VSplit,
    /// `<C-t>` — open in a brand-new tab.
    Tab,
}

/// MG.54: result of [`PickerSourceGenerator::preview`], the hook that
/// fires as the SELECTION moves rather than on `<CR>`.
///
/// **Deliberately not [`PickerAcceptOutcome`]**, which it used to be.
/// That enum answers "what does accepting this candidate do", and it
/// worked as a preview vocabulary only by the accident that
/// `ApplyColorscheme` happens to mean the same thing in both contexts.
/// Showing text in a pane does not: it is a projection the host tears
/// down on `<Esc>`, never something a `<CR>` performs. Adding it to the
/// accept enum would have put a variant there that no accept path can
/// honour — and the next preview-only payload would have compounded it.
///
/// A plugin source implementing `preview` therefore gets a type whose
/// every variant is valid where it is returned (paramount #2).
///
/// [`PickerSourceGenerator::preview`]: crate::source::PickerSourceGenerator::preview
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerPreviewOutcome {
    /// T.12: apply the named theme while the selection sits on it. The
    /// host snapshots the pre-open theme on the first preview and
    /// restores it on `<Esc>`. Swaps the GLOBAL theme, not a buffer, so
    /// it is orthogonal to the buffer projection below.
    Colorscheme { name: String },
    /// MG.54: show `text` in the active pane as a read-only projection
    /// — the pane's committed buffer is untouched and snaps back when
    /// the picker closes.
    ///
    /// For content that has no file to read: a git blob at a revision,
    /// a generated listing, a plugin's rendering. `syntax_path` is the
    /// path the content *would* have (`src/main.rs` for
    /// `git show HEAD:src/main.rs`) and drives language detection only;
    /// nothing reads it from disk. `None` previews as plain text.
    ///
    /// The source hands over text it has ALREADY fetched. Whatever it
    /// costs to produce is the source's problem, and a source whose
    /// production is expensive says so via
    /// [`PickerSourceGenerator::preview_debounce`] so the host only
    /// asks once the selection settles.
    ///
    /// [`PickerSourceGenerator::preview_debounce`]: crate::source::PickerSourceGenerator::preview_debounce
    Buffer {
        /// Synthetic buffer name, shown wherever a buffer's name is
        /// (`*magit:file:HEAD:src/main.rs*`).
        name: String,
        text: String,
        syntax_path: Option<PathBuf>,
    },
}

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
    /// T.12: apply the named theme via the ThemeRegistry catalog;
    /// host calls apply_theme + signals ThemeChanged. Emitted by the
    /// colorscheme picker on both accept and live preview.
    ApplyColorscheme { name: String },
    /// MB.3: load `text` into the editable `:` command line WITHOUT
    /// executing it. Host routes through `Editor::open_command_line`,
    /// exactly as if the user had typed `:` and the text by hand —
    /// they then tweak (or `<C-x><C-e>` expand) and `<CR>` to run.
    /// Emitted by the `history` picker source (`q:` / `:history`).
    LoadCommandLine { text: String },
    /// MB.5: load `text` into the editable `/` search line WITHOUT
    /// executing it. Host routes through `Editor::open_search_line`
    /// (Forward direction) + `set_search_line_text` — the user tweaks
    /// and `<CR>` to execute. Emitted by the `search-history` picker
    /// source (`q/` / `q?` / `:history search`).
    LoadSearchLine { text: String },
    /// Open a generic one-line minibuffer text prompt —
    /// picker-accept's peer of `Effect::OpenPrompt` (same fields, same
    /// name-based `on_submit_action` lookup, no closures). Lets a
    /// source chain "pick an item, then type a value" (e.g. magit's
    /// branch-create: pick the base branch via this picker, then
    /// prompt for the new branch's name) without inventing bespoke
    /// per-source plumbing. `buffer_name`, when set, stashes context
    /// (e.g. the picked base branch) in the prompt buffer's synthetic
    /// name for the submit handler to read back.
    OpenPrompt {
        prompt: String,
        initial: String,
        on_submit_action: String,
        buffer_name: Option<String>,
    },
    /// YR.3 / MG.53.e: the picked item's **text**, for whatever opened
    /// the picker.
    ///
    /// The outcome for a source that answers a question rather than
    /// performing an action. Where the text lands is not the source's
    /// business and not encoded here — the host consumes the
    /// [`FillTarget`] it captured when the picker was opened. The same
    /// `file-pick` list fills a magit argument; the same `yank-ring`
    /// list fills the document, the `:` line, a prompt, or another
    /// picker's query.
    ///
    /// Deliberately not `InvokeCommand`. A source that supplies text
    /// does not know, and must not decide, what the text is for. Baking
    /// a command into the source would mean one registered source per
    /// consumer, which is the duplication this exists to avoid.
    ///
    /// The host echoes and drops it when no target was captured; text
    /// arriving with nowhere to go is a wiring bug, not a user error,
    /// and silently discarding it would present as a picker that does
    /// nothing.
    FillCaller { text: String },
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
