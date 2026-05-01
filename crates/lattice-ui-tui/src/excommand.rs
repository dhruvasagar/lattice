//! Phase 2 ex-command parser.
//!
//! This is the *minimal* string-to-typed-call parser for the `:` minibuffer.
//! Per DESIGN.md §5.2.1 the long-term shape is one `CommandRegistry` with
//! an ex-command parser front-end producing `CommandInvocation`s; we'll
//! migrate to that once enough of the registry is populated. For Phase 2 we
//! parse a small typed enum covering save / quit, which is enough to make
//! the editor actually useful.
//!
//! Supported:
//! - `:w`               write to current path
//! - `:w <path>`        write to the named path (and remember it)
//! - `:q`               quit (refuses if dirty)
//! - `:q!`              quit even if dirty
//! - `:wq`, `:x`        write then quit
//! - `:wq!`, `:x!`      write then quit (force-quit if write fails)

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExCommand {
    Write { path: Option<PathBuf> },
    Quit { force: bool },
    WriteQuit { force: bool },
    Substitute {
        scope: SubstituteScope,
        pattern: String,
        replacement: String,
        global: bool,
    },
    /// Vim's `:g/pat/cmd` (and `:v/pat/cmd`). Runs `body` on every line
    /// matching `pattern` (or NOT matching, when `inverted = true`).
    Global {
        pattern: String,
        inverted: bool,
        body: String,
    },
    /// Vim's `:d` -- delete the current line (CurrentLine range).
    DeleteLine,
    /// Vim's `:noh[lsearch]` -- clear the hlsearch overlay.
    NoHlSearch,
    /// Vim's `:reg[isters]` -- show register contents.
    ListRegisters,
    /// Vim's `:marks` -- show all set marks.
    ListMarks,
    /// Vim's `:set <option>` -- v1 only handles a small fixed set:
    /// number / nonumber / nu / nonu, relativenumber / norelativenumber /
    /// rnu / nornu.
    Set { option: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubstituteScope {
    CurrentLine,
    Whole,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExCommandError {
    #[error("empty command")]
    Empty,
    #[error("unknown command: {0}")]
    Unknown(String),
    #[error("trailing characters after command")]
    TrailingArgs,
    #[error("malformed substitute: {0}")]
    BadSubstitute(&'static str),
}

pub fn parse(line: &str) -> Result<ExCommand, ExCommandError> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(ExCommandError::Empty);
    }

    // Substitute has a delimiter-based syntax that breaks the
    // whitespace-split-then-keyword model. Handle it inline:
    // `[%]s/pattern/replacement/[flags]`.
    if let Some(sub) = parse_substitute(trimmed)? {
        return Ok(sub);
    }
    // :g and :v have similar `/pattern/body` shape.
    if let Some(gcmd) = parse_global(trimmed)? {
        return Ok(gcmd);
    }

    // Split into command word and rest. The command word may end in `!`.
    let (cmd, rest) = match trimmed.find(char::is_whitespace) {
        Some(i) => (&trimmed[..i], trimmed[i..].trim()),
        None => (trimmed, ""),
    };

    match cmd {
        "w" | "write" => Ok(ExCommand::Write { path: parse_optional_path(rest) }),
        "q" | "quit" => no_args(rest)?.then_some(ExCommand::Quit { force: false })
            .ok_or(ExCommandError::TrailingArgs),
        "q!" | "quit!" => no_args(rest)?.then_some(ExCommand::Quit { force: true })
            .ok_or(ExCommandError::TrailingArgs),
        "wq" | "x" => no_args(rest)?.then_some(ExCommand::WriteQuit { force: false })
            .ok_or(ExCommandError::TrailingArgs),
        "wq!" | "x!" => no_args(rest)?.then_some(ExCommand::WriteQuit { force: true })
            .ok_or(ExCommandError::TrailingArgs),
        "d" | "delete" => no_args(rest)?
            .then_some(ExCommand::DeleteLine)
            .ok_or(ExCommandError::TrailingArgs),
        "noh" | "nohl" | "nohlsearch" => no_args(rest)?
            .then_some(ExCommand::NoHlSearch)
            .ok_or(ExCommandError::TrailingArgs),
        "reg" | "registers" => no_args(rest)?
            .then_some(ExCommand::ListRegisters)
            .ok_or(ExCommandError::TrailingArgs),
        "marks" => no_args(rest)?
            .then_some(ExCommand::ListMarks)
            .ok_or(ExCommandError::TrailingArgs),
        "set" => {
            if rest.is_empty() {
                return Err(ExCommandError::Unknown("set requires an option".into()));
            }
            Ok(ExCommand::Set {
                option: rest.to_string(),
            })
        }
        other => Err(ExCommandError::Unknown(other.to_string())),
    }
}

/// Parse vim's :g and :v: `g/pattern/body` and `v/pattern/body`.
fn parse_global(input: &str) -> Result<Option<ExCommand>, ExCommandError> {
    let (inverted, rest) = if let Some(rest) = input.strip_prefix("g/") {
        (false, rest)
    } else if let Some(rest) = input.strip_prefix("v/") {
        (true, rest)
    } else {
        return Ok(None);
    };
    // Walk forward to the next unescaped `/`.
    let mut pattern = String::new();
    let mut chars = rest.chars().peekable();
    let mut found_delim = false;
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                pattern.push(next);
            }
            continue;
        }
        if c == '/' {
            found_delim = true;
            break;
        }
        pattern.push(c);
    }
    if !found_delim {
        return Err(ExCommandError::BadSubstitute(
            "missing closing `/` after pattern",
        ));
    }
    if pattern.is_empty() {
        return Err(ExCommandError::BadSubstitute("empty pattern"));
    }
    let body: String = chars.collect();
    Ok(Some(ExCommand::Global {
        pattern,
        inverted,
        body,
    }))
}

/// Parse vim's substitute syntax: `[%]s/pattern/replacement/[flags]`.
/// Returns `Ok(Some)` on a successful s-form match, `Ok(None)` if the
/// input doesn't look like substitute at all, `Err(BadSubstitute)` on a
/// malformed s-form.
fn parse_substitute(input: &str) -> Result<Option<ExCommand>, ExCommandError> {
    // Detect `%s/.../.../...` and `s/.../.../...`.
    let (scope, body) = if let Some(rest) = input.strip_prefix("%s/") {
        (SubstituteScope::Whole, rest)
    } else if let Some(rest) = input.strip_prefix("s/") {
        (SubstituteScope::CurrentLine, rest)
    } else {
        return Ok(None);
    };

    // Walk `body` character-by-character respecting backslash-escapes.
    // Vim's substitute uses `\/` as an escape-for-delimiter; we accept
    // `\/` as a literal `/` in either pattern or replacement.
    let mut pattern = String::new();
    let mut replacement = String::new();
    let mut flags = String::new();
    let mut state = 0u8; // 0 = pattern, 1 = replacement, 2 = flags
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            // Take the next char literally (escape).
            if let Some(next) = chars.next() {
                let target = match state {
                    0 => &mut pattern,
                    1 => &mut replacement,
                    _ => &mut flags,
                };
                target.push(next);
            }
            continue;
        }
        if c == '/' {
            state += 1;
            if state > 2 {
                return Err(ExCommandError::BadSubstitute("too many `/` separators"));
            }
            continue;
        }
        let target = match state {
            0 => &mut pattern,
            1 => &mut replacement,
            _ => &mut flags,
        };
        target.push(c);
    }

    if pattern.is_empty() {
        return Err(ExCommandError::BadSubstitute("empty pattern"));
    }
    // 'g' is the only flag honored in v1; 'i', 'c', etc. are accepted
    // but ignored.
    let global = flags.contains('g');
    Ok(Some(ExCommand::Substitute {
        scope,
        pattern,
        replacement,
        global,
    }))
}

fn parse_optional_path(rest: &str) -> Option<PathBuf> {
    if rest.is_empty() {
        None
    } else {
        Some(PathBuf::from(rest))
    }
}

fn no_args(rest: &str) -> Result<bool, ExCommandError> {
    if rest.is_empty() {
        Ok(true)
    } else {
        Err(ExCommandError::TrailingArgs)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn empty_input_is_empty_error() {
        assert_eq!(parse(""), Err(ExCommandError::Empty));
        assert_eq!(parse("   "), Err(ExCommandError::Empty));
    }

    #[test]
    fn write_without_path() {
        assert_eq!(parse("w"), Ok(ExCommand::Write { path: None }));
        assert_eq!(parse("write"), Ok(ExCommand::Write { path: None }));
    }

    #[test]
    fn write_with_path() {
        assert_eq!(
            parse("w foo.txt"),
            Ok(ExCommand::Write {
                path: Some(PathBuf::from("foo.txt"))
            })
        );
        assert_eq!(
            parse("write   /abs/path.rs"),
            Ok(ExCommand::Write {
                path: Some(PathBuf::from("/abs/path.rs"))
            })
        );
    }

    #[test]
    fn quit_short_and_long_form() {
        assert_eq!(parse("q"), Ok(ExCommand::Quit { force: false }));
        assert_eq!(parse("quit"), Ok(ExCommand::Quit { force: false }));
    }

    #[test]
    fn force_quit() {
        assert_eq!(parse("q!"), Ok(ExCommand::Quit { force: true }));
        assert_eq!(parse("quit!"), Ok(ExCommand::Quit { force: true }));
    }

    #[test]
    fn write_quit_aliases() {
        assert_eq!(parse("wq"), Ok(ExCommand::WriteQuit { force: false }));
        assert_eq!(parse("x"), Ok(ExCommand::WriteQuit { force: false }));
        assert_eq!(parse("wq!"), Ok(ExCommand::WriteQuit { force: true }));
        assert_eq!(parse("x!"), Ok(ExCommand::WriteQuit { force: true }));
    }

    #[test]
    fn unknown_command_reports_name() {
        assert_eq!(
            parse("frobnicate"),
            Err(ExCommandError::Unknown("frobnicate".into()))
        );
    }

    #[test]
    fn trailing_args_on_no_arg_command_is_error() {
        assert_eq!(parse("q please"), Err(ExCommandError::TrailingArgs));
        assert_eq!(parse("wq somefile"), Err(ExCommandError::TrailingArgs));
    }

    #[test]
    fn substitute_current_line_basic() {
        assert_eq!(
            parse("s/foo/bar/"),
            Ok(ExCommand::Substitute {
                scope: SubstituteScope::CurrentLine,
                pattern: "foo".into(),
                replacement: "bar".into(),
                global: false,
            })
        );
    }

    #[test]
    fn substitute_global_flag() {
        assert_eq!(
            parse("s/foo/bar/g"),
            Ok(ExCommand::Substitute {
                scope: SubstituteScope::CurrentLine,
                pattern: "foo".into(),
                replacement: "bar".into(),
                global: true,
            })
        );
    }

    #[test]
    fn substitute_whole_buffer() {
        assert_eq!(
            parse("%s/foo/bar/g"),
            Ok(ExCommand::Substitute {
                scope: SubstituteScope::Whole,
                pattern: "foo".into(),
                replacement: "bar".into(),
                global: true,
            })
        );
    }

    #[test]
    fn substitute_with_escaped_slash_in_pattern() {
        // `\/` -> literal `/` in pattern.
        assert_eq!(
            parse("s/a\\/b/c/"),
            Ok(ExCommand::Substitute {
                scope: SubstituteScope::CurrentLine,
                pattern: "a/b".into(),
                replacement: "c".into(),
                global: false,
            })
        );
    }

    #[test]
    fn substitute_empty_pattern_is_error() {
        assert!(matches!(parse("s//bar/"), Err(ExCommandError::BadSubstitute(_))));
    }

    #[test]
    fn substitute_empty_replacement_is_valid_delete() {
        // Empty replacement -> delete the pattern.
        assert_eq!(
            parse("s/foo//g"),
            Ok(ExCommand::Substitute {
                scope: SubstituteScope::CurrentLine,
                pattern: "foo".into(),
                replacement: String::new(),
                global: true,
            })
        );
    }

    #[test]
    fn substitute_unknown_flag_is_ignored() {
        // 'i' (case-insensitive), 'c' (confirm), etc. are accepted but
        // ignored in v1.
        assert_eq!(
            parse("s/foo/bar/gi"),
            Ok(ExCommand::Substitute {
                scope: SubstituteScope::CurrentLine,
                pattern: "foo".into(),
                replacement: "bar".into(),
                global: true,
            })
        );
    }

    #[test]
    fn global_basic_match_with_delete_body() {
        assert_eq!(
            parse("g/foo/d"),
            Ok(ExCommand::Global {
                pattern: "foo".into(),
                inverted: false,
                body: "d".into(),
            })
        );
    }

    #[test]
    fn vglobal_inverts_match() {
        assert_eq!(
            parse("v/foo/d"),
            Ok(ExCommand::Global {
                pattern: "foo".into(),
                inverted: true,
                body: "d".into(),
            })
        );
    }

    #[test]
    fn global_body_can_be_substitute() {
        assert_eq!(
            parse("g/foo/s/foo/bar/g"),
            Ok(ExCommand::Global {
                pattern: "foo".into(),
                inverted: false,
                body: "s/foo/bar/g".into(),
            })
        );
    }

    #[test]
    fn nohlsearch_aliases() {
        assert_eq!(parse("noh"), Ok(ExCommand::NoHlSearch));
        assert_eq!(parse("nohl"), Ok(ExCommand::NoHlSearch));
        assert_eq!(parse("nohlsearch"), Ok(ExCommand::NoHlSearch));
    }

    #[test]
    fn registers_aliases() {
        assert_eq!(parse("reg"), Ok(ExCommand::ListRegisters));
        assert_eq!(parse("registers"), Ok(ExCommand::ListRegisters));
    }

    #[test]
    fn set_with_option_parses() {
        assert_eq!(
            parse("set number"),
            Ok(ExCommand::Set {
                option: "number".into()
            })
        );
        assert_eq!(
            parse("set nu"),
            Ok(ExCommand::Set {
                option: "nu".into()
            })
        );
    }

    #[test]
    fn set_without_option_errors() {
        assert!(matches!(parse("set"), Err(ExCommandError::Unknown(_))));
    }

    #[test]
    fn marks_command_parses() {
        assert_eq!(parse("marks"), Ok(ExCommand::ListMarks));
    }

    #[test]
    fn delete_short_form() {
        assert_eq!(parse("d"), Ok(ExCommand::DeleteLine));
        assert_eq!(parse("delete"), Ok(ExCommand::DeleteLine));
    }

    #[test]
    fn whitespace_around_command_is_tolerated() {
        assert_eq!(parse("  w  "), Ok(ExCommand::Write { path: None }));
        assert_eq!(
            parse("\t w foo.rs \t"),
            Ok(ExCommand::Write {
                path: Some(PathBuf::from("foo.rs"))
            })
        );
    }
}
