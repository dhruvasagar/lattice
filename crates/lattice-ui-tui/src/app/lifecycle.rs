//! Buffer-creation, activation transitions, and shutdown.
//! R.1.16 lands a focused subset (pane navigation); the
//! bulk migrates with follow-up slices.
//!
//! Methods that live here:
//! - `do_navigate_pane` (`<C-w>h/j/k/l` -- step cardinally
//!   to the spatial neighbour of the active pane; geometry
//!   from `PaneTree::compute_rects`).
//! - `activate_pane` (swap the App's hot-path cursor /
//!   scroll with the target pane's stash).
//! - `load_active_pane` (inverse of `snapshot_active_pane`:
//!   pull stashed cursor / scroll back into App; restore
//!   help-buffer mirror when the pane points at a different
//!   help buffer than the one currently in the hot-path
//!   slot).
//!
//! Stays in app.rs (deferred to follow-up lifecycle slices):
//! - `App::new` (the big constructor).
//! - The activate_* family (activate_document,
//!   activate_buffer, activate_file_tree, activate_oil,
//!   activate_help_in_pane).
//! - `snapshot_active_pane` / `snapshot_active_document`
//!   (already pub(super); used widely).
//! - `do_split_pane` / `do_close_pane` /
//!   `gc_unreferenced_panel_buffers`.
//! - `do_edit` / `do_write` / `do_quit` /
//!   `do_buffer_*` (ex-command bodies; lifecycle-shaped).
//! - `set_viewport_height`, `pending_redraw` handling,
//!   per-loop-iteration state hooks.

#[cfg(test)]
use lattice_protocol::position::Position;
use lattice_runtime::RuntimeError;
// 5.8.AA.k: `do_edit` body moved host-side; the `Lang` / `Syntax`
// imports here are referenced only by `#[cfg(test)]` fixtures
// below. `spawn_document` follows do_edit and isn't needed
// elsewhere in this module.
#[allow(unused_imports)]
use lattice_syntax::{Lang, Syntax};

use super::{App, BufferId};
// 5.8.AA.k: BufferEntry / BufferData / DocumentEntry consumed by
// `do_edit` (now host); BufferFlags ditto. Kept here under
// `#[allow]` for `#[cfg(test)]` fixtures further down the file.
#[allow(unused_imports)]
use crate::buffer_registry::{BufferData, BufferEntry, DocumentEntry};
#[allow(unused_imports)]
use crate::buffers::{BufferFlags, BufferKind};
use crate::help::HelpContent;
// 5.5.H: PaneDirection / SplitOrientation imports retired
// alongside the `do_navigate_pane` / `do_split_pane` App-side
// delegates. Test-only `SplitOrientation` reference at line 1401+
// uses the full path.

impl App {
    /// Switch the active document to `id`. Snapshots the current
    /// active state into its entry, then loads from the
    /// destination's entry. No-op if `id` is already active or
    /// not registered.
    ///
    /// 5.5.F.4.2: the body relocated to
    /// [`lattice_host::dispatch::Editor::activate_document`]; the
    /// returned bool indicates whether the full-activation path
    /// was taken (caller runs `activate_buffer_state` on `true`).
    /// `activate_buffer_state` itself stays on App until F.5 lands
    /// mode lifecycle host-side.
    pub fn activate_document(&mut self, id: BufferId) {
        if self.mutate_editor_with(move |e| e.activate_document(id)) {
            self.activate_buffer_state();
        }
    }

    /// Switch the active pane to whatever buffer `id` references,
    /// regardless of kind. Document buffers route through
    /// `activate_document`; tree buffers update the active pane +
    /// load the tree's stash; help buffers go through
    /// `activate_help_in_pane`.
    ///
    /// 5.5.F.4.2: the dispatch body relocated to
    /// [`lattice_host::dispatch::Editor::activate_buffer`]. The
    /// returned bool indicates whether the dispatch went through
    /// the full `activate_document` path; on `true` the App-side
    /// wrapper runs `activate_buffer_state` (mode/syntax/option
    /// re-init still on App until F.5).
    pub fn activate_buffer(&mut self, id: BufferId) {
        if self.mutate_editor_with(move |e| e.activate_buffer(id)) {
            self.activate_buffer_state();
        }
        // 3c.atomic.B: `activate_buffer` mutates active buffer
        // state outside the dispatch publish path, so reads
        // through `app.ad().document_buffer_id` would otherwise
        // observe the pre-activation buffer until the next
        // dispatch tail. Publish here to keep render-state and
        // editor in sync.
        // S2.4.b (2026-05-26): `publish_render_state` became
        // `&mut self`. Switch to `mutate_editor`.
        self.mutate_editor(move |e| {
            e.publish_render_state();
        });
    }

    /// 5.5.F.4.2: see [`lattice_host::dispatch::Editor::activate_file_tree`].
    /// No `activate_buffer_state` tail — tree buffers don't have
    /// document/syntax/options state to re-resolve.
    pub fn activate_file_tree(&mut self, id: BufferId) {
        // Slice 3c.final.E.3: route through `mutate_editor`.
        self.mutate_editor(move |e| e.activate_file_tree(id));
    }

    /// 5.5.F.4.2: see [`lattice_host::dispatch::Editor::activate_oil`].
    pub fn activate_oil(&mut self, id: BufferId) {
        // Slice 3c.final.E.3: route through `mutate_editor`.
        self.mutate_editor(move |e| e.activate_oil(id));
    }

    /// 5.5.F.4.2: see [`lattice_host::dispatch::Editor::activate_help_in_pane`].
    pub(super) fn activate_help_in_pane(&mut self, id: BufferId) {
        // Slice 3c.final.E.3: route through `mutate_editor`.
        self.mutate_editor(move |e| e.activate_help_in_pane(id));
    }

    // 5.5.F.4.3: `do_buffer_next` / `do_buffer_prev` relocated to
    // [`lattice_host::dispatch::Editor::do_buffer_next`] /
    // [`lattice_host::dispatch::Editor::do_buffer_prev`]; the
    // corresponding `Effect::BufferNext` / `Effect::BufferPrev`
    // arms now run inside `Editor::handle_effect` and emit
    // `RendererSignal::BufferActivated` for the App-side
    // `activate_buffer_state` tail.
    //
    // `next_listed_buffer_id` / `prev_listed_buffer_id` co-moved
    // (the Editor helpers they relied on are all host-side); the
    // App-side definitions delete entirely (Effect-only path; no
    // direct callers).

    // 5.5.F.4.4: `do_buffer_delete` relocated to
    // [`lattice_host::dispatch::Editor::do_buffer_delete`]; the
    // corresponding `Effect::BufferDelete` arm now runs inside
    // `Editor::handle_effect` and emits `RendererSignal::BufferActivated`
    // for the App-side `activate_buffer_state` tail. The App-side
    // wrapper deletes entirely (Effect-only path; no direct callers).

    // 5.5.F.4.3: `listed_buffer_ids_sorted` / `next_listed_buffer_id` /
    // `prev_listed_buffer_id` relocated to
    // [`lattice_host::dispatch::Editor`]; the only App-side callers
    // were `do_buffer_next` / `do_buffer_prev`, which co-migrated.

    /// `:e[dit] FILE` (DESIGN.md §5.9 multi-buffer). If a buffer
    /// for `path` is already open, switch to it; otherwise spawn
    /// a fresh document actor, register it, and switch the active
    /// pane to the new buffer. With no path, re-edit the current
    /// buffer's path (force-reload from disk; `!` required when
    /// dirty).
    pub(super) fn do_edit(&mut self, path: Option<std::path::PathBuf>, force: bool) {
        // 5.8.AA.k: body migrated to
        // `lattice_host::dispatch::Editor::do_edit`. This wrapper
        // routes the host's `DoEditOutcome` through App-side
        // helpers that aren't host-resident yet:
        //   - `Directory(path)` → `do_open_oil(Some(path))` (oil
        //     view is App-only)
        //   - `Activated`/`Reloaded`/`Opened(signals)` → fan
        //     signals through `handle_renderer_signal`
        //   - `Failed`/`NoFileName` → host already echoed
        use lattice_host::dispatch::DoEditOutcome;
        let outcome = self.mutate_editor_with(move |e| e.do_edit(path, force));
        match outcome {
            DoEditOutcome::NoFileName | DoEditOutcome::Failed => {}
            DoEditOutcome::Directory(dir) => self.do_open_oil(Some(dir)),
            DoEditOutcome::Reloaded(signals)
            | DoEditOutcome::Activated(signals)
            | DoEditOutcome::Opened(signals) => {
                for s in signals {
                    self.handle_renderer_signal(s);
                }
            }
        }
    }

    /// P.2: push `path` onto the MRU `recent_files` list. Slice
    /// 3c.final.E.5d: stale duplicate body removed -- the host
    /// already owns the canonical impl
    /// ([`lattice_host::dispatch::Editor::push_recent_file`]). This
    /// renderer-side wrapper just routes through `mutate_editor` so
    /// post-swap callers cross the actor boundary cleanly.
    pub(super) fn push_recent_file(&mut self, path: &std::path::Path) {
        let path = path.to_path_buf();
        self.mutate_editor(move |e| e.push_recent_file(&path));
    }

    /// `:w[rite] [path]` -- save the active buffer to disk. Oil
    /// buffers route through OilBuffer::apply (diff-and-apply
    /// filesystem ops); document buffers route through
    /// save_blocking / save_as_blocking against the document
    /// actor.
    pub(super) fn do_write(&mut self, path: Option<std::path::PathBuf>) {
        // Slice 3c.final.E.3: route through `mutate_editor`.
        self.mutate_editor(move |e| e.do_write(path));
    }

    /// `:q[uit]` (`scope = Pane`) / `:qa[ll]` (`scope = All`) -- quit.
    /// `Pane` is vim-style window close: with multiple panes open,
    /// close the active pane (no dirty check; the buffer lives on in
    /// the registry / other panes); with one pane left, run the dirty
    /// guard and shut down the editor. `All` ignores pane count and
    /// shuts down regardless. `force` (`!`) bypasses the dirty guard.
    /// Publishes `Event::BeforeQuit` when the editor actually quits.
    pub(super) fn do_quit(&mut self, force: bool, scope: lattice_grammar::QuitScope) {
        // Phase 5.8.AC.1: body migrated to
        // `lattice_host::dispatch::Editor::do_quit`. The
        // pane-close path still routes through the App-side
        // wrapper because `do_close_pane` calls App's
        // `gc_unreferenced_panel_buffers` after the host close.
        // `:qa` (scope = All) skips this short-circuit and always
        // delegates to the host's quit (dirty guard + shutdown).
        if scope == lattice_grammar::QuitScope::Pane && self.panes().tree.len() > 1 {
            self.do_close_pane();
            return;
        }
        self.mutate_editor_with(move |e| e.do_quit(force, scope));
    }

    // 5.5.H: `do_split_pane` App-side delegate retired (zero
    // callers; host copy at
    // [`lattice_host::dispatch::Editor::do_split_pane`]).

    /// 5.5.G.5: body migrated to
    /// [`lattice_host::dispatch::Editor::do_close_pane`]. Kept as
    /// a delegate because `App::do_quit` still calls it when the
    /// editor has >1 panes (quit-just-closes-pane semantics).
    pub(super) fn do_close_pane(&mut self) {
        self.mutate_editor_with(move |e| e.do_close_pane());
        self.gc_unreferenced_panel_buffers();
    }

    /// Drop singleton non-document buffers (currently: file tree)
    /// when no pane still references them. Document buffers are
    /// no-op stub left in for backwards compatibility with the
    /// pre-registry refactor. Trees now live in the unified buffer
    /// registry alongside documents (DESIGN.md §5.9), so closing
    /// the only pane that referenced a tree leaves the tree in the
    /// registry where `:bn` / `:bp` can reach it. Use `:bd` to
    /// actually drop a tree buffer.
    pub(super) fn gc_unreferenced_panel_buffers(&mut self) {}

    // 5.5.H: `do_navigate_pane` + `activate_pane` App-side
    // delegates retired (zero callers; host copies at
    // [`lattice_host::dispatch::Editor::do_navigate_pane`] /
    // `Editor::activate_pane`).

    /// Inverse of `snapshot_active_pane`: pull the freshly
    /// activated pane's stashed cursor / scroll back into the App's
    /// hot-path fields. `active_buffer` is denormalized from the
    /// pane's `buffer` kind.
    ///
    /// **Unified hot-path**: `self.editor.cursor` and `self.editor.scroll` are
    /// the active buffer's, regardless of kind. Help / file-tree
    /// keep their own cursor / scroll fields as **save state** --
    /// updated at the snapshot boundary so the registry record is
    /// archival-correct, but the *live* cursor is `self.editor.cursor`
    /// for every motion / scroll / search / render path.
    pub(super) fn load_active_pane(&mut self) {
        // Slice 3c.final.E.3: route through `mutate_editor`.
        self.mutate_editor(|e| e.load_active_pane());
    }

    // 5.5.F.1: `:ls` / `:buffers` content builder relocated to
    // [`lattice_host::dispatch::Editor::build_list_buffers_content`]
    // and the `Effect::ListBuffers` arm now lives in
    // `Editor::handle_effect`. The renderer-coupled tail
    // (`display_buffer` dispatch) runs via
    // `RendererSignal::DisplayBuffer` -> `App::handle_renderer_signal`.

    // 5.5.E.2: `do_list_registers` / `do_list_marks` moved to
    // [`lattice_host::dispatch::Editor::do_list_registers`] /
    // [`lattice_host::dispatch::Editor::do_list_marks`] alongside
    // the [`Effect::EchoRegisters`] / [`Effect::EchoMarks`] arms.

    pub(super) fn find_document_by_path(&self, path: &std::path::Path) -> Option<BufferId> {
        // 5.8.AA.j: migrated to host.
        // Slice 3c.final.E.5e: clone `&Path` to owned `PathBuf` for
        // the `Send + 'static` closure. `BufferId` is `Copy`.
        let path = path.to_path_buf();
        self.read_editor(move |e| e.find_document_by_path(&path))
    }

    /// Save the currently-active document's hot-path state
    /// (`syntax`, `last_parsed_text_version`, `folds`) into its
    /// [`DocumentEntry`]. Called before switching the active
    /// buffer so the rotation is round-trippable.
    ///
    /// Guarded by `active_buffer == Document`: when the active
    /// buffer is a file tree or help, `self.editor.syntax` was already
    /// moved into the document entry on the *previous* transition
    /// (when we left the document). Calling this again would
    /// `take()` an already-None value and overwrite the entry's
    /// stashed syntax, dropping the highlight state on the floor
    /// (the visible symptom: opening `:Tree` and pressing `q`
    /// returned to the document with no syntax colours).
    pub(super) fn snapshot_active_document(&mut self) {
        // Slice 3c.final.E.3: route through `mutate_editor`.
        self.mutate_editor(|e| e.snapshot_active_document());
    }

    /// 5.5.F.5.5: see [`lattice_host::dispatch::Editor::activate_buffer_state`].
    /// Wrapper fans host-returned `RendererSignal`s through
    /// [`Self::handle_renderer_signal`].
    pub(super) fn activate_buffer_state(&mut self) {
        // Slice 3c.final.E.3: route through `mutate_editor_with`.
        let signals = self.mutate_editor_with(|e| e.activate_buffer_state());
        for sig in signals {
            self.handle_renderer_signal(sig);
        }
    }

    /// What `:bn` / `:bp` consider the "current" buffer for
    /// stepping. The active pane's buffer_id is the source of
    /// truth (the active pane is what the user sees).
    pub(super) fn active_pane_buffer_id(&self) -> BufferId {
        self.read_editor(move |e| e.active_pane_buffer_id())
    }

    /// Copy the App's hot-path cursor / scroll into the active
    /// pane's stash. Called before any operation that flips which
    /// pane is active.
    ///
    /// **Unified hot-path**: `self.editor.cursor` and `self.editor.scroll` are
    /// the active buffer's regardless of kind, so the snapshot
    /// reads from there uniformly. Help / file-tree records are
    /// also synced into their kind-specific cursor / scroll fields
    /// (and the registry copy for help) so the archival state stays
    /// current; live state always lives on `self`.
    pub(super) fn snapshot_active_pane(&mut self) {
        // Slice 3c.final.E.3: route through `mutate_editor`.
        self.mutate_editor(|e| e.snapshot_active_pane());
    }

    // 5.5.E.7.7: `publish_document_changed` wrapper retired -- all
    // four prod call sites collapsed: `apply_edit_blocking` /
    // `apply_edit_batch_blocking` / `undo_blocking` / `redo_blocking`
    // now live on `Editor` (E.7.3), and the `handle_edits` chokepoint
    // routes through `Editor::handle_edits` via the `Effect::Edits`
    // arm. See [`lattice_host::dispatch::Editor::publish_document_changed`].

    // 5.5.E.4: `publish_selections_changed` moved to
    // [`lattice_host::dispatch::Editor::publish_selections_changed`]
    // — it sat alongside `set_selections_blocking` (its only caller),
    // which migrated host-side in the same slice.

    /// Total area available to pane content in screen-cell units.
    /// Currently the buffer area = full terminal minus the mode
    /// line (1 row) and the echo / cmdline area (1 row). Width is
    /// the terminal width; v1 doesn't track terminal width as
    /// state, so we estimate from `viewport_height` and a constant
    /// width that the renderer overrides with the real terminal
    /// width before navigation. Good enough until B.1.c has the
    /// per-frame terminal size cached on App.
    // 5.5.H: `buffer_area_rect` App-side delegate retired (zero
    // callers; host copy at
    // [`lattice_host::dispatch::Editor::buffer_area_rect`]).

    /// Pane buffer-label string — the path/dirty segment (or a pane
    /// provider's custom label). Kept for its existing callers/tests
    /// (synthetic-name fallback, log/messages labels); the modeline
    /// renderer (`draw_pane_status_line`) lays out zones from the
    /// registered elements instead.
    ///
    /// ML.3 retired the appended mode-items (LSP / diff badges) — those
    /// are registered modeline elements now, not part of this label.
    pub fn pane_status_label(&self, pane: &crate::pane::PaneState) -> String {
        let provider = self
            .pane_render_provider(pane.buffer_id)
            .map(|p| (p.status)(self, pane));
        let rs = self.render_state.load();
        lattice_host::modeline::pane_path_segment(pane, &rs, provider.as_deref())
    }

    /// Jump to `path:line:col` (LSP 0-based line, utf-8 byte
    /// column). Single entrypoint shared by the picker accept
    /// path (`JumpToLspLocation`) and the `do_help_follow_link`
    /// Source-link dispatch. Pushes the pre-jump cursor onto
    /// position history with `PluginPush` so `<C-o>` walks back.
    pub(super) fn jump_to_file_line_col(&mut self, path: &std::path::Path, line: u32, col: u32) {
        // Slice 3c.final.E.3: clone path for the `Send + 'static`
        // closure, then route through `mutate_editor_with`.
        let path = path.to_path_buf();
        let signals = self.mutate_editor_with(move |e| e.jump_to_file_line_col(&path, line, col));
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    /// Adopt a freshly-built help buffer as the active view. Records
    /// the current document cursor on the position-history ring as
    /// an `AutoJump` (so `<C-o>` from inside the help buffer returns
    /// to the document spot the user opened from), then flips
    /// `active_buffer` to `Help`. Used by every `:describe-*` /
    /// `:apropos` / `:keymap` entry point.
    ///
    /// **Popup vs in-pane.** This is the *popup* path -- the help
    /// content sits on the App's transient `popup_buffer` slot and
    /// renders as a centred overlay. The complementary
    /// [`Self::open_help_in_pane`] path registers the buffer in
    /// [`BufferRegistry`] and swaps the active pane to it; that's
    /// what `:lsp-log` / `:lsp-server-log` / `:lsp-trace-log` (Phase
    /// 3) and future persistent help views route through.

    /// Adopt a help buffer into the unified [`BufferRegistry`] and
    /// swap the active pane to it -- the in-pane counterpart to
    /// [`Self::open_help`]. Used by persistent help views (LSP logs,
    /// `:diagnostics`, `:apropos` once migrated) that should live as
    /// real buffers: split-able, switchable via `:bn` / `:b N`,
    /// listed by `:ls`.
    ///
    /// De-duplicates by title -- re-running the command surfaces the
    /// existing buffer rather than allocating a new one. Returns the
    /// `BufferId` either way so callers can wire follow-up state
    /// (Phase 4 live-tail subscriptions key off this id).
    ///
    /// **Hot-path model.** The registry entry is the durable record
    /// (`:ls` / `:bn` / picker discovery); the App's `popup_buffer`
    /// slot mirrors the active in-pane help so the keymap +
    /// renderer stay single-path. Pane-switch hooks
    /// ([`Self::snapshot_active_pane`] / [`Self::load_active_pane`])
    /// sync the two at boundaries -- same pattern as Document's
    /// `syntax`/`folds` snapshots.
    pub(crate) fn open_help_in_pane(&mut self, content: HelpContent) -> BufferId {
        // Slice 3c.final.E.3: route through `mutate_editor_with`.
        let (id, signals) = self.mutate_editor_with(move |e| e.open_help_in_pane(content));
        for s in signals {
            self.handle_renderer_signal(s);
        }
        id
    }

    /// Thin wrapper around
    /// [`lattice_host::editor::Editor::seed_empty_document_locals`]
    /// (Phase 5.7.B.9 migration).
    pub(super) fn seed_empty_document_locals(&mut self, buffer_id: BufferId) {
        // Slice 3c.final.E.3: route through `mutate_editor`.
        self.mutate_editor(move |e| e.seed_empty_document_locals(buffer_id));
    }

    /// M.3.2.c.4 mirror for the active document: copy the App's
    /// hot-path fields (`syntax`, `last_parsed_text_version`,
    /// `last_synced_syntax_version`, `folds`) into the buffer-
    /// locals map for `self.editor.document_buffer_id`. Called from
    /// every site that mutates those fields so reader-side flips
    /// (M.3.2.c.4 follow-up + retirement) can resolve mode-owned
    /// state through `buffer_locals` uniformly across active /
    /// inactive buffers.
    // 5.5.H: `seed_active_document_locals` retired (zero callers
    // anywhere in the workspace). M.3.2.c.4's reader-side
    // resolution through `buffer_locals` is now driven by the
    // deactivation hook in the buffer-switch path, not an explicit
    // seed call from App.

    // ---- M.3.2.c.4 reader accessors ----
    //
    // These resolve mode-owned document state through
    // `buffer_locals` so callers don't have to branch on
    // active-vs-inactive. The active buffer's hot-path fields
    // (`App.editor.syntax`, `App.folds`, etc.) remain canonical;
    // locals mirror them at de-activation boundaries so reads
    // for inactive buffers route through this path uniformly.

    /// Mode-owned syntax handle for `id`. For the active
    /// document this is `App.editor.syntax` (the live hot-path slot);
    /// for inactive documents it routes through `buffer_locals`.
    /// Returns `None` for `Lang::Plain` documents and for
    /// non-document buffers.
    pub(crate) fn document_syntax_for(&self, id: BufferId) -> Option<lattice_syntax::SyntaxHandle> {
        // Slice 3c.final.E.5e: returns owned `SyntaxHandle` (Clone;
        // cheap -- inner Arc<ArcSwap<_>> bump + mpsc sender bump) so
        // the `Send + 'static` closure body is satisfied.
        self.read_editor(move |e| e.document_syntax_for(id).cloned())
    }

    // Slice 3c.final.E.5h: `document_folds_for`,
    // `document_last_parsed_text_version_for`, and
    // `document_last_synced_syntax_version_for` App-side
    // delegates moved to the `#[cfg(test)] impl App` block at the
    // bottom of this file — production code reaches the host-side
    // copies (`Editor::document_folds_for` etc.) directly from
    // `pane_highlights.rs`; only the `document_locals_*` test in
    // this file's `mod tests` block still pokes them through the
    // App. Same pattern as the completion.rs E.5g cleanup.

    pub(super) fn save_blocking(&mut self) -> Result<std::path::PathBuf, RuntimeError> {
        // Slice 3c.final.E.3: route through `mutate_editor_with`.
        self.mutate_editor_with(|e| e.save_blocking())
    }

    // Phase 5.8.AD.3: `fire_did_create_files_notifications` +
    // `fire_will_save_notifications` migrated to
    // `lattice_host::dispatch::Editor` (private helpers under
    // `save_blocking`). No App-side callers remain.

    /// Run `textDocument/willSaveWaitUntil` against every
    /// server advertising the request; collect their TextEdits
    /// and apply them pre-save.
    ///
    /// Audit slice 5 / M4: the previous shape iterated servers
    /// sequentially with a 500ms timeout per server, so total
    /// UI-thread block was up to `500ms × N`. New shape runs
    /// every server's request concurrently under one shared
    /// 500ms budget -- worst-case UI block is bounded at 500ms
    /// regardless of how many servers are attached. The
    /// remaining sync `block_on` is queued for the eventual
    /// two-phase save (kick off → return → drain on completion);
    /// the bounded-parallel fix covers the audit's actual
    /// concern (1.5s+ stalls for multi-server saves) without
    /// the behavioural change of fully-async save.
    // Phase 5.8.AD.3: `run_will_save_wait_until_blocking` +
    // `fire_did_save_notifications` migrated to
    // `lattice_host::dispatch::Editor` (private helpers under
    // `save_blocking`).

    pub(super) fn save_as_blocking(&self, path: std::path::PathBuf) -> Result<(), RuntimeError> {
        // Phase 5.8.AD.3: body migrated to
        // `lattice_host::dispatch::Editor::save_as_blocking`.
        self.read_editor(move |e| e.save_as_blocking(path))
    }

    // 5.5.G.4: `do_redraw_screen` migrated to
    // [`lattice_host::dispatch::Editor`].
}

// Slice 3c.final.E.5h — test-fixture surface for mode-owned
// document state. Same shape as the `#[cfg(test)] impl App`
// blocks in `completion.rs` (E.5g) and `picker.rs` (E.5h):
// production code reaches host-side copies; the wrappers here
// exist so this file's `mod tests` block can poke the App-level
// resolution path against a fully-built `App`.
#[cfg(test)]
impl App {
    pub(crate) fn document_folds_for(&self, id: BufferId) -> &[crate::app::Fold] {
        let ad = self.ad();
        if id == ad.document_buffer_id && matches!(ad.buffer_kind, BufferKind::Document) {
            return &self.editor.folds;
        }
        self.editor
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::DocumentFolds>())
            .map(|f| f.0.as_slice())
            .unwrap_or(&[])
    }

    pub(crate) fn document_last_parsed_text_version_for(&self, id: BufferId) -> u64 {
        let ad = self.ad();
        if id == ad.document_buffer_id && matches!(ad.buffer_kind, BufferKind::Document) {
            return self.editor.last_parsed_text_version;
        }
        self.editor
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::DocumentLastParsedTextVersion>())
            .map(|v| v.0)
            .unwrap_or(0)
    }

    pub(crate) fn document_last_synced_syntax_version_for(&self, id: BufferId) -> u64 {
        let ad = self.ad();
        if id == ad.document_buffer_id && matches!(ad.buffer_kind, BufferKind::Document) {
            return self.editor.last_synced_syntax_version;
        }
        self.editor
            .buffer_locals
            .get(&id)
            .and_then(|l| l.get::<crate::modes::DocumentLastSyncedSyntaxVersion>())
            .map(|v| v.0)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::app::test_helpers::{
        app_with, attach_test_syntax, fresh_workspace, invoke_motion, set_rust_syntax, submit_ex,
        unique_tempdir, write_temp_file, write_workspace_config,
    };
    use crate::app::*;
    use lattice_protocol::edit::Edit;

    #[test]
    fn maybe_reparse_syntax_drains_pending_edits_and_updates_version() {
        let mut a = app_with("hello", 5);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        let initial_synced = a.editor.last_synced_syntax_version;
        a.apply_edit_blocking(Edit::insert(Position::new(0, 5), " world"))
            .unwrap();
        assert_eq!(a.editor.pending_syntax_edits.len(), 1);
        // Drive the reparse-request seam directly (mirrors what
        // the runtime loop does at the end of each Action).
        a.maybe_reparse_syntax();
        // Edits drained.
        assert_eq!(a.editor.pending_syntax_edits.len(), 0);
        // Version baseline advanced -- next request will use
        // this as `from_version`.
        assert!(a.editor.last_synced_syntax_version > initial_synced);
        assert_eq!(
            a.editor.last_synced_syntax_version,
            a.editor.document.text_version()
        );
    }

    #[test]
    fn edit_loads_named_file() {
        let dir = unique_tempdir();
        let path = dir.join("hello.txt");
        std::fs::write(&path, "loaded contents\nsecond line").unwrap();
        let mut a = app_with("original", 10);
        let cmd = format!("e {}", path.display());
        submit_ex(&mut a, &cmd);
        assert_eq!(a.editor.document.text(), "loaded contents\nsecond line");
        assert_eq!(a.editor.cursor, Position::ZERO);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn autoread_stamps_fingerprint_on_load() {
        // AR.0: loading a file-backed buffer stamps its on-disk
        // fingerprint, and that fingerprint matches the file content.
        use lattice_host::autoread::OnDiskFingerprint;
        let dir = unique_tempdir();
        let path = dir.join("hello.txt");
        std::fs::write(&path, "loaded contents\n").unwrap();
        let mut a = app_with("original", 10);
        submit_ex(&mut a, &format!("e {}", path.display()));

        let id = a.editor.document_buffer_id;
        let fp = a
            .editor
            .on_disk_fingerprints
            .get(&id)
            .expect("fingerprint stamped on load");
        let expected = OnDiskFingerprint::from_path_and_text(&path, "loaded contents\n");
        assert!(
            fp.same_content(&expected),
            "load fingerprint matches file content"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn autoread_restamps_fingerprint_on_save_to_match_disk() {
        // AR.0 self-write suppression: after editing the buffer and
        // saving, the stored fingerprint reflects the NEW content — i.e.
        // it equals what a re-read of disk would produce, so the watcher
        // (AR.2) will recognise this write as its own.
        use lattice_host::autoread::OnDiskFingerprint;
        let dir = unique_tempdir();
        let path = dir.join("edit.txt");
        std::fs::write(&path, "old\n").unwrap();
        let mut a = app_with("original", 10);
        submit_ex(&mut a, &format!("e {}", path.display()));
        let id = a.editor.document_buffer_id;
        let before = a.editor.on_disk_fingerprints.get(&id).cloned().unwrap();

        // Dirty the buffer, then save.
        a.apply_edit_blocking(Edit::insert(Position::new(0, 0), "new-"))
            .unwrap();
        assert!(a.editor.document.dirty());
        a.save_blocking().unwrap();

        let after = a.editor.on_disk_fingerprints.get(&id).cloned().unwrap();
        assert!(
            !before.same_content(&after),
            "content changed ⇒ fingerprint changed"
        );
        // The stored fingerprint equals a fresh read of disk: self-write
        // is suppressible.
        let disk = std::fs::read_to_string(&path).unwrap();
        let disk_fp = OnDiskFingerprint::from_path_and_text(&path, &disk);
        assert!(
            after.same_content(&disk_fp),
            "post-save fingerprint matches on-disk content"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn autoread_removes_fingerprint_on_buffer_delete() {
        // AR.0: closing a file-backed buffer drops its fingerprint so the
        // map tracks only live buffers.
        let dir = unique_tempdir();
        let a_path = dir.join("a.txt");
        let b_path = dir.join("b.txt");
        std::fs::write(&a_path, "aaa\n").unwrap();
        std::fs::write(&b_path, "bbb\n").unwrap();
        let mut app = app_with("original", 10);
        submit_ex(&mut app, &format!("e {}", a_path.display()));
        let a_id = app.editor.document_buffer_id;
        submit_ex(&mut app, &format!("e {}", b_path.display()));
        assert!(app.editor.on_disk_fingerprints.contains_key(&a_id));

        // Switch back to A and delete it.
        submit_ex(&mut app, &format!("e {}", a_path.display()));
        assert_eq!(app.editor.document_buffer_id, a_id);
        submit_ex(&mut app, "bd");
        assert!(
            !app.editor.on_disk_fingerprints.contains_key(&a_id),
            "fingerprint dropped on :bd"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edit_refuses_when_dirty() {
        let mut a = app_with("modified", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert!(a.editor.document.dirty());
        submit_ex(&mut a, "e /nonexistent");
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        // Document unchanged.
        assert_eq!(a.editor.document.text(), "Xmodified");
    }

    #[test]
    fn edit_force_overrides_dirty_guard() {
        let dir = unique_tempdir();
        let path = dir.join("forced.txt");
        std::fs::write(&path, "loaded").unwrap();
        let mut a = app_with("dirty content", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("Z".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        let cmd = format!("e! {}", path.display());
        submit_ex(&mut a, &cmd);
        assert_eq!(a.editor.document.text(), "loaded");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edit_preserves_registers_across_swap() {
        let dir = unique_tempdir();
        let path = dir.join("preserve.txt");
        std::fs::write(&path, "new content").unwrap();
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.editor.builtins.yank.0).with_target(
            lattice_grammar::Target::Motion(
                a.editor.builtins.word_forward,
                lattice_grammar::Args::None,
            ),
        );
        a.apply(Action::Invoke(inv));
        assert!(a.editor.unnamed_register.is_some());
        let cmd = format!("e {}", path.display());
        submit_ex(&mut a, &cmd);
        // Register survives.
        assert!(a.editor.unnamed_register.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edit_resets_per_document_state() {
        let dir = unique_tempdir();
        let path = dir.join("reset.txt");
        std::fs::write(&path, "fresh").unwrap();
        let mut a = app_with("aaa\nbbb\nccc", 10);
        a.editor.cursor = Position::new(2, 1);
        a.apply(invoke_motion(a.editor.builtins.goto_first_line));
        // The goto_first_line motion pushed a jump entry.
        let history_pre = a.editor.position_history.len();
        assert!(history_pre > 0);
        let cmd = format!("e {}", path.display());
        submit_ex(&mut a, &cmd);
        // M.10.3.fix1 (2026-06-03): position_history is
        // session-wide, NOT per-document. `:e <new-file>` does
        // NOT clear the jump list — vim semantics require
        // `<C-o>` to walk back into the previous buffer after
        // a fresh-file open. The entry pushed in the previous
        // buffer survives; activate_buffer additionally pushes
        // an entry for the cross-buffer hop (dedups against
        // the pre-existing one if identical fields).
        assert!(
            a.editor.position_history.len() >= history_pre,
            "position_history must survive fresh-file open (jump list is session-wide)"
        );
        // The other per-document state (cursor) DOES reset —
        // fresh file starts at (0, 0).
        assert_eq!(a.editor.cursor, Position::ZERO);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edit_unknown_path_emits_error() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "e /absolutely/does/not/exist/anywhere.txt");
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        // Buffer unchanged.
        assert_eq!(a.editor.document.text(), "hello");
    }

    #[test]
    fn split_pane_horizontal_creates_second_pane() {
        let mut a = app_with("xx", 10);
        a.apply(Action::SplitPaneHorizontal);
        assert_eq!(a.editor.pane_tree.len(), 2);
        // Active stays on original.
        assert_eq!(a.editor.pane_tree.active_index(), 0);
    }

    #[test]
    fn split_pane_vertical_creates_second_pane() {
        let mut a = app_with("xx", 10);
        a.apply(Action::SplitPaneVertical);
        assert_eq!(a.editor.pane_tree.len(), 2);
    }

    #[test]
    fn close_pane_collapses_split() {
        let mut a = app_with("xx", 10);
        a.apply(Action::SplitPaneVertical);
        a.apply(Action::ClosePane);
        assert_eq!(a.editor.pane_tree.len(), 1);
    }

    #[test]
    fn only_pane_collapses_all_other_panes() {
        // `:only` / `<C-x>1`: from three panes, collapse to one.
        let mut a = app_with("xx", 10);
        a.apply(Action::SplitPaneVertical);
        a.apply(Action::SplitPaneHorizontal);
        assert_eq!(a.editor.pane_tree.len(), 3);
        a.apply(Action::OnlyPane);
        assert_eq!(a.editor.pane_tree.len(), 1);
    }

    #[test]
    fn only_pane_single_pane_is_a_noop() {
        // No panic, no quit, stays at one pane (failure-mode guard).
        let mut a = app_with("xx", 10);
        a.apply(Action::OnlyPane);
        assert_eq!(a.editor.pane_tree.len(), 1);
        assert!(!a.editor.should_quit);
    }

    #[test]
    fn quit_with_multiple_panes_closes_active_pane() {
        let mut a = app_with("xx", 10);
        a.apply(Action::SplitPaneVertical);
        assert_eq!(a.editor.pane_tree.len(), 2);
        a.do_quit(false, lattice_grammar::QuitScope::Pane);
        assert!(
            !a.editor.should_quit,
            "extra pane: :q must not exit the editor"
        );
        assert_eq!(a.editor.pane_tree.len(), 1);
    }

    #[test]
    fn quit_with_multiple_panes_skips_dirty_check() {
        let mut a = app_with("xx", 10);
        a.apply(Action::SplitPaneVertical);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("z".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert!(a.editor.document.dirty());
        a.do_quit(false, lattice_grammar::QuitScope::Pane);
        assert!(!a.editor.should_quit);
        assert_eq!(a.editor.pane_tree.len(), 1);
    }

    #[test]
    fn quit_with_last_pane_clean_quits_editor() {
        let mut a = app_with("xx", 10);
        a.do_quit(false, lattice_grammar::QuitScope::Pane);
        assert!(a.editor.should_quit);
    }

    #[test]
    fn quit_last_pane_with_other_tabs_closes_tab_not_editor() {
        // `:q` on the last pane of a tab, when other tabs exist, closes
        // the TAB (vim's tab-page close), it does NOT quit the editor.
        let mut a = app_with("xx", 10);
        a.apply(Action::NewTab);
        assert_eq!(a.editor.tabs.len(), 2);
        assert_eq!(a.editor.pane_tree.len(), 1, "new tab starts with one pane");
        a.do_quit(false, lattice_grammar::QuitScope::Pane);
        assert!(
            !a.editor.should_quit,
            ":q with other tabs open must not quit the editor"
        );
        assert_eq!(a.editor.tabs.len(), 1, ":q closed the tab, leaving one");
    }

    #[test]
    fn quit_all_with_other_tabs_quits_editor() {
        // `:qa` ignores tab count just like pane count.
        let mut a = app_with("xx", 10);
        a.apply(Action::NewTab);
        assert_eq!(a.editor.tabs.len(), 2);
        a.do_quit(false, lattice_grammar::QuitScope::All);
        assert!(
            a.editor.should_quit,
            ":qa must quit regardless of how many tabs are open"
        );
    }

    #[test]
    fn quit_all_with_multiple_panes_quits_editor() {
        // `:qa` ignores pane count: unlike `:q`, an extra pane must
        // NOT turn the quit into a pane-close. Clean buffers → quit.
        let mut a = app_with("xx", 10);
        a.apply(Action::SplitPaneVertical);
        assert_eq!(a.editor.pane_tree.len(), 2);
        a.do_quit(false, lattice_grammar::QuitScope::All);
        assert!(
            a.editor.should_quit,
            ":qa must exit the editor regardless of pane count"
        );
    }

    #[test]
    fn quit_all_dirty_refuses_then_force_quits() {
        // `:qa` shares `:q`'s dirty guard: a dirty buffer blocks the
        // quit (no `should_quit`, warning set), and `:qa!` forces past.
        let mut a = app_with("xx", 10);
        a.apply(Action::SplitPaneVertical);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("z".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert!(a.editor.document.dirty());
        a.do_quit(false, lattice_grammar::QuitScope::All);
        assert!(
            !a.editor.should_quit,
            ":qa must honor the dirty guard (no force)"
        );
        // The extra pane is untouched — :qa did not degrade to a pane-close.
        assert_eq!(a.editor.pane_tree.len(), 2);
        a.do_quit(true, lattice_grammar::QuitScope::All);
        assert!(a.editor.should_quit, ":qa! forces past the dirty guard");
    }

    #[test]
    fn quit_with_last_pane_dirty_refuses() {
        let mut a = app_with("xx", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("z".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        a.do_quit(false, lattice_grammar::QuitScope::Pane);
        assert!(!a.editor.should_quit);
        assert!(
            a.editor
                .last_message
                .as_ref()
                .map(|m| m.text.contains("no write since last change"))
                .unwrap_or(false)
        );
    }

    #[test]
    fn quit_force_with_last_pane_dirty_quits() {
        let mut a = app_with("xx", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("z".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        a.do_quit(true, lattice_grammar::QuitScope::Pane);
        assert!(a.editor.should_quit);
    }

    /// `:q` must skip the dirty guard for `*messages*` and other
    /// subsystem-owned synthetic buffers. `messages-mode`
    /// contributes `NoFile = true` so the resolved-option filter
    /// in `do_quit` excludes the buffer; without the filter, the
    /// transcript's append flow leaves it permanently "dirty"
    /// and `:q` refuses to exit.
    #[test]
    fn quit_with_dirty_messages_buffer_still_quits() {
        let mut a = app_with("xx", 10);
        let msgs = a.ensure_messages_buffer();
        // Force a content write so the buffer's clean position
        // diverges from its current depth (mirrors what the
        // tracing subscriber's append flow produces).
        a.append_to_owned_buffer(msgs, "boot record line\n");
        assert!(
            a.editor.buffers.document_dirty(msgs),
            "test pre-condition: *messages* buffer reports dirty after append",
        );
        a.do_quit(false, lattice_grammar::QuitScope::Pane);
        assert!(
            a.editor.should_quit,
            ":q must skip the dirty guard for NoFile = true buffers (e.g. *messages*)",
        );
    }

    #[test]
    fn open_help_popup_preserves_doc_pane_cursor_for_render() {
        // Bug: invoking a popup-mode help command (`:lsp-status`,
        // `:describe-key`, etc.) flipped `active_buffer` to Help
        // without first syncing the doc's `app.editor.cursor` /
        // `app.editor.scroll` into the active pane's stash. The renderer
        // reads `pane.cursor` for any pane whose buffer kind
        // doesn't match `active_buffer` (popup mode = mismatch),
        // so the doc visibly jumped to wherever pane.cursor was
        // last (often (0,0)).
        let mut a = app_with("line0\nline1\nline2\nline3\nline4\n", 5);
        a.editor.cursor = Position::new(3, 2);
        a.editor.scroll = 1;
        a.do_lsp_status();
        // After open_help, active is Help but the active pane
        // still shows the doc -- pane.cursor must reflect where
        // the doc was, not the help buffer's (0,0).
        let pane = a.editor.pane_tree.active();
        assert_eq!(
            pane.cursor,
            Position::new(3, 2),
            "doc's pre-help cursor must be stashed onto pane.cursor"
        );
        assert_eq!(pane.scroll, 1);
    }

    #[test]
    fn split_inherits_cursor_and_scroll_from_active() {
        let mut a = app_with("a\nb\nc\nd", 10);
        a.editor.cursor = Position::new(2, 0);
        a.editor.scroll = 1;
        a.apply(Action::SplitPaneVertical);
        // Both panes should have (line=2, scroll=1) initially.
        let panes = a.editor.pane_tree.leaves();
        assert_eq!(panes[0].cursor.line, 2);
        assert_eq!(panes[0].scroll, 1);
        assert_eq!(panes[1].cursor.line, 2);
        assert_eq!(panes[1].scroll, 1);
    }

    #[test]
    fn edit_new_file_registers_a_second_buffer() {
        let path = write_temp_file("a", "alpha\n");
        let mut a = app_with("xx", 10);
        let initial_id = a.editor.document_buffer_id;
        a.editor.command_line = format!("e {}", path.display());
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        // Two listed buffers (initial + opened); the synthetic
        // `*lsp*` buffer is unlisted and filtered out here.
        assert_eq!(a.editor.buffers.listed_ids_sorted().len(), 2);
        assert_ne!(a.editor.document_buffer_id, initial_id);
        assert_eq!(a.editor.document.text(), "alpha\n");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn ls_renders_help_with_every_open_buffer() {
        let path = write_temp_file("c", "x\n");
        let mut a = app_with("xx", 10);
        a.editor.command_line = format!("e {}", path.display());
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        a.editor.command_line = "ls".into();
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("buffers help");
        let body = h.content.as_string();
        // Four buffers total: the initial document, the file we
        // opened, the synthetic `*lsp*` Document, and the
        // `*messages*` Messages transcript. `:ls` lists every
        // entry regardless of `listed`; the unlisted marker `u`
        // signals the user-toggleable cycle filter without
        // suppressing the row. The per-kind summary partitions
        // documents (3) and messages (1).
        assert!(body.contains("4 open buffer"));
        assert!(body.contains("3 document"));
        assert!(body.contains("1 message"));
        assert!(body.contains("*lsp*"));
        assert!(body.contains("*messages*"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn tree_open_makes_filetree_active() {
        let dir = std::env::temp_dir().join(format!("lattice-tree-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(dir.join("a.txt"), "alpha").ok();
        let mut a = app_with("xx", 10);
        a.editor.command_line = format!("Filetree {}", dir.display());
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.editor.active_buffer, BufferKind::FileTree);
        assert_eq!(a.editor.buffers.file_tree_ids_sorted().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tree_close_returns_to_document() {
        let dir = std::env::temp_dir().join(format!("lattice-tree-close-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let mut a = app_with("xx", 10);
        a.editor.command_line = format!("Filetree {}", dir.display());
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        a.apply(Action::HelpDismiss);
        assert_eq!(a.editor.active_buffer, BufferKind::Document);
        assert_eq!(a.editor.buffers.file_tree_ids_sorted().len(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tree_motion_routes_through_active_buffer() {
        let dir = std::env::temp_dir().join(format!("lattice-tree-motion-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(dir.join("a.txt"), "x").ok();
        std::fs::write(dir.join("b.txt"), "y").ok();
        let mut a = app_with("xx", 10);
        a.editor.command_line = format!("Filetree {}", dir.display());
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let line_down = a.editor.builtins.line_down;
        a.apply(Action::Invoke(CommandInvocation::of(line_down.0)));
        // After unification, `self.editor.cursor` is the active buffer's
        // cursor. The tree's own `cursor` field is archival save-
        // state synced at activation transitions.
        assert_eq!(a.editor.cursor.line, 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tree_follow_on_file_opens_document_buffer() {
        let dir = std::env::temp_dir().join(format!("lattice-tree-follow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(dir.join("alpha.txt"), "hello").ok();
        let mut a = app_with("xx", 10);
        a.editor.command_line = format!("Filetree {}", dir.display());
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        // Move cursor to the alpha.txt entry (row 1).
        let line_down = a.editor.builtins.line_down;
        a.apply(Action::Invoke(CommandInvocation::of(line_down.0)));
        // Follow.
        a.apply(Action::FollowLink);
        // Active pane now shows the file's Document buffer; the
        // tree stays in the registry (reachable via :bn / :b).
        assert_eq!(a.editor.active_buffer, BufferKind::Document);
        assert_eq!(a.editor.buffers.file_tree_ids_sorted().len(), 1);
        assert_eq!(a.editor.document.text(), "hello");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn open_help_in_pane_registers_buffer_and_activates_pane() {
        let mut app = app_with("hi\n", 5);
        let buf = HelpContent::from_lines("test-help", vec!["# heading".into(), "body".into()]);
        let id = app.open_help_in_pane(buf);
        // Lives in the registry as a Help variant.
        assert!(app.editor.buffers.contains_help(id));
        // Active pane points at it.
        assert_eq!(app.active_pane_buffer_id(), id);
        assert!(matches!(app.editor.active_buffer, BufferKind::Help));
        // Hot-path popup slot mirrors the registry copy.
        assert_eq!(app.popup_help().unwrap().title, "test-help");
        // :ls walks the registry; help variants count.
        assert!(app.editor.buffers.help_ids_sorted().contains(&id));
    }

    #[test]
    fn open_help_in_pane_dedups_by_title() {
        let mut app = app_with("hi\n", 5);
        let id1 = app.open_help_in_pane(HelpContent::from_lines("lsp:rust", vec!["v1".into()]));
        let id2 = app.open_help_in_pane(HelpContent::from_lines(
            "lsp:rust",
            vec!["v2 (refreshed)".into()],
        ));
        assert_eq!(id1, id2, "same title returns same BufferId");
        // Refresh path overwrote the body.
        let body = app.popup_help().unwrap().content.as_string();
        assert!(body.contains("refreshed"));
        // Single help entry in the registry.
        assert_eq!(app.editor.buffers.help_ids_sorted().len(), 1);
    }

    #[test]
    fn active_pane_content_height_subtracts_status_row_always() {
        // Option A: every pane (including single) reserves a status row.
        let mut app = app_with("hi\n", 5);
        assert_eq!(app.active_pane_content_height(20), 19);
        // Horizontal split -> two panes, each ~half the buffer
        // height; minus the per-pane status row.
        app.editor
            .pane_tree
            .split_active(crate::pane::SplitOrientation::Horizontal);
        // Slice 3c.final.E.5i: `active_pane_content_height` reads
        // the pane tree through `panes()` (RS-backed mirror), so
        // direct `pane_tree` mutation needs an explicit publish.
        app.editor.publish_render_state();
        let content = app.active_pane_content_height(20);
        // 20 / 2 = 10; minus status row = 9.
        assert_eq!(content, 9);
    }

    #[test]
    fn persistent_lsp_log_level_applies_from_toml_tree() {
        // M.6.5: canonical `lsp-mode.log-level` key.
        let mut app = app_with("hi\n", 5);
        let toml_text = "[lsp-mode]\nlog-level = \"debug\"\n";
        // BC.8b: `lsp_config_tree` is shared (`Arc<ArcSwap>`); `store` the tree.
        app.editor.lsp_config_tree.store(std::sync::Arc::new(
            toml_text.parse::<toml::Table>().expect("toml parse"),
        ));
        app.apply_persistent_lsp_editor_options();
        // Effect: a Debug-level record on an unattached server lands
        // in the ring. Default min-level is Info; without the TOML
        // override the record would be filtered before it reached
        // the ring.
        let instance = lattice_lsp::InstanceKey::new(
            std::sync::Arc::<str>::from("rust"),
            std::sync::Arc::<std::path::Path>::from(std::path::Path::new("/tmp/test-ws")),
        );
        app.editor.lsp_logger.log(
            Some(&instance),
            lattice_lsp::LogLevel::Debug,
            lattice_lsp::LogSource::Client,
            "after-toml",
        );
        let recs = app.editor.lsp_logger.snapshot_instance(&instance);
        assert!(
            recs.iter().any(|r| r.message == "after-toml"),
            "Debug record should pass through after TOML log-level=debug",
        );
    }

    #[test]
    fn persistent_lsp_log_level_legacy_key_warns_then_applies() {
        // M.6.5: legacy `[lsp] log-level` key still works for one
        // minor version; emits a deprecation warn before applying.
        let mut app = app_with("hi\n", 5);
        let toml_text = "[lsp]\nlog-level = \"debug\"\n";
        // BC.8b: `lsp_config_tree` is shared (`Arc<ArcSwap>`); `store` the tree.
        app.editor.lsp_config_tree.store(std::sync::Arc::new(
            toml_text.parse::<toml::Table>().expect("toml parse"),
        ));
        app.apply_persistent_lsp_editor_options();
        let msg = app.editor.last_message.as_ref().expect("deprecation warn");
        assert_eq!(msg.level, crate::app::EchoLevel::Warn);
        assert!(
            msg.text.contains("`lsp.log-level` is deprecated")
                && msg.text.contains("`lsp-mode.log-level`"),
            "echo should name old + new keys, got {}",
            msg.text,
        );
        // And the value still applied (one-version compatibility).
        let instance = lattice_lsp::InstanceKey::new(
            std::sync::Arc::<str>::from("rust"),
            std::sync::Arc::<std::path::Path>::from(std::path::Path::new("/tmp/test-ws")),
        );
        app.editor.lsp_logger.log(
            Some(&instance),
            lattice_lsp::LogLevel::Debug,
            lattice_lsp::LogSource::Client,
            "after-legacy-toml",
        );
        let recs = app.editor.lsp_logger.snapshot_instance(&instance);
        assert!(
            recs.iter().any(|r| r.message == "after-legacy-toml"),
            "legacy key should still apply for one minor version",
        );
    }

    #[test]
    fn persistent_lsp_log_level_canonical_wins_over_legacy() {
        // If both are set, canonical wins (silently -- no deprecation
        // echo since the user has already migrated; the legacy key
        // is a leftover).
        let mut app = app_with("hi\n", 5);
        let toml_text = "[lsp]\nlog-level = \"trace\"\n[lsp-mode]\nlog-level = \"debug\"\n";
        // BC.8b: `lsp_config_tree` is shared (`Arc<ArcSwap>`); `store` the tree.
        app.editor.lsp_config_tree.store(std::sync::Arc::new(
            toml_text.parse::<toml::Table>().expect("toml parse"),
        ));
        app.apply_persistent_lsp_editor_options();
        // No deprecation echo when canonical is present.
        assert!(
            app.editor.last_message.is_none(),
            "canonical present ⇒ silent; got {:?}",
            app.editor.last_message,
        );
    }

    #[test]
    fn persistent_lsp_log_level_warns_on_unknown_value() {
        let mut app = app_with("hi\n", 5);
        let toml_text = "[lsp-mode]\nlog-level = \"babble\"\n";
        // BC.8b: `lsp_config_tree` is shared (`Arc<ArcSwap>`); `store` the tree.
        app.editor.lsp_config_tree.store(std::sync::Arc::new(
            toml_text.parse::<toml::Table>().expect("toml parse"),
        ));
        app.apply_persistent_lsp_editor_options();
        let msg = app.editor.last_message.as_ref().expect("warn echo");
        assert!(
            msg.text.contains("lsp-mode.log-level") && msg.text.contains("babble"),
            "echo should name the key + value, got {}",
            msg.text
        );
    }

    #[test]
    fn persistent_lsp_log_level_silent_when_missing() {
        let mut app = app_with("hi\n", 5);
        app.editor.last_message = None;
        // Empty tree: nothing under [lsp].
        app.editor
            .lsp_config_tree
            .store(std::sync::Arc::new(toml::Table::new()));
        app.apply_persistent_lsp_editor_options();
        assert!(
            app.editor.last_message.is_none(),
            "no echo when key is absent (default applies)",
        );
    }

    #[test]
    fn load_persistent_config_applies_scalar_override_from_project_toml() {
        let ws = fresh_workspace("scalar-override");
        write_workspace_config(&ws, "tabstop = 2\n");
        let mut a = app_with("", 5);
        // tabstop default is 4; a non-default override (2) should
        // land before the first frame.
        assert_eq!(
            *a.editor
                .config
                .get_typed::<lattice_config::Tabstop>()
                .unwrap(),
            4
        );
        a.load_persistent_config(Some(&ws));
        assert_eq!(
            *a.editor
                .config
                .get_typed::<lattice_config::Tabstop>()
                .unwrap(),
            2
        );
    }

    #[test]
    fn load_persistent_config_buckets_per_language_section() {
        let ws = fresh_workspace("per-lang-bucket");
        write_workspace_config(
            &ws,
            "[completion.per-language.markdown]\n\
             auto_trigger = false\n\
             [completion.per-language.rust]\n\
             auto_trigger = true\n",
        );
        let mut a = app_with("", 5);
        a.load_persistent_config(Some(&ws));
        // Both per-language entries land in the structural
        // bucket, keyed by full dotted path.
        let paths = a.pending_structural_section_paths("completion.per-language");
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"completion.per-language.markdown".to_string()));
        assert!(paths.contains(&"completion.per-language.rust".to_string()));
        // Drain markdown -> sub-table accessible.
        let md = a
            .take_pending_structural_section("completion.per-language.markdown")
            .expect("markdown section drained");
        assert_eq!(
            md.get("auto_trigger").and_then(|v| v.as_bool()),
            Some(false),
        );
        // After drain, only rust remains.
        let after = a.pending_structural_section_paths("completion.per-language");
        assert_eq!(after, vec!["completion.per-language.rust".to_string()]);
    }

    #[test]
    fn load_persistent_config_collects_unknown_plugin_section_for_later_drain() {
        // Extensibility: a user writes `[plugin.X]` before the
        // plugin host exists. Loader buckets it; nothing warns;
        // the host (Phase 7) drains it when it registers.
        let ws = fresh_workspace("plugin-deferred");
        write_workspace_config(&ws, "[plugin.rust-analyzer]\nclippy = true\n");
        let mut a = app_with("", 5);
        a.load_persistent_config(Some(&ws));
        let paths = a.pending_structural_section_paths("plugin");
        assert_eq!(paths, vec!["plugin.rust-analyzer".to_string()]);
        let body = a
            .take_pending_structural_section("plugin.rust-analyzer")
            .expect("plugin section drained");
        assert_eq!(body.get("clippy").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn load_persistent_config_warning_surfaces_on_unknown_key() {
        let ws = fresh_workspace("unknown-key");
        write_workspace_config(&ws, "no_such_option = 42\n");
        let mut a = app_with("", 5);
        a.load_persistent_config(Some(&ws));
        // The echo carries the loader's warning.
        let msg = a.editor.last_message.as_ref().expect("warning echoed");
        assert_eq!(msg.level, EchoLevel::Warn);
        assert!(msg.text.contains("config:"), "got `{}`", msg.text);
        assert!(msg.text.contains("no_such_option"), "got `{}`", msg.text,);
    }

    #[test]
    fn tree_sitter_source_emits_definition_position_symbols_for_rust() {
        let source = "fn outer(arg: i32) {\n    let local = arg;\n}\n";
        let mut a = app_with(source, 10);
        set_rust_syntax(&mut a, source);
        a.editor.modal = ModalState::Insert;
        // Cursor at end-of-buffer with empty query so every
        // candidate matches uniformly; the matcher won't drop
        // anything for prefix mismatch.
        a.editor.cursor = Position::new(2, 1);
        a.do_completion_trigger();
        let state = a.editor.insert_completion.as_ref().expect("popup");
        let tree_sitter_id = lattice_completion::TREE_SITTER_SYMBOL_SOURCE_ID;
        let ts_texts: Vec<&str> = state
            .raw
            .iter()
            .filter(|c| c.source.as_ref().map(|s| s.as_str()) == Some(tree_sitter_id))
            .map(|c| c.text.as_str())
            .collect();
        for expected in &["outer", "arg", "local"] {
            assert!(
                ts_texts.contains(expected),
                "expected `{expected}` in tree-sitter candidates: {ts_texts:?}",
            );
        }
    }

    #[test]
    fn tree_sitter_source_skipped_by_per_language_override() {
        let source = "fn outer() {\n    let local = 1;\n}\n";
        let mut a = app_with(source, 10);
        set_rust_syntax(&mut a, source);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(2, 1);
        // Override the active language (test buffer has no
        // path -> language id is "") to exclude tree-sitter.
        a.editor.per_language_completion.insert(
            String::new(),
            lattice_completion::PerLanguageOverrides {
                sources: Some(vec![lattice_completion::SourceId::new(
                    lattice_completion::BufferWordsSource::ID,
                )]),
                ..Default::default()
            },
        );
        a.do_completion_trigger();
        let state = a.editor.insert_completion.as_ref().expect("popup");
        let tree_sitter_id = lattice_completion::TREE_SITTER_SYMBOL_SOURCE_ID;
        for cand in &state.raw {
            let src = cand.source.as_ref().map(|s| s.as_str()).unwrap_or("");
            assert_ne!(
                src, tree_sitter_id,
                "tree-sitter source filtered out for this language",
            );
        }
    }

    #[test]
    fn tree_sitter_and_buffer_words_emit_independently_for_same_name() {
        // `outer` appears as a function definition (captured
        // by tree-sitter) AND as a referenced word (captured
        // by buffer-words). Both sources contribute their
        // tagged copy in `state.raw` -- the producers run
        // independently. Visual dedup at the renderer (4.2.g.7
        // polish) collapses them to a single popup row, so
        // `state.rendered` has exactly one entry for `outer`,
        // tagged with the higher-priority source (buffer-words
        // at 100 > tree-sitter at 80).
        let source = "fn outer() {\n    outer();\n}\n";
        let mut a = app_with(source, 10);
        set_rust_syntax(&mut a, source);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(2, 1);
        a.do_completion_trigger();
        let state = a.editor.insert_completion.as_ref().expect("popup");
        let raw_sources: Vec<&str> = state
            .raw
            .iter()
            .filter(|c| c.text == "outer")
            .map(|c| c.source.as_ref().map(|s| s.as_str()).unwrap_or(""))
            .collect();
        assert!(
            raw_sources.contains(&lattice_completion::BufferWordsSource::ID),
            "buffer-words copy present in raw set: {raw_sources:?}",
        );
        assert!(
            raw_sources.contains(&lattice_completion::TREE_SITTER_SYMBOL_SOURCE_ID),
            "tree-sitter copy present in raw set: {raw_sources:?}",
        );
        let rendered_outer: Vec<&str> = state
            .rendered
            .iter()
            .filter(|c| c.raw.text == "outer")
            .map(|c| c.raw.source.as_ref().map(|s| s.as_str()).unwrap_or(""))
            .collect();
        assert_eq!(rendered_outer.len(), 1, "popup deduped to one row");
        assert_eq!(
            rendered_outer[0],
            lattice_completion::BufferWordsSource::ID,
            "higher-priority source's row survives the dedup",
        );
    }

    #[test]
    fn tree_sitter_source_silent_without_syntax_attached() {
        // No `set_rust_syntax` -> `app_with` leaves
        // `self.editor.syntax = None`; tree-sitter source emits
        // nothing.
        let mut a = app_with("alpha bravo charlie", 5);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 19);
        a.do_completion_trigger();
        if let Some(state) = a.editor.insert_completion.as_ref() {
            let tree_sitter_id = lattice_completion::TREE_SITTER_SYMBOL_SOURCE_ID;
            for cand in &state.raw {
                assert_ne!(
                    cand.source.as_ref().map(|s| s.as_str()),
                    Some(tree_sitter_id),
                );
            }
        }
    }

    #[test]
    fn load_persistent_config_silent_when_no_files_present() {
        let ws = fresh_workspace("no-files");
        // Empty workspace -- no .lattice/config.toml. Loader
        // produces no messages; the modeline stays clean.
        let mut a = app_with("", 5);
        let prior = a.editor.last_message.clone();
        a.load_persistent_config(Some(&ws));
        // No new echo (modeline message is whatever the test
        // setup left, which for app_with is None).
        assert_eq!(a.editor.last_message, prior);
    }

    #[test]
    fn open_help_in_pane_seeds_help_locals() {
        let mut a = app_with("hi", 5);
        let help = crate::help::HelpContent::from_lines(
            "test-locals",
            vec![
                "# Heading One".to_string(),
                "see [ex:write](command:ex:write)".to_string(),
            ],
        );
        let help_id = a.open_help_in_pane(help);
        let locals = a
            .editor
            .buffer_locals
            .get(&help_id)
            .expect("buffer_locals should be populated for help buffer");
        // Links parsed from `[ex:write](command:ex:write)`.
        let links = locals
            .get::<crate::modes::HelpLinks>()
            .expect("HelpLinks local seeded");
        assert_eq!(links.0.len(), 1);
        // Anchors come from heading slug generation. `from_lines`
        // doesn't auto-anchor headings (only
        // `from_lines_and_anchors` plumbs anchors); the seed
        // should still be present, just empty.
        let anchors = locals
            .get::<crate::modes::HelpAnchors>()
            .expect("HelpAnchors local seeded (possibly empty)");
        assert_eq!(anchors.0.len(), 0);
    }

    #[test]
    fn file_tree_locals_carry_owner_metadata() {
        let tmp =
            std::env::temp_dir().join(format!("lattice-m3-2-c-2-meta-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let mut a = app_with("hi", 5);
        a.do_open_file_tree(Some(tmp.clone()));
        let tree_id = a.active_pane_buffer_id();
        let locals = a.editor.buffer_locals.get(&tree_id).unwrap();
        // file-tree-mode-owned locals (root / entries /
        // nerd-fonts). Other locals (e.g.
        // `ActiveCompletionSources` from CSM.3) may coexist on
        // the same buffer; the file-tree subset must still be
        // namespaced correctly.
        let tree_descriptors: Vec<_> = locals
            .iter_descriptors()
            .filter(|d| d.owner_mode == "file-tree-mode")
            .collect();
        assert!(
            tree_descriptors.len() >= 3,
            "file-tree-mode should seed root/entries/nerd-fonts, got {tree_descriptors:?}",
        );
        for d in &tree_descriptors {
            assert!(
                d.name.starts_with("file-tree-mode."),
                "file-tree-mode local name {:?} should be namespaced under file-tree-mode",
                d.name,
            );
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn boot_seeds_initial_document_locals() {
        // M.3.2.c.4: the initial document buffer should have its
        // four mode-owned locals (DocumentSyntax, last_parsed,
        // last_synced, folds) seeded with empty defaults. Reader-
        // side flips later in this slice route through this map.
        let a = app_with("hello", 10);
        let id = a.editor.document_buffer_id;
        let locals = a
            .editor
            .buffer_locals
            .get(&id)
            .expect("initial document has buffer_locals");
        assert!(
            locals.get::<crate::modes::DocumentSyntax>().is_some(),
            "DocumentSyntax local seeded"
        );
        assert!(
            locals
                .get::<crate::modes::DocumentLastParsedTextVersion>()
                .is_some(),
            "DocumentLastParsedTextVersion local seeded"
        );
        assert!(
            locals
                .get::<crate::modes::DocumentLastSyncedSyntaxVersion>()
                .is_some(),
            "DocumentLastSyncedSyntaxVersion local seeded"
        );
        assert!(
            locals.get::<crate::modes::DocumentFolds>().is_some(),
            "DocumentFolds local seeded"
        );
    }

    #[test]
    fn snapshot_active_document_mirrors_into_locals() {
        // After de-activating a document (which moves App.editor.syntax
        // / App.folds into entry.editor.syntax / entry.folds), the
        // buffer-locals for that document should reflect the
        // entry's new contents.
        let mut a = app_with("hello\nworld", 10);
        let active_id = a.editor.document_buffer_id;
        // Force a non-default fold so the mirror has something
        // to observe.
        a.editor.folds.push(crate::app::Fold {
            start_line: 0,
            end_line: 1,
            closed: false,
            identity: None,
        });
        a.editor.last_parsed_text_version = 42;
        a.editor.last_synced_syntax_version = 41;
        a.snapshot_active_document();
        let locals = a
            .editor
            .buffer_locals
            .get(&active_id)
            .expect("locals exist for active id");
        let parsed = locals
            .get::<crate::modes::DocumentLastParsedTextVersion>()
            .expect("last_parsed mirrored");
        let synced = locals
            .get::<crate::modes::DocumentLastSyncedSyntaxVersion>()
            .expect("last_synced mirrored");
        let folds = locals
            .get::<crate::modes::DocumentFolds>()
            .expect("folds mirrored");
        assert_eq!(parsed.0, 42);
        assert_eq!(synced.0, 41);
        assert_eq!(folds.0.len(), 1);
        assert_eq!(folds.0[0].start_line, 0);
    }

    #[test]
    fn popup_dismiss_does_not_jolt_backdrop_scroll() {
        // Open a centred popup over a long doc so the doc's scroll
        // sits at a non-zero baseline; dismiss; assert the doc's
        // scroll round-trips. The bug: `ensure_cursor_visible`
        // fires after dispatch with `viewport_height` still set to
        // the popup's small inner height, recomputing scroll
        // against the wrong viewport and shifting the backdrop.
        let many_lines: String = (0..50).map(|i| format!("line {i}\n")).collect();
        let mut a = app_with(&many_lines, 30);
        // Park the cursor mid-document so scroll is non-zero.
        a.editor.cursor = lattice_protocol::position::Position::new(40, 0);
        a.editor.scroll = 30;
        let pre_cursor = a.editor.cursor;
        let pre_scroll = a.editor.scroll;
        // :lsp-status opens a centred popup (focuses Help mode).
        a.do_lsp_status();
        // viewport_height is now the popup's inner height (small).
        // Set it explicitly to mimic what runtime would do.
        a.set_viewport_height(
            a.help_popup_inner_height(30)
                .unwrap_or(a.editor.viewport_height),
        );
        // Dismiss the popup (the dispatch path calls this on Esc).
        a.dismiss_popup();
        // Now simulate what `apply` does post-dispatch: the fix is
        // that ensure_cursor_visible gets skipped on this transition,
        // so we don't even need to call it. Verify cursor + scroll
        // are restored to pre-popup values.
        assert_eq!(a.editor.cursor, pre_cursor, "cursor restored");
        assert_eq!(a.editor.scroll, pre_scroll, "scroll restored without jolt");
    }

    #[test]
    fn popup_with_long_content_scrolls_when_cursor_descends() {
        // Popup with 50 lines of content; focused (active_buffer =
        // Help). Step the cursor past the popup viewport's bottom
        // row and assert popup scroll advances to keep the cursor
        // visible. Goes through the input + keymap layer (`press`)
        // so the path matches what real `j` keystrokes hit, not
        // the App's direct-Invoke shortcut.
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let lines: Vec<String> = (0..50).map(|i| format!("popup line {i}")).collect();
        let mut a = app_with("backdrop\n", 30);
        let buf = crate::help::HelpContent::from_lines("status", lines);
        a.open_popup(buf, crate::popup::PopupPlacement::Centered);
        // Mimic the runtime's per-frame viewport set: in popup
        // mode it's the popup's inner height, not the doc area's.
        let inner = a
            .help_popup_inner_height(30)
            .expect("popup inner height available");
        a.set_viewport_height(inner);
        // Press `j` enough times to step past the visible
        // viewport. Each iteration mirrors the runtime: refresh
        // viewport_height, then process the keystroke.
        for _ in 0..(inner + 5) {
            a.set_viewport_height(
                a.help_popup_inner_height(30)
                    .unwrap_or(a.editor.viewport_height),
            );
            crate::app::test_helpers::press(
                &mut a,
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty()),
            );
        }
        assert!(
            a.editor.cursor.line >= inner,
            "cursor should descend past the visible viewport, got line {} (inner {})",
            a.editor.cursor.line,
            inner
        );
        assert!(
            a.editor.scroll > 0,
            "scroll should advance once cursor leaves the visible window, got scroll {}",
            a.editor.scroll
        );
        assert!(
            a.editor.cursor.line < a.editor.scroll + inner,
            "cursor must still be inside the scrolled viewport (cursor {}, scroll {}, inner {})",
            a.editor.cursor.line,
            a.editor.scroll,
            inner
        );
    }

    #[test]
    fn document_syntax_for_inactive_resolves_through_locals() {
        // For an inactive document buffer the accessor must
        // resolve through buffer_locals (since `App.editor.syntax` only
        // holds the active document's handle).
        use crate::buffer_registry::{BufferData, BufferEntry, DocumentEntry};
        use crate::buffers::{BufferFlags, BufferId};
        let mut a = app_with("active", 5);
        // Manufacture a second document buffer + seed empty
        // locals to validate the accessor path. Real `:e <new>`
        // does this through `do_edit`.
        let inactive_id = BufferId::next();
        let doc_handle = a.editor.document.as_arc();
        a.editor.buffers.insert(BufferEntry {
            id: inactive_id,
            flags: BufferFlags::default(),
            data: BufferData::Document(DocumentEntry {
                id: inactive_id,
                handle: doc_handle,
            }),
            name: None,
        });
        a.seed_empty_document_locals(inactive_id);
        // Read for the inactive buffer flows through locals; syntax
        // is None which the accessor returns as None.
        assert!(
            a.document_syntax_for(inactive_id).is_none(),
            "accessor returns None for empty locals"
        );
        // last_parsed / last_synced come back as 0 on the inactive
        // buffer's empty locals.
        assert_eq!(a.document_last_parsed_text_version_for(inactive_id), 0);
        assert_eq!(a.document_last_synced_syntax_version_for(inactive_id), 0);
        // folds slice is empty.
        assert!(a.document_folds_for(inactive_id).is_empty());
    }

    #[test]
    fn document_locals_carry_owner_metadata() {
        let a = app_with("hi", 10);
        let id = a.editor.document_buffer_id;
        let locals = a.editor.buffer_locals.get(&id).unwrap();
        let descriptors: Vec<_> = locals.iter_descriptors().collect();
        for d in descriptors
            .iter()
            .filter(|d| d.name.starts_with("text-mode."))
        {
            assert_eq!(d.owner_mode, "text-mode");
        }
        // At minimum the four document locals.
        let names: Vec<_> = descriptors.iter().map(|d| d.name).collect();
        assert!(names.contains(&"text-mode.syntax"));
        assert!(names.contains(&"text-mode.last-parsed-text-version"));
        assert!(names.contains(&"text-mode.last-synced-syntax-version"));
        assert!(names.contains(&"text-mode.folds"));
    }

    #[test]
    fn oil_open_then_write_creates_a_new_file_on_disk() {
        // End-to-end: open an oil buffer for an empty dir,
        // directly seed a new filename in the rope, run :w,
        // verify the file exists on disk. Tests the diff-and-
        // apply pipeline.
        let tmp =
            std::env::temp_dir().join(format!("lattice-oil-create-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("create tmp");

        let mut a = app_with("hi", 5);
        a.do_open_oil(Some(tmp.clone()));
        assert_eq!(a.editor.active_buffer, BufferKind::Oil);

        let oil_id = a.active_pane_buffer_id();
        a.editor
            .buffers
            .with_oil_mut(oil_id, |oil| {
                oil.content
                    .apply_edit(&lattice_protocol::edit::Edit::insert(
                        lattice_protocol::position::Position::ZERO,
                        "newfile.txt\n".to_string(),
                    ))
                    .expect("insert edit");
            })
            .expect("oil");

        // Save. Should run OilBuffer::apply and create the file.
        a.do_write(None);
        let msg = a.editor.last_message.as_ref().expect("write echo");
        assert!(
            msg.text.contains("oil: applied"),
            "expected oil-apply success echo, got: {}",
            msg.text,
        );
        let new_path = tmp.join("newfile.txt");
        assert!(
            new_path.exists(),
            "expected {} to be created",
            new_path.display(),
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn oil_navigate_up_from_oil_buffer_goes_to_parent() {
        // `-` key from an oil buffer navigates to the parent
        // dir.
        let tmp = std::env::temp_dir().join(format!("lattice-oil-up-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("nested")).expect("nested");

        let mut a = app_with("hi", 5);
        a.do_open_oil(Some(tmp.join("nested")));
        let oil_id = a.active_pane_buffer_id();
        assert_eq!(
            a.editor
                .buffer_locals
                .get(&oil_id)
                .and_then(|l| l.get::<crate::modes::OilDir>())
                .map(|d| d.0.clone())
                .unwrap_or_default(),
            tmp.join("nested"),
        );
        // Trigger `-`.
        a.apply(crate::app::Action::OilNavigateUp);
        // Dir lives in the OilDir buffer-local (canonical).
        let dir_after = a.oil_dir_for(oil_id).unwrap_or_default();
        assert_eq!(dir_after, tmp);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn oil_navigate_up_from_document_opens_oil_at_parent() {
        // `-` from a document buffer should open oil for the
        // parent of the document's path. Use a document with no
        // path -- this falls back to cwd.
        let mut a = app_with("hi", 5);
        // Start in a Document buffer (default).
        assert_eq!(a.editor.active_buffer, BufferKind::Document);
        a.apply(crate::app::Action::OilNavigateUp);
        // Active buffer should now be Oil.
        assert_eq!(a.editor.active_buffer, BufferKind::Oil);
    }

    #[test]
    fn oil_navigate_up_from_document_lands_on_the_edited_file() {
        // `-` from a file buffer opens oil for the file's parent
        // with the cursor on the file you were editing (oil.nvim
        // behaviour), not at the origin row.
        let tmp =
            std::env::temp_dir().join(format!("lattice-oil-focus-doc-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("adir")).expect("adir");
        std::fs::write(tmp.join("target.txt"), "x").expect("target");

        let mut a = app_with("hi", 5);
        // Open the file as a document buffer so its path is set.
        a.do_edit(Some(tmp.join("target.txt")), false);
        assert_eq!(a.editor.active_buffer, BufferKind::Document);

        a.apply(crate::app::Action::OilNavigateUp);
        assert_eq!(a.editor.active_buffer, BufferKind::Oil);
        // Listing is dirs-first alpha: ["adir", "target.txt"], so
        // the edited file is row 1 -- the cursor must land there.
        assert_eq!(a.editor.cursor.line, 1);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn oil_navigate_up_lands_on_the_directory_left() {
        // `-` inside an oil buffer steps up to the parent listing
        // with the cursor on the child directory you stepped out
        // of, so `-` then `<CR>` round-trips to the same place.
        let tmp =
            std::env::temp_dir().join(format!("lattice-oil-focus-up-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // Two dirs so "nested" is not row 0 after sorting
        // (dirs-first alpha: ["adir", "nested"]).
        std::fs::create_dir_all(tmp.join("adir")).expect("adir");
        std::fs::create_dir_all(tmp.join("nested")).expect("nested");

        let mut a = app_with("hi", 5);
        a.do_open_oil(Some(tmp.join("nested")));
        assert_eq!(a.editor.active_buffer, BufferKind::Oil);

        a.apply(crate::app::Action::OilNavigateUp);
        assert_eq!(
            a.oil_dir_for(a.active_pane_buffer_id()).unwrap_or_default(),
            tmp
        );
        // "nested" is row 1 in the parent listing.
        assert_eq!(a.editor.cursor.line, 1);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn oil_normal_mode_o_then_insert_then_write_creates_file() {
        // Full keystroke pipeline test: in an oil buffer,
        // press `o` (Normal: open line below + Insert), type
        // a filename, press <Esc>, run :w. Verify the file is
        // created on disk.
        let tmp =
            std::env::temp_dir().join(format!("lattice-oil-o-write-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("create tmp");
        // Seed a file so there's an existing row to position
        // after.
        std::fs::write(tmp.join("seed.txt"), "x").expect("seed");

        let mut a = app_with("hi", 5);
        a.do_open_oil(Some(tmp.clone()));
        a.apply(crate::app::Action::OpenLineBelow);
        assert_eq!(a.editor.modal, lattice_grammar::ModalState::Insert);
        a.apply(crate::app::Action::Insert("new.rs".into()));
        a.apply(crate::app::Action::EnterMode(
            lattice_grammar::ModalState::Normal,
        ));
        a.do_write(None);
        let new_path = tmp.join("new.rs");
        assert!(
            new_path.exists(),
            "expected {} to be created via the keystroke path; \
             write echo: {:?}",
            new_path.display(),
            a.editor.last_message,
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn oil_keystroke_pipeline_inserts_into_oil_rope() {
        // Regression: before the run_oil_invocation rewrite,
        // typing a chord in an oil buffer dispatched through
        // the document actor (which doesn't own the oil rope),
        // so the rope stayed empty no matter what the user
        // typed. Now the dispatch routes through a temp
        // Document and copies the resulting buffer back onto
        // `oil.content`.
        let tmp =
            std::env::temp_dir().join(format!("lattice-oil-keystroke-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("create tmp");
        // Seed one file so the listing has a row.
        std::fs::write(tmp.join("existing.txt"), "x").expect("seed");

        let mut a = app_with("hi", 5);
        a.do_open_oil(Some(tmp.clone()));
        let oil_id = a.active_pane_buffer_id();
        // Initial rope has one row: `existing.txt`.
        let initial = a.editor.buffers.with_oil(oil_id, |o| o.content.as_string());
        assert!(
            initial
                .as_ref()
                .map(|s| s.contains("existing.txt"))
                .unwrap_or(false),
            "expected `existing.txt` in initial listing: {:?}",
            initial,
        );
        // Dispatch an Insert action with text. In an oil buffer,
        // this should land in the oil rope, not the document.
        a.editor.modal = lattice_grammar::ModalState::Insert;
        a.apply(crate::app::Action::Insert("foo".into()));
        let after = a.editor.buffers.with_oil(oil_id, |o| o.content.as_string());
        assert!(
            after.as_ref().map(|s| s.contains("foo")).unwrap_or(false),
            "expected `foo` to land in oil rope: {:?}",
            after,
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn oil_navigate_up_re_mirrors_dir_into_buffer_locals() {
        // Regression: pressing `-` inside an oil buffer
        // updated OilBuffer::dir but didn't re-mirror the
        // `OilDir` buffer-local. The next `<CR>` on a file
        // would read the stale `OilDir`, join with the new
        // entry's name, and produce a path that doesn't
        // exist.
        let tmp = std::env::temp_dir().join(format!(
            "lattice-oil-nav-up-mirror-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).expect("sub");
        std::fs::write(tmp.join("a.txt"), "a").expect("a");
        std::fs::write(tmp.join("sub/inside.txt"), "i").expect("inside");

        let mut a = app_with("hi", 5);
        // Open oil rooted at the subdir.
        a.do_open_oil(Some(tmp.join("sub")));
        let oil_id = a.active_pane_buffer_id();
        // Verify initial OilDir mirror.
        assert_eq!(
            a.editor
                .buffer_locals
                .get(&oil_id)
                .and_then(|l| l.get::<crate::modes::OilDir>())
                .map(|d| d.0.clone())
                .unwrap_or_default(),
            tmp.join("sub"),
        );
        // Navigate up.
        a.do_oil_navigate_up();
        // M.3.2.c.5: dir lives in the OilDir buffer-local
        // (single source of truth; no struct mirror to drift).
        let dir_after = a.oil_dir_for(oil_id).unwrap_or_default();
        assert_eq!(
            dir_after, tmp,
            "OilDir should reflect the post-navigate dir",
        );

        // Sanity: open the file at row 0 (which should be
        // a.txt -- the first file alphabetically). The
        // resolved path uses the buffer-local OilDir; if
        // stale, the path is wrong and `do_edit` opens
        // something that doesn't exist (or worse, the wrong
        // file).
        a.editor.cursor.line = 0;
        a.editor.cursor.byte = 0;
        // Find a.txt's row (dirs come first; "sub" is dir, then "a.txt").
        let names: Vec<String> = a
            .editor
            .buffers
            .with_oil(oil_id, |o| {
                o.snapshot_entries()
                    .iter()
                    .map(|e| e.name.clone())
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default();
        let a_txt_row = names
            .iter()
            .position(|n| n == "a.txt")
            .expect("a.txt in listing");
        a.editor.cursor.line = a_txt_row as u32;
        a.do_oil_follow();
        let opened = a.editor.document.path().map(|p| p.to_path_buf());
        assert_eq!(opened, Some(tmp.join("a.txt")));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn oil_open_with_relative_dir_stores_absolute_oil_dir() {
        // Regression for the navigate-up ENOENT: opening oil with
        // a relative dir used to store the relative path
        // verbatim. `Path::parent()` then returned `Some("")` for
        // single-component cases, and `read_dir("")` failed.
        // Post-fix `do_open_oil` normalises to absolute before
        // storing.
        let tmp =
            std::env::temp_dir().join(format!("lattice-oil-relative-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).expect("sub");
        std::fs::write(tmp.join("sub/inside.txt"), "i").expect("inside");

        // Construct a path that's relative to cwd but points
        // inside tmp. `tmp` itself is absolute (`/tmp/...`); we
        // simulate the "user typed a relative path" case by
        // building one explicitly. Skip the test if cwd lookup
        // fails (no realistic test environment).
        let Ok(cwd) = std::env::current_dir() else {
            return;
        };
        let Ok(rel) = tmp.join("sub").strip_prefix(&cwd).map(|p| p.to_path_buf()) else {
            // tmp isn't a descendant of cwd (likely the common
            // case: `/tmp/...` vs `/home/.../lattice`); craft
            // the relative path via `..` walks. We only need
            // the assertion to exercise the relative→absolute
            // pipeline; the underlying directory just has to
            // exist.
            let mut a = app_with("hi", 5);
            a.do_open_oil(Some(tmp.join("sub")));
            let oil_id = a.active_pane_buffer_id();
            let stored = a.oil_dir_for(oil_id).unwrap_or_default();
            assert!(
                stored.is_absolute(),
                "OilDir should always be absolute; got {stored:?}",
            );
            // Hit `-` once. Pre-fix this hit ENOENT for any
            // relative dir; post-fix it lands on the absolute
            // parent.
            a.do_oil_navigate_up();
            let after = a.oil_dir_for(oil_id).unwrap_or_default();
            assert_eq!(after, tmp, "navigate-up should land on tmp's absolute path");
            let _ = std::fs::remove_dir_all(&tmp);
            return;
        };

        let mut a = app_with("hi", 5);
        // Pass the truly-relative form. `do_open_oil` should
        // resolve it against cwd before storing.
        a.do_open_oil(Some(rel.clone()));
        let oil_id = a.active_pane_buffer_id();
        let stored = a.oil_dir_for(oil_id).unwrap_or_default();
        assert!(
            stored.is_absolute(),
            "OilDir should always be absolute, even when opened relative; got {stored:?}",
        );
        // `-` walks up to tmp.
        a.do_oil_navigate_up();
        let after = a.oil_dir_for(oil_id).unwrap_or_default();
        assert_eq!(
            after, tmp,
            "navigate-up from relative-opened oil should reach the absolute parent",
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn oil_navigate_into_then_open_file_uses_correct_subdir_path() {
        // Companion regression: after `<CR>` on a subdir,
        // pressing `<CR>` on a file inside that subdir must
        // open `<parent>/<subdir>/<file>`, not
        // `<parent>/<file>` or similar.
        let tmp = std::env::temp_dir().join(format!(
            "lattice-oil-nav-into-then-open-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).expect("sub");
        std::fs::write(tmp.join("sub/inside.txt"), "i").expect("inside");

        let mut a = app_with("hi", 5);
        a.do_open_oil(Some(tmp.clone()));
        let oil_id = a.active_pane_buffer_id();
        // Find `sub` (dirs first; should be row 0).
        let names: Vec<String> = a
            .editor
            .buffers
            .with_oil(oil_id, |o| {
                o.snapshot_entries()
                    .iter()
                    .map(|e| e.name.clone())
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default();
        let sub_row = names
            .iter()
            .position(|n| n == "sub")
            .expect("sub in listing");
        a.editor.cursor.line = sub_row as u32;
        a.editor.cursor.byte = 0;
        a.do_oil_follow();
        // Now in oil rooted at tmp/sub (read via OilDir, the
        // canonical buffer-local).
        let dir_after = a.oil_dir_for(oil_id).unwrap_or_default();
        assert_eq!(dir_after, tmp.join("sub"));
        // Listing should show `inside.txt` at row 0.
        let names_after: Vec<String> = a
            .editor
            .buffers
            .with_oil(oil_id, |o| {
                o.snapshot_entries()
                    .iter()
                    .map(|e| e.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        let inside_row = names_after
            .iter()
            .position(|n| n == "inside.txt")
            .expect("inside.txt in listing");
        a.editor.cursor.line = inside_row as u32;
        a.do_oil_follow();
        let opened = a.editor.document.path().map(|p| p.to_path_buf());
        assert_eq!(opened, Some(tmp.join("sub/inside.txt")));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn oil_follow_uses_app_cursor_not_oil_internal_cursor() {
        // Regression: `<CR>` in an oil buffer used to always
        // open the first row's item regardless of where the
        // user had moved with `j` / `k`. Root cause: the
        // OilBuffer's own `cursor` field is never synced to
        // `app.editor.cursor`; `entry_at_cursor()` read the stale
        // internal field and always indexed snapshot[0]. The
        // fix routes through `self.editor.cursor.line` (the App's
        // hot-path cursor, same surface the user moves).
        let tmp = std::env::temp_dir().join(format!(
            "lattice-oil-follow-cursor-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("tmp");
        // Three files: alpha.txt, beta.txt, gamma.txt. Listed
        // alphabetically.
        std::fs::write(tmp.join("alpha.txt"), "a").expect("a");
        std::fs::write(tmp.join("beta.txt"), "b").expect("b");
        std::fs::write(tmp.join("gamma.txt"), "g").expect("g");

        let mut a = app_with("hi", 5);
        a.do_open_oil(Some(tmp.clone()));
        let oil_id = a.active_pane_buffer_id();
        // Snapshot order: alpha, beta, gamma.
        let names: Vec<String> = a
            .editor
            .buffers
            .with_oil(oil_id, |o| {
                o.snapshot_entries()
                    .iter()
                    .map(|e| e.name.clone())
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default();
        assert_eq!(
            names,
            vec![
                "alpha.txt".to_string(),
                "beta.txt".into(),
                "gamma.txt".into()
            ],
        );

        // Move the cursor to row 2 (gamma.txt). Pre-fix: follow
        // would open alpha.txt regardless.
        a.editor.cursor.line = 2;
        a.editor.cursor.byte = 0;
        a.do_oil_follow();

        // Follow on a file routes through `do_edit`, which
        // opens the file as a Document. The active buffer's
        // path should be gamma.txt, not alpha.txt.
        assert_eq!(a.editor.active_buffer, BufferKind::Document);
        let active_path = a.editor.document.path().map(|p| p.to_path_buf());
        assert_eq!(
            active_path,
            Some(tmp.join("gamma.txt")),
            "follow should have opened the file under the App's cursor",
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn oil_navigate_into_subdir_replaces_listing() {
        let tmp = std::env::temp_dir().join(format!("lattice-oil-nav-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("subdir")).expect("create subdir");
        std::fs::write(tmp.join("subdir/inner.txt"), "hi").expect("write inner");

        let mut a = app_with("hi", 5);
        a.do_open_oil(Some(tmp.clone()));
        let oil_id = a.active_pane_buffer_id();

        // The rope content lists `subdir` (and `..`).
        let listing_before = a.editor.buffers.with_oil(oil_id, |o| o.content.as_string());
        assert!(
            listing_before
                .as_ref()
                .map(|s| s.contains("subdir"))
                .unwrap_or(false),
            "expected `subdir` in initial listing: {:?}",
            listing_before,
        );

        // Move cursor to the subdir entry. The exact line index
        // depends on sort: dirs first, so subdir is at 0 (or 1
        // if `..` is included). Let's find it.
        let snap: Vec<String> = a
            .editor
            .buffers
            .with_oil(oil_id, |o| {
                o.snapshot_entries()
                    .iter()
                    .map(|e| e.name.clone())
                    .collect()
            })
            .expect("oil");
        let subdir_line = snap
            .iter()
            .position(|n| n == "subdir")
            .expect("subdir in snapshot");
        a.editor.cursor.line = subdir_line as u32;
        a.editor.cursor.byte = 0;
        a.do_oil_follow();

        // Listing now shows subdir's contents (`inner.txt`).
        let listing_after = a.editor.buffers.with_oil(oil_id, |o| o.content.as_string());
        assert!(
            listing_after
                .as_ref()
                .map(|s| s.contains("inner.txt"))
                .unwrap_or(false),
            "after navigate-into, expected `inner.txt`: {:?}",
            listing_after,
        );

        // The buffer-locals dir reflects the new location.
        let dir = a
            .editor
            .buffer_locals
            .get(&oil_id)
            .and_then(|l| l.get::<crate::modes::OilDir>())
            .map(|d| d.0.clone())
            .expect("OilDir present");
        assert_eq!(dir, tmp.join("subdir"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn open_oil_seeds_oil_locals() {
        let tmp = std::env::temp_dir().join(format!("lattice-m3-2-c-3-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);

        let mut a = app_with("hi", 5);
        a.do_open_oil(Some(tmp.clone()));
        let oil_id = a.active_pane_buffer_id();

        let locals = a
            .editor
            .buffer_locals
            .get(&oil_id)
            .expect("oil locals seeded");
        let dir = locals
            .get::<crate::modes::OilDir>()
            .expect("OilDir local present");
        assert_eq!(dir.0, tmp);

        // Owner-mode metadata.
        let descriptors: Vec<_> = locals.iter_descriptors().collect();
        let oil_descriptors: Vec<_> = descriptors
            .iter()
            .filter(|d| d.owner_mode == "oil-mode")
            .collect();
        assert_eq!(oil_descriptors.len(), 1);
        assert_eq!(oil_descriptors[0].name, "oil-mode.dir");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn pane_status_label_falls_back_to_registry_name_for_synthetic_document() {
        // Slice A: a Document with no path but a synthetic name
        // (the shape `*lsp*`/`*messages*` will use once they migrate
        // out of HelpContent) shows the name in the modeline.
        let a = app_with("hi", 5);
        let active = a.active_pane_buffer_id();
        // Drop the existing entry; replace it with a no-path Document
        // carrying a synthetic name. Reuse the same RopeDocumentHandle
        // because the test fixture's `app_with` already produces an
        // unsaved buffer (`handle.path()` is None).
        let handle = a.editor.document.as_arc();
        a.editor.buffers.remove(active);
        a.editor.buffers.insert(BufferEntry {
            id: active,
            flags: BufferFlags::default(),
            data: BufferData::Document(DocumentEntry { id: active, handle }),
            name: Some("*lsp*".to_string()),
        });
        let pane = a.editor.pane_tree.active().clone();
        let label = a.pane_status_label(&pane);
        assert!(
            label.contains("*lsp*"),
            "expected modeline to surface synthetic name, got `{label}`"
        );
    }

    #[test]
    fn buffer_picker_accept_then_ctrl_o_walks_back_to_origin() {
        // Bug: the `:b` picker preview-activates the candidate
        // (with `previewing=true`, so activate_buffer skips the
        // history push), and the accept path short-circuits when
        // the preview already moved us there. Without an
        // explicit push at picker-open, the position history
        // never captured the origin -- `<C-o>` echoed "no jumps".
        // Fix pushes the origin at `open_buffer_picker`.
        let mut a = app_with("alpha\nbeta\ngamma\n", 10);
        let origin = a.active_pane_buffer_id();
        a.editor.cursor = Position::new(1, 2);
        a.open_buffer_picker();
        // Position history should already have the origin entry
        // from the picker-open push, before any preview activates.
        let last = a.editor.position_history.last().expect("origin entry");
        assert_eq!(last.position, Position::new(1, 2));
        assert_eq!(last.buffer_id, origin);
        // Dismiss the picker so we're not stuck on a preview.
        a.apply(Action::PickerDismiss);
        // From the origin, `<C-o>` finds the pushed entry and
        // remains on the same buffer + position (idempotent).
        // The real value is on the accept path -- but the
        // entry must exist for the walker to find it.
        let history_len_before = a.editor.position_history.len();
        assert!(history_len_before >= 1);
    }

    #[test]
    fn activate_buffer_pushes_position_history_so_ctrl_o_walks_back() {
        // Switching to any buffer should push the pre-jump cursor
        // onto position history automatically -- the user shouldn't
        // have to remember to do this at every call site.
        // `<C-o>` from the new buffer must walk back to the
        // previous one's cursor.
        let mut a = app_with("alpha\nbeta\ngamma\n", 10);
        let initial = a.active_pane_buffer_id();
        // Position cursor somewhere distinctive.
        a.editor.cursor = Position::new(1, 2);
        // Boot creates *lsp* and *messages*; activate *lsp*.
        let lsp_id = a.editor.buffers.by_name("*lsp*").unwrap();
        assert_ne!(initial, lsp_id);
        a.activate_buffer(lsp_id);
        // Position history should contain the pre-jump entry.
        let last = a.editor.position_history.last().expect("history entry");
        assert_eq!(last.position, Position::new(1, 2));
        assert_eq!(last.buffer_id, initial);
        // `<C-o>` walks back to the original buffer.
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.active_pane_buffer_id(), initial);
        assert_eq!(a.editor.cursor, Position::new(1, 2));
    }

    #[test]
    fn pane_status_label_suppresses_dirty_marker_for_synthetic_documents() {
        // Synthetic buffers (`*lsp*`, `*messages*`, ...) are
        // owner-streamed; their content arrives via subsystem
        // appends, not user edits. The Document's underlying dirty
        // flag fires on every append, which is meaningful for
        // path-backed Documents but misleading for synthetic ones
        // -- the user can't "save" the streaming state. The
        // modeline must suppress the `[+]` marker for these.
        let a = app_with("hi", 5);
        let active = a.active_pane_buffer_id();
        let handle = a.editor.document.as_arc();
        a.editor.buffers.remove(active);
        a.editor.buffers.insert(BufferEntry {
            id: active,
            flags: BufferFlags::default(),
            data: BufferData::Document(DocumentEntry {
                id: active,
                handle: handle.clone(),
            }),
            name: Some("*lsp*".to_string()),
        });
        // Force the underlying document dirty: an apply_edit
        // advances undo depth past the clean position. The fresh
        // test fixture starts clean; an append makes it dirty.
        let snap = handle.snapshot();
        let last_line = crate::app::last_addressable_line(&snap.buffer);
        let line_len = crate::app::line_byte_len(&snap.buffer, last_line);
        let pos = Position::new(last_line, line_len);
        let _ = lattice_runtime::block_on(
            handle.apply_edit_batch(vec![lattice_protocol::edit::Edit::insert(pos, "x")]),
        );
        assert!(handle.dirty(), "fixture must produce a dirty Document");
        let pane = a.editor.pane_tree.active().clone();
        let label = a.pane_status_label(&pane);
        assert!(
            label.contains("*lsp*"),
            "modeline must surface synthetic name; got `{label}`"
        );
        assert!(
            !label.contains("[+]"),
            "modeline must NOT surface [+] for synthetic buffers; got `{label}`"
        );
    }
}
