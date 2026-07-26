use std::path::PathBuf;
use std::sync::OnceLock;

use fancy_regex::Regex;
use lattice_protocol::error_list::{ErrorEntry, ErrorSeverity};

use super::{compiled, parse_1based};
use crate::parser::CompilationParser;

/// `path:line:col` or `path:line` anywhere in the output line.
///
/// Unlike the format-specific parsers, this pattern is NOT anchored
/// at the start of the line — it finds the first `file:line:col`
/// occurrence anywhere in the line using [`Regex::find_iter`]. The
/// path is validated with [`is_file_like`] (contains `/` or `.`) to
/// reject timestamps, version strings, and other `word:digits`
/// noise.
///
/// This is the catch-all: any tool's output that embeds a file
/// location (log formats, script output, printf debugging, bespoke
/// linters) is parsed regardless of prefix, suffix, or decoration.
/// Format-specific parsers (cargo/rustc, gnu-style, test panics) run
/// first and provide better severity/message metadata when they
/// match; the general parser fires only for lines the specific
/// parsers miss.
const GENERAL_PATTERN: &str = r"(\S+?):(\d+)(?::(\d+))?";

fn general_re() -> Option<&'static Regex> {
    static CELL: OnceLock<Option<Regex>> = OnceLock::new();
    compiled(&CELL, GENERAL_PATTERN)
}

/// The path must carry a `/` (directory) or `.` (extension),
/// otherwise `word:digits` patterns like `version:1.2.3` or
/// `START:12345` or `12:34:56` (timestamp) are rejected.
fn is_file_like(path: &str) -> bool {
    path.contains('/') || path.contains('.')
}

fn is_numeric(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit())
}

fn extract_entry(m: &fancy_regex::Match<'_>) -> Option<ErrorEntry> {
    let text = m.as_str();
    let mut parts: Vec<&str> = text.rsplitn(3, ':').collect();
    parts.reverse();

    match parts.len() {
        3 => {
            let path = parts[0];
            let line_s = parts[1];
            let col_s = parts[2];
            if !is_file_like(path) {
                return None;
            }
            if is_numeric(path) {
                return None;
            }
            let line0 = parse_1based(line_s)?;
            let col0 = parse_1based(col_s)?;
            Some(ErrorEntry {
                path: PathBuf::from(path),
                line: line0,
                col: col0,
                severity: ErrorSeverity::Info,
                message: String::new(),
            })
        }
        2 => {
            let path = parts[0];
            let line_s = parts[1];
            if !is_file_like(path) {
                return None;
            }
            if is_numeric(path) {
                return None;
            }
            let line0 = parse_1based(line_s)?;
            Some(ErrorEntry {
                path: PathBuf::from(path),
                line: line0,
                col: 0,
                severity: ErrorSeverity::Info,
                message: String::new(),
            })
        }
        _ => None,
    }
}

pub(crate) fn match_location(line: &str) -> Option<(PathBuf, u32, u32)> {
    let re = general_re()?;
    for result in re.find_iter(line) {
        let Ok(m) = result else {
            continue;
        };
        let entry = extract_entry(&m)?;
        return Some((entry.path, entry.line, entry.col));
    }
    None
}

pub(crate) fn match_severity(_line: &str) -> Option<ErrorSeverity> {
    None
}

pub struct GeneralParser;

impl GeneralParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GeneralParser {
    fn default() -> Self {
        Self::new()
    }
}

impl CompilationParser for GeneralParser {
    fn feed(&mut self, line: &str) -> Vec<ErrorEntry> {
        let Some(re) = general_re() else {
            return Vec::new();
        };
        for result in re.find_iter(line) {
            let Ok(m) = result else {
                continue;
            };
            if let Some(entry) = extract_entry(&m) {
                return vec![entry];
            }
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn path_line_col_anywhere_in_line() {
        let mut p = GeneralParser::new();
        let entries = p.feed("[ERROR] src/foo.rs:42:18: something failed");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("src/foo.rs"));
        assert_eq!(entries[0].line, 41, "1-based 42 → 0-based 41");
        assert_eq!(entries[0].col, 17, "1-based 18 → 0-based 17");
    }

    #[test]
    fn path_line_only_no_col() {
        let mut p = GeneralParser::new();
        let entries = p.feed("  src/foo.rs:42  something");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("src/foo.rs"));
        assert_eq!(entries[0].line, 41);
        assert_eq!(entries[0].col, 0);
    }

    #[test]
    fn line_starting_with_non_path_text() {
        let mut p = GeneralParser::new();
        let entries = p.feed("2026-07-22 12:00:00 ERROR src/foo.rs:42:18 bad thing");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("src/foo.rs"));
        assert_eq!(entries[0].line, 41);
    }

    #[test]
    fn js_like_log_format() {
        let mut p = GeneralParser::new();
        let entries = p.feed("file: src/foo.rs:42:18, function: bar");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("src/foo.rs"));
        assert_eq!(entries[0].line, 41);
    }

    #[test]
    fn timestamp_is_rejected() {
        let mut p = GeneralParser::new();
        assert!(p.feed("12:34:56").is_empty(), "timestamp must be rejected");
        assert!(
            p.feed("  12:34:56 starting").is_empty(),
            "timestamp with text must be rejected"
        );
    }

    #[test]
    fn plain_version_without_colon_is_rejected() {
        let mut p = GeneralParser::new();
        assert!(
            p.feed("1.2.3").is_empty(),
            "version without colon must be rejected"
        );
    }

    #[test]
    fn version_like_string_is_parsed_when_followed_by_line() {
        let mut p = GeneralParser::new();
        let entries = p.feed("v1.2.3:4");
        assert_eq!(
            entries.len(),
            1,
            "v1.2.3 contains . so is_file_like accepts it"
        );
        assert_eq!(entries[0].path.to_string_lossy(), "v1.2.3");
        assert_eq!(entries[0].line, 3);
    }

    #[test]
    fn gcc_style_with_indent_is_caught() {
        let mut p = GeneralParser::new();
        let entries = p.feed("  main.c:10:5: error: 'foo' undeclared");
        assert_eq!(entries.len(), 1, "gcc with indent must be parsed");
        assert_eq!(entries[0].path, PathBuf::from("main.c"));
        assert_eq!(entries[0].line, 9);
    }

    #[test]
    fn rustc_location_line_is_caught() {
        let mut p = GeneralParser::new();
        let entries = p.feed("  --> src/foo.rs:12:9");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("src/foo.rs"));
        assert_eq!(entries[0].line, 11);
        assert_eq!(entries[0].col, 8);
    }

    #[test]
    fn bare_path_line_is_caught() {
        let mut p = GeneralParser::new();
        let entries = p.feed("src/main.rs:42");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("src/main.rs"));
        assert_eq!(entries[0].line, 41);
    }

    #[test]
    fn path_without_extension_or_slash_is_rejected() {
        let mut p = GeneralParser::new();
        assert!(
            p.feed("Makefile:10: missing separator").is_empty(),
            "Makefile (no / or .) must be rejected by general parser"
        );
    }

    #[test]
    fn match_location_finds_anywhere_in_line() {
        let loc = match_location("before src/bar.rs:33:5 after").unwrap();
        assert_eq!(loc.0, PathBuf::from("src/bar.rs"));
        assert_eq!(loc.1, 32);
        assert_eq!(loc.2, 4);
    }

    #[test]
    fn match_location_non_file_is_none() {
        assert!(match_location("12:34:56").is_none());
        assert!(match_location("just prose").is_none());
        assert!(match_location("Makefile:10").is_none());
    }

    #[test]
    fn match_severity_always_none() {
        assert_eq!(match_severity("src/foo.rs:42:18"), None);
        assert_eq!(match_severity("anything"), None);
    }

    #[test]
    fn absolute_path_is_parsed() {
        let mut p = GeneralParser::new();
        let entries = p.feed("Error: /home/user/src/app/lib.rs:15:3:");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("/home/user/src/app/lib.rs"));
        assert_eq!(entries[0].line, 14);
        assert_eq!(entries[0].col, 2);
    }

    #[test]
    fn path_with_dots_is_parsed() {
        let mut p = GeneralParser::new();
        let entries = p.feed("data/file.tar.gz:42: error: corrupt");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("data/file.tar.gz"));
        assert_eq!(entries[0].line, 41);
    }

    #[test]
    fn path_with_multiple_records_line_only_uses_first() {
        let mut p = GeneralParser::new();
        let entries = p.feed("file1.rs:10 and file2.rs:20 and file3.rs:30");
        assert_eq!(
            entries.len(),
            1,
            "general parser returns at most one entry per line"
        );
        assert_eq!(entries[0].path, PathBuf::from("file1.rs"));
    }

    #[test]
    fn non_matching_lines_yield_no_entries() {
        let mut p = GeneralParser::new();
        assert!(p.feed("Compiling foo v0.1.0").is_empty());
        assert!(p.feed("   Finished dev [unoptimized] target(s)").is_empty());
        assert!(p.feed("   Compiling something").is_empty());
        assert!(p.feed("==== running 3 tests ====").is_empty());
    }

    #[test]
    fn all_numeric_path_is_rejected() {
        let mut p = GeneralParser::new();
        assert!(
            p.feed("1234:56").is_empty(),
            "all-numeric path must be rejected"
        );
        assert!(
            p.feed("12:34:56").is_empty(),
            "purely numeric path:line:col must be rejected"
        );
    }
}
