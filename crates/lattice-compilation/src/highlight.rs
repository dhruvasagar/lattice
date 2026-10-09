//! Semantic highlighting for compiler diagnostics and error traces, one line at
//! a time.
//!
//! The streaming parsers in [`crate::parsers`] answer *where is the error* —
//! they produce [`ErrorEntry`](lattice_protocol::error_list::ErrorEntry)s for
//! the error list and the `<CR>` jump. This answers a different question:
//! *how should this text be read*. A rustc diagnostic is laid out for the eye
//! — a coloured header, a dim gutter, carets under the offending span — and
//! every one of those cues is gone once it has been captured through a pipe
//! and printed as plain text. What is left is a wall in which the one line
//! that matters looks like the nineteen around it.
//!
//! So this puts the cues back, as [`StyledSpan`]s over the plain text. It is
//! a *reading* aid, deliberately not a parser: it never fails, never rejects
//! a line, and a line it does not recognise is simply left unstyled.
//!
//! Two views read through it: `*compilation*`, where the pipe reader
//! applies it to every line the tool left uncoloured, and `:plugins`, which
//! shows a failed plugin build with it. One classifier, so a compiler report
//! reads the same wherever it is shown.
//!
//! ## Why spans, and not the compiler's own colours
//!
//! `cargo --color always` would hand over rustc's real SGR sequences, and
//! [`crate::ansi`] could convert them. But the text being highlighted is not
//! only cargo's: the same block carries wasmtime trap backtraces, an
//! `anyhow` cause chain, and lattice's own first line (`cargo build failed
//! (exit status: 101)`), none of which arrive coloured. It is also logged —
//! and escape sequences in `*messages*` are noise. One classifier over plain
//! text covers all of it and keeps the stored error a string a user can copy.
//!
//! ## What it recognises
//!
//! | Line | Styled as |
//! |---|---|
//! | `error[E0063]: …` / `error: …` | label in the error colour, message bold |
//! | `warning: …`, `note: …`, `help: …` | label in its own severity colour |
//! | `  --> src/lib.rs:43:14` | arrow dim, location as a link |
//! | `43 \|     code` | gutter dim, code untouched |
//! | `   \|     ^^^^ missing field` | gutter dim, marker + label in the diagnostic's colour |
//! | `   \|     --   ^^^ expected u8` | each run its own colour: `-` secondary, `^` the diagnostic's |
//! | `   = note: …` | `=` dim, label coloured |
//! | `Caused by:` / `… backtrace:` | bold |
//! | `   3: 0x1a2b - module!func` | index dim, address as a number, symbol as a function |
//! | `      at src/lib.rs:10:5` | `at` dim, location as a link |
//!
//! The marker colour follows the diagnostic it belongs to, which is the one
//! piece of state: a `^^^^` under a `warning:` is a warning's, so
//! [`DiagnosticHighlighter`] remembers the last header it saw.

use lattice_cells::{Style, StyledSpan};

/// Highlights diagnostics line by line. Feed lines in order — the marker
/// colour of a `^^^^` line comes from the header above it.
#[derive(Debug, Clone)]
pub struct DiagnosticHighlighter {
    /// The colour of the diagnostic currently being read.
    severity: Style,
}

impl Default for DiagnosticHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl DiagnosticHighlighter {
    pub fn new() -> Self {
        Self {
            // Text handed to this is, in practice, an error. Markers seen
            // before any header read as one.
            severity: Style::DiagnosticError,
        }
    }

    /// The spans for one line. Byte offsets within `line`; never overlapping,
    /// in ascending order. Empty for a line with nothing to say.
    pub fn line(&mut self, line: &str) -> Vec<StyledSpan> {
        let body = line.trim_start();
        let indent = line.len() - body.len();
        if body.is_empty() {
            return Vec::new();
        }
        let span = |start: usize, end: usize, style: Style| StyledSpan { start, end, style };
        let whole = |style: Style| vec![span(indent, line.len(), style)];

        // `error[E0063]: missing field` — the header of a diagnostic.
        if let Some((label_len, style, is_header)) = severity_label(body) {
            if is_header {
                self.severity = style;
            }
            let label_end = indent + label_len;
            let mut spans = vec![span(indent, label_end, style)];
            // Past the `: `. The message is what the reader came for.
            let message = line[label_end + 1..].trim_start();
            if !message.is_empty() {
                spans.push(span(line.len() - message.len(), line.len(), Style::Bold));
            }
            return spans;
        }

        // `--> src/lib.rs:43:14`, and `:::` for a secondary file.
        for arrow in ["--> ", "::: "] {
            if let Some(location) = body.strip_prefix(arrow) {
                let at = indent + arrow.len();
                let mut spans = vec![span(indent, at - 1, Style::Comment)];
                if !location.trim().is_empty() {
                    spans.push(span(at, line.len(), Style::Link));
                }
                return spans;
            }
        }

        // `= note: expected …` under a snippet.
        if let Some(rest) = body.strip_prefix("= ") {
            let mut spans = vec![span(indent, indent + 1, Style::Comment)];
            if let Some((label_len, style, _)) = severity_label(rest) {
                spans.push(span(indent + 2, indent + 2 + label_len, style));
            }
            return spans;
        }

        // `43 |     code` and `   |     ^^^^ label`.
        if let Some(pipe) = gutter_pipe(body) {
            let gutter_end = indent + pipe + 1;
            let mut spans = vec![span(indent, gutter_end, Style::Comment)];
            let has_line_number = body.as_bytes()[0].is_ascii_digit();
            let after = &line[gutter_end..];
            let marker = after.trim_start();
            // rustc always puts a space between the gutter and a marker. A
            // `|---|---|` table rule does not, and is not one.
            let spaced = after.starts_with(' ');
            if !has_line_number && spaced && marker.starts_with(MARKERS) {
                let at = gutter_end + (after.len() - marker.len());
                spans.extend(marker_spans(line, at, self.severity));
            }
            return spans;
        }

        // An error chain, and the backtraces a trap or a panic carries.
        if body == "Caused by:" || body.ends_with("backtrace:") {
            return whole(Style::Bold);
        }
        if let Some(spans) = frame_spans(line, indent, body) {
            return spans;
        }
        if body.strip_prefix("at ").is_some_and(looks_like_location) {
            return vec![
                span(indent, indent + 2, Style::Comment),
                span(indent + 3, line.len(), Style::Link),
            ];
        }

        // lattice's own summary line leads the block; it is the verdict.
        if body.starts_with("cargo build failed") {
            // Through the `(exit status: 101)`; a toolchain problem may follow.
            let end = body.find(')').map_or(body.len(), |close| close + 1);
            return vec![span(indent, indent + end, Style::DiagnosticError)];
        }
        // Trailers nobody needs to read twice.
        if body.starts_with('…') || body.starts_with("For more information about this error") {
            return whole(Style::Comment);
        }
        Vec::new()
    }
}

/// Highlight a whole block: one span list per line of `text`, in order.
pub fn highlight_diagnostics(text: &str) -> Vec<Vec<StyledSpan>> {
    let mut highlighter = DiagnosticHighlighter::new();
    text.lines().map(|line| highlighter.line(line)).collect()
}

/// A leading severity label — `error`, `error[E0063]`, `warning`, `note`,
/// `help` — followed by `:`. Returns the label's byte length (without the
/// colon), its style, and whether it opens a diagnostic (`note` / `help`
/// annotate the one above and must not recolour its markers).
fn severity_label(text: &str) -> Option<(usize, Style, bool)> {
    const LABELS: &[(&str, Style, bool)] = &[
        ("error", Style::DiagnosticError, true),
        ("warning", Style::DiagnosticWarning, true),
        ("note", Style::DiagnosticInfo, false),
        ("help", Style::DiagnosticHint, false),
    ];
    for (word, style, is_header) in LABELS {
        let Some(rest) = text.strip_prefix(word) else {
            continue;
        };
        // `error[E0063]` — the code is part of the label.
        let code_len = match rest.strip_prefix('[') {
            Some(code) => code.find(']').map(|close| close + 2)?,
            None => 0,
        };
        if rest[code_len..].starts_with(':') {
            return Some((word.len() + code_len, *style, *is_header));
        }
    }
    None
}

/// The characters rustc draws under a span.
const MARKERS: [char; 4] = ['^', '-', '_', '|'];

/// The spans of a marker line from `at` on: `--   ^^^ expected u8`.
///
/// One line can underline several spans, and they are not the same kind:
/// `^` is the primary one and takes the diagnostic's colour, `-` is a
/// secondary one, which rustc draws in blue. So each run is styled by what
/// it is, and the label that follows takes the colour of the run it labels —
/// the last one. Styling the line by its first character would paint the
/// error's own `^^^ expected …` as a secondary note whenever a `--` sits to
/// its left, which is the common shape of a type mismatch.
fn marker_spans(line: &str, at: usize, severity: Style) -> Vec<StyledSpan> {
    let mut spans = Vec::new();
    let mut pos = at;
    let mut last = Style::DiagnosticInfo;
    loop {
        let rest = &line[pos..];
        let run = rest
            .find(|c: char| !MARKERS.contains(&c))
            .unwrap_or(rest.len());
        if run == 0 {
            break;
        }
        last = if rest[..run].contains('^') {
            severity
        } else {
            Style::DiagnosticInfo
        };
        spans.push(StyledSpan {
            start: pos,
            end: pos + run,
            style: last,
        });
        let gap = rest[run..].len() - rest[run..].trim_start().len();
        pos += run + gap;
        if gap == 0 {
            break;
        }
    }
    if pos < line.len() {
        // What is left is the label. Joined to its run when nothing but
        // the one space separates them, so the common case stays one span.
        match spans.last_mut() {
            Some(prev) if prev.end + 1 == pos => prev.end = line.len(),
            _ => spans.push(StyledSpan {
                start: pos,
                end: line.len(),
                style: last,
            }),
        }
    }
    spans
}

/// The byte offset of the gutter's `|` when `body` (already left-trimmed)
/// opens with one: optional line number, optional spaces, `|`.
///
/// A bare `|` with no number is a gutter too — rustc indents it to line up
/// under the numbers, and `body` is trimmed, so it arrives here at offset 0.
/// Accepted: prose does not start a line with a pipe.
fn gutter_pipe(body: &str) -> Option<usize> {
    let digits = body.bytes().take_while(u8::is_ascii_digit).count();
    let spaces = body[digits..].bytes().take_while(|b| *b == b' ').count();
    let pipe = digits + spaces;
    (body.as_bytes().get(pipe) == Some(&b'|')).then_some(pipe)
}

/// `3: 0x1a2b - module!func` (wasmtime) or `3: core::panicking::panic` (std).
fn frame_spans(line: &str, indent: usize, body: &str) -> Option<Vec<StyledSpan>> {
    let digits = body.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || !body[digits..].starts_with(": ") {
        return None;
    }
    // Only an indented `N:` is a frame. Unindented, `2: something` is far
    // more likely to be prose or a numbered step.
    if indent == 0 {
        return None;
    }
    let mut spans = vec![StyledSpan {
        start: indent,
        end: indent + digits + 1,
        style: Style::Comment,
    }];
    let rest_at = indent + digits + 2;
    let rest = &line[rest_at..];
    let symbol_at = match rest.strip_prefix("0x") {
        Some(hex) => {
            let addr_len = 2 + hex.bytes().take_while(u8::is_ascii_hexdigit).count();
            spans.push(StyledSpan {
                start: rest_at,
                end: rest_at + addr_len,
                style: Style::Number,
            });
            // ` - ` separates the address from the symbol.
            match rest[addr_len..].strip_prefix(" - ") {
                Some(_) => rest_at + addr_len + 3,
                None => line.len(),
            }
        }
        None => rest_at,
    };
    if symbol_at < line.len() {
        spans.push(StyledSpan {
            start: symbol_at,
            end: line.len(),
            style: Style::Function,
        });
    }
    Some(spans)
}

/// `src/lib.rs:10:5` — a path followed by `:line`. Enough to tell a
/// backtrace's `at <location>` from a sentence that begins "at".
fn looks_like_location(text: &str) -> bool {
    text.contains(':')
        && !text.contains(' ')
        && text
            .rsplit(':')
            .next()
            .is_some_and(|last| !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// The text each span covers, with its style — what a reader would see
    /// coloured. Asserting on slices rather than offsets is what catches an
    /// off-by-one that colours the neighbouring space and still looks right.
    fn styled(line: &str) -> Vec<(&str, Style)> {
        DiagnosticHighlighter::new()
            .line(line)
            .into_iter()
            .map(|s| (&line[s.start..s.end], s.style))
            .collect()
    }

    #[test]
    fn an_error_header_colours_the_label_and_bolds_the_message() {
        assert_eq!(
            styled("error[E0063]: missing field `minor_modes`"),
            vec![
                ("error[E0063]", Style::DiagnosticError),
                ("missing field `minor_modes`", Style::Bold),
            ]
        );
        assert_eq!(
            styled("warning: unused variable: `x`"),
            vec![
                ("warning", Style::DiagnosticWarning),
                ("unused variable: `x`", Style::Bold),
            ]
        );
    }

    #[test]
    fn a_location_line_is_a_link() {
        assert_eq!(
            styled("  --> src/lib.rs:43:14"),
            vec![("-->", Style::Comment), ("src/lib.rs:43:14", Style::Link)]
        );
    }

    #[test]
    fn the_gutter_is_dim_and_the_code_is_left_alone() {
        // Colouring the source line would compete with the marker under it,
        // and it is not ours to highlight — it is in whatever language the
        // plugin is written in.
        assert_eq!(
            styled("43 |             &EventFilter {"),
            vec![("43 |", Style::Comment)]
        );
    }

    #[test]
    fn a_marker_takes_the_colour_of_its_diagnostic() {
        let mut h = DiagnosticHighlighter::new();
        let marker = "   |              ^^^^^^^^^^^ missing `minor_modes`";
        let colour = |h: &mut DiagnosticHighlighter| h.line(marker).last().unwrap().style;

        h.line("warning: unused import");
        assert_eq!(colour(&mut h), Style::DiagnosticWarning);
        h.line("error[E0063]: missing field");
        assert_eq!(colour(&mut h), Style::DiagnosticError);
        // A `note:` annotates the error above; it must not recolour it.
        h.line("note: required by this bound");
        assert_eq!(colour(&mut h), Style::DiagnosticError);

        let spans = h.line(marker);
        assert_eq!(
            &marker[spans[1].start..spans[1].end],
            "^^^^^^^^^^^ missing `minor_modes`"
        );
    }

    /// The classifier reads whatever a build prints, not only rustc. A
    /// table's rule line starts with a pipe and a dash and is no marker.
    /// The common shape of a type mismatch: a secondary `--` to the left of
    /// the primary `^^^`. Each is its own colour, and the message belongs to
    /// the primary — styled by the line's first character it read as a note.
    #[test]
    fn each_marker_run_on_a_line_takes_its_own_colour() {
        assert_eq!(
            styled("  |            --   ^^^ expected `u8`, found `&str`"),
            vec![
                ("|", Style::Comment),
                ("--", Style::DiagnosticInfo),
                ("^^^ expected `u8`, found `&str`", Style::DiagnosticError),
            ]
        );
        // A continuation bar under a secondary span.
        assert_eq!(
            styled("  |            |"),
            vec![("|", Style::Comment), ("|", Style::DiagnosticInfo)]
        );
    }

    #[test]
    fn a_table_rule_is_not_a_marker() {
        assert_eq!(styled("|---|---|"), vec![("|", Style::Comment)]);
        assert_eq!(styled("| a | b |"), vec![("|", Style::Comment)]);
    }

    #[test]
    fn a_secondary_marker_is_not_the_primary_colour() {
        let spans = styled("   |     ----- expected due to this");
        assert_eq!(
            spans[1],
            ("----- expected due to this", Style::DiagnosticInfo)
        );
    }

    #[test]
    fn an_attached_note_colours_only_its_label() {
        assert_eq!(
            styled("   = note: expected `u32`, found `String`"),
            vec![("=", Style::Comment), ("note", Style::DiagnosticInfo)]
        );
    }

    #[test]
    fn a_wasm_backtrace_frame_is_read_in_three_parts() {
        assert_eq!(
            styled("    0: 0x1a2b - lattice_init.wasm!register_events"),
            vec![
                ("0:", Style::Comment),
                ("0x1a2b", Style::Number),
                ("lattice_init.wasm!register_events", Style::Function),
            ]
        );
        assert_eq!(
            styled("wasm backtrace:"),
            vec![("wasm backtrace:", Style::Bold)]
        );
        assert_eq!(styled("Caused by:"), vec![("Caused by:", Style::Bold)]);
    }

    #[test]
    fn a_native_frame_and_its_location() {
        assert_eq!(
            styled("   3: core::panicking::panic_fmt"),
            vec![
                ("3:", Style::Comment),
                ("core::panicking::panic_fmt", Style::Function)
            ]
        );
        assert_eq!(
            styled("             at src/lib.rs:10:5"),
            vec![("at", Style::Comment), ("src/lib.rs:10:5", Style::Link)]
        );
    }

    #[test]
    fn prose_is_left_unstyled() {
        // The failure mode of a highlighter is colouring things that are not
        // what it thinks they are. None of these are diagnostics.
        for line in [
            "the build produced no component",
            "at this point the plugin is not loaded",
            "2: then restart the editor",
            "error handling is described below",
            "",
            "   ",
        ] {
            assert!(
                styled(line).is_empty(),
                "{line:?} was styled: {:?}",
                styled(line)
            );
        }
    }

    #[test]
    fn spans_never_overlap_and_stay_inside_the_line() {
        let block = "cargo build failed (exit status: 101)\n\
                     error[E0063]: missing field `minor_modes` in initializer\n  \
                     --> src/lib.rs:43:14\n   |\n43 |             &EventFilter {\n   \
                     |              ^^^^^^^^^^^ missing `minor_modes`\n   \
                     = help: add it\n\nFor more information about this error, try \
                     `rustc --explain E0063`.\nerror: could not compile `x`\n… 3 more lines";
        let all = highlight_diagnostics(block);
        assert_eq!(all.len(), block.lines().count());
        for (line, spans) in block.lines().zip(&all) {
            let mut at = 0;
            for s in spans {
                assert!(
                    s.start >= at && s.start < s.end && s.end <= line.len(),
                    "{line:?} {s:?}"
                );
                assert!(line.is_char_boundary(s.start) && line.is_char_boundary(s.end));
                at = s.end;
            }
        }
        // The verdict leads in the error colour; the truncation trailer is dim.
        assert_eq!(all[0][0].style, Style::DiagnosticError);
        assert_eq!(
            styled("cargo build failed (exit status: 101): the target is missing")[0].0,
            "cargo build failed (exit status: 101)"
        );
        assert_eq!(all.last().unwrap()[0].style, Style::Comment);
    }
}
