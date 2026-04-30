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
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExCommandError {
    #[error("empty command")]
    Empty,
    #[error("unknown command: {0}")]
    Unknown(String),
    #[error("trailing characters after command")]
    TrailingArgs,
}

pub fn parse(line: &str) -> Result<ExCommand, ExCommandError> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(ExCommandError::Empty);
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
        other => Err(ExCommandError::Unknown(other.to_string())),
    }
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
