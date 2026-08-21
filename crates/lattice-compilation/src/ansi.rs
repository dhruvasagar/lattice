//! CM.5: ANSI escape handling over captured compilation output.
//!
//! Two jobs, and the first matters more than the second.
//!
//! **Stripping.** Captured stdout/stderr is a pipe, so cargo and rustc
//! disable colour on their own and the common case is already clean.
//! It stops being clean the moment anything forces colour —
//! `cargo build --color=always`, `CLICOLOR_FORCE=1`, `ls --color=always`,
//! or any runner that probes `TERM` instead of isatty. Then raw
//! `ESC[…m` bytes land in the `*compilation*` buffer **and** in front of
//! the [`crate::parser`] regexes, so `error[E0308]` silently stops being
//! recognised as an error. Stripping is therefore a correctness fix, not
//! a cosmetic one, and it is unconditional.
//!
//! **Colouring.** Having parsed the SGR parameters in order to remove
//! them, turning them into [`StyledSpan`]s is nearly free, and it is
//! what makes a forced-colour build read the way it does in a terminal.
//!
//! ## What is deliberately not modelled
//!
//! - **Background colour.** [`StyledSpan`] carries a foreground
//!   [`Style`] only; backgrounds are a separate axis (`RefineSpan`,
//!   DR.2) with different precedence, and opening it here would force
//!   an opinion about layering onto a producer that has none. `40`–`47`
//!   / `49` / `100`–`107` are parsed and dropped.
//! - **256-colour and truecolor.** `38;5;n` is honoured only for
//!   `n < 16` (where it names an ANSI slot); the cube and greyscale
//!   ramp, and `38;2;r;g;b`, are parsed to keep the parameter walk in
//!   sync and then dropped. Real compiler output uses the 16-colour
//!   palette — anstyle, which cargo builds on, emits nothing else.
//! - **Italic / underline / reverse.** [`Style`] is one value per span,
//!   not a set, so a span cannot be both "red" and "underlined". The
//!   colour is the information-bearing half and wins.
//!
//! ## Bold is bright
//!
//! `SGR 1` with a normal colour maps to that colour's **bright** slot,
//! which is how terminals have rendered bold-plus-colour since the
//! hardware did it. It is also what makes cargo's `bold red` `error:`
//! prefix arrive as bright red rather than losing one of its two
//! attributes. Bold with no colour maps to [`AnsiPalette::bold`].

use lattice_cells::{Style, StyledSpan};
use lattice_theme::{Color, ElementId, NamedColor};
use lattice_theme::{ColorRef, ElementName, ElementOwner, StyleSpec, ThemeRegistry};

/// The 4-bit ANSI colour slots, in SGR order: `0`–`7` normal,
/// `8`–`15` bright. Index with the SGR parameter minus its base
/// (`30` for normal, `90` for bright).
const ANSI_NAMES: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "bright-black",
    "bright-red",
    "bright-green",
    "bright-yellow",
    "bright-blue",
    "bright-magenta",
    "bright-cyan",
    "bright-white",
];

/// The terminal channel each slot paints on.
///
/// [`NamedColor`] is the 16-entry terminal vocabulary, so this is an
/// identity mapping with two naming seams worth stating: ratatui (and
/// therefore [`NamedColor`]) calls slot 7 `Gray` and slot 15 `White`,
/// where ANSI calls them `white` and `bright white`. Slot 8 is
/// `DarkGray`, ANSI's `bright black`.
const ANSI_CHANNELS: [NamedColor; 16] = [
    NamedColor::Black,
    NamedColor::Red,
    NamedColor::Green,
    NamedColor::Yellow,
    NamedColor::Blue,
    NamedColor::Magenta,
    NamedColor::Cyan,
    NamedColor::Gray,
    NamedColor::DarkGray,
    NamedColor::LightRed,
    NamedColor::LightGreen,
    NamedColor::LightYellow,
    NamedColor::LightBlue,
    NamedColor::LightMagenta,
    NamedColor::LightCyan,
    NamedColor::White,
];

/// The interned theme elements CM.5 paints ANSI output with.
///
/// `Copy` so the pipe readers can each hold one without sharing state
/// — the ids are process-stable once registered.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AnsiPalette {
    /// Indexed by 4-bit ANSI slot (see [`ANSI_NAMES`]).
    pub colors: [ElementId; 16],
    /// Bold with no colour attached.
    pub bold: ElementId,
}

impl AnsiPalette {
    /// Register `compilation.ansi.*` and intern the ids. Idempotent —
    /// [`ThemeRegistry::register`] returns the existing id for a name
    /// already present, so a second boot (or a second install in a
    /// test) does not duplicate elements.
    ///
    /// The defaults are [`ColorRef::Literal`] rather than palette
    /// references on purpose. The `ansi.*` **palette** family exists
    /// already, but every one of the 21 builtin palettes defines its
    /// entries as the same `Color::Named(..)` pass-through — they are
    /// terminal channels, not palette-specific accents, so a per-theme
    /// key would be 21 identical copies of one value. A theme that
    /// genuinely wants to retune them (a light theme where the
    /// terminal's red is unreadable) overrides them **by element
    /// name** through the T.9 override path, which needs no palette
    /// key. Promoting these to a core `ansi.*` element family is the
    /// upgrade path if a second consumer (terminal, agent output)
    /// appears.
    pub fn register(registry: &dyn ThemeRegistry) -> Self {
        let owner = ElementOwner::Mode(std::borrow::Cow::Borrowed("compilation-mode"));
        let mut colors = [ElementId::INVALID; 16];
        for (slot, name) in ANSI_NAMES.iter().enumerate() {
            colors[slot] = registry.register(
                ElementName::from(format!("compilation.ansi.{name}")),
                owner.clone(),
                StyleSpec::new().fg(ColorRef::Literal(Color::Named(ANSI_CHANNELS[slot]))),
                "ANSI colour in captured compilation output.",
            );
        }
        let bold = registry.register(
            ElementName::from_static("compilation.ansi.bold"),
            owner,
            StyleSpec::new().bold(),
            "Bold (SGR 1) with no colour in captured compilation output.",
        );
        Self { colors, bold }
    }

    /// The style an [`SgrState`] paints as, or `None` when the state
    /// carries nothing renderable.
    fn style(&self, state: SgrState) -> Option<Style> {
        match (state.fg, state.bold) {
            // Bold is bright: promote a normal slot to its bright peer.
            (Some(slot), true) if slot < 8 => Some(Style::Element(self.colors[slot as usize + 8])),
            (Some(slot), _) => Some(Style::Element(self.colors[slot as usize])),
            (None, true) => Some(Style::Element(self.bold)),
            (None, false) => None,
        }
    }
}

/// Active SGR attributes.
///
/// Carried **across** lines by the caller: a producer is free to set a
/// colour on one line and reset it three lines later, and each pipe
/// reader owns one of these for the life of the pipe. Not carried
/// across pipes — stdout and stderr are independent streams whose
/// interleaving in the buffer says nothing about either one's state.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SgrState {
    /// Active 4-bit foreground slot.
    fg: Option<u8>,
    bold: bool,
}

impl SgrState {
    /// Apply one SGR parameter list (the numbers between `ESC[` and
    /// `m`). An empty list is `SGR 0` — reset — per ECMA-48.
    fn apply(&mut self, params: &str) {
        let mut it = params.split(';').peekable();
        if params.is_empty() {
            *self = Self::default();
            return;
        }
        while let Some(raw) = it.next() {
            // An empty parameter is a defaulted 0, not a parse error.
            let code: u16 = if raw.is_empty() {
                0
            } else {
                match raw.parse() {
                    Ok(c) => c,
                    Err(_) => {
                        tracing::debug!(param = raw, "compilation: unparsable SGR parameter");
                        continue;
                    }
                }
            };
            match code {
                0 => *self = Self::default(),
                1 => self.bold = true,
                // 21 is "doubly underlined" on some terminals and
                // "not bold" on others; 22 is unambiguous. Treat both
                // as clearing bold, which is what the ambiguous one
                // means in every producer that emits it.
                21 | 22 => self.bold = false,
                30..=37 => self.fg = Some((code - 30) as u8),
                90..=97 => self.fg = Some((code - 90) as u8 + 8),
                39 => self.fg = None,
                // Extended colour. Consume its parameters so the walk
                // stays aligned, then keep only what maps to a slot.
                38 => match it.next() {
                    Some("5") => {
                        let n = it.next().and_then(|v| v.parse::<u16>().ok());
                        self.fg = match n {
                            Some(n) if n < 16 => Some(n as u8),
                            _ => None,
                        };
                    }
                    Some("2") => {
                        for _ in 0..3 {
                            it.next();
                        }
                        self.fg = None;
                    }
                    _ => {}
                },
                // Background: parsed to stay aligned, then dropped.
                48 => match it.next() {
                    Some("5") => {
                        it.next();
                    }
                    Some("2") => {
                        for _ in 0..3 {
                            it.next();
                        }
                    }
                    _ => {}
                },
                // Everything else (italic, underline, reverse,
                // backgrounds, fonts) is a no-op by design — see the
                // module docs.
                _ => {}
            }
        }
    }
}

/// One line with every escape sequence removed, plus the spans that
/// describe what colour the surviving bytes were.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanLine {
    pub text: String,
    pub spans: Vec<StyledSpan>,
}

/// Strip escape sequences out of one line, returning the clean text
/// and its spans. `state` is updated in place so the next line
/// continues where this one left off.
///
/// Spans are byte offsets **into `text`** (the clean output), because
/// that is what lands in the buffer and what the renderer indexes.
///
/// A bare `\r` that is not ending the line restarts it: everything
/// before the carriage return is discarded, along with its spans. That
/// is what a terminal does with cargo's progress redraw, and without
/// it a build streams every intermediate `Compiling …` state
/// concatenated into one unreadable row.
pub fn clean_line(raw: &str, state: &mut SgrState, palette: Option<&AnsiPalette>) -> CleanLine {
    let mut out = CleanLine::default();
    let bytes = raw.as_bytes();
    let mut i = 0;
    // Byte offset in `out.text` where the current style run began.
    let mut run_start = 0usize;
    let mut run_style = palette.and_then(|p| p.style(*state));

    // Close the open run at the current end of `out.text`.
    fn close(out: &mut CleanLine, run_start: usize, style: Option<Style>) {
        if let Some(style) = style
            && out.text.len() > run_start
        {
            out.spans.push(StyledSpan {
                start: run_start,
                end: out.text.len(),
                style,
            });
        }
    }

    while i < bytes.len() {
        // Bulk-copy the run up to the next control byte. Copying one
        // scalar at a time instead costs ~2x on uncoloured output,
        // which is the case nearly every build takes — and slicing at
        // a control byte is inherently UTF-8-safe, since neither
        // `ESC` nor `\r` can appear inside a multi-byte scalar.
        let run_end = bytes[i..]
            .iter()
            .position(|&b| b == 0x1b || b == b'\r')
            .map_or(bytes.len(), |off| i + off);
        if run_end > i {
            match std::str::from_utf8(&bytes[i..run_end]) {
                Ok(s) => out.text.push_str(s),
                Err(_) => {
                    // Unreachable from a `&str` input; skipping beats
                    // panicking if the slicing above ever rots.
                    tracing::debug!("compilation: skipping malformed UTF-8 in output");
                }
            }
            i = run_end;
            continue;
        }
        match bytes[i] {
            0x1b => {
                let (consumed, sgr) = scan_escape(&bytes[i..]);
                if consumed == 0 {
                    // A trailing lone ESC with nothing after it.
                    // Drop it rather than emitting it into the buffer.
                    break;
                }
                if let Some(params) = sgr {
                    let next = {
                        let mut probe = *state;
                        probe.apply(params);
                        probe
                    };
                    if next != *state {
                        close(&mut out, run_start, run_style);
                        *state = next;
                        run_start = out.text.len();
                        run_style = palette.and_then(|p| p.style(*state));
                    }
                }
                i += consumed;
            }
            b'\r' => {
                // Line restart. Drop the text and every span so far;
                // the style state deliberately survives, because the
                // producer did not reset it.
                out.text.clear();
                out.spans.clear();
                run_start = 0;
                i += 1;
            }
            // Unreachable: the bulk-copy above consumes every byte
            // that is neither `ESC` nor `\r`. Advancing keeps the loop
            // total rather than relying on that argument.
            _ => i += 1,
        }
    }
    close(&mut out, run_start, run_style);
    out
}

/// Measure the escape sequence at the head of `bytes`.
///
/// Returns `(bytes_consumed, Some(sgr_params))` for a CSI sequence
/// terminated by `m`, `(bytes_consumed, None)` for any other escape
/// sequence, and `(0, None)` when the sequence is incomplete (a lone
/// trailing `ESC`).
fn scan_escape(bytes: &[u8]) -> (usize, Option<&str>) {
    if bytes.len() < 2 {
        return (0, None);
    }
    match bytes[1] {
        // CSI: ESC [ params intermediates final
        b'[' => {
            let mut j = 2;
            while j < bytes.len() && (0x30..=0x3f).contains(&bytes[j]) {
                j += 1;
            }
            let params_end = j;
            while j < bytes.len() && (0x20..=0x2f).contains(&bytes[j]) {
                j += 1;
            }
            if j >= bytes.len() {
                return (0, None);
            }
            let final_byte = bytes[j];
            let consumed = j + 1;
            if final_byte == b'm' {
                // Private-parameter sequences (`ESC[?…m`) are not SGR.
                let params = &bytes[2..params_end];
                if params.first().is_some_and(|b| (0x3c..=0x3f).contains(b)) {
                    return (consumed, None);
                }
                match std::str::from_utf8(params) {
                    Ok(s) => (consumed, Some(s)),
                    Err(_) => (consumed, None),
                }
            } else {
                (consumed, None)
            }
        }
        // OSC: ESC ] ... (BEL | ESC \)
        b']' => {
            let mut j = 2;
            while j < bytes.len() {
                if bytes[j] == 0x07 {
                    return (j + 1, None);
                }
                if bytes[j] == 0x1b && bytes.get(j + 1) == Some(&b'\\') {
                    return (j + 2, None);
                }
                j += 1;
            }
            // Unterminated OSC: swallow the rest of the line rather
            // than leaking its payload as text.
            (bytes.len(), None)
        }
        // Two-byte escapes (ESC c, ESC 7, ESC =, ...).
        _ => (2, None),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    /// A palette with recognisable ids so assertions can name slots.
    fn palette() -> AnsiPalette {
        let mut colors = [ElementId::INVALID; 16];
        for (slot, c) in colors.iter_mut().enumerate() {
            *c = ElementId(slot as u32);
        }
        AnsiPalette {
            colors,
            bold: ElementId(100),
        }
    }

    fn clean(raw: &str) -> CleanLine {
        let p = palette();
        let mut st = SgrState::default();
        clean_line(raw, &mut st, Some(&p))
    }

    fn slot(span: &StyledSpan) -> u32 {
        match span.style {
            Style::Element(id) => id.0,
            other => panic!("expected an element style, got {other:?}"),
        }
    }

    #[test]
    fn plain_text_is_untouched_and_unspanned() {
        let out = clean("error: something broke");
        assert_eq!(out.text, "error: something broke");
        assert!(out.spans.is_empty());
    }

    #[test]
    fn sgr_is_stripped_from_the_text() {
        let out = clean("\u{1b}[31merror\u{1b}[0m: broke");
        assert_eq!(out.text, "error: broke");
    }

    #[test]
    fn sgr_colour_becomes_a_span_over_the_clean_offsets() {
        let out = clean("\u{1b}[31merror\u{1b}[0m: broke");
        assert_eq!(out.spans.len(), 1);
        assert_eq!((out.spans[0].start, out.spans[0].end), (0, 5));
        assert_eq!(slot(&out.spans[0]), 1, "slot 1 is red");
    }

    #[test]
    fn bold_plus_normal_colour_is_the_bright_slot() {
        // cargo's `error:` prefix: bold red.
        let out = clean("\u{1b}[1m\u{1b}[31merror\u{1b}[0m");
        assert_eq!(out.text, "error");
        assert_eq!(out.spans.len(), 1);
        assert_eq!(slot(&out.spans[0]), 9, "bold red promotes to bright red");
    }

    #[test]
    fn bold_plus_a_bright_colour_stays_put() {
        let out = clean("\u{1b}[1;91mx\u{1b}[0m");
        assert_eq!(slot(&out.spans[0]), 9);
    }

    #[test]
    fn bold_without_colour_uses_the_bold_element() {
        let out = clean("\u{1b}[1mwarning\u{1b}[0m");
        assert_eq!(out.spans.len(), 1);
        assert_eq!(slot(&out.spans[0]), 100);
    }

    #[test]
    fn combined_parameters_in_one_sequence() {
        let out = clean("\u{1b}[1;32mok\u{1b}[m done");
        assert_eq!(out.text, "ok done");
        assert_eq!(out.spans.len(), 1);
        assert_eq!((out.spans[0].start, out.spans[0].end), (0, 2));
        assert_eq!(slot(&out.spans[0]), 10, "bold green → bright green");
    }

    #[test]
    fn bare_sgr_m_is_a_reset() {
        let out = clean("\u{1b}[31mred\u{1b}[mplain");
        assert_eq!(out.text, "redplain");
        assert_eq!(out.spans.len(), 1);
        assert_eq!((out.spans[0].start, out.spans[0].end), (0, 3));
    }

    #[test]
    fn adjacent_runs_produce_adjacent_spans() {
        let out = clean("\u{1b}[31ma\u{1b}[32mb\u{1b}[0mc");
        assert_eq!(out.text, "abc");
        assert_eq!(out.spans.len(), 2);
        assert_eq!((out.spans[0].start, out.spans[0].end), (0, 1));
        assert_eq!((out.spans[1].start, out.spans[1].end), (1, 2));
        assert_eq!(slot(&out.spans[1]), 2);
    }

    #[test]
    fn state_carries_across_lines() {
        let p = palette();
        let mut st = SgrState::default();
        let first = clean_line("\u{1b}[33mopen", &mut st, Some(&p));
        assert_eq!(first.spans.len(), 1);
        // No reset was emitted, so the second line is still yellow.
        let second = clean_line("still yellow\u{1b}[0m", &mut st, Some(&p));
        assert_eq!(second.text, "still yellow");
        assert_eq!(second.spans.len(), 1);
        assert_eq!((second.spans[0].start, second.spans[0].end), (0, 12));
        assert_eq!(slot(&second.spans[0]), 3);
        assert_eq!(st, SgrState::default(), "the trailing reset landed");
    }

    #[test]
    fn non_sgr_csi_is_stripped_without_affecting_style() {
        // Cursor-up + erase-line, as a progress renderer emits.
        let out = clean("\u{1b}[31ma\u{1b}[2K\u{1b}[1Ab\u{1b}[0m");
        assert_eq!(out.text, "ab");
        assert_eq!(out.spans.len(), 1);
        assert_eq!((out.spans[0].start, out.spans[0].end), (0, 2));
    }

    #[test]
    fn osc_hyperlink_payload_does_not_leak_into_the_text() {
        let out = clean("see \u{1b}]8;;https://example.com\u{7}link\u{1b}]8;;\u{7} end");
        assert_eq!(out.text, "see link end");
    }

    #[test]
    fn osc_terminated_by_string_terminator() {
        let out = clean("a\u{1b}]0;title\u{1b}\\b");
        assert_eq!(out.text, "ab");
    }

    #[test]
    fn private_csi_ending_in_m_is_not_treated_as_sgr() {
        let out = clean("\u{1b}[?1049mx");
        assert_eq!(out.text, "x");
        assert!(out.spans.is_empty());
    }

    #[test]
    fn carriage_return_restarts_the_line() {
        let out = clean("   Compiling a\r   Compiling b\r   Compiling c");
        assert_eq!(out.text, "   Compiling c");
    }

    #[test]
    fn carriage_return_drops_the_spans_it_discards() {
        let out = clean("\u{1b}[31mgone\r\u{1b}[32mkept\u{1b}[0m");
        assert_eq!(out.text, "kept");
        assert_eq!(out.spans.len(), 1);
        assert_eq!((out.spans[0].start, out.spans[0].end), (0, 4));
        assert_eq!(slot(&out.spans[0]), 2);
    }

    #[test]
    fn extended_256_colour_below_16_maps_to_its_slot() {
        let out = clean("\u{1b}[38;5;4mx\u{1b}[0m");
        assert_eq!(out.text, "x");
        assert_eq!(slot(&out.spans[0]), 4);
    }

    #[test]
    fn extended_256_colour_above_16_is_dropped_without_desyncing() {
        // The `1` after the colour must still be read as bold.
        let out = clean("\u{1b}[38;5;208;1mx\u{1b}[0m");
        assert_eq!(out.text, "x");
        assert_eq!(out.spans.len(), 1);
        assert_eq!(slot(&out.spans[0]), 100, "colour dropped, bold survives");
    }

    #[test]
    fn truecolour_is_dropped_without_desyncing() {
        let out = clean("\u{1b}[38;2;255;128;0;1mx\u{1b}[0m");
        assert_eq!(out.spans.len(), 1);
        assert_eq!(slot(&out.spans[0]), 100);
    }

    #[test]
    fn background_parameters_are_consumed_not_painted() {
        let out = clean("\u{1b}[48;5;22;31mx\u{1b}[0m");
        assert_eq!(out.text, "x");
        assert_eq!(out.spans.len(), 1);
        assert_eq!(slot(&out.spans[0]), 1, "the fg after the bg still applies");
    }

    #[test]
    fn unparsable_parameter_is_skipped_and_the_rest_still_applies() {
        // `1:2` is a valid parameter *string* (colons are parameter
        // bytes, the ITU sub-parameter form) but not a number. It is
        // logged and skipped; the `31` after it must still land.
        let out = clean("\u{1b}[1:2;31mx\u{1b}[0m");
        assert_eq!(out.text, "x");
        assert_eq!(out.spans.len(), 1);
        assert_eq!(slot(&out.spans[0]), 1);
    }

    #[test]
    fn a_csi_with_a_non_m_final_byte_ends_at_that_byte() {
        // `z` is a valid CSI final byte, so `ESC[3z` is a complete
        // (non-SGR) sequence and everything after it is literal text.
        // Pinned because the natural misreading — "scan to the next
        // `m`" — would eat the visible text instead.
        let out = clean("\u{1b}[3zkept");
        assert_eq!(out.text, "kept");
        assert!(out.spans.is_empty());
    }

    #[test]
    fn trailing_lone_escape_is_dropped() {
        let out = clean("tail\u{1b}");
        assert_eq!(out.text, "tail");
    }

    #[test]
    fn incomplete_csi_at_end_of_line_is_dropped() {
        let out = clean("tail\u{1b}[31");
        assert_eq!(out.text, "tail");
    }

    #[test]
    fn multibyte_text_keeps_byte_offsets_aligned() {
        let out = clean("\u{1b}[31mré\u{1b}[0m→");
        assert_eq!(out.text, "ré→");
        assert_eq!(out.spans.len(), 1);
        // "ré" is three bytes; the span must end there, not at 2.
        assert_eq!((out.spans[0].start, out.spans[0].end), (0, 3));
    }

    #[test]
    fn no_palette_strips_but_does_not_span() {
        let mut st = SgrState::default();
        let out = clean_line("\u{1b}[31merror\u{1b}[0m", &mut st, None);
        assert_eq!(out.text, "error");
        assert!(
            out.spans.is_empty(),
            "stripping is unconditional; colouring needs a palette"
        );
    }

    #[test]
    fn reset_with_no_open_run_emits_nothing() {
        let out = clean("\u{1b}[0mplain\u{1b}[0m");
        assert_eq!(out.text, "plain");
        assert!(out.spans.is_empty());
    }

    #[test]
    fn empty_line_is_empty() {
        let out = clean("");
        assert_eq!(out.text, "");
        assert!(out.spans.is_empty());
    }

    #[test]
    fn a_line_that_is_only_escapes_is_empty_text() {
        let out = clean("\u{1b}[31m\u{1b}[0m");
        assert_eq!(out.text, "");
        assert!(out.spans.is_empty());
    }
}
