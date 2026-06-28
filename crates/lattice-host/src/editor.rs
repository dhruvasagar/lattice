// The renderer-agnostic editor state.
//
// Phase 5.B.3 introduces [`Editor`] as the destination for
// the per-cluster field migration from
// `lattice-ui-tui::App`. See
// [`docs/dev/architecture/phase-5b-app-design.md`] for the
// Option-D → Option-E pivot that this struct realises:
//
// - The host owns the editor's state and logic in `Editor`.
// - Each renderer crate composes `Editor` into its own
//   concrete `App` wrapper alongside its renderer-specific
//   caches (`theme`, `pane_render_registry`, ...).
//
// Subsequent slices (5.B.4 onwards) relocate field clusters
// one at a time from `App` into `Editor`, moving the methods
// that touch only those fields into `impl Editor` here. Each
// per-cluster commit ships green: methods that still live in
// `impl App` access migrated fields via `self.editor.foo`;
// methods that have moved access them via `self.foo` (now an
// inherent method on `Editor`).
//
// The empty-now/grows-later shape is intentional: it lets
// the wrapper field `editor: Editor` get added to `App`
// before any field actually moves, giving every subsequent
// migration a target that already exists in the type
// system.

use std::collections::HashMap;
use std::path::PathBuf;

use lattice_grammar::{CommandInvocation, Register};
use lattice_protocol::position::{Position, Range as ProtoRange};

use std::sync::Arc;

use lattice_config::{ConfigRegistry, OptionOverrideSet, ResolvedOptions};
use lattice_core::ui::popup::PopupPlacement;
use lattice_grammar::CommandRegistry;
use lattice_grammar::ModalState;
use lattice_grammar::builtins::Builtins;
use lattice_help::topics::HelpTopicRegistry;
use lattice_lsp::cache::{
    CodeActionOutcome, CodeActionRow, CompletionItemRow, CompletionOutcome,
    CompletionResolveOutcome, DocumentHighlightCache, FormatOutcome, HoverOutcome,
    InsertCompletionLspOutcome, LspCodeLensCache, LspDocumentColorCache, LspDocumentLinksCache,
    LspFoldsCache, LspInlayHintCache, LspNavKind, LspPullDiagnosticsCache, LspSelectionChain,
    LspSemanticTokensCache, ReferencesOutcome, RenameOutcome, SelectionRangeOutcome,
    SignatureHelpOutcome, SymbolsOutcome,
};
// Phase 5.8.AF.5 / Slice 3b.0–3b.5: `CodeLensOutcome`,
// `DocumentColorOutcome`, `DocumentHighlightOutcome`,
// `DocumentLinksOutcome`, `FoldingRangeOutcome`,
// `InlayHintOutcome`, `PullDiagnosticsOutcome`,
// `SemanticTokensOutcome` no longer imported -- their
// `pending_*_rx` fields retired (spawned tasks write directly
// via `PerBufferCache::insert_for` / `ArcSwapOption::store`).
use lattice_lsp::{DiagnosticsLayer, LspLogger, LspSupervisorHandle};
use lattice_mode::{
    ActiveModes, BufferLocals, GuardStoreHandle, ModeRegistry, ServiceRegistry,
    TickCallbackRegistration,
};
use lattice_picker::{Picker, PickerMruIndex, PickerRegistry};
use lattice_protocol::CancellationToken;
use lattice_protocol::Event;
use lattice_protocol::edit::EditDelta;
use lattice_runtime::{EventBus, MessagePushed, MessagesRing, SnapshotCache};
use lattice_syntax::{LangRegistry, SyntaxHandle};

use crate::action::{Action, EchoMessage};
use crate::actions::ActionIds;
use crate::buffer_registry::BufferRegistry;
use crate::buffers::BufferId;
use crate::chord::KeyChord;
use crate::dispatch::RendererSignal;
use crate::keymap_registry::{KeymapHandle, LayerId};
use crate::pane::PaneTree;
use crate::state::{
    CompletionState, LastFind, LastSearch, LastVisual, LivePickerQueryState, MacroRecording,
    OptionCache, PendingBlockInsert, PendingPickerInit, PositionEntry, PrevPaneState, ReplaceEntry,
    SearchLine, SubstitutePreview, TagStackEntry, UnnamedRegister,
};
use crate::versioned::Versioned;
use lattice_core::BufferKind;
use lattice_protocol::position::Position as ProtoPosition;

/// Renderer-agnostic editor state.
///
/// The renderer-agnostic half of every editor App. Each
/// renderer's `App` struct composes one of these alongside
/// its renderer-specific caches. Host-level code (mode
/// lifecycle, dispatch, picker sources, LSP supervisor, ...)
/// takes `&mut Editor` directly; renderer-side code takes
/// `&mut App` and reaches the editor via `app.editor`.
///
/// **Field set grows per-cluster.** Each 5.B.x slice
/// migrates a logical cluster of fields here from
/// `lattice-ui-tui::App`. As clusters land, this struct
/// accumulates state; in parallel, `App`'s direct field set
/// shrinks. When the migration completes, every renderer-
/// agnostic field on App lives here, every renderer-agnostic
/// method on App lives in this crate's `impl Editor` blocks,
/// and App becomes a thin wrapper holding `editor: Editor`
/// plus renderer-specific caches only.
///
/// **Clusters landed so far:**
/// - 5.B.4 -- macro recording state (`macros`,
///   `macro_recording`, `last_played_macro`).
/// - 5.B.5 -- marks + registers (`marks`, `registers`,
///   `pending_register`, `unnamed_register`).
/// - 5.B.6 -- position history + tag stack
///   (`position_history`, `position_history_cursor`,
///   `recent_files`, `tag_stack`, `pending_tag_origin`).
/// - 5.B.7 -- search state (`search_line`, `last_search`,
///   `current_match`, `all_matches`, `substitute_preview`).
/// - 5.B.8 -- vim repeat + visual state (`pending_count`,
///   `op_count`, `visual_anchor`, `last_change`,
///   `last_visual`, `last_find`).
/// - 5.B.9 -- replace + insert state (`replace_history`,
///   `last_insert`, `recording_insert`,
///   `pending_block_insert`).
/// - 5.B.10 -- popup (subset) (`popup_buffer`,
///   `prev_pane_for_help`, `popup_placement`). Skipped:
///   `popup_back_stack` -- holds `PopupSnapshot` which still
///   lives in `lattice-ui-tui::app::popup`; follow-up slice
///   moves the snapshot type to host before migrating the
///   field.
/// - 5.B.11 -- cmdline + echo (`command_line`,
///   `last_message`, `messages`,
///   `pending_message_event_rx`, `pending_redraw`,
///   `command_history`, `command_history_cursor`,
///   `command_history_pending`, `auto_submit_after_chord`).
/// - 5.B.12 -- syntax (`lang_registry`, `syntax`,
///   `last_parsed_text_version`, `pending_syntax_edits`,
///   `last_synced_syntax_version`, `visible_highlights`,
///   `pane_highlights`). Skipped:
///   `visible_highlights_key` -- its type
///   `VisibleHighlightsKey` lives in
///   `lattice-ui-tui::app::highlights` with `pub(super)`
///   visibility; follow-up slice promotes it to host first.
/// - 5.B.13 -- picker (`picker`, `picker_registry`,
///   `picker_mru`, `picker_mru_path`,
///   `pending_picker_init`, `live_picker_query`,
///   `previewing`). Picker support types (`PendingPickerInit`,
///   `LivePickerQueryState`, `InFlightLiveQuery`,
///   `LIVE_PICKER_DEBOUNCE`) also moved from
///   `lattice-ui-tui::app` to `lattice_host::state`.
/// - 5.B.14 -- config + modes (`config`, `option_cache`,
///   `mode_registry`, `services`, `mode_guards`,
///   `active_modes`, `buffer_locals`, `resolved_options`,
///   `buffer_local_overrides`, `option_change_rx`,
///   `help_topics`, `host_theme`).
/// - 5.B.15 -- modal + dispatch (`modal`, `partial_chord`,
///   `registry`, `event_bus`, `builtins`, `action_ids`,
///   `keymap`, `completion_popup_layer`).
/// - 5.B.16 -- active-pane state (subset) (`cursor`,
///   `scroll`, `should_quit`, `viewport_height`,
///   `terminal_width`, `active_buffer`,
///   `document_buffer_id`, `buffers`). Skipped:
///   `document`, `snapshot_cache`, `pane_tree` --
///   their types have no natural `Default` (actor handles
///   + tree-with-root invariants). Follow-up slice removes
///   `#[derive(Default)]` from `Editor` in favour of an
///   `Editor::new(...)` constructor so these can migrate.
/// - 5.B.17 -- LSP per-buffer caches (`lsp_progress`,
///   `lsp_selection_chain`, `lsp_selection_chain_index`,
///   `lsp_document_highlights`,
///   `last_document_highlight_issue_cursor`,
///   `lsp_folds_cache`, `lsp_inlay_hints_cache`,
///   `lsp_document_links_cache`, `lsp_code_lens_cache`,
///   `lsp_document_color_cache`,
///   `lsp_semantic_tokens_cache`,
///   `lsp_pull_diagnostics_cache`).
/// - 5.B.18 -- all remaining LSP fields: subsystem handles
///   (`lsp`, `lsp_diagnostics`, `lsp_logger`, plus
///   `lsp_log_event_rx`, `lsp_progress_event_rx`,
///   `lsp_config_tree`, `buffer_uris`), server-initiated
///   channels (`pending_apply_edit_rx`,
///   `pending_show_message_request_rx`,
///   `lsp_pending_show_message_requests`,
///   `lsp_show_message_request_queue`,
///   `lsp_next_show_message_request_id`), and all per-
///   feature request channels (the `pending_*_rx` /
///   `pending_*_token` pairs for hover, definition,
///   references, symbols, format, signature-help,
///   completion, moniker, rename, code-action,
///   selection-range, document-highlight, folding-range,
///   document-links, code-lens, document-color, inlay-hint,
///   semantic-tokens, pull-diagnostics, plus the refresh
///   channels and the lifecycle / detach channels).
///   `LspSupervisorHandle` and `DiagnosticsLayer` gained
///   placeholder `Default` impls (dropped-receiver
///   channels; production overwrites in `boot.rs`).
///   `lsp_file_watcher` stays on App for now: its inner
///   type `LspFileWatcher` lives in `lattice-ui-tui::app::
///   lsp_watcher` -- migrates with a follow-up that moves
///   the watcher into a host module.
/// - 5.B.19 -- call-site migration for all LSP per-feature
///   request channel fields scaffolded in 5.B.18: updated
///   all `self.pending_*` / `app.pending_*` accesses in
///   `app/lsp.rs`, `app/boot.rs`, `app/picker.rs`,
///   `app/mode.rs`, `app/completion.rs` to
///   `self.editor.pending_*`; removed the now-redundant
///   duplicate declarations from `App`. Completion cluster
///   (`completion_registry`, `completion_state`,
///   `insert_completion`, etc.) stays on App -- next slice.
/// - 5.B.20 -- completion cluster tail + popup back-stack
///   + pending config bucket. `insert_completion`,
///   `snippet_registry`, `insert_completion_snippet_meta`,
///   `completion_accept_freq`, `per_language_completion`,
///   `completion_in_path_context`, `active_snippet`,
///   `snippet_dirs`, `popup_back_stack` (popup #7 tail), and
///   `pending_config_structural_sections` move from `App`
///   to `Editor`. `SnippetCandidateMeta` moved from
///   `lattice-ui-tui::app` to `lattice-host::state` so
///   the sidecar type lives next to the field that owns it;
///   `lattice-ui-tui::app` re-exports the type for
///   compatibility. After this slice the only fields left on
///   `App` are the renderer-specific caches (`theme`,
///   `pane_render_registry`) plus the `LspFileWatcher`
///   wrapper -- `App` becomes a thin renderer wrapper.
/// Cross-thread wake signal for the overlay worker.
///
/// Wraps `Arc<tokio::sync::Notify>` so `Editor` can keep its
/// `#[derive(Default)]` (Notify itself doesn't impl Default).
/// Cloning the wrapper clones the inner Arc — same notify
/// channel. `notify_one()` is fired at the tail of
/// `publish_render_state` so the worker re-evaluates inputs after
/// every state change without polling.
///
/// Phase 5.8.AF.5 / Slice X2; renamed from `HighlightWake` in
/// display-line B4.2 (the worker no longer makes highlights).
#[derive(Clone)]
pub struct OverlayWake(pub Arc<tokio::sync::Notify>);

/// 2026-05-26: invocation-runner function pointer. The host
/// registers one per [`lattice_mode::Mode`] whose
/// [`lattice_mode::Mode::invocation_runner`] returns `Some(id)`.
/// Returns `true` when the runner claimed the invocation,
/// `false` when [`Editor::run_invocation`] should fall through
/// to the grammar Action gate / `run_document_invocation` for
/// central dispatch.
pub type InvocationRunnerFn = fn(&mut Editor, lattice_grammar::CommandInvocation) -> bool;

impl Default for OverlayWake {
    fn default() -> Self {
        Self(Arc::new(tokio::sync::Notify::new()))
    }
}

impl std::fmt::Debug for OverlayWake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayWake").finish_non_exhaustive()
    }
}

/// S2.1 (2026-05-26): wake signal for the cell-builder worker
/// (S2.2+). Same shape as [`OverlayWake`]: a `Notify` cloned
/// into [`Editor::cells_wake`] and a sibling clone held by the
/// worker task. `publish_render_state` fires `notify_one()`
/// after every dispatch tick so the worker re-evaluates inputs
/// from the latest published
/// [`crate::render_state::CellsRenderState`].
#[derive(Clone)]
pub struct CellsWake(pub Arc<tokio::sync::Notify>);

/// D.0a.1 (2026-05-29): wake signal for the
/// `virtual_rows_worker`. Sibling of `CellsWake`. Fired by
/// `publish_render_state` and by provider-state changes (e.g.,
/// `DiffSubsystem` after publishing new hunks). The worker
/// awaits via `notified()` and rebuilds the `VirtualRowMatrix`
/// off the UI thread.
#[derive(Clone)]
pub struct VirtualRowsWake(pub Arc<tokio::sync::Notify>);

impl Default for VirtualRowsWake {
    fn default() -> Self {
        Self(Arc::new(tokio::sync::Notify::new()))
    }
}

impl std::fmt::Debug for VirtualRowsWake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtualRowsWake").finish_non_exhaustive()
    }
}

impl Default for CellsWake {
    fn default() -> Self {
        Self(Arc::new(tokio::sync::Notify::new()))
    }
}

impl std::fmt::Debug for CellsWake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CellsWake").finish_non_exhaustive()
    }
}

/// A minor mode whose active/inactive state is driven by a
/// predicate reading a shared, mode-owned session service —
/// reconciled on the active buffer each `sync_keymap_overlays`
/// cycle. Modes contribute these at boot so the generic
/// overlay-sync carries no subsystem-specific knowledge:
/// `active-snippet-mode` keys off the shared `SnippetSession`
/// (`lattice_snippet::snippet_active_predicate`). See
/// `feedback_mode_owns_its_surface`.
#[derive(Clone)]
pub struct SessionBackedMinor {
    /// `true` ⇒ the mode should be active on the **given buffer**.
    /// SN.3e: the predicate is buffer-scoped so a session live in one
    /// buffer never activates the mode in another; `sync_keymap_overlays`
    /// passes the buffer it is reconciling.
    pub active: std::sync::Arc<dyn Fn(lattice_core::BufferId) -> bool + Send + Sync>,
    /// The minor mode toggled by `active`.
    pub mode_id: lattice_mode::ModeId,
}

impl std::fmt::Debug for SessionBackedMinor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The predicate closure isn't Debug and is now buffer-scoped
        // (no buffer to evaluate against here); report the mode it
        // drives instead.
        f.debug_struct("SessionBackedMinor")
            .field("mode_id", &self.mode_id)
            .finish_non_exhaustive()
    }
}

/// I4 (openDiff) D-fix.1: the pane/buffer bookkeeping needed to tear a
/// programmatic side-by-side diff down cleanly — close the two transient diff
/// panes and return focus to the pane the `openDiff` was launched from (the
/// `:claude` terminal). Recorded by
/// [`Editor::open_programmatic_diff`](crate::dispatch), consumed by
/// `Editor::finish_programmatic_diff_panes` on `:diff-accept` / `:diff-reject`.
/// Keyed in [`Editor::programmatic_diff_panes`] by the session's primary
/// (proposed / right) `BufferId`.
#[derive(Debug, Clone)]
pub struct ProgrammaticDiffPanes {
    /// The pane that was active when `openDiff` fired (the `:claude` terminal
    /// pane). Focus returns here on resolve — Option A: claude stays put while
    /// the diff opens in transient panes to its right.
    pub origin_pane: lattice_core::ui::pane::PaneId,
    /// The transient baseline + proposed panes opened for the diff; both closed
    /// on resolve. The `origin_pane` is never in this list.
    pub diff_panes: Vec<lattice_core::ui::pane::PaneId>,
    /// The throwaway in-memory baseline + proposed buffers; removed from the
    /// registry on resolve so they don't linger in `:ls`.
    pub diff_buffers: Vec<lattice_core::BufferId>,
    /// D-fix.6: the IDE-peer connection that opened this diff (its
    /// `ProgrammaticDiffRequest.origin_session`). A session-scoped close
    /// (`close_tab` / `closeAllDiffTabs` from that same connection) tears
    /// down only the diffs whose `origin_session` matches — so one agent
    /// session can never close another's diffs. `0` = no originating session
    /// (a non-IDE producer), matched by no connection's close.
    pub origin_session: u64,
}

#[derive(Debug, Default)]
pub struct Editor {
    /// Perf plan B.4: identity-preserving sub-state cache for
    /// `build_render_state`. Cached `Arc<SubState>` slots keyed
    /// by the `u64` version captured from the corresponding
    /// `Versioned<T>` field. `std::sync::Mutex` (not `RefCell`)
    /// because `Editor` is shared across threads as `Arc<Editor>`
    /// and therefore must be `Sync`; uncontested in practice
    /// because only `build_render_state` (called on the actor
    /// thread) takes the lock. See
    /// [`crate::render_state::PublishCache`] for the slot
    /// inventory and rebuild contract.
    pub publish_cache: std::sync::Mutex<crate::render_state::PublishCache>,

    /// Completed macro recordings keyed by register name.
    /// Replays go through the dispatch layer's `PlayMacro`
    /// action handler. v1 records `Action` streams; insert-
    /// mode keystrokes ARE captured (every Action::Insert is
    /// recorded), but dot-repeat-style replay of insert content
    /// from `c`/`i`/`a` remains a §15 follow-up.
    pub macros: HashMap<char, Vec<Action>>,
    /// In-flight macro recording. `Some` while between
    /// `q<reg>` start and the matching `q` stop; pushed
    /// Actions append to `actions`.
    pub macro_recording: Option<MacroRecording>,
    /// The most recently played macro register, for `@@`
    /// repeat.
    pub last_played_macro: Option<char>,
    /// Unnamed register -- destination of `y` / `d` / `c`,
    /// source of `p` / `P`. `None` until something has been
    /// yanked.
    pub unnamed_register: Option<UnnamedRegister>,
    /// User-set marks. v1 stores them flat by name (a-z,
    /// A-Z, 0-9); uppercase / numbered global marks treat
    /// all marks as buffer-local since the v1 TUI runs
    /// against a single document.
    pub marks: HashMap<char, Position>,
    /// Named registers `"a-z`, `"A-Z`, numbered `"0-"9`,
    /// etc. Stores content + kind. `""` (the unnamed
    /// register) is [`Self::unnamed_register`]; this map
    /// covers everything else.
    pub registers: HashMap<Register, UnnamedRegister>,
    /// Register selected for the next operator / paste
    /// (`"a` prefix). Consumed-and-cleared by `run_invocation`
    /// (operators) and `do_paste` (paste). `None` means use
    /// unnamed.
    pub pending_register: Option<Register>,
    /// Unified position-history ring (DESIGN.md §5.1.1).
    /// Every entry is tagged by source so different keybindings
    /// walk filtered views of the same data (`Ctrl-O` / `Ctrl-I`
    /// walk `AutoJump` + `PluginPush`; `g;` / `g,` walk
    /// `NamedMark`).
    pub position_history: Vec<PositionEntry>,
    /// Cursor into [`Self::position_history`] -- the next entry
    /// the navigation action would visit.
    pub position_history_cursor: usize,
    /// MRU list of canonical paths the user has opened via
    /// `:edit` (or any path flowing through `do_edit`). Newest
    /// first; deduplicated; capped at `MAX_RECENT_FILES`. Source
    /// for the `:recent` picker.
    pub recent_files: Vec<PathBuf>,
    /// Vim-style tag stack (DESIGN.md §5.1.1 follow-up).
    /// Distinct from the jump list: each "drill-down" navigation
    /// (`gd` / `gD` / `gy` / `gI` and their multi-result picker
    /// accept variants) pushes one entry; `<C-t>` pops the most
    /// recent entry. `<C-o>` walks all jumps chronologically;
    /// `<C-t>` pops only the LIFO tag-style drill-downs.
    pub tag_stack: Vec<TagStackEntry>,
    /// Pre-jump origin captured when an LSP nav request fires;
    /// transferred to [`Self::tag_stack`] on the actual jump
    /// (single-result drain or multi-result picker accept).
    /// Cleared on picker dismiss / nav cancellation / drain
    /// with no results.
    pub pending_tag_origin: Option<TagStackEntry>,
    /// In-progress `/` or `?` search. `Some` only while
    /// `modal == ModalState::Search(_)`.
    pub search_line: Option<SearchLine>,
    /// Most recent submitted search; consulted by `n` / `N`.
    pub last_search: Option<LastSearch>,
    /// Range of the most recent search match, used to draw
    /// the primary highlight in the buffer view. Cleared on
    /// Esc and on cursor motion.
    pub current_match: Option<ProtoRange>,
    /// Every occurrence of the most recent search pattern,
    /// used to draw the secondary "hlsearch" overlay.
    /// Cleared on Esc; persists after submit until the next
    /// search.
    pub all_matches: Vec<ProtoRange>,
    /// In-progress substitute preview. Populated as the user
    /// types `:s/pat...`; the renderer overlays match ranges
    /// (and the typed replacement once the second `/` has
    /// been entered) so the user sees the substitution before
    /// pressing Enter. Cleared when the cmdline closes or the
    /// input no longer parses as a substitute (DESIGN.md
    /// §5.9.10).
    pub substitute_preview: Option<SubstitutePreview>,
    /// In-progress count prefix being typed (`3` of `3w`,
    /// `12` of `12dd`). 0 means "no count typed". The next
    /// `Action::Invoke` consumes this and resets it to 0.
    pub pending_count: u32,
    /// Count latched when an operator key was pressed (`2`
    /// of `2d3w`). Multiplied with the motion's count (`3`)
    /// to give the final count the operator dispatches with
    /// (`6`). 0 means "no operator count".
    pub op_count: u32,
    /// Anchor position when Visual mode was entered. `None`
    /// outside Visual; restored on Esc. The `head` of the
    /// selection follows the cursor; the anchor stays put so
    /// the selection extends or contracts as the user moves.
    pub visual_anchor: Option<Position>,
    /// Last operator-class invocation that mutated the
    /// buffer. `.` re-dispatches it from the current cursor.
    /// v1 records operator + motion / operator + range /
    /// Visual-mode operator; insert-mode text replay remains
    /// a §5.2.4 gap.
    pub last_change: Option<CommandInvocation>,
    /// Last Visual-mode selection extents, captured on exit
    /// so `gv` can re-enter Visual with the same anchor /
    /// head / kind.
    pub last_visual: Option<LastVisual>,
    /// Last f/F/t/T find on this buffer, for `;` / `,`.
    pub last_find: Option<LastFind>,
    /// Per-Replace-session log of overwritten bytes so
    /// backspace can restore the original (rather than
    /// deleting). Cleared on entry, pushed on each
    /// `OverwriteChar`, popped on `ReplaceUndoLast`.
    pub replace_history: Vec<ReplaceEntry>,
    /// Text inserted during the most recently completed
    /// Insert session. Captured on Esc out of Insert;
    /// replayed by dot-repeat after the operator part. `None`
    /// if the last change had no insert phase.
    pub last_insert: Option<String>,
    /// In-flight blockwise-visual `I` / `A` session. Captured
    /// at mode-entry time (block extents + per-line insert
    /// column); consumed when Insert exits, at which point
    /// the recorded text is replicated to every line in the
    /// block other than the top row (the top row's insert is
    /// the recording itself). `None` outside a block-visual
    /// insert.
    pub pending_block_insert: Option<PendingBlockInsert>,
    /// Text being captured during the *current* Insert
    /// session. Promoted into [`Self::last_insert`] when
    /// leaving Insert.
    pub recording_insert: Option<String>,
    /// Active popup buffer slot, if a popup overlay is open.
    /// The concrete content lives in the [`crate::buffer_registry::BufferRegistry`]
    /// keyed by this id, flagged
    /// `BufferFlags { listed: false, hidden: true }`.
    pub popup_buffer: Option<BufferId>,
    /// Pane state captured before activating help -- used by
    /// `dismiss_popup` to restore the user to whatever buffer
    /// / cursor / scroll they came from. Set by both display
    /// paths (in-pane activation and popup overlay); cleared
    /// by dismiss.
    pub prev_pane_for_help: Option<PrevPaneState>,
    /// Where the popup overlay sits on screen when one is
    /// open. Lives on the editor (not on the buffer) because
    /// the popup is a generic rectangular surface inside
    /// which any buffer kind renders -- placement is a
    /// property of the popup, not of whatever buffer happens
    /// to be its content.
    pub popup_placement: PopupPlacement,
    /// Cursor position snapshot at popup-open time, used as
    /// the anchor for `CursorAnchored` popups. Captured in
    /// `open_floating_popup` / `open_popup` BEFORE any cursor
    /// mutation so the renderer paints the popup at the
    /// symbol the user pressed K on, not the cursor's current
    /// position. Issue 2026-05-22 (third triage round): without
    /// this the popup follows the cursor — moving with motions
    /// rather than staying anchored. `None` when no popup is
    /// open OR for centered popups (anchor irrelevant).
    pub popup_anchor: Option<lattice_protocol::Position>,
    /// Document scroll captured at popup-open time so
    /// CursorAnchored renderers can convert `popup_anchor.line`
    /// to a screen row in State B (where `self.scroll` is the
    /// POPUP's scroll, not the document's).
    pub popup_doc_scroll_at_anchor: u32,
    /// PU.1a: the popup's persisted view state when it is NOT the
    /// focused buffer (State A), and the stash loaded into
    /// `self.scroll` / `self.cursor` when focus moves into the
    /// popup (State B). Replaces the old `HelpBuffer.{scroll,cursor}`
    /// registry fields now that help content is an actor-backed
    /// Document with no per-view cursor of its own. Reset to 0 /
    /// ZERO on every popup open; updated by `snapshot_active_pane`
    /// when an in-pane help buffer is stashed.
    pub popup_scroll: u32,
    pub popup_cursor: lattice_protocol::Position,
    /// In-progress text in the `:` minibuffer. Populated
    /// only while `modal == ModalState::Command`.
    pub command_line: String,
    /// Most recent transient status / error message,
    /// displayed in the echo area until replaced.
    pub last_message: Option<EchoMessage>,
    /// Append-only chronological ring of every echo
    /// (`set_message` call). The `:messages` ex-command
    /// opens a `*messages*` buffer rendered from this ring
    /// (the emacs `*Messages*` analogue). Bounded by
    /// `MessagesRing::capacity`. Wrapped in `Arc<Mutex<>>`
    /// so the boot-installed `MessagesLayer` (a
    /// `tracing::Layer` running on whatever thread emitted
    /// the event) can push into the same ring the App reads
    /// on the main thread for backlog seeding.
    pub messages: std::sync::Arc<std::sync::Mutex<MessagesRing>>,
    /// Receiver for [`lattice_runtime::MessagePushed`] events
    /// published by `set_message`. The runtime's per-tick
    /// drain coalesces bursts and rebuilds the `*messages*`
    /// buffer view once per frame.
    pub pending_message_event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<MessagePushed>>,
    /// Set by `Action::RedrawScreen` (`<C-l>`); the runtime
    /// clears this on its next frame after issuing a full
    /// terminal-clear so any leftover ANSI / stale glyph
    /// state gets repainted from scratch.
    pub pending_redraw: bool,
    /// Submitted `:` command history. Newest at the back.
    /// Bounded.
    pub command_history: Vec<String>,
    /// While in Command modal: index into
    /// [`Self::command_history`] of the entry currently
    /// shown (`None` = the user's in-progress text).
    pub command_history_cursor: Option<usize>,
    /// Snapshot of the user's typed `command_line` on the
    /// first Up so Down can return to it after walking
    /// through history.
    pub command_history_pending: Option<String>,
    /// Chord-capture overlay flag. Set when the user submitted
    /// a Chord-arg-required command with no value
    /// (`:describe-key<CR>` or the K.3.2 `<C-h>k` binding); the
    /// cmdline pre-fills with the command word + space and
    /// translation routes every key through
    /// `translate_command_chord_capture` so plain letters
    /// appear as chord tokens (`g` → `g`, `<C-c>` → `<C-c>`,
    /// `<Up>` → `<Up>`, ...). The renderer reads this through
    /// `auto_submit_hint` to draw the chord-capture cmdline
    /// hint. Reset on cancel / submit.
    ///
    /// K.3.5.fix (2026-06-03): the field's original purpose
    /// also included auto-submitting on the FIRST captured
    /// chord token — that auto-submit was dropped because
    /// chord arguments are sequences (`gg`, `<C-w>v`, `]e`,
    /// `<leader>fz`), not single chords. The user now types
    /// the full chord text and submits with `<CR>`. Field name
    /// kept for backward compat across the renderer / context
    /// boundaries; behavior is "chord-capture mode active,"
    /// no longer "auto-submit on chord."
    pub auto_submit_after_chord: bool,
    /// Tree-sitter language registry. Services the document
    /// buffer's `Syntax` and every `HelpBuffer` constructed
    /// by `:describe-*` / `:apropos` / `:keymap` (help
    /// bodies render with markdown highlighting + fenced-
    /// block injections sourced from this same registry).
    pub lang_registry: Arc<LangRegistry>,
    /// Per-document tree-sitter state. `None` when the
    /// document's language is `Plain` (no grammar bundled).
    /// Reparses run on a worker task; reads against the
    /// latest snapshot are wait-free via `ArcSwap`.
    pub syntax: Option<SyntaxHandle>,
    /// `text_version` last sent to the syntax handle's
    /// reparse channel. Used to skip republishing identical
    /// state when no text mutation has happened since the
    /// previous frame.
    pub last_parsed_text_version: u64,

    /// Tree-sitter-shaped edit deltas accumulated since the
    /// last `maybe_reparse_syntax` call. Pushed by
    /// `publish_document_changed` after each
    /// `Buffer::apply_edit`; drained by
    /// `maybe_reparse_syntax` and shipped to the syntax
    /// worker as `Vec<EditDelta>` for incremental reparse.
    pub pending_syntax_edits: Vec<EditDelta>,
    /// `text_version` the syntax worker's tree is known to
    /// be at. Sent as `from_version` on the next reparse
    /// request so the worker can verify edits apply to the
    /// correct tree baseline.
    pub last_synced_syntax_version: u64,
    /// Pane tree (DESIGN.md §5.9). Always represents the
    /// ACTIVE tab's panes — when switching tabs we
    /// `mem::swap` between this field and `tabs[target].panes`.
    /// Perf plan B.4: wrapped in [`Versioned`] so the panes
    /// sub-state cache in `build_render_state` can reuse its prior
    /// `Arc<PanesRenderState>` when the tree hasn't moved since the
    /// last publish. `Deref` reads (e.g. `editor.pane_tree.active()`)
    /// do NOT bump; `DerefMut` accesses (split/close/set_active)
    /// fire one `u64` increment.
    pub pane_tree: Versioned<PaneTree>,

    /// Issue #29 (2026-05-22): tab pages. Each `TabSlot` carries
    /// one tab's pane tree + optional label. The active tab's
    /// `panes` field is a default placeholder while live — its
    /// real tree sits on `editor.pane_tree`. Inactive tabs hold
    /// the full stashed tree. Always non-empty; default boot
    /// state is one tab whose pane_tree matches `editor.pane_tree`.
    /// Perf plan B.4.b: wrapped in [`Versioned`] so the tabs
    /// sub-state cache can detect when the tab list shape changes
    /// (push / remove / reorder). The composite cache key for
    /// `tabs` also includes `active_tab`, `pane_tree.version()`,
    /// and `buffers.version()` because label resolution reads
    /// across all four inputs.
    pub tabs: Versioned<Vec<lattice_core::ui::tab::TabSlot>>,
    /// Index of the active tab in `tabs`. Always valid (clamped
    /// on tab close).
    pub active_tab: usize,

    /// Issue #32 (2026-05-22): override for the next picker
    /// accept's open routing. Set by `<C-s>` / `<C-v>` / `<C-t>`
    /// chords on picker overlays before dispatching the accept;
    /// read + cleared by `apply_picker_outcome` for the file-
    /// targeting variants (OpenFile / SwitchBuffer / JumpInBuffer
    /// / JumpToLocation). `Default` for `<CR>`.
    pub picker_open_target: lattice_picker::OpenTarget,

    /// Issue #37 (2026-05-22): preview-restore handoff during
    /// picker accept. When the picker live-previews the
    /// selected candidate, the candidate's buffer is activated
    /// in the active pane — replacing the buffer that was
    /// there before the picker opened. If the accept then
    /// SPLITS or creates a new TAB, both halves inherit the
    /// preview and the original buffer is lost from the
    /// source pane.
    ///
    /// `do_picker_accept` writes the picker's `preview_origin`
    /// here at entry. `prepare_open_target_pane` consumes it
    /// via `mem::take` and, for non-Default targets,
    /// re-activates the origin buffer in the active pane
    /// BEFORE splitting / tab-creating so the source pane
    /// shows the original buffer afterwards.
    pub pending_picker_preview_origin: Option<lattice_core::BufferId>,

    /// T.12a: the theme to restore if the colorscheme picker is
    /// dismissed (`<Esc>`). Captured on the FIRST live preview as a
    /// `(palette, overrides)` snapshot of the theme active when the
    /// picker opened. `<Esc>` calls `ThemeRegistry::set_theme` with
    /// these to undo the preview; `<CR>` clears it (keeps the
    /// previewed theme). Mirrors `pending_picker_preview_origin` —
    /// `None` when no colorscheme preview is in flight.
    pub pending_theme_preview_restore:
        Option<(lattice_theme::Palette, Vec<(lattice_theme::ElementName, lattice_theme::StyleSpec)>)>,

    /// Handle to the per-document actor (or, in M.1+, a
    /// composing multibuffer handle) and its snapshot cache.
    /// M.0: typed as the [`ActiveDocument`] newtype around
    /// `Arc<dyn Document>` so the slot can hold either a
    /// `RopeDocumentHandle` or a `MultibufferDocumentHandle`
    /// without kind-branching at the use site. `Default::
    /// default()` populates this with a placeholder rope
    /// handle whose actor is already gone — production code
    /// overwrites the slot before any traffic flows.
    pub document: lattice_runtime::ActiveDocument,
    pub snapshot_cache: SnapshotCache,

    // DR.2 (decoration-retention): the `pane_highlights` /
    // `pane_highlight_keys` inactive-pane span cache + its
    // `refresh_pane_highlights` producer were retired here. Inactive
    // panes now render from their own retained per-pane `DisplayMatrix`
    // (the cells worker builds one for every visible pane), the same
    // canonical producer the active pane uses — one producer, zero
    // decoration recompute on focus change. See
    // `docs/dev/architecture/decoration-retention.md`.
    /// Active picker overlay. `None` outside picker mode.
    pub picker: Option<Picker>,
    /// Manual folds. v1 supports non-nested folds defined by line range.
    pub folds: Vec<lattice_core::Fold>,
    /// D.3.f.0 (2026-05-29): fold-provider registry. Holds the
    /// five built-in `Primary` providers (Manual / Indent /
    /// Markdown / Syntax / Lsp) and the list of registered
    /// `Overlay` providers, which are mode-owned (DX.3-C7):
    /// `diff-mode`'s `HunkFoldSource`, multibuffer's excerpt +
    /// file-boundary sources, all registered via the
    /// `FoldOverlayService` on mode activation).
    /// See `docs/dev/architecture/fold-architecture.md`.
    /// M.7: shared behind `Arc<Mutex>` so `FoldOverlayServiceImpl`
    /// can call `add_overlay`/`remove_overlay` from mode-activation
    /// context (outside `&mut Editor`) without blocking the UI thread.
    pub fold_registry: std::sync::Arc<std::sync::Mutex<crate::fold_provider::FoldRegistry>>,

    /// BC.3b: boot-lifetime tick-callback registration tokens handed off from
    /// the `BootContext` via `into_registrations()`. A subsystem `install(boot)`
    /// that wires an off-keystroke `boot.inbound::<T>` drain (the first is the
    /// Claude Code IDE peer's write bus) produces an RAII token here; holding it
    /// for the editor's lifetime keeps the drain registered (dropping it would
    /// unregister the drain mid-session). Empty when no subsystem installs an
    /// inbound/tick drain at boot. Never read — held purely to keep the drains
    /// alive; the leading `_` documents that.
    pub _boot_tick_registrations: Vec<TickCallbackRegistration>,
    /// D.4.a (2026-05-29): scroll-binding pane groups. Each
    /// entry binds a set of `(pane, buffer)` pairs through a
    /// pluggable `RowMapper`; propagation runs at
    /// `publish_render_state` tail. Membership keyed on the
    /// pair so buffer changes within a pane suspend the
    /// binding automatically. Subsystems (diff D.4.d, future
    /// `:set scrollbind`, zen mode, `:windo`) add/drop their
    /// groups around lifecycle. See
    /// `docs/dev/architecture/pane-groups.md`.
    pub pane_groups: Vec<crate::pane_group::PaneGroup>,
    /// D.8.e (2026-05-31): session key of the **singleton**
    /// `:diffthis` group, if any. `:diffthis` toggles per-buffer
    /// membership in this one group; other diff sessions
    /// (`:diffsplit`, AI-driven openDiff flows, future magit)
    /// run as independent `DiffSession`s and **don't** affect
    /// this field.
    ///
    /// State transitions (per
    /// `docs/dev/architecture/n-way-diff-membership.md` §6.2):
    /// - `None` → user runs `:diffthis` in any pane: create
    ///   N=1 dormant session keyed under the active buffer;
    ///   set this to `Some(active_buf)`.
    /// - `Some(g)` + active buffer not in g: extend the group
    ///   via `add_participant`; arity grows.
    /// - `Some(g)` + active buffer in g: shrink the group via
    ///   `remove_participant_buffer`; if arity drops to 0 the
    ///   subsystem auto-drops the session and this clears
    ///   back to `None`.
    /// D.0b (2026-06-08): id of the singleton identity-mapper
    /// pane group that backs `:set scrollbind`. `None` when no
    /// panes currently have `scrollbind=true` (the group is
    /// dropped when the last member opts out). Rebuilt by
    /// `rebuild_scrollbind_group` on every `scrollbind`
    /// option-change cascade.
    pub scrollbind_group_id: Option<lattice_core::ui::pane::PaneGroupId>,
    pub diffthis_group: Option<lattice_core::BufferId>,
    /// D.8.e (2026-05-31): pane members corresponding to each
    /// participant of the diffthis group, in `:diffthis`-call
    /// order. The first entry is the pane the FIRST
    /// `:diffthis` invocation came from; subsequent entries
    /// are appended as the user invokes `:diffthis` in new
    /// panes. Used to construct / reshape the pane group when
    /// arity transitions across 2 (need scroll-bind +
    /// fillers).
    ///
    /// Cleared in lockstep with `diffthis_group` — both reset
    /// to empty / `None` when the group drops to arity 0.
    pub diffthis_members: Vec<crate::pane_group::PaneGroupMember>,
    /// Picker source registry -- `:picker` source kinds.
    pub picker_registry: Arc<PickerRegistry>,
    /// Per-source MRU index that biases the picker's initial
    /// candidate ordering toward recently-accepted picks.
    pub picker_mru: PickerMruIndex,
    /// Optional on-disk persistence path for [`Self::picker_mru`].
    /// `None` for ephemeral / test installs.
    pub picker_mru_path: Option<PathBuf>,
    /// In-flight async picker init, if the active picker
    /// source's `init` returned a Future.
    pub pending_picker_init: Option<PendingPickerInit>,
    /// Live-picker query state -- present only when the
    /// active picker source has `spec().live == true`.
    pub live_picker_query: Option<LivePickerQueryState>,
    /// One-tick preview gate: set when the picker should
    /// render its candidate row preview into the auxiliary
    /// region. Cleared after the renderer reads it.
    pub previewing: bool,
    /// Shared typed-options registry (DESIGN.md §5.12).
    /// Every option's *current value* lives in here behind
    /// an `ArcSwap<T>`; `:set` parses against it; the
    /// customize buffer view (post-1.0) reads + writes
    /// through the same surface.
    pub config: Arc<ConfigRegistry>,
    /// Hot-path read cache for the option values.
    /// Repopulated by `rebuild_option_cache` after every
    /// `:set`. Accessor methods on App read this cached
    /// primitive directly (~1ns) instead of going through
    /// the registry's mutex + ArcSwap + downcast (~33ns).
    pub option_cache: OptionCache,
    /// Mode registry (M.1). Owns the catalogue of registered
    /// modes; activation / deactivation routes through here.
    pub mode_registry: Arc<ModeRegistry>,
    /// 2026-05-26: per-mode invocation runner table. Boot
    /// registers a runner function under each mode-id whose
    /// [`lattice_mode::Mode::invocation_runner`] returns
    /// `Some(id)`; [`Editor::run_invocation`] looks the runner
    /// up by walking the active modes on the active pane's
    /// buffer (minors first, then major) and calls the first
    /// match. Empty for modes that don't own dispatch
    /// (text-mode, completion-mode, semantic-tokens-mode, …).
    /// Replaces the hardcoded `match BufferKind` block in
    /// `run_invocation`; plugin-installed modes for plugin-
    /// installed buffer kinds extend the dispatcher through
    /// this map without touching host code.
    pub invocation_runners: HashMap<lattice_mode::ModeId, InvocationRunnerFn>,
    /// Typed service map subsystems hand off to modes so
    /// `Mode::on_activate` can pull subsystem handles via
    /// `ctx.service::<T>()`. Populated at boot; read-only
    /// after init.
    pub services: Arc<ServiceRegistry>,
    /// Per-`(buffer, mode)` Guard storage. Modes return an
    /// owned `Mode::Guard` from `on_activate`; the
    /// dispatcher stashes it here keyed by `(BufferId,
    /// ModeId)`. On deactivation the dispatcher drops the
    /// Guard, firing its `Drop` impl for synchronous
    /// cleanup. Wrapped in `Arc<Mutex<>>` because the
    /// spawned lifecycle task inserts from a worker thread.
    pub mode_guards: GuardStoreHandle,
    /// Per-buffer active modes (major + minors).
    ///
    /// Perf plan B.4: wrapped in [`Versioned`] so the modes
    /// sub-state cache can reuse its prior Arc across publishes
    /// when no mode toggle has fired. The `.insert` / `.remove`
    /// sites in dispatch autoref `&mut self.active_modes`, which
    /// bumps the version once per mutation.
    pub active_modes: Versioned<HashMap<BufferId, ActiveModes>>,
    /// Per-buffer mode-owned local state. Modes populate
    /// locals via the `BufferLocal` typed-map during
    /// `on_activate`; the App routes `&mut BufferLocals`
    /// into the registry's activation methods.
    ///
    /// Perf plan B.4: wrapped in [`Versioned`] for the same reason
    /// as `active_modes` — most publishes don't touch
    /// `buffer_locals`, so the deep typed-map clone in
    /// `build_render_state` can be avoided via Arc reuse.
    pub buffer_locals: Versioned<HashMap<BufferId, BufferLocals>>,
    /// Per-buffer mode-resolved options cache. Refreshed
    /// eagerly on mode toggle and option write.
    pub resolved_options: HashMap<BufferId, ResolvedOptions>,
    /// Buffer-local explicit overrides (`:setlocal foo=bar`)
    /// per buffer. Inputs to resolution; the resolver chains
    /// these with mode contributions before writing
    /// [`Self::resolved_options`].
    pub buffer_local_overrides: HashMap<BufferId, OptionOverrideSet>,
    /// Receiver for `OptionChanged` events published by the
    /// option-cascade pipeline. `Option` only because the
    /// field needs to be `take`-able so the drain method can
    /// borrow `&mut self` for cascade work while iterating
    /// the receiver. Always `Some` between calls.
    pub option_change_rx: Option<tokio::sync::mpsc::UnboundedReceiver<Event>>,
    /// Free-form help topic registry (DESIGN.md §5.11).
    /// `:help` reads from this; built-ins are sourced from
    /// `docs/user/*.md` at build time. Plugins / future LSP
    /// integrations register additional topics through the
    /// same registry.
    pub help_topics: Arc<HelpTopicRegistry>,
    /// T.4: builtin element ids interned once at boot from the
    /// `ThemeRegistryHandle` (looked up from [`Self::services`]).
    /// Snapshotted (Copy) into `RenderState` so a renderer read is
    /// `resolved.get(ids.<elem>)`. The registry handle itself lives
    /// only in `services` (it is `Arc<dyn ThemeRegistry>`, which has no
    /// `Default`, so it cannot be a field on this `derive(Default)`
    /// struct); `build_render_state` looks it up to snapshot
    /// `resolved()`.
    pub builtin_element_ids: crate::ui::theme::BuiltinElementIds,
    /// Buffer-level modal state machine (DESIGN.md §5.2).
    /// One of Normal / Insert / Visual / Op-pending /
    /// Command / Search / Replace.
    pub modal: ModalState,
    /// In-flight partial-chord stack from the trie. When the
    /// trie returns `LookupResult::Partial`, the dispatch
    /// layer appends the chord here; the next keystroke
    /// runs through the trie with this stack as prefix.
    /// Cleared on every non-`AbsorbPartialChord` action.
    pub partial_chord: Vec<KeyChord>,
    /// Grammar registry shared with the document actor by
    /// `Arc`. The actor calls `lattice_grammar::execute`
    /// with this registry from inside its own task. The App
    /// also reads it directly for the parser, completion
    /// pipeline, and introspection.
    pub registry: Arc<CommandRegistry>,
    /// In-process event bus (DESIGN.md §5.10). The App
    /// publishes editor lifecycle events
    /// (DocumentChanged, SelectionsChanged,
    /// ModalModeChanged, BeforeSave, DocumentSaved,
    /// BeforeQuit, OptionChanged) after observing the
    /// corresponding state transitions.
    pub event_bus: Arc<EventBus>,
    /// Built-in command-ids (`d`, `y`, `w`, `j`, …) -- the
    /// canonical `CommandId` values keymap registrations
    /// resolve against.
    pub builtins: Builtins,
    /// App-side typed action IDs (`CommandKind::Action`
    /// registrations). Each field is a `CommandId`
    /// resolving to an `ActionSpec` whose `apply` returns
    /// `Effect::AppAction(AppEffect::Foo)`.
    pub action_ids: ActionIds,
    /// Layered keymap registry (DESIGN.md §5.2.3).
    /// Populated at construction; the input dispatcher
    /// reads from it on every keystroke. Wait-free reads
    /// via internal `ArcSwap`; concurrent writes (mode
    /// push/pop, plugin registration, `:bind`) never stall
    /// the input path.
    pub keymap: KeymapHandle,
    /// `LayerId` of the active completion-popup minor-mode
    /// layer when the popup is open; `None` otherwise.
    /// Pushed / popped in lockstep with `insert_completion`.
    pub completion_popup_layer: Option<LayerId>,
    /// Pluggable completion pipeline (DESIGN.md §5.11.3). Owned by
    /// the host editor.
    pub completion_registry: lattice_completion::CompletionRegistry,
    /// Active command-line completion popup state (for `:` line).
    pub completion_state: Option<CompletionState>,
    /// Active **Insert-mode** completion popup (Phase 4.2.g).
    /// Distinct from `completion_state` (which drives the `:` line
    /// completion popup): this one floats over the buffer, shows
    /// candidates from sources (LSP / snippets / buffer-words /
    /// path / tree-sitter / plugin), and the host's keystroke
    /// dispatcher routes through a "completion-popup minor mode"
    /// keymap layer while it's `Some`. Behavioural spec lives in
    /// [`docs/dev/architecture/insert-completion.md`].
    pub insert_completion: Option<lattice_completion::InsertCompletionState>,
    /// Per-language snippet registry (Phase 4.2.g.4). Loaded
    /// at startup from bundled / user / project paths via
    /// `lattice-snippet::load`; the `gen:snippet` source
    /// consults it per-popup-trigger.
    /// CSM.5: held as `Arc<ArcSwap<...>>` so the mode-captured
    /// handle stays valid across `:reload-snippets`. Source reads
    /// load the current snapshot via `.load()` (wait-free); the
    /// reload path swaps the inner via `.store()` so the mode's
    /// next produce sees the fresh data.
    pub snippet_registry: Arc<arc_swap::ArcSwap<lattice_snippet::SnippetRegistry>>,
    /// SN.3b: shared cell holding the folded `snippet-mode`
    /// [`ActivationPolicy`](lattice_mode::ActivationPolicy).
    /// `register_snippet_modes` creates it (default `Global`) and the
    /// `snippet-mode` gate reads it on every `MajorEntered`; boot +
    /// the `snippet.activation` / `snippet.languages`
    /// `apply_option_cascade` arm fold config into it via
    /// `lattice_snippet::fold_activation_policy`.
    pub snippet_activation_policy: lattice_snippet::SnippetActivationPolicyHandle,
    /// SN.3c.0: app-lifetime registration tokens for modes'
    /// declarative *global* action handlers (`Mode::action_handlers()`,
    /// registered once at boot by
    /// [`crate::mode_action_handlers::register_mode_action_handlers`]).
    /// Held here so the handlers stay registered for the editor's
    /// whole lifetime; dropped at shutdown when `Editor` drops.
    pub global_action_handler_regs: Vec<lattice_mode::ActionHandlerRegistration>,
    /// Sidecar metadata for snippet candidates in the active
    /// insert-completion popup.
    /// CSM.5: retired. Snippet candidates now carry their stable
    /// `name` in the `Extension::payload` field; the accept path
    /// re-resolves the body via `Editor.snippet_registry.by_name`.
    /// Field kept as an empty Vec for one slice so callers that
    /// haven't migrated still compile; field deletion in a
    /// follow-up cleanup slice.
    pub insert_completion_snippet_meta: Vec<crate::state::SnippetCandidateMeta>,
    /// Per-session accept-count map for the insert-mode
    /// completion popup (Phase 4.2.g.5). Each accepted candidate
    /// bumps the counter for its `(text, kind)` pair; the ranker
    /// reads this map and adds a bounded bonus
    /// (`InsertRanker::FREQUENCY_BONUS_CAP`) so recently-accepted
    /// items bubble above tied peers next time.
    pub completion_accept_freq: HashMap<(String, lattice_completion::CandidateKind), u32>,
    /// TOML structural sections collected by the config loader at
    /// startup but not yet routed to their owners. Keyed by full
    /// dotted path (e.g. `"completion.per-language.markdown"`,
    /// `"plugin.rust-analyzer"`); value is the sub-table verbatim.
    /// Phase 4.2.g.5 (3b/3) drains the `completion.per-language.*`
    /// entries into `per_language_completion`; the plugin host
    /// (Phase 7) will drain `plugin.*`.
    pub pending_config_structural_sections: std::collections::BTreeMap<String, toml::Table>,
    /// Per-language insert-completion overrides (Phase 4.2.g.5
    /// (3b/3); spec at `docs/dev/architecture/insert-completion.md` §9).
    pub per_language_completion: HashMap<String, lattice_completion::PerLanguageOverrides>,
    /// `true` while the active insert-completion popup is in
    /// path-completion mode (Phase 4.2.g.6 (2/2)).
    pub completion_in_path_context: bool,
    /// Live snippet expansion (SN.2: relocated to a shared
    /// `SnippetSession` service so the `SnippetActiveMode`-owned
    /// `<Tab>` / `<S-Tab>` handlers can reach it). Active while a
    /// snippet is expanding; the session ends on `$0` consumption /
    /// `<Esc>` / cursor leaving the tabstop ranges. The same `Arc` is
    /// registered in `ServiceRegistry` under `SnippetSessionHandle`.
    pub snippet_session: lattice_snippet::SnippetSessionHandle,
    /// Session-backed minor modes reconciled on the active buffer
    /// each `sync_keymap_overlays` cycle (one entry per
    /// service-driven minor). Each pairs a predicate — reading a
    /// shared, mode-owned session service — with the minor's
    /// `ModeId`; the mode is active iff its predicate is true. Modes
    /// contribute these at boot (`active-snippet-mode` keys off the
    /// shared `SnippetSession`), so the generic overlay-sync carries
    /// no subsystem-specific `is_active()` literal
    /// (`feedback_mode_owns_its_surface`).
    pub session_backed_minors: Vec<SessionBackedMinor>,
    /// Per-language directories from which snippet packs are
    /// loaded on startup / `:reload-snippets` (Phase 4.2.g.4).
    pub snippet_dirs: Vec<PathBuf>,
    /// LIFO stack of snapshots taken every time the popup's content
    /// gets swapped in place by a help -> help link follow (e.g.
    /// `:describe-buffer` -> click `[text-mode](mode:text-mode)` ->
    /// `:describe-mode text-mode`). One popup buffer is reused
    /// across the navigation so jump-list / marks / search /
    /// register state stay coherent; this stack records what was
    /// in the buffer before each swap so `<C-o>` from inside the
    /// popup can restore the prior frame without leaving Help.
    pub popup_back_stack: Vec<crate::popup::PopupSnapshot>,
    /// Active buffer's cursor (DESIGN.md §5.1.1). Updated
    /// in lockstep with the active pane's stash so cross-
    /// pane jumps restore the right position.
    pub cursor: ProtoPosition,
    /// Sticky display-column target for `gj`/`gk` (vim's `w_curswant`).
    /// Stores the byte offset within the current wrap segment so
    /// consecutive display-line moves try to land at the same column.
    /// `None` between any non-display-line motion.
    pub goal_col: Option<u32>,
    /// First visible line in the viewport (0-based).
    pub scroll: u32,
    /// First visible display column in the viewport (0-based) —
    /// horizontal scroll for the active pane. Mirrors the active
    /// `PaneState::leftcol`; maintained by
    /// `ensure_cursor_horizontally_visible`. Always 0 when `wrap`
    /// is on (the body reflows, nothing is off-screen-right).
    pub leftcol: u32,
    /// Quit flag. The main loop reads this and tears down
    /// after the next paint. Set by `:q` / `:qa` / `Ctrl-C`
    /// / SIGINT.
    pub should_quit: bool,
    /// Last height we were drawn at; used by motion
    /// clamping and viewport scrolling. Updated by the
    /// renderer before each frame.
    pub viewport_height: u32,
    /// Last terminal width we were drawn at. Used by pane
    /// geometry (DESIGN.md §5.9 navigation needs to know
    /// which pane is horizontally adjacent). `None` until
    /// the renderer first records it.
    pub terminal_width: Option<u16>,
    /// Which buffer the input pipeline currently routes to.
    /// When a help overlay is open this is `Help`; otherwise
    /// `Document`. Denormalized from
    /// `pane_tree.active().buffer` -- updated in lockstep
    /// with the active pane.
    pub active_buffer: BufferKind,
    /// Stable id for the *active* document buffer. Mirrors
    /// the active pane's `buffer_id` whenever that pane
    /// holds a Document leaf.
    pub document_buffer_id: BufferId,
    /// Unified buffer registry (DESIGN.md §5.9). Holds
    /// every open buffer regardless of kind -- documents,
    /// file trees, future outline / diagnostics views.
    pub buffers: BufferRegistry,
    /// ML.0b-2: shared modeline element service (descriptor registry +
    /// content store, ArcSwap-backed). The SAME `Arc` is registered into
    /// `services` at boot so modes reach it via
    /// `ctx.service::<ModelineServiceHandle>()`; the host reads
    /// `modeline.snapshot()` each `build_render_state` into
    /// `RenderState.modeline_elements`. `Arc<ModelineService>` is
    /// `Default`, so `#[derive(Default)]` on `Editor` still holds (the
    /// boot literal overrides it with the registered instance).
    pub modeline: lattice_mode::ModelineServiceHandle,
    /// ML.3: actor-thread drain channel for [`lattice_mode::ModelineElementUpdate`]
    /// events pushed by modes/plugins over the event bus. Boot subscribes
    /// a sender; `drain_modeline_element_updates` (in `run_tick_pending`)
    /// applies each into `modeline`'s content store (single-writer). A
    /// separate boot subscription fires `async_landed` so a pushed update
    /// repaints off-keystroke (§12 wake). `Option` is `Default` (None), so
    /// `#[derive(Default)]` on `Editor` still holds.
    pub modeline_update_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<lattice_mode::ModelineElementUpdate>>,
    /// Cached `textDocument/selectionRange` chain for the
    /// smart-expansion operator.
    pub lsp_selection_chain: Option<LspSelectionChain>,
    /// Current step inside `lsp_selection_chain.ranges`.
    /// 0 = innermost; `chain.ranges.len() - 1` = outermost.
    pub lsp_selection_chain_index: usize,
    /// Cached `textDocument/documentHighlight` for the
    /// active buffer + symbol position.
    ///
    /// Phase 5.8.AF.5 / Slice 3b.0: the cache lives behind
    /// `Arc<ArcSwapOption<...>>` so the spawned task on the LSP
    /// runtime can store results directly when the response
    /// arrives — no channel, no UI-thread drain. Renderers read
    /// wait-free via `editor.render_state.load().lsp.document_highlights.load()`.
    pub lsp_document_highlights: std::sync::Arc<arc_swap::ArcSwapOption<DocumentHighlightCache>>,
    /// Cursor position at which the most recent
    /// `documentHighlight` request was issued.
    pub last_document_highlight_issue_cursor: Option<ProtoPosition>,
    /// Per-buffer cache of the last
    /// `textDocument/foldingRange` response.
    ///
    /// Phase 5.8.AF.5 / Slice 3b.1: `PerBufferCache<T>` so the
    /// spawned LSP request task can write results directly when
    /// the response arrives -- no channel, no UI-thread drain.
    /// Renderers read wait-free via
    /// `rs.lsp.folds.get_for(buffer_id)`.
    pub lsp_folds_cache: crate::per_buffer_cache::PerBufferCache<LspFoldsCache>,
    /// Phase 5.8.AF.5 / Slice 3b.1: the old drain
    /// (`drain_pending_folding_range`) called `recompute_folds()`
    /// inline after writing the cache so `self.folds` reflected
    /// the latest LSP response. The new shape has the task
    /// writing the cache off-thread; this tuple lets
    /// `maybe_request_folding_range` detect when the cache
    /// version has changed and trigger `recompute_folds()` on
    /// the renderer thread (where `&mut self.folds` is safe).
    /// `Some((buffer_id, document_version))` records the cache
    /// state last reflected into `self.folds`.
    pub last_recomputed_lsp_fold_version: Option<(BufferId, u64)>,
    /// Per-buffer `inlayHint` cache.
    ///
    /// Phase 5.8.AF.5 / Slice 3b.1: see `lsp_folds_cache` note.
    /// Renderers read wait-free via
    /// `rs.lsp.inlay_hints.get_for(buffer_id)`.
    pub lsp_inlay_hints_cache: crate::per_buffer_cache::PerBufferCache<LspInlayHintCache>,
    /// Per-buffer `documentLink` cache.
    /// Per-buffer `textDocument/documentLink` cache. Phase
    /// 5.8.AF.5 / Slice 3b.4: `PerBufferCache<T>` so the spawned
    /// LSP request task writes results directly. Renderers read
    /// via `rs.lsp.document_links.get_for(buffer_id)`.
    pub lsp_document_links_cache: crate::per_buffer_cache::PerBufferCache<LspDocumentLinksCache>,
    /// Per-buffer code-lens cache.
    /// Per-buffer `textDocument/codeLens` cache. Phase 5.8.AF.5
    /// / Slice 3b.3: `PerBufferCache<T>` so the spawned LSP
    /// request task writes results directly. Renderers read
    /// via `rs.lsp.code_lens.get_for(buffer_id)`.
    pub lsp_code_lens_cache: crate::per_buffer_cache::PerBufferCache<LspCodeLensCache>,
    /// Per-buffer `documentColor` cache.
    /// Per-buffer `textDocument/documentColor` cache. Phase
    /// 5.8.AF.5 / Slice 3b.4: `PerBufferCache<T>`.
    pub lsp_document_color_cache: crate::per_buffer_cache::PerBufferCache<LspDocumentColorCache>,
    /// Per-buffer semantic-tokens cache.
    /// Per-buffer cache of the last `textDocument/semanticTokens/*`
    /// response. Phase 5.8.AF.5 / Slice 3b.2: `PerBufferCache<T>`
    /// so the spawned LSP request task writes results (Items /
    /// Delta-applied / Empty) directly when the response arrives.
    /// Renderers read wait-free via
    /// `rs.lsp.semantic_tokens.get_for(buffer_id)`.
    pub lsp_semantic_tokens_cache: crate::per_buffer_cache::PerBufferCache<LspSemanticTokensCache>,
    /// Per-buffer pull-diagnostics cache (keyed
    /// `result_id`s for `Unchanged` short-circuit).
    /// Per-buffer `textDocument/diagnostic` (pull) cache.
    /// Phase 5.8.AF.5 / Slice 3b.5: `PerBufferCache<T>`.
    pub lsp_pull_diagnostics_cache:
        crate::per_buffer_cache::PerBufferCache<LspPullDiagnosticsCache>,
    // ---- LSP subsystem handles + log/progress channels ----
    pub lsp: LspSupervisorHandle,
    /// ML.3c: handle to the `lattice-lsp`-owned progress/status store
    /// (decision A — the accumulator relocated out of the host). The
    /// modeline forwarder writes it; the host reads it here only for
    /// `:lsp-progress-cancel` (in-flight cancellable tokens). `Arc<…>` is
    /// `Default`, so `#[derive(Default)]` on `Editor` still holds.
    pub lsp_progress_store: lattice_lsp::modeline::LspProgressStoreHandle,
    pub lsp_diagnostics: DiagnosticsLayer,
    /// L4a.2 (lsp-architecture.md §15): inline cursor-line
    /// diagnostic-summary idle gate. `inline_diag_line` is the line
    /// the gate is currently timing (the cursor line at arm time);
    /// `inline_diag_deadline` is the [`tokio::time::Instant`] at which
    /// its summary becomes visible (the actor's pinned sleep targets
    /// it); `inline_diag_visible` flips true when that deadline passes
    /// and back to false on re-arm (new cursor line) / Insert mode /
    /// `ui.diagnostics.inline = off`. The published summary in
    /// `DiagnosticsRenderState::inline_summary` is recomputed each
    /// `build_render_state` while visible, so diagnostics landing on
    /// the line after the gate fires refresh it for free. See
    /// `update_inline_diag_gate` / `fire_inline_diag_gate`.
    pub inline_diag_line: Option<u32>,
    pub inline_diag_deadline: Option<tokio::time::Instant>,
    pub inline_diag_visible: bool,
    pub lsp_logger: LspLogger,
    /// 4.4.l.2 / 5.8.AA.o / 5.8.AF.5: file-watcher service handle.
    /// `None` until the first actor with `workspace/didChangeWatchedFiles`
    /// capability is observed; at that point the actual watcher +
    /// notify event loop is spawned on the LSP runtime (see
    /// `crate::lsp_watcher::spawn_lsp_file_watcher_task`). Editor
    /// only sends `SyncSubscriptions` commands through this
    /// handle — no notify API calls, no event drains, ever run
    /// on the renderer's per-tick loop. Per paramount goal #4.
    pub lsp_watcher: Option<crate::lsp_watcher::LspFileWatcherHandle>,
    /// Editor-side memo of `server_id → CachedSubscription`. Used
    /// to detect whether the actor roster or its compiled
    /// subscriptions changed since the last `sync` call; only
    /// non-trivial diffs are pushed to the task. Mirrors the
    /// per-server map the task itself holds, but lives here so the
    /// "did anything change?" check stays a cheap fingerprint
    /// compare on the renderer's `refresh_lsp_file_watcher` path.
    pub lsp_watcher_subscriptions:
        std::collections::HashMap<String, crate::lsp_watcher::CachedSubscription>,
    /// Editor-side memo of currently-watched roots. Mirrors the
    /// task's set so we can skip sending `SyncSubscriptions` when
    /// nothing changed.
    pub lsp_watcher_watched_roots: std::collections::HashSet<std::path::PathBuf>,
    /// Phase 5.8.AF.5 / Slice 3a: renderer's wait-free read
    /// contract. Published by `Editor::publish_render_state` at
    /// the end of every `dispatch()` tick. Renderers load via
    /// `editor.render_state.load_full()` once per frame and read
    /// every per-frame field through the returned snapshot.
    pub render_state: std::sync::Arc<arc_swap::ArcSwap<crate::render_state::RenderState>>,
    /// Phase 5.8.AF.5 / Slice X2: wake signal for the overlay
    /// worker. `publish_render_state` fires
    /// `overlay_wake.0.notify_one()` at its tail so the worker
    /// re-evaluates the syntax inputs published into
    /// `RenderState.syntax` and re-buckets overlay quads on a cache
    /// miss. `Notify` coalesces — a burst of publishes wakes the
    /// worker once, which is what we want (it always reads the
    /// latest published inputs anyway).
    ///
    /// display-line B4.2: renamed from `highlight_wake`; the dead
    /// span/row prepaint cache the worker also fed was deleted.
    pub overlay_wake: OverlayWake,
    /// Perf plan B.2 slice B.2.a: parallel cell carrying the
    /// worker's per-row pre-bucketed static-overlay quads
    /// (doc_highlight / all_matches / substitute) for the active
    /// pane's visible window. The Arc identity lives on `Editor` so
    /// `build_render_state` clones it into every snapshot; the
    /// overlay worker writes directly and renderer peers read the
    /// latest write via the published Arc without a republish
    /// round-trip.
    pub syntax_static_overlay_quads_cell:
        std::sync::Arc<arc_swap::ArcSwap<crate::render_state::StaticOverlayQuads>>,
    /// S2.1 (2026-05-26): cell-grid renderer output cell. Same
    /// stability pattern as `syntax_static_overlay_quads_cell`: the Arc
    /// identity lives on `Editor` so `build_render_state` clones
    /// it into every snapshot; the cell-builder worker (S2.2+)
    /// holds a sibling clone and writes directly via
    /// `cell.store(new_matrix)`. Empty `CellMatrix` until the
    /// worker lands.
    pub cells_matrix_cell: std::sync::Arc<arc_swap::ArcSwap<lattice_cells::CellMatrix>>,
    /// D.4.d.0 (2026-05-29): per-document cells-matrix
    /// registry. Each visible buffer gets its own
    /// `Arc<ArcSwap<CellMatrix>>` so the cells worker can
    /// rebuild per buffer, and the renderer can pull the
    /// matrix matching each pane's buffer at paint time
    /// (load-bearing for side-by-side diff — D.4 — where
    /// two panes show different buffers simultaneously).
    ///
    /// The active-document entry is stored under
    /// `document_buffer_id` and **shares its Arc identity
    /// with [`Self::cells_matrix_cell`]** so the existing
    /// hot path (cells_worker writing through the field,
    /// renderer reading through `RenderState.cells.matrix`)
    /// stays bit-identical until the worker iteration
    /// upgrade lands in D.4.d.1.
    ///
    /// Inserts are lazy via
    /// [`Self::cells_matrix_for`] — a buffer's entry shows
    /// up the first time anything asks for its matrix.
    /// Pruning of stale entries is deferred until the
    /// worker actually consumes the registry; for now,
    /// entries accumulate without harm because
    /// [`arc_swap::ArcSwap`] over an empty `CellMatrix` is
    /// a cheap idle resource.
    pub cells_matrices: std::sync::Arc<
        std::sync::Mutex<
            std::collections::HashMap<
                lattice_core::BufferId,
                std::sync::Arc<arc_swap::ArcSwap<lattice_cells::CellMatrix>>,
            >,
        >,
    >,
    /// B2.1 (2026-06-04): per-line display-cache output cell — the
    /// substrate that retires `cells_matrix_cell`. Same stability
    /// pattern: the Arc identity lives on `Editor` so the publisher
    /// clones it into every snapshot; the worker (B2.2) holds a
    /// sibling clone and writes via `cell.store(new_matrix)`. Empty
    /// `DisplayMatrix` until the worker build path lands.
    /// See `docs/dev/architecture/display-line.md`.
    pub display_matrix_cell:
        std::sync::Arc<arc_swap::ArcSwap<crate::display_matrix::DisplayMatrix>>,
    /// B2.1 (2026-06-04): per-document display-matrix registry.
    /// Mirror of [`Self::cells_matrices`] for the per-line cache.
    /// Each visible buffer gets its own `Arc<ArcSwap<DisplayMatrix>>`
    /// so the worker can rebuild per buffer and the renderer can pull
    /// the matrix matching each pane's buffer at paint time. The
    /// active-document entry is boot-seeded to share its Arc identity
    /// with [`Self::display_matrix_cell`]; other entries are inserted
    /// lazily via [`Self::display_matrix_for`].
    pub display_matrices: std::sync::Arc<
        std::sync::Mutex<
            std::collections::HashMap<
                lattice_core::BufferId,
                std::sync::Arc<arc_swap::ArcSwap<crate::display_matrix::DisplayMatrix>>,
            >,
        >,
    >,
    /// D.2.d (2026-05-29): diff subsystem instance. Holds the
    /// per-buffer `DiffSession` registry, the routing inverse
    /// index, and the per-session lazy debouncer. Reads through
    /// the host's `BufferTextProvider` impl (wired post-D.2.c
    /// when the first consumer slice lands) for live-rope
    /// baselines / current sources. `:describe-diff` reads
    /// `diff_subsystem.build_describe_diff_content()` directly.
    /// See `docs/dev/architecture/diff-system.md` §3.4.
    pub diff_subsystem: std::sync::Arc<crate::diff::subsystem::DiffSubsystem>,
    /// D.0a.1 (2026-05-29): virtual-rows worker output cell.
    /// Same stability pattern as `cells_matrix_cell`: the Arc
    /// identity lives on `Editor` so `build_render_state`
    /// clones it into every snapshot; the
    /// `virtual_rows_worker` holds a sibling clone and writes
    /// directly via `cell.store(new_matrix)`. Empty
    /// `VirtualRowMatrix` until the first provider registers
    /// and the worker rebuilds.
    pub virtual_rows_matrix_cell:
        std::sync::Arc<arc_swap::ArcSwap<lattice_cells::VirtualRowMatrix>>,
    /// D.4.d.2.0 (2026-05-29): per-document virtual-rows
    /// matrix registry. Mirror of [`Self::cells_matrices`]
    /// for the displacing-virtual-row primitive. Each
    /// visible buffer that takes the virtual-rows path gets
    /// its own `Arc<ArcSwap<VirtualRowMatrix>>` so the
    /// worker (after D.4.d.2.1.b) can rebuild per buffer,
    /// and the renderer can pull the right matrix per pane
    /// at paint time (load-bearing for side-by-side diff
    /// fillers — D.4 — where two panes show different
    /// hunks' filler rows simultaneously).
    ///
    /// The active-document entry is stored under
    /// `document_buffer_id` and **shares its Arc identity
    /// with [`Self::virtual_rows_matrix_cell`]** so the
    /// existing hot path (virtual_rows_worker writing
    /// through the field, renderer reading through
    /// `RenderState.virtual_rows.matrix`) stays bit-identical
    /// until the worker iteration upgrade lands in
    /// D.4.d.2.1.b.
    ///
    /// Inserts are lazy via
    /// [`Self::virtual_rows_matrix_for`] — a buffer's entry
    /// shows up the first time anything asks for its matrix.
    /// Pruning of stale entries is deferred until the worker
    /// actually consumes the registry.
    pub virtual_rows_matrices: std::sync::Arc<
        std::sync::Mutex<
            std::collections::HashMap<
                lattice_core::BufferId,
                std::sync::Arc<arc_swap::ArcSwap<lattice_cells::VirtualRowMatrix>>,
            >,
        >,
    >,
    /// D.0a.1 (2026-05-29): wake signal for the virtual-rows
    /// worker. `publish_render_state` fires `notify_one()`
    /// after every dispatch tick (permit-style coalescing
    /// mirrors `cells_wake`). Provider state changes fire the
    /// same signal directly to wake the worker between
    /// dispatch ticks.
    pub virtual_rows_wake: VirtualRowsWake,
    /// D.0a.1 (2026-05-29): the provider registry the
    /// virtual-rows worker iterates on every wake. Consumers
    /// (D.3 inline diff deletion-block provider, M.2
    /// multibuffer excerpt-header provider) register their
    /// providers here at slice-mount time and unregister at
    /// teardown.
    pub virtual_row_providers:
        std::sync::Arc<crate::virtual_rows_worker::VirtualRowProviderRegistry>,
    /// D.3.a.1 (2026-05-29): the bus-subscription guard from
    /// `DiffSubsystem::bind`. Held for the editor's lifetime;
    /// its `Drop` unsubscribes the bus + aborts the drainer
    /// task on editor teardown. Stored behind `Option` so the
    /// `Editor::default()` path (used by tests that don't
    /// boot through `editor_boot`) can leave it unset without
    /// the bind machinery firing.
    pub diff_subscription_guard: Option<crate::diff::subsystem::DiffSubscriptionGuard>,
    /// D.3.a.1 (2026-05-29): per-session wake-forwarder
    /// `JoinHandle`s. `:diff` spawns a tokio task that awaits
    /// `DiffSession::publish_notify().notified()` and fires
    /// `VirtualRowsWake` on each publish; `:diffoff` aborts
    /// the task by `BufferId` and unregisters the provider.
    /// `tokio::sync::Mutex` is overkill here — mutation is
    /// `:diff`/`:diffoff` frequency, never per-frame.
    pub diff_forwarders: std::sync::Arc<
        std::sync::Mutex<
            std::collections::HashMap<lattice_core::BufferId, tokio::task::JoinHandle<()>>,
        >,
    >,
    /// I4 (Claude Code IDE peer, `openDiff`): host-drained inbound receiver for
    /// programmatic side-by-side diff requests. An off-thread producer (the IDE
    /// peer) `send`s a [`lattice_diff::ProgrammaticDiffRequest`] on the matching
    /// [`lattice_diff::ProgrammaticDiffBus`] (registered as a boot service); the
    /// `send` wakes the editor, and [`Self::drain_inbound_programmatic_diffs`]
    /// drains this receiver per tick, opening each diff on the actor thread. The
    /// open is irreducibly `&mut Editor` + lattice-diff types, so — like LSP
    /// `workspace/applyEdit` (`pending_apply_edit_rx`) — it is host-drained, not
    /// a mode-owned `Effect` handler.
    pub pending_programmatic_diff_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<lattice_diff::ProgrammaticDiffRequest>>,
    /// I4: per-session "save the current (right) side to this path on Accept"
    /// map, keyed by the session's primary `BufferId` (the proposed/right
    /// buffer). Set when [`Self::open_programmatic_diff`] registers a session;
    /// honored in `tear_down_single_diff_session` (a `DiffOutcome::Accept` writes
    /// the buffer here before firing the bound oneshot — the openDiff
    /// `FILE_SAVED` contract: the review *is* the save). Removed on teardown.
    pub programmatic_diff_accept_paths:
        std::collections::HashMap<lattice_core::BufferId, std::path::PathBuf>,
    /// I4 (openDiff) D-fix.1: per programmatic-diff-session pane teardown info,
    /// keyed by the session's primary (proposed) `BufferId`. Recorded by
    /// `open_programmatic_diff`; consumed by `finish_programmatic_diff_panes`
    /// on `:diff-accept` / `:diff-reject` to close the transient diff panes and
    /// return focus to the originating (`:claude`) pane. Populated/cleared in
    /// lockstep with `programmatic_diff_accept_paths`.
    pub programmatic_diff_panes:
        std::collections::HashMap<lattice_core::BufferId, ProgrammaticDiffPanes>,
    /// D-fix.5: per diff-participant buffer, the last `HunkIndex`
    /// revision its folds were recomputed against. `refresh_diff_folds`
    /// (run each tick on the diff-publish wake) consults this to skip
    /// buffers whose hunks haven't moved — so a diff session's unchanged
    /// + hunk folds refresh off-keystroke when the async recompute
    /// publishes, without re-folding on every unrelated wake. Keyed by
    /// the participant `BufferId` (each side tracked independently, since
    /// each folds its own slot). Stale entries are harmless; the buffer's
    /// `diff-mode` deactivation drops its fold sources, and the next
    /// recompute simply finds none.
    pub diff_fold_seen_revisions: std::collections::HashMap<lattice_core::BufferId, u64>,
    /// S2.1 (2026-05-26): wake signal for the cell-builder worker.
    /// `publish_render_state` fires `notify_one()` after every
    /// dispatch tick. The worker `notified().await`s; permit-style
    /// coalescing handles bursts.
    pub cells_wake: CellsWake,
    /// S2.4.b (2026-05-26): single-edit tracker for the
    /// cell-builder's incremental rebuild path. `Some(delta)`
    /// iff exactly one `apply_edit_blocking` (or LSP-applied
    /// edit) call has happened since the last
    /// `build_render_state` AND the previous publish cycle had no
    /// pending delta. Any second edit, batch, undo, redo, or
    /// other multi-edit path clears it back to `None` —
    /// conservatively forcing the worker to full-rebuild rather
    /// than risk applying a stale single-edit shift.
    /// `build_render_state` `take()`s and hands it to the cells
    /// substate.
    pub last_edit_for_cells: Option<lattice_cells::EditDelta>,
    /// Phase 5.8.AF.6 / Slice X1b: paint-request signal. The
    /// highlights worker fires `paint_request.notify_one()` after
    /// every `WorkerDecision::Recomputed` so renderer peers can
    /// schedule a paint even when no user input was in flight.
    /// `Notify` coalesces; bursts of recomputes wake the bridge
    /// once and a single paint covers the latest spans. The TUI
    /// peer's 100ms event-poll picks up the new cell naturally
    /// (no bridge needed); the GPUI peer spawns a foreground-
    /// executor future that awaits this Notify and calls
    /// `cx.notify()` to schedule a render.
    pub paint_request: std::sync::Arc<tokio::sync::Notify>,
    /// Slice B.1 (2026-06-03): "async work landed" wake. Fired by
    /// async completions that produce render-relevant state with no
    /// keystroke in flight — today the syntax reparse worker (via the
    /// `on_publish` Notify handed to each `SyntaxHandle`). The editor
    /// actor's loop `select!`s on this and runs `run_tick_pending` +
    /// `publish_render_state`, so an idle reparse repaints without
    /// waiting for the next key (closes the X1b idle-arrival gap for
    /// syntax; LSP-response tasks can fire the same Notify as a
    /// follow-up). Distinct from `paint_request`, which is the
    /// downstream UI-redraw signal fired after a worker publishes.
    pub async_landed: std::sync::Arc<tokio::sync::Notify>,
    pub lsp_log_event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<lattice_lsp::LspLogPushed>>,
    /// Merged user+project `lsp.*` config tree. BC.8b: shared
    /// (`Arc<ArcSwap<…>>`) so the mode-owned `workspace/configuration` inbound
    /// handler (`lattice_lsp::configuration::make_handler`) reads the *current*
    /// tree; the host re-`store`s it on config reload.
    pub lsp_config_tree: std::sync::Arc<arc_swap::ArcSwap<toml::Table>>,
    /// Perf plan B.4.b: wrapped in [`Versioned`] so the buffers
    /// sub-state cache can elide the per-publish HashMap clone.
    /// Mutators (`buffer_uris.insert/remove`) autoref `&mut`,
    /// fire `DerefMut`, and bump.
    pub buffer_uris: Versioned<HashMap<BufferId, lattice_lsp::Uri>>,
    // ---- LSP server-initiated channels ----
    pub pending_apply_edit_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<lattice_lsp::InboundApplyEdit>>,
    // BC.8b/BC.8c: `pending_configuration_rx` + `pending_show_document_rx`
    // removed — the `workspace/configuration` and `window/showDocument` buses
    // are now the generic `InboundBus`, drained per-tick through their mode-
    // owned handlers (`boot.inbound`), not host `Editor` receiver fields.
    pub pending_show_message_request_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<lattice_lsp::InboundShowMessageRequest>>,
    pub lsp_pending_show_message_requests: HashMap<u32, lattice_lsp::InboundShowMessageRequest>,
    pub lsp_show_message_request_queue: std::collections::VecDeque<u32>,
    pub lsp_next_show_message_request_id: u32,
    // ---- LSP per-feature request channels (rx + token pairs) ----
    pub pending_hover_rx: Option<tokio::sync::mpsc::UnboundedReceiver<HoverOutcome>>,
    pub pending_hover_token: Option<CancellationToken>,
    /// 2026-05-27: cursor + scroll captured at K-press time, consumed
    /// by `open_floating_popup` when the LSP response arrives so the
    /// hover popup anchors to the invocation site rather than wherever
    /// the cursor has drifted to. One-shot — cleared after the popup
    /// opens (or when the request is cancelled).
    pub pending_hover_anchor: Option<(lattice_protocol::position::Position, u32)>,
    pub pending_definition_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<Vec<lattice_lsp::lsp_types::Location>>>,
    pub pending_definition_token: Option<CancellationToken>,
    pub pending_nav_kind: Option<LspNavKind>,
    pub pending_references_rx: Option<tokio::sync::mpsc::UnboundedReceiver<ReferencesOutcome>>,
    pub pending_references_token: Option<CancellationToken>,
    pub pending_symbols_rx: Option<tokio::sync::mpsc::UnboundedReceiver<SymbolsOutcome>>,
    pub pending_symbols_token: Option<CancellationToken>,
    pub pending_format_rx: Option<tokio::sync::mpsc::UnboundedReceiver<FormatOutcome>>,
    pub pending_format_token: Option<CancellationToken>,
    pub pending_signature_help_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<SignatureHelpOutcome>>,
    pub pending_signature_help_token: Option<CancellationToken>,
    pub pending_completion_rx: Option<tokio::sync::mpsc::UnboundedReceiver<CompletionOutcome>>,
    pub pending_completion_token: Option<CancellationToken>,
    pub pending_completion_items: Option<Vec<CompletionItemRow>>,
    pub pending_moniker_rx: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
    pub pending_rename_rx: Option<tokio::sync::mpsc::UnboundedReceiver<RenameOutcome>>,
    pub pending_rename_token: Option<CancellationToken>,
    pub pending_code_action_rx: Option<tokio::sync::mpsc::UnboundedReceiver<CodeActionOutcome>>,
    pub pending_code_action_token: Option<CancellationToken>,
    pub pending_code_action_items: Option<Vec<CodeActionRow>>,
    pub pending_code_action_handle: Option<lattice_lsp::ServerHandle>,
    pub pending_selection_range_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<SelectionRangeOutcome>>,
    pub pending_selection_range_token: Option<CancellationToken>,
    pub pending_document_highlight_token: Option<CancellationToken>,
    // Phase 5.8.AF.5 / Slice 3b.0: `pending_document_highlight_rx`
    // retired -- the spawned task now writes directly into
    // `lsp_document_highlights` (`ArcSwapOption`) when the
    // response arrives. No channel, no drain.
    pub pending_folding_range_token: Option<CancellationToken>,
    // Phase 5.8.AF.5 / Slice 3b.1: `pending_folding_range_rx`
    // retired -- the spawned task writes directly into
    // `lsp_folds_cache` via `PerBufferCacheExt::insert_for`.
    pub pending_document_links_token: Option<CancellationToken>,
    // Phase 5.8.AF.5 / Slice 3b.4: `pending_document_links_rx`
    // retired -- spawned task writes directly into
    // `lsp_document_links_cache` via `PerBufferCacheExt::insert_for`.
    pub pending_code_lens_token: Option<CancellationToken>,
    // Phase 5.8.AF.5 / Slice 3b.3: `pending_code_lens_rx` retired
    // -- spawned task writes directly into `lsp_code_lens_cache`
    // via `PerBufferCacheExt::insert_for`.
    pub pending_code_lens_refresh_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<lattice_lsp::LspCodeLensRefresh>>,
    pub pending_code_lens_items: Option<Vec<lattice_lsp::lsp_types::CodeLens>>,
    pub pending_code_lens_server: Option<Arc<str>>,
    pub pending_document_color_token: Option<CancellationToken>,
    // Phase 5.8.AF.5 / Slice 3b.4: `pending_document_color_rx`
    // retired -- spawned task writes directly into
    // `lsp_document_color_cache` via `PerBufferCacheExt::insert_for`.
    pub pending_color_presentations: Option<Vec<lattice_lsp::lsp_types::ColorPresentation>>,
    pub pending_color_range: Option<lattice_lsp::lsp_types::Range>,
    pub pending_inlay_hint_token: Option<CancellationToken>,
    // Phase 5.8.AF.5 / Slice 3b.1: `pending_inlay_hint_rx`
    // retired -- the spawned task writes directly into
    // `lsp_inlay_hints_cache` via `PerBufferCacheExt::insert_for`.
    pub pending_semantic_tokens_token: Option<CancellationToken>,
    // Phase 5.8.AF.5 / Slice 3b.2: `pending_semantic_tokens_rx`
    // retired -- the spawned task writes directly into
    // `lsp_semantic_tokens_cache` via `PerBufferCacheExt::insert_for`
    // (or `remove_for` on result_id mismatch in the Delta path).
    pub pending_pull_diagnostics_token: Option<CancellationToken>,
    // Phase 5.8.AF.5 / Slice 3b.5: `pending_pull_diagnostics_rx`
    // retired -- spawned task writes directly into
    // `lsp_pull_diagnostics_cache` + `lsp_diagnostics` layer.
    pub pending_diagnostic_refresh_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<lattice_lsp::LspDiagnosticRefresh>>,
    pub pending_inlay_hint_refresh_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<lattice_lsp::LspInlayHintRefresh>>,
    /// 2026-06-03: buffers whose server sent
    /// `workspace/inlayHint/refresh` since their hints were last
    /// requested. `drain_inlay_hint_refresh` marks here instead of
    /// wiping `lsp_inlay_hints_cache`, so the previously-resolved
    /// hints stay rendered until the refetch lands (no
    /// disappear-then-reappear flicker — `feedback_decorations_update_in_place`).
    /// `maybe_request_inlay_hint` consults this to force a refetch
    /// even when the document version is unchanged, and clears the
    /// entry once it issues the request.
    pub inlay_refresh_pending: std::collections::HashSet<lattice_core::BufferId>,
    /// 2026-06-03: same shape as [`Self::inlay_refresh_pending`] for
    /// `workspace/semanticTokens/refresh`. The semantic-token colour
    /// overlay renders directly from `lsp_semantic_tokens_cache` every
    /// frame, so wiping the cache on refresh blanked all LSP colouring
    /// until the refetch landed (whole-viewport flicker per keystroke).
    /// `drain_semantic_tokens_refresh` marks here instead; the prior
    /// tokens keep rendering and `maybe_request_semantic_tokens` forces
    /// a refetch (delta from the retained `result_id`) that swaps them
    /// in place. (Pull diagnostics render from the persistent
    /// `DiagnosticsLayer` and code lenses are picker-only, so neither
    /// needs this — audited 2026-06-03.)
    pub semantic_tokens_refresh_pending: std::collections::HashSet<lattice_core::BufferId>,
    pub pending_semantic_tokens_refresh_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<lattice_lsp::LspSemanticTokensRefresh>>,
    pub pending_lsp_detach_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<lattice_lsp::events::LspBufferDetached>>,
    pub pending_mode_lifecycle_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<lattice_mode::ModeEvent>>,
    /// MA.2: receives `Event::MajorEntered` so the per-tick
    /// minor-activation resolver (`drain_minor_activation`) can
    /// auto-activate minors whose `ActivationPolicy` admits the
    /// just-entered major on this buffer's kind.
    pub pending_major_entered_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<lattice_protocol::Event>>,
    pub pending_insert_completion_lsp_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<InsertCompletionLspOutcome>>,
    pub pending_insert_completion_lsp_token: Option<CancellationToken>,
    pub pending_completion_resolve_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<CompletionResolveOutcome>>,
    pub pending_completion_resolve_token: Option<CancellationToken>,
    /// M.2.b.2 (2026-06-01): `RendererSignal`s accumulated by
    /// `impl ModeActivator for Editor` calls made through
    /// extension-crate code paths (`create_multibuffer_view` and
    /// future provider triggers). The trait surface returns `()`
    /// — keeping `RendererSignal` out of `lattice-mode` — so
    /// signals are stashed here until the App's dispatch loop
    /// drains them via
    /// [`Editor::drain_pending_renderer_signals`].
    pub pending_renderer_signals: Vec<RendererSignal>,
}

impl Editor {
    /// M.2.b.2 (2026-06-01): drain renderer signals accumulated
    /// by `impl ModeActivator for Editor` calls — extension-crate
    /// code (`lattice_multibuffer::create_multibuffer_view`,
    /// future provider triggers) drives activation through the
    /// trait surface that returns `()`, so the host loop must
    /// pull queued signals into the active `DispatchOutcome`
    /// after the call frame returns.
    #[must_use]
    pub fn drain_pending_renderer_signals(&mut self) -> Vec<RendererSignal> {
        std::mem::take(&mut self.pending_renderer_signals)
    }

    /// M.2.b.2 (2026-06-01): push a renderer-signal batch onto
    /// the trait-activator's pending queue. Called by
    /// [`crate::activator`]'s impl after each cascade returns.
    pub(crate) fn enqueue_renderer_signals(&mut self, mut signals: Vec<RendererSignal>) {
        self.pending_renderer_signals.append(&mut signals);
    }

    /// 2026-05-26: register an invocation-runner function under
    /// the mode-id its owning [`lattice_mode::Mode`] declares via
    /// [`lattice_mode::Mode::invocation_runner`]. Called from
    /// `Editor::boot` for each built-in runner
    /// (`run_help_invocation` / `run_oil_invocation` /
    /// `run_file_tree_invocation` / `run_terminal_invocation`);
    /// plugins (post Phase 7) reuse this entry point for the
    /// modes they install. Overwrites silently on duplicate
    /// registration — boot order is the single writer.
    pub fn register_invocation_runner(
        &mut self,
        id: lattice_mode::ModeId,
        runner: InvocationRunnerFn,
    ) {
        self.invocation_runners.insert(id, runner);
    }

    /// 2026-05-26: resolve the invocation runner for `buffer_id`
    /// by walking the active modes (minors most-recently-
    /// activated first, then major) and returning the first
    /// runner whose mode declared
    /// [`lattice_mode::Mode::invocation_runner`] and has a
    /// registered function on `self.invocation_runners`.
    /// Mirrors [`crate::pane_render::resolve_pane_render_mode`]
    /// — same walk, different table. Returns `None` when no
    /// active mode owns dispatch (Document panes today).
    pub fn resolve_invocation_runner(
        &self,
        buffer_id: lattice_core::BufferId,
    ) -> Option<InvocationRunnerFn> {
        let modes = self.active_modes.get(&buffer_id)?;
        for &minor_id in modes.minors().iter().rev() {
            let mode = self.mode_registry.get(minor_id)?;
            if let Some(runner_id) = mode.invocation_runner()
                && let Some(runner) = self.invocation_runners.get(&runner_id)
            {
                return Some(*runner);
            }
        }
        let major_id = modes.major()?;
        let mode = self.mode_registry.get(major_id)?;
        let runner_id = mode.invocation_runner()?;
        self.invocation_runners.get(&runner_id).copied()
    }

    /// D.4.d.0 (2026-05-29): lazy port into the per-document
    /// [`Self::cells_matrices`] registry. Returns the matrix
    /// cell for `buffer_id`, inserting an empty
    /// `Arc<ArcSwap<CellMatrix>>` on first ask.
    ///
    /// Idempotent: every call for the same `buffer_id`
    /// returns the same `Arc` identity so renderer reads and
    /// worker writes stay coherent.
    ///
    /// The active document's entry is seeded at boot to
    /// share its `Arc` with [`Self::cells_matrix_cell`], so
    /// callers that resolve the active doc through either
    /// surface land on the same cell.
    pub fn cells_matrix_for(
        &self,
        buffer_id: lattice_core::BufferId,
    ) -> std::sync::Arc<arc_swap::ArcSwap<lattice_cells::CellMatrix>> {
        let mut map = self
            .cells_matrices
            .lock()
            .expect("cells_matrices mutex poisoned");
        map.entry(buffer_id)
            .or_insert_with(std::sync::Arc::default)
            .clone()
    }

    /// B2.1 (2026-06-04): lazy port into the per-document
    /// [`Self::display_matrices`] registry. Mirror of
    /// [`Self::cells_matrix_for`] for the per-line display cache.
    /// Returns the matrix cell for `buffer_id`, inserting an empty
    /// `Arc<ArcSwap<DisplayMatrix>>` on first ask.
    ///
    /// Idempotent: every call for the same `buffer_id` returns the
    /// same `Arc` identity so renderer reads and worker writes stay
    /// coherent. The active document's entry is boot-seeded to share
    /// its `Arc` with [`Self::display_matrix_cell`].
    pub fn display_matrix_for(
        &self,
        buffer_id: lattice_core::BufferId,
    ) -> std::sync::Arc<arc_swap::ArcSwap<crate::display_matrix::DisplayMatrix>> {
        let mut map = self
            .display_matrices
            .lock()
            .expect("display_matrices mutex poisoned");
        map.entry(buffer_id)
            .or_insert_with(std::sync::Arc::default)
            .clone()
    }

    /// D.4.d.2.0 (2026-05-29): lazy port into the per-document
    /// [`Self::virtual_rows_matrices`] registry. Mirror of
    /// [`Self::cells_matrix_for`] for the virtual-row pipeline.
    /// Returns the matrix cell for `buffer_id`, inserting an
    /// empty `Arc<ArcSwap<VirtualRowMatrix>>` on first ask.
    ///
    /// Idempotent: every call for the same `buffer_id`
    /// returns the same `Arc` identity so renderer reads and
    /// worker writes stay coherent.
    ///
    /// The active document's entry is seeded at boot to
    /// share its `Arc` with
    /// [`Self::virtual_rows_matrix_cell`], so callers that
    /// resolve the active doc through either surface land
    /// on the same cell.
    pub fn virtual_rows_matrix_for(
        &self,
        buffer_id: lattice_core::BufferId,
    ) -> std::sync::Arc<arc_swap::ArcSwap<lattice_cells::VirtualRowMatrix>> {
        let mut map = self
            .virtual_rows_matrices
            .lock()
            .expect("virtual_rows_matrices mutex poisoned");
        map.entry(buffer_id)
            .or_insert_with(std::sync::Arc::default)
            .clone()
    }
}
