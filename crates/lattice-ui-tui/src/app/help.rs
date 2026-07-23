//! Help-buffer App surface -- the `:describe-*` / `:apropos`
//! / `:help` / `:keymap` writers that compose help bodies.
//! Each method renders a help-buffer body via shared helpers
//! (`HelpContent::from_lines{,_and_anchors}`) and hands it to
//! `open_help`; syntax / link styling rides the live cells-worker
//! `DisplayMatrix`.
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

use super::App;
// Phase 5.8.AD.5: `HelpContent` no longer used at module
// scope — every content builder + display path is host-side.

impl App {
    /// `:help [topic]` (DESIGN.md §5.11). With no topic the index
    /// is rendered (the topic registered as `index`); with a
    /// topic name the registry is queried and the topic body is
    /// rendered into a help buffer through the same markdown-
    /// highlighting path `:describe-command` uses. Unknown topic
    /// surfaces as a clear echo error so completion + typo
    /// recovery work.
    pub(super) fn do_open_help_topic(&mut self, topic: Option<&str>) {
        // Slice 3c.final.E.3: clone to owned for the `Send + 'static`
        // closure, then route through `mutate_editor_with`.
        let topic = topic.map(|s| s.to_string());
        let signals = self.mutate_editor_with(move |e| e.do_open_help_topic(topic.as_deref()));
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    // 5.5.F.1: `:describe-buffer` content builder relocated to
    // [`lattice_host::dispatch::Editor::build_describe_buffer_content`]
    // and the `Effect::DescribeBuffer` arm now lives in
    // `Editor::handle_effect`. The renderer-coupled tail flows back
    // through `RendererSignal::DisplayBuffer`.
    //
    // 5.5.F.2: `:describe-command` / `:apropos` / `:describe-key` /
    // `:list-keymap` content builders co-migrated through the same
    // pipe; builders live alongside `build_describe_buffer_content`
    // host-side
    // ([`lattice_host::dispatch::Editor::build_describe_command_content`]
    // et al.) and the corresponding `Effect::*` arms now run inside
    // `Editor::handle_effect`. The thin `do_describe_command` /
    // `do_describe_key` wrappers below remain App-side because
    // help-link follow handlers (`HelpLinkTarget::Command` /
    // `HelpLinkTarget::Chord`) and the cmdline's `<C-h>` invoke them
    // directly without going through an `Effect`; they fan the
    // host-built `HelpContent` through `display_buffer` (the same
    // routing the `RendererSignal::DisplayBuffer` arm uses).

    /// `:describe-command <name>` direct-call wrapper for
    /// renderer-side callers (help-link follow, cmdline `<C-h>`).
    /// Effect-path callers route through `Editor::handle_effect`
    /// + `RendererSignal::DisplayBuffer`.
    pub(super) fn do_describe_command(&mut self, name: &str, anchor: Option<&str>) {
        // Phase 5.8.AD.5: body migrated.
        // Slice 3c.final.E.3.
        let name = name.to_string();
        let anchor = anchor.map(|s| s.to_string());
        let signals =
            self.mutate_editor_with(move |e| e.do_describe_command(&name, anchor.as_deref()));
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    /// `:describe-key <chord>` direct-call. Phase 5.8.AD.5.
    pub(super) fn do_describe_key(&mut self, chord: &str) {
        // Slice 3c.final.E.3.
        let chord = chord.to_string();
        let signals = self.mutate_editor_with(move |e| e.do_describe_key(&chord));
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    /// `K` (LSP hover) response handler / `:hover [markdown]`.
    /// Enters **State A**: popup overlay shown, focus stays on
    /// the main buffer. The popup auto-dismisses on the next
    /// motion (apply()'s post-dispatch hook) since it's anchored
    /// to the symbol the user K'd. To navigate inside the popup
    /// the user presses `K` again, which `do_lsp_hover_request`
    /// recognises as "focus into popup" -> State B.
    pub(super) fn do_open_hover(&mut self, markdown: &str) {
        // Slice 3c.final.E.3.
        let markdown = markdown.to_string();
        let signals = self.mutate_editor_with(move |e| e.do_open_hover(&markdown));
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    // 5.5.LSP.1: `focus_help_popup` (State A -> B promote)
    // relocated to [`lattice_host::dispatch::Editor::focus_help_popup`].
    // The host method is identical (stashes pane state, swaps
    // `active_buffer` to Help) and is invoked by `Editor::
    // lsp_hover_request`. App-side callers (forthcoming LSP.2-5
    // navigation / references / etc.) will call the host version
    // once those request helpers migrate alongside their drains.

    /// `:HoverClose` -- dismiss the hover popup. Routes through
    /// the unified `dismiss_popup` path so State A and State B
    /// both unwind cleanly (B restores via `prev_pane_for_popup`;
    /// A just drops the popup).
    pub(super) fn do_dismiss_popup(&mut self) {
        self.dismiss_popup();
    }

    /// Show a popup overlay for the (host-ensured) buffer named `name` under
    /// major mode `mode_id` — the content-agnostic `Effect::OpenPopup` arm
    /// (popup-api.md §4.3). Delegates to the host primitive
    /// [`lattice_host::dispatch::Editor::open_popup_named`] and drains any
    /// renderer signals, mirroring `do_open_hover`. The GPUI peer reaches the
    /// same primitive via `mutate_editor`.
    pub(super) fn open_popup_named(
        &mut self,
        name: &str,
        mode_id: &str,
        placement: lattice_core::ui::popup::PopupPlacement,
        focus: lattice_core::ui::popup::PopupFocus,
    ) {
        let name = name.to_string();
        let mode_id = mode_id.to_string();
        let signals =
            self.mutate_editor_with(move |e| e.open_popup_named(&name, &mode_id, placement, focus));
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    // 5.5.F.2: `do_list_keymap` relocated -- see pointer block
    // above. Builder lives at
    // [`lattice_host::dispatch::Editor::build_list_keymap_content`].

    // 5.5.F.3: `:describe-events` / `:describe-event` content
    // builders relocated to
    // [`lattice_host::dispatch::Editor::build_describe_events_content`]
    // and [`build_describe_event_content`]; the corresponding
    // `Effect::*` arms now run inside `Editor::handle_effect` and
    // emit `RendererSignal::DisplayBuffer`. The thin wrappers
    // below remain App-side because integration tests in
    // `app/mode.rs` invoke them directly; both share the same
    // host content builder with the Effect-driven path.

    /// `:describe-events` direct-call wrapper for test-mode
    /// callers (asserting renderer-side display routing). Effect-
    /// path callers route through `Editor::handle_effect` +
    /// `RendererSignal::DisplayBuffer`.
    #[allow(dead_code)]
    pub(super) fn do_describe_events(&mut self) {
        // Slice 3c.final.E.3.
        let signals = self.mutate_editor_with(|e| e.do_describe_events());
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    /// `:describe-event <name>` direct-call. Phase 5.8.AD.5.
    #[allow(dead_code)]
    pub(super) fn do_describe_event(&mut self, name: &str) {
        // Slice 3c.final.E.3.
        let name = name.to_string();
        let signals = self.mutate_editor_with(move |e| e.do_describe_event(&name));
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    // 5.5.F.6: `:list-modes` content builder relocated to
    // [`lattice_host::dispatch::Editor::build_list_modes_content`];
    // the `Effect::ListModes` arm now runs inside
    // `Editor::handle_effect` and emits `RendererSignal::DisplayBuffer`.
    // Effect-only path; no wrapper retained App-side.

    /// 5.5.F.6: see [`lattice_host::dispatch::Editor::build_describe_mode_content`].
    /// App-side wrapper retained because the `HelpLinkTarget::Mode`
    /// follow-handler (link-click on a `[label](mode:NAME)` link)
    /// calls it directly outside the Effect-arm dispatch path.
    #[allow(dead_code)]
    pub(super) fn do_describe_mode(&mut self, name: &str) {
        // Slice 3c.final.E.3.
        let name = name.to_string();
        let signals = self.mutate_editor_with(move |e| e.do_describe_mode(&name));
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    // 5.5.F.3: `:describe-option-resolution` content builder
    // relocated to
    // [`lattice_host::dispatch::Editor::build_describe_option_resolution_content`];
    // the `Effect::DescribeOptionResolution` arm now runs inside
    // `Editor::handle_effect` and emits
    // `RendererSignal::DisplayBuffer`. Effect-only path; no
    // wrapper retained App-side.
    /// 5.5.F.6: see the three host content builders
    /// (`build_customize_picker_content`, `build_customize_mode_content`,
    /// `build_customize_group_content`). App-side wrapper retained
    /// because the `HelpLinkTarget::Customize` follow-handler calls
    /// it directly outside the Effect-arm dispatch path.
    #[allow(dead_code)]
    pub(super) fn do_customize(&mut self, name: Option<&str>) {
        // Slice 3c.final.E.3.
        let name = name.map(|s| s.to_string());
        let signals = self.mutate_editor_with(move |e| e.do_customize(name.as_deref()));
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    // 5.5.F.6: `do_customize_picker` / `do_customize_group` /
    // `do_customize_mode` / `append_customize_row` relocated to
    // [`lattice_host::dispatch::Editor::build_customize_picker_content`]
    // / `..._group_content` / `..._mode_content` /
    // `append_customize_row`. The `Effect::Customize` arm dispatches
    // host-side; App-side `do_customize` wrapper above stays for
    // the `HelpLinkTarget::Customize` follow-handler.

    // 2026-05-27: `do_customize_edit` hoisted into
    // `Editor::do_customize_edit`. The CustomizeEdit follow-handler
    // is now reachable from both renderer peers through
    // `Editor::do_help_follow_link`.

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
        // Slice 3c.final.E.3.
        let signals = self.mutate_editor_with(move |e| e.do_tutor(lesson));
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    // Phase 5.8.AD.5: legacy do_tutor body removed.

    /// Follow the help link under the cursor (`<CR>` in help mode).
    ///
    /// 2026-05-27: hoisted into `Editor::do_help_follow_link` so both
    /// renderer peers route through one host-side dispatcher. This
    /// wrapper is kept so existing tests (and any direct callers)
    /// reach the same flow without going through `apply(Action::FollowLink)`.
    pub(super) fn do_help_follow_link(&mut self) {
        let signals = self.mutate_editor_with(|e| {
            let mut out = lattice_host::dispatch::DispatchOutcome::default();
            e.do_help_follow_link(&mut out);
            out.renderer_signals
        });
        for s in signals {
            self.handle_renderer_signal(s);
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
        a.editor.set_command_line_text("describe-command ex:write");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("help view should open");
        assert!(h.title.contains("ex:write"));
        // First two lines: "ex:write :" + blank.
        let lines = h.lines();
        assert!(lines[0].contains("ex:write"));
        assert!(lines[0].contains(":"));
    }

    /// PU.1b-3: the renderer→host popup-geometry feedback helper
    /// returns the popup's inner rect only while a FLOATING popup is
    /// open. `None` with no popup; `Some` once help is open, with the
    /// centered inner width = `popup_outer_size` width − 2 borders.
    #[test]
    fn popup_feedback_inner_dims_only_for_open_floating_popup() {
        let mut a = app_with("xx", 10);
        assert_eq!(
            crate::render::popup_feedback_inner_dims(&a, 120, 40),
            None,
            "no dims when no popup is open"
        );
        a.editor.set_command_line_text("describe-command ex:write");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.popup().is_open(), "floating popup open");
        let (rows, cols) = crate::render::popup_feedback_inner_dims(&a, 120, 40)
            .expect("dims resolve for an open floating popup");
        assert!(rows >= 1, "inner height positive: {rows}");
        // Centered cap: outer width = (120 - 4).clamp(30, 120) = 116;
        // inner = outer − 2 borders = 114.
        assert_eq!(cols, 114, "centered popup inner width = outer − 2 borders");
    }

    #[test]
    fn describe_command_shows_source_link_to_registration_site() {
        // §5.11: every :describe-* must surface a file link to the
        // registration site. The buffer text is the rendered label
        // (`ex_commands.rs:LINE`) only -- the URL lives on the
        // parsed HelpLink target. Built-in commands record their
        // source via #[track_caller] when populate() runs.
        let mut a = app_with("xx", 10);
        a.editor.set_command_line_text("describe-command ex:write");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("describe-command ex:quit");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("describe-command ex:apropos");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("describe-command ex:quit");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("describe-command ex:apropos");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("describe-command ex:apropos");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("describe-key j");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().unwrap();
        let body = h.content.as_string();
        assert!(
            body.contains("Bound at:"),
            "describe-key output missing `Bound at:`: {body}"
        );
        // K.2.4.A.0.1 (2026-06-02): static catalog moved to
        // `lattice-mode::keymap_entry`; the `file!()` captures
        // now resolve to `keymap_entry.rs` rather than `keymap.rs`.
        assert!(
            body.contains("keymap_entry.rs"),
            "describe-key output missing source label: {body}"
        );
        let links = a.popup_help_links().expect("help links seeded");
        let has_source = links.iter().any(|l| {
            matches!(&l.target, crate::help::HelpLinkTarget::Source { path, .. }
                if path.to_string_lossy().contains("keymap_entry.rs"))
        });
        assert!(has_source, "expected a Source HelpLink to keymap_entry.rs");
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
        a.editor.set_command_line_text("describe-key j");
        a.editor.modal = ModalState::Command;
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
        // `j` has 2 catalog descriptors -- Normal (line down)
        // and Visual (extend down). Help inherits Normal's `j`
        // via active-buffer routing (DESIGN.md §5.9), so it
        // doesn't surface as a separate descriptor. Each
        // catalog descriptor should surface its own
        // `(file:...)` link pointing at the entry's row in
        // `lattice-mode::keymap_entry` (K.2.4.A.0.1 catalog
        // location).
        //
        // K.2.4.A.3 (2026-06-02): the resolved-binding section
        // (K.2.4.A.1) and runtime-registry section (K.1.d)
        // ALSO emit `as_link()` source links now, so the
        // total source-link count grew beyond 2. The contract
        // this test still defends is "each catalog descriptor
        // has a distinct keymap_entry.rs source link" — so
        // filter to keymap_entry.rs links and dedup the line
        // numbers.
        let mut a = app_with("xx", 10);
        a.editor.set_command_line_text("describe-key j");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let _ = a.popup_help().unwrap();
        let links = a.popup_help_links().expect("help links seeded");
        let catalog_lines: Vec<u32> = links
            .iter()
            .filter_map(|l| match &l.target {
                crate::help::HelpLinkTarget::Source { path, line }
                    if path.to_string_lossy().contains("keymap_entry.rs") =>
                {
                    Some(*line)
                }
                _ => None,
            })
            .collect();
        let mut deduped = catalog_lines.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(
            deduped.len(),
            2,
            "expected 2 distinct catalog source lines in keymap_entry.rs; got {deduped:?} \
             (all source links: {links:?})",
        );
    }

    #[test]
    fn describe_key_unknown_chord_renders_not_bound_message() {
        let mut a = app_with("xx", 10);
        a.editor.set_command_line_text("describe-key xyzzy");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.popup_help().unwrap().content.as_string();
        assert!(body.contains("not bound"), "body: {body}");
    }

    // ---- K.2.4.A.1: resolved-binding indicator ----

    #[test]
    fn describe_key_shows_resolved_binding_section_for_bound_chord() {
        // T12 (K.1.d resolve_trace rebuild): describe-key leads with
        // a per-mode trace showing what fires RIGHT NOW. For `j`
        // (Builtin Normal + Visual + Replace), the trace surfaces
        // all three mode sections.
        let mut a = app_with("xx", 10);
        a.editor.set_command_line_text("describe-key j");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.popup_help().unwrap().content.as_string();
        assert!(
            body.contains("registration(s) across"),
            "missing registration count header: {body}"
        );
        assert!(
            body.contains("[Normal mode]"),
            "missing [Normal mode] resolution row: {body}"
        );
        assert!(
            body.contains("[Visual mode]"),
            "missing [Visual mode] resolution row: {body}"
        );
    }

    #[test]
    fn describe_key_resolved_binding_renders_canonical_command_name() {
        // T12 (K.1.d): the layer trace renders the registry's
        // canonical command name (`motion:line-down`) rather than
        // `CommandId(N)` debug formatting. There is no longer a
        // separate "Resolved binding" vs "Runtime registry" split —
        // the whole output is the trace.
        let mut a = app_with("xx", 10);
        a.editor.set_command_line_text("describe-key j");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.popup_help().unwrap().content.as_string();
        assert!(
            body.contains("→ motion:line-down"),
            "trace should render canonical command name: {body}"
        );
        assert!(
            !body.contains("→ CommandId("),
            "trace should not leak CommandId debug: {body}"
        );
        assert!(
            body.contains("(fires now)"),
            "winner hit should be marked with (fires now): {body}"
        );
    }

    #[test]
    fn describe_key_resolved_binding_source_is_clickable_link() {
        // K.2.4.A.3: source rows in the resolved-binding
        // section render via SourceLocation::as_link() so the
        // file:line entry is a clickable markdown link. The
        // help framework extracts `[label](file:URL)` into
        // the popup's HelpLink table; the rendered body
        // shows only the label. Assert both ends of the
        // contract: (a) the body has the bare file:line
        // label form (the as_link() output's label half),
        // and (b) the link table carries Source-target
        // entries that the help follow-handler can route on
        // `<CR>`.
        let mut a = app_with("xx", 10);
        a.editor.set_command_line_text("describe-key j");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.popup_help().unwrap().content.as_string();
        // Body shows the label form (path:line) without the
        // surrounding `[...]` markdown markers (they were
        // extracted into the help-link table).
        assert!(
            body.contains("source: crates/lattice-host/src/keymap_normal.rs:"),
            "resolved section should render source as as_link() label: {body}"
        );
        // And NOT the debug shape `SourceLocation { ... }`
        // — K.2.4.A.3 contract.
        assert!(
            !body.contains("SourceLocation {"),
            "no debug-formatted SourceLocation should leak: {body}"
        );
        // The link table should carry Source-target entries
        // for the file:line URLs. At least one points at the
        // host-side keymap_normal.rs the resolution traces
        // back to.
        let links = a.popup_help_links().expect("help links seeded");
        let has_clickable_source = links.iter().any(|l| {
            matches!(&l.target, crate::help::HelpLinkTarget::Source { path, .. }
                if path.to_string_lossy().contains("keymap_normal.rs"))
        });
        assert!(
            has_clickable_source,
            "expected a clickable Source link to keymap_normal.rs from the resolved section: {links:?}"
        );
    }

    #[test]
    fn describe_key_runtime_registry_section_renders_canonical_command_names() {
        // T12 (K.1.d): the "Runtime registry:" separate section was
        // removed; the whole output is now one unified trace (T12's
        // resolve_trace API). Check that the trace renders canonical
        // names and doesn't leak CommandId debug format anywhere.
        let mut a = app_with("xx", 10);
        a.editor.set_command_line_text("describe-key j");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.popup_help().unwrap().content.as_string();
        assert!(
            body.contains("→ motion:line-down"),
            "trace should contain canonical command name: {body}"
        );
        assert!(
            !body.contains("→ CommandId("),
            "trace should not leak CommandId debug anywhere: {body}"
        );
    }

    #[test]
    fn describe_key_resolved_binding_uses_friendly_layer_label() {
        // K.2.4.A.2: layer label in the resolved section
        // renders as `Built-in` (friendly) rather than
        // `Builtin` (debug). The runtime-registry section
        // (K.1.d, retiring in K.2.4.A.4) also uses friendly
        // labels, so this assertion holds against the whole
        // body.
        let mut a = app_with("xx", 10);
        a.editor.set_command_line_text("describe-key j");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.popup_help().unwrap().content.as_string();
        assert!(
            body.contains("layer: Built-in"),
            "expected friendly `layer: Built-in` label: {body}"
        );
        // Friendly form is `Built-in` (with hyphen); the
        // raw debug form is `Builtin` (no hyphen). Assert the
        // debug form does NOT appear standalone.
        assert!(
            !body.contains("layer: Builtin\n") && !body.contains("layer: Builtin "),
            "debug-format `Builtin` should not appear: {body}"
        );
    }

    #[test]
    fn describe_key_resolved_binding_falls_back_to_unbound_message() {
        // Chord parses fine but doesn't bind in any mode.
        // T12: the fallback is "X is not bound in any mode."
        let mut a = app_with("xx", 10);
        a.editor.set_command_line_text("describe-key <C-S-q>");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.popup_help().unwrap().content.as_string();
        assert!(
            body.contains("is not bound in any mode."),
            "missing unbound fallback message: {body}"
        );
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
            a.editor
                .last_message
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
        assert!(a.editor.popup_buffer.is_none());
        let m = a.editor.last_message.as_ref().unwrap();
        assert_eq!(m.level, EchoLevel::Error);
    }

    #[test]
    fn describe_command_with_no_args_omits_arguments_section() {
        // ex:quit has args_schema: vec![] -- no Arguments section.
        let mut a = app_with("xx", 10);
        a.editor.set_command_line_text("describe-command ex:quit");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("describe-command ex:nope");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.editor.popup_buffer.is_none());
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn describe_buffer_renders_state_summary() {
        let mut a = app_with("hello\nworld", 10);
        a.editor.set_command_line_text("describe-buffer");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("describe-buffer");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("describe-buffer");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let initial_id = a.editor.popup_buffer.expect("describe-buffer should open");
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
        a.editor.cursor = link.range.start;
        a.apply(Action::FollowLink);
        let h = a.popup_help().expect("popup should still be open");
        assert_eq!(h.title, "describe-mode text-mode");
        assert!(h.content.as_string().contains("text-mode"));
        assert_eq!(
            a.editor.popup_buffer,
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
        a.editor.set_command_line_text("describe-buffer");
        a.editor.modal = ModalState::Command;
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
        a.editor.cursor = link.range.start;
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
            a.editor.active_buffer,
            BufferKind::Help,
            "user should remain within the popup, not bail to the document",
        );
    }

    #[test]
    fn apropos_lists_matching_commands() {
        let mut a = app_with("xx", 10);
        a.editor.set_command_line_text("apropos write");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("apropos zxqzxqzxq");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().unwrap();
        let body = h.content.as_string();
        assert!(body.contains("no matches"));
    }

    #[test]
    fn help_with_no_arg_opens_index() {
        let mut a = app_with("xx", 10);
        a.editor.set_command_line_text("help");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("help folding");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("help nonexistent");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.editor.popup_buffer.is_none());
        let msg = a.editor.last_message.as_ref().expect("error");
        assert!(msg.text.contains("no help topic"), "got: {}", msg.text);
    }

    #[test]
    fn describe_buffer_command_emits_topic_cross_link() {
        // `:buffers` (registered as `ex:buffers`) matches the
        // buffers topic's `buffer` pattern, so the describe view
        // should append a `[buffers](help:buffers)` cross-link.
        let mut a = app_with("xx", 10);
        a.editor.set_command_line_text("describe-command ex:buffers");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("describe-command ex:buffers");
        a.editor.modal = ModalState::Command;
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
        a.editor.cursor = target_pos;
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
        a.editor.set_command_line_text("help languages");
        a.editor.modal = ModalState::Command;
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
        // After unification, the active cursor lives on `app.editor.cursor`
        // (regardless of buffer kind); we set it there.
        a.editor.cursor = link.range.start;
        a.apply(Action::FollowLink);
        let h = a.popup_help().expect("help still open");
        assert_eq!(
            h.title, "help languages",
            "follow-link must NOT swap topics for an anchor jump"
        );
        assert_eq!(
            a.editor.cursor.line, target_anchor_line,
            "cursor should land on the heading line"
        );
        assert_eq!(
            a.editor.scroll, target_anchor_line,
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
        assert!(a.editor.popup_buffer.is_none());
        assert_eq!(a.editor.active_buffer, BufferKind::Document);
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
        let line_down = a.editor.builtins.line_down;
        for _ in 0..3 {
            a.apply(Action::Invoke(CommandInvocation::of(line_down.0)));
        }
        // After unification, `self.editor.cursor` / `self.editor.scroll` are
        // the active buffer's. The popup_buffer's cursor field is
        // archival save-state synced at activation transitions.
        assert_eq!(a.editor.cursor.line, 3);
        assert_eq!(a.editor.scroll, 0);
    }

    #[test]
    fn help_motion_clamps_to_last_line() {
        let mut a = app_with("xx", 10);
        let lines: Vec<String> = (0..50).map(|i| format!("line-{i}")).collect();
        install_help(&mut a, HelpContent::from_lines("scroll-test", lines));
        let line_down = a.editor.builtins.line_down;
        for _ in 0..1000 {
            a.apply(Action::Invoke(CommandInvocation::of(line_down.0)));
        }
        assert_eq!(a.editor.cursor.line, 49);
        // Scroll keeps cursor on screen: viewport 10, cursor 49,
        // so scroll = 49 + 1 - 10 = 40. Production runtime sets
        // viewport per-frame via active_pane_content_height (which
        // shrinks for help popups); the test fixture sets a fixed
        // viewport of 10 and the assertion follows from that.
        assert_eq!(a.editor.scroll, 40);
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
        a.editor.popup_placement = crate::popup::PopupPlacement::CursorAnchored;
        // Slice 3c.final.E.5i: `help_popup_inner_height` reads
        // `popup_placement` through `popup()` (RS-backed mirror),
        // so direct field mutation needs an explicit publish.
        a.editor.publish_render_state();
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
        assert_eq!(a.editor.pane_tree.active().buffer_id, id);
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
        let line_down = a.editor.builtins.line_down;
        let line_up = a.editor.builtins.line_up;
        // `G` to the last line first so we're at the clamp.
        let goto_last = a.editor.builtins.goto_last_line;
        a.apply(Action::Invoke(CommandInvocation::of(goto_last.0)));
        assert_eq!(a.editor.cursor.line, 49);
        // Press j five times past the last line. cursor.line must
        // stay pinned at 49 -- no phantom overshoot.
        for _ in 0..5 {
            a.apply(Action::Invoke(CommandInvocation::of(line_down.0)));
        }
        assert_eq!(a.editor.cursor.line, 49);
        // First k must move up immediately, not "unwind" any
        // overshoot.
        a.apply(Action::Invoke(CommandInvocation::of(line_up.0)));
        assert_eq!(a.editor.cursor.line, 48);
    }

    #[test]
    fn help_motion_up_clamps_at_zero() {
        let mut a = app_with("xx", 10);
        install_help(
            &mut a,
            HelpContent::from_lines("scroll-test", vec!["a".into(); 30]),
        );
        let line_up = a.editor.builtins.line_up;
        for _ in 0..1000 {
            a.apply(Action::Invoke(CommandInvocation::of(line_up.0)));
        }
        assert_eq!(a.editor.cursor.line, 0);
        assert_eq!(a.editor.scroll, 0);
    }

    #[test]
    fn help_horizontal_motion_runs_through_grammar() {
        let mut a = app_with("xx", 10);
        install_help(
            &mut a,
            HelpContent::from_lines("hl-test", vec!["hello world".into()]),
        );
        let char_right = a.editor.builtins.char_right;
        let char_left = a.editor.builtins.char_left;
        let line_end = a.editor.builtins.line_end;
        let line_start = a.editor.builtins.line_start;
        for _ in 0..3 {
            a.apply(Action::Invoke(CommandInvocation::of(char_right.0)));
        }
        assert_eq!(a.editor.cursor.byte, 3);
        a.apply(Action::Invoke(CommandInvocation::of(char_left.0)));
        assert_eq!(a.editor.cursor.byte, 2);
        a.apply(Action::Invoke(CommandInvocation::of(line_end.0)));
        // `motion:line-end` lands at `byte == line_len` (one past
        // the last byte) -- the same convention as the document
        // path. The grammar uses this position so operator targets
        // (d$, c$, y$) take an exclusive end.
        assert_eq!(a.editor.cursor.byte, 11);
        a.apply(Action::Invoke(CommandInvocation::of(line_start.0)));
        assert_eq!(a.editor.cursor.byte, 0);
    }

    #[test]
    fn help_gg_and_capital_g_route_through_grammar() {
        let mut a = app_with("xx", 10);
        install_help(&mut a, HelpContent::from_lines("jt", vec!["x".into(); 30]));
        let goto_first = a.editor.builtins.goto_first_line;
        let goto_last = a.editor.builtins.goto_last_line;
        a.apply(Action::Invoke(CommandInvocation::of(goto_last.0)));
        assert_eq!(a.editor.cursor.line, 29);
        assert!(a.editor.scroll > 0);
        a.apply(Action::Invoke(CommandInvocation::of(goto_first.0)));
        assert_eq!(a.editor.cursor.line, 0);
        assert_eq!(a.editor.scroll, 0);
    }

    #[test]
    fn help_count_motions_compose() {
        // `5j` -- the same count semantics as Normal mode.
        let mut a = app_with("xx", 10);
        let lines: Vec<String> = (0..50).map(|i| format!("l{i}")).collect();
        install_help(&mut a, HelpContent::from_lines("count", lines));
        let line_down = a.editor.builtins.line_down;
        a.apply(Action::Invoke(
            CommandInvocation::of(line_down.0).with_count(lattice_grammar::command::Count(5)),
        ));
        assert_eq!(a.editor.cursor.line, 5);
    }

    #[test]
    fn help_invoke_operator_echoes_read_only() {
        // Operators on a help buffer are rejected with a "read-only"
        // echo -- v1 doesn't model yank-against-help yet.
        let mut a = app_with("xx", 10);
        install_help(&mut a, HelpContent::from_lines("ro", vec!["abc".into(); 5]));
        let yank = a.editor.builtins.yank;
        a.apply(Action::Invoke(
            CommandInvocation::of(yank.0).with_range(lattice_grammar::Range::CurrentLine),
        ));
        let msg = a.editor.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("read-only"), "got: {msg:?}");
        assert!(a.editor.unnamed_register.is_none());
    }

    #[test]
    fn help_action_insert_blocked_with_echo() {
        // The read-only guard short-circuits direct mutation
        // actions so a stray Action::Insert while help is active
        // doesn't fall through onto the document.
        let mut a = app_with("xx", 10);
        let original = a.editor.document.text();
        install_help(&mut a, HelpContent::from_lines("ro", vec!["abc".into()]));
        a.apply(Action::Insert("PWNED".into()));
        assert_eq!(a.editor.document.text(), original);
        let msg = a.editor.last_message.as_ref().expect("echo");
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
            .editor
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
        let locals = a.editor.buffer_locals.get(&help_id).unwrap();
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
        a.editor.set_command_line_text("list-modes");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("describe-mode line-numbers-mode");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("describe-mode help");
        let body = h.content.as_string();
        assert!(body.contains("# mode :: line-numbers-mode"));
        assert!(body.contains("- kind: ◇"));
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
        a.editor.set_command_line_text("describe-mode definitely-not-a-mode");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.editor.popup_buffer.is_none());
        let msg = a.editor.last_message.as_ref().expect("error echo");
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("no mode named"));
    }

    #[test]
    fn describe_option_resolution_shows_minor_contribution() {
        // M.8: with `:line-numbers-mode` active, the resolution
        // view marks that minor as a contributor for `number`.
        let mut a = app_with("xx", 10);
        a.toggle_mode_by_name("line-numbers-mode");
        a.editor.set_command_line_text("describe-option-resolution number");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("describe-option-resolution wrap");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("customize");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("customize editor");
        a.editor.modal = ModalState::Command;
        a.editor.publish_render_state();
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("customize editor");
        let body = h.content.as_string();
        assert!(body.contains("# customize :: editor"));
        // Editor group has tabstop, number, wrap, etc.
        assert!(body.contains("tabstop"), "missing tabstop\n{body}");
        assert!(body.contains("number"), "missing number\n{body}");
        // Doc lines indented under each row.
        assert!(
            body.contains("Number of columns a hard tab"),
            "tabstop doc not rendered\n{body}",
        );
    }

    #[test]
    fn customize_mode_shows_contributed_options() {
        // line-numbers-mode contributes Number=true. The
        // mode view should show that option.
        let mut a = app_with("xx", 10);
        a.editor.set_command_line_text("customize line-numbers-mode");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("customize line-numbers-mode");
        a.editor.modal = ModalState::Command;
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
        a.editor.set_command_line_text("customize");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let popup_id = a.editor.popup_buffer.expect("picker open");
        let editor_link = a
            .editor
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
        a.editor.cursor.line = link.range.start.line;
        a.editor.cursor.byte = link.range.start.byte;
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
        a.editor.set_command_line_text("customize editor");
        a.editor.modal = ModalState::Command;
        a.editor.publish_render_state();
        a.apply(Action::CommandLineSubmit);
        let popup_id = a.editor.popup_buffer.expect("customize editor open");
        // Find the customize-edit link for `tabstop`.
        let edit_link = a
            .editor
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
        a.editor.cursor.line = link.range.start.line;
        a.editor.cursor.byte = link.range.start.byte;
        a.do_help_follow_link();
        // Cmdline should be prefilled with `set tabstop=4`
        // (default value).
        assert_eq!(a.editor.command_line(), "set tabstop=4");
        assert_eq!(a.editor.modal, ModalState::Command);
    }

    #[test]
    fn customize_edit_then_set_submit_writes_through_normal_pipeline() {
        // M.9.2 round-trip: edit link prefills cmdline; user
        // overwrites the value and submits; `:set` machinery
        // applies the write through the normal cascade.
        let mut a = app_with("xx", 10);
        a.editor.set_command_line_text("customize editor");
        a.editor.modal = ModalState::Command;
        a.editor.publish_render_state();
        a.apply(Action::CommandLineSubmit);
        let popup_id = a.editor.popup_buffer.expect("editor view open");
        let link = a
            .editor
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
        a.editor.cursor.line = link.range.start.line;
        a.editor.cursor.byte = link.range.start.byte;
        a.do_help_follow_link();
        // User edits the value: `set tabstop=4`.
        a.editor.set_command_line_text("set tabstop=4");
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
        // Lessons are markdown (`do_tutor` writes `…-lesson-N.md`).
        let path = std::env::temp_dir().join("lattice-tutor-lesson-1.md");
        assert!(
            path.exists(),
            "tutor should have written lesson file at {path:?}",
        );
        // The file's content should match the embedded lesson.
        let written = std::fs::read_to_string(&path).expect("read lesson file");
        assert!(written.contains("Welcome to the Lattice Tutor"));
        assert!(written.contains("Lesson 1: Basic Motions"));
        // Cleanup.
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tutor_unknown_lesson_echoes_error() {
        let mut a = app_with("xx", 10);
        a.do_tutor(Some(99));
        let msg = a.editor.last_message.as_ref().expect("error echo");
        assert_eq!(msg.level, EchoLevel::Error);
        // Host wording: "lesson 99 doesn't exist (lessons 1-5 available); …".
        assert!(msg.text.contains("lesson 99 doesn't exist"));
    }

    #[test]
    fn customize_unknown_group_emits_error_no_overlay() {
        let mut a = app_with("xx", 10);
        a.editor.set_command_line_text("customize definitely-not-a-group");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.editor.popup_buffer.is_none());
        let msg = a.editor.last_message.as_ref().expect("error echo");
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("no group named"));
    }

    #[test]
    fn customize_unknown_mode_emits_error_no_overlay() {
        let mut a = app_with("xx", 10);
        a.editor.set_command_line_text("customize definitely-not-a-mode");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.editor.popup_buffer.is_none());
        let msg = a.editor.last_message.as_ref().expect("error echo");
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("no mode named"));
    }

    #[test]
    fn describe_option_resolution_unknown_emits_error() {
        let mut a = app_with("xx", 10);
        a.editor.set_command_line_text("describe-option-resolution definitely-not-an-option");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let msg = a.editor.last_message.as_ref().expect("error echo");
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("E518"));
    }

    #[test]
    fn describe_mode_shows_active_state_for_current_buffer() {
        let mut a = app_with("xx", 10);
        a.toggle_mode_by_name("lsp-mode");
        a.editor.set_command_line_text("describe-mode lsp-mode");
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let body = a.popup_help().unwrap().content.as_string();
        assert!(
            body.contains("active on current buffer: yes"),
            "expected active=yes\n{body}",
        );
    }
}
