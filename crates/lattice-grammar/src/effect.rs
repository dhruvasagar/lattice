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
///
/// **msg-mode.1 (tracing bridge):** the five variants mirror
/// `tracing::Level` exactly so `App::set_message` can route through
/// a single `tracing::event!` call without lossy conversion. `Trace` +
/// `Debug` are below the default `Info` filter so they don't show
/// in the echo area today — they exist for subsystems that want
/// verbose records in `*messages*` (e.g. `editor=trace`, `lsp=debug`)
/// to surface without inventing a parallel severity scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EchoLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl From<EchoLevel> for tracing::Level {
    fn from(level: EchoLevel) -> Self {
        match level {
            EchoLevel::Trace => tracing::Level::TRACE,
            EchoLevel::Debug => tracing::Level::DEBUG,
            EchoLevel::Info => tracing::Level::INFO,
            EchoLevel::Warn => tracing::Level::WARN,
            EchoLevel::Error => tracing::Level::ERROR,
        }
    }
}

impl From<tracing::Level> for EchoLevel {
    fn from(level: tracing::Level) -> Self {
        // `tracing::Level` is a unit-struct wrapper; match via
        // associated consts because the inner repr is not pub.
        match level {
            tracing::Level::TRACE => EchoLevel::Trace,
            tracing::Level::DEBUG => EchoLevel::Debug,
            tracing::Level::INFO => EchoLevel::Info,
            tracing::Level::WARN => EchoLevel::Warn,
            tracing::Level::ERROR => EchoLevel::Error,
        }
    }
}

/// Scope for `Effect::Substitute`. Mirrors vim's `:s/.../.../` (current
/// line) vs. `:%s/.../.../` (whole buffer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SubstituteScope {
    CurrentLine,
    Whole,
}

/// Scope for `Effect::QuitEditor`. Mirrors vim's `:q` (close the active
/// pane; quit only when it is the last one) vs. `:qa` (quit the editor
/// regardless of how many panes / tabs are open). `:q` and `:qa` stay
/// distinct *commands* (separate registrations + aliases); `QuitScope`
/// is the one axis on which they differ, so the shared shutdown + dirty
/// guard stays in a single `Editor::do_quit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum QuitScope {
    /// `:q[!]` -- close the active pane when more than one is open;
    /// run the dirty guard and shut the editor only on the last pane.
    Pane,
    /// `:qa[!]` -- ignore pane / tab count; run the dirty guard and
    /// shut the editor outright.
    All,
}

/// BC.8c: a UTF-16 code-unit column on a given line. LSP's default
/// position encoding counts UTF-16 code units, not bytes; converting to
/// Lattice's byte offset (`lattice_protocol::Position.byte`) needs the
/// target line's text, which only exists after the file is open. So
/// [`Effect::OpenBufferAtColumn`] carries this *unconverted* column for
/// the host to resolve post-open. Plain `u32`s — no `lsp_types` leak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Utf16Pos {
    pub line: u32,
    pub col: u32,
}

/// L7 (lsp-architecture.md §16): which LSP **navigation** request a
/// mode-owned nav chord (`K` / `gd` / `gD` / `gy` / `gI` / `gr` / `gx`)
/// wants the host to fire. Pure data — no `lsp_types`, no `lattice-lsp`
/// dependency — so it can ride inside the host-owned [`Effect::Lsp`]
/// boundary. `lsp-mode`'s `action_handlers()` closure returns
/// `Effect::Lsp(LspRequest::X)`; the host's `editor.lsp_request`
/// dispatcher maps each arm onto the existing (unchanged) async request
/// substrate (`lsp_hover_request` / `lsp_nav_request` /
/// `lsp_references_request` / `do_lsp_follow_link_at_cursor`). The
/// handler carries no position — the substrate reads live `Editor`
/// cursor/scroll, so the popup/jump anchors to the symbol the chord
/// fired on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspRequest {
    /// `K` -- `textDocument/hover`.
    Hover,
    /// `gd` -- `textDocument/definition`.
    Definition,
    /// `gD` -- `textDocument/declaration`.
    Declaration,
    /// `gy` -- `textDocument/typeDefinition`.
    TypeDefinition,
    /// `gI` -- `textDocument/implementation`.
    Implementation,
    /// `gr` -- `textDocument/references`.
    References,
    /// `gx` -- follow the `textDocument/documentLink` covering the cursor.
    FollowLink,
    /// LR.2 (2026-08-11): `:lsp-references` -- the same
    /// `textDocument/references` query as [`Self::References`], landing
    /// at the **editable multibuffer** terminus instead of the picker.
    ///
    /// A data arm rather than a new `Effect` variant, per §16's grain:
    /// a further LSP surface adds an arm here, not a host `Action`, not
    /// a renderer classifier entry. The request, cancellation token,
    /// anchor stash and wake are the unchanged substrate; only what the
    /// drain does with the result differs.
    ReferencesView,
    /// LR.3 (2026-08-11): `gr` **inside** a references view — re-run the
    /// query at the view's stored origin and rebuild it in place.
    ///
    /// Distinct from [`Self::ReferencesView`] because the position it
    /// queries is different: this one must NOT read the live cursor.
    /// By the time a refresh fires the cursor sits inside the
    /// multibuffer, so querying there would ask about whatever symbol
    /// happens to be under it — a different question with a
    /// plausible-looking answer. The substrate reads the origin the
    /// view recorded when it opened.
    ReferencesViewRefresh,
}

#[derive(Debug, Clone)]
pub enum Effect {
    None,
    /// AP.0.2: the action DECLINES this chord — it did nothing, and the
    /// dispatcher should re-resolve the chord as if this action's keymap layer
    /// weren't present, falling through to the next binding (a lower-priority
    /// minor, the builtin/user layer, or Insert-mode self-insert). The
    /// `with-eval-after-load` of keymaps: a plugin action (auto-pair's manual
    /// close key / backspace) declines when it has nothing to do, so the key
    /// still does whatever else is bound (completion nav, a normal backspace, a
    /// user remap). Distinct from `None` (a no-op that CONSUMES the chord).
    Declined,
    Edits(Vec<AppliedEdit>),
    /// CR.0: a generic "apply this edit to this buffer" primitive.
    ///
    /// Mode-contributed action handlers (the snippet / lsp
    /// `action_handlers()` pattern) compute an edit against their own
    /// state — a diff hunk get/put, a conflict resolution — and hand it
    /// back through this effect. The host applies `edit` to `target`,
    /// routing through the active-document pipeline (LSP `didChange` +
    /// syntax reparse + highlight shift) when `target` is the focused
    /// buffer, or the peer-buffer registry handle otherwise; when
    /// `cursor` is `Some`, it then parks the active cursor at that
    /// **position** (line + byte) — column-precise, so a plugin action
    /// (auto-pair) can place the caret *between* an inserted pair, not
    /// only at a row start (AP.2). Native row-start callers pass
    /// `Position::new(row, 0)`.
    ///
    /// Distinct from [`Effect::Edits`], which carries `AppliedEdit`s the
    /// grammar dispatcher already applied to the active document (routed
    /// only for side effects). `ApplyEdit` carries a *pending* `Edit` the
    /// host has yet to apply, addressed at an explicit `target`.
    ///
    /// The Effect *vocabulary* is the host boundary by design
    /// (`feedback_effect_vocabulary_is_host_boundary`): this lets a mode
    /// drive an arbitrary document edit without the host growing a
    /// feature-specific `Action` variant + `do_<x>` method per feature.
    ApplyEdit {
        target: lattice_core::BufferId,
        edit: lattice_protocol::edit::Edit,
        cursor: Option<lattice_protocol::position::Position>,
    },
    SelectionChange(SelectionSet),
    /// Move the cursor to `target` without affecting the selection.
    /// The semantically-clean cursor-only jump — use this for navigation
    /// chords (]]/[[, ]c/[c, ]f/[f) rather than overloading SelectionChange
    /// with a collapsed cursor. The host writes `editor.cursor = target`.
    CursorMove(lattice_protocol::position::Position),
    /// MG.18d: [`Effect::CursorMove`] addressed at a **specific buffer** —
    /// the host moves the cursor only while `target` is the focused
    /// buffer, and drops the effect otherwise.
    ///
    /// The peer of [`Effect::ApplyEdit`]'s `target` field, for the
    /// cursor half. A chord-time `CursorMove` needs no target because the
    /// buffer it fired in *is* the focused one; an **async** producer has
    /// no such guarantee. Between a magit stage and the refresh that
    /// finishes it lie two git calls, and a `q` or `C-^` in that window
    /// would otherwise land the jump in whatever buffer the user moved
    /// to — a caret teleporting in a file they were about to type in.
    ///
    /// Dropping (rather than stashing for the buffer's return) is
    /// deliberate: the position was computed against content that will
    /// have been rebuilt again by the time focus comes back, and a
    /// stale jump is worse than none. Producers that want position
    /// restored on return use marks / position history, which are
    /// per-buffer by construction.
    CursorMoveIn {
        target: lattice_core::BufferId,
        position: lattice_protocol::position::Position,
    },
    Yank {
        register: Register,
        content: String,
        kind: YankKind,
        /// `true` when this write came from an explicit **yank** (`y`,
        /// `yy`, Visual `y`); `false` for the register writes that
        /// delete / change / `x` also perform. Drives the *yank-only*
        /// system-clipboard mirror (`clipboard.md` §5): the host mirrors
        /// to the OS clipboard only when this is an explicit yank (under
        /// the `clipboard` option) or the target is the `+`/`*` register,
        /// so an incidental delete never clobbers the clipboard.
        explicit_yank: bool,
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
    /// `:q[!]` (`scope = Pane`) / `:qa[!]` (`scope = All`) -- quit.
    /// `force = true` ignores dirty state. The two are distinct
    /// *commands* but one *effect*: quit is a single host operation
    /// parameterized by scope, exactly as `force` is a parameter.
    /// `Pane` closes the active pane when more than one is open and
    /// only shuts the editor on the last pane (vim's `:q`); `All`
    /// ignores pane/tab count and shuts the editor outright (vim's
    /// `:qa`). The dirty guard (unless forced) is identical for both
    /// and lives once in `Editor::do_quit`.
    QuitEditor {
        force: bool,
        scope: QuitScope,
    },
    /// `:e[!] [path]` -- swap the current document for the file at `path`.
    /// With `path = None` reload from the document's existing path.
    /// `force = true` discards unsaved changes.
    OpenBuffer {
        path: Option<PathBuf>,
        force: bool,
    },
    /// M.10.3 bug fix (2026-06-03): atomic "open file + position
    /// cursor." Used by mode-contributed jump handlers (search
    /// `<CR>`, lsp-references `<CR>`, future project-diff `<CR>`)
    /// so the cursor lands at the matched (row, byte) inside
    /// the newly-opened buffer on the FIRST render.
    ///
    /// Necessary because `Effect::SelectionChange` runs
    /// synchronously in the host's `handle_effect` (writes to
    /// `editor.cursor` against whatever buffer is active at
    /// that moment), while `Effect::OpenBuffer` is renderer-
    /// coupled — applied LATER by the TUI/GPUI peers via
    /// `do_edit`. The two can't be ordered to land cursor on
    /// the new buffer without an atomic step. The peer
    /// renderer's arm for this variant calls `do_edit` THEN
    /// `set_selections_blocking` in a single atomic block.
    ///
    /// Pre-fix, `<CR>` on a search hit opened the file but
    /// landed at (0,0) because the host's SelectionChange ran
    /// first (against the still-active multibuffer) and the
    /// later `do_edit` reset cursor for the freshly-loaded
    /// document.
    OpenBufferAt {
        path: Option<PathBuf>,
        position: lattice_protocol::position::Position,
        force: bool,
    },
    /// BC.8c: open `uri` via the OS handler (`open` / `xdg-open` /
    /// `explorer`). Emitted by the LSP `window/showDocument` handler for
    /// `external: true` requests; generic enough to reuse for any "open
    /// this URL externally" need (`gx`, markdown / help external links).
    /// A plain URL string — no `lsp_types` leak into the grammar.
    /// **Host-applied** in `Editor::handle_effect`: the spawn is a host
    /// side-effect, and the showDocument bus drains off-keystroke through
    /// the generic inbound tick-callback (where peer-applied effects are
    /// not forwarded), so the work must run host-side.
    OpenExternalUri {
        uri: String,
    },
    /// BC.8c: **host-applied** atomic open + optional UTF-16-column
    /// cursor placement. Unlike [`Effect::OpenBufferAt`] (peer-applied,
    /// carrying a pre-converted byte offset), this runs entirely in
    /// `Editor::handle_effect` so it works on the off-keystroke async
    /// path: server-initiated `window/showDocument` drains through the
    /// generic inbound tick-callback, where peer-applied effects are
    /// discarded — the open must happen host-side (as the retired
    /// `drain_inbound_show_documents` did via `do_edit`).
    ///
    /// `column = None` opens only, leaving the cursor where `do_edit`
    /// puts it (the no-selection showDocument case). `Some` positions the
    /// cursor at the UTF-16 code-unit column, converted to a byte offset
    /// against the opened line — the conversion needs the line text, which
    /// only exists post-open, which is why the column travels unconverted.
    OpenBufferAtColumn {
        path: Option<PathBuf>,
        column: Option<Utf16Pos>,
        force: bool,
    },
    /// I5.1 (Claude Code IDE peer): spawn a child process in a new
    /// `BufferKind::Terminal` buffer, optionally injecting extra environment
    /// and activating a minor mode on the new buffer. Host-applied (the open is
    /// irreducibly `&mut Editor`): the host calls `do_terminal_spawn` with
    /// `cmd_line` + `env`, then activates `activate_minor` (a minor mode, by its
    /// mode-id name) on the spawned buffer when set. `:terminal` reaches the
    /// same host path via the host-side `AppEffect::TerminalSpawn`; this grammar
    /// variant lets crate-owned ex-commands — the IDE peer's `:claude`, which
    /// must inject `CLAUDE_CODE_SSE_PORT` + `ENABLE_IDE_INTEGRATION` — request
    /// the spawn through the Effect vocabulary (the host boundary) instead of a
    /// bespoke channel.
    SpawnTerminal {
        /// Command line (`program [args...]`); `None` spawns `$SHELL`.
        cmd_line: Option<String>,
        /// Extra environment injected on top of the inherited parent env.
        env: Vec<(String, String)>,
        /// Minor mode to activate on the spawned buffer, by mode-id name (e.g.
        /// `"claude-code-mode"`); `None` leaves the buffer mode-bare.
        activate_minor: Option<String>,
    },
    /// D-fix.4 (Claude Code IDE peer): write raw bytes to the focused
    /// terminal's PTY. Host-applied (`do_terminal_input`, irreducibly
    /// `&mut Editor` + the terminal registry). The IDE peer's
    /// `:claude-interrupt` emits `TerminalInput(vec![0x1b])` to forward an
    /// `<Esc>` to the running `claude` CLI — `<Esc>` can't be sent by typing
    /// because the terminal's modal layer consumes it for Insert→Normal, so
    /// an ex-command is the only interrupt path. Targets the active pane's
    /// terminal (the focused claude session); a no-op (logged) when the
    /// active buffer isn't a terminal.
    TerminalInput(Vec<u8>),
    /// `:set <option>` -- the host parses the option spec; the closure
    /// just hands the raw text through.
    SetOption {
        spec: String,
    },
    /// `:setlocal <option>` -- like `SetOption` but writes to the
    /// buffer-local override layer for the active buffer only, without
    /// touching the global config registry.
    SetLocalOption {
        spec: String,
    },
    /// `:setglobal <option>` -- like `SetOption` but only writes the
    /// global config registry without updating any buffer-local override
    /// layers. Reads back the global value on `:setglobal name?`.
    SetGlobalOption {
        spec: String,
    },
    /// `:noh[lsearch]` -- clear the hlsearch overlay.
    ClearSearchHighlight,
    /// `:colorscheme <name>` (T.9.b) -- swap the active theme by name.
    /// The host looks `name` up in `lattice_theme::builtin_themes()`
    /// and calls `ThemeRegistry::set_theme` (palette + override swap),
    /// then emits `RendererSignal::ThemeChanged` so both renderers
    /// rebuild their caches. An unknown name echoes a host-side error.
    /// The closure only packages the name (it has no registry access).
    SetColorscheme(String),
    /// Display a one-line message in the echo area.
    Echo {
        level: EchoLevel,
        text: String,
    },
    /// L4b (lsp-architecture.md §15): show a cursor-anchored popup with
    /// the pre-formatted diagnostic lines for the cursor's line. The
    /// owning mode (`lsp-diagnostics-mode`) formats `lines` in its
    /// `gl` handler; the host renders them through the hover popup
    /// pipeline (`HelpContent` → `DisplayBufferRequest`,
    /// `PopupPlacement::CursorAnchored`). Empty `lines` → host echoes
    /// "no diagnostics on line" instead of an empty popup. Each line is
    /// `(text, severity_rank)` where rank is Error = 0 … Hint = 3
    /// (matching `lattice_lsp`'s severity_rank); the host colours each
    /// popup line by its severity via the matching `Style::Diagnostic*`
    /// highlight.
    ShowDiagnosticsPopup {
        lines: Vec<(String, u8)>,
    },
    /// L7 (lsp-architecture.md §16): fire a mode-owned LSP **navigation**
    /// request (`K` / `gd` / `gD` / `gy` / `gI` / `gr` / `gx`). The
    /// owning `lsp-mode` `action_handlers()` closure decides *which*
    /// request via [`LspRequest`]; the host's `editor.lsp_request`
    /// dispatcher runs the (unchanged) async request substrate
    /// host-side. Host-applied — both renderers treat it as a
    /// host-handled no-op in their effect classifiers, and it neither
    /// mutates nor yanks. `LspRequest::FollowLink` is the one variant
    /// that yields `RendererSignal`s synchronously (open buffer / OS
    /// handler); the apply arm extends `out.renderer_signals`.
    Lsp(LspRequest),
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
    /// `:cd [path]` -- change the editor's working directory.
    /// No arg changes to the user's home directory.
    ChangeDir(Option<String>),
    /// `:pwd` -- print the current working directory.
    PrintWorkingDir,
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
    /// `:describe-element NAME` / `:describe-face NAME` (T.9.d) --
    /// render a registered theme element's metadata in a help view:
    /// owner, doc, the authoring (reference-form) `StyleSpec` default
    /// (palette keys + inherit parent), and the concrete resolved
    /// `Style`. The introspection counterpart of `:describe-option` /
    /// `:describe-mode` for theme elements. The host reads the
    /// `ThemeRegistry::describe` snapshot; an unknown name echoes an
    /// error.
    DescribeElement {
        name: String,
    },
    /// `:options` -- list every registered option.
    ListOptions,
    /// `:describe-plugin-api [<seam>]` (PI.2) -- render the plugin-API
    /// catalog. With `seam` = an interface name (`host-services`,
    /// `picker-source`, ...), render that one interface's functions +
    /// direction + capability. Without, render the same as
    /// `:list-plugin-apis`. The catalog is derived from `wit/` at build
    /// time (`lattice-plugin-api`); the host holds no plugin runtime.
    DescribePluginApi {
        seam: Option<String>,
    },
    /// `:list-plugin-apis` (PI.2) -- list every plugin-API interface the
    /// `wit/` package exposes, one row per interface (name + direction +
    /// capability + function count).
    ListPluginApis,
    /// `:export-plugin-api [markdown|json]` (PI.2b) -- dump the whole
    /// plugin-API catalog into a savable synthetic buffer (`*plugin-api.md*`
    /// / `*plugin-api.json*`) the author saves with `:w <path>`. `format`
    /// defaults to markdown; `json` selects the machine-readable form. The
    /// host owns the dump + buffer open (the `OpenSyntheticBuffer` pattern).
    ExportPluginApi {
        format: Option<String>,
    },
    /// `:list-commands` (PI.3) -- enumerate every registered command grouped
    /// by source layer (built-in / user config / plugin / ...). The one
    /// introspection enumeration the help family was missing; a plugin group
    /// resolves the plugin id to its manifest name where the host knows it.
    ListCommands,
    /// `:describe-plugin <name>` (PI.4, Facet B) -- render one loaded plugin's
    /// own documentation + contributions. The doc comes from the plugin
    /// (embedded WIT world doc / manifest `doc`), resolved once at load. Unknown
    /// / no-plugin-loaded echoes an error. Loaded-plugin enumeration is
    /// Phase-8-gated; the surface + registry seam exist now.
    DescribePlugin {
        name: String,
    },
    /// `:list-plugins` (PI.4) -- list every loaded plugin (name + doc summary).
    /// Empty until the Phase-8 loader populates the registry.
    ListPlugins,
    /// `:hover [text]` -- open a hover popup at the cursor with
    /// `text` as the markdown body. v1 path: lets the user
    /// validate the popup positioning + dismissal; Phase 4 LSP
    /// will source `text` from a real `textDocument/hover` reply.
    OpenHover {
        markdown: String,
    },
    /// Dismiss the active popup, whatever its content. Content-agnostic
    /// (routes through `dismiss_popup`); produced today by `:HoverClose`.
    DismissPopup,
    /// Show a popup overlay at `placement` with the given `focus`
    /// (popup-api.md §4.3). Content-agnostic and data-only: the host
    /// idempotently ensures a popup buffer named `name` under major mode
    /// `mode_id` (`Editor::open_popup_named`) and the owning mode's
    /// `on_activate` projects the content. Name-based, not id-based, because
    /// the emitters (the `:ai-permission` ex-command, the async tick callback)
    /// have no host access to register a buffer and supply a `BufferId` — a
    /// name keeps the effect vocabulary the host boundary, like
    /// `OpenSyntheticBuffer`.
    OpenPopup {
        name: String,
        mode_id: String,
        placement: lattice_core::ui::popup::PopupPlacement,
        focus: lattice_core::ui::popup::PopupFocus,
    },
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
    /// CM.8: `:clist` / `:cl` — open the error list in a fuzzy
    /// picker (the flat browse-and-jump surface, parallel to
    /// `:diagnostics`). Complements `:cnext` (step) and `:copen` (the
    /// `*problems*` multibuffer). Host builds it from `Editor::error_list`.
    ListErrors,
    /// `]d` / `:diag-next` -- move the cursor to the
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
    /// `:ai-log [provider]` (AI-1b) -- open the per-session AI log
    /// buffer (`*ai:<provider>:<index>*`). With no known session,
    /// echoes an info hint; with exactly one, opens it directly; with
    /// more (optionally narrowed by the `session` provider
    /// prefilter), raises a picker. Peer-applied via the host's
    /// `do_open_ai_log`, exactly like [`Effect::OpenLspLog`]. The
    /// `lattice-ai` crate owns the `:ai-log` binding + this
    /// emission; the host owns only the generic
    /// `ensure_named_synthetic_document` + `AiLogMode` open.
    OpenAiLog {
        session: Option<String>,
    },
    /// Open (or focus) a named synthetic buffer under a given major mode --
    /// the generic primitive behind provider-owned buffer views (e.g. the
    /// `ai-conversation` `*ai:opencode*` buffer). The emitter (a mode's
    /// command handler) supplies the buffer name + the mode id; the host owns
    /// only the generic `ensure_named_synthetic_document` open, so no
    /// provider-specific host method is added. `mode_id` is the mode's string
    /// id (`ModeId::new(&mode_id)`); the mode must be registered at boot.
    OpenSyntheticBuffer {
        name: String,
        mode_id: String,
    },
    /// MG.50: [`Effect::OpenSyntheticBuffer`] + cursor placement, in one
    /// step.
    ///
    /// The synthetic peer of [`Effect::OpenBufferAt`], and it exists for
    /// exactly the same reason: the open is peer-applied while a cursor
    /// effect runs host-side against whatever buffer is active at that
    /// moment, so `Many([open, move])` cannot land the caret on a buffer
    /// that does not exist yet. Emitted by magit's `<CR>`, which opens a
    /// staged blob at the line the cursor was reading in the diff.
    OpenSyntheticBufferAt {
        name: String,
        mode_id: String,
        position: lattice_protocol::position::Position,
    },
    /// `:messages` -- open the `*messages*` buffer (the emacs
    /// `*Messages*` analogue). Renders a chronological view
    /// of every echo / minibuffer notification; live-tails as
    /// new entries arrive via the typed event bus.
    OpenMessages,
    /// `:dashboard` -- open (or re-compose + activate) the
    /// `*dashboard*` launch page. The applier reads config, composes
    /// the enabled sections via the crate-owned `DashboardRegistry`
    /// service, and seeds a read-only `BufferKind::Dashboard` buffer.
    /// See `docs/dev/architecture/dashboard.md` §9.
    OpenDashboard,
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
    /// EP.4 (2026-08-10): `:lsp-diagnostics-to-error-list` -- pull the
    /// current published diagnostics into the error list's `Lsp` slice
    /// on demand.
    ///
    /// The manual peer of the live feed gated by
    /// `lsp.diagnostics-to-error-list`. Useful when that option is off,
    /// and as a forced refresh after a server restart when it is on.
    /// Echoes the entry count, because this surfaces what servers have
    /// *published* -- not a workspace scan -- and an empty result must
    /// not be misread as a clean tree.
    LspDiagnosticsToErrorList,
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
    /// SN.3c.1: direct snippet expansion over a known trigger
    /// range. The mode-owned `<C-x><C-s>` handler
    /// (`snippet-mode`'s `action_handlers()`) does the word-prefix
    /// scan and emits this with `replace_range = token-start..cursor`;
    /// the **host** owns resolution + expansion (language detection,
    /// registry lookup, variable render, buffer splice + session
    /// install) via `Editor::expand_snippet_from_range`. A deliberate
    /// first-party effect: the typed `Effect` enum stays a host-owned
    /// vocabulary (`feedback_effect_vocabulary_is_host_boundary`) —
    /// the mode owns the *trigger*, the host owns the *expansion
    /// mechanics*. No-op (quiet info echo) when no snippet matches the
    /// prefix.
    ExpandSnippet {
        replace_range: lattice_protocol::position::Range,
    },
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
    /// `:describe-diff` -- render a help buffer listing every
    /// active diff session (D.2.d). Walks the host's
    /// `DiffSubsystem::describe_sessions` and formats each row
    /// as `BufferId | Algorithm | Rev | Hunks | Watches`.
    DescribeDiff,
    /// `:diff` (no args) -- open an inline diff session for
    /// the active document against its on-disk content.
    /// D.3.a.1.
    DiffOpen,
    /// `:diffoff[!]` -- close the active pane's diff session
    /// (if any). v1 two-way semantics collapse `:diffoff` and
    /// `:diffoff!` to the same teardown (removing one side of
    /// a two-way diff leaves the other degenerate, so both
    /// drop the whole session). The bang is a forward-compat
    /// surface: D.6 (three-way merge) will distinguish per-
    /// participant removal (`:diffoff`) from full-session
    /// teardown (`:diffoff!`). The handler reads the session's
    /// watch list as the source of truth for participants —
    /// the tab is not a grouping unit. D.3.a.1 / D.4.d.3.a.
    DiffOff {
        force: bool,
    },
    /// `:diffthis` -- stage the active pane for a two-pane
    /// diff; the second `:diffthis` invocation in a different
    /// pane completes the session (creates a `DiffSession`
    /// against the two live buffers, a `PaneGroup` with
    /// `HunkRowMapper`, and `FillerRowProvider`s on each
    /// side). Same pane twice unstages. Third-pane staging
    /// errors out — v1 is two-way only; multi-way arrives
    /// with D.6 three-way merge. D.4.d.3.a.
    Diffthis,
    /// `:diffsplit <file> [<remote>]` -- open `<file>` (and
    /// optionally `<remote>`) in new vertical splits and
    /// register a diff session between the current pane and
    /// the new pane(s).
    ///
    /// - **One arg** (`:diffsplit base`): two-way diff
    ///   between the current pane (current side) and a new
    ///   pane loading `base` (baseline side). D.4.d.3.b.
    /// - **Two args** (`:diffsplit base remote`): three-way
    ///   merge with the current pane as "local", a new pane
    ///   loading `base` as the common ancestor, and a third
    ///   new pane loading `remote` as the other side. D.6.c.
    ///
    /// Composes vsplit + `:edit <path>` in each new pane +
    /// the appropriate registration helper
    /// (`register_two_pane_diff` for one arg,
    /// `register_three_pane_diff` for two). Empty first arg
    /// errors at parse time. Cursor lands in the *first*
    /// new pane (vim parity).
    Diffsplit {
        path: std::path::PathBuf,
        remote: Option<std::path::PathBuf>,
    },
    /// `]c` / `:hunk-next` -- jump cursor to the start of the
    /// next diff hunk on the current side (`ranges[1]`).
    /// Wraps to top. D.3.c.
    /// `:diffget [<bufnr>]` -- pull the hunk under the cursor
    /// from the named (or auto-resolved) buffer side. D.6.d.
    /// `target` is the optional buffer number passed by the
    /// user; `None` means "the peer side" (two-way: unique;
    /// three-way: ambiguous — dispatch emits "target required").
    /// The chord-driven `do` operator stays unit-variant
    /// `Action::DiffGet`; this ex-command variant is a parallel
    /// entry point for explicit-target invocations.
    DiffGetCmd {
        target: Option<u32>,
    },
    /// `:diffput [<bufnr>]` -- push the hunk under the cursor
    /// into the named (or auto-resolved) buffer side. D.6.d.
    /// Mirror of [`Self::DiffGetCmd`] but for the put direction.
    DiffPutCmd {
        target: Option<u32>,
    },
    /// `:diff-accept` -- resolve the active pane's diff
    /// session with [`DiffOutcome::Accept`]. v1 semantics:
    /// equivalent to `:diffoff` + signal Accept on the
    /// session's completion channel (if any). The buffer's
    /// current content (whatever the user applied via
    /// `do`/`dp` or left alone) becomes the accepted
    /// resolution; plugins consuming the outcome commit
    /// from there. D.6.e.
    DiffAccept,
    /// `:diff-reject` -- resolve the active pane's diff
    /// session with [`DiffOutcome::Reject`]. v1 semantics:
    /// equivalent to `:diffoff!` + signal Reject. Plugins
    /// consuming the outcome should revert any
    /// pre-session state. D.6.e.
    DiffReject,
    /// `:diff-accept-all` — resolve EVERY pending review (each session with a
    /// bound completion) with [`DiffOutcome::Accept`]. The bulk counterpart to
    /// `:diff-accept` for when several agent reviews are open at once.
    DiffAcceptAll,
    /// `:diff-reject-all` — resolve EVERY pending review with
    /// [`DiffOutcome::Reject`]. Bulk counterpart to `:diff-reject`.
    DiffRejectAll,
    /// D-fix.6: an IDE-peer connection's `close_tab` — tear down (as a
    /// Reject) every *programmatic* diff session that THIS connection
    /// (`origin_session`) opened, regardless of how/where it is
    /// displayed. Host-applied: it fires each matching session's bound
    /// completion oneshot with [`DiffOutcome::Reject`] and closes its
    /// panes (the `:diff-reject` teardown, but targeted by
    /// `origin_session` rather than the active pane). If the connection
    /// opened no diff, the host falls back to closing the active buffer
    /// when `tab_name` matches its path (the legacy I3 file-close — the
    /// only remaining `tab_name` use, orthogonal to the diff teardown).
    CloseSessionDiffs {
        /// The originating connection id; only diffs tagged with it are
        /// torn down (`0` = none — a non-IDE producer's diff is never
        /// matched). Cross-session isolation: connection A's close can
        /// never affect connection B's diffs.
        origin_session: u64,
        /// The agent's close-tab label, used ONLY for the legacy
        /// active-buffer file-close fallback when no diff matched.
        tab_name: String,
    },
    /// D-fix.6: an IDE-peer connection's `closeAllDiffTabs` — tear down
    /// (as a Reject) every programmatic diff session `origin_session`
    /// opened. Same scoping as [`Self::CloseSessionDiffs`] but with no
    /// file-close fallback (it is unambiguously a diff-only bulk close).
    CloseAllSessionDiffs {
        /// The originating connection id; scopes the bulk teardown.
        origin_session: u64,
    },
    NextHunk,
    /// `[c` / `:hunk-prev` -- jump cursor to the start of the
    /// previous diff hunk on the current side. Wraps to
    /// bottom. D.3.c.
    PrevHunk,
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
    /// `:describe-active-modes` (`<C-h>m`) -- render the mode
    /// stack live on the *active* buffer: the major plus every
    /// minor, each with the chords it contributes.
    ///
    /// Distinct from [`Effect::DescribeMode`], which describes
    /// one *named* mode whether or not it is active, and from
    /// [`Effect::ListModes`], which lists every *registered*
    /// mode. This one answers "what is this buffer, and what
    /// can I press in it".
    ///
    /// Major-only would under-report by construction: the
    /// minor-mode convention deliberately pushes chords shared
    /// across majors out into a minor (magit's `gr` / `q` /
    /// `]]` live on `magit-core-mode`, not on each magit
    /// major), so the view is major + minors.
    ///
    /// An additive variant rather than widening
    /// `DescribeMode`'s `name` to `Option<String>` — the WIT
    /// declaration is `describe-mode(string)` and widening it
    /// would break a published plugin API.
    DescribeActiveModes,
    /// `:describe-bindings` (`<C-h>K`) -- the chords that can
    /// actually fire on the *active* buffer: builtin entries live in
    /// the current binding-mode, plus every active mode's
    /// contributions.
    ///
    /// Distinct from [`Effect::ListKeymap`] (`:keymap`), which
    /// renders the whole static catalog regardless of what is
    /// active. `:keymap` stays the exhaustive reference; this one
    /// answers "what can I press *here*".
    DescribeActiveBindings,
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

    /// M.10.3 (2026-06-03): record the editor's CURRENT cursor +
    /// active buffer onto the position-history ring as an
    /// `AutoJump` entry — vim's jump-list semantics for "big
    /// motions" (gg, G, /, *, mark jumps, ...). Used by
    /// mode-contributed jump actions (search `<CR>`,
    /// lsp-references `<CR>`, future project-diff `<CR>`) so
    /// `<C-o>` walks the user back to where they were before
    /// the jump. Must be the FIRST sub-effect inside an
    /// `Effect::Many` that also opens a new buffer — the host
    /// reads the cursor/active-buffer state at apply time, so
    /// after `OpenBuffer` lands the recorded entry would be the
    /// new doc's start, not the pre-jump location.
    RecordJump,

    /// Open a yes/no confirmation dialog. The host shows a transient
    /// picker with `prompt` as the title; `y` dispatches `yes_action`
    /// with `args`, `n` / `q` / Esc dismisses.
    ///
    /// **IX.1: `args` is what makes the confirmed thing and the executed
    /// thing the same thing.** Without it a yes-half has to re-derive
    /// its target when it fires, and the context it derives from is not
    /// stable across the wait — a background refresh can rebuild the
    /// buffer and move the cursor while the dialog is up, so the action
    /// lands on a different target than the prompt named. Carrying the
    /// target closes that window by construction.
    ///
    /// Carry the **payload, not a pointer to it**: a path, a SHA, a
    /// synthesized patch — not a cursor row or a row span, which a
    /// rebuild invalidates. For patch-shaped payloads this also makes
    /// `git apply`'s context check refuse a stale one loudly instead of
    /// applying it somewhere plausible.
    ///
    /// `Args::None` keeps the pre-IX.1 behaviour (the yes-half
    /// re-derives), so existing confirms are unaffected until migrated.
    ///
    /// **Why a name + `Args` rather than a `CommandInvocation`,** which
    /// is otherwise the canonical "thing to execute" (design §5.2.1):
    /// this effect has to cross the plugin seam, and `CommandInvocation`
    /// has no WIT mirror — it is exactly why [`Effect::Global`] fails at
    /// the boundary. `Args` is mirrored and a name is a string, so this
    /// payload crosses. A name is also the plugin-native form: plugins
    /// register actions by name and cannot hold a host `CommandId`.
    Confirm {
        prompt: String,
        yes_action: String,
        args: crate::Args,
    },

    /// Return the active pane to the buffer it was displaying before a
    /// full-pane synthetic buffer took it over, and drop that buffer's
    /// hold on the pane ("bury", in vim's sense — the buffer stays in
    /// the registry, it just stops being shown).
    ///
    /// Distinct from [`Effect::DismissPopup`] on purpose. A popup
    /// *floats over* the pane: the underlying document is never
    /// swapped out, so dismissing one only has to drop the overlay.
    /// A synthetic buffer opened full-pane (magit's views, oil, the
    /// plugin manager) genuinely replaced the pane's buffer AND the
    /// editor's active-document handle, so returning has to swap both
    /// back. magit's `q` used `DismissPopup` for this and left the
    /// active document pointing at magit while the pane pointed at the
    /// file — the pane said one thing and the screen painted another.
    ///
    /// A no-op when nothing was buried (no origin to return to), so a
    /// mode can bind it unconditionally.
    BuryBuffer,

    /// Open a named transient picker menu. `source` is a name
    /// registered into a `TransientSourceRegistry` (`lattice-picker`)
    /// by the owning mode crate at boot — mirrors `OpenPicker`'s
    /// named-source shape. The registry, not this enum, holds the
    /// actual `TransientSpec` builder, since `TransientSpec` lives in
    /// a crate downstream of `lattice-grammar`.
    OpenTransient {
        source: String,
    },

    /// Open a one-line minibuffer text prompt. `prompt` is shown as
    /// an info-level echo label; `initial` pre-seeds the input
    /// buffer's content; `on_submit_action` names a registered
    /// `action:*` handler fired on `<CR>` with the typed text
    /// available as `ActionContext::prompt_value` (never a closure —
    /// same name-based-lookup convention as `Confirm`'s `yes_action`
    /// and `OpenTransient`'s `source`, so the variant stays a plain,
    /// serializable value with no crate carrying a callback type
    /// downstream of `lattice-grammar`). `buffer_name`, when set,
    /// becomes the synthetic prompt buffer's name — callers use this
    /// to stash context for the submit handler to read back (mirrors
    /// how magit's blame/rebase/revision modes encode their target in
    /// the buffer name); `None` uses a default unnamed prompt buffer.
    /// `<Esc>` cancels without firing anything.
    OpenPrompt {
        prompt: String,
        initial: String,
        on_submit_action: String,
        buffer_name: Option<String>,
    },

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
            explicit_yank: true,
        };
        match e {
            Effect::Yank {
                register,
                content,
                kind,
                explicit_yank,
            } => {
                assert_eq!(register, Register::Unnamed);
                assert_eq!(content, "hello");
                assert_eq!(kind, YankKind::Charwise);
                assert!(explicit_yank);
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
