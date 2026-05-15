//! Help-buffer App surface -- the `:describe-*` / `:apropos`
//! / `:help` / `:keymap` writers that compose help bodies.
//! Each method renders a help-buffer body via shared helpers
//! (`HelpContent::from_lines{,_and_anchors}` +
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
use crate::help::{HelpContent, command_link, key_link, mode_link};

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
        self.display_buffer(
            HelpContent::from_lines_and_anchors(title, lines, anchors)
                .with_markdown_syntax(self.lang_registry.clone()),
            lattice_core::ui::display::BufferDisplayCategory::HelpTopic,
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
        let mut content =
            HelpContent::from_lines_and_anchors(format!("describe-command {name}"), lines, anchors)
                .with_markdown_syntax(self.lang_registry.clone());
        // M.3.2.c.5: scroll-to-anchor was a method on `HelpBuffer`
        // that read `self.anchors`. With anchors retired off the
        // struct, look them up on the metadata directly.
        if let Some(a) = anchor
            && let Some(line) = crate::help::anchor_line(&content.metadata.anchors, a)
        {
            content.buffer.scroll = line as usize;
        }
        self.display_buffer(
            content,
            lattice_core::ui::display::BufferDisplayCategory::HelpDescribe,
        );
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
        lines.push(format!("macros stored:  {}", self.editor.macros.len()));
        lines.push(format!("folds:          {}", self.folds.len()));
        lines.push(format!(
            "options:        number={}  relativenumber={}",
            self.show_line_numbers(),
            self.relative_line_numbers()
        ));
        // Active modes on the document buffer. Each mode name is a
        // clickable `[name](mode:name)` link -- follow-link routes to
        // `:describe-mode <name>` and pushes position history so
        // `<C-o>` walks back to this view.
        lines.push(String::new());
        lines.push("## Active modes".to_string());
        let active = self.active_modes.get(&self.document_buffer_id);
        let major = active.and_then(|a| a.major());
        let minors: Vec<_> = active.map(|a| a.minors().to_vec()).unwrap_or_default();
        if let Some(major) = major {
            lines.push(format!("- major: {}", mode_link(major.as_str())));
        } else {
            lines.push("- major: (none)".to_string());
        }
        if minors.is_empty() {
            lines.push("- minors: (none)".to_string());
        } else {
            lines.push(format!("- minors ({}):", minors.len()));
            for id in minors {
                lines.push(format!("    - {}", mode_link(id.as_str())));
            }
        }
        self.display_buffer(
            HelpContent::from_lines("describe-buffer", lines)
                .with_markdown_syntax(self.lang_registry.clone()),
            lattice_core::ui::display::BufferDisplayCategory::HelpDescribe,
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
        self.display_buffer(
            HelpContent::from_lines(format!("apropos {pattern}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
            lattice_core::ui::display::BufferDisplayCategory::HelpApropos,
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
        self.display_buffer(
            HelpContent::from_lines(format!("describe-key {chord}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
            lattice_core::ui::display::BufferDisplayCategory::HelpDescribe,
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
        let content = HelpContent::from_lines("hover", lines)
            .with_markdown_syntax(self.lang_registry.clone());
        // M.4 follow-up: hover routes through the unified
        // dispatch like every other dedicated-buffer producer.
        // `BufferDisplayCategory::Hover` resolves to
        // `BufferDisplay::FloatingPopup(CursorAnchored)`, which
        // dispatches to `open_floating_popup` -- State A
        // semantics (active stays on the doc, cursor untouched,
        // hover-mode minor instead of help-mode).
        self.display_buffer(
            content,
            lattice_core::ui::display::BufferDisplayCategory::Hover,
        );
    }

    /// **State A -> State B**: focus moves into the popup. After
    /// this, the popup behaves like any other buffer -- vim
    /// grammar (motions, `/` search, `n`/`N`, `gg`/`G`, `:` ex
    /// commands) operates on the popup's content; the doc behind
    /// is frozen. Dismiss with `<Esc>` / `q` returns focus to
    /// the doc at the cursor it was on.
    pub(super) fn focus_help_popup(&mut self) {
        let Some(help) = self.popup_help() else {
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
    /// the unified `dismiss_popup` path so State A and State B
    /// both unwind cleanly (B restores via `prev_pane_for_help`;
    /// A just drops the popup).
    pub(super) fn do_close_hover(&mut self) {
        self.dismiss_popup();
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
        self.display_buffer(
            HelpContent::from_lines("keymap", lines)
                .with_markdown_syntax(self.lang_registry.clone()),
            lattice_core::ui::display::BufferDisplayCategory::HelpList,
        );
    }

    /// `:describe-events` (M.5.3.c) -- render every registered
    /// event's descriptor as a help buffer. Walks
    /// [`lattice_protocol::event_registry::EVENT_DESCRIPTORS`]
    /// (the `linkme` distributed slice every `register_event!`
    /// invocation pushes into); groups rows by source crate so
    /// the catalogue is easy to scan. Routes through
    /// [`Self::display_buffer`] with `HelpList` so the user's
    /// `:set help.list.display = ...` override applies.
    pub(super) fn do_describe_events(&mut self) {
        use lattice_protocol::event_registry::registered_events;
        // Group + sort: by source crate, then by name within
        // each group. Stable presentation across runs (linkme
        // iteration order is link-determined).
        let mut by_crate: std::collections::BTreeMap<
            &'static str,
            Vec<&'static lattice_protocol::event_registry::EventDescriptor>,
        > = std::collections::BTreeMap::new();
        for d in registered_events() {
            by_crate.entry(d.source_crate).or_default().push(d);
        }
        let mut total = 0usize;
        let mut lines: Vec<String> = Vec::new();
        lines.push("# Registered events".into());
        lines.push(String::new());
        if by_crate.is_empty() {
            lines.push("(none)".into());
        }
        for (source_crate, mut entries) in by_crate {
            entries.sort_by_key(|d| d.name);
            total += entries.len();
            lines.push(format!("## {source_crate} ({})", entries.len()));
            lines.push(String::new());
            for d in entries {
                lines.push(format!("- [{}](event:{})  {}", d.name, d.name, d.doc));
            }
            lines.push(String::new());
        }
        if total > 0 {
            lines.insert(
                1,
                format!(
                    "({total} registered event(s) across {} crate(s))",
                    lines.iter().filter(|l| l.starts_with("## ")).count()
                ),
            );
        }
        self.display_buffer(
            HelpContent::from_lines("describe-events", lines)
                .with_markdown_syntax(self.lang_registry.clone()),
            lattice_core::ui::display::BufferDisplayCategory::HelpList,
        );
    }

    /// `:describe-event <name>` (M.5.3.c) -- render the
    /// descriptor for one registered event. Mirrors
    /// `:describe-command` / `:describe-option`'s shape.
    pub(super) fn do_describe_event(&mut self, name: &str) {
        use lattice_protocol::event_registry::descriptor_by_name;
        let Some(d) = descriptor_by_name(name) else {
            self.set_message(EchoLevel::Error, format!("no event named `{name}`"));
            return;
        };
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("# event :: {}", d.name));
        lines.push(String::new());
        lines.push(format!("- source crate: `{}`", d.source_crate));
        lines.push(format!("- type-id name: `{}`", d.name));
        lines.push(String::new());
        lines.push(d.doc.to_string());
        lines.push(String::new());
        lines.push(
            "Subscribe via `EventBus::subscribe_typed::<T>(tx)` where `T` \
             is the concrete event struct exported by the source crate."
                .into(),
        );
        self.display_buffer(
            HelpContent::from_lines(format!("describe-event {name}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
            lattice_core::ui::display::BufferDisplayCategory::HelpDescribe,
        );
    }

    /// `:list-modes` (M.8) -- render every registered mode as a
    /// help buffer. Groups by kind (Major / Minor); each row
    /// shows the mode's id and `*` if currently active on the
    /// active buffer. Mode counterpart of `:options`.
    pub(super) fn do_list_modes(&mut self) {
        let mut majors: Vec<lattice_mode::ModeId> = Vec::new();
        let mut minors: Vec<lattice_mode::ModeId> = Vec::new();
        for (id, kind) in self.mode_registry.iter_meta() {
            match kind {
                lattice_mode::ModeKind::Major => majors.push(id),
                lattice_mode::ModeKind::Minor => minors.push(id),
            }
        }
        majors.sort_by_key(|m| m.as_str().to_string());
        minors.sort_by_key(|m| m.as_str().to_string());

        let buffer_id = self.document_buffer_id;
        let active = self.active_modes.get(&buffer_id);
        let active_major = active.and_then(|a| a.major());
        let is_minor_active =
            |id: lattice_mode::ModeId| -> bool { active.map(|a| a.has_minor(id)).unwrap_or(false) };

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!(
            "# Modes ({} registered)",
            majors.len() + minors.len(),
        ));
        lines.push(String::new());
        lines.push(
            "Mark `*` indicates the mode is active on the currently \
             focused buffer. For per-mode detail run \
             `:describe-mode <name>`. Toggle a mode with \
             `:<mode-name>` (e.g. `:lsp-mode`)."
                .into(),
        );
        lines.push(String::new());

        lines.push(format!("## majors ({})", majors.len()));
        lines.push(String::new());
        for id in &majors {
            let marker = if Some(*id) == active_major { "*" } else { " " };
            lines.push(format!("- {marker} [{id}](mode:{id})"));
        }
        lines.push(String::new());

        lines.push(format!("## minors ({})", minors.len()));
        lines.push(String::new());
        for id in &minors {
            let marker = if is_minor_active(*id) { "*" } else { " " };
            lines.push(format!("- {marker} [{id}](mode:{id})"));
        }

        self.display_buffer(
            HelpContent::from_lines("list-modes", lines)
                .with_markdown_syntax(self.lang_registry.clone()),
            lattice_core::ui::display::BufferDisplayCategory::HelpList,
        );
    }

    /// `:describe-mode <name>` (M.8) -- render one mode's
    /// metadata: id, kind, contributed option overrides
    /// (mapping each `TypeId` back to the option's display name
    /// via `OPTION_DECLS`), required capabilities, and current
    /// activation state on the active buffer. Mode counterpart
    /// of `:describe-option`.
    pub(super) fn do_describe_mode(&mut self, name: &str) {
        let mode_id = lattice_mode::ModeId::new(name);
        let Some(mode) = self.mode_registry.get(mode_id) else {
            self.set_message(EchoLevel::Error, format!("no mode named `{name}`"));
            return;
        };

        // TypeId → option name lookup. The OPTION_DECLS slice
        // carries `(name, type_id_fn)` pairs for every registered
        // option; we walk it to render the mode's contributed
        // overrides as readable names instead of opaque TypeIds.
        let type_id_to_name: std::collections::HashMap<std::any::TypeId, &'static str> =
            lattice_config::OPTION_DECLS
                .iter()
                .map(|d| ((d.type_id)(), d.name))
                .collect();

        let buffer_id = self.document_buffer_id;
        let active = self.active_modes.get(&buffer_id);
        let is_active = match mode.kind() {
            lattice_mode::ModeKind::Major => active.and_then(|a| a.major()) == Some(mode_id),
            lattice_mode::ModeKind::Minor => active.map(|a| a.has_minor(mode_id)).unwrap_or(false),
        };
        let kind_label = match mode.kind() {
            lattice_mode::ModeKind::Major => "major",
            lattice_mode::ModeKind::Minor => "minor",
        };

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("# mode :: {}", mode_id));
        lines.push(String::new());
        lines.push(format!("- kind: `{kind_label}`"));
        lines.push(format!(
            "- active on current buffer: {}",
            if is_active { "yes" } else { "no" }
        ));

        // Option contributions.
        let opts = mode.options();
        if opts.is_empty() {
            lines.push("- contributed options: (none)".into());
        } else {
            lines.push(format!("- contributed options ({}):", opts.iter().count(),));
            for ovr in opts.iter() {
                let name = type_id_to_name
                    .get(&ovr.option_type_id)
                    .copied()
                    .unwrap_or("(unknown option)");
                lines.push(format!("    - `{name}`"));
            }
        }

        // Capabilities.
        let caps = mode.required_capabilities();
        if caps == lattice_mode::CapabilitySet::empty() {
            lines.push("- required capabilities: (none)".into());
        } else {
            lines.push(format!("- required capabilities: `{caps:?}`"));
        }

        lines.push(String::new());
        lines.push(format!(
            "Toggle with `:{}`. For options the mode contributes, \
             see `:describe-option <name>`.",
            mode_id,
        ));

        self.display_buffer(
            HelpContent::from_lines(format!("describe-mode {name}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
            lattice_core::ui::display::BufferDisplayCategory::HelpDescribe,
        );
    }

    /// `:describe-option-resolution <name>` (M.8) -- show
    /// which resolver layer provides the resolved value for
    /// `<name>` on the active buffer. Helps debug surprising
    /// values where a mode contribution shadows a `:set` write
    /// or vice versa. The mode-architecture §6.1 layer model
    /// translated to a per-option introspection view.
    pub(super) fn do_describe_option_resolution(&mut self, name: &str) {
        let Some(spec) = self.config.lookup(name) else {
            self.set_message(EchoLevel::Error, format!("E518: Unknown option: {name}"));
            return;
        };
        // Derive TypeId from the canonical name by walking
        // OPTION_DECLS (the linkme slice every `options!` macro
        // invocation pushes into). The spec we just looked up
        // gives us `spec.name()` (canonical even if the user
        // typed an alias). If the option has aliases, we may
        // miss; fall back to a name-prefix scan if needed.
        let canonical_name = spec.name();
        let target_type_id = lattice_config::OPTION_DECLS
            .iter()
            .find(|d| d.name == canonical_name)
            .map(|d| (d.type_id)())
            .expect("registered option must have OPTION_DECLS entry");

        let buffer_id = self.document_buffer_id;
        let modes_snapshot = self
            .active_modes
            .get(&buffer_id)
            .cloned()
            .unwrap_or_default();
        let buffer_local = self.buffer_local_overrides.get(&buffer_id);

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("# option resolution :: {}", name));
        lines.push(String::new());
        lines.push(format!("- type:                 `{}`", spec.type_label()));
        lines.push(format!(
            "- resolved value:       `{}`",
            spec.get_formatted()
        ));
        lines.push(format!(
            "- typed-option (`:set`): `{}`",
            spec.get_formatted()
        ));
        lines.push(format!(
            "- default:              `{}`",
            spec.default_formatted()
        ));
        lines.push(String::new());
        lines.push("Layered contributions for this buffer (highest → lowest):".into());
        lines.push(String::new());

        // Layer 1: modal-state (always empty in v1).
        lines.push("- modal-state: (empty -- v1 wires no modal-overrides)".into());

        // Layer 2: buffer-local.
        let local_has = buffer_local
            .map(|set| set.iter().any(|o| o.option_type_id == target_type_id))
            .unwrap_or(false);
        if local_has {
            lines.push("- buffer-local (`:setlocal`): contributes ⭐".into());
        } else {
            lines.push("- buffer-local (`:setlocal`): (no override)".into());
        }

        // Layer 3: minors (in reverse activation order — last
        // activated has highest priority among modes).
        let minors: Vec<lattice_mode::ModeId> =
            modes_snapshot.minors().iter().copied().rev().collect();
        if minors.is_empty() {
            lines.push("- minors: (none active)".into());
        } else {
            let mut any_contributes = false;
            for minor_id in &minors {
                let Some(minor) = self.mode_registry.get(*minor_id) else {
                    continue;
                };
                let opts = minor.options();
                let contributes = opts.iter().any(|o| o.option_type_id == target_type_id);
                if contributes {
                    if !any_contributes {
                        lines.push("- minors:".into());
                        any_contributes = true;
                    }
                    lines.push(format!("    - `{minor_id}` ⭐"));
                }
            }
            if !any_contributes {
                lines.push(format!(
                    "- minors: {} active, none contribute this option",
                    minors.len(),
                ));
            }
        }

        // Layer 4: major.
        match modes_snapshot.major() {
            Some(major_id) => match self.mode_registry.get(major_id) {
                Some(major) => {
                    let opts = major.options();
                    let contributes = opts.iter().any(|o| o.option_type_id == target_type_id);
                    if contributes {
                        lines.push(format!("- major: `{major_id}` contributes ⭐"));
                    } else {
                        lines.push(format!("- major: `{major_id}` (no contribution)",));
                    }
                }
                None => {
                    lines.push(format!("- major: `{major_id}` (mode missing)"));
                }
            },
            None => {
                lines.push("- major: (none active)".into());
            }
        }

        // Layers 5/6: typed-option + built-in default.
        lines.push(format!("- typed-option layer: `{}`", spec.get_formatted(),));
        lines.push(format!(
            "- built-in default:   `{}`",
            spec.default_formatted(),
        ));

        lines.push(String::new());
        lines.push(
            "⭐ marks layers contributing this option. The highest \
             marked layer wins. Mode-architecture §6.1 explains the \
             layer priority order in detail."
                .into(),
        );

        self.display_buffer(
            HelpContent::from_lines(format!("describe-option-resolution {name}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
            lattice_core::ui::display::BufferDisplayCategory::HelpDescribe,
        );
    }

    /// `:customize [name]` (M.9.0) -- open the customize
    /// buffer. Three resolution paths:
    ///
    /// - **No arg.** Open the picker view: every registered
    ///   group + every mode with at least one customizable
    ///   option. Each row is a hyperlink that re-fires
    ///   `:customize <name>` on follow. Lets users browse
    ///   without knowing names up front.
    /// - **`<name>` ending in `-mode`.** Focused mode view:
    ///   the options the mode contributes via
    ///   `Mode::options()`. Useful for understanding what a
    ///   mode flips when it activates.
    /// - **`<name>` not ending in `-mode`.** Group view: every
    ///   option declared with that group, sectioned by group
    ///   header. Cross-mode browsing.
    ///
    /// Read-only listing in M.9.0; M.9.1 lands per-row
    /// navigation + Enter-to-edit. Today the user can use
    /// `:set NAME=VALUE` from the cmdline to edit any option
    /// they see in the form -- the values flow through the
    /// same registry the form reads from.
    pub(super) fn do_customize(&mut self, name: Option<&str>) {
        match name {
            None => self.do_customize_picker(),
            Some(n) if lattice_config::ends_with_mode_suffix(n) => self.do_customize_mode(n),
            Some(n) => self.do_customize_group(n),
        }
    }

    /// `:customize` (no args) -- render the picker view:
    /// every group + every registered mode that contributes
    /// at least one customizable option. Each row is a
    /// `[name](customize:name)` link.
    fn do_customize_picker(&mut self) {
        // Modes that contribute at least one customizable
        // option. We map TypeId → customizable flag via
        // OPTION_DECLS so non-customizable contributions
        // don't pull modes onto the list.
        let customizable_type_ids: std::collections::HashSet<std::any::TypeId> =
            lattice_config::OPTION_DECLS
                .iter()
                .filter(|d| d.customizable)
                .map(|d| (d.type_id)())
                .collect();
        let mut customisable_modes: Vec<lattice_mode::ModeId> = Vec::new();
        for (mode_id, _kind) in self.mode_registry.iter_meta() {
            if let Some(mode) = self.mode_registry.get(mode_id) {
                let opts = mode.options();
                if opts
                    .iter()
                    .any(|o| customizable_type_ids.contains(&o.option_type_id))
                {
                    customisable_modes.push(mode_id);
                }
            }
        }
        customisable_modes.sort_by_key(|m| m.as_str().to_string());

        // Groups: every registered OptionGroup plus its
        // option count.
        let mut group_counts: std::collections::BTreeMap<&'static str, (usize, &'static str)> =
            std::collections::BTreeMap::new();
        for g in lattice_config::GROUP_DECLS.iter() {
            group_counts.insert(g.name, (0, g.doc));
        }
        for d in lattice_config::OPTION_DECLS.iter() {
            if !d.customizable {
                continue;
            }
            if let Some(entry) = group_counts.get_mut(d.group_name) {
                entry.0 += 1;
            }
        }

        let mut lines: Vec<String> = Vec::new();
        lines.push("# Customize".into());
        lines.push(String::new());
        lines.push(
            "Pick a group to browse options across modes, or a mode \
             to see what it contributes. `:customize <name>` opens \
             the focused view; this picker is just navigation."
                .into(),
        );
        lines.push(String::new());

        lines.push(format!("## groups ({})", group_counts.len()));
        lines.push(String::new());
        for (name, (count, doc)) in &group_counts {
            lines.push(format!("- [{name}](customize:{name}) ({count}) -- {doc}"));
        }
        lines.push(String::new());

        lines.push(format!("## modes ({})", customisable_modes.len(),));
        lines.push(String::new());
        for id in &customisable_modes {
            lines.push(format!("- [{id}](customize:{id})"));
        }

        self.display_buffer(
            HelpContent::from_lines("customize", lines)
                .with_markdown_syntax(self.lang_registry.clone()),
            lattice_core::ui::display::BufferDisplayCategory::HelpList,
        );
    }

    /// `:customize <group>` -- render every customizable
    /// option in `<group>`. Each row shows the option's
    /// canonical name + aliases, type, current value, default
    /// (when it differs), and the doc string.
    fn do_customize_group(&mut self, group_name: &str) {
        let group_doc = lattice_config::GROUP_DECLS
            .iter()
            .find(|g| g.name == group_name)
            .map(|g| g.doc);
        let Some(doc) = group_doc else {
            self.set_message(EchoLevel::Error, format!("no group named `{group_name}`"));
            return;
        };

        let mut entries: Vec<&'static lattice_config::OptionDeclMetadata> =
            lattice_config::OPTION_DECLS
                .iter()
                .filter(|d| d.customizable && d.group_name == group_name)
                .copied()
                .collect();
        entries.sort_by_key(|d| d.name);

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("# customize :: {group_name}"));
        lines.push(String::new());
        lines.push(doc.to_string());
        lines.push(String::new());
        if entries.is_empty() {
            lines.push("(no customizable options in this group)".into());
        } else {
            lines.push(format!("{} option(s):", entries.len()));
            lines.push(String::new());
            for meta in &entries {
                self.append_customize_row(&mut lines, meta);
            }
        }
        lines.push(String::new());
        lines.push(
            "To edit any option above run `:set NAME=VALUE` from \
             the cmdline. Per-row edit affordances land in M.9.1."
                .into(),
        );
        self.display_buffer(
            HelpContent::from_lines(format!("customize {group_name}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
            lattice_core::ui::display::BufferDisplayCategory::HelpList,
        );
    }

    /// `:customize <mode-name>` -- render every option the
    /// mode contributes via `Mode::options()`. Each row shows
    /// the same metadata as the group view, plus a
    /// `[mode-shadow]` indicator when the contribution is
    /// active on the active buffer (the contribution would
    /// shadow a `:set` write of a different value).
    fn do_customize_mode(&mut self, mode_name: &str) {
        let mode_id = lattice_mode::ModeId::new(mode_name);
        let Some(mode) = self.mode_registry.get(mode_id) else {
            self.set_message(EchoLevel::Error, format!("no mode named `{mode_name}`"));
            return;
        };

        // Build TypeId → metadata lookup so we can render the
        // mode's contributed TypeIds with full option detail.
        let by_type_id: std::collections::HashMap<
            std::any::TypeId,
            &'static lattice_config::OptionDeclMetadata,
        > = lattice_config::OPTION_DECLS
            .iter()
            .map(|d| ((d.type_id)(), *d))
            .collect();

        let buffer_id = self.document_buffer_id;
        let active = self.active_modes.get(&buffer_id);
        let mode_active_here = match mode.kind() {
            lattice_mode::ModeKind::Major => active.and_then(|a| a.major()) == Some(mode_id),
            lattice_mode::ModeKind::Minor => active.map(|a| a.has_minor(mode_id)).unwrap_or(false),
        };

        let mut entries: Vec<&'static lattice_config::OptionDeclMetadata> = Vec::new();
        for ovr in mode.options().iter() {
            if let Some(meta) = by_type_id.get(&ovr.option_type_id)
                && meta.customizable
            {
                entries.push(meta);
            }
        }
        entries.sort_by_key(|d| d.name);
        entries.dedup_by_key(|d| d.name);

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("# customize :: {mode_id}"));
        lines.push(String::new());
        lines.push(format!(
            "Mode kind: `{}`. {} on the active buffer.",
            match mode.kind() {
                lattice_mode::ModeKind::Major => "major",
                lattice_mode::ModeKind::Minor => "minor",
            },
            if mode_active_here {
                "Active"
            } else {
                "Inactive"
            },
        ));
        lines.push(String::new());
        if entries.is_empty() {
            lines.push("(this mode contributes no customizable options)".into());
        } else {
            lines.push(format!("Contributes {} option(s):", entries.len()));
            lines.push(String::new());
            for meta in &entries {
                self.append_customize_row(&mut lines, meta);
                if mode_active_here {
                    lines.push(
                        "    [mode-shadow] this mode's contribution is \
                         active on the active buffer; a `:set` write \
                         here will be overridden by the mode-contribution \
                         layer until the mode deactivates."
                            .into(),
                    );
                }
            }
        }
        lines.push(String::new());
        lines.push(
            "To edit any option above run `:set NAME=VALUE` from \
             the cmdline. Per-row edit affordances land in M.9.1."
                .into(),
        );
        self.display_buffer(
            HelpContent::from_lines(format!("customize {mode_name}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
            lattice_core::ui::display::BufferDisplayCategory::HelpList,
        );
    }

    /// Shared row formatter for the customize views. Renders
    /// one option's metadata in the
    /// `:options`-listing-compatible shape. Wraps the option
    /// name in a `[NAME](customize-edit:NAME)` link so `<CR>`
    /// on the row prefills the cmdline with `:set NAME=current`
    /// for inline editing (M.9.2).
    fn append_customize_row(
        &self,
        lines: &mut Vec<String>,
        meta: &lattice_config::OptionDeclMetadata,
    ) {
        let spec = self.config.lookup(meta.name);
        let aliases = spec
            .as_ref()
            .map(|s| s.aliases())
            .filter(|a| !a.is_empty())
            .map(|a| format!(" [{}]", a.join(", ")))
            .unwrap_or_default();
        let type_label = (meta.type_label)();
        let default = (meta.default_formatted)();
        let current = spec
            .as_ref()
            .map(|s| s.get_formatted())
            .unwrap_or_else(|| "?".into());
        // M.9.2: name is a link target -- `<CR>` fires the
        // cmdline-prefill edit. The link's label cleans down
        // to the bare option name during render
        // (`extract_links_and_clean`), so the visible row text
        // is unchanged from M.9.0.
        let name_link = format!("[{0}](customize-edit:{0})", meta.name);
        let header = if current == default {
            format!(
                "- **{}**{} : {} = {}",
                name_link, aliases, type_label, current
            )
        } else {
            format!(
                "- **{}**{} : {} = {} (default: {})",
                name_link, aliases, type_label, current, default,
            )
        };
        lines.push(header);
        for doc_line in meta.doc.lines() {
            let trimmed = doc_line.trim();
            if !trimmed.is_empty() {
                lines.push(format!("    {trimmed}"));
            }
        }
        if let Some(values) = spec.as_ref().and_then(|s| s.enumerate_values()) {
            lines.push(format!("    values: {}", values.join(", ")));
        }
        lines.push(String::new());
    }

    /// `<CR>` on a customize-edit link (M.9.2). Prefills the
    /// cmdline with `:set NAME=current_value` and switches to
    /// Command mode so the user can edit the value and submit.
    /// The actual write goes through the existing `:set` parser
    /// (validates, cascades, fires `OptionChanged` on the bus).
    ///
    /// `read-only` and other `customizable = false` options are
    /// rejected -- they're not in the customize listing in the
    /// first place, but a stale link from a prior render
    /// shouldn't crash. Echoes an info message.
    fn do_customize_edit(&mut self, name: &str) {
        let Some(spec) = self.config.lookup(name) else {
            self.set_message(EchoLevel::Error, format!("E518: Unknown option: {name}"));
            return;
        };
        let current = spec.get_formatted();
        // For booleans, surface the `noNAME` alternative form
        // by prefilling without `=` -- the user can overwrite
        // with `noNAME` directly. For non-bool, prefill with
        // `name=current` so the user sees the value and can
        // edit it inline.
        let prefill = if spec.is_bool() {
            format!("set {name}={current}")
        } else {
            format!("set {name}={current}")
        };
        self.command_line = prefill;
        self.modal = lattice_grammar::ModalState::Command;
    }

    /// `:tutor [N]` -- open the interactive Lattice tutor
    /// lesson `N` (default: 1). Vim-tutor pattern: the lesson
    /// content is embedded in the binary via `include_str!`,
    /// and each invocation copies a fresh practice file to a
    /// temp path so the user can edit / practice motions on
    /// the file itself without losing the canonical lesson
    /// source. Re-running `:tutor` starts over.
    ///
    /// v1 ships lesson 1 only. Subsequent lessons land as
    /// additional `docs/user/tutor/lesson-N.md` files, each
    /// added to this match.
    pub(super) fn do_tutor(&mut self, lesson: Option<u32>) {
        let lesson_num = lesson.unwrap_or(1);
        let lesson_text: &'static str = match lesson_num {
            1 => include_str!("../../../../docs/user/tutor/lesson-1.md"),
            n => {
                self.set_message(
                    crate::app::EchoLevel::Error,
                    format!(
                        "lesson {n} doesn't exist yet (lessons 1 available); \
                         contributions welcome"
                    ),
                );
                return;
            }
        };
        // Copy the embedded lesson to a temp file so the user
        // can edit / practice without affecting the binary's
        // canonical copy. Each invocation overwrites, so re-
        // running `:tutor` starts the user fresh.
        let mut path = std::env::temp_dir();
        path.push(format!("lattice-tutor-lesson-{lesson_num}.txt"));
        if let Err(e) = std::fs::write(&path, lesson_text) {
            self.set_message(
                crate::app::EchoLevel::Error,
                format!("tutor: failed to write lesson file: {e}"),
            );
            return;
        }
        // Open the temp file via the existing `:e` mechanism.
        // The user can `:w` to save edits to the temp path; the
        // canonical lesson source stays untouched.
        self.do_edit(Some(path), false);
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
        if self.popup_buffer.is_none() {
            return;
        }
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
        // Note: the popup buffer's own id and the registered id
        // (= `pane.buffer_id`) are intentionally different (see
        // comment in `open_help_in_pane`); locals are keyed by
        // the registered id, so we look up via the active
        // pane's buffer id, not the popup buffer's id.
        // M.3.2.c.5: in centred-popup mode `pane.buffer_id` is the
        // doc behind the popup, not the popup's content. The
        // popup's construction id (`help.id`) is the locals key
        // `open_popup` seeded under, so prefer it; fall back to
        // `pane.buffer_id` for the in-pane case (where the pane
        // was swapped to the registered help id).
        let active_help_id = self
            .popup_buffer
            .unwrap_or_else(|| self.pane_tree.active().buffer_id);
        let Some(link) = self
            .buffer_locals
            .get(&active_help_id)
            .and_then(|locals| locals.get::<crate::modes::HelpLinks>())
            .and_then(|hl| {
                hl.0.iter()
                    .find(|link| range_contains_position(&link.range, cursor))
                    .cloned()
            })
            .or_else(|| {
                // For in-pane help where popup_buffer != pane.buffer_id,
                // try the pane's id too.
                let pane_id = self.pane_tree.active().buffer_id;
                if pane_id == active_help_id {
                    return None;
                }
                self.buffer_locals
                    .get(&pane_id)
                    .and_then(|locals| locals.get::<crate::modes::HelpLinks>())
                    .and_then(|hl| {
                        hl.0.iter()
                            .find(|link| range_contains_position(&link.range, cursor))
                            .cloned()
                    })
            })
        else {
            self.set_message(EchoLevel::Info, "no link under cursor".to_string());
            return;
        };
        // Clone the target so we can drop the `&help` borrow
        // before calling `push_position_history` (`&mut self`).
        let target = link.target.clone();
        let prev_help_cursor = cursor;
        match target {
            crate::help::HelpLinkTarget::Command(name) => {
                // Help → help link: `do_describe_command` swaps
                // the existing popup's content in place (see
                // `open_popup`'s Help-active branch) and pushes a
                // frame onto `popup_back_stack`. `<C-o>` walks the
                // back-stack so we deliberately skip the outer
                // `push_position_history` -- otherwise the user
                // gets a wasted `<C-o>` step on a dedup entry
                // pointing at the link cursor in the new content.
                self.do_describe_command(&name, None);
            }
            crate::help::HelpLinkTarget::Execute(cmdline) => {
                // `[label](exec:CMDLINE)` -- run `:CMDLINE` as if
                // the user had typed it. Used by picker-style help
                // buffers (e.g. `:lsp-server-log`) where each row
                // dispatches the underlying ex-command on Enter.
                // The cmdline may navigate outside Help (e.g. open
                // a file), so push position-history for outer
                // `<C-o>` continuity.
                self.push_position_history(prev_help_cursor, PositionSource::AutoJump);
                self.execute_ex_line(&cmdline);
            }
            crate::help::HelpLinkTarget::Chord(chord) => {
                // Same content-swap path as Command -- skip the
                // outer push; back-stack handles `<C-o>`.
                self.do_describe_key(&chord);
            }
            crate::help::HelpLinkTarget::Topic(name) => {
                self.do_open_help_topic(Some(&name));
            }
            crate::help::HelpLinkTarget::Customize(name) => {
                self.do_customize(Some(&name));
            }
            crate::help::HelpLinkTarget::CustomizeEdit(name) => {
                self.do_customize_edit(&name);
            }
            crate::help::HelpLinkTarget::Mode(name) => {
                // `[label](mode:NAME)` -- describe the mode. Same
                // content-swap path as Command/Topic; the popup
                // back-stack handles `<C-o>`.
                self.do_describe_mode(&name);
            }
            crate::help::HelpLinkTarget::Anchor(slug) => {
                // Intra-doc jump: scroll the *current* help buffer to
                // the anchor line and move the cursor there. Push
                // history so `<C-o>` returns to the link site.
                self.push_position_history(prev_help_cursor, PositionSource::AutoJump);
                // M.3.2.c.5: anchors live in buffer_locals
                // exclusively. Look up under the popup buffer's
                // own id first (centred-popup case where
                // pane.buffer_id is the doc behind the popup),
                // then fall back to the pane's id (in-pane case).
                let popup_id = self.popup_buffer;
                let pane_id = self.pane_tree.active().buffer_id;
                let target_line = popup_id
                    .and_then(|id| self.buffer_locals.get(&id))
                    .and_then(|locals| locals.get::<crate::modes::HelpAnchors>())
                    .and_then(|anchors| anchors.0.iter().find(|a| a.name == slug).map(|a| a.line))
                    .or_else(|| {
                        self.buffer_locals
                            .get(&pane_id)
                            .and_then(|locals| locals.get::<crate::modes::HelpAnchors>())
                            .and_then(|anchors| {
                                anchors.0.iter().find(|a| a.name == slug).map(|a| a.line)
                            })
                    });
                if let Some(line) = target_line {
                    let buffer = self.active_text();
                    let len = line_byte_len(&buffer, line);
                    self.cursor = Position::new(line, self.cursor.byte.min(len));
                    self.scroll = line;
                } else {
                    self.set_message(EchoLevel::Warn, format!("anchor not found: #{slug}"));
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

    use crate::app::test_helpers::{app_in_command_mode, app_with, install_help};
    use crate::app::*;
    use crate::help::HelpContent;

    #[test]
    fn describe_command_opens_help_buffer_with_metadata() {
        let mut a = app_with("xx", 10);
        // `:describe-command ex:write` -- the registry knows about this.
        a.command_line = "describe-command ex:write".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("help view should open");
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
        let h = a.popup_help().unwrap();
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
        let links = a.popup_help_links().expect("help links seeded");
        let has_source = links.iter().any(|l| {
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
        let _ = a.popup_help().unwrap();
        let links = a.popup_help_links().expect("help links seeded");
        let source_link = links
            .iter()
            .find(|l| matches!(l.target, crate::help::HelpLinkTarget::Source { .. }));
        assert!(
            source_link.is_some(),
            "expected at least one HelpLink with Source target; got {:?}",
            links
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
        let _ = a.popup_help().unwrap();
        // ex:apropos has one arg "pattern" -- expect "args" plus "arg:pattern".
        let anchors = a.popup_help_anchors().expect("help anchors seeded");
        assert!(
            anchors.iter().any(|a| a.name == "args"),
            "expected 'args' anchor, got {anchors:?}"
        );
        assert!(
            anchors.iter().any(|a| a.name == "arg:pattern"),
            "expected 'arg:pattern' anchor, got {anchors:?}"
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
        let _ = a.popup_help().unwrap();
        let anchors = a.popup_help_anchors().expect("help anchors seeded");
        assert!(
            anchors.iter().all(|a| a.name == "latency"),
            "ex:quit has no args; only the latency anchor is expected: {anchors:?}",
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
        let lines = a.popup_help().unwrap().lines();
        let anchors = a.popup_help_anchors().expect("help anchors seeded");
        let args_anchor = anchors.iter().find(|a| a.name == "args").unwrap();
        let arg_anchor = anchors.iter().find(|a| a.name == "arg:pattern").unwrap();
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
        let body = a.popup_help().unwrap().content.as_string();
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
        let h = a.popup_help().unwrap();
        let body = h.content.as_string();
        assert!(
            body.contains("Bound at:"),
            "describe-key output missing `Bound at:`: {body}"
        );
        assert!(
            body.contains("keymap.rs"),
            "describe-key output missing source label: {body}"
        );
        let links = a.popup_help_links().expect("help links seeded");
        let has_source = links.iter().any(|l| {
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
        let h = a.popup_help().unwrap();
        let body = h.content.as_string();
        assert!(
            body.contains("motion:line-down"),
            "expected `motion:line-down` label: {body}"
        );
        // The Command target carries the canonical command name.
        let links = a.popup_help_links().expect("help links seeded");
        let has_cmd_link = links.iter().any(|l| {
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
        let _ = a.popup_help().unwrap();
        let links = a.popup_help_links().expect("help links seeded");
        let source_links: Vec<_> = links
            .iter()
            .filter(|l| matches!(l.target, crate::help::HelpLinkTarget::Source { .. }))
            .collect();
        assert_eq!(
            source_links.len(),
            2,
            "expected 2 source links (one per binding); got {}: {:?}",
            source_links.len(),
            links
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
        let body = a.popup_help().unwrap().content.as_string();
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
            .popup_help()
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
        let h = a.popup_help().expect("describe-command w");
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
        assert!(a.popup_buffer.is_none());
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
        let body = a.popup_help().unwrap().content.as_string();
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
        assert!(a.popup_buffer.is_none());
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn describe_buffer_renders_state_summary() {
        let mut a = app_with("hello\nworld", 10);
        a.command_line = "describe-buffer".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("help view should open");
        // Some predictable content lines.
        let body = h.content.as_string();
        assert!(body.contains("modal state"));
        assert!(body.contains("cursor:"));
        assert!(body.contains("dirty:"));
        assert!(body.contains("line count:"));
    }

    #[test]
    fn describe_buffer_lists_active_modes_as_links() {
        // The "Active modes" section names the buffer's major +
        // every minor; each name renders as a `(mode:NAME)` link so
        // `<CR>` routes to `:describe-mode NAME`.
        let mut a = app_with("hello", 10);
        a.command_line = "describe-buffer".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let _ = a.popup_help().expect("help view should open");
        let body = a.popup_help().unwrap().content.as_string();
        assert!(body.contains("Active modes"), "section missing: {body}");
        // Default plain-text buffer activates `text-mode` as major.
        let links = a.popup_help_links().expect("help links seeded");
        let has_major_mode_link = links.iter().any(|l| {
            matches!(
                &l.target,
                crate::help::HelpLinkTarget::Mode(name) if name == "text-mode"
            )
        });
        assert!(
            has_major_mode_link,
            "expected a `mode:text-mode` link in describe-buffer; got {links:?}"
        );
    }

    #[test]
    fn describe_buffer_mode_link_follows_to_describe_mode() {
        // Click on a mode-link inside `:describe-buffer` and the
        // popup re-renders as `:describe-mode <name>` (the title
        // changes; the body now describes the mode). The popup
        // buffer id is reused -- the content swaps in place so
        // jump-list / marks / search state keyed on the popup
        // stay coherent across in-popup navigation.
        let mut a = app_with("hello", 10);
        a.command_line = "describe-buffer".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let initial_id = a.popup_buffer.expect("describe-buffer should open");
        let link = a
            .popup_help_links()
            .expect("help links seeded")
            .iter()
            .find(|l| {
                matches!(
                    &l.target,
                    crate::help::HelpLinkTarget::Mode(name) if name == "text-mode"
                )
            })
            .expect("text-mode link present")
            .clone();
        a.cursor = link.range.start;
        a.apply(Action::FollowLink);
        let h = a.popup_help().expect("popup should still be open");
        assert_eq!(h.title, "describe-mode text-mode");
        assert!(h.content.as_string().contains("text-mode"));
        assert_eq!(
            a.popup_buffer,
            Some(initial_id),
            "popup buffer id should be reused across in-popup navigation",
        );
    }

    #[test]
    fn ctrl_o_after_mode_link_returns_to_describe_buffer() {
        // The user's requested behaviour: `<C-o>` after following a
        // mode link must walk back into the popup (not bail to the
        // document). The position-history push in
        // `do_help_follow_link` plus the registry-keep that
        // `open_popup` performs while `active_buffer == Help` make
        // the previous popup buffer reachable for the jump-history
        // walker.
        let mut a = app_with("hello", 10);
        a.command_line = "describe-buffer".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let _ = a.popup_help().expect("describe-buffer should open");
        let link = a
            .popup_help_links()
            .expect("help links seeded")
            .iter()
            .find(|l| {
                matches!(
                    &l.target,
                    crate::help::HelpLinkTarget::Mode(name) if name == "text-mode"
                )
            })
            .expect("text-mode link present")
            .clone();
        a.cursor = link.range.start;
        a.apply(Action::FollowLink);
        assert_eq!(
            a.popup_help().unwrap().title,
            "describe-mode text-mode",
            "follow-link should have opened describe-mode",
        );
        a.apply(Action::JumpHistoryBack);
        let h = a
            .popup_help()
            .expect("popup should still be open after <C-o>");
        assert_eq!(
            h.title, "describe-buffer",
            "<C-o> should restore the originating describe-buffer popup",
        );
        assert_eq!(
            a.active_buffer,
            BufferKind::Help,
            "user should remain within the popup, not bail to the document",
        );
    }

    #[test]
    fn apropos_lists_matching_commands() {
        let mut a = app_with("xx", 10);
        a.command_line = "apropos write".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("help view should open");
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
        let h = a.popup_help().unwrap();
        let body = h.content.as_string();
        assert!(body.contains("no matches"));
    }

    #[test]
    fn help_with_no_arg_opens_index() {
        let mut a = app_with("xx", 10);
        a.command_line = "help".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("help open");
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
        let h = a.popup_help().expect("help open");
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
        assert!(a.popup_buffer.is_none());
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
        let _ = a.popup_help().expect("describe-command open");
        let links = a.popup_help_links().expect("help links seeded");
        assert!(
            links
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
        let _ = a.popup_help().expect("describe open");
        let link = a
            .popup_help_links()
            .expect("help links seeded")
            .iter()
            .find(|l| matches!(&l.target, crate::help::HelpLinkTarget::Topic(_)))
            .expect("topic link present")
            .clone();
        let target_pos = link.range.start;
        a.cursor = target_pos;
        a.apply(Action::FollowLink);
        let h = a.popup_help().expect("help reopen");
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
        let _ = a.popup_help().expect("languages help open");
        // Find the anchor link to "#1-tree-sitter-core" (which the
        // languages topic ships in its quick-reference table).
        let link = a
            .popup_help_links()
            .expect("help links seeded")
            .iter()
            .find(|l| {
                matches!(
                    &l.target,
                    crate::help::HelpLinkTarget::Anchor(s) if s == "1-tree-sitter-core"
                )
            })
            .expect("anchor link to #1-tree-sitter-core present")
            .clone();
        let target_anchor_line = a
            .popup_help_anchors()
            .expect("help anchors seeded")
            .iter()
            .find(|a| a.name == "1-tree-sitter-core")
            .expect("anchor generated for `## 1. Tree-sitter, core`")
            .line;
        // Position the cursor on the link, then follow.
        // After unification, the active cursor lives on `app.cursor`
        // (regardless of buffer kind); we set it there.
        a.cursor = link.range.start;
        a.apply(Action::FollowLink);
        let h = a.popup_help().expect("help still open");
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
            HelpContent::from_lines("test", vec!["a".into(), "b".into()]),
        );
        a.apply(Action::HelpDismiss);
        assert!(a.popup_buffer.is_none());
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
        install_help(&mut a, HelpContent::from_lines("scroll-test", lines));
        let line_down = a.builtins.line_down;
        for _ in 0..3 {
            a.apply(Action::Invoke(CommandInvocation::of(line_down.0)));
        }
        // After unification, `self.cursor` / `self.scroll` are
        // the active buffer's. The popup_buffer's cursor field is
        // archival save-state synced at activation transitions.
        assert_eq!(a.cursor.line, 3);
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn help_motion_clamps_to_last_line() {
        let mut a = app_with("xx", 10);
        let lines: Vec<String> = (0..50).map(|i| format!("line-{i}")).collect();
        install_help(&mut a, HelpContent::from_lines("scroll-test", lines));
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
    fn help_popup_inner_height_caps_for_centered_placement() {
        // M.7.3 follow-up: centered popups (the default; reading
        // surfaces) get a larger cap than the old 20-row tooltip
        // bound. 50-line help in a 60-row buffer: max_h =
        // min(60*3/4, 40) = 40; popup height = 40; inner = 38.
        // Motion uses this as the viewport so ensure_cursor_visible
        // scrolls the popup -- not the full pane -- when the
        // cursor reaches the bottom row.
        let mut a = app_with("xx", 60);
        let lines: Vec<String> = (0..50).map(|i| format!("line-{i}")).collect();
        install_help(&mut a, HelpContent::from_lines("size", lines));
        assert_eq!(a.help_popup_inner_height(60), Some(38));
        // Confirm `active_pane_content_height` routes through the
        // popup-inner branch in State B, so the runtime feeds 38
        // into `set_viewport_height` (not the full 60-row pane).
        assert_eq!(a.active_pane_content_height(60), 38);
    }

    #[test]
    fn help_popup_inner_height_caps_at_twenty_for_cursor_anchored() {
        // Cursor-anchored popups (hover, signature help) keep the
        // tight tooltip caps: 20-row outer / 18-row inner. Same
        // 50-line content as the centered test, different
        // placement → different inner height.
        let mut a = app_with("xx", 60);
        let lines: Vec<String> = (0..50).map(|i| format!("line-{i}")).collect();
        install_help(&mut a, HelpContent::from_lines("size", lines));
        a.popup_placement = crate::popup::PopupPlacement::CursorAnchored;
        assert_eq!(a.help_popup_inner_height(60), Some(18));
    }

    #[test]
    fn help_popup_inner_height_fits_short_content() {
        // 4-line help: popup auto-fits to height 6 (4 + 2 borders),
        // inner = 4. Cursor can never go off-popup-viewport
        // because the popup shows every line of the help buffer.
        let mut a = app_with("xx", 60);
        install_help(&mut a, HelpContent::from_lines("tiny", vec!["a".into(); 4]));
        assert_eq!(a.help_popup_inner_height(60), Some(4));
    }

    #[test]
    fn help_popup_inner_height_none_when_pane_holds_help() {
        // In-pane help (e.g. `:lsp-log`) -- pane.buffer is Help, so
        // the help fills the pane and the regular pane-content-
        // height path applies. No overlay sizing.
        let mut a = app_with("xx", 60);
        let id = a.open_help_in_pane(HelpContent::from_lines("log", vec!["a".into(); 8]));
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
        install_help(&mut a, HelpContent::from_lines("scroll", lines));
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
            HelpContent::from_lines("scroll-test", vec!["a".into(); 30]),
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
            HelpContent::from_lines("hl-test", vec!["hello world".into()]),
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
        install_help(&mut a, HelpContent::from_lines("jt", vec!["x".into(); 30]));
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
        install_help(&mut a, HelpContent::from_lines("count", lines));
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
        install_help(&mut a, HelpContent::from_lines("ro", vec!["abc".into(); 5]));
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
        install_help(&mut a, HelpContent::from_lines("ro", vec!["abc".into()]));
        a.apply(Action::Insert("PWNED".into()));
        assert_eq!(a.document.text(), original);
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("read-only"), "got: {msg:?}");
    }

    #[test]
    fn help_buffer_active_modes_are_markdown_major_help_minor() {
        // M.4 (Option B): a help buffer is `markdown-mode` major +
        // `help-mode` minor. Markdown carries the syntax + motion
        // chassis; help-mode adds ReadOnly + link/anchor follow +
        // help-only commands. Decouples help-as-content from
        // popup-as-display.
        let mut a = app_with("hi", 5);
        let help = crate::help::HelpContent::from_lines("test", vec!["line one".to_string()]);
        let help_id = a.open_help_in_pane(help);
        let active = a
            .active_modes
            .get(&help_id)
            .expect("active_modes populated for help");
        assert_eq!(
            active.major(),
            Some(lattice_syntax::MarkdownMode::mode_id())
        );
        assert!(
            active.minors().contains(&crate::modes::HelpMode::mode_id()),
            "help-mode should be active as a minor; got {:?}",
            active.minors()
        );
    }

    #[test]
    fn help_locals_carry_owner_metadata_for_describe_buffer() {
        let mut a = app_with("hi", 5);
        let help = crate::help::HelpContent::from_lines("t", vec!["body".into()]);
        let help_id = a.open_help_in_pane(help);
        let locals = a.buffer_locals.get(&help_id).unwrap();
        // Help-mode-owned locals (links / anchors / highlights)
        // all live under the help-mode namespace. Other locals
        // (e.g. `ActiveCompletionSources` owned by
        // completion-mode -- CSM.3) may coexist on the same
        // buffer; the help-mode subset must still be namespaced
        // correctly.
        let help_descriptors: Vec<_> = locals
            .iter_descriptors()
            .filter(|d| d.owner_mode == "help-mode")
            .collect();
        assert!(
            !help_descriptors.is_empty(),
            "help-mode-owned locals should be seeded",
        );
        for d in &help_descriptors {
            assert!(
                d.name.starts_with("help-mode."),
                "help-mode local name {:?} should be namespaced under help-mode",
                d.name,
            );
        }
    }

    // ---- M.8: :list-modes / :describe-mode ----

    #[test]
    fn list_modes_groups_by_kind_and_marks_active() {
        // M.8: `:list-modes` renders every registered mode under
        // `## majors` / `## minors` headers; the current major
        // gets a `*` marker.
        let mut a = app_with("hi", 5);
        // Activate `lsp-mode` so the active marker appears on a
        // minor too.
        a.toggle_mode_by_name("lsp-mode");
        a.command_line = "list-modes".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("list-modes opens help");
        let body = h.content.as_string();
        assert!(body.contains("## majors"), "missing majors section\n{body}");
        assert!(body.contains("## minors"), "missing minors section\n{body}");
        // Active markers: text-mode is the default major; lsp-mode
        // was just activated. (The `[name](mode:name)` markdown
        // links get cleaned to bare `name` in the rendered body
        // by `extract_links_and_clean` -- the link metadata
        // travels separately on `HelpMetadata.links`.)
        assert!(
            body.lines().any(|l| l.contains("- * text-mode")),
            "text-mode should be marked active\n{body}",
        );
        assert!(
            body.lines().any(|l| l.contains("- * lsp-mode")),
            "lsp-mode should be marked active\n{body}",
        );
        // Inactive entries get a space marker, not `*`.
        assert!(
            body.lines().any(|l| l.contains("-   help-mode")),
            "help-mode should appear without active marker\n{body}",
        );
    }

    #[test]
    fn describe_mode_renders_metadata() {
        // M.8: `:describe-mode line-numbers-mode` shows kind +
        // contributed options + capabilities.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-mode line-numbers-mode".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("describe-mode help");
        let body = h.content.as_string();
        assert!(body.contains("# mode :: line-numbers-mode"));
        assert!(body.contains("- kind: `minor`"));
        assert!(
            body.contains("contributed options"),
            "missing contributed-options section\n{body}",
        );
        assert!(
            body.contains("`number`"),
            "should reference the contributed option name\n{body}",
        );
    }

    #[test]
    fn describe_mode_unknown_emits_error_no_overlay() {
        let mut a = app_with("xx", 10);
        a.command_line = "describe-mode definitely-not-a-mode".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.popup_buffer.is_none());
        let msg = a.last_message.as_ref().expect("error echo");
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("no mode named"));
    }

    #[test]
    fn describe_option_resolution_shows_minor_contribution() {
        // M.8: with `:line-numbers-mode` active, the resolution
        // view marks that minor as a contributor for `number`.
        let mut a = app_with("xx", 10);
        a.toggle_mode_by_name("line-numbers-mode");
        a.command_line = "describe-option-resolution number".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.popup_help().unwrap().content.as_string();
        assert!(body.contains("# option resolution :: number"));
        assert!(body.contains("minors"));
        assert!(
            body.contains("`line-numbers-mode` ⭐"),
            "should mark line-numbers-mode as a contributor\n{body}",
        );
    }

    #[test]
    fn describe_option_resolution_no_modes_contributing() {
        // No display mode active ⇒ minor section says none
        // contribute.
        let mut a = app_with("xx", 10);
        a.command_line = "describe-option-resolution wrap".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.popup_help().unwrap().content.as_string();
        assert!(body.contains("# option resolution :: wrap"));
        // wrap-mode isn't auto-active by default.
        assert!(
            body.contains("none contribute this option") || body.contains("(none active)"),
            "expected no-contribution message\n{body}",
        );
    }

    // ---- M.9.0: :customize ----

    #[test]
    fn customize_no_args_renders_picker_with_groups_and_modes() {
        let mut a = app_with("xx", 10);
        a.command_line = "customize".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("customize picker");
        let body = h.content.as_string();
        assert!(body.contains("# Customize"));
        assert!(body.contains("## groups"));
        assert!(body.contains("## modes"));
        // Built-in groups appear in the picker.
        assert!(body.contains("editor"), "missing editor group\n{body}");
        assert!(body.contains("display"), "missing display group\n{body}");
        // Customize-able modes appear (display modes from M.7
        // contribute typed options).
        assert!(
            body.contains("line-numbers-mode"),
            "missing line-numbers-mode in picker\n{body}",
        );
        assert!(
            body.contains("wrap-mode"),
            "missing wrap-mode in picker\n{body}",
        );
    }

    #[test]
    fn customize_group_renders_options_with_metadata() {
        let mut a = app_with("xx", 10);
        a.command_line = "customize editor".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("customize editor");
        let body = h.content.as_string();
        assert!(body.contains("# customize :: editor"));
        // Editor group has tabstop, number, wrap, etc.
        assert!(body.contains("tabstop"), "missing tabstop\n{body}");
        assert!(body.contains("number"), "missing number\n{body}");
        // Doc lines indented under each row.
        assert!(
            body.contains("Number of spaces a hard tab"),
            "tabstop doc not rendered\n{body}",
        );
    }

    #[test]
    fn customize_mode_shows_contributed_options() {
        // line-numbers-mode contributes Number=true. The
        // mode view should show that option.
        let mut a = app_with("xx", 10);
        a.command_line = "customize line-numbers-mode".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("customize line-numbers-mode");
        let body = h.content.as_string();
        assert!(body.contains("# customize :: line-numbers-mode"));
        assert!(body.contains("Mode kind:"));
        assert!(
            body.contains("**number**"),
            "should list contributed Number option\n{body}",
        );
    }

    #[test]
    fn customize_mode_emits_shadow_indicator_when_active() {
        // M.9.0 contract: when the mode is active on the
        // current buffer, the form surfaces a [mode-shadow]
        // indicator -- the user understands that a `:set`
        // write would be overridden.
        let mut a = app_with("xx", 10);
        a.toggle_mode_by_name("line-numbers-mode");
        a.command_line = "customize line-numbers-mode".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.popup_help().unwrap().content.as_string();
        assert!(
            body.contains("[mode-shadow]"),
            "expected mode-shadow indicator\n{body}",
        );
        assert!(body.contains("Active"), "should report active state");
    }

    #[test]
    fn customize_picker_link_follows_to_focused_view() {
        // M.9.1: <CR> on a `[name](customize:name)` link in
        // the picker re-fires `:customize <name>` and renders
        // the focused view for the group / mode. Read the
        // links from `buffer_locals` (post-M.3.2.c.5 readers
        // go through there exclusively) keyed on the popup's
        // construction id.
        let mut a = app_with("xx", 10);
        a.command_line = "customize".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let popup_id = a.popup_buffer.expect("picker open");
        let editor_link = a
            .buffer_locals
            .get(&popup_id)
            .and_then(|locals| locals.get::<crate::modes::HelpLinks>())
            .and_then(|hl| {
                hl.0.iter()
                    .find(|l| {
                        matches!(
                            &l.target,
                            lattice_help::HelpLinkTarget::Customize(s) if s == "editor"
                        )
                    })
                    .cloned()
            });
        let link = editor_link.expect("no editor link in picker");
        // Move cursor to the link's start position, then
        // dispatch follow-link.
        a.cursor.line = link.range.start.line;
        a.cursor.byte = link.range.start.byte;
        a.do_help_follow_link();
        // After the follow, the popup buffer holds the focused
        // group view.
        let h2 = a.popup_help().expect("focused view open");
        let body = h2.content.as_string();
        assert!(
            body.contains("# customize :: editor"),
            "expected focused editor view, got: {body}",
        );
    }

    #[test]
    fn customize_edit_link_prefills_cmdline_with_set_name_value() {
        // M.9.2: <CR> on an option row in the customize buffer
        // prefills the cmdline with `:set NAME=current_value`
        // and switches to Command mode for inline editing.
        let mut a = app_with("xx", 10);
        a.command_line = "customize editor".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let popup_id = a.popup_buffer.expect("customize editor open");
        // Find the customize-edit link for `tabstop`.
        let edit_link = a
            .buffer_locals
            .get(&popup_id)
            .and_then(|locals| locals.get::<crate::modes::HelpLinks>())
            .and_then(|hl| {
                hl.0.iter()
                    .find(|l| {
                        matches!(
                            &l.target,
                            lattice_help::HelpLinkTarget::CustomizeEdit(s) if s == "tabstop"
                        )
                    })
                    .cloned()
            });
        let link = edit_link.expect("no customize-edit link for tabstop");
        // Move cursor to the link, follow.
        a.cursor.line = link.range.start.line;
        a.cursor.byte = link.range.start.byte;
        a.do_help_follow_link();
        // Cmdline should be prefilled with `set tabstop=8`
        // (default value).
        assert_eq!(a.command_line, "set tabstop=8");
        assert_eq!(a.modal, ModalState::Command);
    }

    #[test]
    fn customize_edit_then_set_submit_writes_through_normal_pipeline() {
        // M.9.2 round-trip: edit link prefills cmdline; user
        // overwrites the value and submits; `:set` machinery
        // applies the write through the normal cascade.
        let mut a = app_with("xx", 10);
        a.command_line = "customize editor".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let popup_id = a.popup_buffer.expect("editor view open");
        let link = a
            .buffer_locals
            .get(&popup_id)
            .and_then(|locals| locals.get::<crate::modes::HelpLinks>())
            .and_then(|hl| {
                hl.0.iter()
                    .find(|l| {
                        matches!(
                            &l.target,
                            lattice_help::HelpLinkTarget::CustomizeEdit(s) if s == "tabstop"
                        )
                    })
                    .cloned()
            })
            .expect("no edit link");
        a.cursor.line = link.range.start.line;
        a.cursor.byte = link.range.start.byte;
        a.do_help_follow_link();
        // User edits the value: `set tabstop=4`.
        a.command_line = "set tabstop=4".into();
        a.apply(Action::CommandLineSubmit);
        // Drain the OptionChanged event so the cache rebuilds.
        a.drain_option_changes();
        // `:set` write took effect.
        assert_eq!(a.tabstop(), 4);
    }

    // ---- :tutor ----

    #[test]
    fn tutor_writes_lesson_one_to_temp_and_opens_it() {
        // `:tutor` (no arg) defaults to lesson 1; copies the
        // embedded markdown to a temp file and opens it.
        let mut a = app_with("xx", 10);
        a.do_tutor(None);
        // The active document path now points at the temp file.
        let path = std::env::temp_dir().join("lattice-tutor-lesson-1.txt");
        assert!(
            path.exists(),
            "tutor should have written lesson file at {path:?}",
        );
        // The file's content should match the embedded lesson.
        let written = std::fs::read_to_string(&path).expect("read lesson file");
        assert!(written.contains("Welcome to the Lattice Tutor"));
        assert!(written.contains("Lesson 1.1"));
        // Cleanup.
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tutor_unknown_lesson_echoes_error() {
        let mut a = app_with("xx", 10);
        a.do_tutor(Some(99));
        let msg = a.last_message.as_ref().expect("error echo");
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("lesson 99 doesn't exist yet"));
    }

    #[test]
    fn customize_unknown_group_emits_error_no_overlay() {
        let mut a = app_with("xx", 10);
        a.command_line = "customize definitely-not-a-group".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.popup_buffer.is_none());
        let msg = a.last_message.as_ref().expect("error echo");
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("no group named"));
    }

    #[test]
    fn customize_unknown_mode_emits_error_no_overlay() {
        let mut a = app_with("xx", 10);
        a.command_line = "customize definitely-not-a-mode".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.popup_buffer.is_none());
        let msg = a.last_message.as_ref().expect("error echo");
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("no mode named"));
    }

    #[test]
    fn describe_option_resolution_unknown_emits_error() {
        let mut a = app_with("xx", 10);
        a.command_line = "describe-option-resolution definitely-not-an-option".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let msg = a.last_message.as_ref().expect("error echo");
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("E518"));
    }

    #[test]
    fn describe_mode_shows_active_state_for_current_buffer() {
        let mut a = app_with("xx", 10);
        a.toggle_mode_by_name("lsp-mode");
        a.command_line = "describe-mode lsp-mode".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.popup_help().unwrap().content.as_string();
        assert!(
            body.contains("active on current buffer: yes"),
            "expected active=yes\n{body}",
        );
    }
}
