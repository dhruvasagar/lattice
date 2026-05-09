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
//! - Substitute: `do_substitute` (the `:s` body) plus
//!   `refresh_substitute_preview` and
//!   `collect_substitute_matches_for_line` (the live
//!   preview pipeline driven from cmdline keystrokes).
//! - Free fns: `compile_search_pattern` (the regex compile
//!   wrapper used everywhere a pattern enters the search
//!   engine) and `step_byte` (one-byte cursor advance for
//!   `n` / `N` to skip the current match).
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
use lattice_core::search::SearchHit;
use lattice_grammar::CommandInvocation;
use lattice_grammar::SearchDirection;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range as ProtoRange};
use lattice_runtime::CancellationToken;

use super::{
    App, BufferKind, EchoLevel, FindKind, LastSearch, PositionSource, SubstitutePreview,
    is_word_char_byte, last_addressable_line, line_byte_len, previous_position,
};

impl App {
    /// Run the in-progress pattern from origin. Used to highlight the
    /// current match while typing in `/` or `?`. Does not move cursor;
    /// the cursor jumps only on `SearchSubmit`.
    pub(super) fn preview_search(&mut self) {
        let Some(line) = self.search_line.as_ref() else {
            return;
        };
        if line.pattern.is_empty() {
            self.current_match = None;
            self.all_matches.clear();
            return;
        }
        // Live preview tolerates compile errors silently -- the user
        // is still typing. The submit path surfaces the error.
        let Ok(regex) = compile_search_pattern(&line.pattern) else {
            self.current_match = None;
            self.all_matches.clear();
            return;
        };
        let dir = match line.direction {
            SearchDirection::Forward => lattice_core::search::Direction::Forward,
            SearchDirection::Backward => lattice_core::search::Direction::Backward,
        };
        let buffer = self.active_text();
        match lattice_core::search::find(
            &buffer,
            &regex,
            line.origin,
            dir,
            &CancellationToken::never(),
        ) {
            Ok(Some(SearchHit { range, .. })) => self.current_match = Some(range),
            _ => self.current_match = None,
        }
        // Live hlsearch: highlight every occurrence as the user types.
        self.all_matches =
            lattice_core::search::find_all(&buffer, &regex, &CancellationToken::never())
                .unwrap_or_default();
    }

    pub(super) fn submit_search(&mut self) {
        let Some(line) = self.search_line.take() else {
            return;
        };
        self.modal = lattice_grammar::ModalState::Normal;
        if line.pattern.is_empty() {
            // Empty submit: re-run last_search if any (vim behavior).
            if self.last_search.is_some() {
                self.repeat_search(false);
            }
            return;
        }
        // Save the pre-search position so Ctrl-O returns.
        // Records the active buffer kind alongside the position
        // so cross-buffer walks (search in a help buffer ->
        // back to the document) work uniformly.
        self.push_position_history(line.origin, PositionSource::AutoJump);
        // Compile once for both find + find_all + later n/N replays.
        let regex = match compile_search_pattern(&line.pattern) {
            Ok(r) => r,
            Err(msg) => {
                self.set_message(EchoLevel::Error, format!("regex: {msg}"));
                self.current_match = None;
                self.all_matches.clear();
                return;
            }
        };
        let dir = match line.direction {
            SearchDirection::Forward => lattice_core::search::Direction::Forward,
            SearchDirection::Backward => lattice_core::search::Direction::Backward,
        };
        let buffer = self.active_text();
        match lattice_core::search::find(
            &buffer,
            &regex,
            line.origin,
            dir,
            &CancellationToken::never(),
        ) {
            Ok(Some(hit)) => {
                self.cursor = hit.range.start;
                self.current_match = Some(hit.range);
                self.all_matches =
                    lattice_core::search::find_all(&buffer, &regex, &CancellationToken::never())
                        .unwrap_or_default();
                if hit.wrapped {
                    let level = EchoLevel::Warn;
                    let text = match line.direction {
                        SearchDirection::Forward => "search hit BOTTOM, continuing at TOP",
                        SearchDirection::Backward => "search hit TOP, continuing at BOTTOM",
                    };
                    self.set_message(level, text.to_string());
                }
                self.last_search = Some(LastSearch {
                    pattern: line.pattern,
                    direction: line.direction,
                });
                if matches!(self.active_buffer, BufferKind::Document) {
                    self.auto_open_folds_at_cursor();
                }
            }
            Ok(None) => {
                self.current_match = None;
                self.all_matches.clear();
                self.set_message(
                    EchoLevel::Error,
                    format!("E486: Pattern not found: {}", line.pattern),
                );
                // Vim still records the pattern so `n`/`N` can retry later.
                self.last_search = Some(LastSearch {
                    pattern: line.pattern,
                    direction: line.direction,
                });
            }
            Err(_) => {
                self.current_match = None;
                self.all_matches.clear();
            }
        }
    }

    pub(super) fn cancel_search(&mut self) {
        if let Some(line) = self.search_line.take() {
            self.cursor = line.origin;
        }
        self.current_match = None;
        self.all_matches.clear();
        self.modal = lattice_grammar::ModalState::Normal;
    }

    /// Repeat last search. `reverse=false` keeps the original direction
    /// (`n`); `reverse=true` flips it (`N`).
    pub(super) fn repeat_search(&mut self, reverse: bool) {
        let Some(last) = self.last_search.clone() else {
            self.set_message(
                EchoLevel::Error,
                "E35: no previous regular expression".to_string(),
            );
            return;
        };
        // Push pre-jump cursor onto the unified ring regardless
        // of buffer kind so `<C-o>` walks back across help /
        // tree / document boundaries.
        let cur = self.cursor;
        self.push_position_history(cur, PositionSource::AutoJump);
        let direction = match (last.direction, reverse) {
            (SearchDirection::Forward, false) | (SearchDirection::Backward, true) => {
                SearchDirection::Forward
            }
            (SearchDirection::Backward, false) | (SearchDirection::Forward, true) => {
                SearchDirection::Backward
            }
        };
        let dir = match direction {
            SearchDirection::Forward => lattice_core::search::Direction::Forward,
            SearchDirection::Backward => lattice_core::search::Direction::Backward,
        };
        let buffer = self.active_text();
        // Skip current match: advance one byte in the chosen direction.
        let from = step_byte(&buffer, self.cursor, direction);
        let regex = match compile_search_pattern(&last.pattern) {
            Ok(r) => r,
            Err(msg) => {
                self.set_message(EchoLevel::Error, format!("regex: {msg}"));
                self.current_match = None;
                return;
            }
        };
        match lattice_core::search::find(&buffer, &regex, from, dir, &CancellationToken::never()) {
            Ok(Some(hit)) => {
                self.cursor = hit.range.start;
                self.current_match = Some(hit.range);
                if hit.wrapped {
                    let text = match direction {
                        SearchDirection::Forward => "search hit BOTTOM, continuing at TOP",
                        SearchDirection::Backward => "search hit TOP, continuing at BOTTOM",
                    };
                    self.set_message(EchoLevel::Warn, text.to_string());
                }
                if matches!(self.active_buffer, BufferKind::Document) {
                    self.auto_open_folds_at_cursor();
                }
            }
            Ok(None) => {
                self.current_match = None;
                self.set_message(
                    EchoLevel::Error,
                    format!("E486: Pattern not found: {}", last.pattern),
                );
            }
            Err(_) => {
                self.current_match = None;
            }
        }
    }

    /// Drops the preview when the cmdline doesn't parse as a
    /// substitute, when the pattern is empty, or when regex
    /// compilation fails. Cleared explicitly by CommandLineCancel
    /// and by execute_ex_line on submit.
    pub(super) fn refresh_substitute_preview(&mut self) {
        let parsed = match crate::excommand::try_parse_substitute_partial(&self.command_line) {
            Some(p) => p,
            None => {
                self.substitute_preview = None;
                return;
            }
        };
        if parsed.pattern.is_empty() {
            self.substitute_preview = None;
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

        let buffer = self.document.snapshot().buffer.clone();
        let mut matches: Vec<ProtoRange> = Vec::new();
        match parsed.scope {
            crate::excommand::SubstitutePartialScope::CurrentLine => {
                self.collect_substitute_matches_for_line(
                    &buffer,
                    &regex,
                    self.cursor.line,
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

        self.substitute_preview = Some(SubstitutePreview {
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

    /// Vim's `:s/pattern/replacement/[g]` (and `:%s/...` for whole-buffer
    /// scope). Replacement template syntax follows fancy-regex /
    /// `regex` crate: `$1`, `${name}`, `$0` (whole match), `$$` for a
    /// literal `$`. NOT vim's `\1`/`&` -- modern syntax. Returns count
    /// of replacements via the echo area.
    pub(super) fn do_substitute(
        &mut self,
        scope: lattice_grammar::SubstituteScope,
        pattern: &str,
        replacement: &str,
        global: bool,
    ) {
        if pattern.is_empty() {
            self.set_message(EchoLevel::Error, "empty pattern".to_string());
            return;
        }
        // Compile once. Surface compile errors to the user.
        let regex = match compile_search_pattern(pattern) {
            Ok(r) => r,
            Err(msg) => {
                self.set_message(EchoLevel::Error, format!("regex: {msg}"));
                return;
            }
        };
        // Determine the line range.
        let (first_line, last_line) = match scope {
            lattice_grammar::SubstituteScope::CurrentLine => (self.cursor.line, self.cursor.line),
            lattice_grammar::SubstituteScope::Whole => {
                let last = last_addressable_line(&self.document.snapshot().buffer);
                (0, last)
            }
        };
        let mut total = 0usize;
        // Apply per line, top-down. fancy-regex's `replace_all` /
        // `replace` does the heavy lifting: SIMD literal prefilter
        // for backref-free patterns, NFA fallback when needed,
        // template substitution with $1/${name}.
        for line in first_line..=last_line {
            let line_text = self
                .document
                .snapshot()
                .buffer
                .line(line)
                .unwrap_or_default();
            let new_line = if global {
                regex.replace_all(&line_text, replacement)
            } else {
                regex.replace(&line_text, replacement)
            };
            // If nothing changed on this line, skip the edit.
            if new_line == line_text {
                continue;
            }
            // Count substitutions: cheap to tally via find_iter.
            let count_on_line = if global {
                let mut c = 0usize;
                for m in regex.find_iter(&line_text) {
                    if m.is_ok() {
                        c += 1;
                    }
                }
                c
            } else {
                1
            };
            let line_len = line_text.len() as u32;
            let r = ProtoRange::new(Position::new(line, 0), Position::new(line, line_len));
            let _ = self.apply_edit_blocking(Edit::replace(r, new_line.into_owned()));
            total += count_on_line;
        }
        if total == 0 {
            self.set_message(
                EchoLevel::Error,
                format!("E486: Pattern not found: {pattern}"),
            );
        } else {
            self.set_message(
                EchoLevel::Info,
                format!("{total} substitution{}", if total == 1 { "" } else { "s" }),
            );
        }
    }

    /// Vim's `;` / `,`: repeat the last f/F/t/T find on the current
    /// line. `reverse = false` keeps the original direction; `true`
    /// flips it.
    pub(super) fn do_find_repeat(&mut self, reverse: bool) {
        let Some(last) = self.last_find else {
            self.set_message(EchoLevel::Error, "no previous find".to_string());
            return;
        };
        let kind = if reverse {
            match last.kind {
                FindKind::Forward => FindKind::Backward,
                FindKind::Backward => FindKind::Forward,
                FindKind::TillForward => FindKind::TillBackward,
                FindKind::TillBackward => FindKind::TillForward,
            }
        } else {
            last.kind
        };
        let motion_id = match kind {
            FindKind::Forward => self.builtins.find_char_forward,
            FindKind::Backward => self.builtins.find_char_backward,
            FindKind::TillForward => self.builtins.till_char_forward,
            FindKind::TillBackward => self.builtins.till_char_backward,
        };
        // Don't update last_find on repeat -- the original direction
        // sticks (vim semantics: ; preserves direction even after ,).
        let inv =
            CommandInvocation::of(motion_id.0).with_args(lattice_grammar::Args::Char(last.target));
        // Bypass run_invocation's last_find recording by dispatching
        // directly. We still want the standard pending/count consumption.
        self.run_invocation(inv);
    }

    /// Vim's `*` / `#`: extract the word at the cursor, store it as
    /// `last_search`, and jump to the next (or previous) occurrence.
    /// Skips the current match by stepping one byte beyond it before
    /// invoking the search engine.
    pub(super) fn do_search_word_under_cursor(&mut self, direction: SearchDirection) {
        let pre_jump = self.cursor;
        let text = self.document.text();
        let bytes = text.as_bytes();
        let cursor_byte = match self
            .document
            .snapshot()
            .buffer
            .position_to_byte(self.cursor)
        {
            Ok(b) => b,
            Err(_) => return,
        };
        // Find the word boundaries at cursor; if cursor isn't on a word
        // byte, scan forward to the next word on the same line.
        let mut start = cursor_byte;
        if start >= bytes.len() || !is_word_char_byte(bytes[start]) {
            // Scan forward up to end-of-line for a word byte.
            while start < bytes.len() && bytes[start] != b'\n' && !is_word_char_byte(bytes[start]) {
                start += 1;
            }
            if start >= bytes.len() || bytes[start] == b'\n' {
                self.set_message(EchoLevel::Error, "no word under cursor".to_string());
                return;
            }
        }
        // Walk back to start of word.
        while start > 0 && is_word_char_byte(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = start;
        while end < bytes.len() && is_word_char_byte(bytes[end]) {
            end += 1;
        }
        let word = String::from_utf8_lossy(&bytes[start..end]).into_owned();
        if word.is_empty() {
            self.set_message(EchoLevel::Error, "no word under cursor".to_string());
            return;
        }
        let dir = match direction {
            SearchDirection::Forward => lattice_core::search::Direction::Forward,
            SearchDirection::Backward => lattice_core::search::Direction::Backward,
        };
        // Skip the current match: search from one byte past for forward,
        // one byte before for backward.
        let from = step_byte(&self.document.snapshot().buffer, self.cursor, direction);
        // The word is a literal we want to find verbatim, not a
        // pattern. Escape regex metachars before compiling so words
        // containing `.`, `*`, `(` etc. don't trigger metacharacter
        // semantics. (vim's `*` also adds `\<...\>` word-boundary
        // anchors -- if we want that later, change this to
        // `\b{escaped}\b`.)
        let escaped = fancy_regex::escape(&word).into_owned();
        let regex = match compile_search_pattern(&escaped) {
            Ok(r) => r,
            Err(_) => {
                self.set_message(EchoLevel::Error, "regex compile failed".to_string());
                return;
            }
        };
        match lattice_core::search::find(
            &self.document.snapshot().buffer,
            &regex,
            from,
            dir,
            &CancellationToken::never(),
        ) {
            Ok(Some(hit)) => {
                self.push_position_history(pre_jump, PositionSource::AutoJump);
                self.cursor = hit.range.start;
                self.current_match = Some(hit.range);
                self.all_matches = lattice_core::search::find_all(
                    &self.document.snapshot().buffer,
                    &regex,
                    &CancellationToken::never(),
                )
                .unwrap_or_default();
                if hit.wrapped {
                    let text = match direction {
                        SearchDirection::Forward => "search hit BOTTOM, continuing at TOP",
                        SearchDirection::Backward => "search hit TOP, continuing at BOTTOM",
                    };
                    self.set_message(EchoLevel::Warn, text.to_string());
                }
            }
            Ok(None) => {
                self.current_match = None;
                self.all_matches.clear();
                self.set_message(EchoLevel::Error, format!("E486: Pattern not found: {word}"));
            }
            Err(_) => {
                self.current_match = None;
                self.all_matches.clear();
            }
        }
        self.last_search = Some(LastSearch {
            pattern: word,
            direction,
        });
    }
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

/// One byte forward or backward, wrapping across newlines. Caller for
/// search-repeat: skip the current match by advancing one byte before
/// calling the engine. At buffer extremes we return the original
/// position; the engine then handles wrap.
fn step_byte(buf: &Buffer, p: Position, dir: SearchDirection) -> Position {
    match dir {
        SearchDirection::Forward => {
            let len = line_byte_len(buf, p.line);
            if p.byte < len {
                Position::new(p.line, p.byte + 1)
            } else {
                let last = last_addressable_line(buf);
                if p.line < last {
                    Position::new(p.line + 1, 0)
                } else {
                    p
                }
            }
        }
        SearchDirection::Backward => previous_position(buf, p),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use crate::app::*;
    use crate::app::test_helpers::{app_with, install_help, submit_ex};
    use crate::help::HelpBuffer;
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
        a.cursor = Position::new(0, 0);
        type_cmdline(&mut a, "s/foo/X");
        let preview = a.substitute_preview.as_ref().expect("preview live");
        assert_eq!(preview.matches.len(), 1, "only the leftmost match -- no /g");
        assert_eq!(preview.matches[0].start, Position::new(0, 0));
        assert_eq!(preview.replacement.as_deref(), Some("X"));
        assert!(!preview.global);
    }

    #[test]
    fn substitute_preview_with_g_flag_highlights_every_match_on_line() {
        let mut a = app_with("foo bar foo baz foo\nfoo elsewhere", 10);
        a.cursor = Position::new(0, 0);
        type_cmdline(&mut a, "s/foo/X/g");
        let preview = a.substitute_preview.as_ref().unwrap();
        // Three matches on the cursor's line; line 1 is out of scope.
        assert_eq!(preview.matches.len(), 3);
        assert!(preview.global);
    }

    #[test]
    fn substitute_preview_percent_scope_walks_whole_buffer() {
        let mut a = app_with("foo\nbar foo\nfoo", 10);
        a.cursor = Position::new(0, 0);
        type_cmdline(&mut a, "%s/foo/X/g");
        let preview = a.substitute_preview.as_ref().unwrap();
        // Three matches across three lines.
        assert_eq!(preview.matches.len(), 3);
    }

    #[test]
    fn substitute_preview_clears_on_cmdline_cancel() {
        let mut a = app_with("foo bar", 10);
        type_cmdline(&mut a, "s/foo/X");
        assert!(a.substitute_preview.is_some());
        a.apply(Action::CommandLineCancel);
        assert!(a.substitute_preview.is_none());
    }

    #[test]
    fn substitute_preview_clears_on_cmdline_submit() {
        let mut a = app_with("foo bar", 10);
        type_cmdline(&mut a, "s/foo/X");
        assert!(a.substitute_preview.is_some());
        a.apply(Action::CommandLineSubmit);
        assert!(a.substitute_preview.is_none());
    }

    #[test]
    fn substitute_preview_dropped_when_input_no_longer_parses_as_substitute() {
        let mut a = app_with("foo bar", 10);
        // Enter a substitute, get preview, then backspace past `s` --
        // input is no longer a substitute.
        type_cmdline(&mut a, "s/foo");
        assert!(a.substitute_preview.is_some());
        for _ in 0.."s/foo".len() {
            a.apply(Action::CommandLineBackspace);
        }
        assert!(a.substitute_preview.is_none());
    }

    #[test]
    fn substitute_preview_empty_pattern_drops_preview() {
        // After typing `s/` the pattern is empty -- preview shouldn't
        // highlight anything (no matches to show).
        let mut a = app_with("foo bar", 10);
        type_cmdline(&mut a, "s/");
        assert!(a.substitute_preview.is_none());
    }

    // ---- Search basic (/, ?, n, N) ----

    #[test]
    fn enter_search_seeds_state() {
        let mut a = app_with("hello world", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        assert_eq!(a.modal, ModalState::Search(SearchDirection::Forward));
        let line = a.search_line.as_ref().expect("search_line populated");
        assert_eq!(line.pattern, "");
        assert_eq!(line.origin, Position::ZERO);
    }

    #[test]
    fn search_append_grows_pattern_and_previews_match() {
        let mut a = app_with("foo bar foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "bar");
        let line = a.search_line.as_ref().unwrap();
        assert_eq!(line.pattern, "bar");
        // Preview should highlight the first match without moving cursor.
        let m = a.current_match.expect("match previewed");
        assert_eq!(m.start, Position::new(0, 4));
        assert_eq!(a.cursor, Position::ZERO);
    }

    #[test]
    fn search_backspace_shrinks_pattern_and_re_previews() {
        let mut a = app_with("foo bar baz", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "baz");
        a.apply(Action::SearchBackspace);
        assert_eq!(a.search_line.as_ref().unwrap().pattern, "ba");
        let m = a.current_match.expect("preview after backspace");
        assert_eq!(m.start, Position::new(0, 4));
    }

    #[test]
    fn search_backspace_on_empty_pattern_exits_search() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        a.apply(Action::SearchBackspace);
        assert_eq!(a.modal, ModalState::Normal);
        assert!(a.search_line.is_none());
    }

    #[test]
    fn search_submit_jumps_cursor_to_match_and_records_last_search() {
        let mut a = app_with("foo bar foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "bar");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.modal, ModalState::Normal);
        assert_eq!(a.cursor, Position::new(0, 4));
        assert!(a.search_line.is_none());
        let last = a.last_search.as_ref().unwrap();
        assert_eq!(last.pattern, "bar");
        assert_eq!(last.direction, SearchDirection::Forward);
    }

    #[test]
    fn search_submit_with_no_match_records_pattern_and_warns() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "xyz");
        a.apply(Action::SearchSubmit);
        assert!(a.current_match.is_none());
        assert_eq!(a.last_search.as_ref().unwrap().pattern, "xyz");
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("Pattern not found"));
    }

    #[test]
    fn search_cancel_restores_cursor_to_origin() {
        let mut a = app_with("foo bar foo", 10);
        a.cursor = Position::new(0, 5);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        // Preview should have set current_match to "foo" at byte 8.
        assert_eq!(a.current_match.unwrap().start, Position::new(0, 8));
        a.apply(Action::SearchCancel);
        assert_eq!(a.modal, ModalState::Normal);
        assert_eq!(a.cursor, Position::new(0, 5));
        assert!(a.current_match.is_none());
    }

    #[test]
    fn n_after_forward_search_advances_to_next_match() {
        let mut a = app_with("foo bar foo bar", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.cursor, Position::new(0, 0));
        a.apply(Action::SearchNext);
        assert_eq!(a.cursor, Position::new(0, 8));
    }

    #[test]
    fn capital_n_reverses_direction() {
        let mut a = app_with("foo bar foo bar", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        a.apply(Action::SearchSubmit);
        a.apply(Action::SearchNext);
        assert_eq!(a.cursor, Position::new(0, 8));
        a.apply(Action::SearchPrevious);
        assert_eq!(a.cursor, Position::new(0, 0));
    }

    #[test]
    fn n_with_no_last_search_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::SearchNext);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("no previous"));
    }

    #[test]
    fn search_forward_wraps_and_warns() {
        let mut a = app_with("alpha beta gamma alpha", 10);
        a.cursor = Position::new(0, 17); // past the second "alpha"... actually at it
        // Move past it for clarity.
        a.cursor = Position::new(0, 18);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "alpha");
        a.apply(Action::SearchSubmit);
        // First "alpha" is at byte 0; we wrapped from byte 18.
        assert_eq!(a.cursor, Position::new(0, 0));
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Warn);
        assert!(msg.text.contains("BOTTOM"));
    }

    #[test]
    fn search_backward_finds_previous_match() {
        let mut a = app_with("alpha beta gamma alpha", 10);
        a.cursor = Position::new(0, 22);
        a.apply(Action::EnterSearch(SearchDirection::Backward));
        type_pattern(&mut a, "alpha");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.cursor, Position::new(0, 17));
    }

    // ---- :noh ----

    #[test]
    fn nohlsearch_clears_overlay() {
        let mut a = app_with("foo bar foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        a.apply(Action::SearchSubmit);
        assert!(!a.all_matches.is_empty());
        submit_ex(&mut a, "noh");
        assert!(a.all_matches.is_empty());
        assert!(a.current_match.is_none());
    }

    // ---- Substitute (:s/foo/bar/[g]) ----

    #[test]
    fn substitute_first_match_on_current_line() {
        let mut a = app_with("foo bar foo", 10);
        submit_ex(&mut a, "s/foo/baz/");
        assert_eq!(a.document.text(), "baz bar foo");
    }

    #[test]
    fn substitute_global_replaces_all_on_line() {
        let mut a = app_with("foo bar foo", 10);
        submit_ex(&mut a, "s/foo/baz/g");
        assert_eq!(a.document.text(), "baz bar baz");
    }

    #[test]
    fn substitute_whole_buffer_with_g_flag() {
        let mut a = app_with("foo\nbar foo\nfoo", 10);
        submit_ex(&mut a, "%s/foo/X/g");
        assert_eq!(a.document.text(), "X\nbar X\nX");
    }

    #[test]
    fn substitute_no_match_emits_error() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "s/xyz/abc/");
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("Pattern not found"));
        assert_eq!(a.document.text(), "hello");
    }

    #[test]
    fn substitute_empty_replacement_deletes_pattern() {
        let mut a = app_with("hello world hello", 10);
        submit_ex(&mut a, "s/hello //g");
        assert_eq!(a.document.text(), "world hello");
    }

    #[test]
    fn substitute_count_message() {
        let mut a = app_with("foo foo foo", 10);
        submit_ex(&mut a, "s/foo/X/g");
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Info);
        assert!(msg.text.contains("3"));
    }

    #[test]
    fn substitute_only_current_line_without_percent() {
        let mut a = app_with("foo\nfoo\nfoo", 10);
        a.cursor = Position::new(1, 0);
        submit_ex(&mut a, "s/foo/X/");
        assert_eq!(a.document.text(), "foo\nX\nfoo");
    }

    // ---- Find-repeat (; / ,) ----

    #[test]
    fn semicolon_repeats_last_find_forward() {
        let mut a = app_with("hello world", 10);
        // First f-find for 'l': cursor moves to byte 2.
        let inv = lattice_grammar::CommandInvocation::of(a.builtins.find_char_forward.0)
            .with_args(lattice_grammar::Args::Char('l'));
        a.apply(Action::Invoke(inv));
        assert_eq!(a.cursor, Position::new(0, 2));
        // `;` repeats: byte 3.
        a.apply(Action::FindRepeat { reverse: false });
        assert_eq!(a.cursor, Position::new(0, 3));
    }

    #[test]
    fn comma_reverses_last_find_direction() {
        let mut a = app_with("hello world", 10);
        // f l forward.
        let inv = lattice_grammar::CommandInvocation::of(a.builtins.find_char_forward.0)
            .with_args(lattice_grammar::Args::Char('l'));
        a.apply(Action::Invoke(inv));
        assert_eq!(a.cursor, Position::new(0, 2));
        // f l again, then `,` should reverse to find the previous 'l'.
        a.apply(Action::FindRepeat { reverse: false });
        assert_eq!(a.cursor, Position::new(0, 3));
        a.apply(Action::FindRepeat { reverse: true });
        assert_eq!(a.cursor, Position::new(0, 2));
    }

    #[test]
    fn find_repeat_with_no_history_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::FindRepeat { reverse: false });
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    // ---- Search submit + position history ----

    #[test]
    fn search_submit_pushes_position_history() {
        let mut a = app_with("foo bar baz foo", 10);
        a.cursor = Position::new(0, 8); // on 'b' of "baz"
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        for c in "foo".chars() {
            a.apply(Action::SearchAppend(c));
        }
        a.apply(Action::SearchSubmit);
        // Cursor jumped to second "foo" at byte 12.
        assert_eq!(a.cursor, Position::new(0, 12));
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.cursor, Position::new(0, 8));
    }

    // ---- Search auto-opens fold ----

    #[test]
    fn search_into_closed_fold_auto_opens_it() {
        // `docs/help/folding.md`: search hits open the fold the
        // cursor lands in.
        let initial = "# H1\nbody one needle\nbody two\n# H2\nafter\n";
        let mut a = app_with(initial, 10);
        a.set_foldmethod_for_test(FoldMethod::Markdown);
        a.recompute_folds();
        let idx = a
            .folds
            .iter()
            .position(|f| f.start_line == 0)
            .expect("H1 fold");
        a.folds[idx].closed = true;
        // Submit a forward search from the top of the buffer.
        a.search_line = Some(SearchLine {
            origin: Position::ZERO,
            pattern: "needle".into(),
            direction: SearchDirection::Forward,
        });
        a.modal = ModalState::Search(SearchDirection::Forward);
        a.apply(Action::SearchSubmit);
        // The fold containing `body one` should now be open.
        let fold = a
            .folds
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
        // `active_text()` and `self.cursor`; on a hit it writes
        // `self.cursor` -- exactly the document path.
        let mut a = app_with("xx", 10);
        let body: Vec<String> = vec![
            "alpha".into(),
            "beta".into(),
            "gamma needle".into(),
            "delta".into(),
        ];
        install_help(&mut a, HelpBuffer::from_lines("search-test", body));
        // Open `/` and type `needle` then submit.
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        for c in "needle".chars() {
            a.apply(Action::SearchAppend(c));
        }
        a.apply(Action::SearchSubmit);
        // Cursor should land on line 2 (gamma needle).
        assert_eq!(a.cursor.line, 2, "cursor jumped to the help line");
        // Active buffer stays Help -- search didn't leak into the
        // document.
        assert!(matches!(a.active_buffer, BufferKind::Help));
    }

    #[test]
    fn search_in_help_buffer_populates_all_matches_for_hlsearch() {
        // The renderer paints `app.all_matches` as styled overlays
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
        install_help(&mut a, HelpBuffer::from_lines("hl-test", body));
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        for c in "needle".chars() {
            a.apply(Action::SearchAppend(c));
        }
        a.apply(Action::SearchSubmit);
        assert_eq!(
            a.all_matches.len(),
            3,
            "every occurrence in the help body should be in all_matches"
        );
        assert!(a.current_match.is_some());
    }

    #[test]
    fn search_in_help_buffer_no_longer_blocked_by_read_only_guard() {
        // Regression: `EnterSearch` etc. used to be in the
        // `action_is_document_mutation` allow-list, so `/` in a
        // help buffer echoed "buffer is read-only". They're not
        // mutations -- the guard list now only covers true edits.
        let mut a = app_with("xx", 10);
        install_help(&mut a, HelpBuffer::from_lines("ro", vec!["abc".into(); 5]));
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        // Should be in search modal, not Normal with a read-only
        // echo.
        assert!(
            matches!(a.modal, ModalState::Search(_)),
            "should be in Search modal, got {:?}",
            a.modal
        );
        assert!(
            a.last_message.is_none(),
            "no read-only echo expected, got {:?}",
            a.last_message
        );
    }

    // ---- Search hlsearch / all_matches ----

    #[test]
    fn search_preview_populates_all_matches() {
        let mut a = app_with("foo bar foo baz foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        assert_eq!(a.all_matches.len(), 3);
    }

    #[test]
    fn search_submit_keeps_all_matches_for_hlsearch() {
        let mut a = app_with("foo bar foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.all_matches.len(), 2);
    }

    #[test]
    fn search_cancel_clears_all_matches() {
        let mut a = app_with("foo bar foo", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        assert!(!a.all_matches.is_empty());
        a.apply(Action::SearchCancel);
        assert!(a.all_matches.is_empty());
    }

    #[test]
    fn search_word_under_cursor_populates_all_matches() {
        let mut a = app_with("foo bar foo bar foo", 10);
        a.cursor = Position::new(0, 1); // on first "foo"
        a.apply(Action::SearchWordUnderCursor(SearchDirection::Forward));
        assert_eq!(a.all_matches.len(), 3);
    }

    #[test]
    fn search_works_across_lines() {
        let mut a = app_with("foo\nbar\nfoo\nbaz", 10);
        a.apply(Action::EnterSearch(SearchDirection::Forward));
        type_pattern(&mut a, "foo");
        a.apply(Action::SearchSubmit);
        assert_eq!(a.cursor, Position::new(0, 0));
        a.apply(Action::SearchNext);
        assert_eq!(a.cursor, Position::new(2, 0));
    }
}
