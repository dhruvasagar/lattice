//! The unified dispatcher (DESIGN.md §5.2.1).
//!
//! `execute` is the single entry point through which every command flows:
//! built-in operators / motions / text-objects, ex-commands parsed from `:`,
//! plugin contributions, and palette selections. Vim's grammar UX is
//! preserved exactly via the keystroke parser (which assembles a
//! `CommandInvocation` and calls `execute`); the simplification is that there
//! is just one dispatcher below the parser.
//!
//! Phase 1 implements operator-with-motion-target (the most common path),
//! motion-alone, text-object-alone, and explicit grammar `Range` resolution.
//! Stub `Action` and `ExCommand` paths return an error until those layers
//! land.

use lattice_protocol::position::{Position, Range as ProtoRange};

use crate::command::{CommandInvocation, CommandKind};
use crate::effect::Effect;
use crate::error::{CommandError, GrammarResult};
use crate::range::Range;
use crate::registry::{
    CommandEntry, CommandRegistry, MotionContext, OperatorContext, TextObjectContext,
    require_motion, require_operator, require_text_object,
};
use crate::target::Target;
use lattice_core::Document;

/// Execute a `CommandInvocation` against `document`, using `registry` to
/// resolve motions / text-objects / operators.
///
/// `cursor` is the position the modal engine considers "current" -- typically
/// the primary selection's head.
pub fn execute(
    registry: &CommandRegistry,
    document: &mut Document,
    cursor: Position,
    invocation: CommandInvocation,
) -> GrammarResult<Effect> {
    let entry = registry
        .entry(invocation.command)
        .ok_or(CommandError::UnknownCommand)?;

    match entry.spec.kind {
        CommandKind::Motion => execute_motion(document, cursor, &invocation, entry),
        CommandKind::TextObject => execute_text_object(document, cursor, &invocation, entry),
        CommandKind::Operator => execute_operator(registry, document, cursor, &invocation, entry),
        CommandKind::ExCommand | CommandKind::Action => Err(CommandError::InvalidArgs(
            "ex-commands and free-form actions are not yet wired in Phase 1",
        )),
    }
}

fn execute_motion(
    document: &Document,
    cursor: Position,
    invocation: &CommandInvocation,
    entry: &CommandEntry,
) -> GrammarResult<Effect> {
    let motion = require_motion(entry)?;
    let ctx = MotionContext {
        buffer: document.buffer(),
        from: cursor,
        count: invocation.count_or_default(),
        args: invocation.args.clone(),
    };
    let result = (motion.apply)(&ctx)?;
    // Motions in Phase 1 don't mutate selections directly here; the modal
    // engine's caller takes the new position and updates state. We surface
    // the position via Effect::SelectionChange when the caller is the
    // dispatch (i.e., a top-level invocation).
    let mut selections = document.selections().clone();
    selections.replace_primary(lattice_protocol::selection::Selection::cursor(result.target));
    Ok(Effect::SelectionChange(selections))
}

fn execute_text_object(
    document: &Document,
    cursor: Position,
    invocation: &CommandInvocation,
    entry: &CommandEntry,
) -> GrammarResult<Effect> {
    let tobj = require_text_object(entry)?;
    let ctx = TextObjectContext {
        buffer: document.buffer(),
        at: cursor,
        count: invocation.count_or_default(),
        args: invocation.args.clone(),
    };
    let _range = (tobj.apply)(&ctx)?;
    // A text-object alone (no operator) is unusual; vim's behavior is to
    // expand the visual selection. Phase 1 returns Effect::None until the
    // visual-mode integration lands.
    Ok(Effect::None)
}

fn execute_operator(
    registry: &CommandRegistry,
    document: &mut Document,
    cursor: Position,
    invocation: &CommandInvocation,
    entry: &CommandEntry,
) -> GrammarResult<Effect> {
    let operator = require_operator(entry)?;

    let motion_count = invocation.count_or_default();
    let target_range: ProtoRange = match (&invocation.range, &invocation.target) {
        (Some(grammar_range), _) => resolve_grammar_range(document, grammar_range, cursor)?,
        (None, Some(target)) => {
            resolve_target(registry, document, cursor, target, motion_count)?
        }
        (None, None) => return Err(CommandError::MissingTarget),
    };

    let visual_linewise = matches!(
        invocation.range,
        Some(Range::Selection)
    ) && matches!(
        document.selections().primary().visual,
        Some(lattice_protocol::selection::VisualMode::Linewise)
    );
    let mut ctx = OperatorContext {
        document,
        range: target_range,
        linewise: matches!(
            invocation.range,
            Some(Range::CurrentLine) | Some(Range::Whole)
        ) || visual_linewise,
        register: invocation.register_or_default(),
        count: invocation.count_or_default(),
        args: invocation.args.clone(),
    };
    (operator.apply)(&mut ctx)
}

fn resolve_target(
    registry: &CommandRegistry,
    document: &Document,
    cursor: Position,
    target: &Target,
    count: crate::command::Count,
) -> GrammarResult<ProtoRange> {
    match target {
        Target::Motion(motion_id, args) => {
            let entry = registry
                .entry(motion_id.0)
                .ok_or(CommandError::UnknownCommand)?;
            let motion = require_motion(entry)?;
            let ctx = MotionContext {
                buffer: document.buffer(),
                from: cursor,
                count,
                args: args.clone(),
            };
            let r = (motion.apply)(&ctx)?;
            Ok(motion_to_range(cursor, r.target, motion.exclusive))
        }
        Target::TextObject(tobj_id, args) => {
            let entry = registry
                .entry(tobj_id.0)
                .ok_or(CommandError::UnknownCommand)?;
            let tobj = require_text_object(entry)?;
            let ctx = TextObjectContext {
                buffer: document.buffer(),
                at: cursor,
                count,
                args: args.clone(),
            };
            (tobj.apply)(&ctx)
        }
        Target::Range(grammar_range) => resolve_grammar_range(document, grammar_range, cursor),
    }
}

fn resolve_grammar_range(
    document: &Document,
    range: &Range,
    cursor: Position,
) -> GrammarResult<ProtoRange> {
    match range {
        Range::Whole => {
            let buffer = document.buffer();
            let last_line = buffer.line_count().saturating_sub(1);
            let start = Position::ZERO;
            let end = Position::new(last_line, line_byte_len(buffer, last_line));
            Ok(ProtoRange::new(start, end))
        }
        Range::CurrentLine => {
            let buffer = document.buffer();
            let line = cursor.line;
            Ok(ProtoRange::new(
                Position::new(line, 0),
                Position::new(line, line_byte_len(buffer, line)),
            ))
        }
        Range::Selection => {
            let sel = document.selections().primary();
            let (a, b) = ordered(sel.anchor, sel.head);
            match sel.visual {
                Some(lattice_protocol::selection::VisualMode::Linewise) => {
                    // Linewise visual covers complete lines from anchor's
                    // line to head's line, regardless of byte offsets.
                    let buffer = document.buffer();
                    let start = Position::new(a.line, 0);
                    let end = Position::new(b.line, line_byte_len(buffer, b.line));
                    Ok(ProtoRange::new(start, end))
                }
                Some(lattice_protocol::selection::VisualMode::Charwise) | None => {
                    // Charwise visual: half-open `[a, b)` -- but vim treats
                    // visual ranges as INCLUSIVE of the head, so we extend
                    // the end by one byte (clamped to line length).
                    let buffer = document.buffer();
                    let line_len = line_byte_len(buffer, b.line);
                    let extended_end = Position::new(b.line, (b.byte + 1).min(line_len));
                    Ok(ProtoRange::new(a, extended_end))
                }
                Some(lattice_protocol::selection::VisualMode::Blockwise) => {
                    // Blockwise visual is a post-1.0 feature; v1 falls back
                    // to charwise semantics.
                    Ok(ProtoRange::new(a, b))
                }
            }
        }
        Range::Span { .. } | Range::Custom(_) => Err(CommandError::InvalidArgs(
            "Span and Custom ranges are not yet resolved in Phase 1",
        )),
    }
}

fn motion_to_range(from: Position, to: Position, _exclusive: bool) -> ProtoRange {
    let (a, b) = ordered(from, to);
    ProtoRange::new(a, b)
}

fn ordered(a: Position, b: Position) -> (Position, Position) {
    if a <= b { (a, b) } else { (b, a) }
}

fn line_byte_len(buffer: &lattice_core::Buffer, line: u32) -> u32 {
    let s = buffer.as_string();
    let lines: Vec<&str> = s.split_inclusive('\n').collect();
    lines
        .get(line as usize)
        .map(|l| l.trim_end_matches('\n').len() as u32)
        .unwrap_or(0)
}
