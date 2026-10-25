//! `gS{motion}` — wrap text in a snippet (snippet-integrity SI.10).
//!
//! The operator takes the span its motion or text object covers (or the
//! Visual selection) and opens the snippet picker over it; the snippet you
//! pick expands in its place, with that text as its `$TM_SELECTED_TEXT`.
//! Three pieces, all owned here:
//!
//! - **`operator:snippet-wrap`** — registered into the grammar like every
//!   operator, so it composes with every motion and text object. It removes
//!   nothing: it opens `:picker snippets --over <range>`.
//! - **The wrap rows** — [`crate::picker_sources`] reads `--over` and gives
//!   each row the range ([`lattice_picker::RoutingPayload::ExpandSnippet`]'s
//!   `over`), so the MRU still keys on the snippet.
//! - **`action:snippet-wrap`** — what a wrap row accepts into: it emits
//!   [`Effect::ExpandSnippetOver`], and the host expands.
//!
//! Closing the picker leaves the text exactly as it was, because nothing
//! was taken from it — the alternative, stashing the text until the next
//! expansion (UltiSnips' visual placeholder), is invisible state.

use std::sync::Arc;

use lattice_core::Buffer;
use lattice_grammar::args::{ArgKind, ArgSpec, ArgValue, Args};
use lattice_grammar::effect::Effect;
use lattice_grammar::error::CommandError;
use lattice_grammar::registry::{ActionSpec, OperatorContext, OperatorSpec};
use lattice_grammar::{CommandRegistry, OperatorId};
use lattice_protocol::position::{Position, Range};

/// The operator's registered name; the host binds `gS` to it by name.
pub const SNIPPET_WRAP_OPERATOR: &str = "operator:snippet-wrap";

/// The action a wrap row accepts into: `(snippet, range)`.
pub const SNIPPET_WRAP_ACTION: &str = "action:snippet-wrap";

/// The `:picker snippets` argument that turns its rows into wrap rows,
/// followed by the range ([`encode_range`]).
pub const OVER_FLAG: &str = "--over";

/// `range` as a picker argument: `line:byte-line:byte`, zero-based.
pub fn encode_range(range: Range) -> String {
    format!(
        "{}:{}-{}:{}",
        range.start.line, range.start.byte, range.end.line, range.end.byte
    )
}

/// The range [`encode_range`] wrote, or `None` for anything else.
pub fn decode_range(text: &str) -> Option<Range> {
    let (start, end) = text.split_once('-')?;
    let position = |s: &str| -> Option<Position> {
        let (line, byte) = s.split_once(':')?;
        Some(Position::new(line.parse().ok()?, byte.parse().ok()?))
    };
    Some(Range::new(position(start)?, position(end)?))
}

/// The text a wrap replaces. Charwise, the span itself. Linewise, from the
/// first line's first non-blank to the end of the last line's text: the
/// first line's indentation stays outside, so the snippet lands at the
/// block's indentation and its continuation lines follow it; the last
/// newline stays outside, so the snippet's own lines decide where the block
/// ends. `None` when that leaves nothing to wrap.
pub fn wrap_range(buffer: &Buffer, range: Range, linewise: bool) -> Option<Range> {
    let range = if linewise {
        let (first, last) = lattice_grammar::range::span_to_whole_lines(
            range.start.line,
            range.start.byte,
            range.end.line,
            range.end.byte,
        );
        let indent = lattice_core::IndentUnit::indent_len(&buffer.line(first)?) as u32;
        Range::new(
            Position::new(first, indent),
            Position::new(last, buffer.line_byte_len(last)),
        )
    } else {
        range
    };
    (range.start < range.end).then_some(range)
}

/// `text`, taken from a buffer where its first line starts after the
/// indentation `base`, as `$TM_SELECTED_TEXT` carries it: each later line
/// loses `base` (as much of it as the line shares), so the snippet decides
/// where those lines sit and their extra depth is kept. VS Code's rule —
/// without it a wrapped block is indented twice.
pub fn selection_relative_to(text: &str, base: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
            let shared = line
                .bytes()
                .zip(base.bytes())
                .take_while(|(a, b)| a == b)
                .count();
            out.push_str(&line[shared..]);
        } else {
            out.push_str(line);
        }
    }
    out
}

/// `gS{motion}`: open the snippet picker over the span.
fn operator_snippet_wrap(ctx: &mut OperatorContext) -> Result<Effect, CommandError> {
    let Some(range) = wrap_range(ctx.document.buffer(), ctx.range, ctx.linewise) else {
        return Ok(Effect::None);
    };
    Ok(Effect::OpenPicker {
        source: "snippets".to_string(),
        args: vec![OVER_FLAG.to_string(), encode_range(range)],
        root: None,
        fill_action: None,
        query: None,
    })
}

/// The arguments [`SNIPPET_WRAP_ACTION`] takes, as a wrap row carries them.
pub fn wrap_action_args(snippet: &str, range: Range) -> Args {
    Args::List(vec![
        ArgValue::String(snippet.to_string()),
        ArgValue::String(encode_range(range)),
    ])
}

/// `action:snippet-wrap(snippet, range)`: expand `snippet` around `range`.
fn wrap_effect(args: &Args) -> Result<Effect, CommandError> {
    let Some([ArgValue::String(snippet), ArgValue::String(range)]) = args.as_list() else {
        return Err(CommandError::InvalidArgs(
            "snippet-wrap takes a snippet and a range",
        ));
    };
    let Some(range) = decode_range(range) else {
        return Err(CommandError::InvalidArgs(
            "snippet-wrap's range is `line:byte-line:byte`",
        ));
    };
    Ok(Effect::ExpandSnippetOver {
        snippet: snippet.clone(),
        range,
    })
}

/// Register `operator:snippet-wrap` and `action:snippet-wrap`. Returns the
/// operator's id; the host also resolves it by name to bind `gS`.
pub fn register_snippet_wrap(registry: &mut CommandRegistry) -> OperatorId {
    registry.register_action(
        SNIPPET_WRAP_ACTION,
        "Expand a snippet around a range of the buffer, the range's text becoming \
         its `$TM_SELECTED_TEXT`. What a `gS` picker row runs.",
        ActionSpec {
            apply: Arc::new(|ctx| wrap_effect(&ctx.args)),
            args_schema: vec![
                ArgSpec::required(
                    "snippet",
                    ArgKind::String,
                    "The snippet's identity, as the snippet picker's rows carry it",
                ),
                ArgSpec::required(
                    "range",
                    ArgKind::String,
                    "The text to wrap: `line:byte-line:byte`, zero-based, end exclusive",
                ),
            ],
        },
    );
    registry.register_operator(
        SNIPPET_WRAP_OPERATOR,
        "Wrap the {motion}/{text-object} that follows `gS` in a snippet: pick one \
         and the text becomes its `$TM_SELECTED_TEXT`. `gSip` wraps a paragraph; \
         `gS` in Visual wraps the selection.",
        OperatorSpec {
            // A picker answers it, so `.` has nothing to replay.
            repeatable: false,
            blockwise_per_row: false,
            post_motion_char: false,
            args_schema: vec![],
            apply: Arc::new(operator_snippet_wrap),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(sl: u32, sb: u32, el: u32, eb: u32) -> Range {
        Range::new(Position::new(sl, sb), Position::new(el, eb))
    }

    #[test]
    fn a_range_round_trips_through_its_argument() {
        let r = range(3, 4, 7, 0);
        assert_eq!(encode_range(r), "3:4-7:0");
        assert_eq!(decode_range(&encode_range(r)), Some(r));
        assert_eq!(decode_range("3:4"), None);
        assert_eq!(decode_range("a:b-c:d"), None);
    }

    #[test]
    fn a_linewise_wrap_keeps_the_indentation_and_last_newline_outside() {
        let buf = Buffer::from_text("fn f() {\n    a();\n    b();\n}\n");
        // A linewise span over lines 1..=2 that ends at line 3 byte 0.
        assert_eq!(
            wrap_range(&buf, range(1, 0, 3, 0), true),
            Some(range(1, 4, 2, 8))
        );
        assert_eq!(
            wrap_range(&buf, range(1, 4, 1, 6), false),
            Some(range(1, 4, 1, 6)),
            "charwise is the span itself"
        );
        assert_eq!(wrap_range(&buf, range(1, 4, 1, 4), false), None);
    }

    #[test]
    fn a_selection_loses_its_first_lines_indentation_and_keeps_its_depth() {
        assert_eq!(
            selection_relative_to("if x {\n        y();\n    }", "    "),
            "if x {\n    y();\n}"
        );
        assert_eq!(
            selection_relative_to("a\n  b\n\nc", "    "),
            "a\nb\n\nc",
            "a shallower line loses only what it shares"
        );
    }

    #[test]
    fn both_commands_register() {
        let mut registry = CommandRegistry::new();
        register_snippet_wrap(&mut registry);
        assert!(registry.id_by_name(SNIPPET_WRAP_OPERATOR).is_some());
        assert!(registry.id_by_name(SNIPPET_WRAP_ACTION).is_some());
    }

    #[test]
    fn the_action_names_the_snippet_and_the_range() {
        let effect = wrap_effect(&wrap_action_args("rust\u{1f}if", range(0, 4, 2, 1)));
        assert!(
            matches!(
                &effect,
                Ok(Effect::ExpandSnippetOver { snippet, range: r })
                    if snippet == "rust\u{1f}if" && *r == range(0, 4, 2, 1)
            ),
            "{effect:?}"
        );
        assert!(wrap_effect(&Args::None).is_err());
        assert!(
            wrap_effect(&Args::List(vec![
                ArgValue::String("x".into()),
                ArgValue::String("nope".into()),
            ]))
            .is_err()
        );
    }
}
