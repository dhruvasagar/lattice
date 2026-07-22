//! CM.3a: the built-in compilation parsers + their shared helpers.
//!
//! - [`CargoRustcParser`] — multi-line rustc/cargo diagnostics
//!   (`error[E0308]: …` header + `--> path:line:col` location).
//! - [`GnuStyleParser`] — single-line gcc/clang/eslint diagnostics
//!   (`path:line:col: severity: message` and `path:line: message`).

mod cargo_rustc;
mod gnu;

pub use cargo_rustc::CargoRustcParser;
pub use gnu::GnuStyleParser;

use std::sync::OnceLock;

use fancy_regex::Regex;
use lattice_protocol::error_list::ErrorSeverity;

/// Compile a built-in parser regex once (lazily) and hand back a
/// reference. Built-in patterns are static and known-good; a compile
/// failure is logged at `error!` and yields `None`, so the parser then
/// simply matches nothing — never a panic on the parse path.
pub(crate) fn compiled<'a>(cell: &'a OnceLock<Option<Regex>>, pattern: &str) -> Option<&'a Regex> {
    cell.get_or_init(|| match Regex::new(pattern) {
        Ok(re) => Some(re),
        Err(e) => {
            tracing::error!(
                pattern,
                error = %e,
                "compilation parser: built-in regex failed to compile; parser disabled"
            );
            None
        }
    })
    .as_ref()
}

/// CM.3b: try the built-in location patterns (rustc `-->` first, then
/// gnu-style `path:line:col:` / `path:line:`) against ONE line and
/// return the first 0-based `(path, line, col)` match. Reuses the same
/// compiled regexes the streaming parsers use — no pattern is
/// duplicated. `None` when the line carries no navigable location.
///
/// Backs [`crate::parser::parse_location_line`], the `<CR>`-jump seam:
/// stdout/stderr interleave in the `*compilation*` buffer, so a
/// buffer-line→entry map is unreliable; parsing the cursor line
/// directly is interleaving-proof and covers both the gnu lines and
/// the cargo `-->` location line.
pub(crate) fn match_location_line(line: &str) -> Option<(std::path::PathBuf, u32, u32)> {
    cargo_rustc::match_location(line).or_else(|| gnu::match_location(line))
}

/// CM.3c: severity of ONE `*compilation*` line, or `None` when the line
/// declares no severity keyword. Tries the rustc/cargo header form
/// (`error[..]:` / `error:` / `warning:`) then the gnu full form
/// (`path:line:col: error|warning|note: …`). Keyword-driven: progress /
/// summary / prose / gnu short-form lines carry no severity keyword and
/// return `None`. Reuses the same compiled patterns the streaming parsers
/// use — no duplication. Backs [`crate::parser::scan_severities`], the
/// in-buffer severity-gutter producer.
pub(crate) fn match_severity(line: &str) -> Option<ErrorSeverity> {
    cargo_rustc::match_header_severity(line).or_else(|| gnu::match_full_severity(line))
}

/// Convert a 1-based line/column string (rustc + gnu tools are both
/// 1-based) to the 0-based `u32` the error substrate +
/// `Editor::jump_to_file_line_col` expect. Returns `None` on a
/// non-numeric field (the caller logs + skips). A `0` field (invalid
/// as 1-based) saturates to `0` rather than underflowing.
pub(crate) fn parse_1based(s: &str) -> Option<u32> {
    s.parse::<u32>().ok().map(|n| n.saturating_sub(1))
}
