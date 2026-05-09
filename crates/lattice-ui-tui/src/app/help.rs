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
//! Sibling homes:
//! - `open_help`, `open_help_in_pane`,
//!   `activate_help_in_pane`, `seed_help_locals` --
//!   lifecycle / registry adoption; in `app/lifecycle.rs`.
//! - `do_describe_option`, `do_list_options` --
//!   in `app/options.rs`.
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::app::*;
    use crate::app::test_helpers::{app_in_command_mode, app_with, install_help};

    #[test]
    fn describe_command_opens_help_buffer_with_metadata() {
        let mut a = app_with("xx", 10);
        // `:describe-command ex:write` -- the registry knows about this.
        a.command_line = "describe-command ex:write".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("help view should open");
        assert!(h.title.contains("ex:write"));
        // First two lines: "ex:write  (ex-command)" + blank.
        let lines = h.lines();
        assert!(lines[0].contains("ex:write"));
        assert!(lines[0].contains("ex-command"));
    }

    #[test]
    fn describe_command_shows_source_link_to_registration_site() {
        // §5.11: every :describe-* must surface a file link to the
        // registration site. The buffer text is the rendered label
        // (`ex_commands.rs:LINE`) only -- the URL lives on the
        // parsed HelpLink target. Built-in commands record their
        // source via #[track_caller] when populate() runs.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:write".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        let body = h.content.as_string();
        assert!(
            body.contains("Defined at:"),
            "body should label the source: {body}"
        );
        assert!(
            body.contains("ex_commands.rs"),
            "body should contain the file path label: {body}"
        );
        // The HelpLink target carries the URL's resolved type.
        let has_source = h.links.iter().any(|l| {
            matches!(&l.target, crate::help::HelpLinkTarget::Source { path, .. }
                if path.to_string_lossy().contains("ex_commands.rs"))
        });
        assert!(has_source, "expected a Source HelpLink to ex_commands.rs");
        assert!(
            body.contains("(built-in)"),
            "body should label the source layer: {body}"
        );
    }

    #[test]
    fn describe_command_link_is_extracted_by_help_link_parser() {
        // The HelpBuffer constructor runs parse_help_links over the
        // body so the `[label](file:...)` markdown link becomes a
        // HelpLink with a Source target -- ready for the styled-link
        // renderer + follow-link motion (post-1.0).
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:quit".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        let source_link = h
            .links
            .iter()
            .find(|l| matches!(l.target, crate::help::HelpLinkTarget::Source { .. }));
        assert!(
            source_link.is_some(),
            "expected at least one HelpLink with Source target; got {:?}",
            h.links
        );
    }

    #[test]
    fn describe_command_emits_per_arg_anchors() {
        // §5.11 anchor system: every arg produces an `arg:<name>`
        // anchor, plus a parent `args` anchor for the section.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:apropos".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        // ex:apropos has one arg "pattern" -- expect "args" plus "arg:pattern".
        assert!(
            h.anchors.iter().any(|a| a.name == "args"),
            "expected 'args' anchor, got {:?}",
            h.anchors
        );
        assert!(
            h.anchors.iter().any(|a| a.name == "arg:pattern"),
            "expected 'arg:pattern' anchor, got {:?}",
            h.anchors
        );
    }

    #[test]
    fn describe_command_with_no_args_emits_no_arg_anchors() {
        // ex:quit has no args, so no `arg:*` or `args` anchors. The
        // `latency` anchor is always present (latency-class
        // declaration is mandatory metadata, DESIGN.md §5.2.5).
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:quit".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        assert!(
            h.anchors.iter().all(|a| a.name == "latency"),
            "ex:quit has no args; only the latency anchor is expected: {:?}",
            h.anchors,
        );
    }

    #[test]
    fn describe_command_anchor_lines_match_actual_section_headings() {
        // Verify the recorded line index actually points at the
        // section's heading row in the rendered content.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:apropos".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        let lines = h.lines();
        let args_anchor = h.anchors.iter().find(|a| a.name == "args").unwrap();
        let arg_anchor = h.anchors.iter().find(|a| a.name == "arg:pattern").unwrap();
        assert_eq!(lines[args_anchor.line as usize], "Arguments:");
        assert!(lines[arg_anchor.line as usize].contains("pattern"));
    }

    #[test]
    fn describe_command_arguments_section_renders_args_schema() {
        // ex:apropos has a schema with one required arg "pattern".
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:apropos".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.help_buffer.as_ref().unwrap().content.as_string();
        assert!(
            body.contains("Arguments:"),
            "expected Arguments section: {body}"
        );
        assert!(
            body.contains("pattern"),
            "expected arg name `pattern`: {body}"
        );
    }

    #[test]
    fn describe_key_shows_source_link_to_keymap_row() {
        let mut a = app_with("xx", 10);
        a.command_line = "describe-key j".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        let body = h.content.as_string();
        assert!(
            body.contains("Bound at:"),
            "describe-key output missing `Bound at:`: {body}"
        );
        assert!(
            body.contains("keymap.rs"),
            "describe-key output missing source label: {body}"
        );
        let has_source = h.links.iter().any(|l| {
            matches!(&l.target, crate::help::HelpLinkTarget::Source { path, .. }
                if path.to_string_lossy().contains("keymap.rs"))
        });
        assert!(has_source, "expected a Source HelpLink to keymap.rs");
        assert!(
            body.contains("(built-in)"),
            "describe-key output missing source-layer label: {body}"
        );
    }

    #[test]
    fn describe_key_renders_command_cross_reference_links() {
        // For `j`, three Normal/Visual/Help bindings -- the first
        // two have a `command`. The buffer text shows the LABEL
        // (`motion:line-down`); the URL is on the HelpLink target.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-key j".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        let body = h.content.as_string();
        assert!(
            body.contains("motion:line-down"),
            "expected `motion:line-down` label: {body}"
        );
        // The Command target carries the canonical command name.
        let has_cmd_link = h.links.iter().any(|l| {
            matches!(&l.target, crate::help::HelpLinkTarget::Command(c) if c == "motion:line-down")
        });
        assert!(has_cmd_link, "expected Command(motion:line-down) link");
    }

    #[test]
    fn describe_key_each_binding_has_its_own_source_link() {
        // `j` has 2 bindings -- Normal (line down) and Visual
        // (extend down). Help inherits Normal's `j` via active-
        // buffer routing (DESIGN.md §5.9), so it doesn't surface as
        // a separate descriptor. Each remaining binding should
        // surface its own `(file:...)` link because every
        // KeymapEntry's source is captured at its own row.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-key j".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        let source_links: Vec<_> = h
            .links
            .iter()
            .filter(|l| matches!(l.target, crate::help::HelpLinkTarget::Source { .. }))
            .collect();
        assert_eq!(
            source_links.len(),
            2,
            "expected 2 source links (one per binding); got {}: {:?}",
            source_links.len(),
            h.links
        );
        // Each link should point at a distinct line in keymap.rs.
        let mut lines: Vec<u32> = source_links
            .iter()
            .filter_map(|l| match &l.target {
                crate::help::HelpLinkTarget::Source { line, .. } => Some(*line),
                _ => None,
            })
            .collect();
        lines.sort();
        lines.dedup();
        assert_eq!(
            lines.len(),
            2,
            "expected 2 distinct source line numbers; got {lines:?}",
        );
    }

    #[test]
    fn describe_key_unknown_chord_renders_not_bound_message() {
        let mut a = app_with("xx", 10);
        a.command_line = "describe-key xyzzy".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.help_buffer.as_ref().unwrap().content.as_string();
        assert!(body.contains("not bound"), "body: {body}");
    }

    #[test]
    fn describe_command_resolves_alias_arg() {
        // `:describe-command apropos` -- the arg is an alias.
        // The handler must do two-stage resolution: alias `apropos`
        // -> canonical `ex:apropos` -> CommandSpec lookup.
        // Regression for the bug where the handler did a single
        // direct id_by_name(name) and failed for every alias.
        let mut a = app_in_command_mode("describe-command apropos");
        a.apply(Action::CommandLineSubmit);
        let h = a
            .help_buffer
            .as_ref()
            .expect("describe-command apropos should open help");
        assert!(
            h.title.contains("apropos"),
            "title should reference apropos, got `{}`",
            h.title
        );
        // Should NOT be the error path.
        assert!(
            a.last_message
                .as_ref()
                .map(|m| m.level != EchoLevel::Error)
                .unwrap_or(true)
        );
    }

    #[test]
    fn describe_command_resolves_short_alias_arg() {
        // Same shape but with a short alias (`w` -> `ex:write`).
        let mut a = app_in_command_mode("describe-command w");
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("describe-command w");
        // Title shows whatever the user typed; the resolved spec
        // is `ex:write`. Body must mention the canonical name to
        // confirm we resolved correctly.
        let body = h.content.as_string();
        assert!(
            body.contains("ex:write"),
            "body should reference ex:write: {body}"
        );
    }

    #[test]
    fn describe_command_unknown_alias_emits_error() {
        let mut a = app_in_command_mode("describe-command xyzzy-not-a-thing");
        a.apply(Action::CommandLineSubmit);
        assert!(a.help_buffer.is_none());
        let m = a.last_message.as_ref().unwrap();
        assert_eq!(m.level, EchoLevel::Error);
    }

    #[test]
    fn describe_command_with_no_args_omits_arguments_section() {
        // ex:quit has args_schema: vec![] -- no Arguments section.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:quit".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.help_buffer.as_ref().unwrap().content.as_string();
        assert!(
            !body.contains("Arguments:"),
            "Arguments section should be omitted: {body}"
        );
    }

    #[test]
    fn describe_command_unknown_emits_error_no_overlay() {
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:nope".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.help_buffer.is_none());
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn describe_buffer_renders_state_summary() {
        let mut a = app_with("hello\nworld", 10);
        a.command_line = "describe-buffer".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("help view should open");
        // Some predictable content lines.
        let body = h.content.as_string();
        assert!(body.contains("modal state"));
        assert!(body.contains("cursor:"));
        assert!(body.contains("dirty:"));
        assert!(body.contains("line count:"));
    }

    #[test]
    fn apropos_lists_matching_commands() {
        let mut a = app_with("xx", 10);
        a.command_line = "apropos write".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("help view should open");
        let body = h.content.as_string();
        // Both ex:write and ex:write-quit match the substring.
        assert!(body.contains("ex:write"));
        assert!(body.contains("ex:write-quit"));
    }

    #[test]
    fn apropos_no_matches_renders_empty_view() {
        let mut a = app_with("xx", 10);
        a.command_line = "apropos zxqzxqzxq".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().unwrap();
        let body = h.content.as_string();
        assert!(body.contains("no matches"));
    }

    #[test]
    fn help_with_no_arg_opens_index() {
        let mut a = app_with("xx", 10);
        a.command_line = "help".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("help open");
        assert_eq!(h.title, "help");
        let body = h.content.as_string();
        // Index page advertises the topic table.
        assert!(body.contains("Topic"), "got: {body}");
    }

    #[test]
    fn help_with_topic_opens_that_topic() {
        let mut a = app_with("xx", 10);
        a.command_line = "help folding".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("help open");
        assert_eq!(h.title, "help folding");
        let body = h.content.as_string();
        assert!(
            body.to_lowercase().contains("fold"),
            "expected fold-related content"
        );
    }

    #[test]
    fn help_unknown_topic_errors() {
        let mut a = app_with("xx", 10);
        a.command_line = "help nonexistent".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.help_buffer.is_none());
        let msg = a.last_message.as_ref().expect("error");
        assert!(msg.text.contains("no help topic"), "got: {}", msg.text);
    }

    #[test]
    fn describe_buffer_command_emits_topic_cross_link() {
        // `:buffers` (registered as `ex:buffers`) matches the
        // buffers topic's `buffer` pattern, so the describe view
        // should append a `[buffers](help:buffers)` cross-link.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:buffers".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("describe-command open");
        assert!(
            h.links
                .iter()
                .any(|l| matches!(&l.target, crate::help::HelpLinkTarget::Topic(name) if name == "buffers")),
            "expected `Topic(buffers)` link"
        );
    }

    #[test]
    fn help_topic_link_follow_dispatches_to_help() {
        // Open describe-command for a buffers cmd (which appends a
        // topic link), then follow that link via FollowLink.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-command ex:buffers".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("describe open");
        let link = h
            .links
            .iter()
            .find(|l| matches!(&l.target, crate::help::HelpLinkTarget::Topic(_)))
            .expect("topic link present")
            .clone();
        let target_pos = link.range.start;
        a.cursor = target_pos;
        a.apply(Action::FollowLink);
        let h = a.help_buffer.as_ref().expect("help reopen");
        assert_eq!(h.title, "help buffers");
    }

    #[test]
    fn help_anchor_link_scrolls_within_current_topic() {
        // `:help languages` ships intra-doc anchor links of the form
        // `[Section 1](#1-tree-sitter-core)`. Following one should
        // scroll the *current* help buffer to the matching heading,
        // not raise "no handler" / not switch topics.
        let mut a = app_with("xx", 10);
        a.command_line = "help languages".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.help_buffer.as_ref().expect("languages help open");
        // Find the anchor link to "#1-tree-sitter-core" (which the
        // languages topic ships in its quick-reference table).
        let link = h
            .links
            .iter()
            .find(|l| {
                matches!(
                    &l.target,
                    crate::help::HelpLinkTarget::Anchor(s) if s == "1-tree-sitter-core"
                )
            })
            .expect("anchor link to #1-tree-sitter-core present")
            .clone();
        let target_anchor_line = h
            .anchors
            .iter()
            .find(|a| a.name == "1-tree-sitter-core")
            .expect("anchor generated for `## 1. Tree-sitter, core`")
            .line;
        // Position the cursor on the link, then follow.
        // After unification, the active cursor lives on `app.cursor`
        // (regardless of buffer kind); we set it there.
        a.cursor = link.range.start;
        a.apply(Action::FollowLink);
        let h = a.help_buffer.as_ref().expect("help still open");
        assert_eq!(
            h.title, "help languages",
            "follow-link must NOT swap topics for an anchor jump"
        );
        assert_eq!(
            a.cursor.line, target_anchor_line,
            "cursor should land on the heading line"
        );
        assert_eq!(
            a.scroll, target_anchor_line,
            "scroll should follow the anchor"
        );
    }

    #[test]
    fn help_dismiss_clears_overlay_and_routes_back_to_document() {
        let mut a = app_with("xx", 10);
        install_help(
            &mut a,
            HelpBuffer::from_lines("test", vec!["a".into(), "b".into()]),
        );
        a.apply(Action::HelpDismiss);
        assert!(a.help_buffer.is_none());
        assert_eq!(a.active_buffer, BufferKind::Document);
    }

    #[test]
    fn help_motion_routes_through_active_buffer() {
        // `j` in help mode should resolve via the same chord grammar
        // as a code buffer, but the apply layer routes the resulting
        // motion to the help cursor (DESIGN.md §5.9 active-buffer
        // routing). 3 line_down invocations -> help cursor line 3,
        // scroll still 0 (viewport math is 10*7/10 - 2 = 5 rows).
        let mut a = app_with("xx", 10);
        let lines: Vec<String> = (0..50).map(|i| format!("line-{i}")).collect();
        install_help(&mut a, HelpBuffer::from_lines("scroll-test", lines));
        let line_down = a.builtins.line_down;
        for _ in 0..3 {
            a.apply(Action::Invoke(CommandInvocation::of(line_down.0)));
        }
        // After unification, `self.cursor` / `self.scroll` are
        // the active buffer's. The help_buffer's cursor field is
        // archival save-state synced at activation transitions.
        assert_eq!(a.cursor.line, 3);
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn help_motion_clamps_to_last_line() {
        let mut a = app_with("xx", 10);
        let lines: Vec<String> = (0..50).map(|i| format!("line-{i}")).collect();
        install_help(&mut a, HelpBuffer::from_lines("scroll-test", lines));
        let line_down = a.builtins.line_down;
        for _ in 0..1000 {
            a.apply(Action::Invoke(CommandInvocation::of(line_down.0)));
        }
        assert_eq!(a.cursor.line, 49);
        // Scroll keeps cursor on screen: viewport 10, cursor 49,
        // so scroll = 49 + 1 - 10 = 40. Production runtime sets
        // viewport per-frame via active_pane_content_height (which
        // shrinks for help popups); the test fixture sets a fixed
        // viewport of 10 and the assertion follows from that.
        assert_eq!(a.scroll, 40);
    }

    #[test]
    fn help_popup_inner_height_caps_at_twenty() {
        // 50-line help in a 60-row buffer: popup height clamps at
        // 20, inner = 18. Motion uses this as the viewport so
        // ensure_cursor_visible scrolls the popup -- not the full
        // pane -- when the cursor reaches the bottom row.
        let mut a = app_with("xx", 60);
        let lines: Vec<String> = (0..50).map(|i| format!("line-{i}")).collect();
        install_help(&mut a, HelpBuffer::from_lines("size", lines));
        assert_eq!(a.help_popup_inner_height(60), Some(18));
        // Confirm `active_pane_content_height` routes through the
        // popup-inner branch in State B, so the runtime feeds 18
        // into `set_viewport_height` (not the full 60-row pane).
        assert_eq!(a.active_pane_content_height(60), 18);
    }

    #[test]
    fn help_popup_inner_height_fits_short_content() {
        // 4-line help: popup auto-fits to height 6 (4 + 2 borders),
        // inner = 4. Cursor can never go off-popup-viewport
        // because the popup shows every line of the help buffer.
        let mut a = app_with("xx", 60);
        install_help(
            &mut a,
            HelpBuffer::from_lines("tiny", vec!["a".into(); 4]),
        );
        assert_eq!(a.help_popup_inner_height(60), Some(4));
    }

    #[test]
    fn help_popup_inner_height_none_when_pane_holds_help() {
        // In-pane help (e.g. `:lsp-log`) -- pane.buffer is Help, so
        // the help fills the pane and the regular pane-content-
        // height path applies. No overlay sizing.
        let mut a = app_with("xx", 60);
        let id = a.open_help_in_pane(HelpBuffer::from_lines("log", vec!["a".into(); 8]));
        assert_eq!(a.pane_tree.active().buffer_id, id);
        assert_eq!(a.help_popup_inner_height(60), None);
    }

    #[test]
    fn help_popup_j_past_last_line_does_not_advance_cursor() {
        // Regression for "j past last line in popup advanced
        // cursor.line internally" -- the pane viewport (60 rows)
        // hid the overshoot from `ensure_cursor_visible`, so
        // cursor.line crept past the last visible popup row and
        // every k afterwards had to walk back through the phantom
        // gap before any visible motion. Now `viewport_height`
        // matches the popup's inner height (18 here) AND the
        // motion path clamps `cursor.line` to last_addressable.
        let mut a = app_with("xx", 60);
        let lines: Vec<String> = (0..50).map(|i| format!("line-{i}")).collect();
        install_help(&mut a, HelpBuffer::from_lines("scroll", lines));
        a.set_viewport_height(a.active_pane_content_height(60));
        let line_down = a.builtins.line_down;
        let line_up = a.builtins.line_up;
        // `G` to the last line first so we're at the clamp.
        let goto_last = a.builtins.goto_last_line;
        a.apply(Action::Invoke(CommandInvocation::of(goto_last.0)));
        assert_eq!(a.cursor.line, 49);
        // Press j five times past the last line. cursor.line must
        // stay pinned at 49 -- no phantom overshoot.
        for _ in 0..5 {
            a.apply(Action::Invoke(CommandInvocation::of(line_down.0)));
        }
        assert_eq!(a.cursor.line, 49);
        // First k must move up immediately, not "unwind" any
        // overshoot.
        a.apply(Action::Invoke(CommandInvocation::of(line_up.0)));
        assert_eq!(a.cursor.line, 48);
    }

    #[test]
    fn help_motion_up_clamps_at_zero() {
        let mut a = app_with("xx", 10);
        install_help(
            &mut a,
            HelpBuffer::from_lines("scroll-test", vec!["a".into(); 30]),
        );
        let line_up = a.builtins.line_up;
        for _ in 0..1000 {
            a.apply(Action::Invoke(CommandInvocation::of(line_up.0)));
        }
        assert_eq!(a.cursor.line, 0);
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn help_horizontal_motion_runs_through_grammar() {
        let mut a = app_with("xx", 10);
        install_help(
            &mut a,
            HelpBuffer::from_lines("hl-test", vec!["hello world".into()]),
        );
        let char_right = a.builtins.char_right;
        let char_left = a.builtins.char_left;
        let line_end = a.builtins.line_end;
        let line_start = a.builtins.line_start;
        for _ in 0..3 {
            a.apply(Action::Invoke(CommandInvocation::of(char_right.0)));
        }
        assert_eq!(a.cursor.byte, 3);
        a.apply(Action::Invoke(CommandInvocation::of(char_left.0)));
        assert_eq!(a.cursor.byte, 2);
        a.apply(Action::Invoke(CommandInvocation::of(line_end.0)));
        // `motion:line-end` lands at `byte == line_len` (one past
        // the last byte) -- the same convention as the document
        // path. The grammar uses this position so operator targets
        // (d$, c$, y$) take an exclusive end.
        assert_eq!(a.cursor.byte, 11);
        a.apply(Action::Invoke(CommandInvocation::of(line_start.0)));
        assert_eq!(a.cursor.byte, 0);
    }

    #[test]
    fn help_gg_and_capital_g_route_through_grammar() {
        let mut a = app_with("xx", 10);
        install_help(&mut a, HelpBuffer::from_lines("jt", vec!["x".into(); 30]));
        let goto_first = a.builtins.goto_first_line;
        let goto_last = a.builtins.goto_last_line;
        a.apply(Action::Invoke(CommandInvocation::of(goto_last.0)));
        assert_eq!(a.cursor.line, 29);
        assert!(a.scroll > 0);
        a.apply(Action::Invoke(CommandInvocation::of(goto_first.0)));
        assert_eq!(a.cursor.line, 0);
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn help_count_motions_compose() {
        // `5j` -- the same count semantics as Normal mode.
        let mut a = app_with("xx", 10);
        let lines: Vec<String> = (0..50).map(|i| format!("l{i}")).collect();
        install_help(&mut a, HelpBuffer::from_lines("count", lines));
        let line_down = a.builtins.line_down;
        a.apply(Action::Invoke(
            CommandInvocation::of(line_down.0).with_count(lattice_grammar::command::Count(5)),
        ));
        assert_eq!(a.cursor.line, 5);
    }

    #[test]
    fn help_invoke_operator_echoes_read_only() {
        // Operators on a help buffer are rejected with a "read-only"
        // echo -- v1 doesn't model yank-against-help yet.
        let mut a = app_with("xx", 10);
        install_help(&mut a, HelpBuffer::from_lines("ro", vec!["abc".into(); 5]));
        let yank = a.builtins.yank;
        a.apply(Action::Invoke(
            CommandInvocation::of(yank.0).with_range(lattice_grammar::Range::CurrentLine),
        ));
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("read-only"), "got: {msg:?}");
        assert!(a.unnamed_register.is_none());
    }

    #[test]
    fn help_action_insert_blocked_with_echo() {
        // The read-only guard short-circuits direct mutation
        // actions so a stray Action::Insert while help is active
        // doesn't fall through onto the document.
        let mut a = app_with("xx", 10);
        let original = a.document.text();
        install_help(&mut a, HelpBuffer::from_lines("ro", vec!["abc".into()]));
        a.apply(Action::Insert("PWNED".into()));
        assert_eq!(a.document.text(), original);
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("read-only"), "got: {msg:?}");
    }

    #[test]
    fn help_buffer_active_mode_is_help_mode() {
        let mut a = app_with("hi", 5);
        let help = crate::help::HelpBuffer::from_lines(
            "test",
            vec!["line one".to_string()],
        );
        let help_id = a.open_help_in_pane(help);
        let active = a
            .active_modes
            .get(&help_id)
            .expect("active_modes populated for help");
        assert_eq!(active.major(), Some(crate::modes::HelpMode::mode_id()));
    }

    #[test]
    fn help_locals_carry_owner_metadata_for_describe_buffer() {
        let mut a = app_with("hi", 5);
        let help = crate::help::HelpBuffer::from_lines("t", vec!["body".into()]);
        let help_id = a.open_help_in_pane(help);
        let locals = a.buffer_locals.get(&help_id).unwrap();
        // Every seeded local should claim help-mode as its owner.
        let descriptors: Vec<_> = locals.iter_descriptors().collect();
        assert!(!descriptors.is_empty());
        for d in &descriptors {
            assert_eq!(d.owner_mode, "help-mode");
            assert!(
                d.name.starts_with("help-mode."),
                "name {:?} should be namespaced under help-mode",
                d.name
            );
        }
    }

}
