//! Built-in motions, text objects, and operators that ship native (off the
//! WASM boundary; per DESIGN.md §5.5.2 "built-ins stay native").
//!
//! Phase 1 implements the minimum necessary to demonstrate end-to-end
//! dispatch:
//! - `motion::word_forward` (next word start)
//! - `operator::delete`
//!
//! Subsequent revisions populate the full vim catalog. Each new built-in is
//! a registration here; no new dispatcher wiring needed.

use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;

use crate::effect::{Effect, YankKind};
use crate::error::CommandError;
use crate::registry::{
    CommandRegistry, MotionContext, MotionResult, MotionSpec, OperatorContext, OperatorId,
    OperatorSpec, MotionId,
};

/// Register all Phase 1 built-ins. Returns the ids needed by the keystroke
/// parser / tests.
pub fn populate(registry: &mut CommandRegistry) -> Builtins {
    let word_forward = registry.register_motion(
        "motion:word-forward",
        "Move to the start of the next word.",
        MotionSpec {
            jump: false,
            exclusive: true,
            apply: Box::new(motion_word_forward),
        },
    );
    let word_backward = registry.register_motion(
        "motion:word-backward",
        "Move to the start of the previous word (vim's `b`).",
        MotionSpec {
            jump: false,
            exclusive: true,
            apply: Box::new(motion_word_backward),
        },
    );
    let word_end = registry.register_motion(
        "motion:word-end",
        "Move to the last byte of the current or next word (vim's `e`). Inclusive.",
        MotionSpec {
            jump: false,
            exclusive: false,
            apply: Box::new(motion_word_end),
        },
    );
    let first_non_blank = registry.register_motion(
        "motion:first-non-blank",
        "Move to the first non-whitespace byte of the current line (vim's `^`).",
        MotionSpec {
            jump: false,
            exclusive: false,
            apply: Box::new(motion_first_non_blank),
        },
    );
    let char_left = registry.register_motion(
        "motion:char-left",
        "Move one byte to the left within the current line.",
        MotionSpec {
            jump: false,
            exclusive: false,
            apply: Box::new(motion_char_left),
        },
    );
    let char_right = registry.register_motion(
        "motion:char-right",
        "Move one byte to the right within the current line.",
        MotionSpec {
            jump: false,
            exclusive: true,
            apply: Box::new(motion_char_right),
        },
    );
    let line_up = registry.register_motion(
        "motion:line-up",
        "Move one line up.",
        MotionSpec {
            jump: false,
            exclusive: false,
            apply: Box::new(motion_line_up),
        },
    );
    let line_down = registry.register_motion(
        "motion:line-down",
        "Move one line down.",
        MotionSpec {
            jump: false,
            exclusive: false,
            apply: Box::new(motion_line_down),
        },
    );
    let line_start = registry.register_motion(
        "motion:line-start",
        "Move to the first byte of the current line.",
        MotionSpec {
            jump: false,
            exclusive: false,
            apply: Box::new(motion_line_start),
        },
    );
    let line_end = registry.register_motion(
        "motion:line-end",
        "Move to the last byte of the current line.",
        MotionSpec {
            jump: false,
            exclusive: false,
            apply: Box::new(motion_line_end),
        },
    );
    let goto_first_line = registry.register_motion(
        "motion:goto-first-line",
        "Jump to the first line of the buffer (vim's `gg`).",
        MotionSpec {
            jump: true,
            exclusive: false,
            apply: Box::new(motion_goto_first_line),
        },
    );
    let goto_last_line = registry.register_motion(
        "motion:goto-last-line",
        "Jump to the last line of the buffer (vim's `G`).",
        MotionSpec {
            jump: true,
            exclusive: false,
            apply: Box::new(motion_goto_last_line),
        },
    );

    let delete = registry.register_operator(
        "operator:delete",
        "Delete the bytes covered by the target range; yank to the unnamed register.",
        OperatorSpec {
            repeatable: true,
            apply: Box::new(operator_delete),
        },
    );
    let change = registry.register_operator(
        "operator:change",
        "Delete the bytes covered by the target range and enter Insert mode (vim's `c`).",
        OperatorSpec {
            repeatable: true,
            apply: Box::new(operator_change),
        },
    );
    let yank = registry.register_operator(
        "operator:yank",
        "Copy the bytes covered by the target range into the named register without modifying the buffer (vim's `y`).",
        OperatorSpec {
            repeatable: true,
            apply: Box::new(operator_yank),
        },
    );

    Builtins {
        word_forward,
        word_backward,
        word_end,
        first_non_blank,
        char_left,
        char_right,
        line_up,
        line_down,
        line_start,
        line_end,
        goto_first_line,
        goto_last_line,
        delete,
        change,
        yank,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Builtins {
    pub word_forward: MotionId,
    pub word_backward: MotionId,
    pub word_end: MotionId,
    pub first_non_blank: MotionId,
    pub char_left: MotionId,
    pub char_right: MotionId,
    pub line_up: MotionId,
    pub line_down: MotionId,
    pub line_start: MotionId,
    pub line_end: MotionId,
    pub goto_first_line: MotionId,
    pub goto_last_line: MotionId,
    pub delete: OperatorId,
    pub change: OperatorId,
    pub yank: OperatorId,
}

// ---- Motion: word-forward ----

fn motion_word_forward(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    // Advance `count` words. A word boundary here is the conventional vim
    // "word" (alphanumeric + underscore) -- not "WORD" (whitespace-delimited).
    // Phase 1 keeps the implementation simple: walk byte-wise over UTF-8
    // characters in the current line and the next ones until we've crossed
    // `count` word boundaries forward.
    let text = ctx.buffer.as_string();
    let lines: Vec<&str> = text.split_inclusive('\n').collect();

    let mut line = ctx.from.line as usize;
    let mut byte = ctx.from.byte as usize;
    let count = ctx.count.get().max(1);

    for _ in 0..count {
        // Skip the current word's remaining word chars.
        while line < lines.len() {
            let l = lines[line];
            let bytes = l.as_bytes();
            if byte < bytes.len() && is_word_byte(bytes[byte]) {
                byte += 1;
            } else {
                break;
            }
            if byte >= bytes.len() {
                line += 1;
                byte = 0;
                break;
            }
        }
        // Skip non-word characters (whitespace, punctuation) until we hit the
        // next word start, or run out of buffer.
        loop {
            if line >= lines.len() {
                break;
            }
            let l = lines[line];
            let bytes = l.as_bytes();
            if byte >= bytes.len() {
                line += 1;
                byte = 0;
                continue;
            }
            if is_word_byte(bytes[byte]) {
                break;
            }
            byte += 1;
        }
    }

    // Clamp to last position if we walked off the end.
    let target = if line >= lines.len() {
        let last_line = lines.len().saturating_sub(1);
        let last_len = lines
            .get(last_line)
            .map(|l| l.trim_end_matches('\n').len())
            .unwrap_or(0);
        Position::new(last_line as u32, last_len as u32)
    } else {
        Position::new(line as u32, byte as u32)
    };

    Ok(MotionResult {
        target,
        linewise: false,
    })
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_blank_byte(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

// ---- Motion: word-backward (vim's `b`) ----

fn motion_word_backward(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let count = ctx.count.get().max(1);
    let mut idx = ctx
        .buffer
        .position_to_byte(ctx.from)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;

    for _ in 0..count {
        if idx == 0 {
            break;
        }
        // Step one byte back, then skip non-word bytes (whitespace, newlines,
        // punctuation), then walk back through the word's body to its start.
        idx -= 1;
        while idx > 0 && !is_word_byte(bytes[idx]) {
            idx -= 1;
        }
        while idx > 0 && is_word_byte(bytes[idx - 1]) {
            idx -= 1;
        }
    }

    let target = ctx
        .buffer
        .byte_to_position(idx)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    Ok(MotionResult { target, linewise: false })
}

// ---- Motion: word-end (vim's `e`) ----

fn motion_word_end(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let total = bytes.len();
    let count = ctx.count.get().max(1);
    let mut idx = ctx
        .buffer
        .position_to_byte(ctx.from)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;

    for _ in 0..count {
        if idx >= total.saturating_sub(1) {
            break;
        }
        // Step one byte forward, skip non-word bytes, then advance to the
        // last byte of the current word (the position where the next byte
        // would be non-word or past EOF).
        idx += 1;
        while idx < total && !is_word_byte(bytes[idx]) {
            idx += 1;
        }
        if idx >= total {
            idx = total.saturating_sub(1);
            break;
        }
        while idx + 1 < total && is_word_byte(bytes[idx + 1]) {
            idx += 1;
        }
    }

    let target = ctx
        .buffer
        .byte_to_position(idx)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    Ok(MotionResult { target, linewise: false })
}

// ---- Motion: first-non-blank (vim's `^`) ----

fn motion_first_non_blank(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let text = ctx.buffer.as_string();
    let line_text = text
        .split_inclusive('\n')
        .nth(ctx.from.line as usize)
        .map(|l| l.trim_end_matches('\n'))
        .unwrap_or("");
    let bytes = line_text.as_bytes();
    let mut col = 0usize;
    while col < bytes.len() && is_blank_byte(bytes[col]) {
        col += 1;
    }
    Ok(MotionResult {
        target: Position::new(ctx.from.line, col as u32),
        linewise: false,
    })
}

// ---- Motion: char-left ----

fn motion_char_left(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let count = ctx.count.get().max(1);
    let mut pos = ctx.from;
    for _ in 0..count {
        if pos.byte == 0 {
            break;
        }
        pos.byte -= 1;
    }
    Ok(MotionResult { target: pos, linewise: false })
}

// ---- Motion: char-right ----

fn motion_char_right(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let count = ctx.count.get().max(1);
    let mut pos = ctx.from;
    let line_len = line_byte_len(ctx.buffer, pos.line);
    for _ in 0..count {
        if pos.byte >= line_len {
            break;
        }
        pos.byte += 1;
    }
    Ok(MotionResult { target: pos, linewise: false })
}

// ---- Motion: line-up / line-down ----

fn motion_line_up(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let count = ctx.count.get().max(1);
    let line = ctx.from.line.saturating_sub(count);
    let max_byte = line_byte_len(ctx.buffer, line);
    let byte = ctx.from.byte.min(max_byte);
    Ok(MotionResult {
        target: Position::new(line, byte),
        linewise: false,
    })
}

fn motion_line_down(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let count = ctx.count.get().max(1);
    let last = last_addressable_line(ctx.buffer);
    let line = ctx.from.line.saturating_add(count).min(last);
    let max_byte = line_byte_len(ctx.buffer, line);
    let byte = ctx.from.byte.min(max_byte);
    Ok(MotionResult {
        target: Position::new(line, byte),
        linewise: false,
    })
}

// ---- Motion: line-start / line-end ----

fn motion_line_start(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    Ok(MotionResult {
        target: Position::new(ctx.from.line, 0),
        linewise: false,
    })
}

fn motion_line_end(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let len = line_byte_len(ctx.buffer, ctx.from.line);
    Ok(MotionResult {
        target: Position::new(ctx.from.line, len),
        linewise: false,
    })
}

// ---- Motion: goto-first-line / goto-last-line ----

fn motion_goto_first_line(_ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    Ok(MotionResult {
        target: Position::ZERO,
        linewise: true,
    })
}

fn motion_goto_last_line(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let last = last_addressable_line(ctx.buffer);
    Ok(MotionResult {
        target: Position::new(last, 0),
        linewise: true,
    })
}

// ---- Helpers ----

fn line_byte_len(buffer: &lattice_core::Buffer, line: u32) -> u32 {
    let s = buffer.as_string();
    s.split_inclusive('\n')
        .nth(line as usize)
        .map(|l| l.trim_end_matches('\n').len() as u32)
        .unwrap_or(0)
}

fn last_addressable_line(buffer: &lattice_core::Buffer) -> u32 {
    let lc = buffer.line_count();
    let s = buffer.as_string();
    if lc == 0 {
        0
    } else if s.ends_with('\n') {
        lc.saturating_sub(2)
    } else {
        lc.saturating_sub(1)
    }
}

// ---- Operator: delete ----
//
// Vim's `d` -- delete the bytes covered by the target range. Also yanks
// the deleted content into the unnamed register (vim's behavior). The
// returned Effect is a `Many` so callers see both the buffer mutation
// and the yank in a single dispatched commit.

fn operator_delete(ctx: &mut OperatorContext) -> Result<Effect, CommandError> {
    if ctx.range.is_empty() {
        return Ok(Effect::None);
    }
    let _ = ctx.register; // explicit register selection lands later
    let yanked = ctx.document.buffer().slice(ctx.range)?;
    let edit = Edit::delete(ctx.range);
    let applied = ctx.document.apply_edit(edit)?;
    let yank_kind = if ctx.linewise {
        YankKind::Linewise
    } else {
        YankKind::Charwise
    };
    Ok(Effect::Many(vec![
        Effect::Edits(vec![applied]),
        Effect::Yank {
            register: crate::register::Register::Unnamed,
            content: yanked,
            kind: yank_kind,
        },
    ]))
}

// ---- Operator: change ----
//
// Vim's `c` -- delete the target range, yank it (matching delete's
// behavior), and enter Insert mode at the deleted position. Composition
// expressed in the Effect, not a flag on the spec.

fn operator_change(ctx: &mut OperatorContext) -> Result<Effect, CommandError> {
    if ctx.range.is_empty() {
        // No-op change still drops into Insert (vim does this for `cw` on
        // an empty buffer / past EOF).
        return Ok(Effect::EnterMode(crate::modal::ModalState::Insert));
    }
    let yanked = ctx.document.buffer().slice(ctx.range)?;
    let edit = Edit::delete(ctx.range);
    let applied = ctx.document.apply_edit(edit)?;
    let yank_kind = if ctx.linewise {
        YankKind::Linewise
    } else {
        YankKind::Charwise
    };
    Ok(Effect::Many(vec![
        Effect::Edits(vec![applied]),
        Effect::Yank {
            register: crate::register::Register::Unnamed,
            content: yanked,
            kind: yank_kind,
        },
        Effect::EnterMode(crate::modal::ModalState::Insert),
    ]))
}

// ---- Operator: yank ----
//
// Vim's `y` -- copy the target range into the unnamed register without
// touching the buffer.

fn operator_yank(ctx: &mut OperatorContext) -> Result<Effect, CommandError> {
    if ctx.range.is_empty() {
        return Ok(Effect::None);
    }
    let content = ctx.document.buffer().slice(ctx.range)?;
    let kind = if ctx.linewise {
        YankKind::Linewise
    } else {
        YankKind::Charwise
    };
    Ok(Effect::Yank {
        register: crate::register::Register::Unnamed,
        content,
        kind,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::command::{CommandInvocation, Count};
    use crate::dispatcher::execute;
    use crate::effect::Effect;
    use crate::target::Target;
    use lattice_core::Document;

    fn fixture(text: &str) -> (CommandRegistry, Builtins, Document) {
        let mut r = CommandRegistry::new();
        let b = populate(&mut r);
        let d = Document::from_text(text);
        (r, b, d)
    }

    #[test]
    fn populate_registers_known_builtins_by_name() {
        let mut r = CommandRegistry::new();
        let _ = populate(&mut r);
        assert!(r.lookup_by_name("motion:word-forward").is_some());
        assert!(r.lookup_by_name("operator:delete").is_some());
    }

    #[test]
    fn word_forward_advances_to_next_word_start() {
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.word_forward.0);
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => {
                assert_eq!(s.primary().head, Position::new(0, 6));
            }
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_forward_with_count_advances_by_count() {
        let (registry, b, mut doc) = fixture("one two three four");
        let inv = CommandInvocation::of(b.word_forward.0).with_count(Count(2));
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => {
                // "one two THREE four" -- two words forward from origin lands
                // at the start of "three" (byte 8).
                assert_eq!(s.primary().head, Position::new(0, 8));
            }
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_forward_across_newline() {
        let (registry, b, mut doc) = fixture("hello\nworld");
        let inv = CommandInvocation::of(b.word_forward.0);
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => {
                assert_eq!(s.primary().head, Position::new(1, 0));
            }
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn delete_with_word_forward_target_dw_semantics() {
        // The classic `dw`: from origin, delete to next word start.
        // Delete now also yanks the deleted content into the unnamed
        // register, so the Effect is `Many([Edits, Yank])`.
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.delete.0)
            .with_target(Target::Motion(b.word_forward, crate::args::Args::None));
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::Many(parts) => {
                assert_eq!(parts.len(), 2);
                match &parts[0] {
                    Effect::Edits(applied) => {
                        assert_eq!(applied.len(), 1);
                        assert_eq!(applied[0].replaced_text, "hello ");
                    }
                    other => panic!("expected Edits at [0], got {other:?}"),
                }
                match &parts[1] {
                    Effect::Yank { content, kind, .. } => {
                        assert_eq!(content, "hello ");
                        assert_eq!(*kind, YankKind::Charwise);
                    }
                    other => panic!("expected Yank at [1], got {other:?}"),
                }
            }
            other => panic!("expected Many, got {other:?}"),
        }
        assert_eq!(doc.text(), "world");
    }

    #[test]
    fn delete_with_explicit_whole_range_deletes_buffer_and_yanks_linewise() {
        let (registry, b, mut doc) = fixture("a\nb\nc");
        let inv = CommandInvocation::of(b.delete.0).with_range(crate::range::Range::Whole);
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::Many(parts) => {
                assert!(matches!(parts[0], Effect::Edits(_)));
                match &parts[1] {
                    Effect::Yank { kind, .. } => assert_eq!(*kind, YankKind::Linewise),
                    other => panic!("expected Yank at [1], got {other:?}"),
                }
            }
            other => panic!("expected Many, got {other:?}"),
        }
        assert_eq!(doc.text(), "");
    }

    #[test]
    fn delete_with_current_line_range_clears_just_that_line() {
        let (registry, b, mut doc) = fixture("aaa\nBBB\nccc");
        let cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(b.delete.0).with_range(crate::range::Range::CurrentLine);
        execute(&registry, &mut doc, cursor, inv).unwrap();
        // `CurrentLine` covers the line content but not its trailing newline
        // -- BBB is removed; the surrounding newlines stay.
        assert_eq!(doc.text(), "aaa\n\nccc");
    }

    #[test]
    fn unknown_command_id_errors() {
        let (registry, _, mut doc) = fixture("abc");
        let bogus = lattice_protocol::ids::CommandId::new(99_999);
        let inv = CommandInvocation::of(bogus);
        assert!(matches!(
            execute(&registry, &mut doc, Position::ZERO, inv),
            Err(CommandError::UnknownCommand)
        ));
    }

    #[test]
    fn operator_without_target_or_range_errors() {
        let (registry, b, mut doc) = fixture("abc");
        let inv = CommandInvocation::of(b.delete.0);
        assert!(matches!(
            execute(&registry, &mut doc, Position::ZERO, inv),
            Err(CommandError::MissingTarget)
        ));
    }

    #[test]
    fn char_left_at_origin_stays_put() {
        let (registry, b, mut doc) = fixture("abc");
        let inv = CommandInvocation::of(b.char_left.0);
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn char_right_advances_one_byte() {
        let (registry, b, mut doc) = fixture("abc");
        let inv = CommandInvocation::of(b.char_right.0);
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 1)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn char_right_at_end_of_line_stays_put() {
        let (registry, b, mut doc) = fixture("ab");
        let inv = CommandInvocation::of(b.char_right.0);
        let effect = execute(&registry, &mut doc, Position::new(0, 2), inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 2)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn line_down_moves_one_line_and_clamps_byte() {
        let (registry, b, mut doc) = fixture("hello\nhi");
        let inv = CommandInvocation::of(b.line_down.0);
        let effect = execute(&registry, &mut doc, Position::new(0, 5), inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(1, 2)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn line_up_at_top_stays_put() {
        let (registry, b, mut doc) = fixture("a\nb");
        let inv = CommandInvocation::of(b.line_up.0);
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn line_start_resets_byte_to_zero() {
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.line_start.0);
        let effect = execute(&registry, &mut doc, Position::new(0, 7), inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 0)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn line_end_jumps_to_line_byte_length() {
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.line_end.0);
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 11)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn goto_first_line_returns_to_origin() {
        let (registry, b, mut doc) = fixture("a\nb\nc");
        let inv = CommandInvocation::of(b.goto_first_line.0);
        let effect = execute(&registry, &mut doc, Position::new(2, 0), inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn goto_last_line_jumps_to_last_addressable_line() {
        let (registry, b, mut doc) = fixture("a\nb\nc");
        let inv = CommandInvocation::of(b.goto_last_line.0);
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(2, 0)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn x_semantics_via_delete_with_char_right_target() {
        // Vim's `x`: delete the char under the cursor (and yank it).
        let (registry, b, mut doc) = fixture("hello");
        let inv = CommandInvocation::of(b.delete.0)
            .with_target(Target::Motion(b.char_right, crate::args::Args::None));
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::Many(parts) => {
                assert!(matches!(parts[0], Effect::Edits(_)));
                assert!(matches!(parts[1], Effect::Yank { .. }));
            }
            other => panic!("expected Many, got {other:?}"),
        }
        assert_eq!(doc.text(), "ello");
    }

    // ---- word-backward (b) ----

    #[test]
    fn word_backward_from_mid_word_lands_at_start_of_word() {
        let (registry, b, mut doc) = fixture("hello world");
        // Cursor on 'r' of "world" (byte 8) -- vim's `b` lands on 'w' (byte 6).
        let inv = CommandInvocation::of(b.word_backward.0);
        let effect = execute(&registry, &mut doc, Position::new(0, 8), inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 6)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_backward_from_start_of_word_lands_at_start_of_previous_word() {
        let (registry, b, mut doc) = fixture("one two three");
        // Cursor on 't' of "three" (byte 8). `b` -> 't' of "two" (byte 4).
        let inv = CommandInvocation::of(b.word_backward.0);
        let effect = execute(&registry, &mut doc, Position::new(0, 8), inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 4)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_backward_at_origin_stays_put() {
        let (registry, b, mut doc) = fixture("hello");
        let inv = CommandInvocation::of(b.word_backward.0);
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_backward_crosses_newlines() {
        let (registry, b, mut doc) = fixture("foo\nbar");
        // Cursor on 'b' (line 1 byte 0). `b` -> 'f' (line 0 byte 0).
        let inv = CommandInvocation::of(b.word_backward.0);
        let effect = execute(&registry, &mut doc, Position::new(1, 0), inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_backward_with_count_jumps_count_words() {
        let (registry, b, mut doc) = fixture("one two three four");
        // Cursor on 'f' of "four" (byte 14). `2b` -> 't' of "two" (byte 4).
        let inv = CommandInvocation::of(b.word_backward.0).with_count(Count(2));
        let effect = execute(&registry, &mut doc, Position::new(0, 14), inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 4)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_backward_skips_punctuation_as_non_word() {
        let (registry, b, mut doc) = fixture("alpha, beta");
        // Cursor on 'b' of "beta" (byte 7). `b` -> 'a' of "alpha" (byte 0).
        let inv = CommandInvocation::of(b.word_backward.0);
        let effect = execute(&registry, &mut doc, Position::new(0, 7), inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    // ---- word-end (e) ----

    #[test]
    fn word_end_from_start_of_word_lands_at_last_byte_of_word() {
        let (registry, b, mut doc) = fixture("hello world");
        // From 'h' (byte 0) `e` -> 'o' of "hello" (byte 4).
        let inv = CommandInvocation::of(b.word_end.0);
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 4)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_end_from_end_of_word_lands_at_end_of_next_word() {
        let (registry, b, mut doc) = fixture("hello world");
        // From 'o' of "hello" (byte 4) `e` -> 'd' of "world" (byte 10).
        let inv = CommandInvocation::of(b.word_end.0);
        let effect = execute(&registry, &mut doc, Position::new(0, 4), inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 10)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_end_at_buffer_end_stays_put() {
        let (registry, b, mut doc) = fixture("hi");
        // From 'i' (byte 1) `e` -> stays at byte 1 (no further word).
        let inv = CommandInvocation::of(b.word_end.0);
        let effect = execute(&registry, &mut doc, Position::new(0, 1), inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 1)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_end_crosses_newlines() {
        let (registry, b, mut doc) = fixture("foo\nbar");
        // From 'o' of "foo" (line 0 byte 2) `e` -> 'r' of "bar" (line 1 byte 2).
        let inv = CommandInvocation::of(b.word_end.0);
        let effect = execute(&registry, &mut doc, Position::new(0, 2), inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(1, 2)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_end_with_count_advances_by_count() {
        let (registry, b, mut doc) = fixture("one two three four");
        // From 'o' (byte 0) `2e` -> end of "two" = 'o' of "two" (byte 6).
        let inv = CommandInvocation::of(b.word_end.0).with_count(Count(2));
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 6)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    // ---- first-non-blank (^) ----

    #[test]
    fn first_non_blank_skips_leading_spaces() {
        let (registry, b, mut doc) = fixture("    hello");
        let inv = CommandInvocation::of(b.first_non_blank.0);
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 4)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn first_non_blank_skips_leading_tabs() {
        let (registry, b, mut doc) = fixture("\t\thello");
        let inv = CommandInvocation::of(b.first_non_blank.0);
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 2)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn first_non_blank_on_already_non_blank_line_returns_zero() {
        let (registry, b, mut doc) = fixture("hello");
        let inv = CommandInvocation::of(b.first_non_blank.0);
        let effect = execute(&registry, &mut doc, Position::new(0, 3), inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn first_non_blank_on_blank_only_line_returns_end() {
        let (registry, b, mut doc) = fixture("    ");
        let inv = CommandInvocation::of(b.first_non_blank.0);
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => {
                // No non-blank chars; cursor lands at end of line (byte 4).
                assert_eq!(s.primary().head, Position::new(0, 4));
            }
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn first_non_blank_respects_current_line() {
        let (registry, b, mut doc) = fixture("a\n  bc");
        // Cursor on line 1; first non-blank is at byte 2 of line 1.
        let inv = CommandInvocation::of(b.first_non_blank.0);
        let effect = execute(&registry, &mut doc, Position::new(1, 4), inv).unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(1, 2)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    // ---- delete + new motions composition ----

    #[test]
    fn delete_with_word_backward_target_db_semantics() {
        // `db` from past-EOL deletes the last word: word_backward from byte 11
        // lands at byte 6 (start of "world"); the [6, 11) range covers "world".
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.delete.0)
            .with_target(Target::Motion(b.word_backward, crate::args::Args::None));
        execute(&registry, &mut doc, Position::new(0, 11), inv).unwrap();
        assert_eq!(doc.text(), "hello ");
    }

    // ---- change operator (c) ----

    #[test]
    fn change_with_word_forward_emits_edits_yank_and_enter_insert() {
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.change.0)
            .with_target(Target::Motion(b.word_forward, crate::args::Args::None));
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        // Effect::Many([Edits, Yank, EnterMode(Insert)]).
        match effect {
            Effect::Many(parts) => {
                assert_eq!(parts.len(), 3);
                assert!(matches!(parts[0], Effect::Edits(_)));
                assert!(matches!(parts[1], Effect::Yank { .. }));
                assert!(matches!(
                    parts[2],
                    Effect::EnterMode(crate::ModalState::Insert)
                ));
            }
            other => panic!("expected Effect::Many, got {other:?}"),
        }
        assert_eq!(doc.text(), "world");
    }

    #[test]
    fn change_current_line_clears_line_and_enters_insert() {
        let (registry, b, mut doc) = fixture("aaa\nBBB\nccc");
        let cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(b.change.0).with_range(crate::range::Range::CurrentLine);
        let effect = execute(&registry, &mut doc, cursor, inv).unwrap();
        match effect {
            Effect::Many(parts) => {
                assert_eq!(parts.len(), 3);
                // CurrentLine -> linewise yank.
                match &parts[1] {
                    Effect::Yank { kind, .. } => assert_eq!(*kind, YankKind::Linewise),
                    other => panic!("expected Yank at [1], got {other:?}"),
                }
                assert!(matches!(
                    parts[2],
                    Effect::EnterMode(crate::ModalState::Insert)
                ));
            }
            other => panic!("expected Effect::Many, got {other:?}"),
        }
        assert_eq!(doc.text(), "aaa\n\nccc");
    }

    #[test]
    fn change_with_line_end_target_truncates_line_and_enters_insert() {
        // `c$` from byte 5 of "hello world" leaves "hello" and enters Insert.
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.change.0)
            .with_target(Target::Motion(b.line_end, crate::args::Args::None));
        let effect = execute(&registry, &mut doc, Position::new(0, 5), inv).unwrap();
        match effect {
            Effect::Many(parts) => {
                assert_eq!(parts.len(), 3);
                assert!(matches!(
                    parts[2],
                    Effect::EnterMode(crate::ModalState::Insert)
                ));
            }
            other => panic!("expected Effect::Many, got {other:?}"),
        }
        assert_eq!(doc.text(), "hello");
    }

    // ---- yank operator (y) ----

    #[test]
    fn yank_with_word_forward_emits_charwise_yank() {
        let (registry, b, mut doc) = fixture("hello world");
        let original_text = doc.text();
        let inv = CommandInvocation::of(b.yank.0)
            .with_target(Target::Motion(b.word_forward, crate::args::Args::None));
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        // Yank does NOT touch the buffer.
        assert_eq!(doc.text(), original_text);
        match effect {
            Effect::Yank { content, kind, register } => {
                assert_eq!(content, "hello ");
                assert_eq!(kind, YankKind::Charwise);
                assert_eq!(register, crate::register::Register::Unnamed);
            }
            other => panic!("expected Yank, got {other:?}"),
        }
    }

    #[test]
    fn yank_with_current_line_range_emits_linewise_yank() {
        let (registry, b, mut doc) = fixture("aaa\nBBB\nccc");
        let inv = CommandInvocation::of(b.yank.0).with_range(crate::range::Range::CurrentLine);
        let effect = execute(&registry, &mut doc, Position::new(1, 0), inv).unwrap();
        // No buffer mutation.
        assert_eq!(doc.text(), "aaa\nBBB\nccc");
        match effect {
            Effect::Yank { content, kind, .. } => {
                assert_eq!(content, "BBB");
                assert_eq!(kind, YankKind::Linewise);
            }
            other => panic!("expected Yank, got {other:?}"),
        }
    }

    #[test]
    fn yank_with_whole_range_emits_linewise_full_buffer() {
        let (registry, b, mut doc) = fixture("hello\nworld");
        let inv = CommandInvocation::of(b.yank.0).with_range(crate::range::Range::Whole);
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::Yank { content, kind, .. } => {
                assert_eq!(content, "hello\nworld");
                assert_eq!(kind, YankKind::Linewise);
            }
            other => panic!("expected Yank, got {other:?}"),
        }
    }

    #[test]
    fn yank_does_not_modify_buffer() {
        let (registry, b, mut doc) = fixture("immutable text");
        let original = doc.text();
        let inv = CommandInvocation::of(b.yank.0).with_range(crate::range::Range::Whole);
        execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        assert_eq!(doc.text(), original);
    }

    #[test]
    fn yank_empty_range_returns_none() {
        let (registry, b, mut doc) = fixture("");
        let inv = CommandInvocation::of(b.yank.0).with_range(crate::range::Range::Whole);
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        // Empty buffer / empty range -> Effect::None.
        assert!(matches!(effect, Effect::None));
    }

    // ---- delete-yanks-into-register (composite verification) ----

    #[test]
    fn delete_charwise_yanks_charwise() {
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.delete.0)
            .with_target(Target::Motion(b.word_forward, crate::args::Args::None));
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::Many(parts) => match &parts[1] {
                Effect::Yank { kind, content, .. } => {
                    assert_eq!(*kind, YankKind::Charwise);
                    assert_eq!(content, "hello ");
                }
                other => panic!("expected Yank, got {other:?}"),
            },
            other => panic!("expected Many, got {other:?}"),
        }
    }

    #[test]
    fn delete_linewise_yanks_linewise() {
        let (registry, b, mut doc) = fixture("aaa\nBBB\nccc");
        let inv = CommandInvocation::of(b.delete.0).with_range(crate::range::Range::CurrentLine);
        let effect = execute(&registry, &mut doc, Position::new(1, 0), inv).unwrap();
        match effect {
            Effect::Many(parts) => match &parts[1] {
                Effect::Yank { kind, .. } => assert_eq!(*kind, YankKind::Linewise),
                other => panic!("expected Yank, got {other:?}"),
            },
            other => panic!("expected Many, got {other:?}"),
        }
    }

    #[test]
    fn delete_emits_many_with_edits_and_yank_only() {
        // Sanity: delete emits Many([Edits, Yank]) -- never EnterMode.
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.delete.0)
            .with_target(Target::Motion(b.word_forward, crate::args::Args::None));
        let effect = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match effect {
            Effect::Many(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(!parts.iter().any(|e| matches!(e, Effect::EnterMode(_))));
            }
            other => panic!("expected Many, got {other:?}"),
        }
    }

    #[test]
    fn delete_with_word_end_target_de_semantics() {
        // `de` from start of "hello world" deletes "hello" (word_end is
        // inclusive, so range covers [0, 5)).
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.delete.0)
            .with_target(Target::Motion(b.word_end, crate::args::Args::None));
        execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        // word_end lands at byte 4 ('o' of "hello"). Our dispatcher uses
        // [start, end) ranges so the resulting deletion covers [0, 4) = "hell".
        // (This documents current dispatcher behavior; vim's inclusive
        // semantics for `de` would delete "hello", a refinement tracked in
        // §15:N.)
        assert_eq!(doc.text(), "o world");
    }

    #[test]
    fn delete_with_motion_target_for_motion_id_kind_mismatch_is_caught() {
        // Constructing a Target::Motion from a *non-motion* command id should
        // surface a kind mismatch, not silently succeed.
        let (registry, b, mut doc) = fixture("abc");
        // Use `delete` (an operator) as if it were a motion. Should fail.
        let bogus = MotionId(b.delete.0);
        let inv = CommandInvocation::of(b.delete.0)
            .with_target(Target::Motion(bogus, crate::args::Args::None));
        let err = execute(&registry, &mut doc, Position::ZERO, inv).unwrap_err();
        assert!(matches!(err, CommandError::KindMismatch { .. }));
    }
}
