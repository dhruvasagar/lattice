//! CM.7 (2026-07-22): compilation mode is **tool-agnostic**.
//!
//! The requirement is decoupling, not cargo-specific parsing: `:compile
//! <any-cli>` must extract `file:line[:col]` references from an
//! arbitrary tool's output into the error list. These tests feed the
//! built-in `ParserRegistry` output from tools that are NOT cargo/rustc
//! — grep, eslint, gcc, a pytest-style line — and assert entries are
//! produced with the right 0-based locations. This is what lets
//! `*compilation*` + `*problems*` work identically regardless of tool.

use lattice_compilation::ParserRegistry;
use lattice_protocol::error_list::ErrorSeverity;

/// Feed a whole tool transcript line-by-line and collect every entry.
fn parse_all(lines: &[&str]) -> Vec<lattice_protocol::error_list::ErrorEntry> {
    let mut reg = ParserRegistry::with_builtins();
    let mut out = Vec::new();
    for line in lines {
        out.extend(reg.feed(line));
    }
    out
}

#[test]
fn grep_hn_output_populates_the_list() {
    // `grep -Hn TODO -r .` → `path:line:matched text` (no column).
    let entries = parse_all(&[
        "src/main.rs:42:    // TODO: refactor this",
        "src/lib.rs:7:// TODO tidy imports",
        "not a match line at all",
    ]);
    assert_eq!(entries.len(), 2, "two grep hits → two entries: {entries:?}");
    assert_eq!(entries[0].path.to_string_lossy(), "src/main.rs");
    assert_eq!(entries[0].line, 41, "1-based 42 → 0-based 41");
    assert_eq!(entries[0].col, 0, "grep has no column → 0");
    assert_eq!(entries[1].path.to_string_lossy(), "src/lib.rs");
    assert_eq!(entries[1].line, 6);
}

#[test]
fn eslint_and_gcc_style_output_populates_the_list() {
    // `path:line:col: severity: message` — eslint / gcc / clang / tsc.
    let entries = parse_all(&[
        "app/index.js:10:5: error: 'x' is not defined",
        "app/util.js:3:12: warning: unused variable 'y'",
        "src/foo.c:88:1: note: previous declaration here",
    ]);
    assert_eq!(entries.len(), 3, "{entries:?}");
    assert_eq!(entries[0].path.to_string_lossy(), "app/index.js");
    assert_eq!(entries[0].line, 9);
    assert_eq!(entries[0].col, 4, "1-based col 5 → 0-based 4");
    assert_eq!(entries[0].severity, ErrorSeverity::Error);
    assert_eq!(entries[1].severity, ErrorSeverity::Warning);
    assert_eq!(entries[2].severity, ErrorSeverity::Note);
}

#[test]
fn timestamps_are_not_mistaken_for_locations() {
    // `hh:mm:ss` and `hh:mm: text` log prefixes look like `path:line:…`
    // but the "path" is all-numeric — they must NOT become entries.
    let entries = parse_all(&[
        "12:34:56 build started",
        "09:01: warming caches",
        "[12:00:00] done",
    ]);
    assert!(
        entries.is_empty(),
        "numeric timestamps must not parse as locations: {entries:?}"
    );
}

#[test]
fn arbitrary_tool_is_not_coupled_to_cargo() {
    // A tool that never emits cargo's `error[E….]:` / `-->` syntax
    // still fills the list purely via the generic gnu matcher — proving
    // compilation mode is not tied to cargo/rustc.
    let entries = parse_all(&[
        "Running custom-linter v9…",
        "checks/naming.py:120:3: error: identifier too short",
        "0 files reformatted, 1 error",
    ]);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path.to_string_lossy(), "checks/naming.py");
    assert_eq!(entries[0].line, 119);
    assert_eq!(entries[0].col, 2);
    assert_eq!(entries[0].severity, ErrorSeverity::Error);
}
