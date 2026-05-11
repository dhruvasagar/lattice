//! Picker / fuzzy-finder App surface -- buffer picker
//! (`:b` no-arg), LSP-instance picker (used by `:lsp-log`,
//! `:lsp-server-log`, `:lsp-trace-log`), and LSP-location
//! pickers (multi-result `gd` / `gr` / `:diagnostics` /
//! symbol pickers / code actions / completion items).
//!
//! Methods that live here:
//! - `open_buffer_picker` (`:b` no-arg, the vertico-style
//!   buffer switcher with live preview).
//! - `open_lsp_picker` (instance picker; `:lsp-log` etc.).
//! - `open_lsp_locations_picker` (multi-result navs).
//! - `snapshot_lsp_instances` (helper for the instance
//!   picker).
//! - `preview_picker_selection` (per-selection preview for
//!   the buffer picker -- no jump-list pollution).
//! - `do_picker_dismiss`, `do_picker_accept` (the two
//!   terminal actions; accept fans out by RoutingPayload
//!   into buffer / lsp-log / lsp-location / completion /
//!   code-action handlers).
//! - `raw_buffer_candidates` (free fn that builds the
//!   buffer-picker candidate set from the registry).
//!
//! What does NOT live here: the `Picker` type, matcher
//! engine, candidate scoring -- those live in the sibling
//! `lattice-picker` crate. This module is App's *workflow*
//! layer above that.

use crate::buffer_registry::{BufferData, BufferRegistry};
use crate::buffers::BufferId;

use super::{App, EchoLevel};

impl App {
    /// Build the snapshot the picker primitive hands to source
    /// generators on each `:picker <source>` invocation.
    /// Caller holds `snap` for the duration of the synchronous
    /// `init` call -- the returned `PickerContext` borrows from
    /// both `self` and `snap`.
    ///
    /// Owned vec fields (`buffers`, `marks`, `registers`,
    /// `position_history`) translate from the App's richer
    /// types into the picker's renderer-agnostic views in this
    /// single pass. Total allocation is O(N) over each at
    /// trivial sizes (<100 buffers, <26 marks, <40 registers,
    /// <100 history entries).
    pub fn build_picker_context<'a>(
        &'a self,
        snap: &'a lattice_runtime::DocumentSnapshot,
    ) -> lattice_picker::PickerContext<'a> {
        use lattice_picker::{
            ActiveBufferSnapshot, BufferEntry, PickerContext, PositionEntry, PositionSource,
        };

        let active_id = self.active_pane_buffer_id();
        let path: Option<&std::path::Path> = snap.path.as_ref().map(|p| p.as_path());
        let language = Some(lattice_syntax::Lang::detect_from_path(path).label());
        // Selection: the most recent visual-mode range. While in
        // Command mode (the only state from which `:picker ...` can
        // fire) the cursor is not in Visual, so `last_visual`
        // captures the prior selection -- exactly what `:grep`
        // would want as its default pattern when invoked after
        // selecting a word.
        let selection = self
            .last_visual
            .as_ref()
            .map(|v| (v.anchor, v.head));

        let active_buffer = ActiveBufferSnapshot {
            buffer_id: active_id.0,
            path,
            language,
            cursor: self.cursor,
            selection,
            buffer: &snap.buffer,
        };

        let workspace_root = self.picker_workspace_root_path(snap);

        // Buffer registry -> picker BufferEntry view.
        let buffers: Vec<BufferEntry> = self
            .buffers
            .iter()
            .map(|entry| picker_buffer_entry(entry, &self.buffer_locals))
            .collect();

        // Marks: HashMap<char, Position> -> Vec<(char, Position)>.
        let mut marks: Vec<(char, lattice_protocol::Position)> =
            self.marks.iter().map(|(c, p)| (*c, *p)).collect();
        marks.sort_by_key(|(c, _)| *c);

        // Registers: unnamed + named, both as (name, preview).
        let mut registers: Vec<(String, String)> = Vec::new();
        if let Some(r) = &self.unnamed_register {
            registers.push(("\"".into(), super::preview_register(&r.content)));
        }
        let mut named: Vec<(super::Register, String)> = self
            .registers
            .iter()
            .map(|(k, v)| (*k, super::preview_register(&v.content)))
            .collect();
        named.sort_by_key(|(k, _)| match k {
            super::Register::Named(c) => format!("a{c}"),
            super::Register::Numbered(n) => format!("b{n}"),
            super::Register::System => "z+".into(),
            _ => "z".into(),
        });
        for (key, preview) in named {
            let name = match key {
                super::Register::Named(c) => c.to_string(),
                super::Register::Numbered(n) => n.to_string(),
                super::Register::System => "+".into(),
                _ => "?".into(),
            };
            registers.push((name, preview));
        }

        // Position history: translate from the App's richer
        // PositionEntry (which carries BufferKind) into the
        // picker's renderer-agnostic view.
        let position_history: Vec<PositionEntry> = self
            .position_history
            .iter()
            .map(|e| PositionEntry {
                buffer_id: e.buffer_id.0,
                line: e.position.line,
                col: e.position.byte,
                source: match e.source {
                    super::PositionSource::AutoJump => PositionSource::AutoJump,
                    super::PositionSource::ExplicitMark => PositionSource::ExplicitMark,
                    super::PositionSource::PluginPush => PositionSource::PluginPush,
                    super::PositionSource::NamedMark(c) => PositionSource::NamedMark(c),
                },
            })
            .collect();

        PickerContext {
            active_buffer,
            workspace_root,
            recent_files: &self.recent_files,
            position_history,
            buffers,
            marks,
            registers,
        }
    }

    /// Translate a picker source's typed outcome into App-state
    /// mutation. Single dispatch site -- adding a new outcome
    /// variant requires editing exactly this match.
    pub(super) fn apply_picker_outcome(
        &mut self,
        outcome: lattice_picker::PickerAcceptOutcome,
    ) {
        use lattice_picker::PickerAcceptOutcome::*;
        match outcome {
            OpenFile { path } => {
                self.prepare_pane_for_picker_result();
                self.do_edit(Some(path), false);
            }
            SwitchBuffer { buffer_id } => {
                let id = BufferId(buffer_id);
                if id != self.active_pane_buffer_id() {
                    self.prepare_pane_for_picker_result();
                    self.activate_buffer(id);
                }
            }
            JumpInBuffer { buffer_id, line, col } => {
                self.push_position_history(self.cursor, super::PositionSource::PluginPush);
                let id = BufferId(buffer_id);
                if id != self.active_pane_buffer_id() {
                    self.prepare_pane_for_picker_result();
                    self.activate_buffer(id);
                }
                let snap = self.document.snapshot();
                let line = line.min(super::last_addressable_line(&snap.buffer));
                let len = super::line_byte_len(&snap.buffer, line);
                let col = col.min(len);
                self.cursor = lattice_protocol::Position::new(line, col);
            }
            JumpToLocation { path, line, col } => {
                self.prepare_pane_for_picker_result();
                self.jump_to_file_line_col(&path, line, col);
            }
            OpenLspLog { server_id } => self.open_lsp_log_in_pane(&server_id),
            OpenLspTraceLog { server_id } => self.open_lsp_trace_log_in_pane(&server_id),
            NoOp => {}
            // Outcome variants whose first emitting source lands
            // with a later P.x slice. Each migration wires its
            // own translation; until then the host echoes a
            // clear "not yet wired" message so a misrouted
            // outcome surfaces loudly in development.
            JumpToMark { name } => {
                // Reuse the existing keyboard-driven mark-jump
                // path so cursor placement, position-history
                // push, and "mark not set" error all match the
                // `` ` `` motion's behavior. `exact = true`
                // mirrors vim's back-tick semantics (jump to
                // the recorded byte, not just the line).
                self.do_jump_mark(name, true);
            }
            InvokeCommand { id, .. } => {
                // Strip the `ex:` registration prefix so the parser
                // sees the user-facing command word. The parser
                // accepts the canonical id as well, but the alias
                // is what `:apropos` / `:describe-command` would
                // show, so we route through it for consistency.
                // Args are intentionally ignored at this layer
                // today: the command palette emits `Args::None`,
                // and future "pick a thing, then run a command on
                // it" flows can serialize args into the
                // execute_ex_line string.
                let user_facing = id.strip_prefix("ex:").unwrap_or(&id).to_string();
                self.execute_ex_line(&user_facing);
            }
            PasteRegister { name } => {
                // Stash the chosen register on `pending_register`
                // so `do_paste` picks it up the same way `"<X>p`
                // does in Normal mode. `do_paste(false)` is the
                // `p` (paste-after) flavor; the picker doesn't
                // distinguish before/after today, matching the
                // simplest user expectation. Charwise / linewise /
                // blockwise routing flows through the existing
                // paste path. Invalid register names (`_`,
                // unknown chars) echo without panicking.
                if let Some(reg) = lattice_grammar::register::Register::from_input_char(name) {
                    self.pending_register = Some(reg);
                    self.do_paste(false);
                } else {
                    self.set_message(
                        EchoLevel::Error,
                        format!("picker: register `{name}` is not pasteable"),
                    );
                }
            }
            ExpandSnippet { id } => self.set_message(
                EchoLevel::Error,
                format!("picker: ExpandSnippet `{id}` not yet wired (lands with snippets picker)"),
            ),
            ApplyLspCodeAction { handle, index } => self.set_message(
                EchoLevel::Error,
                format!(
                    "picker: ApplyLspCodeAction handle={handle} idx={index} \
                     not yet wired (lands with LSP picker migration)"
                ),
            ),
            ApplyLspCompletion { index } => self.set_message(
                EchoLevel::Error,
                format!(
                    "picker: ApplyLspCompletion idx={index} \
                     not yet wired (lands with LSP picker migration)"
                ),
            ),
        }
    }

    /// Best-effort workspace root for picker sources.
    /// Active document's parent if it has one; current working
    /// directory otherwise; `.` if cwd resolution fails.
    /// Returned owned so the context's `workspace_root` field
    /// has a stable home regardless of fallback.
    fn picker_workspace_root_path(
        &self,
        snap: &lattice_runtime::DocumentSnapshot,
    ) -> std::path::PathBuf {
        if let Some(arc) = snap.path.as_ref()
            && let Some(parent) = arc.parent()
        {
            return parent.to_path_buf();
        }
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    }

    /// Snapshot of every running LSP actor as picker rows.
    /// Built by reading the supervisor's `ArcSwap<SupervisorSnapshot>`,
    /// so the read is wait-free; the previous `try_lock`
    /// fall-through (degrade to empty if supervisor was
    /// busy) is gone -- the snapshot is always readable.
    fn snapshot_lsp_instances(&mut self) -> Vec<lattice_picker::LspInstanceRow> {
        let actors = self.lsp.running_actors();
        actors
            .into_iter()
            .map(|((workspace, server_id), handle)| {
                let key = (workspace.clone(), server_id.clone());
                let buffer_count = self.lsp.buffer_count_for(&key);
                let caps = handle.capabilities();
                let cap_summary = lattice_lsp::help_views::summarise_capabilities(&caps);
                lattice_picker::LspInstanceRow {
                    workspace,
                    server_id,
                    buffer_count,
                    cap_summary,
                }
            })
            .collect()
    }

    /// Build + open an LSP location picker (multi-result `gd` /
    /// `gr` / `:diagnostics` / future symbol pickers).
    ///
    /// Reads the line text from each location's file once
    /// (cached per file in a `HashMap`) so the displayed rows
    /// look like ripgrep output. Empty `locations` is a no-op
    /// (caller already echoed "no X found" in that case).
    pub(super) fn open_lsp_locations_picker(
        &mut self,
        title: impl Into<String>,
        locations: &[lsp_types::Location],
    ) {
        if locations.is_empty() {
            return;
        }
        let mut file_cache: std::collections::HashMap<std::path::PathBuf, Vec<String>> =
            std::collections::HashMap::new();
        let rows: Vec<lattice_picker::LspLocationRow> = locations
            .iter()
            .filter_map(|loc| {
                let path = lattice_lsp::actor::uri_to_path(&loc.uri)?;
                let line = loc.range.start.line;
                let lines_cache = file_cache.entry(path.clone()).or_insert_with(|| {
                    std::fs::read_to_string(&path)
                        .ok()
                        .map(|s| s.lines().map(|l| l.to_string()).collect())
                        .unwrap_or_default()
                });
                let preview = lines_cache.get(line as usize).cloned().unwrap_or_default();
                // utf-16 char column → utf-8 byte column for jump.
                let line_text = lines_cache.get(line as usize).cloned().unwrap_or_default();
                let col = lattice_lsp::position::utf16_column_to_utf8_byte(
                    &line_text,
                    loc.range.start.character,
                );
                Some(lattice_picker::LspLocationRow {
                    path,
                    line,
                    col,
                    preview,
                    marginalia: String::new(),
                })
            })
            .collect();
        if rows.is_empty() {
            self.set_message(
                EchoLevel::Info,
                "no usable locations (non-file URIs?)".to_string(),
            );
            return;
        }
        let mut p = lattice_picker::Picker::new(
            title,
            lattice_picker::PickerSource::LspLocations,
            lattice_picker::PickerAction::JumpToLspLocation,
        );
        p.set_lsp_locations(rows);
        self.picker = Some(p);
    }

    /// Build + open an LSP instance picker. Called by `:lsp-log`,
    /// `:lsp-server-log`, and `:lsp-trace-log`. The `prefilter`
    /// arg pre-narrows the candidate list to one server id while
    /// still allowing the user to disambiguate between multiple
    /// workspaces. `on_accept` decides which buffer the chosen
    /// row opens (`OpenLspLog` or `OpenLspTraceLog`).
    pub(super) fn open_lsp_picker(
        &mut self,
        title: &str,
        prefilter: Option<String>,
        on_accept: lattice_picker::PickerAction,
    ) {
        let rows = self.snapshot_lsp_instances();
        if rows.is_empty() {
            self.set_message(
                EchoLevel::Info,
                "no LSP servers running; open a file with a matching language to attach"
                    .to_string(),
            );
            return;
        }
        // Resolve the user's prefilter through the alias table so
        // `:lsp-log rust-analyzer` finds the `rust` actor. On miss
        // we fall back to the literal string -- the picker UI then
        // shows "no match" with the unresolved name in the echo.
        let resolved_prefilter = prefilter.as_deref().and_then(|n| self.resolve_server_id(n));
        let effective = resolved_prefilter.clone().or_else(|| prefilter.clone());
        // Single match short-circuit: when prefilter narrows the
        // candidate set to exactly one row, skip the picker and
        // open the buffer directly. Vim-style "do what I mean"
        // (e.g. `:lsp-log rust` with one rust workspace).
        let matches: Vec<&lattice_picker::LspInstanceRow> = rows
            .iter()
            .filter(|r| {
                effective
                    .as_ref()
                    .is_none_or(|want| r.server_id == *want)
            })
            .collect();
        if matches.len() == 1 {
            let server_id = matches[0].server_id.clone();
            match on_accept {
                lattice_picker::PickerAction::OpenLspLog => {
                    self.open_lsp_log_in_pane(&server_id)
                }
                lattice_picker::PickerAction::OpenLspTraceLog => {
                    self.open_lsp_trace_log_in_pane(&server_id)
                }
                lattice_picker::PickerAction::SwitchToBuffer
                | lattice_picker::PickerAction::JumpToLspLocation
                | lattice_picker::PickerAction::AcceptLspCompletion
                | lattice_picker::PickerAction::AcceptLspCodeAction
                | lattice_picker::PickerAction::OpenFile => {}
            }
            return;
        }
        if matches.is_empty() {
            let asked = prefilter.clone().unwrap_or_default();
            let running = self.running_server_ids();
            let listing = if running.is_empty() {
                String::new()
            } else {
                format!(" (running: {})", running.join(", "))
            };
            self.set_message(
                EchoLevel::Info,
                format!("no LSP server matching {asked:?} running{listing}"),
            );
            return;
        }
        let mut p = lattice_picker::Picker::new(
            title,
            lattice_picker::PickerSource::LspInstances {
                prefilter: effective,
            },
            on_accept,
        );
        p.set_lsp_instances(rows);
        self.picker = Some(p);
    }

    /// `:b` with no arg (DESIGN.md §5.9.7) -- open the vertico-style
    /// buffer switcher. Type to filter; `<Up>` / `<Down>` (or
    /// `<C-p>` / `<C-n>`) to move; `<CR>` to switch to the
    /// selected buffer; `<Esc>` to dismiss. Marginalia shows the
    /// kind (`doc` / `tree` / `help`) plus a `(current)` tag on
    /// the active buffer.
    ///
    /// **Live preview.** While the picker is open, every selection
    /// change activates the candidate buffer in the active pane
    /// (without polluting the jump list). On accept, that
    /// activation becomes the real switch; on dismiss, the
    /// pane reverts to whatever buffer was active when the
    /// picker opened.
    /// `:picker <source> [args]` -- canonical entry point.
    /// Looks the source up in `picker_registry`, fetches its
    /// `PickerSourceGenerator`, builds a `PickerContext`, and
    /// dispatches `gen.init(...)`. Inline results seat the
    /// picker immediately; async / streaming variants are
    /// rejected with a clear echo until the first async source
    /// (P.8 / P.9 LSP-flavored or `:picker grep`) migrates and
    /// wires the spawn path.
    ///
    /// Unknown source ids surface an error echo listing every
    /// registered id so the user can recover without `:apropos`.
    pub(super) fn open_picker(&mut self, source: String, args: Vec<String>) {
        // Resolve generator from the registry. A registered
        // entry without a generator (metadata-only legacy
        // shape) surfaces a distinct echo so the drift is
        // visible.
        let Some(entry) = self.picker_registry.entry(&source) else {
            let known: Vec<&str> = self.picker_registry.ids().collect();
            let msg = if known.is_empty() {
                format!("picker: unknown source `{source}` (no sources registered)")
            } else {
                format!(
                    "picker: unknown source `{source}` (known: {})",
                    known.join(", ")
                )
            };
            self.set_message(EchoLevel::Error, msg);
            return;
        };
        let Some(generator) = entry.generator.clone() else {
            self.set_message(
                EchoLevel::Error,
                format!("picker: source `{source}` has no generator wired"),
            );
            return;
        };

        // Sync prelude: build the context against a fresh
        // snapshot, call init, drop the borrow.
        let snap = self.document.snapshot();
        let ctx = self.build_picker_context(&snap);
        let init_result = match generator.init(&ctx, &args) {
            Ok(r) => r,
            Err(e) => {
                self.set_message(EchoLevel::Info, e);
                return;
            }
        };
        drop(ctx);
        drop(snap);

        match init_result {
            lattice_picker::PickerInitResult::Inline(pairs) => {
                let title = source.clone();
                let mut picker = lattice_picker::Picker::new(
                    title,
                    Self::picker_source_for(&source),
                    Self::picker_action_for(&source),
                );
                picker.set_raw_candidates_with_routing(pairs);
                // Stamp the source id so accept can resolve the
                // generator and call `gen.accept(...)` instead
                // of running the legacy per-routing dispatch.
                picker.source_id = Some(source.clone());
                // Preserve the buffer-switcher's preview-origin
                // ergonomics: when the source is `buffers` the
                // picker stashes the active buffer id so dismiss
                // can restore it (alternate-buffer convention).
                if source == "buffers" {
                    picker.preview_origin = Some(self.active_pane_buffer_id().0);
                }
                self.picker = Some(picker);
                // Active-pane preview for buffer switcher.
                if source == "buffers" {
                    self.preview_picker_selection();
                }
            }
            lattice_picker::PickerInitResult::Future(_)
            | lattice_picker::PickerInitResult::Stream(_) => {
                self.set_message(
                    EchoLevel::Error,
                    format!(
                        "picker: async / streaming sources not yet wired (source `{source}` returned a Future / Stream)"
                    ),
                );
            }
        }
    }

    /// Translate a first-party source id into the
    /// `PickerSource` tag the picker primitive stores. The
    /// tag is mostly informational today (refresh paths read
    /// it); slice 13d will retire it entirely once every
    /// source registers as a trait object.
    fn picker_source_for(source: &str) -> lattice_picker::PickerSource {
        match source {
            "buffers" => lattice_picker::PickerSource::Buffers,
            _ => lattice_picker::PickerSource::Files,
        }
    }

    /// Translate a first-party source id into the
    /// `PickerAction` tag. Retires alongside `picker_source_for`
    /// once the trait-driven path is the only one.
    fn picker_action_for(source: &str) -> lattice_picker::PickerAction {
        match source {
            "buffers" => lattice_picker::PickerAction::SwitchToBuffer,
            _ => lattice_picker::PickerAction::OpenFile,
        }
    }

    /// `:files [root]` -- open the workspace file picker.
    /// Resolves `root` the same way `:Filetree` does (explicit
    /// path > current document parent > cwd). Walks the tree
    /// up to `FILE_PICKER_MAX_ENTRIES`, skipping the ignore set
    /// (`.git`, `target`, `node_modules`, `dist`, `.cache`) and
    /// dotfiles. Each row is a relative path; accept hands the
    /// absolute path to `do_edit`.
    pub(super) fn open_file_picker(&mut self, root: Option<std::path::PathBuf>) {
        let root = match root {
            Some(p) => p,
            None => match self.document.path().and_then(|p| p.parent().map(Into::into)) {
                Some(parent) => parent,
                None => match std::env::current_dir() {
                    Ok(p) => p,
                    Err(e) => {
                        self.set_message(EchoLevel::Error, format!("cwd error: {e}"));
                        return;
                    }
                },
            },
        };
        let canonical_root = std::fs::canonicalize(&root).unwrap_or(root.clone());
        let entries = walk_files_for_picker(&canonical_root);
        if entries.is_empty() {
            self.set_message(
                EchoLevel::Info,
                format!("files: no files under {}", canonical_root.display()),
            );
            return;
        }
        let mut items: Vec<(
            lattice_completion::RawCandidate,
            lattice_picker::RoutingPayload,
        )> = Vec::with_capacity(entries.len());
        for abs in entries {
            let rel = abs
                .strip_prefix(&canonical_root)
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| abs.clone());
            let display = rel.display().to_string();
            let cand = lattice_completion::RawCandidate::plain(
                display,
                lattice_completion::CandidateKind::Plain,
            );
            items.push((cand, lattice_picker::RoutingPayload::OpenFile { path: abs }));
        }
        let mut picker = lattice_picker::Picker::new(
            format!("files: {}", canonical_root.display()),
            lattice_picker::PickerSource::Files,
            lattice_picker::PickerAction::OpenFile,
        );
        picker.set_raw_candidates_with_routing(items);
        self.picker = Some(picker);
    }

    /// P.2: `:recent` -- open the recent-files picker. Walks
    /// `App.recent_files` (MRU, newest first); each row routes
    /// through `RoutingPayload::OpenFile`. Display is the
    /// canonical absolute path so the user can distinguish
    /// same-name files from different roots. Empty MRU echoes
    /// "no recent files."
    pub(super) fn open_recent_files_picker(&mut self) {
        if self.recent_files.is_empty() {
            self.set_message(EchoLevel::Info, "no recent files".to_string());
            return;
        }
        let items: Vec<(
            lattice_completion::RawCandidate,
            lattice_picker::RoutingPayload,
        )> = self
            .recent_files
            .iter()
            .map(|p| {
                let display = p.display().to_string();
                let cand = lattice_completion::RawCandidate::plain(
                    display,
                    lattice_completion::CandidateKind::Plain,
                );
                (
                    cand,
                    lattice_picker::RoutingPayload::OpenFile { path: p.clone() },
                )
            })
            .collect();
        let mut picker = lattice_picker::Picker::new(
            "recent",
            lattice_picker::PickerSource::Files,
            lattice_picker::PickerAction::OpenFile,
        );
        picker.set_raw_candidates_with_routing(items);
        self.picker = Some(picker);
    }

    pub(super) fn open_buffer_picker(&mut self) {
        let active = self.active_pane_buffer_id();
        let mut p = lattice_picker::Picker::new(
            "buffers",
            lattice_picker::PickerSource::Buffers,
            lattice_picker::PickerAction::SwitchToBuffer,
        );
        // Host-side candidate build (the picker module is
        // renderer-agnostic and doesn't import `BufferRegistry`).
        let pairs = raw_buffer_candidates(&self.buffers, &self.buffer_locals, active);
        p.set_raw_candidates_with_routing(pairs);
        // Stash the original active buffer id so dismiss can
        // restore. None on no-buffer pickers (LSP); for the
        // buffer switcher we always have one. Encoded as `u32`
        // because `Picker::preview_origin` is renderer-agnostic
        // (the host newtype-wraps).
        p.preview_origin = Some(active.0);
        self.picker = Some(p);
        // Preview the initial selection. With the active buffer
        // floated to the bottom, the initial selection is a
        // *different* buffer (the alternate-buffer convention),
        // so opening the picker immediately shows what `<CR>`
        // would land on.
        self.preview_picker_selection();
    }

    /// If the picker is open and its action is
    /// [`lattice_picker::PickerAction::SwitchToBuffer`], activate
    /// the currently-selected candidate's buffer in the active
    /// pane *as a preview* -- no position-history push, no
    /// commit. Called after every selection change while a buffer
    /// picker is open.
    pub(super) fn preview_picker_selection(&mut self) {
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        if !matches!(picker.on_accept, lattice_picker::PickerAction::SwitchToBuffer) {
            return;
        }
        let Some(c) = picker.selected_candidate() else {
            return;
        };
        let Some(lattice_picker::RoutingPayload::Buffer { id: raw_id }) =
            picker.routing_for(c)
        else {
            return;
        };
        let id = BufferId(*raw_id);
        if id == self.active_pane_buffer_id() {
            // Already showing this buffer; nothing to preview.
            return;
        }
        self.previewing = true;
        self.activate_buffer(id);
        self.previewing = false;
    }

    /// Apply `Action::PickerDismiss` -- close the picker and, if
    /// a buffer-switch picker was previewing, restore the active
    /// pane to whatever buffer it was on at picker-open. Tested
    /// by `picker_dismiss_restores_origin_when_previewing`.
    pub(super) fn do_picker_dismiss(&mut self) {
        // Drop any pending tag origin -- the user dismissed the
        // picker, so no drill-down happened. Without this clear
        // a subsequent `:lsp-symbols` (or any picker open) would
        // inherit the stale origin and a later accept would
        // push the wrong entry.
        self.pending_tag_origin = None;
        let Some(picker) = self.picker.take() else {
            return;
        };
        if let Some(origin_raw) = picker.preview_origin {
            let origin = BufferId(origin_raw);
            if origin != self.active_pane_buffer_id() {
                self.previewing = true;
                self.activate_buffer(origin);
                self.previewing = false;
            }
        }
    }

    /// Apply `Action::PickerAccept` -- run the picker's stored
    /// action against the selected candidate, then dismiss.
    /// For [`lattice_picker::PickerAction::SwitchToBuffer`] the
    /// preview-activated buffer is already on screen; the accept
    /// path just commits (clears preview tracking) without
    /// re-activating, so the position history sees ONE entry for
    /// the user's original cursor (pushed at picker-open in
    /// future, today the help-arm autopush handles cross-buffer-
    /// kind landings).
    pub(super) fn do_picker_accept(&mut self) {
        let Some(picker) = self.picker.take() else {
            return;
        };
        let Some(c) = picker.selected_candidate() else {
            // Empty filter -- bail without acting (the picker is
            // already gone since we `take()`d it). Restore the
            // original buffer if we'd been previewing.
            if let Some(origin) = picker.preview_origin {
                self.previewing = true;
                self.activate_buffer(BufferId(origin));
                self.previewing = false;
            }
            return;
        };
        // Snapshot the typed routing payload (Phase 4.2.g.7
        // polish). Pre-polish the dispatch parsed `c.raw.text`
        // with per-action string parsers; now each candidate's
        // `Extension { kind_id, payload }` indexes the picker's
        // typed `routing_meta` sidecar.
        let routing = match picker.routing_for(c).cloned() {
            Some(r) => r,
            None => {
                self.set_message(
                    EchoLevel::Error,
                    "picker: candidate carries no routing payload"
                        .to_string(),
                );
                if let Some(origin) = picker.preview_origin {
                    self.previewing = true;
                    self.activate_buffer(BufferId(origin));
                    self.previewing = false;
                }
                return;
            }
        };
        // Trait-driven path: when the picker was seated via
        // `:picker <source>` (slice 13), the source id is
        // stamped on the Picker. Resolve the generator, call
        // its `accept`, and translate the typed outcome.
        if let Some(source_id) = picker.source_id.as_deref()
            && let Some(generator) = self.picker_registry.generator(source_id).cloned()
        {
            let snap = self.document.snapshot();
            let ctx = self.build_picker_context(&snap);
            let outcome = match generator.accept(&ctx, &routing) {
                Ok(o) => o,
                Err(e) => {
                    self.set_message(EchoLevel::Error, e);
                    return;
                }
            };
            drop(ctx);
            drop(snap);
            self.apply_picker_outcome(outcome);
            return;
        }
        // Legacy imperative path: `:b`, `:lsp-log`, multi-
        // result LSP locations, completion / code-action
        // pickers. Each routing variant has its own arm.
        // Migration to the trait-driven path is per-source.
        match routing {
            lattice_picker::RoutingPayload::Buffer { id: raw_id } => {
                let id = BufferId(raw_id);
                // Already on the target via preview; no additional
                // action needed beyond letting the picker drop.
                if id != self.active_pane_buffer_id() {
                    // M.4: honour `BufferDisplayCategory::PickerResult`.
                    // For `Split(orientation)` this opens a new pane
                    // and focuses it before the activation, matching
                    // a user override like `:set picker-result.display
                    // = split-h`. `ActivePane` (default) is a no-op.
                    self.prepare_pane_for_picker_result();
                    self.activate_buffer(id);
                }
            }
            lattice_picker::RoutingPayload::LspInstance { server_id, .. } => {
                match picker.on_accept {
                    lattice_picker::PickerAction::OpenLspLog => {
                        self.open_lsp_log_in_pane(&server_id);
                    }
                    lattice_picker::PickerAction::OpenLspTraceLog => {
                        self.open_lsp_trace_log_in_pane(&server_id);
                    }
                    _ => {
                        self.set_message(
                            EchoLevel::Error,
                            "picker: lsp-instance routing on non-lsp-log action"
                                .to_string(),
                        );
                    }
                }
            }
            lattice_picker::RoutingPayload::LspLocation { path, line, col } => {
                // If this picker came from a tag-intent nav
                // (`gd` / `gD` / `gy` / `gI` multi-result),
                // push the captured pre-jump origin onto the
                // tag stack now -- the user has committed to
                // a drill-down candidate. References /
                // `:diagnostics` / symbol pickers don't set
                // the origin so this is a no-op for them.
                if let Some(origin) = self.pending_tag_origin.take() {
                    self.tag_stack.push(origin);
                }
                // M.4: honour `BufferDisplayCategory::PickerResult`
                // for the destination pane (split into a new sibling
                // before the jump if the user has overridden to a
                // `Split` display).
                self.prepare_pane_for_picker_result();
                self.jump_to_file_line_col(&path, line, col);
            }
            lattice_picker::RoutingPayload::LspCompletion { index } => {
                let Some(items) = self.pending_completion_items.take() else {
                    return;
                };
                let Some(item) = items.into_iter().nth(index as usize) else {
                    self.set_message(
                        EchoLevel::Error,
                        format!("picker: completion idx {index} out of range"),
                    );
                    return;
                };
                self.apply_lsp_completion_item(&item);
            }
            lattice_picker::RoutingPayload::LspCodeAction { index } => {
                let Some(items) = self.pending_code_action_items.take() else {
                    return;
                };
                let handle = self.pending_code_action_handle.take();
                let Some(row) = items.into_iter().nth(index as usize) else {
                    self.set_message(
                        EchoLevel::Error,
                        format!("picker: code-action idx {index} out of range"),
                    );
                    return;
                };
                self.apply_lsp_code_action(row, handle);
            }
            lattice_picker::RoutingPayload::OpenFile { path } => {
                // M.4: honour `BufferDisplayCategory::PickerResult`
                // for the destination pane (same hook the
                // LspLocation arm uses).
                self.prepare_pane_for_picker_result();
                self.do_edit(Some(path), false);
            }
            lattice_picker::RoutingPayload::JumpInBuffer { buffer_id, line, col } => {
                // Legacy fallback only fires when a picker without
                // a `source_id` emits JumpInBuffer. Today's
                // emitters all set `source_id` (trait-driven path
                // intercepts before reaching here), so this arm
                // is reachability-guard only -- if it ever does
                // fire, route it through the same outcome
                // translator the trait path uses so the behavior
                // doesn't diverge.
                self.apply_picker_outcome(
                    lattice_picker::PickerAcceptOutcome::JumpInBuffer {
                        buffer_id,
                        line,
                        col,
                    },
                );
            }
            lattice_picker::RoutingPayload::InvokeCommand { id, args } => {
                // Reachability-guard for the same reason as
                // `JumpInBuffer`: trait-driven sources set
                // `source_id` and intercept above.
                self.apply_picker_outcome(
                    lattice_picker::PickerAcceptOutcome::InvokeCommand { id, args },
                );
            }
            lattice_picker::RoutingPayload::PasteRegister { name } => {
                self.apply_picker_outcome(
                    lattice_picker::PickerAcceptOutcome::PasteRegister { name },
                );
            }
            lattice_picker::RoutingPayload::JumpToMark { name } => {
                self.apply_picker_outcome(
                    lattice_picker::PickerAcceptOutcome::JumpToMark { name },
                );
            }
        }
    }
}

/// Translate a host-side [`crate::buffer_registry::BufferEntry`]
/// into the picker's renderer-agnostic [`lattice_picker::BufferEntry`].
/// Pure function on the input + buffer-locals map; called by
/// [`App::build_picker_context`] for every registry entry.
fn picker_buffer_entry(
    entry: &crate::buffer_registry::BufferEntry,
    buffer_locals: &std::collections::HashMap<BufferId, lattice_mode::BufferLocals>,
) -> lattice_picker::BufferEntry {
    let id = entry.id.0;
    let (kind_label, path, title, dirty) = match &entry.data {
        BufferData::Document(d) => {
            let path = d.handle.path().map(std::path::PathBuf::from);
            let title = path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "[no name]".to_string());
            ("doc".to_string(), path, title, d.handle.dirty())
        }
        BufferData::FileTree(_) => {
            let root = buffer_locals
                .get(&entry.id)
                .and_then(|locals| locals.get::<crate::modes::FileTreeRoot>())
                .map(|r| r.0.clone());
            let title = root
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "[no root]".to_string());
            ("tree".to_string(), root, title, false)
        }
        BufferData::Help(h) => (
            "help".to_string(),
            None,
            h.title.clone(),
            false,
        ),
        BufferData::Oil(_) => {
            let dir = buffer_locals
                .get(&entry.id)
                .and_then(|locals| locals.get::<crate::modes::OilDir>())
                .map(|d| d.0.clone());
            let title = dir
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "[no dir]".to_string());
            ("oil".to_string(), dir, title, false)
        }
    };
    lattice_picker::BufferEntry {
        id,
        kind_label,
        path,
        title,
        dirty,
    }
}

/// Hard cap on the file-picker walker's emitted entry count.
/// At this scale the host's fuzzy matcher stays well inside the
/// per-keystroke frame budget; larger trees fall back to ripgrep-
/// style live filtering via `:grep` (P.10) or `:Filetree`'s
/// per-directory lazy walk.
const FILE_PICKER_MAX_ENTRIES: usize = 5000;

/// Walk `root` recursively (BFS) and return the absolute paths
/// of every regular file, capped at [`FILE_PICKER_MAX_ENTRIES`].
/// Skips the conventional ignore directories (`.git`, `target`,
/// `node_modules`, `dist`, `.cache`) and dotfiles at the top of
/// each directory entry. Symlinks aren't followed -- a cycle on
/// disk would silently consume the cap.
///
/// Errors are silently absorbed (unreadable directories show up
/// as gaps in the listing); the picker UX prefers "some results"
/// over a hard failure when the workspace has a permission
/// pocket somewhere.
pub(crate) fn walk_files_for_picker(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    const IGNORE_DIRS: &[&str] = &[".git", "target", "node_modules", "dist", ".cache"];
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if out.len() >= FILE_PICKER_MAX_ENTRIES {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut subdirs: Vec<std::path::PathBuf> = Vec::new();
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                if IGNORE_DIRS.contains(&name) {
                    continue;
                }
                subdirs.push(path);
            } else if ft.is_file() {
                files.push(path);
            }
        }
        // Stable order: alphabetic. Files first so they show up
        // before deep subdirs in the candidate list (relative-
        // path sort still scrambles them, but the matcher is
        // fuzzy so order isn't load-bearing).
        files.sort();
        subdirs.sort();
        for f in files {
            if out.len() >= FILE_PICKER_MAX_ENTRIES {
                break;
            }
            out.push(f);
        }
        // BFS-ish: push subdirs in reverse so pop() drains
        // alphabetically.
        for sub in subdirs.into_iter().rev() {
            stack.push(sub);
        }
    }
    out
}

/// Build the buffer-picker candidate set: every entry in the
/// registry, with the active buffer floated to the bottom and
/// tagged `(current)` in marginalia.
///
/// Free function rather than a method because the picker
/// module's matcher path stores routing payloads; this helper
/// composes both shapes (`RawCandidate` + `RoutingPayload`)
/// and isn't useful for callers that don't already have a
/// `BufferRegistry` in hand.
pub(super) fn raw_buffer_candidates(
    registry: &BufferRegistry,
    buffer_locals: &std::collections::HashMap<BufferId, lattice_mode::BufferLocals>,
    active: BufferId,
) -> Vec<(lattice_completion::RawCandidate, lattice_picker::RoutingPayload)> {
    let mut ids = registry.sorted_ids();
    ids.sort_by_key(|id| (*id == active, *id));
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(entry) = registry.get(id) else {
            continue;
        };
        let active_marker = if id == active { " (current)" } else { "" };
        let (body, kind_label) = match &entry.data {
            BufferData::Document(d) => {
                let path = d
                    .handle
                    .path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "[no name]".to_string());
                let dirty = if d.handle.dirty() { " [+]" } else { "" };
                (
                    format!("#{:<3} {path}{dirty}", id.0),
                    format!("doc{active_marker}"),
                )
            }
            BufferData::FileTree(_) => {
                // M.3.2.c.5: file-tree's root lives in the
                // FileTreeRoot buffer-local (canonical; no
                // struct mirror). Reads route through the
                // passed-in `buffer_locals` map.
                let root_display = buffer_locals
                    .get(&id)
                    .and_then(|locals| locals.get::<crate::modes::FileTreeRoot>())
                    .map(|r| r.0.display().to_string())
                    .unwrap_or_else(|| "[no root]".to_string());
                (
                    format!("#{:<3} {}", id.0, root_display),
                    format!("tree{active_marker}"),
                )
            }
            BufferData::Help(h) => (
                format!("#{:<3} {}", id.0, h.title),
                format!("help{active_marker}"),
            ),
            BufferData::Oil(_) => {
                // M.3.2.c.5: oil's dir lives in the `OilDir`
                // buffer-local. The picker reads through the
                // passed-in `buffer_locals` map; no struct
                // mirror to fall back on.
                let dir_display = buffer_locals
                    .get(&id)
                    .and_then(|locals| locals.get::<crate::modes::OilDir>())
                    .map(|d| d.0.display().to_string())
                    .unwrap_or_else(|| "[no dir]".to_string());
                (
                    format!("#{:<3} {}", id.0, dir_display),
                    format!("oil{active_marker}"),
                )
            }
        };
        // `text` is the user-facing buffer id; matcher matches
        // against `display`. The typed routing payload carries
        // the buffer id the accept dispatch consumes.
        let mut raw = lattice_completion::RawCandidate::plain(
            format!("#{}", id.0),
            lattice_completion::CandidateKind::Buffer,
        );
        raw.display = format!("{body:<60} {kind_label}");
        out.push((raw, lattice_picker::RoutingPayload::Buffer { id: id.0 }));
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use crate::app::*;
    use crate::app::test_helpers::app_with;
    use crate::help::HelpContent;

    #[test]
    fn open_buffer_picker_seeds_with_every_registry_entry() {
        let mut app = app_with("hi\n", 5);
        // Add a help buffer so the picker has more than just the
        // initial document to filter against.
        let _help_id = app.open_help_in_pane(HelpContent::from_lines(
            "lsp:rust",
            vec!["a".into()],
        ));
        // Activate back to the document so the picker's "active"
        // marker doesn't land on the help buffer.
        let doc_id = app
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        app.open_buffer_picker();
        let p = app.picker.as_ref().expect("picker should be open");
        // Initial: every buffer in the registry. With no filter,
        // both the doc and the help buffer should be present.
        assert!(p.candidates.len() >= 2);
        assert_eq!(p.title, "buffers");
    }

    #[test]
    fn picker_accept_switches_to_selected_buffer() {
        let mut app = app_with("hi\n", 5);
        let help_id = app.open_help_in_pane(HelpContent::from_lines(
            "test-target",
            vec!["body".into()],
        ));
        let doc_id = app
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        // Start on the doc.
        app.activate_document(doc_id);
        assert!(matches!(app.active_buffer, BufferKind::Document));
        // Open picker, type the help title, accept.
        app.open_buffer_picker();
        for c in "test-target".chars() {
            app.apply(Action::PickerAppend(c));
        }
        app.apply(Action::PickerAccept);
        // Picker is dismissed; active pane is on the help buffer.
        assert!(app.picker.is_none());
        assert_eq!(app.active_pane_buffer_id(), help_id);
        assert!(matches!(app.active_buffer, BufferKind::Help));
    }

    #[test]
    fn picker_dismiss_leaves_active_pane_unchanged() {
        let mut app = app_with("hi\n", 5);
        let doc_id = app
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        app.open_buffer_picker();
        app.apply(Action::PickerDismiss);
        assert!(app.picker.is_none());
        assert_eq!(app.active_pane_buffer_id(), doc_id);
    }

    #[test]
    fn buffer_picker_previews_initial_selection_in_active_pane() {
        // With doc + help in registry, opening the picker on the
        // doc immediately previews the alternate (help) buffer in
        // the active pane.
        let mut app = app_with("hi\n", 5);
        let help_id = app.open_help_in_pane(HelpContent::from_lines(
            "alt",
            vec!["alt body".into()],
        ));
        let doc_id = app
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        // Sanity: starting state.
        assert_eq!(app.active_pane_buffer_id(), doc_id);
        app.open_buffer_picker();
        // Picker open + preview switched the pane to the help
        // buffer (the alternate -- "(current)" is the doc).
        assert_eq!(app.active_pane_buffer_id(), help_id);
        assert!(matches!(app.active_buffer, BufferKind::Help));
    }

    #[test]
    fn picker_dismiss_restores_origin_when_previewing() {
        let mut app = app_with("hi\n", 5);
        let _help_id = app.open_help_in_pane(HelpContent::from_lines(
            "alt",
            vec!["alt body".into()],
        ));
        let doc_id = app
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        app.open_buffer_picker();
        // Preview moved us off the doc.
        assert_ne!(app.active_pane_buffer_id(), doc_id);
        app.apply(Action::PickerDismiss);
        // Esc restored the original.
        assert!(app.picker.is_none());
        assert_eq!(app.active_pane_buffer_id(), doc_id);
        assert!(matches!(app.active_buffer, BufferKind::Document));
    }

    #[test]
    fn picker_select_next_re_previews_new_candidate() {
        let mut app = app_with("hi\n", 5);
        let help_a = app.open_help_in_pane(HelpContent::from_lines(
            "alpha-help",
            vec!["a".into()],
        ));
        let help_b = app.open_help_in_pane(HelpContent::from_lines(
            "beta-help",
            vec!["b".into()],
        ));
        let doc_id = app
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        app.open_buffer_picker();
        let first_preview = app.active_pane_buffer_id();
        // Move down -- previews the next candidate.
        app.apply(Action::PickerSelectNext);
        let second_preview = app.active_pane_buffer_id();
        assert_ne!(first_preview, second_preview, "selection moved -> different preview");
        // Both previews land on one of the help buffers we set up.
        assert!(first_preview == help_a || first_preview == help_b || first_preview == doc_id);
        assert!(second_preview == help_a || second_preview == help_b || second_preview == doc_id);
        // Dismiss restores the original document.
        app.apply(Action::PickerDismiss);
        assert_eq!(app.active_pane_buffer_id(), doc_id);
    }

    #[test]
    fn picker_preview_does_not_pollute_position_history() {
        // Hover-previewing through several candidates should not
        // push to the jump list; only an *accepted* switch should.
        let mut app = app_with("hi\n", 5);
        let _h1 = app.open_help_in_pane(HelpContent::from_lines(
            "h-one",
            vec!["a".into()],
        ));
        let _h2 = app.open_help_in_pane(HelpContent::from_lines(
            "h-two",
            vec!["b".into()],
        ));
        let doc_id = app
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        let history_before = app.position_history.len();
        app.open_buffer_picker();
        app.apply(Action::PickerSelectNext);
        app.apply(Action::PickerSelectNext);
        app.apply(Action::PickerSelectPrev);
        app.apply(Action::PickerDismiss);
        let history_after = app.position_history.len();
        assert_eq!(
            history_before, history_after,
            "preview hovers should leave the jump list alone"
        );
    }

    /// P.1: the walker enumerates regular files in `root` while
    /// skipping the conventional ignore directories and
    /// dotfiles.
    #[test]
    fn file_picker_walks_root_and_skips_ignored() {
        let tmp = std::env::temp_dir()
            .join(format!("lattice-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.rs"), "").unwrap();
        std::fs::write(tmp.join("b.rs"), "").unwrap();
        std::fs::create_dir(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("sub").join("c.rs"), "").unwrap();
        // Ignored: dotfile and ignore-dir.
        std::fs::write(tmp.join(".secret"), "").unwrap();
        std::fs::create_dir(tmp.join("target")).unwrap();
        std::fs::write(tmp.join("target").join("d.rs"), "").unwrap();
        let entries = super::walk_files_for_picker(&tmp);
        let names: Vec<String> = entries
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().any(|n| n == "a.rs"));
        assert!(names.iter().any(|n| n == "b.rs"));
        assert!(names.iter().any(|n| n == "c.rs"));
        assert!(!names.iter().any(|n| n == ".secret"));
        assert!(!names.iter().any(|n| n == "d.rs"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// P.1: `open_file_picker` seeds a picker whose every row
    /// carries an `OpenFile { path }` routing payload pointing
    /// to a real file under the supplied root.
    #[test]
    fn open_file_picker_seeds_open_file_routing() {
        let tmp = std::env::temp_dir()
            .join(format!("lattice-files-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("alpha.rs"), "fn alpha() {}\n").unwrap();
        std::fs::write(tmp.join("beta.rs"), "fn beta() {}\n").unwrap();
        let mut app = app_with("hi\n", 5);
        app.open_file_picker(Some(tmp.clone()));
        let p = app.picker.as_ref().expect("picker open");
        assert_eq!(p.candidates.len(), 2);
        // Every candidate routes to an `OpenFile` payload
        // pointing under `tmp`.
        for cand in &p.candidates {
            let routing = p.routing_for(cand).expect("routing");
            match routing {
                lattice_picker::RoutingPayload::OpenFile { path } => {
                    assert!(path.starts_with(std::fs::canonicalize(&tmp).unwrap()));
                }
                other => panic!("expected OpenFile, got {other:?}"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// P.1: empty workspace echoes "no files" and leaves the
    /// picker closed.
    #[test]
    fn open_file_picker_empty_root_echoes() {
        let tmp = std::env::temp_dir()
            .join(format!("lattice-files-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let mut app = app_with("hi\n", 5);
        app.open_file_picker(Some(tmp.clone()));
        assert!(app.picker.is_none());
        let msg = app.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no files"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// P.2: `push_recent_file` keeps MRU order (newest first),
    /// dedups repeats, and caps at the configured ceiling. The
    /// canonicalised path is what lands in the list, so
    /// re-pushing the same path collapses to one entry.
    #[test]
    fn push_recent_file_is_mru_and_dedupes() {
        let tmp = std::env::temp_dir()
            .join(format!("lattice-recent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let a_path = tmp.join("a.rs");
        let b_path = tmp.join("b.rs");
        let c_path = tmp.join("c.rs");
        std::fs::write(&a_path, "").unwrap();
        std::fs::write(&b_path, "").unwrap();
        std::fs::write(&c_path, "").unwrap();
        let mut app = app_with("hi\n", 5);
        app.push_recent_file(&a_path);
        app.push_recent_file(&b_path);
        app.push_recent_file(&c_path);
        // Newest first.
        let canon_a = std::fs::canonicalize(&a_path).unwrap();
        let canon_b = std::fs::canonicalize(&b_path).unwrap();
        let canon_c = std::fs::canonicalize(&c_path).unwrap();
        assert_eq!(app.recent_files, vec![canon_c.clone(), canon_b.clone(), canon_a.clone()]);
        // Re-pushing `a` floats it to the front and drops the
        // older occurrence -- list length stays at 3.
        app.push_recent_file(&a_path);
        assert_eq!(app.recent_files, vec![canon_a, canon_c, canon_b]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// P.2: empty MRU echoes "no recent files" and leaves the
    /// picker closed.
    #[test]
    fn open_recent_files_picker_empty_echoes() {
        let mut app = app_with("hi\n", 5);
        app.open_recent_files_picker();
        assert!(app.picker.is_none());
        let msg = app.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no recent files"));
    }

    /// P.2: each recent-files row carries an `OpenFile { path }`
    /// routing payload pointing to the (canonicalised) path the
    /// user previously `:edit`ed, in MRU order.
    #[test]
    fn open_recent_files_picker_seeds_open_file_routing() {
        let tmp = std::env::temp_dir()
            .join(format!("lattice-recent-seed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let alpha = tmp.join("alpha.rs");
        let beta = tmp.join("beta.rs");
        std::fs::write(&alpha, "").unwrap();
        std::fs::write(&beta, "").unwrap();
        let mut app = app_with("hi\n", 5);
        app.push_recent_file(&alpha);
        app.push_recent_file(&beta);
        app.open_recent_files_picker();
        let p = app.picker.as_ref().expect("picker open");
        assert_eq!(p.candidates.len(), 2);
        // First candidate (selected) routes to the freshly-pushed
        // `beta` -- newest first.
        let first = p.selected_candidate().expect("selected");
        match p.routing_for(first).expect("routing") {
            lattice_picker::RoutingPayload::OpenFile { path } => {
                assert_eq!(path, &std::fs::canonicalize(&beta).unwrap());
            }
            other => panic!("expected OpenFile, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Slice 12: built-in registry seeds the well-known sources so
    /// `:picker <Tab>` (and downstream slice-13 tests) can rely on
    /// them being present.
    #[test]
    fn boot_registers_builtin_picker_sources() {
        let app = app_with("hi\n", 5);
        let ids: Vec<&'static str> = app.picker_registry.ids().collect();
        assert!(ids.contains(&"files"));
        assert!(ids.contains(&"recent"));
        assert!(ids.contains(&"buffers"));
    }

    /// Slice 12: `:picker files` routes through the registry +
    /// dispatch table and seeds the same picker shape `:files`
    /// does today (every row routes to `OpenFile`).
    #[test]
    fn open_picker_files_seeds_open_file_routing() {
        let tmp = std::env::temp_dir()
            .join(format!("lattice-picker-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.rs"), "").unwrap();
        let mut app = app_with("hi\n", 5);
        app.open_picker("files".into(), vec![tmp.display().to_string()]);
        let p = app.picker.as_ref().expect("picker open");
        assert!(!p.candidates.is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Slice 12: `:picker buffers` shares the buffer-switcher
    /// shape `:b` produces -- candidates per registry entry.
    #[test]
    fn open_picker_buffers_opens_buffer_switcher() {
        let mut app = app_with("hi\n", 5);
        app.open_picker("buffers".into(), Vec::new());
        let p = app.picker.as_ref().expect("picker open");
        assert!(!p.candidates.is_empty());
    }

    /// Slice 12: empty MRU `:picker recent` echoes the same
    /// message `:recent` does (closed picker, info echo).
    #[test]
    fn open_picker_recent_with_empty_mru_echoes() {
        let mut app = app_with("hi\n", 5);
        app.open_picker("recent".into(), Vec::new());
        assert!(app.picker.is_none());
        let msg = app.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no recent files"));
    }

    /// Slice 3c: the `gen:picker-sources` completion generator
    /// emits one candidate per source registered with the App's
    /// `picker_registry`. Confirms the Weak<PickerRegistry>
    /// plumbing is wired correctly end-to-end -- the generator
    /// can read the registry the App owns and yields the
    /// expected id-sorted set.
    #[test]
    fn gen_picker_sources_emits_candidate_per_registered_source() {
        let app = app_with("hi\n", 5);
        let generator = app
            .completion_registry
            .generator_by_name("gen:picker-sources")
            .expect("gen:picker-sources must be registered at boot");
        let snap = app.document.snapshot();
        let ctx = lattice_completion::GenerateContext {
            prefix: "",
            buffer: &snap.buffer,
            registry: &app.registry,
            case_sensitive: false,
        };
        let candidates = generator.inner.generate(&ctx);
        let ids: Vec<String> = candidates.iter().map(|c| c.text.clone()).collect();
        // Built-in registry seeds the first-party sources;
        // PickerRegistry::iter is id-sorted so popup order is
        // stable. Each new source migration extends this list.
        assert_eq!(
            ids,
            vec![
                "buffers", "commands", "files", "jumps", "lines",
                "marks", "recent", "registers",
            ]
        );
        // Sanity: matches what the registry itself reports.
        let registry_ids: Vec<&'static str> = app.picker_registry.ids().collect();
        let mut expected: Vec<String> = registry_ids.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(ids, expected);
    }

    /// Slice 3c: dropping the Arc<PickerRegistry> (simulating
    /// App teardown) makes the generator's Weak upgrade fail,
    /// and the generator returns an empty candidate set rather
    /// than panicking. Same discipline as `gen:modes`.
    #[test]
    fn gen_picker_sources_handles_dropped_registry_gracefully() {
        use std::sync::{Arc, Weak};

        let reg: Arc<lattice_picker::PickerRegistry> =
            Arc::new(lattice_picker::PickerRegistry::new());
        let weak: Weak<lattice_picker::PickerRegistry> = Arc::downgrade(&reg);
        drop(reg);
        let generator =
            crate::host_generators::PickerSourcesGenerator { registry: weak };
        // Build a minimal GenerateContext via an App fixture --
        // we just need a real Buffer + CommandRegistry.
        let app = app_with("hi\n", 5);
        let snap = app.document.snapshot();
        let ctx = lattice_completion::GenerateContext {
            prefix: "",
            buffer: &snap.buffer,
            registry: &app.registry,
            case_sensitive: false,
        };
        let candidates = lattice_completion::traits::CandidateGenerator::generate(
            &generator, &ctx,
        );
        assert!(candidates.is_empty());
    }

    /// P.3: end-to-end `:picker lines` -- dispatch routes
    /// through `LinesSource::init` (trait-driven path) and
    /// seats a picker stamped with `source_id: Some("lines")`.
    /// The candidates count matches the active buffer's line
    /// count (sans phantom trailing newline).
    #[test]
    fn open_picker_lines_seeds_one_row_per_line() {
        let mut app = app_with("alpha\nbeta\ngamma\n", 10);
        app.open_picker("lines".into(), Vec::new());
        let p = app.picker.as_ref().expect("picker open");
        assert_eq!(p.candidates.len(), 3);
        assert_eq!(p.source_id.as_deref(), Some("lines"));
    }

    /// P.3: accepting a row from the lines picker routes
    /// through `LinesSource::accept` -> `JumpInBuffer`
    /// outcome -> `apply_picker_outcome` and moves the cursor
    /// to the chosen line.
    #[test]
    fn open_picker_lines_accept_jumps_cursor() {
        let mut app = app_with("alpha\nbeta\ngamma\n", 10);
        app.open_picker("lines".into(), Vec::new());
        // Move selection to the second row (beta, line index 1)
        // and accept.
        app.apply(Action::PickerSelectNext);
        app.apply(Action::PickerAccept);
        assert!(app.picker.is_none());
        assert_eq!(app.cursor.line, 1);
        assert_eq!(app.cursor.byte, 0);
    }

    /// Slice 12: an unknown source id surfaces an error echo
    /// listing every known id so the user can recover without
    /// `:apropos`.
    #[test]
    fn open_picker_unknown_source_echoes_with_known_ids() {
        let mut app = app_with("hi\n", 5);
        app.open_picker("nope".into(), Vec::new());
        assert!(app.picker.is_none());
        let msg = app.last_message.as_ref().expect("echo");
        assert!(
            msg.text.contains("unknown source `nope`"),
            "missing unknown-source prefix: {}",
            msg.text
        );
        assert!(
            msg.text.contains("files") && msg.text.contains("recent"),
            "missing known-ids listing: {}",
            msg.text
        );
    }
}
