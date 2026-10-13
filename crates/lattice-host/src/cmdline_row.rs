//! GD.5b: what the `:` line shows besides the text being typed.
//!
//! Two things, both content rather than paint, so both renderers take
//! them from here:
//!
//! - where the caret is, in columns ([`caret_cols`]);
//! - the hints that trail the line ([`trailing_hints`]): the
//!   "press a chord" cue, the completion count, a live error or a
//!   parameter hint.
//!
//! The terminal renderer had these inline in its draw function and the
//! GPUI window had neither — its command line showed the text and no
//! caret at all. Each renderer still decides how a hint *looks* (the
//! terminal has italics and a dim grey; the window has theme colours),
//! keyed on [`HintKind`].

use crate::excommand::CommandLineDecorations;

/// What a trailing hint is, so a renderer can style it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintKind {
    /// The next keypress is consumed as a chord (`:describe-key`).
    Cue,
    /// `(selected/total)` while the completion popup is open.
    Count,
    /// The line does not parse: unknown command, bad arguments.
    Error,
    /// The resolved command's argument shape.
    Param,
}

/// One hint after the typed text. `text` carries its own leading
/// spacing, so a renderer appends hints in order with nothing between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    pub text: String,
    pub kind: HintKind,
}

/// The hints trailing the `:` line, in the order they are shown.
///
/// - `auto_submit` — a missing-argument prompt is armed and the next
///   chord runs the command; wins over the softer `chord_capture` tag,
///   which only says the cursor is in a chord argument.
/// - `completion` — `(selected, total)` of the open completion popup.
/// - `decorations` — an error wins the last slot; otherwise the
///   parameter hint, unless the completion popup is open, which already
///   says what can go there.
pub fn trailing_hints(
    auto_submit: bool,
    chord_capture: bool,
    completion: Option<(usize, usize)>,
    decorations: Option<&CommandLineDecorations>,
) -> Vec<Hint> {
    let mut out = Vec::new();
    if auto_submit {
        out.push(Hint {
            text: "press a chord".into(),
            kind: HintKind::Cue,
        });
    } else if chord_capture {
        out.push(Hint {
            text: "(chord)".into(),
            kind: HintKind::Cue,
        });
    }
    let popup_open = completion.is_some_and(|(_, total)| total > 0);
    if let Some((selected, total)) = completion
        && total > 0
    {
        out.push(Hint {
            text: format!("  ({}/{})", selected + 1, total),
            kind: HintKind::Count,
        });
    }
    if let Some(d) = decorations {
        if let Some(err) = &d.error {
            out.push(Hint {
                text: format!("  {err}"),
                kind: HintKind::Error,
            });
        } else if let Some(ph) = &d.param_hint
            && !popup_open
        {
            out.push(Hint {
                text: format!("  {ph}"),
                kind: HintKind::Param,
            });
        }
    }
    out
}

/// The byte in `text` the caret sits before: `caret_byte` clamped to
/// the text and moved back onto a character boundary, so it is always
/// safe to slice at.
pub fn caret_byte(text: &str, caret_byte: usize) -> usize {
    let mut b = caret_byte.min(text.len());
    while b > 0 && !text.is_char_boundary(b) {
        b -= 1;
    }
    b
}

/// How many columns of `text` lie before the caret. Characters, not
/// bytes: `:e café` with the caret at the end is 6 columns in, not 7.
pub fn caret_cols(text: &str, caret_byte_in: usize) -> usize {
    text[..caret_byte(text, caret_byte_in)].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deco(error: Option<&str>, param: Option<&str>) -> CommandLineDecorations {
        CommandLineDecorations {
            spans: Vec::new(),
            error: error.map(str::to_string),
            param_hint: param.map(str::to_string),
        }
    }

    fn texts(h: &[Hint]) -> Vec<(&str, HintKind)> {
        h.iter().map(|h| (h.text.as_str(), h.kind)).collect()
    }

    #[test]
    fn nothing_to_say_is_no_hints() {
        assert!(trailing_hints(false, false, None, None).is_empty());
        assert!(trailing_hints(false, false, Some((0, 0)), Some(&deco(None, None))).is_empty());
    }

    #[test]
    fn the_armed_cue_wins_over_the_chord_tag() {
        assert_eq!(
            texts(&trailing_hints(true, true, None, None)),
            vec![("press a chord", HintKind::Cue)]
        );
        assert_eq!(
            texts(&trailing_hints(false, true, None, None)),
            vec![("(chord)", HintKind::Cue)]
        );
    }

    #[test]
    fn the_count_is_one_based() {
        assert_eq!(
            texts(&trailing_hints(false, false, Some((0, 12)), None)),
            vec![("  (1/12)", HintKind::Count)]
        );
    }

    #[test]
    fn an_error_wins_over_the_parameter_hint() {
        let d = deco(Some("unknown command"), Some("<file>"));
        assert_eq!(
            texts(&trailing_hints(false, false, None, Some(&d))),
            vec![("  unknown command", HintKind::Error)]
        );
    }

    /// The popup already lists what can go in the slot; the parameter
    /// hint beside it is noise. An error is not.
    #[test]
    fn the_parameter_hint_yields_to_an_open_completion_popup() {
        let d = deco(None, Some("<file>"));
        assert_eq!(
            texts(&trailing_hints(false, false, None, Some(&d))),
            vec![("  <file>", HintKind::Param)]
        );
        assert_eq!(
            texts(&trailing_hints(false, false, Some((2, 5)), Some(&d))),
            vec![("  (3/5)", HintKind::Count)]
        );
        let e = deco(Some("bad range"), Some("<file>"));
        assert_eq!(
            texts(&trailing_hints(false, false, Some((2, 5)), Some(&e))),
            vec![
                ("  (3/5)", HintKind::Count),
                ("  bad range", HintKind::Error)
            ]
        );
    }

    #[test]
    fn the_caret_is_counted_in_columns_and_never_splits_a_character() {
        assert_eq!(caret_cols("set wrap", 3), 3);
        assert_eq!(caret_cols("set wrap", 99), 8, "clamped to the text");
        // "é" is two bytes: byte 7 is the end of "e café".
        assert_eq!(caret_cols("e café", 7), 6);
        // Byte 6 is inside "é": back to the boundary before it.
        assert_eq!(caret_byte("e café", 6), 5);
        assert_eq!(caret_cols("e café", 6), 5);
        assert_eq!(caret_cols("", 0), 0);
    }
}
