//! CM.3a: the cargo/rustc multi-line diagnostic parser.
//!
//! rustc prints a diagnostic as a header line carrying the severity +
//! code + message, then a following location line:
//!
//! ```text
//! error[E0308]: mismatched types
//!   --> src/foo.rs:12:9
//!    |
//! 12 |     let x: u32 = "s";
//! ```
//!
//! The parser holds the header's `(severity, message)` as *pending*
//! and emits one [`ErrorEntry`] when the `-->` location line
//! arrives (converting rustc's 1-based line/col to 0-based). A header
//! with no following location (e.g. `error: aborting due to N previous
//! errors`) is dropped when the next header overwrites the pending
//! slot or the run ends.

use std::path::PathBuf;
use std::sync::OnceLock;

use fancy_regex::Regex;
use lattice_protocol::error_list::{ErrorEntry, ErrorSeverity};

use super::{compiled, parse_1based};
use crate::parser::CompilationParser;

/// `error[E0308]: msg` / `error: msg` / `warning: msg` — captures the
/// severity keyword (1) and the message (2); the optional `[..]` code
/// is matched but not captured.
const HEADER_PATTERN: &str = r"^(error|warning)(?:\[[^\]]+\])?:\s?(.*)$";

/// `  --> path:line:col` — captures path (1), line (2), col (3).
const LOCATION_PATTERN: &str = r"^\s*-->\s+(.+?):(\d+):(\d+)\s*$";

fn header_re() -> Option<&'static Regex> {
    static CELL: OnceLock<Option<Regex>> = OnceLock::new();
    compiled(&CELL, HEADER_PATTERN)
}

fn location_re() -> Option<&'static Regex> {
    static CELL: OnceLock<Option<Regex>> = OnceLock::new();
    compiled(&CELL, LOCATION_PATTERN)
}

/// CM.3b: extract the 0-based `(path, line, col)` from a rustc
/// `--> path:line:col` location line, or `None` when the line is not
/// a rustc location. Shared by [`CargoRustcParser::feed`] (which pairs
/// it with a pending header to complete an entry) and the
/// `parse_location_line` `<CR>`-jump path — one compiled pattern, no
/// duplication.
pub(crate) fn match_location(line: &str) -> Option<(PathBuf, u32, u32)> {
    let re = location_re()?;
    let caps = re.captures(line).ok()??;
    let (path, l, c) = (caps.get(1)?, caps.get(2)?, caps.get(3)?);
    Some((
        PathBuf::from(path.as_str()),
        parse_1based(l.as_str())?,
        parse_1based(c.as_str())?,
    ))
}

/// CM.3c: severity of a rustc/cargo **header** line (`error[E0308]: …` /
/// `error: …` / `warning: …`), or `None` when the line is not a header.
/// Reuses the same compiled [`HEADER_PATTERN`] the streaming parser uses —
/// backs the `*compilation*` in-buffer severity gutter marks (the header
/// line is where the severity is textually visible, emacs-style). The
/// `--> path:line:col` location line carries no severity keyword, so it is
/// correctly `None` here.
pub(crate) fn match_header_severity(line: &str) -> Option<ErrorSeverity> {
    let re = header_re()?;
    let caps = re.captures(line).ok()??;
    match caps.get(1).map(|m| m.as_str()) {
        Some("error") => Some(ErrorSeverity::Error),
        Some("warning") => Some(ErrorSeverity::Warning),
        _ => None,
    }
}

/// Multi-line rustc/cargo diagnostic parser. See the module docs.
pub struct CargoRustcParser {
    /// The severity + message of a diagnostic whose location line has
    /// not yet arrived.
    pending: Option<(ErrorSeverity, String)>,
}

impl CargoRustcParser {
    pub fn new() -> Self {
        Self { pending: None }
    }
}

impl Default for CargoRustcParser {
    fn default() -> Self {
        Self::new()
    }
}

impl CompilationParser for CargoRustcParser {
    fn feed(&mut self, line: &str) -> Vec<ErrorEntry> {
        // Location line first: a `-->` under a pending header completes
        // the entry. (Order matters only in that a single line can't be
        // both a header and a location.)
        if let Some(re) = location_re() {
            if matches!(re.is_match(line), Ok(true)) {
                let Some((severity, message)) = self.pending.take() else {
                    // A `-->` with no preceding error/warning header
                    // (e.g. a `note:` location) — nothing to emit.
                    return Vec::new();
                };
                match match_location(line) {
                    Some((path, line0, col0)) => {
                        return vec![ErrorEntry {
                            path,
                            line: line0,
                            col: col0,
                            severity,
                            message,
                        }];
                    }
                    None => {
                        tracing::debug!(
                            location = line,
                            "cargo parser: unparseable line/col in location; skipping"
                        );
                    }
                }
                return Vec::new();
            }
        }

        // Header line: prime the pending slot. Overwrites any prior
        // un-located pending (that diagnostic had no location).
        if let Some(re) = header_re() {
            if let Ok(Some(caps)) = re.captures(line) {
                let severity = match caps.get(1).map(|m| m.as_str()) {
                    Some("error") => ErrorSeverity::Error,
                    Some("warning") => ErrorSeverity::Warning,
                    _ => ErrorSeverity::Info,
                };
                let message = caps
                    .get(2)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default();
                self.pending = Some((severity, message));
            }
        }

        Vec::new()
    }

    fn reset(&mut self) {
        self.pending = None;
    }
}
