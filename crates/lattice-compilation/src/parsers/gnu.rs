//! CM.3a: the gnu-style single-line diagnostic parser
//! (gcc / clang / eslint / many linters).
//!
//! Two line forms, tried in order:
//!
//! ```text
//! main.c:10:5: error: 'foo' undeclared          # path:line:col: severity: message
//! Makefile:10: missing separator                # path:line: message (severity → Info)
//! src/lib.rs:7:// TODO tidy imports             # path:line:text  (grep -Hn, no space)
//! ```
//!
//! Stateless — each line stands alone (no pending multi-line state).
//! The path is matched as a run of non-whitespace so a cargo/rustc
//! `  --> src/foo.rs:12:9` location line (leading whitespace) is NOT
//! misread as a gnu diagnostic when both parsers feed the same stream.
//! (Paths containing spaces are out of scope for CM.3a.)
//!
//! CM.7: the short form's post-`line:` whitespace is OPTIONAL so
//! `grep -Hn`'s `path:line:matched-text` (no space before the match)
//! populates the list. To keep that from swallowing `hh:mm:ss`-style
//! timestamps (`12:34:56`, `12:34: starting`), an **all-numeric path is
//! rejected** — real source paths carry a letter, `.`, or `/`.

use std::path::PathBuf;
use std::sync::OnceLock;

use fancy_regex::Regex;
use lattice_protocol::error_list::{ErrorEntry, ErrorSeverity};

use super::{compiled, parse_1based};
use crate::parser::CompilationParser;

/// `path:line:col: severity: message` — captures path (1), line (2),
/// col (3), severity (4), message (5).
const FULL_PATTERN: &str = r"^(\S+):(\d+):(\d+):\s+([A-Za-z][A-Za-z ]*?):\s*(.*)$";

/// `path:line: message` — captures path (1), line (2), message (3).
/// Requires whitespace after `line:` so a `path:line:col:` line does
/// not match this shorter form (that is the FULL form's job) and a
/// `12:34:56` timestamp (no space before the seconds) is not swallowed.
/// Permissive path (`\S+`) so `Makefile:10: missing separator` matches.
const SHORT_PATTERN: &str = r"^(\S+):(\d+):\s+(.*)$";

/// CM.7: `path:line:text` with NO space — `grep -Hn` / `rg` output of a
/// non-indented match. Tried only AFTER the space form fails, and gated
/// on [`is_file_like`]: the path must carry a `/` or `.` so a `12:34:56`
/// / `[12:00:00]` timestamp (no separator in its "path") never matches.
const GREP_PATTERN: &str = r"^(\S+):(\d+):(\S.*)$";

/// The space form's path must carry a non-digit — rejects the `09` of a
/// `09:01: warming caches` timestamp while keeping `Makefile:10: msg`
/// (make errors have no `/` or `.` in the path, so the stricter
/// [`is_file_like`] would wrongly drop them).
fn is_path_like(path: &str) -> bool {
    !path.bytes().all(|b| b.is_ascii_digit())
}

/// A no-space `path:line:text` capture is a real location only when the
/// path looks like a file — contains a `/` (directory) or `.`
/// (extension). Accepts `src/lib.rs`, `a.c`, `./x`; rejects `12`,
/// `[12`, `word` (timestamps / `key:1:value` noise).
fn is_file_like(path: &str) -> bool {
    path.contains('/') || path.contains('.')
}

fn grep_re() -> Option<&'static Regex> {
    static CELL: OnceLock<Option<Regex>> = OnceLock::new();
    compiled(&CELL, GREP_PATTERN)
}

fn full_re() -> Option<&'static Regex> {
    static CELL: OnceLock<Option<Regex>> = OnceLock::new();
    compiled(&CELL, FULL_PATTERN)
}

fn short_re() -> Option<&'static Regex> {
    static CELL: OnceLock<Option<Regex>> = OnceLock::new();
    compiled(&CELL, SHORT_PATTERN)
}

/// CM.3b: extract the 0-based `(path, line, col)` from a gnu-style
/// diagnostic line, trying the full `path:line:col:` form then the
/// short `path:line:` form (col defaults to `0`). `None` when the line
/// is not a gnu location. Shares the compiled full/short patterns with
/// [`GnuStyleParser::feed`]; used by the `parse_location_line`
/// `<CR>`-jump path (which needs only the location, not severity /
/// message).
pub(crate) fn match_location(line: &str) -> Option<(PathBuf, u32, u32)> {
    if let Some(re) = full_re() {
        if let Ok(Some(caps)) = re.captures(line) {
            if let (Some(path), Some(l), Some(c)) = (caps.get(1), caps.get(2), caps.get(3)) {
                if let (Some(line0), Some(col0)) =
                    (parse_1based(l.as_str()), parse_1based(c.as_str()))
                {
                    return Some((PathBuf::from(path.as_str()), line0, col0));
                }
            }
        }
    }
    if let Some(re) = short_re() {
        if let Ok(Some(caps)) = re.captures(line) {
            if let (Some(path), Some(l)) = (caps.get(1), caps.get(2)) {
                if is_path_like(path.as_str()) {
                    if let Some(line0) = parse_1based(l.as_str()) {
                        return Some((PathBuf::from(path.as_str()), line0, 0));
                    }
                }
            }
        }
    }
    // CM.7: grep no-space form (`path:line:text`), file-like path only.
    if let Some(re) = grep_re() {
        if let Ok(Some(caps)) = re.captures(line) {
            if let (Some(path), Some(l)) = (caps.get(1), caps.get(2)) {
                if is_file_like(path.as_str()) {
                    if let Some(line0) = parse_1based(l.as_str()) {
                        return Some((PathBuf::from(path.as_str()), line0, 0));
                    }
                }
            }
        }
    }
    None
}

/// CM.3c: severity of a gnu-style **full-form** diagnostic line
/// (`path:line:col: severity: message`), or `None` when the line is not a
/// gnu full-form diagnostic. Reuses the compiled [`FULL_PATTERN`]'s
/// severity capture — backs the `*compilation*` in-buffer severity gutter
/// marks. The short form (`path:line: message`) carries no severity
/// keyword, so it is deliberately `None` here (keyword-driven marks only).
pub(crate) fn match_full_severity(line: &str) -> Option<ErrorSeverity> {
    let re = full_re()?;
    let caps = re.captures(line).ok()??;
    caps.get(4).map(|sev| gnu_severity(sev.as_str()))
}

/// Map a gnu-style severity keyword onto [`ErrorSeverity`].
fn gnu_severity(keyword: &str) -> ErrorSeverity {
    match keyword.trim().to_ascii_lowercase().as_str() {
        "error" | "fatal error" => ErrorSeverity::Error,
        "warning" => ErrorSeverity::Warning,
        "note" => ErrorSeverity::Note,
        _ => ErrorSeverity::Info,
    }
}

/// Stateless gnu-style diagnostic parser. See the module docs.
pub struct GnuStyleParser;

impl GnuStyleParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GnuStyleParser {
    fn default() -> Self {
        Self::new()
    }
}

impl CompilationParser for GnuStyleParser {
    fn feed(&mut self, line: &str) -> Vec<ErrorEntry> {
        // Full form: path:line:col: severity: message.
        if let Some(re) = full_re() {
            if let Ok(Some(caps)) = re.captures(line) {
                if let (Some(path), Some(l), Some(c), Some(sev), Some(msg)) = (
                    caps.get(1),
                    caps.get(2),
                    caps.get(3),
                    caps.get(4),
                    caps.get(5),
                ) {
                    match (parse_1based(l.as_str()), parse_1based(c.as_str())) {
                        (Some(line0), Some(col0)) => {
                            return vec![ErrorEntry {
                                path: PathBuf::from(path.as_str()),
                                line: line0,
                                col: col0,
                                severity: gnu_severity(sev.as_str()),
                                message: msg.as_str().to_string(),
                            }];
                        }
                        _ => {
                            tracing::debug!(
                                diagnostic = line,
                                "gnu parser: unparseable line/col; skipping"
                            );
                            return Vec::new();
                        }
                    }
                }
            }
        }

        // Short form: path:line: message (space required; no column, Info).
        if let Some(re) = short_re() {
            if let Ok(Some(caps)) = re.captures(line) {
                if let (Some(path), Some(l), Some(msg)) = (caps.get(1), caps.get(2), caps.get(3)) {
                    if !is_path_like(path.as_str()) {
                        // All-numeric path → a `hh:mm: text` timestamp,
                        // not a location. Skip.
                        return Vec::new();
                    }
                    match parse_1based(l.as_str()) {
                        Some(line0) => {
                            return vec![ErrorEntry {
                                path: PathBuf::from(path.as_str()),
                                line: line0,
                                col: 0,
                                severity: ErrorSeverity::Info,
                                message: msg.as_str().to_string(),
                            }];
                        }
                        None => {
                            tracing::debug!(
                                diagnostic = line,
                                "gnu parser: unparseable line; skipping"
                            );
                            return Vec::new();
                        }
                    }
                }
            }
        }

        // CM.7: grep no-space form (`path:line:text`). Gated on a
        // file-like path so timestamps never become entries.
        if let Some(re) = grep_re() {
            if let Ok(Some(caps)) = re.captures(line) {
                if let (Some(path), Some(l), Some(msg)) = (caps.get(1), caps.get(2), caps.get(3)) {
                    if is_file_like(path.as_str()) {
                        if let Some(line0) = parse_1based(l.as_str()) {
                            return vec![ErrorEntry {
                                path: PathBuf::from(path.as_str()),
                                line: line0,
                                col: 0,
                                severity: ErrorSeverity::Info,
                                message: msg.as_str().to_string(),
                            }];
                        }
                    }
                }
            }
        }

        Vec::new()
    }
}
