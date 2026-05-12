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

use crate::app_effect::AppEffect;
use crate::command::CommandInvocation;
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
    SaveBuffer {
        path: Option<PathBuf>,
    },
    /// `:q` / `:q!` -- quit the editor. `force = true` ignores dirty state.
    QuitEditor {
        force: bool,
    },
    /// `:e[!] [path]` -- swap the current document for the file at `path`.
    /// With `path = None` reload from the document's existing path.
    /// `force = true` discards unsaved changes.
    OpenBuffer {
        path: Option<PathBuf>,
        force: bool,
    },
    /// `:set <option>` -- the host parses the option spec; the closure
    /// just hands the raw text through.
    SetOption {
        spec: String,
    },
    /// `:noh[lsearch]` -- clear the hlsearch overlay.
    ClearSearchHighlight,
    /// Display a one-line message in the echo area.
    Echo {
        level: EchoLevel,
        text: String,
    },
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
    /// `body` is a pre-parsed [`CommandInvocation`] -- the parser
    /// front-end (`lattice-ui-tui::excommand`) compiles it once at
    /// `:g` parse time so the host doesn't re-parse per matching
    /// line, and so body parse errors surface before `:g` fires.
    Global {
        pattern: String,
        inverted: bool,
        body: Box<CommandInvocation>,
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
    ///
    /// `anchor` (optional) tells the host to scroll the help to a
    /// named anchor after rendering -- used by the cmdline's
    /// arg-aware `<C-h>` to land on `arg:<name>` directly.
    DescribeCommand {
        name: String,
        anchor: Option<String>,
    },
    /// `:describe-buffer`. The host renders a snapshot of the current
    /// buffer's view-relevant state (path, language, modal, cursor,
    /// dirty, line count, ...).
    DescribeBuffer,
    /// `:apropos <pattern>`. The host runs a substring search over
    /// every registered `CommandSpec` (name + doc) and renders the
    /// matches.
    Apropos {
        pattern: String,
    },
    /// `:describe-key <chord>` (DESIGN.md §5.11). The host queries
    /// its keymap registry for every binding of `chord` (a chord may
    /// appear in multiple modes -- Normal / Visual / Help, etc.) and
    /// renders them.
    DescribeKey {
        chord: String,
    },
    /// `:keymap`. The host renders the full default keymap grouped by
    /// mode.
    ListKeymap,
    /// `:bn[ext]` -- cycle to the next open document buffer.
    BufferNext,
    /// `:bp[rev]` -- cycle to the previous open document buffer.
    BufferPrev,
    /// `:ls` / `:buffers` -- render every open document buffer in a
    /// help-style view.
    ListBuffers,
    /// `:b` with no arg -- open the vertico-style buffer switcher
    /// (DESIGN.md §5.9.7). The user types to filter, `<CR>` to
    /// switch. Type-aware completion in the cmdline can pre-fill
    /// the picker via `:b <prefix>` once that wiring lands; for now
    /// the no-arg form is the entry point.
    OpenBufferPicker,
    /// `:picker <source> [args...]` -- canonical picker entry
    /// point. Dispatches by `source` against the host's
    /// `PickerRegistry`. Unknown source ids surface as a
    /// host-side error echo; per-source arg shape is opaque to
    /// the grammar (the host re-parses `args` against the
    /// resolved source's `args_schema`). Short per-source
    /// aliases like `:files`, `:recent` emit this same effect
    /// with the appropriate `source` set so the trait-driven
    /// dispatch + MRU pipeline runs uniformly.
    OpenPicker {
        source: String,
        args: Vec<String>,
    },
    /// `:bd[elete][!]` -- close the active document buffer.
    /// `force = true` discards unsaved changes.
    BufferDelete {
        force: bool,
    },
    /// `:Tree [path]` -- open a file-tree buffer rooted at `path`.
    /// Absent = the document's parent directory.
    OpenFileTree {
        root: Option<PathBuf>,
    },
    /// `:TreeClose` -- dismiss the file-tree buffer.
    CloseFileTree,
    /// `:Oil [path]` -- open an oil buffer for `path` (flat editable listing).
    /// Absent = current document's parent directory / cwd.
    OpenOil {
        dir: Option<PathBuf>,
    },
    /// `:describe-option NAME` -- render the option's metadata in
    /// a help view.
    DescribeOption {
        name: String,
    },
    /// `:options` -- list every registered option.
    ListOptions,
    /// `:hover [text]` -- open a hover popup at the cursor with
    /// `text` as the markdown body. v1 path: lets the user
    /// validate the popup positioning + dismissal; Phase 4 LSP
    /// will source `text` from a real `textDocument/hover` reply.
    OpenHover {
        markdown: String,
    },
    /// `:HoverClose` -- dismiss the active hover popup.
    CloseHover,
    /// `:help [topic]` -- open a free-form help topic. With no
    /// topic the host renders the index (`docs/user/README.md`
    /// equivalent); with a topic the host looks it up in its
    /// help-topic registry and surfaces the body in a help
    /// buffer. Unknown topics echo an error.
    OpenHelpTopic {
        topic: Option<String>,
    },

    /// `:diagnostics` -- render every workspace diagnostic in a
    /// help-style buffer with clickable per-entry source links
    /// (Phase 4.1.d.iv). The host queries its
    /// `LspSupervisor::diagnostics()` layer and formats.
    ListDiagnostics,
    /// `]d` / `:diag-next` / `:cnext` -- move the cursor to the
    /// next diagnostic in the active buffer. Wraps to top.
    NextDiagnostic,
    /// `[d` / `:diag-prev` / `:cprev` -- move the cursor to the
    /// previous diagnostic in the active buffer. Wraps to
    /// bottom.
    PrevDiagnostic,

    /// `:lsp-log [server]` (Phase 4.1.g) -- open the subsystem
    /// log buffer (`*lsp*`) when `server_id` is None, or the
    /// per-server log (`*lsp:<server>*`) when set.
    OpenLspLog {
        server_id: Option<String>,
    },
    /// `:messages` -- open the `*messages*` buffer (the emacs
    /// `*Messages*` analogue). Renders a chronological view
    /// of every echo / minibuffer notification; live-tails as
    /// new entries arrive via the typed event bus.
    OpenMessages,
    /// `:lsp-trace <server>` -- pure toggle of JSON-RPC tracing
    /// for the server. The trace buffer is opened separately via
    /// `:lsp-trace-log <server>` so peeking mid-stream doesn't
    /// flip the toggle off.
    ToggleLspTrace {
        server_id: String,
    },
    /// `:lsp-trace-log [server]` -- open the JSON-RPC trace ring
    /// (`*lsp:<server>:trace*`) in the active pane via the
    /// vertico picker (Phase 3). No arg = picker over every
    /// running instance; arg = pre-filter; single match short-
    /// circuits the picker. Independent of the trace toggle.
    OpenLspTraceLog {
        server_id: Option<String>,
    },
    /// `:lsp-status` -- render every running server (id, root,
    /// pid, uptime, capability summary) in a help-style buffer.
    LspStatus,
    /// `:lsp-server-log` -- picker-style listing of every running
    /// server actor with workspace root + buffer count +
    /// capability summary, each row carrying `exec:` links to
    /// the per-server log + trace buffers. Use vim search
    /// (`/query`) to filter rows; press `<CR>` on a link to
    /// open. A real fuzzy picker arrives with the bundled
    /// fuzzy-finder plugin (Phase 8b).
    LspServerLogListing,
    /// `:lsp-restart <server>` -- supervisor force-restart with
    /// backoff. Wired but no-op until the supervisor's restart
    /// path lands (4.4).
    LspRestart {
        server_id: String,
    },
    /// `:lsp-progress-cancel [server]` -- send
    /// `window/workDoneProgress/cancel` for every active,
    /// cancellable progress entry on the named server (or, with
    /// no arg, on every server currently attached to the active
    /// buffer). Non-cancellable entries are left alone — the
    /// host's cancel is best-effort regardless. 4.4.c.
    LspProgressCancel {
        server_id: Option<String>,
    },
    /// 4.4.e: `:lsp-expand-region` -- structural smart-
    /// expansion. First invocation fires `textDocument/selectionRange`
    /// at the cursor; each subsequent invocation walks one
    /// `parent` step outward in the cached chain. Enters Visual
    /// mode with the resolved range as the selection.
    LspExpandRegion,
    /// 4.4.e: `:lsp-shrink-region` -- inverse walk through the
    /// cached selection-range chain. No-op when the chain is
    /// empty / at the innermost step.
    LspShrinkRegion,
    /// `:lsp-log-level [server] <level>` -- set the subsystem-
    /// wide default min level (when `server_id` is None) or a
    /// per-server override.
    SetLspLogLevel {
        server_id: Option<String>,
        level: String,
    },
    /// `:lsp-log-clear [server]` -- drop the ring's records.
    /// `None` clears the subsystem-wide ring; a server id
    /// clears that ring.
    LspLogClear {
        server_id: Option<String>,
    },
    /// `:lsp-symbols` -- open a picker over the active document's
    /// LSP symbol outline (`textDocument/documentSymbol`). Phase
    /// 4.2.e.
    LspDocumentSymbol,
    /// `:lsp-workspace-symbol [query]` -- open a picker over
    /// workspace-scoped symbols matching `query` (server-side
    /// substring filter). Phase 4.2.f.
    LspWorkspaceSymbol {
        query: String,
    },
    /// `:lsp-incoming-calls` -- 4.5.a. Prepares call-hierarchy
    /// items at the cursor, fans out `callHierarchy/incomingCalls`
    /// for the first item, opens the merged caller list as a
    /// vertico picker. "Who calls this function?"
    LspIncomingCalls,
    /// `:lsp-outgoing-calls` -- 4.5.a. Symmetric peer of
    /// `LspIncomingCalls`. "What does this function call?"
    LspOutgoingCalls,
    /// `:lsp-supertypes` -- 4.5.b. Same shape as
    /// `LspIncomingCalls` but for type relationships: prepares
    /// type-hierarchy items, fans out
    /// `typeHierarchy/supertypes`, opens the picker. "What
    /// does this type subtype?"
    LspSupertypes,
    /// `:lsp-subtypes` -- 4.5.b. Symmetric peer of
    /// `LspSupertypes`. "What subtypes this type?"
    LspSubtypes,
    /// `:lsp-moniker` -- 4.5.g. Fires `textDocument/moniker`
    /// at the cursor; echoes the resulting moniker list
    /// (scheme + identifier + unique level + optional kind).
    /// Useful for indexers (SCIP / LSIF) + cross-repo
    /// navigation; the result surfaces as a one-line
    /// summary, not a picker.
    LspMoniker,
    /// `:lsp-code-lens` -- 4.5.d. Open a picker over the
    /// cached `textDocument/codeLens` entries for the active
    /// buffer. Accept routes the chosen lens's `command`
    /// through `workspace/executeCommand` (after a lazy
    /// `codeLens/resolve` if the lens arrived without a
    /// command).
    LspCodeLens,
    /// `:lsp-color-presentation` -- 4.5.e. At the cursor,
    /// look up the color literal in the per-buffer
    /// `documentColor` cache and fire
    /// `textDocument/colorPresentation` to fetch alternative
    /// formats (named, rgb(), hex, etc.). Open a picker;
    /// accept splices the chosen alternative.
    LspColorPresentation,
    /// `:format` -- run `textDocument/formatting` on the highest-
    /// priority server with `documentFormattingProvider` and
    /// apply the returned edits as one undo unit. Phase 4.3.
    LspFormat,
    /// `:format-range` -- run `textDocument/rangeFormatting`
    /// over the active Visual selection (when in Visual mode)
    /// or the supplied line range. Apply edits atomically.
    /// Phase 4.3.
    LspFormatRange,
    /// `:signature-help` (or trigger-character driven). Send
    /// `textDocument/signatureHelp` to attached servers; first
    /// non-empty response renders into the hover popup.
    LspSignatureHelp,
    /// `:complete` -- fire `textDocument/completion` at the
    /// cursor and open the merged item list as a vertico
    /// picker. Phase 4.2.g.
    LspComplete,
    /// `:rename <new-name>` -- run textDocument/prepareRename
    /// (when advertised) then textDocument/rename; apply the
    /// returned WorkspaceEdit as one undo unit across every
    /// affected buffer. Phase 4.3.
    LspRename {
        new_name: String,
    },
    /// `:code-actions` -- run textDocument/codeAction at the
    /// cursor / selection; open the merged item list as a
    /// vertico picker. Accept routes through resolve (when
    /// needed) and applies the action's WorkspaceEdit /
    /// command. Phase 4.3.
    LspCodeAction,
    /// `:snippet-expand` -- direct snippet expansion at the
    /// cursor (Phase 4.2.g.4). Looks up the word at the
    /// cursor in the per-language snippet registry; expands
    /// the first matching snippet without surfacing the
    /// completion popup. No-op when no snippet matches.
    /// Surface-form alias of the `<C-x><C-s>` chord.
    SnippetExpand,
    /// `:reload-snippets` -- re-read every snippet file from
    /// disk and rebuild the per-language registry (Phase
    /// 4.2.g.4). Useful after editing a `.code-snippets` /
    /// `.json` file in the project's snippet directory.
    ReloadSnippets,

    /// `:describe-events` -- render a help buffer listing every
    /// registered event (M.5.3.c). Walks
    /// `lattice_protocol::event_registry::EVENT_DESCRIPTORS` and
    /// formats each as `name :: source-crate :: doc`.
    DescribeEvents,
    /// `:describe-event <name>` -- render the descriptor for a
    /// single registered event (M.5.3.c). The introspection
    /// counterpart of `:describe-command` for events.
    DescribeEvent {
        name: String,
    },

    /// `:list-modes` -- render every registered mode in a help
    /// buffer (M.8). Groups by kind (Major / Minor); each row
    /// shows the mode's id and current activation state on the
    /// active buffer. The mode counterpart of `:options`.
    ListModes,
    /// `:describe-mode <name>` -- render one mode's metadata
    /// (M.8): id, kind, contributed option overrides,
    /// required capabilities, and current activation state on
    /// the active buffer. The introspection counterpart of
    /// `:describe-command` / `:describe-option` /
    /// `:describe-event` for modes.
    DescribeMode {
        name: String,
    },
    /// `:describe-option-resolution <name>` -- show which
    /// resolver layer (modal / buffer-local / mode
    /// contribution / typed-option / default) provides the
    /// resolved value for `<name>` on the active buffer
    /// (M.8). Helps debug surprising values when a mode's
    /// contribution shadows a `:set` write or vice versa.
    DescribeOptionResolution {
        name: String,
    },

    /// `:customize [name]` -- open the customize buffer
    /// (M.9). With no arg, opens the group + mode picker.
    /// With an arg ending in `-mode`, opens the focused
    /// view of that mode's contributed options. Otherwise,
    /// opens the cross-mode group view (every option in the
    /// named group, sectioned by owning mode).
    ///
    /// M.9.0 ships the read-only listing form. M.9.1 wires
    /// per-row navigation + Enter-to-edit; for now edits run
    /// via the existing `:set` machinery on the cmdline.
    Customize {
        name: Option<String>,
    },
    /// `:tutor [N]` -- open the interactive tutor lesson `N`
    /// (default: 1) in a fresh editable buffer. The lesson
    /// content is embedded in the binary and copied to a
    /// temp file each time so the user starts fresh and can
    /// practice motions / operators on the file itself
    /// (vim-tutor pattern). v1 ships lesson 1 only;
    /// subsequent lessons land as separate
    /// `docs/user/tutor/lesson-N.md` files registered through
    /// the same handler.
    Tutor {
        lesson: Option<u32>,
    },
    /// `:<mode-name>` -- toggle a registered mode on the active
    /// buffer (M.5.1; mode-architecture §9.6.1). For minors:
    /// activate if inactive, deactivate if active. For majors:
    /// activate if not currently the major; reload (deactivate
    /// then re-activate) if it's already the active major. Mode
    /// resolution is by name, not id object, because the grammar
    /// crate stays renderer-agnostic and doesn't depend on
    /// `lattice-mode`.
    ToggleMode {
        mode_name: String,
    },

    /// Free-form App-side effect produced by a `CommandKind::Action`
    /// dispatch. Carries an [`AppEffect`] -- the typed App-side
    /// counterpart to the dispatcher-native variants above. The
    /// host's `apply_effect` matches the inner `AppEffect` to drive
    /// chord-bound work that has no grammar concept attached
    /// (`<Esc>` exits Visual, `<C-w>v` splits a pane, `o` opens a
    /// line below). Slice 8.i wires this surface; see
    /// `docs/dev/notes/8i-approach.md`.
    AppAction(AppEffect),

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
            Effect::Yank {
                register,
                content,
                kind,
            } => {
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
