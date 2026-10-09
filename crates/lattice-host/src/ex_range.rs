//! Turning a `:` line's range into lines.
//!
//! The parser ([`lattice_grammar::parse_range_prefix`]) reads `.,+3` or
//! `'<,/end/` into a symbolic [`Range`]. What those addresses *are* depends
//! on things only the editor has — where the cursor is, what the marks and
//! the last selection are, what the buffer says — so they are resolved here,
//! once, at the moment the line is executed, which is when vim evaluates
//! them too. Everything downstream — the operator dispatcher, `:s`, `:g`,
//! `:narrow` — is handed concrete lines and needs none of that.

use lattice_grammar::range::{Range, RangeBound};
use lattice_grammar::{CommandInvocation, RangeEnv, RangeError, resolve_lines};

use crate::editor::Editor;

impl Editor {
    /// Resolve a parsed `:` line's symbolic range against the active buffer.
    ///
    /// A [`Range::Span`] comes back as a span of two absolute lines. A bare
    /// range with no command (`:5`, `:$`, `:/pat/`, `:'a`) was parsed as the
    /// go-to-line motion carrying the range; it comes back as that motion
    /// with a count, the last line of the range, which is where vim puts the
    /// cursor. Any other invocation is returned untouched.
    ///
    /// For a command, an address outside the buffer is E16. For the bare
    /// jump it is pulled back to the nearest line, as `:99999` is in vim.
    pub fn resolve_ex_range(
        &self,
        mut inv: CommandInvocation,
    ) -> Result<CommandInvocation, RangeError> {
        let Some(range @ Range::Span { .. }) = inv.range.clone() else {
            return Ok(inv);
        };
        let snapshot = self.document.snapshot();
        let buffer = &snapshot.buffer;
        let last = buffer.content_line_count().saturating_sub(1);
        let marks = crate::visual_marks::HostMarks {
            named: &self.marks,
            visual: self.visual_marks(),
        };
        // The first matching line strictly after `from` (or before it),
        // wrapping round the buffer and ending on `from` itself — a pattern
        // that matches only the cursor's line still finds it, as in vim.
        let search = |pattern: &str, from: u32, forward: bool| -> Result<Option<u32>, String> {
            let regex = fancy_regex::Regex::new(pattern).map_err(|e| e.to_string())?;
            let count = last + 1;
            Ok((1..=count)
                .map(|step| {
                    if forward {
                        (from + step) % count
                    } else {
                        (from + count - (step % count)) % count
                    }
                })
                .find(|line| {
                    buffer
                        .line(*line)
                        .is_some_and(|text| regex.is_match(&text).unwrap_or(false))
                }))
        };
        let env = RangeEnv {
            cursor: self.active_cursor().line.min(last),
            last,
            marks: &marks,
            search: &search,
        };
        let is_jump = inv.command == self.builtins.goto_last_line.0;
        let Some((first, end)) = resolve_lines(&range, &env, !is_jump)? else {
            return Ok(inv);
        };
        if is_jump {
            inv.range = None;
            inv.count = Some(lattice_grammar::command::Count(end + 1));
        } else {
            inv.range = Some(Range::Span {
                start: RangeBound::Line(first),
                end: RangeBound::Line(end),
            });
        }
        Ok(inv)
    }
}
