//! In-buffer search (`/`, `?`, `n`, `N`, `*`, `#`, `;`, `,`)
//! and the substitute (`:s`) command + live preview.
//!
//! Methods that live here:
//! - Search-line state machine: `preview_search`,
//!   `submit_search`, `cancel_search`, `repeat_search`
//!   (the `n` / `N` repeat).
//! - Word-under-cursor (`*` / `#`):
//!   `do_search_word_under_cursor`.
//! - Find-repeat (`;` / `,`): `do_find_repeat`.
//! - Substitute: `refresh_substitute_preview` and
//!   `collect_substitute_matches_for_line` (the live
//!   preview pipeline driven from cmdline keystrokes).
//!   `do_substitute` itself (the `:s` body) lives host-side
//!   in [`lattice_host::dispatch::Editor::do_substitute`].
//! - Free fns: `compile_search_pattern` (the regex compile
//!   wrapper used everywhere a pattern enters the search
//!   engine).
//!
//! What does NOT live here:
//! - The fancy-regex / lattice_core::search engines
//!   themselves (those are in lattice-core).
//! - The `SearchLine` / `LastSearch` / `LastFind` /
//!   `SubstitutePreview` structs (state shapes -- live in
//!   `app.rs` next to the App fields they describe).
//! - `:noh` (currently dispatched in cmdline executor;
//!   migrates with the cmdline slice).

use fancy_regex::Regex;
use lattice_core::Buffer;
use lattice_protocol::position::{Position, Range as ProtoRange};

use super::{App, SubstitutePreview, last_addressable_line};

impl App {
    // 5.5.G.10: `preview_search`, `submit_search`, `cancel_search`,
    // `repeat_search` all migrated to
    // [`lattice_host::dispatch::Editor`] (zero remaining App
    // callers — the `Action::Search*` arms now route host-side
    // and the internal `submit_search -> repeat_search` self-call
    // moved with them).

    /// Drops the preview when the cmdline doesn't parse as a
    /// substitute, when the pattern is empty, or when regex
    /// compilation fails. Cleared explicitly by CommandLineCancel
    /// and by execute_ex_line on submit.
    pub(super) fn refresh_substitute_preview(&mut self) {
        let parsed = match crate::excommand::try_parse_substitute_partial(&self.editor.command_line) {
            Some(p) => p,
            None => {
                self.editor.substitute_preview = None;
                return;
            }
        };
        if parsed.pattern.is_empty() {
            self.editor.substitute_preview = None;
            return;
        }
        let regex = match compile_search_pattern(&parsed.pattern) {
            Ok(r) => r,
            Err(_) => {
                // Pattern doesn't compile yet (mid-typing). Keep the
                // last preview rather than flickering -- but if we
                // never had one, drop quietly.
                return;
            }
        };
        let global = parsed
            .flags
            .as_ref()
            .map(|f| f.contains('g'))
            .unwrap_or(false);

        let buffer = self.editor.document.snapshot().buffer.clone();
        let mut matches: Vec<ProtoRange> = Vec::new();
        match parsed.scope {
            crate::excommand::SubstitutePartialScope::CurrentLine => {
                self.collect_substitute_matches_for_line(
                    &buffer,
                    &regex,
                    self.editor.cursor.line,
                    global,
                    &mut matches,
                );
            }
            crate::excommand::SubstitutePartialScope::Whole => {
                let last = last_addressable_line(&buffer);
                for line in 0..=last {
                    self.collect_substitute_matches_for_line(
                        &buffer,
                        &regex,
                        line,
                        global,
                        &mut matches,
                    );
                }
            }
        }

        self.editor.substitute_preview = Some(SubstitutePreview {
            matches,
            replacement: parsed.replacement,
            global,
        });
    }

    /// Push every match of `regex` on `line` into `out`. Honours
    /// `global`: when false, only the leftmost match is collected
    /// (mirrors vim's default `:s` without the `g` flag).
    fn collect_substitute_matches_for_line(
        &self,
        buffer: &Buffer,
        regex: &fancy_regex::Regex,
        line: u32,
        global: bool,
        out: &mut Vec<ProtoRange>,
    ) {
        let line_text = match buffer.line(line) {
            Some(s) => s,
            None => return,
        };
        if line_text.is_empty() {
            return;
        }
        for m in regex.find_iter(&line_text) {
            let m = match m {
                Ok(m) => m,
                Err(_) => break,
            };
            let start = Position::new(line, m.start() as u32);
            let end = Position::new(line, m.end() as u32);
            out.push(ProtoRange::new(start, end));
            if !global {
                break;
            }
        }
    }

    // 5.5.H: `do_substitute` App-side delegate retired (zero
    // callers; host copy lives at
    // [`lattice_host::dispatch::Editor::do_substitute`]).

    // 5.5.G.23.macros: `do_find_repeat` migrated to
    // [`lattice_host::dispatch::Editor::do_find_repeat`]. Zero
    // remaining App callers after `Action::FindRepeat` collapsed to a
    // host-handled no-op arm.

    // 5.5.G.10: `do_search_word_under_cursor` migrated to
    // [`lattice_host::dispatch::Editor`] (zero remaining App
    // callers).
}

/// Compile a user-supplied pattern with the engine's default
/// flags. Wrapping `Regex::new` so callers stay decoupled from
/// the regex crate choice (fancy-regex today; lattice's own
/// engine post-1.0).
///
/// Why a free function: hlsearch / live-preview compiles per
/// keystroke; the submit path compiles once. Both reach for the
/// same helper. If profiling shows compile cost bites we can add
/// a cache on App keyed by `(pattern, ...flags)` -- but for ~10us
/// compile of typical patterns it's unnecessary.
fn compile_search_pattern(pattern: &str) -> Result<Regex, String> {
    Regex::new(pattern).map_err(|e| e.to_string())
}

// 5.5.H: `step_byte` retired (zero callers; host's
// `lattice_host::dispatch::step_byte` is the live copy used by
// the host-side search repeat).

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use crate::app::test_helpers::{app_with, install_help, submit_ex};
    use crate::app::*;
    use crate::help::HelpContent;
    use lattice_grammar::{ModalState, SearchDirection};
    use lattice_protocol::position::Position;

    fn type_pattern(a: &mut App, pattern: &str) {
        for c in pattern.chars() {
            a.apply(Action::SearchAppend(c));
        }
    }

    fn type_cmdline(a: &mut App, s: &str) {
        a.apply(Action::EnterCommandLine);
        for c in s.chars() {
            a.apply(Action::CommandLineAppend(c));
        }
    }

    // ---- Substitute live preview ----

    #[test]
    fn substitute_preview_highlights_first_match_on_current_line_without_g() {
        let mut a = app_with("foo bar foo baz foo\nfoo elsewhere", 10);
        a.editor.cursor = Position::new(0, 0);
        type_cmdline(&mut a, "s/foo/X");
        let preview = a.editor.substitute_preview.as_ref().expect("preview live");
        assert_eq!(preview.matches.len(), 1, "only the leftmost match -- no /g");
        assert_eq!(preview.matches[0].start, Position::new(0, 0));
        assert_eq!(preview.replacement.as_deref(), Some("X"));
        assert!(!preview.global);
    }

    #[test]
    fn substitute_preview_with_g_flag_highlights_every_match_on_line() {
        let mut a = app_with("foo bar foo baz foo\nfoo elsewhere", 10);
        a.editor.cursor = Position::new(0, 0);
        type_cmdline(&mut a, "s/foo/X/g");
        let preview = a.editor.substitute_preview.as_ref().unwrap();
        // Three matches on the cursor's line; line 1 is out of scope.
        assert_eq!(preview.matches.len(), 3);
        assert!(preview.global);
    }

    #[test]
    fn substitute_preview_percent_scope_walks_whole_buffer() {
        let mut a = app_with("foo\nbar foo\nfoo", 10);
        a.editor.cursor = Position::new(0, 0);
        type_cmdline(&mut a, "%s/foo/X/g");
        let preview = a.editor.substitute_preview.as_ref().unwrap();
        // Three matches across three lines.
        assert_eq!(preview.matches.len(), 3);
    }

    #[test]
    fn substitute_preview_clears_on_cmdline_cancel() {
        let mut a = app_with("foo bar", 10);
        type_cmdline(&mut a, "s/foo/X");
        assert!(a.editor.substitute_preview.is_some());
        a.apply(Action::CommandLineCancel);
        assert!(a.editor.substitute_preview.is_none());
    }

    #[test]
    fn substitute_preview_clears_on_cmdline_submit() {
        let mut a = app_with("foo bar", 10);
        type_cmdline(&mut a, "s/foo/X");
        assert!(a.editor.substitute_preview.is_some());
        a.apply(Action::CommandLineSubmit);
        assert!(a.editor.substitute_preview.is_none());
    }

    #[test]
    fn substitute_preview_dropped_when_input_no_longer_parses_as_substitute() {
        let mut a = app_with("foo bar", 10);
        // Enter a substitute, get preview, then backspace past `s` --
        // input is no longer a substitute.
        type_cmdline(&mut a, "s/foo");
        assert!(a.editor.substitute_preview.is_some());
        for _ in 0.."s/foo".len() {
            a.apply(Action::CommandLineBackspace);
        }
        assert!(a.editor.substitute_preview.is_none());
    }

    #[test]
    fn substitute_preview_empty_pattern_drops_preview() {
        // After typing `s/` the pattern is empty -- preview shouldn't
        // highlight anything (no matches to show).
        let mut a = app_with("foo bar", 10);
        type_cmdline(&mut a, "s/");
        assert!(a.editor.substitute_preview.is_none());
    }

    // ---- Search basic (/, ?, n, N) ----

    #[test]
    fn enter_search_seeds_state() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        assert_eq!(a.editor.modal, ModalState::Search(SearchDirection::Forward));
        let line = a.editor.search_line.as_ref().expect("search_line populated");
        assert_eq!(line.pattern, "");
        assert_eq!(line.origin, Position::ZERO);
    }

    #[test]
    fn search_append_grows_pattern_and_previews_match() {
        let mut a = app_with("foo bar foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "bar");
        let line = a.editor.search_line.as_ref().unwrap();
        assert_eq!(line.pattern, "bar");
        // Preview should highlight the first match without moving cursor.
        let m = a.editor.current_match.expect("match previewed");
        assert_eq!(m.start, Position::new(0, 4));
        assert_eq!(a.editor.cursor, Position::ZERO);
    }

    #[test]
    fn search_backspace_shrinks_pattern_and_re_previews() {
        let mut a = app_with("foo bar baz", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "baz");
        a.apply(Action::SearchBackspace);
        assert_eq!(a.editor.search_line.as_ref().unwrap().pattern, "ba");
        let m = a.editor.current_match.expect("preview after backspace");
        assert_eq!(m.start, Position::new(0, 4));
    }

    #[test]
    fn search_backspace_on_empty_pattern_exits_search() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        a.apply(Action::SearchBackspace);
        assert_eq!(a.editor.modal, ModalState::Normal);
        assert!(a.editor.search_line.is_none());
    }

    #[test]
    fn search_submit_jumps_cursor_to_match_and_records_last_search() {
        let mut a = app_with("foo bar foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "bar");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.editor.modal, ModalState::Normal);
        assert_eq!(a.editor.cursor, Position::new(0, 4));
        assert!(a.editor.search_line.is_none());
        let last = a.editor.last_search.as_ref().unwrap();
        assert_eq!(last.pattern, "bar");
        assert_eq!(last.direction, SearchDirection::Forward);
    }

    #[test]
    fn search_submit_with_no_match_records_pattern_and_warns() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "xyz");
        a.apply(Action::SearchSubmit);
        assert!(a.editor.current_match.is_none());
        assert_eq!(a.editor.last_search.as_ref().unwrap().pattern, "xyz");
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("Pattern not found"));
    }

    #[test]
    fn search_cancel_restores_cursor_to_origin() {
        let mut a = app_with("foo bar foo", 10);
        a.editor.cursor = Position::new(0, 5);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        // Preview should have set current_match to "foo" at byte 8.
        assert_eq!(a.editor.current_match.unwrap().start, Position::new(0, 8));
        a.apply(Action::SearchCancel);
        assert_eq!(a.editor.modal, ModalState::Normal);
        assert_eq!(a.editor.cursor, Position::new(0, 5));
        assert!(a.editor.current_match.is_none());
    }

    #[test]
    fn n_after_forward_search_advances_to_next_match() {
        let mut a = app_with("foo bar foo bar", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.editor.cursor, Position::new(0, 0));
        a.apply(Action::SearchNext);
        assert_eq!(a.editor.cursor, Position::new(0, 8));
    }

    #[test]
    fn capital_n_reverses_direction() {
        let mut a = app_with("foo bar foo bar", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        a.apply(Action::SearchSubmit);
        a.apply(Action::SearchNext);
        assert_eq!(a.editor.cursor, Position::new(0, 8));
        a.apply(Action::SearchPrevious);
        assert_eq!(a.editor.cursor, Position::new(0, 0));
    }

    #[test]
    fn n_with_no_last_search_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::SearchNext);
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("no previous"));
    }

    #[test]
    fn search_forward_wraps_and_warns() {
        let mut a = app_with("alpha beta gamma alpha", 10);
        a.editor.cursor = Position::new(0, 17); // past the second "alpha"... actually at it
        // Move past it for clarity.
        a.editor.cursor = Position::new(0, 18);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "alpha");
        a.apply(Action::SearchSubmit);
        // First "alpha" is at byte 0; we wrapped from byte 18.
        assert_eq!(a.editor.cursor, Position::new(0, 0));
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Warn);
        assert!(msg.text.contains("BOTTOM"));
    }

    #[test]
    fn search_backward_finds_previous_match() {
        let mut a = app_with("alpha beta gamma alpha", 10);
        a.editor.cursor = Position::new(0, 22);
        a.apply(Action::EnterSearch(SearchDirection::Backward));
        type_pattern(&mut a, "alpha");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.editor.cursor, Position::new(0, 17));
    }

    // ---- :noh ----

    #[test]
    fn nohlsearch_clears_overlay() {
        let mut a = app_with("foo bar foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        a.apply(Action::SearchSubmit);
        assert!(!a.editor.all_matches.is_empty());
        submit_ex(&mut a, "noh");
        assert!(a.editor.all_matches.is_empty());
        assert!(a.editor.current_match.is_none());
    }

    // ---- Substitute (:s/foo/bar/[g]) ----

    #[test]
    fn substitute_first_match_on_current_line() {
        let mut a = app_with("foo bar foo", 10);
        submit_ex(&mut a, "s/foo/baz/");
        assert_eq!(a.editor.document.text(), "baz bar foo");
    }

    #[test]
    fn substitute_global_replaces_all_on_line() {
        let mut a = app_with("foo bar foo", 10);
        submit_ex(&mut a, "s/foo/baz/g");
        assert_eq!(a.editor.document.text(), "baz bar baz");
    }

    #[test]
    fn substitute_whole_buffer_with_g_flag() {
        let mut a = app_with("foo\nbar foo\nfoo", 10);
        submit_ex(&mut a, "%s/foo/X/g");
        assert_eq!(a.editor.document.text(), "X\nbar X\nX");
    }

    #[test]
    fn substitute_no_match_emits_error() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "s/xyz/abc/");
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("Pattern not found"));
        assert_eq!(a.editor.document.text(), "hello");
    }

    #[test]
    fn substitute_empty_replacement_deletes_pattern() {
        let mut a = app_with("hello world hello", 10);
        submit_ex(&mut a, "s/hello //g");
        assert_eq!(a.editor.document.text(), "world hello");
    }

    #[test]
    fn substitute_count_message() {
        let mut a = app_with("foo foo foo", 10);
        submit_ex(&mut a, "s/foo/X/g");
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Info);
        assert!(msg.text.contains("3"));
    }

    #[test]
    fn substitute_only_current_line_without_percent() {
        let mut a = app_with("foo\nfoo\nfoo", 10);
        a.editor.cursor = Position::new(1, 0);
        submit_ex(&mut a, "s/foo/X/");
        assert_eq!(a.editor.document.text(), "foo\nX\nfoo");
    }

    // ---- Find-repeat (; / ,) ----

    #[test]
    fn semicolon_repeats_last_find_forward() {
        let mut a = app_with("hello world", 10);
        // First f-find for 'l': cursor moves to byte 2.
        let inv = lattice_grammar::CommandInvocation::of(a.editor.builtins.find_char_forward.0)
            .with_args(lattice_grammar::Args::Char('l'));
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.cursor, Position::new(0, 2));
        // `;` repeats: byte 3.
        a.apply(Action::FindRepeat { reverse: false });
        assert_eq!(a.editor.cursor, Position::new(0, 3));
    }

    #[test]
    fn comma_reverses_last_find_direction() {
        let mut a = app_with("hello world", 10);
        // f l forward.
        let inv = lattice_grammar::CommandInvocation::of(a.editor.builtins.find_char_forward.0)
            .with_args(lattice_grammar::Args::Char('l'));
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.cursor, Position::new(0, 2));
        // f l again, then `,` should reverse to find the previous 'l'.
        a.apply(Action::FindRepeat { reverse: false });
        assert_eq!(a.editor.cursor, Position::new(0, 3));
        a.apply(Action::FindRepeat { reverse: true });
        assert_eq!(a.editor.cursor, Position::new(0, 2));
    }

    #[test]
    fn find_repeat_with_no_history_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::FindRepeat { reverse: false });
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    // ---- Search submit + position history ----

    #[test]
    fn search_submit_pushes_position_history() {
        let mut a = app_with("foo bar baz foo", 10);
        a.editor.cursor = Position::new(0, 8); // on 'b' of "baz"
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        for c in "foo".chars() {
            a.apply(Action::SearchAppend(c));
        }
        a.apply(Action::SearchSubmit);
        // Cursor jumped to second "foo" at byte 12.
        assert_eq!(a.editor.cursor, Position::new(0, 12));
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.editor.cursor, Position::new(0, 8));
    }

    // ---- Search auto-opens fold ----

    #[test]
    fn search_into_closed_fold_auto_opens_it() {
        // `docs/user/folding.md`: search hits open the fold the
        // cursor lands in.
        let initial = "# H1\nbody one needle\nbody two\n# H2\nafter\n";
        let mut a = app_with(initial, 10);
        a.set_foldmethod_for_test(FoldMethod::Markdown);
        a.recompute_folds();
        let idx = a
            .editor.folds
            .iter()
            .position(|f| f.start_line == 0)
            .expect("H1 fold");
        a.editor.folds[idx].closed = true;
        // Submit a forward search from the top of the buffer.
        a.editor.search_line = Some(SearchLine {
            origin: Position::ZERO,
            pattern: "needle".into(),
            direction: SearchDirection::Forward,
        });
        a.editor.modal = ModalState::Search(SearchDirection::Forward);
        a.apply(Action::SearchSubmit);
        // The fold containing `body one` should now be open.
        let fold = a
            .editor.folds
            .iter()
            .find(|f| f.start_line == 0)
            .expect("H1 fold still present");
        assert!(!fold.closed, "search should have auto-opened the fold");
    }

    // ---- Search in help buffer ----

    #[test]
    fn search_in_help_buffer_targets_help_text() {
        // After unification, `/` works in any read-only buffer
        // (help, file-tree, future kinds). Search reads
        // `active_text()` and `self.editor.cursor`; on a hit it writes
        // `self.editor.cursor` -- exactly the document path.
        let mut a = app_with("xx", 10);
        let body: Vec<String> = vec![
            "alpha".into(),
            "beta".into(),
            "gamma needle".into(),
            "delta".into(),
        ];
        install_help(&mut a, HelpContent::from_lines("search-test", body));
        // Open `/` and type `needle` then submit.
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        for c in "needle".chars() {
            a.apply(Action::SearchAppend(c));
        }
        a.apply(Action::SearchSubmit);
        // Cursor should land on line 2 (gamma needle).
        assert_eq!(a.editor.cursor.line, 2, "cursor jumped to the help line");
        // Active buffer stays Help -- search didn't leak into the
        // document.
        assert!(matches!(a.editor.active_buffer, BufferKind::Help));
    }

    #[test]
    fn search_in_help_buffer_populates_all_matches_for_hlsearch() {
        // The renderer paints `app.editor.all_matches` as styled overlays
        // on each visible help line (same painter the document
        // path uses). This test ensures `submit_search` in a help
        // buffer fills `all_matches` against the help text -- the
        // *render* check (visible highlight) is covered by the
        // existing `apply_match_overlay` unit tests; here we just
        // assert the data is in place for the renderer to use.
        let mut a = app_with("xx", 10);
        let body: Vec<String> = vec![
            "alpha needle".into(),
            "beta".into(),
            "gamma needle".into(),
            "delta needle".into(),
        ];
        install_help(&mut a, HelpContent::from_lines("hl-test", body));
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        for c in "needle".chars() {
            a.apply(Action::SearchAppend(c));
        }
        a.apply(Action::SearchSubmit);
        assert_eq!(
            a.editor.all_matches.len(),
            3,
            "every occurrence in the help body should be in all_matches"
        );
        assert!(a.editor.current_match.is_some());
    }

    #[test]
    fn search_in_help_buffer_no_longer_blocked_by_read_only_guard() {
        // Regression: `EnterSearch` etc. used to be in the
        // `action_is_document_mutation` allow-list, so `/` in a
        // help buffer echoed "buffer is read-only". They're not
        // mutations -- the guard list now only covers true edits.
        let mut a = app_with("xx", 10);
        install_help(&mut a, HelpContent::from_lines("ro", vec!["abc".into(); 5]));
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        // Should be in search modal, not Normal with a read-only
        // echo.
        assert!(
            matches!(a.editor.modal, ModalState::Search(_)),
            "should be in Search modal, got {:?}",
            a.editor.modal
        );
        assert!(
            a.editor.last_message.is_none(),
            "no read-only echo expected, got {:?}",
            a.editor.last_message
        );
    }

    // ---- Search hlsearch / all_matches ----

    #[test]
    fn search_preview_populates_all_matches() {
        let mut a = app_with("foo bar foo baz foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        assert_eq!(a.editor.all_matches.len(), 3);
    }

    #[test]
    fn search_submit_keeps_all_matches_for_hlsearch() {
        let mut a = app_with("foo bar foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.editor.all_matches.len(), 2);
    }

    #[test]
    fn search_cancel_clears_all_matches() {
        let mut a = app_with("foo bar foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        assert!(!a.editor.all_matches.is_empty());
        a.apply(Action::SearchCancel);
        assert!(a.editor.all_matches.is_empty());
    }

    #[test]
    fn search_word_under_cursor_populates_all_matches() {
        let mut a = app_with("foo bar foo bar foo", 10);
        a.editor.cursor = Position::new(0, 1); // on first "foo"
        a.apply(Action::SearchWordUnderCursor(SearchDirection::Forward));
        assert_eq!(a.editor.all_matches.len(), 3);
    }

    #[test]
    fn search_works_across_lines() {
        let mut a = app_with("foo\nbar\nfoo\nbaz", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.editor.cursor, Position::new(0, 0));
        a.apply(Action::SearchNext);
        assert_eq!(a.editor.cursor, Position::new(2, 0));
    }

    #[test]
    fn find_no_match_keeps_cursor() {
        let mut a = app_with("hello", 10);
        a.editor.cursor = Position::new(0, 1);
        let inv = CommandInvocation::of(a.editor.builtins.find_char_forward.0)
            .with_args(lattice_grammar::Args::Char('z'));
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }
}
