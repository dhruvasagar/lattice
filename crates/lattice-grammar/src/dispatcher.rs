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
use crate::effect::{Effect, YankKind};
use crate::error::{CommandError, GrammarResult};
use crate::modal::ModalState;
use crate::range::Range;
use crate::registry::{
    CommandEntry, CommandRegistry, ExCommandContext, MotionContext, OperatorContext,
    TextObjectContext, require_ex_command, require_motion, require_operator, require_text_object,
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
        CommandKind::ExCommand => execute_ex_command(&invocation, entry),
        CommandKind::Action => Err(CommandError::InvalidArgs(
            "free-form actions are not yet wired in Phase 1",
        )),
    }
}

fn execute_ex_command(
    invocation: &CommandInvocation,
    entry: &CommandEntry,
) -> GrammarResult<Effect> {
    let spec = require_ex_command(entry)?;
    let ctx = ExCommandContext {
        bang: invocation.bang,
        args: invocation.args.clone(),
        range: invocation.range.clone(),
        register: invocation.register_or_default(),
        count: invocation.count_or_default(),
    };
    (spec.apply)(&ctx)
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

    // Blockwise visual is dispatched per-row: each row's column slice
    // gets its own ProtoRange, the operator's apply runs once per row,
    // and the returned per-row Effects are merged into a single
    // Effect::Many with edits concatenated and yanks collapsed into a
    // single Blockwise yank carrying the row contents joined by '\n'.
    if let Some(Range::Selection) = invocation.range
        && matches!(
            document.selections().primary().visual,
            Some(lattice_protocol::selection::VisualMode::Blockwise)
        )
    {
        return execute_operator_blockwise(operator, document, invocation);
    }

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

/// Per-row dispatch for blockwise visual operators. Vim's `Ctrl-V`
/// selection is a rectangle; `d` / `y` / `c` operate on each row's
/// column slice independently, then the results are committed
/// together. We:
///
/// 1. Compute each row's [`ProtoRange`] from the visual selection
///    (clamped to that row's length -- short rows get an empty range).
/// 2. Snapshot each row's text top-down (for the merged Yank's content).
/// 3. Run `operator.apply` per row, **bottom-up** so deletions on a
///    row don't shift positions on rows above. For non-mutating
///    operators (yank), order doesn't matter; bottom-up is safe.
/// 4. Flatten the per-row Effects, merge yanks into one Blockwise
///    yank, concatenate Edits, deduplicate `EnterMode`.
fn execute_operator_blockwise(
    operator: &crate::registry::OperatorSpec,
    document: &mut Document,
    invocation: &CommandInvocation,
) -> GrammarResult<Effect> {
    let sel = document.selections().primary();
    let (top_line, bottom_line) = (sel.anchor.line.min(sel.head.line), sel.anchor.line.max(sel.head.line));
    let (left_col, right_col) = (
        sel.anchor.byte.min(sel.head.byte),
        sel.anchor.byte.max(sel.head.byte),
    );

    // Per-row ranges, top-down. Each range covers `[left_col,
    // right_col + 1)` clamped to the row's actual length. The +1 is
    // vim's inclusive-end convention for visual selections.
    let mut row_ranges: Vec<ProtoRange> = Vec::with_capacity((bottom_line - top_line + 1) as usize);
    for line in top_line..=bottom_line {
        let line_len = line_byte_len(document.buffer(), line);
        let start = left_col.min(line_len);
        let end = (right_col + 1).min(line_len);
        row_ranges.push(ProtoRange::new(
            Position::new(line, start),
            Position::new(line, end),
        ));
    }

    // Snapshot row contents top-down before any mutation. Each row is
    // either the column slice (if non-empty) or an empty string; the
    // joined-by-newlines result is the Blockwise yank's content.
    let mut row_contents: Vec<String> = Vec::with_capacity(row_ranges.len());
    for r in &row_ranges {
        if r.is_empty() {
            row_contents.push(String::new());
        } else {
            row_contents.push(document.buffer().slice(*r)?);
        }
    }

    // Run apply per row, bottom-up. Collect every produced Effect.
    let register = invocation.register_or_default();
    let count = invocation.count_or_default();
    let args = invocation.args.clone();
    let mut per_row_effects: Vec<Effect> = Vec::with_capacity(row_ranges.len());
    for r in row_ranges.iter().rev() {
        let mut ctx = OperatorContext {
            document,
            range: *r,
            linewise: false,
            register,
            count,
            args: args.clone(),
        };
        let eff = (operator.apply)(&mut ctx)?;
        per_row_effects.push(eff);
    }
    // Restore top-down order so the merged effect ordering tracks the
    // original row order (matters for some downstream consumers, even
    // though Edits / Yanks themselves don't carry an order beyond
    // their internal vec).
    per_row_effects.reverse();

    Ok(merge_blockwise_effects(
        per_row_effects,
        row_contents,
        register,
    ))
}

/// Flatten the per-row Effects and merge:
/// - all `Effect::Edits` -> one combined `Effect::Edits`
/// - per-row `Effect::Yank` -> one `Effect::Yank` per distinct
///   register, with `kind = Blockwise` and content = `row_contents`
///   joined by `\n`. Per-row yank content is discarded; the joined
///   row snapshot is the source of truth.
/// - `Effect::EnterMode` deduplicated (keep one if any).
fn merge_blockwise_effects(
    per_row_effects: Vec<Effect>,
    row_contents: Vec<String>,
    primary_register: crate::register::Register,
) -> Effect {
    let mut flat: Vec<Effect> = Vec::new();
    for e in per_row_effects {
        flatten_effect(e, &mut flat);
    }

    let mut combined_edits: Vec<lattice_core::buffer::AppliedEdit> = Vec::new();
    let mut yank_registers: Vec<crate::register::Register> = Vec::new();
    let mut enter_mode: Option<ModalState> = None;
    for e in flat {
        match e {
            Effect::Edits(edits) => combined_edits.extend(edits),
            Effect::Yank { register, .. } => {
                if !yank_registers.contains(&register) {
                    yank_registers.push(register);
                }
            }
            Effect::EnterMode(m) => enter_mode = Some(m),
            // Other effects shouldn't surface from operator dispatch;
            // drop them defensively.
            _ => {}
        }
    }

    let joined = row_contents.join("\n");
    let mut out: Vec<Effect> = Vec::new();
    if !combined_edits.is_empty() {
        out.push(Effect::Edits(combined_edits));
    }
    // Vim's blockwise paste reads the unnamed register; the operator
    // closures emitted yanks to `primary_register` and possibly to a
    // numbered register (`"0` for yank). Preserve that fan-out, but
    // collapse content to one Blockwise blob.
    if !yank_registers.is_empty() {
        for reg in yank_registers {
            out.push(Effect::Yank {
                register: reg,
                content: joined.clone(),
                kind: YankKind::Blockwise,
            });
        }
    } else if !joined.is_empty() {
        // Defensive: if no yank surfaced (e.g. operator that doesn't
        // yank) but we have content, do nothing.
        let _ = primary_register;
    }
    if let Some(m) = enter_mode {
        out.push(Effect::EnterMode(m));
    }

    Effect::Many(out)
}

fn flatten_effect(e: Effect, out: &mut Vec<Effect>) {
    match e {
        Effect::Many(parts) => {
            for p in parts {
                flatten_effect(p, out);
            }
        }
        Effect::None => {}
        other => out.push(other),
    }
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
                    // Reached when a non-operator path resolves a
                    // grammar Range::Selection while Visual is
                    // Blockwise (e.g. a future motion that takes a
                    // range arg). Operators bypass this branch via
                    // `execute_operator_blockwise`. Fall back to a
                    // single contiguous range here -- no per-row
                    // semantics for non-operators in v1.
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
