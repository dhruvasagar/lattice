//! Help-buffer App surface -- the `:describe-*` / `:apropos`
//! / `:help` / `:keymap` writers that compose help bodies.
//! Each method renders a help-buffer body via shared helpers
//! (`HelpBuffer::from_lines{,_and_anchors}` +
//! `with_markdown_syntax`) and hands it to `open_help`.
//!
//! Methods that live here:
//! - `do_open_help_topic` (`:help [topic]` -- index by
//!   default).
//! - `do_describe_command` (`:describe-command <name>` --
//!   renders the unified `Introspectable` surface plus
//!   See-also cross-links).
//! - `do_describe_buffer` (`:describe-buffer` --
//!   path / lang / cursor / dirty / counts dump).
//! - `do_describe_key` (`:describe-key <chord>` -- one
//!   render_introspection per binding, grouped by mode).
//! - `do_apropos` (`:apropos <pattern>` -- search names +
//!   docs).
//! - `do_list_keymap` (`:keymap` -- mode-grouped chord
//!   table).
//! - `do_help_follow_link` (`<CR>` in help mode -- the
//!   link-target dispatcher; routes Command / Execute /
//!   Chord / Topic / Anchor / Source variants through
//!   describe-command, do_open_help_topic, execute_ex_line,
//!   do_edit, etc.).
//!
//! Stays in app.rs (deferred):
//! - `open_help_in_pane`, `activate_help_in_pane`,
//!   `seed_help_locals` -- lifecycle/registry path; will
//!   migrate with the help-lifecycle slice.
//! - `do_describe_option`, `do_list_options` -- those
//!   already moved with options.rs.
//!
//! What does NOT live here: `HelpBuffer` itself
//! (`crate::help::HelpBuffer`), the markdown parser, link
//! extraction, anchor generation -- those are content-shape
//! concerns owned by `crate::help`. This module is App's
//! *workflow* layer above that.

use lattice_protocol::position::Position;

use super::{
    App, BufferKind, EchoLevel, PositionSource, PrevPaneState, line_byte_len,
    resolve_command_name_or_alias,
};
use crate::help::{HelpBuffer, command_link, key_link};

impl App {
    /// `:help [topic]` (DESIGN.md §5.11). With no topic the index
    /// is rendered (the topic registered as `index`); with a
    /// topic name the registry is queried and the topic body is
    /// rendered into a help buffer through the same markdown-
    /// highlighting path `:describe-command` uses. Unknown topic
    /// surfaces as a clear echo error so completion + typo
    /// recovery work.
    pub(super) fn do_open_help_topic(&mut self, topic: Option<&str>) {
        let name = topic.unwrap_or("index").to_string();
        let registry = self.help_topics.clone();
        let Some(t) = registry.lookup(&name) else {
            self.set_message(EchoLevel::Error, format!("no help topic: {name}"));
            return;
        };
        let body = t.body.render();
        let lines: Vec<String> = body.split('\n').map(|s| s.to_string()).collect();
        // Auto-generate anchors from `#` / `##` / ... headings so
        // intra-doc `[label](#slug)` links route to the right
        // section without authors hand-maintaining anchor tables.
        let anchors = crate::help::generate_heading_anchors(&lines);
        let title = if name == "index" {
            "help".to_string()
        } else {
            format!("help {name}")
        };
        self.open_help(
            HelpBuffer::from_lines_and_anchors(title, lines, anchors)
                .with_markdown_syntax(self.lang_registry.clone()),
        );
    }

    /// `:describe-command <name>` -- render via the unified
    /// `Introspectable` surface so every `:describe-*` formatter
    /// lands in `lattice_grammar::render_introspection`. Adding a
    /// new section to command help (e.g. example invocations) means
    /// extending `impl Introspectable for CommandSpec`, not editing
    /// the host.
    ///
    /// `anchor` (optional) scrolls the help buffer to a named
    /// anchor after rendering. Used by the cmdline's arg-aware
    /// `<C-h>` to jump to `arg:<name>`.
    pub(super) fn do_describe_command(&mut self, name: &str, anchor: Option<&str>) {
        // Two-stage resolution mirrors `excommand::parse_invocation`:
        // try the typed text as a registry name first (canonical
        // forms like `ex:write`), then fall back to alias expansion
        // (`write` -> `ex:write`). Lets users type either form.
        let Some(id) = resolve_command_name_or_alias(&self.registry, name) else {
            self.set_message(EchoLevel::Error, format!("no command named `{name}`"));
            return;
        };
        let Some(spec) = self.registry.lookup(id) else {
            self.set_message(EchoLevel::Error, format!("no command named `{name}`"));
            return;
        };
        let rendered = lattice_grammar::render_introspection(spec);
        let anchors: Vec<crate::help::HelpAnchor> = rendered
            .anchors
            .into_iter()
            .map(|a| crate::help::HelpAnchor {
                name: a.name,
                line: a.line,
            })
            .collect();
        let mut lines = rendered.lines;
        // Cross-link: append `See also: [topic](help:topic)` for
        // every help topic whose `related_command_patterns`
        // matches this command's name. Lets a user reading
        // `:describe-command operator:fold-create` jump to the
        // `folding` topic via `<CR>` on the link.
        let topics: Vec<String> = self
            .help_topics
            .topics_for_command(&spec.name)
            .map(|t| crate::help::topic_link(&t.name))
            .collect();
        if !topics.is_empty() {
            lines.push(String::new());
            lines.push(format!("See also: {}", topics.join(", ")));
        }
        let mut buffer =
            HelpBuffer::from_lines_and_anchors(format!("describe-command {name}"), lines, anchors)
                .with_markdown_syntax(self.lang_registry.clone());
        if let Some(a) = anchor {
            buffer.scroll_to_anchor(a);
        }
        self.open_help(buffer);
    }

    pub(super) fn do_describe_buffer(&mut self) {
        let mut lines: Vec<String> = Vec::new();
        let snap = self.document.snapshot();
        let path = snap
            .path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(no file)".to_string());
        let lang = lattice_syntax::Lang::detect_from_path(snap.path());
        let line_count = snap.buffer.line_count();
        let byte_count = snap.buffer.as_string().len();
        let dirty = if self.document.dirty() { "yes" } else { "no" };
        lines.push(format!("path:           {path}"));
        lines.push(format!("language:       {lang:?}"));
        lines.push(format!("modal state:    {:?}", self.modal));
        lines.push(format!(
            "cursor:         line {}, col {}",
            self.cursor.line + 1,
            self.cursor.byte
        ));
        lines.push(format!("dirty:          {dirty}"));
        lines.push(format!("line count:     {line_count}"));
        lines.push(format!("byte count:     {byte_count}"));
        lines.push(format!("registers set:  {}", self.registers.len()));
        lines.push(format!("marks set:      {}", self.marks.len()));
        lines.push(format!(
            "position-history depth: {}",
            self.position_history.len()
        ));
        lines.push(format!("macros stored:  {}", self.macros.len()));
        lines.push(format!("folds:          {}", self.folds.len()));
        lines.push(format!(
            "options:        number={}  relativenumber={}",
            self.show_line_numbers(),
            self.relative_line_numbers()
        ));
        self.open_help(
            HelpBuffer::from_lines("describe-buffer", lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        );
    }

    pub(super) fn do_apropos(&mut self, pattern: &str) {
        if pattern.is_empty() {
            self.set_message(EchoLevel::Error, "empty pattern".to_string());
            return;
        }
        let needle = pattern.to_ascii_lowercase();
        // Collect (name, kind, first_line_of_doc) for every spec whose
        // name or doc contains `needle` (case-insensitive).
        let mut hits: Vec<(String, &'static str, String)> = Vec::new();
        for name in self.registry.names() {
            let id = match self.registry.id_by_name(name) {
                Some(id) => id,
                None => continue,
            };
            let Some(spec) = self.registry.lookup(id) else {
                continue;
            };
            let name_match = spec.name.to_ascii_lowercase().contains(&needle);
            let doc_match = spec.doc.to_ascii_lowercase().contains(&needle);
            if name_match || doc_match {
                let first = spec.doc.lines().next().unwrap_or("").to_string();
                hits.push((spec.name.clone(), spec.kind.label(), first));
            }
        }
        hits.sort_by(|a, b| a.0.cmp(&b.0));
        let mut lines: Vec<String> = Vec::new();
        if hits.is_empty() {
            lines.push(format!("no matches for `{pattern}`"));
        } else {
            lines.push(format!("{} match(es) for `{pattern}`:", hits.len()));
            lines.push(String::new());
            // Compute alignment width once. We measure pre-link
            // wrapping so the visible text stays aligned even after
            // the renderer eventually styles the link markup.
            let name_w = hits.iter().map(|(n, _, _)| n.len()).max().unwrap_or(0);
            let kind_w = hits.iter().map(|(_, k, _)| k.len()).max().unwrap_or(0);
            for (name, kind, first) in hits {
                let pad_n = name_w.saturating_sub(name.len());
                let pad_k = kind_w.saturating_sub(kind.len());
                lines.push(format!(
                    "  {}{}  {}{}  {}",
                    command_link(&name),
                    " ".repeat(pad_n),
                    kind,
                    " ".repeat(pad_k),
                    first
                ));
            }
        }
        self.open_help(
            HelpBuffer::from_lines(format!("apropos {pattern}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        );
    }

    /// Format `:describe-key <chord>` (DESIGN.md §5.11). A chord may
    /// have entries in multiple modes (e.g. `j` is "line down" in
    /// Normal and Visual, "scroll" in Help). Each entry renders
    /// through the unified `Introspectable` surface so the source
    /// link + Action section come out uniformly with the other
    /// `:describe-*` commands.
    pub(super) fn do_describe_key(&mut self, chord: &str) {
        let hits = crate::keymap::lookup(chord);
        let mut lines: Vec<String> = Vec::new();
        if hits.is_empty() {
            lines.push(format!("`{chord}` is not bound in any mode."));
        } else {
            lines.push(format!("{} -- {} binding(s):", key_link(chord), hits.len()));
            for entry in hits {
                lines.push(String::new());
                for l in lattice_grammar::render_introspection_lines(entry) {
                    lines.push(l);
                }
            }
        }
        self.open_help(
            HelpBuffer::from_lines(format!("describe-key {chord}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        );
    }

    /// `K` (LSP hover) response handler / `:hover [markdown]`.
    /// Enters **State A**: popup overlay shown, focus stays on
    /// the main buffer. The popup auto-dismisses on the next
    /// motion (apply()'s post-dispatch hook) since it's anchored
    /// to the symbol the user K'd. To navigate inside the popup
    /// the user presses `K` again, which `do_lsp_hover_request`
    /// recognises as "focus into popup" -> State B.
    pub(super) fn do_open_hover(&mut self, markdown: &str) {
        let lines: Vec<String> = markdown.split('\n').map(String::from).collect();
        let buffer = HelpBuffer::from_lines("hover", lines)
            .with_markdown_syntax(self.lang_registry.clone());
        // State A: just set the help_buffer. Active stays on the
        // main buffer; self.cursor untouched. prev_pane_for_help
        // remains `None` -- the State-A auto-dismiss key.
        self.help_buffer = Some(buffer);
    }

    /// **State A -> State B**: focus moves into the popup. After
    /// this, the popup behaves like any other buffer -- vim
    /// grammar (motions, `/` search, `n`/`N`, `gg`/`G`, `:` ex
    /// commands) operates on the popup's content; the doc behind
    /// is frozen. Dismiss with `<Esc>` / `q` returns focus to
    /// the doc at the cursor it was on.
    pub(super) fn focus_help_popup(&mut self) {
        let Some(help) = self.help_buffer.as_ref() else {
            return;
        };
        let stash_cursor = help.cursor;
        let stash_scroll = help.scroll as u32;
        // Capture pre-State-B state so dismiss restores cleanly.
        let active = self.pane_tree.active();
        self.prev_pane_for_help = Some(PrevPaneState {
            buffer: active.buffer,
            buffer_id: active.buffer_id,
            cursor: self.cursor,
            scroll: self.scroll,
        });
        // Sync active pane's cursor / scroll stash *before*
        // swapping `active_buffer` to Help.
        self.snapshot_active_pane();
        self.cursor = stash_cursor;
        self.scroll = stash_scroll;
        self.active_buffer = BufferKind::Help;
    }

    /// `:HoverClose` -- dismiss the hover popup. Routes through
    /// the unified help-dismiss path so State A and State B both
    /// unwind cleanly (B restores via `prev_pane_for_help`; A
    /// just drops the popup).
    pub(super) fn do_close_hover(&mut self) {
        self.dismiss_help();
    }

    /// Close the help overlay and route input back to the document.
    /// Idempotent: closing when no help is open is a no-op.
    /// Pane-tracked help buffers stay in the registry (so `:bn` /
    /// `:b N` can return to them); only the popup slot is cleared
    /// and the active buffer flips back to Document.
    pub(super) fn dismiss_help(&mut self) {
        self.help_buffer = None;
        // Restore pre-help state if focus had moved into the help
        // (State B for hover; in-pane mode for `:lsp-log` etc.).
        // State A (popup shown but never focused) leaves
        // `prev_pane_for_help` as `None` -- nothing to restore;
        // active was never flipped to Help.
        if let Some(prev) = self.prev_pane_for_help.take() {
            self.cursor = prev.cursor;
            self.scroll = prev.scroll;
            let pane = self.pane_tree.active_mut();
            pane.buffer = prev.buffer;
            pane.buffer_id = prev.buffer_id;
            self.active_buffer = prev.buffer;
        } else {
            self.active_buffer = BufferKind::Document;
        }
    }

    pub(super) fn do_list_keymap(&mut self) {
        use crate::keymap::{BindingMode, entries};
        let mut by_mode: std::collections::BTreeMap<&str, Vec<&crate::keymap::KeymapEntry>> =
            std::collections::BTreeMap::new();
        // Stable iteration order: enumerate modes in a fixed order so
        // the rendered output reads top-down.
        let mode_order = [
            BindingMode::Normal,
            BindingMode::Visual,
            BindingMode::OperatorPending,
            BindingMode::AfterG,
            BindingMode::AfterZ,
            BindingMode::AfterMark,
            BindingMode::AfterJumpMarkLine,
            BindingMode::AfterJumpMarkExact,
            BindingMode::AfterRegister,
            BindingMode::AfterMacroStart,
            BindingMode::AfterMacroPlay,
            BindingMode::AfterFindChar,
            BindingMode::AfterTextObject,
            BindingMode::Insert,
            BindingMode::Replace,
            BindingMode::Command,
            BindingMode::Search,
            BindingMode::Help,
        ];
        for entry in entries() {
            by_mode.entry(entry.mode.label()).or_default().push(entry);
        }
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!(
            "Default keymap: {} bindings across {} modes",
            entries().len(),
            mode_order.len()
        ));
        lines.push(String::new());
        for mode in mode_order {
            let label = mode.label();
            let Some(group) = by_mode.get(label) else {
                continue;
            };
            lines.push(format!("[{label}]"));
            // Compute alignment width on the unwrapped chord string;
            // pad after the link wrapper so the visible text stays
            // column-aligned once the renderer styles links.
            let chord_w = group.iter().map(|e| e.chord.len()).max().unwrap_or(0);
            for entry in group {
                let pad = chord_w.saturating_sub(entry.chord.len());
                lines.push(format!(
                    "  {}{}  {}",
                    key_link(entry.chord),
                    " ".repeat(pad),
                    entry.doc
                ));
            }
            lines.push(String::new());
        }
        self.open_help(
            HelpBuffer::from_lines("keymap", lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        );
    }

    /// Follow the help link under the cursor (`<CR>` in help mode).
    /// Looks up the link by cursor position, then dispatches based
    /// on the link target's variant. Source links echo the
    /// `path:line` for now -- full file-open lands with multi-buffer.
    pub(super) fn do_help_follow_link(&mut self) {
        // Local helper: same range-containment logic as
        // `HelpBuffer::link_at` (covers same-line + multi-line
        // labels). M.3.2.c.5 retires the method on HelpBuffer
        // and shares this logic via a free function in
        // `crate::help`; for now the inline shape keeps the
        // diff narrow.
        fn range_contains_position(
            r: &lattice_protocol::position::Range,
            pos: lattice_protocol::position::Position,
        ) -> bool {
            if pos.line == r.start.line && pos.line == r.end.line {
                return pos.byte >= r.start.byte && pos.byte < r.end.byte;
            }
            if pos.line < r.start.line || pos.line > r.end.line {
                return false;
            }
            if pos.line == r.start.line {
                return pos.byte >= r.start.byte;
            }
            if pos.line == r.end.line {
                return pos.byte < r.end.byte;
            }
            true
        }

        let cursor = self.cursor;
        let Some(help) = self.help_buffer.as_ref() else {
            return;
        };
        // M.3.2.c.1: prefer help-mode-owned link data from
        // `buffer_locals`; fall back to the HelpBuffer's
        // struct field if the locals don't contain the link.
        // The fallback handles two cases:
        // (a) tests that synthesize a `HelpLink` and push it
        //     directly into `h.links` without going through the
        //     constructor's parsing path -- the link never
        //     reaches `seed_help_locals`.
        // (b) the bootstrap window after construction but
        //     before `seed_help_locals` runs.
        // M.3.2.c.5 retires the struct field; tests will
        // construct a `BufferLocals` directly at that point.
        //
        // Note: `app.help_buffer.id` (the construction-time
        // id) and the registered id (= `pane.buffer_id`) are
        // intentionally different (see comment in
        // `open_help_in_pane`); locals are keyed by the
        // registered id, so we look up via the active pane's
        // buffer id, not the popup-mode buffer's struct
        // field.
        let active_help_id = self.pane_tree.active().buffer_id;
        let link_from_locals = self
            .buffer_locals
            .get(&active_help_id)
            .and_then(|locals| locals.get::<crate::modes::HelpLinks>())
            .and_then(|hl| {
                hl.0.iter()
                    .find(|link| range_contains_position(&link.range, cursor))
                    .cloned()
            });
        let Some(link) = link_from_locals.or_else(|| {
            help.links
                .iter()
                .find(|link| range_contains_position(&link.range, cursor))
                .cloned()
        }) else {
            self.set_message(EchoLevel::Info, "no link under cursor".to_string());
            return;
        };
        // Clone the target so we can drop the `&help` borrow
        // before calling `push_position_history` (`&mut self`).
        let target = link.target.clone();
        let prev_help_cursor = cursor;
        match target {
            crate::help::HelpLinkTarget::Command(name) => {
                // Help -> help transition: record where we were in
                // the *current* help buffer so `<C-o>` brings us
                // back to it. The subsequent `do_describe_command`
                // replaces `help_buffer`, so the entry's
                // `buffer_id` becomes "stale" -- the unified ring
                // walker filters those out (see `do_walk_history`).
                self.push_position_history(prev_help_cursor, PositionSource::AutoJump);
                self.do_describe_command(&name, None);
            }
            crate::help::HelpLinkTarget::Execute(cmdline) => {
                // `[label](exec:CMDLINE)` -- run `:CMDLINE` as if
                // the user had typed it. Used by picker-style help
                // buffers (e.g. `:lsp-server-log`) where each row
                // dispatches the underlying ex-command on Enter.
                // Push history so `<C-o>` walks back into the
                // picker.
                self.push_position_history(prev_help_cursor, PositionSource::AutoJump);
                self.execute_ex_line(&cmdline);
            }
            crate::help::HelpLinkTarget::Chord(chord) => {
                self.push_position_history(prev_help_cursor, PositionSource::AutoJump);
                self.do_describe_key(&chord);
            }
            crate::help::HelpLinkTarget::Topic(name) => {
                self.push_position_history(prev_help_cursor, PositionSource::AutoJump);
                self.do_open_help_topic(Some(&name));
            }
            crate::help::HelpLinkTarget::Anchor(slug) => {
                // Intra-doc jump: scroll the *current* help buffer to
                // the anchor line and move the cursor there. Push
                // history so `<C-o>` returns to the link site.
                self.push_position_history(prev_help_cursor, PositionSource::AutoJump);
                // Anchor lookup runs against the help buffer's
                // anchor list; the cursor + scroll updates land
                // on the App's unified hot path.
                // M.3.2.c.1: read help-mode-owned anchors
                // through `buffer_locals` (keyed by the
                // registered id from `pane.buffer_id`, not
                // `help.id` -- see open_help_in_pane comment)
                // with a fallback to the struct field for
                // the bootstrap window / synthetic-test paths.
                let active_help_id = self.pane_tree.active().buffer_id;
                let target_line = self.help_buffer.as_ref().and_then(|h| {
                    let from_locals = self
                        .buffer_locals
                        .get(&active_help_id)
                        .and_then(|locals| locals.get::<crate::modes::HelpAnchors>())
                        .and_then(|anchors| {
                            anchors.0.iter().find(|a| a.name == slug).map(|a| a.line)
                        });
                    from_locals.or_else(|| {
                        h.anchors.iter().find(|a| a.name == slug).map(|a| a.line)
                    })
                });
                if let Some(line) = target_line {
                    let buffer = self.active_text();
                    let len = line_byte_len(&buffer, line);
                    self.cursor = Position::new(line, self.cursor.byte.min(len));
                    self.scroll = line;
                } else {
                    self.set_message(
                        EchoLevel::Warn,
                        format!("anchor not found: #{slug}"),
                    );
                }
            }
            crate::help::HelpLinkTarget::Source { path, line } => {
                // `[label](file:PATH:LINE)` -- open the file via
                // the existing `:e` machinery (multi-buffer
                // foundation, §5.9), then position the cursor at
                // the requested line. Push the help-side cursor
                // onto position history with `PluginPush` so
                // `<C-o>` walks back into the help view.
                self.push_position_history(prev_help_cursor, PositionSource::PluginPush);
                self.do_edit(Some(path.clone()), false);
                // `do_edit` may have set an error message + bailed
                // (e.g. permission denied). Don't try to jump in
                // that case -- the message is already on screen.
                if matches!(
                    self.last_message.as_ref().map(|m| m.level),
                    Some(EchoLevel::Error)
                ) {
                    return;
                }
                // Source links carry 1-based line numbers (matching
                // every editor + every `path:line` convention);
                // convert to the App's 0-based line index, clamping
                // to a valid line in the now-loaded buffer.
                let snap = self.document.snapshot();
                let last = snap.buffer.line_count().saturating_sub(1);
                let target_line = line.saturating_sub(1).min(last);
                self.cursor = Position::new(target_line, 0);
            }
            crate::help::HelpLinkTarget::Unresolved(url) => {
                self.set_message(EchoLevel::Warn, format!("no handler for `{url}`"));
            }
        }
    }
}
