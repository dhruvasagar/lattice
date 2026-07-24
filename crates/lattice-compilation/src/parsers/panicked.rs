use std::path::PathBuf;
use std::sync::OnceLock;

use fancy_regex::Regex;
use lattice_protocol::error_list::{ErrorEntry, ErrorSeverity};

use super::{compiled, parse_1based};
use crate::parser::CompilationParser;

/// Pattern: `thread '<name>' panicked at path:line:col[: message]`
/// Captures path (1), line (2), column (3), optional message (4).
///
/// Rust test failures and `panic!()` output both use this format:
/// ```text
/// thread 'tests::it_works' panicked at src/lib.rs:20:9:
/// thread 'main' panicked at src/main.rs:10:18: assertion failed
/// ```

/// `thread '<name>' panicked at path:line:col[: message]`
const PANIC_PATTERN: &str = r"^thread '[^']+' panicked at (\S+):(\d+):(\d+)(?::\s?(.*))?$";

fn panic_re() -> Option<&'static Regex> {
    static CELL: OnceLock<Option<Regex>> = OnceLock::new();
    compiled(&CELL, PANIC_PATTERN)
}

pub(crate) fn match_location(line: &str) -> Option<(PathBuf, u32, u32)> {
    let re = panic_re()?;
    let caps = re.captures(line).ok()??;
    let (path, l, c) = (caps.get(1)?, caps.get(2)?, caps.get(3)?);
    Some((
        PathBuf::from(path.as_str()),
        parse_1based(l.as_str())?,
        parse_1based(c.as_str())?,
    ))
}

pub(crate) fn match_severity(line: &str) -> Option<ErrorSeverity> {
    let re = panic_re()?;
    re.is_match(line).ok().and_then(|m| {
        if m { Some(ErrorSeverity::Error) } else { None }
    })
}

pub struct TestPanicParser;

impl TestPanicParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TestPanicParser {
    fn default() -> Self {
        Self::new()
    }
}

impl CompilationParser for TestPanicParser {
    fn feed(&mut self, line: &str) -> Vec<ErrorEntry> {
        let Some(re) = panic_re() else {
            return Vec::new();
        };
        let Ok(Some(caps)) = re.captures(line) else {
            return Vec::new();
        };
        let (Some(path), Some(l), Some(c)) = (caps.get(1), caps.get(2), caps.get(3)) else {
            return Vec::new();
        };
        let message = caps
            .get(4)
            .map(|m| m.as_str().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "test failure".to_string());

        match (parse_1based(l.as_str()), parse_1based(c.as_str())) {
            (Some(line0), Some(col0)) => vec![ErrorEntry {
                path: PathBuf::from(path.as_str()),
                line: line0,
                col: col0,
                severity: ErrorSeverity::Error,
                message,
            }],
            _ => {
                tracing::debug!(
                    diagnostic = line,
                    "panic parser: unparseable line/col; skipping"
                );
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn test_failure_parses_path_line_col() {
        let mut parser = TestPanicParser::new();
        let entries = parser.feed("thread 'tests::it_works' panicked at src/lib.rs:20:9:");
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.path, PathBuf::from("src/lib.rs"));
        assert_eq!(e.line, 19, "1-based 20 → 0-based 19");
        assert_eq!(e.col, 8, "1-based 9 → 0-based 8");
        assert_eq!(e.severity, ErrorSeverity::Error);
    }

    #[test]
    fn test_failure_with_message() {
        let mut parser = TestPanicParser::new();
        let entries = parser.feed(
            "thread 'tests::it_works' panicked at src/main.rs:10:18: assertion `left == right` failed",
        );
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.path, PathBuf::from("src/main.rs"));
        assert_eq!(e.line, 9);
        assert_eq!(e.col, 17);
        assert!(e.message.contains("assertion"));
    }

    #[test]
    fn test_main_thread_panic() {
        let mut parser = TestPanicParser::new();
        let entries =
            parser.feed("thread 'main' panicked at src/bin/foo.rs:42:5: something went wrong");
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.path, PathBuf::from("src/bin/foo.rs"));
        assert_eq!(e.line, 41);
        assert_eq!(e.col, 4);
    }

    #[test]
    fn test_non_panic_line_returns_empty() {
        let mut parser = TestPanicParser::new();
        assert!(parser.feed("running 1 test").is_empty());
        assert!(parser.feed("test result: ok. 0 passed; 0 failed").is_empty());
        assert!(parser.feed("error[E0308]: mismatched types").is_empty());
        assert!(parser.feed("  --> src/foo.rs:12:9").is_empty());
        assert!(parser.feed("main.c:10:5: error: 'foo' undeclared").is_empty());
    }

    #[test]
    fn test_absolute_path() {
        let mut parser = TestPanicParser::new();
        let entries = parser.feed(
            "thread 'main' panicked at /home/user/src/app/src/lib.rs:15:3:",
        );
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.path, PathBuf::from("/home/user/src/app/src/lib.rs"));
        assert_eq!(e.line, 14);
        assert_eq!(e.col, 2);
    }

    #[test]
    fn test_match_location() {
        let loc = super::match_location("thread 'tests::x' panicked at src/lib.rs:20:9:")
            .expect("should parse location");
        assert_eq!(loc.0, PathBuf::from("src/lib.rs"));
        assert_eq!(loc.1, 19);
        assert_eq!(loc.2, 8);
    }

    #[test]
    fn test_match_location_non_panic_is_none() {
        assert!(super::match_location("running 1 test").is_none());
        assert!(super::match_location("  --> src/lib.rs:20:9").is_none());
    }

    #[test]
    fn test_match_severity() {
        assert_eq!(
            super::match_severity("thread 'x' panicked at src/lib.rs:1:1:"),
            Some(ErrorSeverity::Error)
        );
        assert_eq!(super::match_severity("running 1 test"), None);
    }
}
