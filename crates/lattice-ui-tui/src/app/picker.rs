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
        let selection = self.last_visual.as_ref().map(|v| (v.anchor, v.head));

        // Collect tree-sitter symbol locations once per
        // picker-open. Cost is O(parse-tree-size); reads through
        // the document's current `SyntaxSnapshot`. Empty when
        // no parser is registered for the buffer's language
        // (the `outline` source returns Err in that case).
        let syntax_symbols = self
            .syntax
            .as_ref()
            .map(|s| s.snapshot().collect_symbol_locations())
            .unwrap_or_default();
        let active_buffer = ActiveBufferSnapshot {
            buffer_id: active_id.0,
            path,
            language,
            cursor: self.cursor,
            selection,
            buffer: &snap.buffer,
            syntax_symbols,
        };

        let workspace_root = self.picker_workspace_root_path(snap);

        // Buffer registry -> picker BufferEntry view.
        let mut buffers: Vec<BufferEntry> = Vec::new();
        self.buffers.for_each(|entry| {
            buffers.push(picker_buffer_entry(entry, &self.buffer_locals));
        });

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
    pub(super) fn apply_picker_outcome(&mut self, outcome: lattice_picker::PickerAcceptOutcome) {
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
            JumpInBuffer {
                buffer_id,
                line,
                col,
            } => {
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
            ExpandSnippet { id } => {
                // Look up snippet by name (cross-language --
                // `by_name` indexes the full registry). If found,
                // splice the body into the buffer at the current
                // cursor through the existing `expand_snippet`
                // path so tab-stop tracking + mark plumbing
                // match `:snippet-expand` and the `<C-x><C-s>`
                // chord.
                let snippet = self.snippet_registry.load().by_name(&id).cloned();
                let Some(snippet) = snippet else {
                    self.set_message(EchoLevel::Error, format!("picker: no snippet named `{id}`"));
                    return;
                };
                self.expand_snippet(&snippet.body, self.cursor);
            }
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

    /// Write the MRU index to its configured path. Best-
    /// effort: persistence may be disabled (no cache dir) or
    /// fail mid-write (full disk, permission denied). On
    /// failure we `eprintln!` once and continue -- losing one
    /// accept's persistence is annoying, blocking the accept
    /// is unacceptable. Slice 14d (event bus + typed options)
    /// can elevate to a debounced background write.
    fn persist_picker_mru_best_effort(&self) {
        let persist = self
            .config
            .get_typed::<lattice_config::core_options::PickerMruPersist>()
            .map(|b| *b)
            .unwrap_or(true);
        if !persist {
            return;
        }
        let Some(path) = self.picker_mru_path.as_ref() else {
            return;
        };
        if let Err(e) = self.picker_mru.save_to(path) {
            eprintln!(
                "lattice: failed to persist picker MRU at {}: {e}",
                path.display(),
            );
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
            .filter(|r| effective.as_ref().is_none_or(|want| r.server_id == *want))
            .collect();
        if matches.len() == 1 {
            let server_id = matches[0].server_id.clone();
            match on_accept {
                lattice_picker::PickerAction::OpenLspLog => self.open_lsp_log_in_pane(&server_id),
                lattice_picker::PickerAction::OpenLspTraceLog => {
                    self.open_lsp_trace_log_in_pane(&server_id)
                }
                lattice_picker::PickerAction::SwitchToBuffer
                | lattice_picker::PickerAction::JumpToLspLocation
                | lattice_picker::PickerAction::AcceptLspCompletion
                | lattice_picker::PickerAction::AcceptLspCodeAction
                | lattice_picker::PickerAction::AcceptLspCodeLens
                | lattice_picker::PickerAction::AcceptColorPresentation
                | lattice_picker::PickerAction::OpenFile
                | lattice_picker::PickerAction::AcceptShowMessageAction => {}
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
    pub(crate) fn open_picker(&mut self, source: String, args: Vec<String>) {
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

        // Publish the picker-opened typed event. Plugins
        // (Phase 7+) can subscribe to react to source-
        // specific opens; today there are no first-party
        // subscribers, but the surface is introspectable
        // via `:describe-events`.
        self.event_bus
            .publish_typed(lattice_picker::events::PickerOpened {
                source_id: source.clone(),
                ts: std::time::SystemTime::now(),
            });

        match init_result {
            lattice_picker::PickerInitResult::Inline(pairs) => {
                self.seat_picker_from_pairs(source, pairs);
            }
            lattice_picker::PickerInitResult::Future(fut) => {
                // Cancel any prior in-flight init -- vim-style
                // "do what I last said". The previous future
                // may still resolve in the background; the
                // cancel token tells the spawn closure to drop
                // the result without sending.
                if let Some(prev) = self.pending_picker_init.take() {
                    prev.cancel.cancel();
                }
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                let cancel = lattice_protocol::CancellationToken::new();
                let cancel_clone = cancel.clone();
                crate::runtime::spawn_on_lsp_runtime(async move {
                    let result = fut.await;
                    if !cancel_clone.is_cancelled() {
                        let _ = tx.send(result);
                    }
                });
                self.pending_picker_init = Some(crate::app::PendingPickerInit {
                    source_id: source.clone(),
                    generator: generator.clone(),
                    rx,
                    cancel,
                });
                self.set_message(EchoLevel::Info, format!("picker: {source}... (loading)"));
            }
            lattice_picker::PickerInitResult::Stream(_) => {
                // Streaming sources land once the seat-on-batch
                // pump arrives. Today: error out cleanly --
                // single-batch streams could be flattened into
                // Inline anyway.
                self.set_message(
                    EchoLevel::Error,
                    format!(
                        "picker: streaming sources not yet wired (source `{source}` returned a Stream)"
                    ),
                );
            }
        }
    }

    /// Drain the pending async picker init, if any. Called
    /// from the main loop tick. Pumps the channel that the
    /// spawned future writes to; once a result arrives the
    /// picker is seated through the same path Inline init
    /// uses (so MRU snapshot + preview ergonomics behave
    /// identically). Empty channel = future still pending;
    /// closed channel = task dropped without sending (the
    /// cancel path took it).
    pub(crate) fn drain_pending_picker_init(&mut self) {
        let Some(pending) = self.pending_picker_init.as_mut() else {
            return;
        };
        let result = match pending.rx.try_recv() {
            Ok(r) => r,
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => return,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                // Task ended without sending (cancelled or
                // panicked). Drop the pending and move on.
                self.pending_picker_init = None;
                return;
            }
        };
        let pending = self.pending_picker_init.take().expect("guarded above");
        match result {
            Ok(pairs) => {
                self.seat_picker_from_pairs(pending.source_id, pairs);
            }
            Err(e) => {
                self.set_message(EchoLevel::Error, format!("picker: {e}"));
            }
        }
    }

    /// Seat `pairs` into a freshly-constructed picker for
    /// `source`. Shared by sync (Inline) and async (Future)
    /// init paths so MRU bonus snapshot + source-id stamping
    /// + buffer-switcher preview ergonomics behave
    /// identically regardless of how the candidates were
    /// produced.
    fn seat_picker_from_pairs(&mut self, source: String, pairs: lattice_picker::CandidateBatch) {
        let title = source.clone();
        // MRU bonus snapshot -- per-keystroke refilter reads
        // cached bonuses, not the live MRU HashMap. Honors
        // `picker.mru.enabled` (skip bonuses entirely when
        // off) and `picker.mru.recency-half-life-days`.
        let mru_enabled = self
            .config
            .get_typed::<lattice_config::core_options::PickerMruEnabled>()
            .map(|b| *b)
            .unwrap_or(true);
        let now = std::time::SystemTime::now();
        let half_life = self
            .config
            .get_typed::<lattice_config::core_options::PickerMruRecencyHalfLifeDays>()
            .map(|d| std::time::Duration::from_secs((*d).max(1) as u64 * 24 * 60 * 60))
            .unwrap_or(lattice_picker::DEFAULT_HALF_LIFE);
        let bonuses: Vec<f64> = if mru_enabled {
            pairs
                .iter()
                .map(
                    |(_cand, routing)| match lattice_picker::routing_identity(routing) {
                        Some(id) => self.picker_mru.frecency_bonus(&source, &id, now, half_life),
                        None => 0.0,
                    },
                )
                .collect()
        } else {
            vec![0.0; pairs.len()]
        };
        let mut picker = lattice_picker::Picker::new(
            title,
            Self::picker_source_for(&source),
            Self::picker_action_for(&source),
        );
        // Single-pass seat: one refilter instead of two.
        picker.set_raw_candidates_with_routing_and_bonuses(pairs, bonuses);
        picker.source_id = Some(source.clone());
        if source == "buffers" {
            picker.preview_origin = Some(self.active_pane_buffer_id().0);
        }
        self.picker = Some(picker);
        if source == "buffers" {
            self.preview_picker_selection();
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

    pub(super) fn open_buffer_picker(&mut self) {
        let active = self.active_pane_buffer_id();
        // Push the pre-picker cursor onto position history *before*
        // any preview activations fire. The activate_buffer push
        // skips during `previewing = true`, and the accept path
        // typically short-circuits (the preview already activated
        // the target). Recording the origin here gives `<C-o>` a
        // target to walk back to regardless of whether the user
        // accepts a different candidate or dismisses.
        // `push_position_history` coalesces if the same entry
        // appears twice, so this is safe even when the user
        // opens picker → dismiss → open picker again.
        let cur = self.active_cursor();
        self.push_position_history(cur, super::PositionSource::AutoJump);
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
        if !matches!(
            picker.on_accept,
            lattice_picker::PickerAction::SwitchToBuffer
        ) {
            return;
        }
        let Some(c) = picker.selected_candidate() else {
            return;
        };
        let Some(lattice_picker::RoutingPayload::Buffer { id: raw_id }) = picker.routing_for(c)
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
        // 4.4.b: SMR picker dismiss = reply `null` to the
        // server. Snapshot the request id before any other
        // mutation; the finalize call removes the slot, the
        // queue advance opens the next picker (which sets
        // `self.picker` again so the preview-restore branch
        // below must not run for SMR).
        if let lattice_picker::PickerSource::LspShowMessageRequest { request_id, .. } =
            picker.source
        {
            self.finalize_show_message_request(request_id, None);
            self.open_next_queued_show_message_request();
            return;
        }
        // Publish the dismiss event for subscribers tracking
        // open-without-accept sessions. Source id may be
        // absent for legacy imperative pickers (`:b`,
        // multi-result LSP locations); skip the publish in
        // that case rather than emit a misleading default.
        if let Some(source_id) = picker.source_id.as_deref() {
            self.event_bus
                .publish_typed(lattice_picker::events::PickerDismissed {
                    source_id: source_id.to_string(),
                    ts: std::time::SystemTime::now(),
                });
        }
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
                    "picker: candidate carries no routing payload".to_string(),
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
            let source_id_owned = source_id.to_string();
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
            // Record MRU before applying the outcome so the
            // identity captures the user's choice even if the
            // outcome handler echoes an error mid-mutation
            // (e.g. file no longer exists). Identity may be
            // None for drift-prone routing payloads -- the
            // record call silently skips those. `picker.mru.enabled`
            // gates the whole path so users who disable MRU
            // see no recording either.
            let mru_enabled = self
                .config
                .get_typed::<lattice_config::core_options::PickerMruEnabled>()
                .map(|b| *b)
                .unwrap_or(true);
            let identity = lattice_picker::routing_identity(&routing);
            if mru_enabled && let Some(identity) = identity.as_deref() {
                self.picker_mru.record(&source_id_owned, identity);
                self.persist_picker_mru_best_effort();
            }
            // Publish the typed accept event AFTER the MRU
            // record so subscribers walking the event see a
            // consistent "this is what just happened + the
            // index is already up to date" snapshot.
            // Plugin subscribers (Phase 7+) and future
            // telemetry hooks subscribe to this; the MRU
            // record stays on the direct path because it's
            // load-bearing for the very next picker-open and
            // bus delivery is queue-deferred.
            self.event_bus
                .publish_typed(lattice_picker::events::PickerAccepted {
                    source_id: source_id_owned.clone(),
                    identity,
                    routing_payload_path: routing_payload_path(&routing),
                    ts: std::time::SystemTime::now(),
                });
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
                            "picker: lsp-instance routing on non-lsp-log action".to_string(),
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
            lattice_picker::RoutingPayload::JumpInBuffer {
                buffer_id,
                line,
                col,
            } => {
                // Legacy fallback only fires when a picker without
                // a `source_id` emits JumpInBuffer. Today's
                // emitters all set `source_id` (trait-driven path
                // intercepts before reaching here), so this arm
                // is reachability-guard only -- if it ever does
                // fire, route it through the same outcome
                // translator the trait path uses so the behavior
                // doesn't diverge.
                self.apply_picker_outcome(lattice_picker::PickerAcceptOutcome::JumpInBuffer {
                    buffer_id,
                    line,
                    col,
                });
            }
            lattice_picker::RoutingPayload::InvokeCommand { id, args } => {
                // Reachability-guard for the same reason as
                // `JumpInBuffer`: trait-driven sources set
                // `source_id` and intercept above.
                self.apply_picker_outcome(lattice_picker::PickerAcceptOutcome::InvokeCommand {
                    id,
                    args,
                });
            }
            lattice_picker::RoutingPayload::PasteRegister { name } => {
                self.apply_picker_outcome(lattice_picker::PickerAcceptOutcome::PasteRegister {
                    name,
                });
            }
            lattice_picker::RoutingPayload::JumpToMark { name } => {
                self.apply_picker_outcome(lattice_picker::PickerAcceptOutcome::JumpToMark { name });
            }
            lattice_picker::RoutingPayload::ExpandSnippet { id } => {
                self.apply_picker_outcome(lattice_picker::PickerAcceptOutcome::ExpandSnippet {
                    id,
                });
            }
            // 4.4.b: server-initiated showMessageRequest. Look
            // up the inbound slot by `request_id`, ferry the
            // selected `MessageActionItem` back over the
            // oneshot, then drain the queue so the next pending
            // SMR opens on the same tick (servers can pile up
            // refresh / restart prompts after a config change).
            lattice_picker::RoutingPayload::AcceptShowMessageAction {
                request_id,
                action_index,
            } => {
                self.finalize_show_message_request(request_id, Some(action_index));
                self.open_next_queued_show_message_request();
            }
            // 4.5.d: accept one code lens. Resolve if needed,
            // then route the lens's `command` through the
            // originating server's `workspace/executeCommand`.
            lattice_picker::RoutingPayload::LspCodeLens { index } => {
                self.accept_lsp_code_lens(index);
            }
            // 4.5.e: splice the chosen color presentation into
            // the buffer at the cached color range.
            lattice_picker::RoutingPayload::ColorPresentation { index } => {
                self.accept_lsp_color_presentation(index);
            }
        }
    }
}

/// Extract a path from a routing payload when one is
/// directly carried, otherwise `None`. Used by the picker-
/// accepted event publish so subscribers that care about
/// the path (telemetry, recent-file logs, plugin hooks)
/// don't have to repeat the routing-payload match.
fn routing_payload_path(payload: &lattice_picker::RoutingPayload) -> Option<std::path::PathBuf> {
    match payload {
        lattice_picker::RoutingPayload::OpenFile { path }
        | lattice_picker::RoutingPayload::LspLocation { path, .. } => Some(path.clone()),
        _ => None,
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
            let path = d.handle.path();
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
        BufferData::Help(h) => ("help".to_string(), None, h.title.clone(), false),
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
) -> Vec<(
    lattice_completion::RawCandidate,
    lattice_picker::RoutingPayload,
)> {
    // Collect everything we need from the registry under one lock
    // visit: id, listed flag, body, kind_label. The picker module
    // is renderer-agnostic; we shape rows here and sort outside the
    // lock.
    let mut rows: Vec<(BufferId, bool, String, String)> = Vec::new();
    registry.for_each(|entry| {
        let id = entry.id;
        let listed = entry.flags.listed;
        let active_marker = if id == active { " (current)" } else { "" };
        let (body, kind_label) = match &entry.data {
            BufferData::Document(d) => {
                // Picker label: path -> registry name -> "[no name]".
                // The registry's `name` slot lets synthetic buffers
                // (`*lsp*`, `*messages*`) surface their label so users
                // can type `*lsp*` to filter to them.
                let label = d
                    .handle
                    .path()
                    .map(|p| p.display().to_string())
                    .or_else(|| entry.name.clone())
                    .unwrap_or_else(|| "[no name]".to_string());
                // Suppress the modified marker for synthetic
                // buffers -- their content is owner-streamed, not
                // user-edited, so dirty has no actionable meaning.
                let dirty = if entry.name.is_none() && d.handle.dirty() {
                    " [+]"
                } else {
                    ""
                };
                (
                    format!("#{:<3} {label}{dirty}", id.0),
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
        rows.push((id, listed, body, kind_label));
    });
    // Picker order: active LAST, listed BEFORE unlisted, otherwise
    // by id. Unlisted synthetic buffers (`*lsp*`, ...) stay
    // reachable via the picker (the user can filter to them by
    // name) but the initial preview lands on a listed alternate
    // first, matching vim's "skip nobuflisted in cycling" intent.
    rows.sort_by_key(|(id, listed, _, _)| (*id == active, !*listed, *id));
    let mut out = Vec::with_capacity(rows.len());
    for (id, _listed, body, kind_label) in rows {
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

    use crate::app::test_helpers::app_with;
    use crate::app::*;
    use crate::help::HelpContent;

    #[test]
    fn open_buffer_picker_seeds_with_every_registry_entry() {
        let mut app = app_with("hi\n", 5);
        // Add a help buffer so the picker has more than just the
        // initial document to filter against.
        let _help_id = app.open_help_in_pane(HelpContent::from_lines("lsp:rust", vec!["a".into()]));
        // Activate back to the document so the picker's "active"
        // marker doesn't land on the help buffer.
        let doc_id = app.buffers.document_ids_sorted().first().copied().unwrap();
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
        let help_id =
            app.open_help_in_pane(HelpContent::from_lines("test-target", vec!["body".into()]));
        let doc_id = app.buffers.document_ids_sorted().first().copied().unwrap();
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
        let doc_id = app.buffers.document_ids_sorted().first().copied().unwrap();
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
        let help_id =
            app.open_help_in_pane(HelpContent::from_lines("alt", vec!["alt body".into()]));
        let doc_id = app.buffers.document_ids_sorted().first().copied().unwrap();
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
        let _help_id =
            app.open_help_in_pane(HelpContent::from_lines("alt", vec!["alt body".into()]));
        let doc_id = app.buffers.document_ids_sorted().first().copied().unwrap();
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
        let help_a = app.open_help_in_pane(HelpContent::from_lines("alpha-help", vec!["a".into()]));
        let help_b = app.open_help_in_pane(HelpContent::from_lines("beta-help", vec!["b".into()]));
        let doc_id = app.buffers.document_ids_sorted().first().copied().unwrap();
        app.activate_document(doc_id);
        app.open_buffer_picker();
        let first_preview = app.active_pane_buffer_id();
        // Move down -- previews the next candidate.
        app.apply(Action::PickerSelectNext);
        let second_preview = app.active_pane_buffer_id();
        assert_ne!(
            first_preview, second_preview,
            "selection moved -> different preview"
        );
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
        let _h1 = app.open_help_in_pane(HelpContent::from_lines("h-one", vec!["a".into()]));
        let _h2 = app.open_help_in_pane(HelpContent::from_lines("h-two", vec!["b".into()]));
        let doc_id = app.buffers.document_ids_sorted().first().copied().unwrap();
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
        let tmp = std::env::temp_dir().join(format!("lattice-files-{}", std::process::id()));
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

    /// P.2: `push_recent_file` keeps MRU order (newest first),
    /// dedups repeats, and caps at the configured ceiling. The
    /// canonicalised path is what lands in the list, so
    /// re-pushing the same path collapses to one entry.
    #[test]
    fn push_recent_file_is_mru_and_dedupes() {
        let tmp = std::env::temp_dir().join(format!("lattice-recent-{}", std::process::id()));
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
        assert_eq!(
            app.recent_files,
            vec![canon_c.clone(), canon_b.clone(), canon_a.clone()]
        );
        // Re-pushing `a` floats it to the front and drops the
        // older occurrence -- list length stays at 3.
        app.push_recent_file(&a_path);
        assert_eq!(app.recent_files, vec![canon_a, canon_c, canon_b]);
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
        let tmp = std::env::temp_dir().join(format!("lattice-picker-files-{}", std::process::id()));
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
                "buffers",
                "commands",
                "files",
                "grep",
                "jumps",
                "lines",
                "marks",
                "outline",
                "recent",
                "registers",
                "snippets",
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
        let generator = crate::host_generators::PickerSourcesGenerator { registry: weak };
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
        let candidates = lattice_completion::traits::CandidateGenerator::generate(&generator, &ctx);
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

    /// P.10: `:picker snippets` against an empty registry
    /// echoes the source's no-snippets message and leaves
    /// the picker closed (fresh-boot fixture has no snippets
    /// loaded). Confirms the feature-crate registration
    /// pattern -- lattice-snippet's
    /// `picker_sources::register` -- wires through to App
    /// dispatch correctly.
    #[test]
    fn open_picker_snippets_empty_registry_echoes() {
        let mut app = app_with("hi\n", 5);
        app.open_picker("snippets".into(), Vec::new());
        assert!(app.picker.is_none());
        let msg = app.last_message.as_ref().expect("echo");
        assert!(
            msg.text.contains("no snippets registered"),
            "got `{}`",
            msg.text
        );
    }

    /// Slice 14c: accepting a candidate records it in
    /// `picker_mru`; the next open observes a non-zero
    /// frecency bonus for that identity, and -- with two
    /// otherwise-equivalent rows -- floats the recorded one
    /// to the top of the popup.
    #[test]
    fn picker_mru_floats_previously_accepted_to_top() {
        let tmp = std::env::temp_dir().join(format!("lattice-mru-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("alpha.rs"), "").unwrap();
        std::fs::write(tmp.join("beta.rs"), "").unwrap();
        let mut app = app_with("hi\n", 5);
        // Disable persistence -- we don't want this test
        // touching the user's real cache. Also clear any
        // pre-loaded entries from disk so the assertions
        // measure deltas, not absolute counts.
        app.picker_mru.clear();
        app.picker_mru_path = None;
        // Open the files picker and accept the alphabetically-
        // first candidate (alpha.rs sorts before beta.rs in
        // walker output, but order depends on read_dir so use
        // whichever the picker surfaces).
        app.open_picker("files".into(), vec![tmp.display().to_string()]);
        let first_id = {
            let p = app.picker.as_ref().expect("picker open");
            let c = p.selected_candidate().expect("first selected");
            match p.routing_for(c).expect("routing") {
                lattice_picker::RoutingPayload::OpenFile { path } => path.clone(),
                other => panic!("expected OpenFile, got {other:?}"),
            }
        };
        app.apply(Action::PickerAccept);
        assert!(app.picker.is_none());
        // The MRU should now have one entry under `files`.
        let identity = format!("file:{}", first_id.display());
        assert!(
            app.picker_mru.lookup("files", &identity).is_some(),
            "expected MRU entry for {identity}"
        );
        // Re-open the picker. The accepted file should now
        // float to the top (MRU bonus > 0 vs 0 for the other).
        app.open_picker("files".into(), vec![tmp.display().to_string()]);
        let top = {
            let p = app.picker.as_ref().expect("picker open");
            let c = p.selected_candidate().expect("top selected");
            match p.routing_for(c).expect("routing") {
                lattice_picker::RoutingPayload::OpenFile { path } => path.clone(),
                other => panic!("expected OpenFile, got {other:?}"),
            }
        };
        assert_eq!(
            top, first_id,
            "previously-accepted file should float to top"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Slice 14d events: accepting a candidate publishes a
    /// `PickerAccepted` typed event on the §5.10 bus.
    /// Subscribers (the MRU index today; plugin telemetry
    /// hooks tomorrow) receive a synchronous fan-out with
    /// `source_id`, `identity`, and `ts` populated.
    #[test]
    fn picker_accept_publishes_typed_event() {
        let tmp = std::env::temp_dir().join(format!("lattice-evt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("alpha.rs"), "").unwrap();
        let mut app = app_with("hi\n", 5);
        app.picker_mru_path = None;
        // Subscribe before firing the picker.
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_picker::events::PickerAccepted>();
        app.event_bus.subscribe_typed(tx);
        app.open_picker("files".into(), vec![tmp.display().to_string()]);
        let _ = app.picker.as_ref().expect("picker open");
        app.apply(Action::PickerAccept);
        // The event lands synchronously through the bus's
        // forwarder closures; try_recv should see it.
        let evt = rx.try_recv().expect("PickerAccepted should fire");
        assert_eq!(evt.source_id, "files");
        assert!(evt.identity.as_deref().unwrap().starts_with("file:"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Slice 14d events: a successful `:picker <source>`
    /// seat publishes `PickerOpened` for subscribers
    /// (telemetry, plugin hooks). Sources that error in
    /// `init` skip the publish because the picker never
    /// actually opens.
    #[test]
    fn picker_open_publishes_typed_event() {
        let mut app = app_with("hi\n", 5);
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_picker::events::PickerOpened>();
        app.event_bus.subscribe_typed(tx);
        // `:picker buffers` always seats (the App's
        // BufferRegistry has at least the active doc).
        app.open_picker("buffers".into(), Vec::new());
        let evt = rx.try_recv().expect("PickerOpened should fire");
        assert_eq!(evt.source_id, "buffers");
    }

    /// Slice 14d: `picker.mru.enabled = false` short-circuits
    /// both the bonus snapshot (every candidate gets 0.0) and
    /// the record-on-accept path. After accepting a row, the
    /// MRU index is unchanged.
    #[test]
    fn picker_mru_enabled_false_skips_record() {
        let tmp = std::env::temp_dir().join(format!("lattice-mru-off-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("alpha.rs"), "").unwrap();
        let mut app = app_with("hi\n", 5);
        // Start from an empty MRU regardless of what's on the
        // user's disk cache; we measure the delta caused by
        // the accept, not the absolute count.
        app.picker_mru.clear();
        app.picker_mru_path = None;
        let before = app.picker_mru.len();
        // Disable MRU.
        app.config
            .parse_and_set_command("picker.mru.enabled=false")
            .unwrap();
        app.open_picker("files".into(), vec![tmp.display().to_string()]);
        let _ = app.picker.as_ref().expect("picker open");
        app.apply(Action::PickerAccept);
        // With MRU off, the accept must not add a record.
        assert_eq!(
            app.picker_mru.len(),
            before,
            "accept with MRU off must not change the index"
        );
        let _ = std::fs::remove_dir_all(&tmp);
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
