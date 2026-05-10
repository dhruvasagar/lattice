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
//! engine, candidate scoring -- those are owned by
//! `crate::picker`. This module is App's *workflow* layer
//! above that.

use crate::buffer_registry::{BufferData, BufferRegistry};
use crate::buffers::BufferId;

use super::{App, EchoLevel};

impl App {
    /// Snapshot of every running LSP actor as picker rows.
    /// Built by reading the supervisor's `ArcSwap<SupervisorSnapshot>`,
    /// so the read is wait-free; the previous `try_lock`
    /// fall-through (degrade to empty if supervisor was
    /// busy) is gone -- the snapshot is always readable.
    fn snapshot_lsp_instances(&mut self) -> Vec<crate::picker::LspInstanceRow> {
        let actors = self.lsp.running_actors();
        actors
            .into_iter()
            .map(|((workspace, server_id), handle)| {
                let key = (workspace.clone(), server_id.clone());
                let buffer_count = self.lsp.buffer_count_for(&key);
                let caps = handle.capabilities();
                let cap_summary = lattice_lsp::help_views::summarise_capabilities(&caps);
                crate::picker::LspInstanceRow {
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
        let rows: Vec<crate::picker::LspLocationRow> = locations
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
                Some(crate::picker::LspLocationRow {
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
        let mut p = crate::picker::Picker::new(
            title,
            crate::picker::PickerSource::LspLocations,
            crate::picker::PickerAction::JumpToLspLocation,
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
        on_accept: crate::picker::PickerAction,
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
        let matches: Vec<&crate::picker::LspInstanceRow> = rows
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
                crate::picker::PickerAction::OpenLspLog => {
                    self.open_lsp_log_in_pane(&server_id)
                }
                crate::picker::PickerAction::OpenLspTraceLog => {
                    self.open_lsp_trace_log_in_pane(&server_id)
                }
                crate::picker::PickerAction::SwitchToBuffer
                | crate::picker::PickerAction::JumpToLspLocation
                | crate::picker::PickerAction::AcceptLspCompletion
                | crate::picker::PickerAction::AcceptLspCodeAction => {}
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
        let mut p = crate::picker::Picker::new(
            title,
            crate::picker::PickerSource::LspInstances {
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
    pub(super) fn open_buffer_picker(&mut self) {
        let active = self.active_pane_buffer_id();
        let mut p = crate::picker::Picker::new(
            "buffers",
            crate::picker::PickerSource::Buffers,
            crate::picker::PickerAction::SwitchToBuffer,
        );
        // Host-side candidate build (the picker module is
        // renderer-agnostic and doesn't import `BufferRegistry`).
        let pairs = raw_buffer_candidates(&self.buffers, active);
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
    /// [`crate::picker::PickerAction::SwitchToBuffer`], activate
    /// the currently-selected candidate's buffer in the active
    /// pane *as a preview* -- no position-history push, no
    /// commit. Called after every selection change while a buffer
    /// picker is open.
    pub(super) fn preview_picker_selection(&mut self) {
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        if !matches!(picker.on_accept, crate::picker::PickerAction::SwitchToBuffer) {
            return;
        }
        let Some(c) = picker.selected_candidate() else {
            return;
        };
        let Some(crate::picker::RoutingPayload::Buffer { id: raw_id }) =
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
    /// For [`crate::picker::PickerAction::SwitchToBuffer`] the
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
        match routing {
            crate::picker::RoutingPayload::Buffer { id: raw_id } => {
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
            crate::picker::RoutingPayload::LspInstance { server_id, .. } => {
                match picker.on_accept {
                    crate::picker::PickerAction::OpenLspLog => {
                        self.open_lsp_log_in_pane(&server_id);
                    }
                    crate::picker::PickerAction::OpenLspTraceLog => {
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
            crate::picker::RoutingPayload::LspLocation { path, line, col } => {
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
            crate::picker::RoutingPayload::LspCompletion { index } => {
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
            crate::picker::RoutingPayload::LspCodeAction { index } => {
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
        }
    }
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
    active: BufferId,
) -> Vec<(lattice_completion::RawCandidate, crate::picker::RoutingPayload)> {
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
            BufferData::FileTree(t) => (
                // M.3.2.c.2 note: this is a free-function
                // buffer-picker site without App access.
                // Reads the struct field directly; M.3.2.c.5
                // can route through a parameterised
                // `buffer_locals: &HashMap<...>` once free
                // functions are reworked.
                format!("#{:<3} {}", id.0, t.root.display()),
                format!("tree{active_marker}"),
            ),
            BufferData::Help(h) => (
                format!("#{:<3} {}", id.0, h.title),
                format!("help{active_marker}"),
            ),
            BufferData::Oil(o) => (
                format!("#{:<3} {}", id.0, o.dir.display()),
                format!("oil{active_marker}"),
            ),
        };
        // `text` is the user-facing buffer id; matcher matches
        // against `display`. The typed routing payload carries
        // the buffer id the accept dispatch consumes.
        let mut raw = lattice_completion::RawCandidate::plain(
            format!("#{}", id.0),
            lattice_completion::CandidateKind::Buffer,
        );
        raw.display = format!("{body:<60} {kind_label}");
        out.push((raw, crate::picker::RoutingPayload::Buffer { id: id.0 }));
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
}
