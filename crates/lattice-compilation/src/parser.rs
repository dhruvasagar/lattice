//! CM.3a (2026-07-22): the compilation **parser registry** — the
//! extensibility seam that turns streamed compiler-output lines into
//! navigable [`ErrorEntry`]s (emacs `compilation-error-regexp-alist`,
//! on Lattice's substrate).
//!
//! A [`CompilationParser`] is a stateful matcher over lines fed in
//! arrival order. Multi-line formats (cargo/rustc emits an
//! `error[E0308]: …` header then a following `  --> path:line:col`
//! location line) are supported by holding a *pending* severity +
//! message and emitting the entry when the location line arrives.
//!
//! The [`ParserRegistry`] feeds each line to every registered parser
//! and concatenates the results. For CM.3a the active set is the
//! built-in cargo/rustc + gnu-style pair; Phase 7 opens contribution
//! to WASM plugins via this same `Vec<Box<dyn CompilationParser>>`
//! seam (`docs/dev/architecture/compilation-mode.md` §5).
//!
//! ## API for CM.3b
//!
//! CM.3b (in-buffer decoration + `<CR>`-jump) reuses the exact
//! line→entry mapping here: feed a `*compilation*` buffer line to a
//! [`ParserRegistry`] and the returned [`ErrorEntry`]s carry the
//! 0-based `line`/`col` + severity + message for the matched source
//! location. The parser is the single source of truth for "is this
//! log line a navigable error, and where does it point?".

use std::path::PathBuf;

use lattice_protocol::error_list::{ErrorEntry, ErrorSeverity};

/// CM.3b: a source location parsed out of a single `*compilation*`
/// buffer line by [`parse_location_line`] for the `<CR>`-jump. `line` /
/// `col` are **0-based** — the error substrate +
/// `Editor::jump_to_file_line_col` convention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilationLocation {
    pub path: PathBuf,
    /// 0-based source line.
    pub line: u32,
    /// 0-based source column (`0` for gnu short-form lines with no
    /// column).
    pub col: u32,
}

/// CM.3b: parse a source location out of ONE `*compilation*` buffer
/// line — the `<CR>`-jump seam. Runs the built-in rustc-`-->` and
/// gnu-style location regexes (the same compiled patterns the
/// streaming parsers use, via [`crate::parsers::match_location_line`])
/// against the line and returns the first match, converting 1-based
/// line/col to 0-based. `None` when the line carries no navigable
/// location (progress, summary, backtraces, prose).
///
/// Deliberately independent of the streaming [`ParserRegistry`]'s
/// pending multi-line state: stdout/stderr interleave in the buffer,
/// so a buffer-line→entry map isn't reliable. Parsing the cursor line
/// directly is interleaving-proof and covers both the gnu lines and
/// the cargo `-->` location line. CM.3c's severity gutter decoration
/// reuses this exact function.
pub fn parse_location_line(line: &str) -> Option<CompilationLocation> {
    let (path, line0, col0) = crate::parsers::match_location_line(line)?;
    Some(CompilationLocation {
        path,
        line: line0,
        col: col0,
    })
}

/// CM.3c: the severity a single `*compilation*` line declares, or `None`
/// when it carries no severity keyword. Reuses the built-in rustc-header
/// and gnu-full-form patterns (via [`crate::parsers::match_severity`]) —
/// the same compiled regexes the streaming parsers use. Keyword-driven:
/// progress / summary / prose / location (`-->`) / gnu-short lines return
/// `None`. The severity gutter mark lands on the line where the severity is
/// textually visible (the rustc header line; the gnu full-form line),
/// mirroring emacs `compilation-mode`.
pub fn match_severity(line: &str) -> Option<ErrorSeverity> {
    crate::parsers::match_severity(line)
}

/// CM.3c: scan a block of streamed text for severity lines, returning
/// `(absolute_line, severity)` for each match. `base_line` is the 0-based
/// buffer line number the block's FIRST line lands on; line `i` of the
/// block (via [`str::lines`]) maps to absolute line `base_line + i`.
///
/// This is the pure, host-free unit-test seam the compilation drain uses:
/// the drain tracks the running buffer line number (a `Reset` sets it to
/// the header's newline count; each `Append`/`Finished` advances it by its
/// own newline count) and calls this per chunk to grow the buffer's
/// severity index. Text is assumed newline-terminated per line (the pipe
/// readers append `\n` after every captured line); a trailing partial line
/// with no `\n` is still scanned but a following chunk that continues it
/// may not be re-attributed (erring toward not-decorating a partial line,
/// which is acceptable and does not occur with the newline-terminated
/// reader output).
/// CM.3c: scan a block of streamed text for location-bearing
/// lines (lines whose text contains a file path + line:col that
/// `parse_location_line` can navigate to). Returns
/// `(absolute_line, path_byte_start, path_byte_end)` for each match.
///
/// Mirrors [`scan_severities`]: `base_line` is the 0-based buffer
/// line number the block's FIRST line lands on; line `i` of the
/// block maps to absolute line `base_line + i`. The compilation
/// drain calls this per chunk to grow the buffer's location-line
/// index. The byte range is the span of the file-path portion
/// within the line text (for link-like fg highlighting).
pub fn scan_location_lines(base_line: u32, text: &str) -> Vec<(u32, u32, u32)> {
    text.lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let (start, end) = location_path_byte_range(line)?;
            Some((base_line + i as u32, start as u32, end as u32))
        })
        .collect()
}

/// Return the byte range of the file-path portion of a location
/// line. Uses [`parse_location_line`] to locate the path, then
/// searches for its string representation in the line text.
fn location_path_byte_range(line: &str) -> Option<(usize, usize)> {
    let loc = parse_location_line(line)?;
    let path_str = loc.path.to_str()?;
    let byte_start = line.find(path_str)?;
    let byte_end = byte_start + path_str.len();
    Some((byte_start, byte_end))
}

/// CM.3c: scan a block of streamed text for severity lines, returning
/// `(absolute_line, severity)` for each match. `base_line` is the 0-based
/// buffer line number the block's FIRST line lands on; line `i` of the
/// block (via [`str::lines`]) maps to absolute line `base_line + i`.
pub fn scan_severities(base_line: u32, text: &str) -> Vec<(u32, ErrorSeverity)> {
    text.lines()
        .enumerate()
        .filter_map(|(i, line)| match_severity(line).map(|sev| (base_line + i as u32, sev)))
        .collect()
}

/// A named matcher over streamed compiler-output lines producing
/// [`ErrorEntry`]s.
///
/// `feed` is called once per line in arrival order and returns the
/// entries that line *completed* (zero for a header line that only
/// primes a pending diagnostic, or for a line matching nothing).
/// `reset` drops any pending multi-line state at the start of a run.
///
/// Implementors never panic on the parse path: a malformed-but-claimed
/// match is logged at `debug!` and skipped (paramount goal #1's
/// "log + skip, never panic on the process/parse path").
pub trait CompilationParser: Send {
    /// Feed one line; return entries completed by it.
    fn feed(&mut self, line: &str) -> Vec<ErrorEntry>;
}

/// The active set of parsers. Feeds each line to every parser and
/// concatenates their entries. The `Vec<Box<dyn CompilationParser>>`
/// is the extensibility seam: built-in parsers, plus (CM.6) WASM-contributed
/// ones — a plugin implementing the `error-parser` world is registered here
/// as one more `Box<dyn CompilationParser>` and is indistinguishable from a
/// native parser downstream.
pub struct ParserRegistry {
    parsers: Vec<Box<dyn CompilationParser>>,
}

impl ParserRegistry {
    /// An empty registry (no parsers). Prefer [`Self::with_builtins`].
    pub fn new() -> Self {
        Self {
            parsers: Vec::new(),
        }
    }

    /// The default registry: the built-in cargo/rustc (multi-line) +
    /// gnu-style (single-line) parsers.
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(crate::parsers::CargoRustcParser::new()));
        registry.register(Box::new(crate::parsers::GnuStyleParser::new()));
        registry.register(Box::new(crate::parsers::TestPanicParser::new()));
        registry.register(Box::new(crate::parsers::GeneralParser::new()));
        registry
    }

    /// Add a parser to the active set. Order is preserved; each line
    /// is fed to parsers in registration order.
    ///
    /// CM.6: plugin parsers register **before** [`Self::with_builtins`]'s
    /// catch-all would claim a line, but after the format-specific natives.
    /// The dedup in [`Self::feed`] is first-entry-wins per location, so a
    /// plugin that recognises a line rustc also recognises does not displace
    /// rustc's richer entry — and a line only the plugin understands is still
    /// its own.
    pub fn register(&mut self, parser: Box<dyn CompilationParser>) {
        self.parsers.push(parser);
    }

    /// CM.6: register a plugin parser, placed ahead of the catch-all.
    ///
    /// `with_builtins` ends with `GeneralParser`, whose job is to salvage a
    /// `file:line:col` out of anything. If a plugin registered after it, the
    /// catch-all's thin `Info` entry would win the dedup for every location
    /// the plugin also matched, and the plugin's severity and message would
    /// be silently discarded — the plugin would look like it did nothing.
    pub fn register_before_catch_all(&mut self, parser: Box<dyn CompilationParser>) {
        let at = self.parsers.len().saturating_sub(1);
        self.parsers.insert(at, parser);
    }

    /// Feed one line to every registered parser and concatenate the
    /// entries they complete. Deduplicates by `(path, line, col)` —
    /// the FIRST entry for each location wins. Format-specific parsers
    /// register first and produce richer metadata (severity, message);
    /// the catch-all [`crate::parsers::GeneralParser`] registers last
    /// and its `Info`/empty duplicates are silently dropped.
    pub fn feed(&mut self, line: &str) -> Vec<ErrorEntry> {
        let mut out: Vec<ErrorEntry> = Vec::new();
        for parser in &mut self.parsers {
            for entry in parser.feed(line) {
                let dup = out.iter().any(|existing| {
                    existing.path == entry.path
                        && existing.line == entry.line
                        && existing.col == entry.col
                });
                if !dup {
                    out.push(entry);
                }
            }
        }
        out
    }
}

impl Default for ParserRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

#[cfg(test)]
mod cm6_tests {
    use super::*;

    /// A stand-in for a plugin parser: recognises one bespoke shape.
    struct FakePlugin;
    impl CompilationParser for FakePlugin {
        fn feed(&mut self, line: &str) -> Vec<ErrorEntry> {
            line.strip_prefix("QQ ")
                .map(|rest| {
                    vec![ErrorEntry {
                        path: std::path::PathBuf::from(rest),
                        line: 0,
                        col: 0,
                        severity: ErrorSeverity::Error,
                        message: "from the plugin".into(),
                    }]
                })
                .unwrap_or_default()
        }
    }

    /// A parser that claims the same location the catch-all would, so the
    /// ordering is observable.
    struct ClaimsMainRs;
    impl CompilationParser for ClaimsMainRs {
        fn feed(&mut self, line: &str) -> Vec<ErrorEntry> {
            if line.contains("main.rs:10:5") {
                vec![ErrorEntry {
                    path: std::path::PathBuf::from("main.rs"),
                    line: 9,
                    col: 4,
                    severity: ErrorSeverity::Warning,
                    message: "the plugin's richer message".into(),
                }]
            } else {
                Vec::new()
            }
        }
    }

    #[test]
    fn a_registered_plugin_parser_contributes_entries() {
        let mut r = ParserRegistry::with_builtins();
        r.register_before_catch_all(Box::new(FakePlugin));
        let got = r.feed("QQ weird/format.q");
        assert_eq!(got.len(), 1, "got {got:?}");
        assert_eq!(got[0].message, "from the plugin");
    }

    #[test]
    fn a_plugin_parser_beats_the_catch_all_for_the_same_location() {
        // The ordering this exists for. Registered after `GeneralParser`, the
        // catch-all's thin Info entry would win the first-entry-wins dedup and
        // the plugin would appear to do nothing.
        let mut r = ParserRegistry::with_builtins();
        r.register_before_catch_all(Box::new(ClaimsMainRs));
        let got = r.feed("something main.rs:10:5 something");
        assert_eq!(got.len(), 1, "deduped by location: {got:?}");
        assert_eq!(
            got[0].message, "the plugin's richer message",
            "the plugin's entry must win over the catch-all's salvage"
        );
        assert_eq!(got[0].severity, ErrorSeverity::Warning);
    }

    #[test]
    fn a_plugin_parser_does_not_displace_a_format_specific_native() {
        // The other half of the ordering: a native parser that understands
        // the format properly still wins, because it registers first.
        let mut r = ParserRegistry::with_builtins();
        r.register_before_catch_all(Box::new(ClaimsMainRs));
        let got = r.feed("main.rs:10:5: error: real gnu-style diagnostic");
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].message, "real gnu-style diagnostic",
            "the gnu parser understands this line better than the plugin: {got:?}"
        );
    }

    #[test]
    fn a_silent_plugin_parser_costs_nothing() {
        let mut r = ParserRegistry::with_builtins();
        r.register_before_catch_all(Box::new(FakePlugin));
        assert!(r.feed("   Compiling foo v0.1.0").is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_protocol::error_list::ErrorSeverity;
    use std::path::PathBuf;

    /// Feed every line of `block` through one registry in order and
    /// return the concatenated entries (as the stderr reader does).
    fn parse_block(block: &str) -> Vec<ErrorEntry> {
        let mut registry = ParserRegistry::with_builtins();
        let mut out = Vec::new();
        for line in block.lines() {
            out.extend(registry.feed(line));
        }
        out
    }

    #[test]
    fn cargo_multiline_error_block_yields_one_zero_based_entry() {
        let block = "\
Compiling foo v0.1.0 (/tmp/foo)
error[E0308]: mismatched types
  --> src/foo.rs:12:9
   |
12 |     let x: u32 = \"s\";
   |            ---   ^^^ expected `u32`, found `&str`
   |
error: aborting due to 1 previous error
";
        let entries = parse_block(block);
        assert_eq!(entries.len(), 1, "one located diagnostic, got {entries:?}");
        let e = &entries[0];
        assert_eq!(e.path, PathBuf::from("src/foo.rs"));
        assert_eq!(e.line, 11, "rustc 1-based 12 → 0-based 11");
        assert_eq!(e.col, 8, "rustc 1-based 9 → 0-based 8");
        assert_eq!(e.severity, ErrorSeverity::Error);
        assert_eq!(e.message, "mismatched types");
    }

    #[test]
    fn cargo_warning_block_yields_warning_severity() {
        let block = "\
warning: unused variable: `y`
  --> src/lib.rs:3:5
";
        let entries = parse_block(block);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].severity, ErrorSeverity::Warning);
        assert_eq!(entries[0].line, 2);
        assert_eq!(entries[0].col, 4);
        assert_eq!(entries[0].message, "unused variable: `y`");
    }

    #[test]
    fn gnu_full_form_yields_correct_entry() {
        let entries = parse_block("main.c:10:5: error: 'foo' undeclared\n");
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.path, PathBuf::from("main.c"));
        assert_eq!(e.line, 9);
        assert_eq!(e.col, 4);
        assert_eq!(e.severity, ErrorSeverity::Error);
        assert_eq!(e.message, "'foo' undeclared");
    }

    #[test]
    fn gnu_warning_and_note_severities_map() {
        let warn = parse_block("a.c:1:1: warning: w\n");
        assert_eq!(warn[0].severity, ErrorSeverity::Warning);
        let note = parse_block("a.c:2:2: note: n\n");
        assert_eq!(note[0].severity, ErrorSeverity::Note);
        // "fatal error" maps to Error.
        let fatal = parse_block("a.c:3:3: fatal error: f\n");
        assert_eq!(fatal[0].severity, ErrorSeverity::Error);
    }

    #[test]
    fn gnu_short_form_line_only_defaults_to_info() {
        let entries = parse_block("Makefile:42: missing separator\n");
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.path, PathBuf::from("Makefile"));
        assert_eq!(e.line, 41);
        assert_eq!(e.col, 0, "short form has no column → 0");
        assert_eq!(e.severity, ErrorSeverity::Info);
        assert_eq!(e.message, "missing separator");
    }

    #[test]
    fn non_matching_lines_yield_no_entries_and_no_panic() {
        let block = "\
   Compiling something
    Finished dev [unoptimized] target(s) in 1.23s
just some prose with a colon: not a location
==== running 3 tests ====
";
        assert!(parse_block(block).is_empty());
    }

    #[test]
    fn cargo_location_line_is_not_double_counted_by_gnu() {
        // The gnu parser must NOT also fire on rustc's `-->` location
        // line (it has leading whitespace); exactly one entry results.
        let block = "\
error[E0433]: failed to resolve
  --> src/a.rs:7:13
";
        let entries = parse_block(block);
        assert_eq!(
            entries.len(),
            1,
            "cargo block yields exactly one entry, got {entries:?}"
        );
        assert_eq!(entries[0].path, PathBuf::from("src/a.rs"));
    }

    #[test]
    fn parse_location_line_matches_cargo_arrow() {
        let loc = parse_location_line("  --> src/foo.rs:12:9").expect("cargo location");
        assert_eq!(loc.path, PathBuf::from("src/foo.rs"));
        assert_eq!(loc.line, 11, "rustc 1-based 12 → 0-based 11");
        assert_eq!(loc.col, 8, "rustc 1-based 9 → 0-based 8");
    }

    #[test]
    fn parse_location_line_matches_gnu_full() {
        let loc = parse_location_line("main.c:10:5: error: x").expect("gnu location");
        assert_eq!(loc.path, PathBuf::from("main.c"));
        assert_eq!(loc.line, 9, "gnu 1-based 10 → 0-based 9");
        assert_eq!(loc.col, 4, "gnu 1-based 5 → 0-based 4");
    }

    #[test]
    fn parse_location_line_rejects_plain_text() {
        assert_eq!(parse_location_line("just some prose, not a location"), None);
        assert_eq!(parse_location_line("   Compiling foo v0.1.0"), None);
    }

    // ── CM.3c: match_severity + scan_severities ──────────────────────────

    #[test]
    fn match_severity_maps_cargo_headers() {
        assert_eq!(
            match_severity("error[E0308]: mismatched types"),
            Some(ErrorSeverity::Error)
        );
        assert_eq!(
            match_severity("error: aborting due to 1 previous error"),
            Some(ErrorSeverity::Error)
        );
        assert_eq!(
            match_severity("warning: unused variable: `y`"),
            Some(ErrorSeverity::Warning)
        );
    }

    #[test]
    fn match_severity_maps_gnu_full_form() {
        assert_eq!(
            match_severity("main.c:10:5: error: 'foo' undeclared"),
            Some(ErrorSeverity::Error)
        );
        assert_eq!(
            match_severity("a.c:1:1: warning: w"),
            Some(ErrorSeverity::Warning)
        );
        assert_eq!(
            match_severity("a.c:2:2: note: n"),
            Some(ErrorSeverity::Note)
        );
        assert_eq!(
            match_severity("a.c:3:3: fatal error: f"),
            Some(ErrorSeverity::Error)
        );
    }

    #[test]
    fn match_severity_none_on_non_severity_lines() {
        // Progress, the rustc `-->` location line (no keyword), gnu short
        // form (no keyword), prose, and empty all carry no severity keyword.
        assert_eq!(match_severity("   Compiling foo v0.1.0"), None);
        assert_eq!(match_severity("  --> src/foo.rs:12:9"), None);
        assert_eq!(match_severity("Makefile:42: missing separator"), None);
        assert_eq!(
            match_severity("just some prose with a colon: not a location"),
            None
        );
        assert_eq!(match_severity(""), None);
    }

    #[test]
    fn scan_severities_yields_absolute_line_numbers() {
        // Block: line 0 progress, line 1 cargo header (Error), line 2 the
        // `-->` location (no keyword), line 3 a warning header.
        let block = "\
Compiling foo
error[E0308]: mismatched types
  --> src/foo.rs:12:9
warning: unused
";
        assert_eq!(
            scan_severities(0, block),
            vec![(1, ErrorSeverity::Error), (3, ErrorSeverity::Warning)]
        );
    }

    #[test]
    fn scan_severities_respects_base_line_offset() {
        assert_eq!(
            scan_severities(10, "error: boom\n"),
            vec![(10, ErrorSeverity::Error)]
        );
        // gnu full-form on the 2nd line of a block based at line 5 → line 6.
        assert_eq!(
            scan_severities(5, "x\nmain.c:1:1: warning: w\n"),
            vec![(6, ErrorSeverity::Warning)]
        );
    }

    #[test]
    fn scan_severities_empty_and_plain() {
        assert!(scan_severities(0, "").is_empty());
        assert!(scan_severities(0, "Compiling\nFinished\n").is_empty());
    }

    // ── CM.3c: scan_location_lines ─────────────────────────────

    #[test]
    fn scan_location_lines_matches_cargo_arrow_and_gnu() {
        let block = "\
Compiling foo v0.1.0
  --> src/foo.rs:12:9
warning: unused
main.c:10:5: error: x
plain prose
";
        let locs = scan_location_lines(0, block);
        assert_eq!(locs.len(), 2, "two location lines expected");
        assert_eq!(locs[0].0, 1, "line 1 = cargo `-->`");
        assert_eq!(locs[1].0, 3, "line 3 = gnu full-form location");
    }

    #[test]
    fn scan_location_lines_respects_base_line_offset() {
        assert_eq!(
            scan_location_lines(10, "  --> src/a.rs:1:1\n"),
            vec![(10, 6, 14)]
        );
        assert_eq!(
            scan_location_lines(5, "x\nmain.c:3:3: error: e\n"),
            vec![(6, 0, 6)]
        );
    }

    #[test]
    fn scan_location_lines_empty_and_plain() {
        assert!(scan_location_lines(0, "").is_empty());
        assert!(scan_location_lines(0, "Compiling\nFinished\n").is_empty());
        assert!(scan_location_lines(0, "just some prose\n").is_empty());
    }
}
