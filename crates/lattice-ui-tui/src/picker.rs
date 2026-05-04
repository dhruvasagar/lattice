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
//! This module is the **data model** for pickers; it owns no
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
//! When a second renderer needs the picker the natural move is
//! to extract this module into a sibling crate (e.g.
//! `lattice-picker`); the only dependency to carry across is
//! `lattice-completion`. No file-by-file edits required because
//! every coupling already lives in the host.

use std::ops::Range;
use std::path::{Path, PathBuf};

use lattice_completion::{MatchScore, RawCandidate, RenderedCandidate};

/// Where a picker pulls its raw candidates from. The App resolves
/// this on `populate` / `refresh` and walks the appropriate source.
/// One enum variant per first-party source so the App stays
/// decoupled from generator implementations; plugin-provided
/// pickers will arrive as a separate `Plugin(GeneratorId)`
/// variant once the WASM host is online.
#[derive(Debug, Clone)]
pub enum PickerSource {
    /// Walk every entry in [`BufferRegistry`] -- the buffer
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
}

/// What `<CR>` does to the selected candidate. Variants stay
/// dumb data; the App's [`crate::app::App::accept_picker`]
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
    /// For [`PickerAction::SwitchToBuffer`]: the host's buffer
    /// id encoded as `u32` so this module stays renderer-agnostic
    /// (the TUI host newtype-wraps it). The host's preview path
    /// (`activate_buffer_preview`) restores this on `<Esc>` so an
    /// aborted pick doesn't leave the user on a random previewed
    /// buffer. `None` for non-buffer pickers.
    pub preview_origin: Option<u32>,
}

impl Picker {
    pub fn new(
        title: impl Into<String>,
        source: PickerSource,
        on_accept: PickerAction,
    ) -> Self {
        Self {
            title: title.into(),
            query: String::new(),
            query_cursor: 0,
            candidates: Vec::new(),
            selected: 0,
            source,
            on_accept,
            raw: Vec::new(),
            preview_origin: None,
        }
    }

    /// Replace the raw candidate list. Host-built (e.g. the TUI
    /// host walks `BufferRegistry` for the buffer switcher);
    /// picker just stores + refilters. The single mutation entry
    /// point: every other "set the candidates" helper (e.g.
    /// [`Self::set_lsp_instances`]) routes through this.
    pub fn set_raw_candidates(&mut self, raw: Vec<RawCandidate>) {
        self.raw = raw;
        self.refilter();
    }

    /// Replace the raw candidate list with externally-built LSP
    /// instance rows. Caller (`App::open_lsp_picker`) snapshots the
    /// supervisor under its lock and hands the resulting tuples
    /// here. Refreshes the filter.
    pub fn set_lsp_instances(&mut self, rows: Vec<LspInstanceRow>) {
        let prefilter = match &self.source {
            PickerSource::LspInstances { prefilter } => prefilter.clone(),
            _ => None,
        };
        let raw: Vec<RawCandidate> = rows
            .into_iter()
            .filter(|r| match &prefilter {
                Some(want) => r.server_id == *want,
                None => true,
            })
            .map(|r| r.into_candidate())
            .collect();
        self.set_raw_candidates(raw);
    }

    /// Filter `raw` against the current `query` and write the
    /// matches into `candidates`. Substring + case-insensitive,
    /// scored by match offset (earlier match = higher score) so
    /// the first hit becomes the natural top row.
    pub fn refilter(&mut self) {
        let q = self.query.to_lowercase();
        let mut scored: Vec<(RawCandidate, MatchScore, Vec<Range<usize>>)> = Vec::new();
        for raw in &self.raw {
            if q.is_empty() {
                scored.push((raw.clone(), MatchScore::PERFECT, Vec::new()));
                continue;
            }
            let display_lc = raw.display.to_lowercase();
            if let Some(start) = display_lc.find(&q) {
                let end = start + q.len();
                // Score: earlier match wins; prefix-at-zero gets
                // PREFIX, otherwise SUBSTRING with a small bonus
                // inversely proportional to the offset so 'foo'
                // landing at byte 1 still ranks above 'foo' at
                // byte 20.
                let score = if start == 0 {
                    MatchScore::PREFIX
                } else {
                    let bonus = 200u32.saturating_sub(start as u32 * 4);
                    MatchScore(MatchScore::SUBSTRING.get() + bonus)
                };
                scored.push((raw.clone(), score, vec![Range { start, end }]));
            }
        }
        scored.sort_by(|a, b| b.1.cmp(&a.1));
        self.candidates = scored
            .into_iter()
            .map(|(raw, score, match_ranges)| RenderedCandidate {
                raw,
                score,
                match_ranges,
                annotations: Vec::new(),
            })
            .collect();
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

/// Parse a candidate's dispatch text (`"#<id>"`) back into the
/// host's buffer-id raw value (a `u32`; the host newtype-wraps
/// it). Returns `None` if the format doesn't match; callers
/// treat that as "selection no longer routable" and echo an error
/// rather than crashing.
///
/// Returns `u32` rather than a host type so this module stays
/// renderer-agnostic.
pub fn buffer_id_from_text(text: &str) -> Option<u32> {
    text.strip_prefix('#')?.parse().ok()
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
    /// Render this row as a [`RawCandidate`]. Encodes the
    /// `(server_id, workspace)` pair as `text` for round-tripping
    /// through [`lsp_key_from_text`]; `display` is the body +
    /// marginalia the popup paints.
    pub fn into_candidate(self) -> RawCandidate {
        let workspace_str = self.workspace.display().to_string();
        let mut raw = RawCandidate::plain(
            format!("{}\t{}", self.server_id, workspace_str),
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
        raw
    }
}

/// Parse a candidate's dispatch text (`"<server_id>\t<workspace>"`)
/// back into the `(workspace, server_id)` pair the supervisor
/// keys actors by.
pub fn lsp_key_from_text(text: &str) -> Option<(PathBuf, String)> {
    let (server_id, workspace) = text.split_once('\t')?;
    Some((Path::new(workspace).to_path_buf(), server_id.to_string()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use lattice_completion::CandidateKind;

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
    fn empty_query_returns_all_candidates_in_source_order() {
        let mut p =
            Picker::new("buffers", PickerSource::Buffers, PickerAction::SwitchToBuffer);
        p.set_raw_candidates(buffer_fixture());
        assert_eq!(p.candidates.len(), 2);
    }

    #[test]
    fn typing_query_filters_to_substring_matches() {
        let mut p =
            Picker::new("buffers", PickerSource::Buffers, PickerAction::SwitchToBuffer);
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
        let mut p =
            Picker::new("buffers", PickerSource::Buffers, PickerAction::SwitchToBuffer);
        p.set_raw_candidates(buffer_fixture());
        p.append_query('R');
        p.append_query('U');
        p.append_query('S');
        p.append_query('T');
        assert_eq!(p.candidates.len(), 1);
    }

    #[test]
    fn selection_wraps_at_boundaries() {
        let mut p =
            Picker::new("buffers", PickerSource::Buffers, PickerAction::SwitchToBuffer);
        p.set_raw_candidates(buffer_fixture());
        p.select_prev(); // wraps to last
        assert_eq!(p.selected, 1);
        p.select_next(); // wraps back to 0
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn backspace_repopulates_filter_results() {
        let mut p =
            Picker::new("buffers", PickerSource::Buffers, PickerAction::SwitchToBuffer);
        p.set_raw_candidates(buffer_fixture());
        p.append_query('r');
        p.append_query('u');
        assert_eq!(p.candidates.len(), 1);
        p.backspace_query();
        p.backspace_query();
        assert_eq!(p.candidates.len(), 2);
    }

    #[test]
    fn buffer_id_from_text_round_trips() {
        let mut p =
            Picker::new("buffers", PickerSource::Buffers, PickerAction::SwitchToBuffer);
        p.set_raw_candidates(buffer_fixture());
        let text = &p.candidates[0].raw.text;
        let id = buffer_id_from_text(text).expect("parses");
        assert_eq!(id, 1);
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
            assert!(c.raw.text.starts_with("rust\t"));
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
    fn lsp_key_round_trips_from_candidate_text() {
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
        let text = &p.candidates[0].raw.text;
        let (ws, sid) = lsp_key_from_text(text).expect("parses");
        assert_eq!(sid, "rust");
        assert_eq!(ws, PathBuf::from("/proj/example"));
    }

    #[test]
    fn selected_candidate_is_none_when_filter_empties_list() {
        let mut p =
            Picker::new("buffers", PickerSource::Buffers, PickerAction::SwitchToBuffer);
        p.set_raw_candidates(buffer_fixture());
        p.append_query('z'); // matches nothing
        p.append_query('z');
        p.append_query('z');
        assert!(p.candidates.is_empty());
        assert!(p.selected_candidate().is_none());
    }
}
