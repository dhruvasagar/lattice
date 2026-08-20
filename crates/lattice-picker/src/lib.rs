//! Vertico-style picker (DESIGN.md §5.9.7, §5.9.10).
//!
//! Generalises the completion popup's three-stage shape (raw
//! candidates -> filter -> render) into a reusable host for any
//! "type to drill down, Enter to act" UI: buffer switcher, LSP
//! instance picker, future fuzzy-finder, command palette,
//! diagnostics list, register / mark history.
//!
//! ## Architecture
//!
//! - [`Picker`] owns the live state: a query buffer, a cursor on
//!   that query, the unfiltered raw candidate list, the filtered
//!   rendered list, and a selection cursor on the rendered list.
//! - [`PickerSource`] tags how the raw candidate list was built so
//!   refresh paths know which generator to re-run.
//! - [`PickerAction`] tags what to do when the user accepts a row.
//!   The dispatch happens on the host side; Picker is dumb about
//!   side effects.
//!
//! Filtering today is **case-insensitive substring** (cheap, easy
//! to reason about). The pipeline-driven path (`lattice-completion`
//! crate's full vertico stack: matcher / ranker / annotators) takes
//! over once we lift `CommandLineSlot` out of the slot detector --
//! same data shape, richer scoring. Substring is enough to ship a
//! useful buffer switcher and stays cheap when the candidate set is
//! small (typical: <50 buffers, <10 LSP instances).
//!
//! ## Renderer-agnostic by design
//!
//! This crate is the **data model** for pickers; it owns no
//! rendering code and no host-specific imports beyond
//! `lattice-completion`'s candidate shape. Hosts (the TUI
//! renderer today; the GPUI / web renderers later) read picker
//! state and paint it however they like.
//!
//! The buffer-source candidate builder lives in the TUI host
//! (`lattice-ui-tui::app::raw_buffer_candidates`) because it
//! walks the host's `BufferRegistry`. LSP-instance candidates
//! arrive via [`Picker::set_lsp_instances`] which takes a
//! `Vec<LspInstanceRow>` of pure-data rows the host snapshots
//! from the supervisor. Both paths feed [`Picker::set_raw_candidates`]
//! (the only entry point that mutates `raw`).
//!
//! See `docs/dev/architecture/picker.md` for the trait-surface
//! design that the registry, source generators, and MRU
//! pipeline will land on top of this data model.

pub mod context;
pub mod events;
pub mod mru;
pub mod outcome;
pub mod picker_sources;
pub mod source;
pub mod transient;

pub use context::{
    ActiveBufferSnapshot, BufferEntry, PaneHistoryRow, PickerContext, PositionEntry, PositionSource,
};
pub use mru::{
    DEFAULT_CAP_PER_NAMESPACE, DEFAULT_HALF_LIFE, MruEntry, MruKey, MruPersistError,
    PickerMruIndex, bonus_of, default_persist_path, routing_identity,
};
pub use outcome::{FillTarget, OpenTarget, PickerAcceptOutcome, PickerPreviewOutcome};
pub use picker_sources::{FILE_PICK_SOURCE, YANK_RING_SOURCE};
pub use source::{
    AcceptFuture, CandidateBatch, CandidateFuture, CandidateStream, PickerInitResult,
    PickerRegistry, PickerRegistryHandle, PickerSourceGenerator, PickerSourceSpec, SourceResult,
};
pub use transient::{
    KeyResolution, TransientArgSource, TransientContext, TransientGroup, TransientItem,
    TransientItemKind, TransientSourceRegistry, TransientSourceRegistryHandle, TransientSpec,
    TransientState, TransientValue, confirm_transient_spec, transient_initial_state,
};

use std::path::PathBuf;

use lattice_completion::{
    CandidateData, CompletionPipeline, FuzzyDisplayMatcher, MatchScore, MruRanker, RawCandidate,
    RenderedCandidate,
};

/// `CandidateData::Extension { kind_id }` value the picker stamps
/// on every candidate it builds. Each candidate's payload bytes
/// are a `u32` LE index into the picker's `routing_meta` sidecar
/// vec; the sidecar holds the typed [`RoutingPayload`] enum the
/// accept dispatch matches on.
///
/// Same shape used by the insert-completion crate's snippet +
/// LSP-completion sidecars (see `app::SNIPPET_COMPLETION_KIND_ID`
/// / `LSP_COMPLETION_KIND_ID`); a distinct id keeps picker
/// candidates from colliding with insert-completion ones if the
/// surfaces ever share a list (they don't today, but the type
/// system stays honest).
pub const PICKER_ROUTING_KIND_ID: u32 = 200;

/// Typed payload the picker accept dispatch reads to figure out
/// which side effect to run. Replaces the tab-encoded string
/// payloads stuffed into `RawCandidate.text` in earlier phases
/// (Phase 4.2.g.7 polish).
///
/// One variant per [`PickerAction`]; the variant naturally
/// communicates the dispatch path AND carries the typed data the
/// handler needs (no string parsing, no fragility around `\t` in
/// paths). The `PickerAction` tag stays for now -- the App's
/// dispatch matches on it first to choose the code path; future
/// cleanup could pivot to matching on the payload variant alone
/// since the two always agree, but the redundancy is harmless.
#[derive(Debug, Clone)]
pub enum RoutingPayload {
    /// `PickerAction::SwitchToBuffer` -- the host's buffer id
    /// (newtype-wrapped `u32` host-side; we hold the raw value
    /// to keep the picker module renderer-agnostic).
    Buffer { id: u32 },
    /// PBH.5: one entry in the ACTIVE pane's buffer trail, identified by
    /// its **index** rather than its buffer id — the same buffer can
    /// appear at several points in a trail, and picking the third stop
    /// must land on the third stop.
    ///
    /// Accepting moves the walk cursor rather than pushing a visit: the
    /// picker is random access over the existing trail, not a new
    /// navigation. Pushing would append a duplicate and make `<C-7>`
    /// unreachable, exactly as an unsuppressed walk would.
    PaneHistoryEntry { index: u32 },
    /// Resolve a specific pending diff review by its primary buffer id with
    /// Accept (`accept = true`) or Reject. Emitted by the diff-review picker
    /// (`:diff-accept` / `:diff-reject` with >1 pending review) so the user
    /// chooses WHICH diff to resolve. `primary` is the raw host `BufferId`.
    ResolveDiff { primary: u32, accept: bool },
    /// `PickerAction::OpenLspLog` / `OpenLspTraceLog` -- the
    /// supervisor key. `workspace` rides for completeness but
    /// today's handlers only use `server_id`.
    LspInstance {
        server_id: String,
        workspace: PathBuf,
    },
    /// `PickerAction::OpenAiLog` -- the AI session key (provider +
    /// per-provider index) the host reconstructs into a
    /// `SessionKey` to open `*ai:<provider>:<index>*`. Ephemeral
    /// (sessions come and go), so `routing_identity` returns
    /// `None` — no MRU recency.
    AiSession { provider: String, index: u32 },
    /// `PickerAction::JumpToLspLocation` -- canonical `(path,
    /// line, col)` the host's `jump_to_file_line_col` consumes.
    /// LSP 0-based line + utf-8 byte column; the host already
    /// converted these utf-8 host-side at ingestion time.
    LspLocation { path: PathBuf, line: u32, col: u32 },
    /// `PickerAction::AcceptLspCompletion` -- numeric index into
    /// the host's `pending_completion_items` snapshot.
    LspCompletion { index: u32 },
    /// `PickerAction::AcceptLspCodeAction` -- numeric index into
    /// the host's `pending_code_action_items` snapshot.
    LspCodeAction { index: u32 },
    /// `PickerAction::OpenFile` -- canonical filesystem path the
    /// accept dispatch hands to `App::do_edit(Some(path),
    /// false)`. Used by the file picker (`:files`) and the
    /// recent-files picker; directories defer to oil/file-tree
    /// the same way `:e DIR` does.
    OpenFile { path: PathBuf },
    /// `PickerAction::JumpInBuffer` -- jump to `(line, col)` in
    /// an already-registered buffer. Captured at picker-open
    /// time so the destination is stable even if the user
    /// arrowed through another picker's hover-preview.
    /// Emitted by `:picker lines`, `:picker jumps` (for entries
    /// pointing at currently-open buffers), and -- once
    /// migrated -- `:picker marks`. MRU returns `None` for this
    /// variant because `(line, col)` drift as the buffer is
    /// edited; a stale identity would mis-rank candidates.
    JumpInBuffer { buffer_id: u32, line: u32, col: u32 },
    /// Invoke an ex-command by stable id. Emitted by the
    /// command palette (`:picker commands`); the host resolves
    /// `id` against its `CommandRegistry`, builds a
    /// `CommandInvocation`, and routes through the same
    /// dispatcher the `:` line uses. `args` carries any
    /// pre-supplied positional arguments (the command palette
    /// today emits `Args::None`; future "pick a thing, then
    /// run a command on it" flows will carry richer values).
    InvokeCommand {
        id: String,
        args: lattice_grammar::args::Args,
    },
    /// Paste a named register's contents at the cursor.
    /// Emitted by `:picker registers`; `name` is the single-
    /// char register identifier (`a`..`z`, `0`..`9`, `+`,
    /// `"`, etc.). The host's `apply_picker_outcome::PasteRegister`
    /// arm sets the pending register and runs the normal paste
    /// path so charwise / linewise / blockwise distinction is
    /// honored.
    PasteRegister { name: char },
    /// Jump to a named mark (`mX`). Emitted by `:picker marks`.
    /// The host resolves the mark to a position via its
    /// existing `do_jump_mark` path so the cursor placement +
    /// position-history push match the keyboard-driven
    /// behavior. `name` carries a stable identity, so MRU
    /// will record `mark:<name>` once slice 14 lands.
    JumpToMark { name: char },
    /// Expand a snippet by name at the cursor. Emitted by
    /// `:picker snippets`. The host resolves the body through
    /// `SnippetRegistry::by_name` and routes through the
    /// existing `:snippet-expand` path. MRU keys on
    /// `snip:<name>`.
    ExpandSnippet { id: String },
    /// Accept one action from a server-initiated
    /// `window/showMessageRequest`. `request_id` keys into the
    /// App's `lsp_pending_show_message_requests` map (the slot
    /// that holds the inbound oneshot); `action_index` selects
    /// which `MessageActionItem` from the request's actions
    /// vec ferries back to the server. Dismiss (Esc) routes
    /// through `do_picker_dismiss` which replies `null` (i.e.
    /// the user closed the prompt without picking).
    AcceptShowMessageAction { request_id: u32, action_index: u32 },
    /// 4.5.d -- `PickerAction::AcceptLspCodeLens`. Numeric
    /// index into the host's `pending_code_lens_items`
    /// snapshot (a clone of the active buffer's code-lens
    /// cache at picker-open time). The accept dispatch
    /// resolves the lens (if it arrived without a `command`)
    /// and routes the resulting `command` through
    /// `workspace/executeCommand` on the originating server.
    LspCodeLens { index: u32 },
    /// 4.5.e -- `PickerAction::AcceptColorPresentation`.
    /// Numeric index into the host's
    /// `pending_color_presentations` snapshot. Accept splices
    /// the chosen `ColorPresentation.text_edit` (or `label`
    /// fallback) into the buffer at the cached color range.
    ColorPresentation { index: u32 },
    /// T.12: the theme name a colorscheme-picker candidate carries.
    /// Both accept and live-preview resolve the name against the
    /// `ThemeRegistry` catalog and swap the active theme.
    Colorscheme { name: String },
    /// MB.3: the past command a history-picker candidate carries.
    /// Accept loads `text` into the editable `:` line via the
    /// host's `open_command_line` seam — it does **not** execute
    /// (the user tweaks / `<C-x><C-e>`s it, then `<CR>`s). Emitted
    /// by the `history` picker source (`q:` / `:history`).
    /// Ephemeral by nature — `routing_identity` returns `None`, so
    /// no MRU recency (the history ring is already recency-ordered).
    LoadCommandLine { text: String },
    /// MB.5: load `text` into the editable `/` search line. Emitted
    /// by the `search-history` picker source (`q/` / `q?` /
    /// `:history search`). The host opens the `*search-line*` buffer
    /// with Forward direction and seeds the pattern; the user tweaks
    /// it, then `<CR>` to execute. Ephemeral — no MRU recency.
    LoadSearchLine { text: String },
    /// The branch name a base-branch-picker candidate carries.
    /// Emitted by magit's branch-create wizard (`c` in
    /// `magit-branch-mode`): the user picks an existing branch as the
    /// base, then accept opens a follow-up text prompt for the new
    /// branch's name (`PickerAcceptOutcome::OpenPrompt`, stashing
    /// this name in the prompt buffer's synthetic name).
    BranchBase { name: String },
    /// MG.53.e: a plain value the picked candidate stands for, for a
    /// source that answers a question rather than performing an action.
    ///
    /// Carried by `file-pick` and consumed by whatever parked itself
    /// waiting for a value — today a picker-backed transient argument.
    /// The source does not know what the value is for, which is the
    /// point: one registered file listing serves every argument that
    /// names a file.
    SuppliedValue { value: String },
}

/// Where a picker pulls its raw candidates from. The App resolves
/// this on `populate` / `refresh` and walks the appropriate source.
/// One enum variant per first-party source so the App stays
/// decoupled from generator implementations; plugin-provided
/// pickers will arrive as a separate `Plugin(GeneratorId)`
/// variant once the WASM host is online.
#[derive(Debug, Clone)]
pub enum PickerSource {
    /// Walk every entry in `BufferRegistry` -- the buffer
    /// switcher (`:b` with no arg, future `<C-x>b`).
    Buffers,
    /// Walk the LSP supervisor's running actor table, one
    /// candidate per `(workspace_root, server_id)` pair, with
    /// workspace path + buffer count + capability summary as
    /// marginalia. Used by `:lsp-log` / `:lsp-server-log` /
    /// `:lsp-trace-log`. The `prefilter` carries an optional
    /// `server_id` so `:lsp-log rust` shows only rust-* rows;
    /// the picker still appears so the user can disambiguate
    /// when multiple workspaces have a rust server.
    LspInstances { prefilter: Option<String> },
    /// Static location list -- multi-result LSP navigation
    /// (`gd` / `gD` / `gy` / `gI`), `gr` references, and the
    /// `:diagnostics` workspace list. Each row encodes one
    /// `file:line:col` target; the typed
    /// [`RoutingPayload::LspLocation`] carries `(path, line,
    /// col)` to the accept dispatch. Display is
    /// `<rel-path>:<line>:<col>  <line preview>` so the user
    /// sees a ripgrep-style row.
    LspLocations,
    /// Workspace file walk -- `:files` and `:recent`. Each row
    /// is one filesystem path; accept hands it to
    /// `App::do_edit`. Caller seeds the candidate list (the
    /// picker stays renderer-agnostic).
    Files,
    /// Server-initiated `window/showMessageRequest`. Each row
    /// is one `MessageActionItem` title from the request's
    /// `actions` vec. `request_id` keys into the App's pending-
    /// SMR map so the accept arm (and the dismiss path) can
    /// locate the inbound oneshot and reply. `server_id` rides
    /// for display + log breadcrumbs.
    LspShowMessageRequest { request_id: u32, server_id: String },
    /// Walk the AI subsystem's known agent sessions, one
    /// candidate per `SessionKey` (provider + per-provider
    /// index). Used by `:ai-log`. Mirrors [`Self::LspInstances`]
    /// one level simpler — the `prefilter` carries an optional
    /// provider name so `:ai-log opencode` shows only opencode
    /// rows; the picker still appears so the user can
    /// disambiguate when multiple indices exist for a provider.
    AiSessions { prefilter: Option<String> },
}

/// What `<CR>` does to the selected candidate. Variants stay
/// dumb data; the App's `App::accept_picker`
/// dispatcher pattern-matches and calls the right method.
#[derive(Debug, Clone, Copy)]
pub enum PickerAction {
    /// Selected candidate's `text` is `"#<id>"`; activate that
    /// buffer in the current pane.
    SwitchToBuffer,
    /// Selected candidate's `text` is `"<server_id>\t<workspace>"`;
    /// open `*lsp:<server_id>*` (the per-server log) in the
    /// current pane via `App::open_help_in_pane`.
    OpenLspLog,
    /// Same encoding as `OpenLspLog`; opens
    /// `*lsp:<server_id>:trace*` -- the trace ring view --
    /// without flipping the trace toggle. Pair with `:lsp-trace
    /// <server>` to actually start tracing.
    OpenLspTraceLog,
    /// Selected candidate carries [`RoutingPayload::AiSession`];
    /// open `*ai:<provider>:<index>*` (the per-session AI log) in
    /// the current pane via the host's
    /// `ensure_named_synthetic_document` + `AiLogMode`. Emitted by
    /// the `:ai-log` picker.
    OpenAiLog,
    /// Selected candidate's `text` is
    /// `"<path>\t<line>\t<col>"` (LSP 0-based line + utf-8
    /// byte column); jump to that location via the same
    /// `jump_to_file_line` path the help-link click uses.
    /// Used by multi-result `gd` / `gD` / `gy` / `gI`, by
    /// `gr` references, and by `:diagnostics`.
    JumpToLspLocation,
    /// Selected candidate's `text` is `"#<idx>"` -- a numeric
    /// index into the host's `pending_completion_items`
    /// snapshot. The accept handler reads the item by index
    /// and splices it into the buffer at the captured replace
    /// range. Used by `:complete` (Phase 4.2.g).
    AcceptLspCompletion,
    /// Selected candidate's `text` is `"#<idx>"` -- a numeric
    /// index into the host's `pending_code_action_items`
    /// snapshot. Accept resolves the action (when needed)
    /// then applies its WorkspaceEdit / executeCommand.
    /// Used by `:code-actions` (Phase 4.3).
    AcceptLspCodeAction,
    /// Accept the focused row's
    /// [`RoutingPayload::OpenFile`] -- pass the path to
    /// `App::do_edit(Some(path), false)`. File picker
    /// (`:files`) and recent-files picker share this action.
    OpenFile,
    /// Accept one `MessageActionItem` from a server-initiated
    /// `window/showMessageRequest`. Routing payload is
    /// [`RoutingPayload::AcceptShowMessageAction`]; dismiss
    /// (Esc) replies `null` via the SMR-specific arm in
    /// `do_picker_dismiss`.
    AcceptShowMessageAction,
    /// 4.5.d: accept one code lens from the
    /// `:lsp-code-lens` picker. Routing payload is
    /// [`RoutingPayload::LspCodeLens`]; the host's accept
    /// dispatch resolves the lens (when needed) and routes
    /// its `command` through `workspace/executeCommand` on
    /// the originating server.
    AcceptLspCodeLens,
    /// 4.5.e: accept one color presentation from the
    /// `:lsp-color-presentation` picker. Routing payload is
    /// [`RoutingPayload::ColorPresentation`]; the host
    /// splices the chosen alternative into the buffer.
    AcceptColorPresentation,
}

/// One open vertico-style picker. Lives on `App.picker` while
/// active; the input and render layers route to / from it via
/// the `Action::Picker*` family.
#[derive(Debug, Clone)]
pub struct Picker {
    pub title: String,
    pub query: String,
    /// Byte offset within `query` where the cursor sits. Today
    /// the picker only appends / backspaces at end-of-query so
    /// this equals `query.len()`; reserved for future left/right
    /// editing.
    pub query_cursor: usize,
    /// Candidates that pass the current query filter. Re-built
    /// on every `refilter` call.
    pub candidates: Vec<RenderedCandidate>,
    /// Index into `candidates`. Clamped to `0..candidates.len()`
    /// on every refilter.
    pub selected: usize,
    pub source: PickerSource,
    pub on_accept: PickerAction,
    /// Unfiltered candidate list snapshot. `refilter` walks this
    /// against `query`; the host rebuilds it via
    /// [`Self::set_raw_candidates`] (or
    /// [`Self::set_lsp_instances`] for the LSP shape).
    raw: Vec<RawCandidate>,
    /// Typed routing payloads keyed by the candidate's
    /// `Extension { kind_id, payload }` u32 LE index (Phase
    /// 4.2.g.7 polish). Indexed lookup at accept time replaces
    /// the prior tab-encoded string-parsing dispatch. Built
    /// alongside `raw` by [`Self::set_raw_candidates_with_routing`];
    /// the `text`-only constructor leaves it empty (legacy
    /// pickers without typed routing fall back to the
    /// `Plain`-data path -- not used today).
    routing_meta: Vec<RoutingPayload>,
    /// Picker-registry source id that seated this picker. `Some`
    /// when the picker was seated via the trait-driven path
    /// (`:picker <source>`); `None` for legacy imperative
    /// pickers (`:b`, `:lsp-log`, multi-result LSP locations).
    /// `do_picker_accept` reads this to decide whether to
    /// delegate accept to the source's `PickerSourceGenerator`
    /// or fall back to the legacy per-routing dispatch.
    pub source_id: Option<String>,
    /// Frecency bonus per candidate, parallel to
    /// `routing_meta`. Snapshotted host-side at picker-open by
    /// looking each candidate's `routing_identity` up in the
    /// `PickerMruIndex`; refilter combines `match_score + bonus`
    /// when ranking. Empty (slice 12 / non-trait pickers) means
    /// "no MRU contribution"; the combine path treats it as 0.0
    /// for every candidate.
    mru_bonuses: Vec<f64>,
    /// True when the seating source's `spec().live` is true
    /// (`:picker grep` today). Live sources own their own
    /// filtering -- the external program (grep, future LSP
    /// workspace-symbols) IS the filter -- so [`Self::refilter`]
    /// bypasses fuzzy matching and renders `raw` 1:1 in
    /// insertion order. The host sets this via
    /// [`Self::set_live_source_mode`] right after opening the
    /// picker, before the first batch lands.
    live_source_mode: bool,
    /// True while an async fetch for this picker is in flight
    /// (the initial grep on `:picker grep <pat>`, or a live
    /// re-query after a keystroke). Both renderers surface it as
    /// a `searching…` indicator in the prompt so a slow grep
    /// reads as "working", not "nothing happened". The host sets
    /// it when it spawns a fetch future and clears it when
    /// results seat (a fresh `Picker` defaults to `false`) or the
    /// fetch errors. See `seat_picker_from_pairs` /
    /// `open_picker` / `fire_live_picker_query_changed`.
    pub loading: bool,
    /// Active transient-mode specification + live state. When
    /// `Some`, the renderer switches to grouped section layout
    /// with single-key chord dispatch, and the input layer routes
    /// keystrokes through transient chord matching rather than
    /// the query → filter path. See `transient.rs`.
    pub transient: Option<std::sync::Arc<TransientSpec>>,
    pub transient_state: TransientState,
    /// Stack of parent transient specs for `BS`/`DEL` back
    /// navigation through nested submenus. The `usize` is the
    /// parent's `transient_selected` at the moment its submenu opened —
    /// popping restores it instead of leaving the parent's selection
    /// wherever the submenu happened to leave the shared field.
    pub transient_stack: Vec<(std::sync::Arc<TransientSpec>, TransientState, usize)>,
    /// Which of the transient's items `<C-n>` / `<C-p>` have walked to
    /// — an index over [`TransientSpec::selectable_count`], NOT a
    /// scroll offset.
    ///
    /// The distinction is the whole fix: the host can bound an item
    /// index from the spec alone, whereas a scroll offset's true
    /// maximum depends on a viewport height only the renderer knows.
    /// The offset version grew unbounded and each renderer clamped it
    /// privately at paint time, so the stored value drifted tens of
    /// rows past anything renderable and `<C-p>` did nothing visible
    /// until the overshoot had been walked back off. Renderers now
    /// derive their scroll from this every frame
    /// ([`TransientSpec::scroll_for`]), leaving no scroll state to
    /// drift.
    pub transient_selected: usize,
    /// Keys typed at this level that begin some row's key but do not
    /// complete one yet — magit's `, k` / `, r` / `= f` rows.
    ///
    /// Empty whenever no multi-key row is part-way typed, which is
    /// almost always. The host used to compare a single typed `char`
    /// against each row's key string, so a multi-key row rendered,
    /// could be walked to with `<C-n>` and fired with `<CR>`, and did
    /// nothing at all when its own keys were pressed.
    pub transient_prefix: String,
    /// MG.54: when this picker's DEFERRED preview is due.
    ///
    /// Set only for a source that declares
    /// [`PickerSourceGenerator::preview_debounce`](source::PickerSourceGenerator::preview_debounce)
    /// — a source whose preview costs real work, which the settle window
    /// lets the host skip entirely while the selection is moving.
    ///
    /// **It lives on the picker, not on the host.** A deadline is only
    /// ever meaningful for the selection that armed it, so tying its
    /// lifetime to the picker's makes "a settle fired into the picker
    /// that replaced mine" unrepresentable rather than something two
    /// host-side clear sites have to remember. Dismiss drops the picker
    /// and the deadline goes with it.
    preview_settle_until: Option<std::time::Instant>,
}

impl Picker {
    pub fn new(title: impl Into<String>, source: PickerSource, on_accept: PickerAction) -> Self {
        Self {
            title: title.into(),
            query: String::new(),
            query_cursor: 0,
            candidates: Vec::new(),
            selected: 0,
            source,
            on_accept,
            raw: Vec::new(),
            routing_meta: Vec::new(),
            source_id: None,
            mru_bonuses: Vec::new(),
            loading: false,
            live_source_mode: false,
            transient: None,
            transient_state: TransientState::new(),
            transient_stack: Vec::new(),
            transient_selected: 0,
            transient_prefix: String::new(),
            preview_settle_until: None,
        }
    }

    /// MG.54: (re)start this picker's preview settle window.
    ///
    /// Called on every selection move for a source that declares a
    /// window. Each call pushes the deadline out, so a burst of moves
    /// leaves exactly one due time — the moment the user stopped.
    ///
    /// The host still schedules the wake (it owns the runtime), but the
    /// policy and the state are the picker's, so every picker — first
    /// party, plugin, one written next year — gets the same behaviour
    /// from declaring the window alone.
    pub fn arm_preview_settle(&mut self, delay: std::time::Duration) {
        self.preview_settle_until = Some(std::time::Instant::now() + delay);
    }

    /// MG.54: has the settle window elapsed? Consumes the deadline when
    /// it has, so the preview runs once per settle no matter how many
    /// wakes arrive — a burst of N moves schedules N wakes, and the N-1
    /// superseded ones find the deadline still in the future.
    pub fn take_due_preview_settle(&mut self, now: std::time::Instant) -> bool {
        match self.preview_settle_until {
            Some(deadline) if now >= deadline => {
                self.preview_settle_until = None;
                true
            }
            _ => false,
        }
    }

    /// MG.54: whether a deferred preview is waiting on this picker.
    /// Read by tests and by the host's "is anything pending" checks; the
    /// deadline itself is deliberately not exposed.
    pub fn preview_settle_pending(&self) -> bool {
        self.preview_settle_until.is_some()
    }

    /// Unwind one level of transient state: a half-typed multi-key row
    /// first, then one submenu off the stack.
    ///
    /// Returns `false` when there was nothing to unwind — the caller
    /// (`<Esc>` / `BS`) then means "close", because this is the root
    /// menu with no pending keys.
    ///
    /// **The precedence is the point.** A part-typed row (`,` waiting
    /// for `k`) is what the key most likely means to undo, so it goes
    /// first; only once nothing is pending does the same key leave the
    /// menu you are in. Vim gives `<Esc>` the same precedence over a
    /// partial chord.
    ///
    /// Lives here rather than in the host's dispatch arm because `BS`
    /// and `<Esc>` both need it, and two copies of a precedence rule
    /// drift.
    /// LR.5: the routing payloads of the candidates that survived the
    /// current query — the FILTERED set, which is what `<C-q>` means.
    /// Sending the unfiltered list would discard the work the user just
    /// did typing a query (telescope's `send_to_qflist` semantics).
    pub fn filtered_routing(&self) -> Vec<&RoutingPayload> {
        self.candidates
            .iter()
            .filter_map(|c| self.routing_for(c))
            .collect()
    }

    pub fn transient_unwind(&mut self) -> bool {
        if self.transient.is_none() {
            return false;
        }
        if !self.transient_prefix.is_empty() {
            self.transient_prefix.clear();
            return true;
        }
        match self.transient_stack.pop() {
            Some((parent_spec, parent_state, parent_selected)) => {
                self.transient = Some(parent_spec);
                self.transient_state = parent_state;
                self.transient_selected = parent_selected;
                true
            }
            None => false,
        }
    }

    /// Walk the transient's selection one item forward, wrapping at the
    /// end — the same wrap [`Self::select_next`] gives the candidate
    /// list, and the reason `<C-n>` can no longer overshoot: the index
    /// is taken modulo the item count, so there is no out-of-range
    /// value to represent.
    ///
    /// No-op when no transient is open or it has no items.
    pub fn transient_select_next(&mut self) {
        if let Some(count) = self.transient_item_count() {
            self.transient_selected = (self.transient_selected + 1) % count;
        }
    }

    /// The peer of [`Self::transient_select_next`], wrapping the other
    /// way.
    pub fn transient_select_prev(&mut self) {
        if let Some(count) = self.transient_item_count() {
            self.transient_selected = if self.transient_selected == 0 {
                count - 1
            } else {
                self.transient_selected - 1
            };
        }
    }

    /// The open transient's item count, or `None` when there is no
    /// transient or it is empty — the guard both walkers share so
    /// neither can divide by zero or index an empty menu.
    fn transient_item_count(&self) -> Option<usize> {
        self.transient
            .as_ref()
            .map(|spec| spec.selectable_count())
            .filter(|c| *c > 0)
    }

    /// The item `<CR>` would fire — the selection resolved against the
    /// open transient. `None` when there is no transient, it is empty,
    /// or the selection is somehow past its end.
    pub fn transient_selected_item(&self) -> Option<&TransientItem> {
        self.transient.as_ref()?.item_at(self.transient_selected)
    }

    /// Toggle live-source mode (`spec().live == true`). When
    /// on, [`Self::refilter`] renders `raw` verbatim instead
    /// of running fuzzy matching. The host calls this once
    /// at picker-open time after consulting the source's
    /// spec; the flag stays on for the picker's lifetime.
    pub fn set_live_source_mode(&mut self, live: bool) {
        self.live_source_mode = live;
    }

    /// Query accessor for the host (and tests) -- mirrors the
    /// other state predicates.
    pub fn is_live_source_mode(&self) -> bool {
        self.live_source_mode
    }

    /// Stamp the parallel `mru_bonuses` vec the matcher reads
    /// during refilter. Must be called after
    /// [`Self::set_raw_candidates_with_routing`] and must match
    /// `routing_meta.len()` in length; mismatched lengths reset
    /// to zero-bonus so the picker stays in a sane state if a
    /// caller miscounts (mostly a guardrail for tests).
    ///
    /// Callers that already have both pairs + bonuses in hand
    /// should prefer
    /// [`Self::set_raw_candidates_with_routing_and_bonuses`] --
    /// it sets all three vecs and refilters once, vs. this
    /// path which leaves a wasted refilter behind from the
    /// preceding `set_raw_candidates_with_routing` call.
    pub fn set_mru_bonuses(&mut self, bonuses: Vec<f64>) {
        if bonuses.len() != self.routing_meta.len() {
            self.mru_bonuses = vec![0.0; self.routing_meta.len()];
        } else {
            self.mru_bonuses = bonuses;
        }
        self.refilter();
    }

    /// Single-pass seat: mutate `raw` + `routing_meta` +
    /// `mru_bonuses` and refilter exactly once. The
    /// fast-path replacement for the
    /// `set_raw_candidates_with_routing` + `set_mru_bonuses`
    /// pair the host's trait-driven seat path used before
    /// this method existed -- which refiltered twice, with
    /// the first pass entirely wasted because the bonuses
    /// were about to replace the same data.
    ///
    /// Mismatched-length bonuses zero out (same guardrail as
    /// [`Self::set_mru_bonuses`]). Legacy callers that don't
    /// have bonuses yet keep using `set_raw_candidates_with_routing`
    /// + `set_mru_bonuses`; the new method is opt-in.
    pub fn set_raw_candidates_with_routing_and_bonuses(
        &mut self,
        items: Vec<(RawCandidate, RoutingPayload)>,
        bonuses: Vec<f64>,
    ) {
        let mut raw: Vec<RawCandidate> = Vec::with_capacity(items.len());
        let mut routing: Vec<RoutingPayload> = Vec::with_capacity(items.len());
        for (mut cand, payload) in items {
            let idx = routing.len() as u32;
            cand.data = CandidateData::Extension {
                kind_id: PICKER_ROUTING_KIND_ID,
                payload: idx.to_le_bytes().to_vec(),
            };
            raw.push(cand);
            routing.push(payload);
        }
        let bonuses = if bonuses.len() == raw.len() {
            bonuses
        } else {
            vec![0.0; raw.len()]
        };
        self.raw = raw;
        self.routing_meta = routing;
        self.mru_bonuses = bonuses;
        self.refilter();
    }

    /// Borrow the candidate's MRU bonus by its routing-payload
    /// index. Returns 0.0 for candidates without a registered
    /// bonus (slice 12 pickers, legacy LSP pickers, anything
    /// pre-MRU-snapshot). Public so the refilter path can read
    /// it; not intended for downstream callers.
    pub fn mru_bonus_for(&self, candidate: &RenderedCandidate) -> f64 {
        let CandidateData::Extension { kind_id, payload } = &candidate.raw.data else {
            return 0.0;
        };
        if *kind_id != PICKER_ROUTING_KIND_ID {
            return 0.0;
        }
        if payload.len() != 4 {
            return 0.0;
        }
        let idx = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
        self.mru_bonuses.get(idx).copied().unwrap_or(0.0)
    }

    /// Replace the raw candidate list. Host-built (e.g. the TUI
    /// host walks `BufferRegistry` for the buffer switcher);
    /// picker just stores + refilters. The single mutation entry
    /// point: every other "set the candidates" helper (e.g.
    /// [`Self::set_lsp_instances`]) routes through this.
    pub fn set_raw_candidates(&mut self, raw: Vec<RawCandidate>) {
        self.raw = raw;
        self.routing_meta.clear();
        self.refilter();
    }

    /// Replace the raw candidate list AND the typed routing
    /// sidecar (Phase 4.2.g.7 polish). Each input pair is a
    /// `(RawCandidate, RoutingPayload)` -- the picker stores
    /// the payload at index `i` in `routing_meta` and stamps
    /// the candidate's `data` with `Extension { kind_id:
    /// PICKER_ROUTING_KIND_ID, payload: i.to_le_bytes() }`. The
    /// accept dispatch reads the index back, indexes the
    /// sidecar, and matches on the typed enum variant. Replaces
    /// the prior `text`-tab-encoded string-parsing path.
    pub fn set_raw_candidates_with_routing(&mut self, items: Vec<(RawCandidate, RoutingPayload)>) {
        let mut raw: Vec<RawCandidate> = Vec::with_capacity(items.len());
        let mut routing: Vec<RoutingPayload> = Vec::with_capacity(items.len());
        for (mut cand, payload) in items {
            let idx = routing.len() as u32;
            cand.data = CandidateData::Extension {
                kind_id: PICKER_ROUTING_KIND_ID,
                payload: idx.to_le_bytes().to_vec(),
            };
            raw.push(cand);
            routing.push(payload);
        }
        self.raw = raw;
        self.routing_meta = routing;
        self.refilter();
    }

    /// Look up the routing payload for `candidate` -- returns
    /// `None` for candidates that don't carry a picker-routing
    /// `Extension` payload (defensive; the picker only ever
    /// builds candidates through [`Self::set_raw_candidates_with_routing`]
    /// in the new world). Used by the accept dispatch.
    pub fn routing_for(&self, candidate: &RenderedCandidate) -> Option<&RoutingPayload> {
        let CandidateData::Extension { kind_id, payload } = &candidate.raw.data else {
            return None;
        };
        if *kind_id != PICKER_ROUTING_KIND_ID {
            return None;
        }
        if payload.len() != 4 {
            return None;
        }
        let idx = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
        self.routing_meta.get(idx)
    }

    /// Replace the raw candidate list with externally-built LSP
    /// location rows -- multi-result navigation, references,
    /// diagnostics. Caller builds + sorts + dedups the `Vec`
    /// host-side; the picker just stores + refilters.
    pub fn set_lsp_locations(&mut self, rows: Vec<LspLocationRow>) {
        let items: Vec<(RawCandidate, RoutingPayload)> = rows
            .into_iter()
            .map(|r| r.into_candidate_with_routing())
            .collect();
        self.set_raw_candidates_with_routing(items);
    }

    /// Replace the raw candidate list with externally-built LSP
    /// instance rows. Caller (`App::open_lsp_picker`) snapshots the
    /// supervisor under its lock and hands the resulting tuples
    /// here. Refreshes the filter.
    ///
    /// Slice 15: stamps each candidate's `accept_action` based
    /// on the picker's `on_accept` (OpenLspLog vs
    /// OpenLspTraceLog) so the typed dispatch (7d.0) fires
    /// instead of the legacy PickerAction match.
    pub fn set_lsp_instances(&mut self, rows: Vec<LspInstanceRow>) {
        let prefilter = match &self.source {
            PickerSource::LspInstances { prefilter } => prefilter.clone(),
            _ => None,
        };
        let on_accept = self.on_accept;
        let items: Vec<(RawCandidate, RoutingPayload)> = rows
            .into_iter()
            .filter(|r| match &prefilter {
                Some(want) => r.server_id == *want,
                None => true,
            })
            .map(|r| {
                let (mut raw, routing) = r.into_candidate_with_routing();
                if let RoutingPayload::LspInstance {
                    ref server_id,
                    ref workspace,
                } = routing
                {
                    raw.accept_action = match on_accept {
                        PickerAction::OpenLspLog => {
                            Some(Box::new(lattice_completion::AcceptAction::OpenLspLog {
                                server_id: server_id.clone(),
                                workspace: workspace.clone(),
                            }))
                        }
                        PickerAction::OpenLspTraceLog => Some(Box::new(
                            lattice_completion::AcceptAction::OpenLspTraceLog {
                                server_id: server_id.clone(),
                                workspace: workspace.clone(),
                            },
                        )),
                        _ => None,
                    };
                }
                (raw, routing)
            })
            .collect();
        self.set_raw_candidates_with_routing(items);
    }

    /// Replace the raw candidate list with externally-built AI
    /// session rows. Caller (`App::do_open_ai_log`) snapshots the
    /// `AiLogger` service's `known_sessions()` and hands the rows
    /// here. Honors the source's optional `provider` prefilter.
    /// Mirrors [`Self::set_lsp_instances`], minus the
    /// `accept_action` stamping (the AI picker rides the legacy
    /// `RoutingPayload::AiSession` accept dispatch).
    pub fn set_ai_sessions(&mut self, rows: Vec<AiSessionRow>) {
        let prefilter = match &self.source {
            PickerSource::AiSessions { prefilter } => prefilter.clone(),
            _ => None,
        };
        let items: Vec<(RawCandidate, RoutingPayload)> = rows
            .into_iter()
            .filter(|r| match &prefilter {
                Some(want) => r.provider == *want,
                None => true,
            })
            .map(|r| r.into_candidate_with_routing())
            .collect();
        self.set_raw_candidates_with_routing(items);
    }

    /// Filter `raw` against the current `query` and write the
    /// matches into `candidates`. Routes through
    /// [`lattice_completion::fuzzy_match`] -- the same 5-tier
    /// algorithm Insert-mode completion uses (exact / prefix /
    /// word-boundary / substring / subsequence). Picker rows
    /// match against `display` because their `text` field
    /// often carries a routing payload (e.g.
    /// `"<server_id>\t<workspace>"`) the user never sees.
    /// Empty query yields a uniform score so every candidate
    /// passes through; the rust stdlib's stable sort preserves
    /// the host-supplied insertion order on ties (callers like
    /// the buffer switcher depend on this -- alternate-buffer
    /// floats to the top via insertion order).
    pub fn refilter(&mut self) {
        // Live-source bypass: the seating source's external
        // engine (grep, future LSP workspace-symbols) IS the
        // filter -- it returned exactly the rows that match
        // the user's query, in the order it wants them. Fuzzy-
        // matching on top would re-rank or drop rows, defeating
        // the point. Render `raw` 1:1, score = 0, no match
        // ranges (the renderer can highlight via a future
        // source-supplied annotation channel; not in v1).
        if self.live_source_mode {
            self.candidates = self
                .raw
                .iter()
                .cloned()
                .map(|raw| RenderedCandidate {
                    raw,
                    score: MatchScore(0),
                    match_ranges: Vec::new(),
                    annotations: Vec::new(),
                })
                .collect();
            if self.selected >= self.candidates.len() {
                self.selected = self.candidates.len().saturating_sub(1);
            }
            return;
        }
        // Slice `3c.unify.picker-via-pipeline`: picker filter +
        // rank now flows through `CompletionPipeline::match_and_rank`,
        // the shared match+rank entry point in `lattice-completion`.
        //
        // Picker-specific pipeline shape:
        //   - matcher: `FuzzyDisplayMatcher` — picker rows match
        //     on `display` (user-visible label), not `text` (which
        //     carries the routing payload).
        //   - rankers: `MruRanker` capturing the picker's bonus
        //     map via the same index-in-CandidateData encoding the
        //     prior inline path used. The ranker subsumes the
        //     "sort by `score + bonus` descending" logic.
        //   - generators: empty (picker pre-supplies `raw`).
        //   - annotators: empty (picker has no annotations today;
        //     slice 6 plumbs marginalia in).
        //
        // Score = match (0..1000) + mru_bonus (0..~110 typical).
        // Bonus sits below the tier delta between match tiers
        // (200 between FUZZY_LOW and SUBSTRING) so it functions
        // as a within-tier tie-breaker rather than a tier
        // override.
        let bonuses = self.mru_bonuses.clone();
        let pipeline = CompletionPipeline {
            generators: Vec::new(),
            matcher: std::sync::Arc::new(FuzzyDisplayMatcher),
            rankers: vec![std::sync::Arc::new(MruRanker::new(move |raw| {
                bonus_for_raw(raw, &bonuses)
            }))],
            annotators: Vec::new(),
        };
        self.candidates = pipeline.match_and_rank(&self.query, &self.raw);
        if self.selected >= self.candidates.len() {
            self.selected = self.candidates.len().saturating_sub(1);
        }
    }

    pub fn append_query(&mut self, c: char) {
        self.query.push(c);
        self.query_cursor = self.query.len();
        self.selected = 0;
        self.refilter();
    }

    /// Append a whole pasted burst to the query.
    ///
    /// **Newlines are flattened to spaces rather than dropped or
    /// honoured.** The query is a single line, so a multi-line paste has
    /// to become one — and joining with nothing would weld the last word
    /// of each line to the first of the next (`foo.rs` + `bar.rs` →
    /// `foo.rsbar.rs`), which matches nothing and looks like the paste
    /// was corrupted. Other control characters are dropped: they cannot
    /// be typed into the query, so they cannot be intended in it, and a
    /// stray `\t` or `\r` from a terminal round-trip would silently make
    /// the filter match nothing.
    ///
    /// Returns `false` when the burst contributes nothing, so the caller
    /// can skip the refilter and the preview.
    pub fn paste_query(&mut self, text: &str) -> bool {
        let cleaned: String = text
            .chars()
            .filter_map(|c| match c {
                '\n' | '\r' | '\t' => Some(' '),
                c if c.is_control() => None,
                c => Some(c),
            })
            .collect();
        if cleaned.is_empty() {
            return false;
        }
        self.query.push_str(&cleaned);
        self.query_cursor = self.query.len();
        self.selected = 0;
        self.refilter();
        true
    }

    pub fn backspace_query(&mut self) {
        if let Some(last) = self.query.chars().last() {
            let new_len = self.query.len() - last.len_utf8();
            self.query.truncate(new_len);
            self.query_cursor = self.query.len();
            self.selected = 0;
            self.refilter();
        }
    }

    pub fn clear_query(&mut self) {
        self.query.clear();
        self.query_cursor = 0;
        self.selected = 0;
        self.refilter();
    }

    pub fn select_next(&mut self) {
        if !self.candidates.is_empty() {
            self.selected = (self.selected + 1) % self.candidates.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.candidates.is_empty() {
            self.selected = if self.selected == 0 {
                self.candidates.len() - 1
            } else {
                self.selected - 1
            };
        }
    }

    pub fn selected_candidate(&self) -> Option<&RenderedCandidate> {
        self.candidates.get(self.selected)
    }
}

/// Resolve the MRU bonus stamped on `raw` against the parallel
/// `bonuses` slice. Returns 0.0 for candidates without an MRU
/// routing payload (non-picker `RawCandidate`s, or pickers
/// seated before the bonuses vec was populated). Pure helper
/// so the hot-path refilter inlines it.
fn bonus_for_raw(raw: &RawCandidate, bonuses: &[f64]) -> f64 {
    let CandidateData::Extension { kind_id, payload } = &raw.data else {
        return 0.0;
    };
    if *kind_id != PICKER_ROUTING_KIND_ID {
        return 0.0;
    }
    if payload.len() != 4 {
        return 0.0;
    }
    let idx = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
    bonuses.get(idx).copied().unwrap_or(0.0)
}

/// One row of the LSP-instance source. The picker host (App)
/// snapshots this from `LspSupervisor::running_actors()` under
/// the supervisor lock, then drops the lock before handing the
/// vec to the picker. Decouples the picker module from the
/// supervisor's async `Mutex`.
#[derive(Debug, Clone)]
pub struct LspInstanceRow {
    pub workspace: PathBuf,
    pub server_id: String,
    pub buffer_count: usize,
    /// One-line capability summary -- `hover def refs comp`-style
    /// glyph cluster. The host (e.g. `lattice-ui-tui`'s
    /// `summarise_capabilities`) builds this; we just hold a
    /// string.
    pub cap_summary: String,
}

impl LspInstanceRow {
    /// Render this row as a `(RawCandidate, RoutingPayload)`
    /// pair (Phase 4.2.g.7 polish). The candidate's `text`
    /// holds the user-facing server id (the matcher matches
    /// against `display`, so `text`'s value is observational
    /// only); the routing payload carries the typed
    /// `(server_id, workspace)` pair the accept dispatch reads.
    pub fn into_candidate_with_routing(self) -> (RawCandidate, RoutingPayload) {
        let workspace_str = self.workspace.display().to_string();
        let mut raw = RawCandidate::plain(
            self.server_id.clone(),
            lattice_completion::CandidateKind::Plain,
        );
        let marginalia = format!(
            "{} buf{}  {}",
            self.buffer_count,
            if self.buffer_count == 1 { "" } else { "s" },
            self.cap_summary,
        );
        let body = format!("{:<20} {workspace_str}", self.server_id);
        raw.display = format!("{body:<70} {marginalia}");
        let routing = RoutingPayload::LspInstance {
            server_id: self.server_id,
            workspace: self.workspace,
        };
        (raw, routing)
    }
}

/// One row of the AI-session source. The picker host (App)
/// snapshots this from the `AiLogger` service's `known_sessions()`
/// (no async lock — the logger's ring map is a sync `Mutex`), then
/// hands the vec to the picker. Mirrors [`LspInstanceRow`], one
/// level simpler: a session has no workspace / buffer-count /
/// capability marginalia, only its `(provider, index)` key.
#[derive(Debug, Clone)]
pub struct AiSessionRow {
    pub provider: String,
    pub index: u32,
}

impl AiSessionRow {
    /// Render this row as a `(RawCandidate, RoutingPayload)` pair.
    /// The candidate's `display` is the user-facing
    /// `<provider>:<index>` (matching the `*ai:<provider>:<index>*`
    /// buffer name body); the typed [`RoutingPayload::AiSession`]
    /// carries the `(provider, index)` key the accept dispatch
    /// reconstructs into a `SessionKey`.
    pub fn into_candidate_with_routing(self) -> (RawCandidate, RoutingPayload) {
        let label = format!("{}:{}", self.provider, self.index);
        let mut raw = RawCandidate::plain(label.clone(), lattice_completion::CandidateKind::Plain);
        raw.display = label;
        let routing = RoutingPayload::AiSession {
            provider: self.provider,
            index: self.index,
        };
        (raw, routing)
    }
}

/// One row of an LSP-location source -- multi-result navigation,
/// references, diagnostics. Carries the canonical
/// `(path, line, col)` triple the host needs to jump (LSP 0-based
/// line, utf-8 byte column already converted host-side) plus the
/// presentation pieces.
///
/// `display` is what the picker paints (e.g.
/// `src/foo.rs:42:7  let bar = ...`); the typed
/// [`RoutingPayload::LspLocation`] carries the `(path, line,
/// col)` triple to the accept dispatch. `marginalia` is
/// optional and renders right-aligned (e.g. severity `[E]` for
/// diagnostics, kind `[fn]` for symbols once we add
/// documentSymbol picker).
#[derive(Debug, Clone)]
pub struct LspLocationRow {
    pub path: PathBuf,
    /// LSP 0-based line.
    pub line: u32,
    /// utf-8 byte column.
    pub col: u32,
    /// Optional preview text (e.g. the line content from the file)
    /// to append after the `path:line:col` prefix. Empty string
    /// is fine -- only the prefix renders.
    pub preview: String,
    /// Right-aligned annotation. Empty string skips the column.
    pub marginalia: String,
    /// Syntax-highlight spans for `preview.trim_start()`, i.e. relative to
    /// the preview text as it appears at the tail of the rendered `display`
    /// (NOT yet offset by the `path:line:col` prefix). The host fills these
    /// grep-style (grammar by file extension); `into_candidate_with_routing`
    /// shifts them by the prefix length. Empty = plain preview. PH.2/PH.3
    /// mechanism, extended to LSP location pickers.
    pub display_spans: Vec<lattice_completion::DisplaySpan>,
}

impl LspLocationRow {
    /// Build a row from a fully-resolved location triple. Reads
    /// the line text from disk best-effort (callers may also
    /// pre-populate `preview`).
    pub fn from_path_line_col(path: impl Into<PathBuf>, line: u32, col: u32) -> Self {
        Self {
            path: path.into(),
            line,
            col,
            preview: String::new(),
            marginalia: String::new(),
            display_spans: Vec::new(),
        }
    }

    /// Render as a `(RawCandidate, RoutingPayload)` pair (Phase
    /// 4.2.g.7 polish). `text` carries the user-visible
    /// `path:line:col` form (matcher matches on `display`, so
    /// `text` is observational); the routing payload carries
    /// the typed `(path, line, col)` triple the jump dispatch
    /// consumes.
    pub fn into_candidate_with_routing(self) -> (RawCandidate, RoutingPayload) {
        let path_str = self.path.display().to_string();
        // 1-based line / col in the display; LSP 0-based line +
        // utf-8 byte column ride in the routing payload.
        let display = if self.preview.is_empty() && self.marginalia.is_empty() {
            format!("{path_str}:{}:{}", self.line + 1, self.col + 1)
        } else if self.marginalia.is_empty() {
            format!(
                "{path_str}:{}:{}  {}",
                self.line + 1,
                self.col + 1,
                self.preview.trim_start()
            )
        } else {
            format!(
                "{}  {path_str}:{}:{}  {}",
                self.marginalia,
                self.line + 1,
                self.col + 1,
                self.preview.trim_start()
            )
        };
        let mut raw = RawCandidate::plain(
            format!("{path_str}:{}:{}", self.line + 1, self.col + 1),
            lattice_completion::CandidateKind::Plain,
        );
        raw.display = display;
        // PH.2/PH.3: the host highlighted `preview.trim_start()` grep-style
        // (grammar by file extension). Those spans are relative to the
        // preview; shift them by the length of the rendered prefix
        // (`[marginalia  ]path:line:col  `) so they land on the preview run
        // inside `display`, then attach. Empty when unhighlighted (no
        // grammar / no preview) → plain row, exactly as before.
        if !self.display_spans.is_empty() {
            let prefix_len = raw
                .display
                .len()
                .saturating_sub(self.preview.trim_start().len());
            raw.display_spans = self
                .display_spans
                .iter()
                .map(|s| lattice_completion::DisplaySpan {
                    range: (s.range.start + prefix_len)..(s.range.end + prefix_len),
                    style: s.style,
                })
                .collect();
        }
        // Slice 10: typed accept_action so LSP locations
        // (references / definitions / declaration / type-defs /
        // implementations / diagnostics) flow through 7d.0's
        // DefaultAcceptHandler dispatch + 7g's typed preview.
        // Closes the LSP-references-preview gap the user flagged
        // (2026-05-21) — every LSP location picker now previews
        // the file at the reference's line, not buffer-switcher
        // only.
        raw.accept_action = Some(Box::new(
            lattice_completion::AcceptAction::JumpToFileLocation {
                path: self.path.clone(),
                line: self.line,
                col: self.col,
            },
        ));
        let routing = RoutingPayload::LspLocation {
            path: self.path,
            line: self.line,
            col: self.col,
        };
        (raw, routing)
    }
}

#[cfg(test)]
mod tests {

    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use lattice_completion::CandidateKind;

    fn unwind_spec(title: &str) -> std::sync::Arc<TransientSpec> {
        std::sync::Arc::new(TransientSpec {
            title: title.into(),
            groups: Vec::new(),
            preview: None,
            footer: None,
        })
    }

    fn unwind_picker() -> Picker {
        let mut p = Picker::new("t", PickerSource::Buffers, PickerAction::SwitchToBuffer);
        p.transient = Some(unwind_spec("child"));
        p
    }

    /// MG.29: `<Esc>` in a submenu goes back to its parent, and only
    /// closes once there is no parent left. Exiting all the way out on
    /// the first press punishes the ordinary mistake — opening the
    /// wrong submenu.
    #[test]
    fn unwinding_pops_one_level_at_a_time() {
        let mut p = unwind_picker();
        p.transient_stack
            .push((unwind_spec("root"), TransientState::new(), 3));

        assert!(p.transient_unwind(), "there is a parent to go back to");
        assert_eq!(
            p.transient.as_ref().map(|s| s.title.clone()),
            Some("root".into())
        );
        assert_eq!(
            p.transient_selected, 3,
            "the parent's selection comes back where the user left it"
        );

        assert!(
            !p.transient_unwind(),
            "at the root there is nothing left to unwind — the caller closes"
        );
    }

    /// A half-typed multi-key row is what the key most likely means to
    /// undo, so it goes first — the same precedence vim gives `<Esc>`
    /// over a partial chord.
    #[test]
    fn a_pending_prefix_is_undone_before_the_stack() {
        let mut p = unwind_picker();
        p.transient_stack
            .push((unwind_spec("root"), TransientState::new(), 0));
        p.transient_prefix = ",".into();

        assert!(p.transient_unwind());
        assert!(p.transient_prefix.is_empty(), "the prefix cleared");
        assert_eq!(
            p.transient.as_ref().map(|s| s.title.clone()),
            Some("child".into()),
            "and the submenu is still open — one key, one undo"
        );

        assert!(p.transient_unwind(), "the next press pops the stack");
        assert_eq!(
            p.transient.as_ref().map(|s| s.title.clone()),
            Some("root".into())
        );
    }

    #[test]
    fn a_picker_with_no_transient_never_claims_to_unwind() {
        let mut p = Picker::new("t", PickerSource::Buffers, PickerAction::SwitchToBuffer);
        assert!(!p.transient_unwind());
    }

    /// LSP-picker highlighting: the host highlights the *trimmed* preview
    /// grep-style, so spans are preview-relative; `into_candidate_with_routing`
    /// must shift them past the `path:line:col` prefix so they land on the
    /// preview run within the rendered `display`.
    #[test]
    fn lsp_location_preview_spans_shift_past_prefix() {
        let mut row = LspLocationRow::from_path_line_col("src/main.rs", 4, 8);
        row.preview = "    let x = 1;".to_string(); // leading ws → trim_start drops it
        let trimmed = "let x = 1;";
        row.display_spans = vec![lattice_completion::DisplaySpan {
            range: 0..3, // "let" within the trimmed preview
            style: lattice_cells::style::Style::Keyword,
        }];

        let (raw, _routing) = row.into_candidate_with_routing();

        assert!(
            raw.display.ends_with(trimmed),
            "display = {:?}",
            raw.display
        );
        let prefix = raw.display.len() - trimmed.len();
        assert_eq!(raw.display_spans.len(), 1);
        let span = &raw.display_spans[0];
        assert_eq!(span.range, prefix..prefix + 3, "span shifted by the prefix");
        assert_eq!(
            &raw.display[span.range.clone()],
            "let",
            "the shifted span must land exactly on the keyword in `display`"
        );
        assert_eq!(span.style, lattice_cells::style::Style::Keyword);
    }

    /// A row the host didn't highlight (no grammar / message preview / symbol
    /// label) renders exactly as before — no spans, plain candidate.
    #[test]
    fn lsp_location_without_spans_stays_plain() {
        let mut row = LspLocationRow::from_path_line_col("notes.txt", 0, 0);
        row.preview = "just prose".to_string();
        let (raw, _routing) = row.into_candidate_with_routing();
        assert!(
            raw.display_spans.is_empty(),
            "unhighlighted row stays plain"
        );
    }

    /// Build a buffer-source-shaped raw candidate by hand. Mirrors
    /// the host's `raw_buffer_candidates` shape (`text = "#<id>"`,
    /// display ends with the kind label) without depending on the
    /// host's `BufferRegistry`.
    fn buffer_candidate(id: u32, label: &str, kind: &str, current: bool) -> RawCandidate {
        let active_marker = if current { " (current)" } else { "" };
        let mut raw = RawCandidate::plain(format!("#{id}"), CandidateKind::Buffer);
        raw.display = format!("#{id:<3} {label:<55} {kind}{active_marker}");
        raw
    }

    fn buffer_fixture() -> Vec<RawCandidate> {
        vec![
            buffer_candidate(1, "lsp:rust", "help", false),
            buffer_candidate(2, "describe-command write", "help", false),
        ]
    }

    #[test]
    fn live_source_mode_bypasses_fuzzy_refilter() {
        // Slice 1: when the seating source's spec sets
        // `live = true`, the host calls `set_live_source_mode`
        // and the picker renders `raw` verbatim regardless of
        // the query. The source (grep, future live LSP) IS
        // the filter -- fuzzy-matching on top would re-rank or
        // drop rows the source said matched.
        let mut p = Picker::new("grep", PickerSource::Files, PickerAction::OpenFile);
        p.set_live_source_mode(true);
        p.set_raw_candidates(buffer_fixture());
        // Two candidates, neither matches the query, but both
        // render anyway (bypass).
        p.append_query('z');
        p.append_query('z');
        assert_eq!(p.candidates.len(), 2, "live mode must not drop rows");
        assert_eq!(
            p.candidates[0].raw.display,
            buffer_fixture()[0].display,
            "live mode must preserve source order"
        );
    }

    #[test]
    fn live_source_mode_off_keeps_existing_fuzzy_behaviour() {
        // Regression for non-live sources: same setup minus the
        // live flag must still filter by query (the existing
        // path through `typing_query_filters_to_substring_matches`,
        // pinned again here as a paired counter-example to the
        // live-mode test above).
        let mut p = Picker::new(
            "buffers",
            PickerSource::Buffers,
            PickerAction::SwitchToBuffer,
        );
        assert!(!p.is_live_source_mode());
        p.set_raw_candidates(buffer_fixture());
        p.append_query('z');
        p.append_query('z');
        assert_eq!(p.candidates.len(), 0, "non-live mode must filter");
    }

    #[test]
    fn empty_query_returns_all_candidates_in_source_order() {
        let mut p = Picker::new(
            "buffers",
            PickerSource::Buffers,
            PickerAction::SwitchToBuffer,
        );
        p.set_raw_candidates(buffer_fixture());
        assert_eq!(p.candidates.len(), 2);
    }

    #[test]
    fn typing_query_filters_to_substring_matches() {
        let mut p = Picker::new(
            "buffers",
            PickerSource::Buffers,
            PickerAction::SwitchToBuffer,
        );
        p.set_raw_candidates(buffer_fixture());
        p.append_query('r');
        p.append_query('u');
        p.append_query('s');
        p.append_query('t');
        // Only the lsp:rust buffer matches "rust".
        assert_eq!(p.candidates.len(), 1);
        assert!(p.candidates[0].raw.display.contains("lsp:rust"));
    }

    #[test]
    fn case_insensitive_substring_match() {
        let mut p = Picker::new(
            "buffers",
            PickerSource::Buffers,
            PickerAction::SwitchToBuffer,
        );
        p.set_raw_candidates(buffer_fixture());
        p.append_query('R');
        p.append_query('U');
        p.append_query('S');
        p.append_query('T');
        assert_eq!(p.candidates.len(), 1);
    }

    #[test]
    fn selection_wraps_at_boundaries() {
        let mut p = Picker::new(
            "buffers",
            PickerSource::Buffers,
            PickerAction::SwitchToBuffer,
        );
        p.set_raw_candidates(buffer_fixture());
        p.select_prev(); // wraps to last
        assert_eq!(p.selected, 1);
        p.select_next(); // wraps back to 0
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn backspace_repopulates_filter_results() {
        let mut p = Picker::new(
            "buffers",
            PickerSource::Buffers,
            PickerAction::SwitchToBuffer,
        );
        p.set_raw_candidates(buffer_fixture());
        p.append_query('r');
        p.append_query('u');
        assert_eq!(p.candidates.len(), 1);
        p.backspace_query();
        p.backspace_query();
        assert_eq!(p.candidates.len(), 2);
    }

    #[test]
    fn routing_for_buffer_returns_typed_payload() {
        // Build the candidates with typed routing the same way
        // the App's `raw_buffer_candidates_with_routing` does:
        // each (RawCandidate, RoutingPayload::Buffer { id }) pair.
        let pairs: Vec<(RawCandidate, RoutingPayload)> = vec![
            (
                buffer_candidate(1, "lsp:rust", "help", false),
                RoutingPayload::Buffer { id: 1 },
            ),
            (
                buffer_candidate(2, "describe-command write", "help", false),
                RoutingPayload::Buffer { id: 2 },
            ),
        ];
        let mut p = Picker::new(
            "buffers",
            PickerSource::Buffers,
            PickerAction::SwitchToBuffer,
        );
        p.set_raw_candidates_with_routing(pairs);
        let c = p.selected_candidate().expect("first candidate");
        match p.routing_for(c) {
            Some(RoutingPayload::Buffer { id }) => assert_eq!(*id, 1),
            other => panic!("expected Buffer routing, got {other:?}"),
        }
    }

    #[test]
    fn lsp_instances_source_filters_to_named_server_when_prefilter_set() {
        let rows = vec![
            LspInstanceRow {
                workspace: PathBuf::from("/proj/a"),
                server_id: "rust".into(),
                buffer_count: 2,
                cap_summary: "hover def".into(),
            },
            LspInstanceRow {
                workspace: PathBuf::from("/proj/b"),
                server_id: "rust".into(),
                buffer_count: 1,
                cap_summary: "hover def refs".into(),
            },
            LspInstanceRow {
                workspace: PathBuf::from("/proj/c"),
                server_id: "pyright".into(),
                buffer_count: 1,
                cap_summary: "hover".into(),
            },
        ];
        let mut p = Picker::new(
            "lsp",
            PickerSource::LspInstances {
                prefilter: Some("rust".into()),
            },
            PickerAction::OpenLspLog,
        );
        p.set_lsp_instances(rows);
        // Only the two rust rows survive the prefilter.
        assert_eq!(p.candidates.len(), 2);
        for c in &p.candidates {
            match p.routing_for(c) {
                Some(RoutingPayload::LspInstance { server_id, .. }) => {
                    assert_eq!(server_id, "rust");
                }
                other => panic!("expected LspInstance routing, got {other:?}"),
            }
        }
    }

    #[test]
    fn lsp_instances_source_no_prefilter_includes_all() {
        let rows = vec![
            LspInstanceRow {
                workspace: PathBuf::from("/proj/a"),
                server_id: "rust".into(),
                buffer_count: 2,
                cap_summary: "hover".into(),
            },
            LspInstanceRow {
                workspace: PathBuf::from("/proj/b"),
                server_id: "pyright".into(),
                buffer_count: 1,
                cap_summary: "hover".into(),
            },
        ];
        let mut p = Picker::new(
            "lsp",
            PickerSource::LspInstances { prefilter: None },
            PickerAction::OpenLspLog,
        );
        p.set_lsp_instances(rows);
        assert_eq!(p.candidates.len(), 2);
    }

    #[test]
    fn routing_for_lsp_instance_returns_typed_payload() {
        let rows = vec![LspInstanceRow {
            workspace: PathBuf::from("/proj/example"),
            server_id: "rust".into(),
            buffer_count: 1,
            cap_summary: "hover".into(),
        }];
        let mut p = Picker::new(
            "lsp",
            PickerSource::LspInstances { prefilter: None },
            PickerAction::OpenLspLog,
        );
        p.set_lsp_instances(rows);
        let c = p.selected_candidate().expect("first candidate");
        match p.routing_for(c) {
            Some(RoutingPayload::LspInstance {
                server_id,
                workspace,
            }) => {
                assert_eq!(server_id, "rust");
                assert_eq!(*workspace, PathBuf::from("/proj/example"));
            }
            other => panic!("expected LspInstance routing, got {other:?}"),
        }
    }

    #[test]
    fn routing_for_ai_session_returns_typed_payload() {
        let rows = vec![
            AiSessionRow {
                provider: "opencode".into(),
                index: 1,
            },
            AiSessionRow {
                provider: "opencode".into(),
                index: 2,
            },
        ];
        let mut p = Picker::new(
            "ai-log",
            PickerSource::AiSessions { prefilter: None },
            PickerAction::OpenAiLog,
        );
        p.set_ai_sessions(rows);
        assert_eq!(p.candidates.len(), 2);
        let c = p.selected_candidate().expect("first candidate");
        match p.routing_for(c) {
            Some(RoutingPayload::AiSession { provider, index }) => {
                assert_eq!(provider, "opencode");
                assert_eq!(*index, 1);
            }
            other => panic!("expected AiSession routing, got {other:?}"),
        }
    }

    #[test]
    fn ai_session_prefilter_narrows_by_provider() {
        let rows = vec![
            AiSessionRow {
                provider: "opencode".into(),
                index: 1,
            },
            AiSessionRow {
                provider: "claude".into(),
                index: 1,
            },
        ];
        let mut p = Picker::new(
            "ai-log",
            PickerSource::AiSessions {
                prefilter: Some("opencode".to_string()),
            },
            PickerAction::OpenAiLog,
        );
        p.set_ai_sessions(rows);
        assert_eq!(p.candidates.len(), 1);
    }

    #[test]
    fn selected_candidate_is_none_when_filter_empties_list() {
        let mut p = Picker::new(
            "buffers",
            PickerSource::Buffers,
            PickerAction::SwitchToBuffer,
        );
        p.set_raw_candidates(buffer_fixture());
        p.append_query('z'); // matches nothing
        p.append_query('z');
        p.append_query('z');
        assert!(p.candidates.is_empty());
        assert!(p.selected_candidate().is_none());
    }

    /// Slice 14a: MRU bonuses tilt ranking within a match
    /// tier. Two candidates with equal match scores -- the one
    /// with the higher bonus floats to the top.
    #[test]
    fn mru_bonus_breaks_match_score_ties() {
        let pairs = vec![
            (
                {
                    let mut r = RawCandidate::plain(String::from("alpha"), CandidateKind::Plain);
                    r.display = "alpha-file".into();
                    r
                },
                RoutingPayload::OpenFile {
                    path: PathBuf::from("/tmp/alpha"),
                },
            ),
            (
                {
                    let mut r = RawCandidate::plain(String::from("beta"), CandidateKind::Plain);
                    r.display = "beta-file".into();
                    r
                },
                RoutingPayload::OpenFile {
                    path: PathBuf::from("/tmp/beta"),
                },
            ),
        ];
        let mut p = Picker::new("files", PickerSource::Files, PickerAction::OpenFile);
        p.set_raw_candidates_with_routing(pairs);
        // Empty-query match scores are uniform so the tie-
        // breaker is the MRU bonus alone.
        p.set_mru_bonuses(vec![0.0, 50.0]);
        // beta gets the higher bonus and floats above alpha.
        assert_eq!(p.candidates.len(), 2);
        assert!(p.candidates[0].raw.display.contains("beta"));
        assert!(p.candidates[1].raw.display.contains("alpha"));
    }

    /// Slice 14a: mismatched-length bonuses are clamped to all-
    /// zeros rather than panicking on out-of-range access.
    /// Defensive guard for tests / future async-init paths
    /// that might race the bonus snapshot.
    #[test]
    fn mru_bonus_length_mismatch_zeroes_out() {
        let pairs = vec![(
            {
                let mut r = RawCandidate::plain(String::from("only"), CandidateKind::Plain);
                r.display = "only-file".into();
                r
            },
            RoutingPayload::OpenFile {
                path: PathBuf::from("/tmp/only"),
            },
        )];
        let mut p = Picker::new("files", PickerSource::Files, PickerAction::OpenFile);
        p.set_raw_candidates_with_routing(pairs);
        // 2 bonuses for 1 candidate -- mismatch.
        p.set_mru_bonuses(vec![10.0, 20.0]);
        // Refilter still produces the one candidate, with 0.0
        // bonus (since the mismatch reset to zeros).
        assert_eq!(p.candidates.len(), 1);
        assert_eq!(p.mru_bonus_for(&p.candidates[0]), 0.0);
    }
}
