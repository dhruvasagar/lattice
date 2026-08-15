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
//! `ExCommand` and `Action` paths are wired -- the latter via slice 8.i.0
//! (see `docs/dev/notes/8i-approach.md`); registry entries grow during slices
//! 8.i.1-3 as the legacy `Action` bridge in `lattice-ui-tui` retires.

use lattice_protocol::position::{Position, Range as ProtoRange};

use crate::cancel::{CancellationToken, CheckCancelled};
use crate::command::{CommandInvocation, CommandKind};
use crate::effect::{Effect, YankKind};
use crate::error::{CommandError, GrammarResult};
use crate::modal::ModalState;
use crate::range::Range;
use crate::registry::{
    ActionContext, CommandEntry, CommandRegistry, ExCommandContext, MotionContext, OperatorContext,
    TextObjectContext, require_action, require_ex_command, require_motion, require_operator,
    require_text_object,
};
use crate::target::Target;
use lattice_core::{Buffer, BufferId, Document};

/// Execute a `CommandInvocation` against `document`, using `registry` to
/// resolve motions / text-objects / operators.
///
/// `cursor` is the position the modal engine considers "current" -- typically
/// the primary selection's head.
///
/// `cancel` is the cooperative cancellation handle (DESIGN.md §5.2.5).
/// Hot loops inside evaluators poll `cancel.check()?` between iterations;
/// on a flipped token the dispatcher returns
/// [`CommandError::Cancelled`] and commits no `Effect`. Callers that
/// don't drive cancellation (tests, scripts) pass
/// [`CancellationToken::never`].
pub fn execute(
    registry: &CommandRegistry,
    document: &mut Document,
    buffer_id: BufferId,
    cursor: Position,
    invocation: CommandInvocation,
    cancel: &CancellationToken,
) -> GrammarResult<Effect> {
    // Most callers (and any buffer with no tree-sitter parse / no
    // comment syntax) dispatch with an empty env. The structural +
    // comment text objects resolve nothing in that case; everything
    // else is unaffected.
    execute_with_env(
        registry,
        document,
        buffer_id,
        cursor,
        invocation,
        cancel,
        crate::registry::GrammarEnv::default(),
    )
}

/// N.1.4a / N.1.6 (2026-06-10): `execute` plus the per-dispatch
/// [`GrammarEnv`](crate::registry::GrammarEnv) — the tree-sitter
/// `scope_resolver` (`af`/`ac`) and the `comment_syntax` (`aC`/`iC`).
/// The host builds the env (N.1.4b / N.1.6) and threads it down to the
/// `TextObjectContext`; the classic objects (`iw`, `ap`, `i{`) ignore it.
pub fn execute_with_env(
    registry: &CommandRegistry,
    document: &mut Document,
    buffer_id: BufferId,
    cursor: Position,
    invocation: CommandInvocation,
    cancel: &CancellationToken,
    env: crate::registry::GrammarEnv<'_>,
) -> GrammarResult<Effect> {
    // Honor any pre-existing cancellation request before we start.
    cancel.check()?;

    let entry = registry
        .entry(invocation.command)
        .ok_or(CommandError::UnknownCommand)?;

    match entry.spec.kind {
        CommandKind::Motion => {
            execute_motion(document, buffer_id, cursor, &invocation, entry, cancel, env)
        }
        CommandKind::TextObject => {
            execute_text_object(document, cursor, &invocation, entry, cancel, env)
        }
        CommandKind::Operator => execute_operator(
            registry,
            document,
            buffer_id,
            cursor,
            &invocation,
            entry,
            cancel,
            env,
        ),
        CommandKind::ExCommand => execute_ex_command(&invocation, entry, cancel),
        CommandKind::Action => {
            execute_action(document, buffer_id, cursor, &invocation, entry, cancel, env)
        }
    }
}

fn execute_action(
    document: &Document,
    buffer_id: BufferId,
    cursor: Position,
    invocation: &CommandInvocation,
    entry: &CommandEntry,
    cancel: &CancellationToken,
    env: crate::registry::GrammarEnv<'_>,
) -> GrammarResult<Effect> {
    let spec = require_action(entry)?;
    let ctx = ActionContext {
        args: invocation.args.clone(),
        register: invocation.register_or_default(),
        count: invocation.count_or_default(),
        cursor,
        buffer_id,
        // O(1) rope clone (Arc-shared nodes) — a point-in-time buffer view for
        // a plugin action's `document` handle (AP.0.1). Native actions ignore it.
        buffer: document.buffer().clone(),
        // TS.1: clone the per-dispatch tree snapshot (an `Arc` bump) into the
        // owned context so a plugin action's `tree-snapshot` handle reads the
        // SAME point-in-time tree the buffer above was cloned from (§7 version
        // agreement). Native actions ignore it; `None` when the buffer has no
        // parse.
        syntax: env.syntax.map(std::sync::Arc::clone),
        cancel: cancel.clone(),
    };
    (spec.apply)(&ctx)
}

/// Resolve a *motion-only* invocation against a bare [`Buffer`] +
/// cursor, without a [`Document`] / undo stack / selections.
///
/// This is the read-only path used by buffer kinds that aren't
/// document-shaped (today: help buffers; future: file-tree, outline,
/// diagnostics views per DESIGN.md §5.9). The chord grammar is
/// shared -- pressing `j` in a help buffer dispatches the same
/// `line_down` motion as in a code buffer -- but the motion runs
/// against a different rope.
///
/// Returns the resolved target [`Position`]. Operators / text-
/// objects / ex-commands return [`CommandError::InvalidArgs`] --
/// callers route those separately (yank possibly excepted, but yank
/// is an operator not a motion). The motion's `linewise` flag is
/// dropped: callers that need it (yank-by-motion, etc.) are not
/// expected on read-only buffers.
pub fn execute_motion_only(
    registry: &CommandRegistry,
    buffer: &Buffer,
    buffer_id: BufferId,
    cursor: Position,
    invocation: CommandInvocation,
    cancel: &CancellationToken,
    env: crate::registry::GrammarEnv<'_>,
) -> GrammarResult<Position> {
    cancel.check()?;
    let entry = registry
        .entry(invocation.command)
        .ok_or(CommandError::UnknownCommand)?;
    if !matches!(entry.spec.kind, CommandKind::Motion) {
        return Err(CommandError::InvalidArgs(
            "execute_motion_only only accepts motions",
        ));
    }
    let motion = require_motion(entry)?;
    let ctx = MotionContext {
        buffer,
        buffer_id,
        from: cursor,
        count: invocation.count_or_default(),
        has_explicit_count: invocation.count.is_some(),
        args: invocation.args.clone(),
        cancel,
        scope_resolver: env.scope_resolver,
    };
    let result = (motion.apply)(&ctx)?;
    Ok(result.target)
}

fn execute_ex_command(
    invocation: &CommandInvocation,
    entry: &CommandEntry,
    cancel: &CancellationToken,
) -> GrammarResult<Effect> {
    let spec = require_ex_command(entry)?;
    let ctx = ExCommandContext {
        bang: invocation.bang,
        args: invocation.args.clone(),
        range: invocation.range.clone(),
        register: invocation.register_or_default(),
        count: invocation.count_or_default(),
        cancel: cancel.clone(),
    };
    (spec.apply)(&ctx)
}

fn execute_motion(
    document: &Document,
    buffer_id: BufferId,
    cursor: Position,
    invocation: &CommandInvocation,
    entry: &CommandEntry,
    cancel: &CancellationToken,
    env: crate::registry::GrammarEnv<'_>,
) -> GrammarResult<Effect> {
    let motion = require_motion(entry)?;
    let ctx = MotionContext {
        buffer: document.buffer(),
        buffer_id,
        from: cursor,
        count: invocation.count_or_default(),
        has_explicit_count: invocation.count.is_some(),
        args: invocation.args.clone(),
        cancel,
        scope_resolver: env.scope_resolver,
    };
    let result = (motion.apply)(&ctx)?;
    // Motions emit a cursor-only jump — the modal engine's caller
    // takes the new position and updates state. We surface the
    // position via Effect::CursorMove, the semantically-clean
    // cursor-jump primitive (replaces the former SelectionChange-
    // with-collapsed-cursor pattern).
    Ok(Effect::CursorMove(result.target))
}

fn execute_text_object(
    document: &Document,
    cursor: Position,
    invocation: &CommandInvocation,
    entry: &CommandEntry,
    cancel: &CancellationToken,
    env: crate::registry::GrammarEnv<'_>,
) -> GrammarResult<Effect> {
    let tobj = require_text_object(entry)?;
    let ctx = TextObjectContext {
        buffer: document.buffer(),
        at: cursor,
        count: invocation.count_or_default(),
        args: invocation.args.clone(),
        cancel,
        scope_resolver: env.scope_resolver,
        comment_syntax: env.comment_syntax,
    };
    let range = (tobj.apply)(&ctx)?;
    // A bare text object (no operator) sets the selection to the object's
    // span -- this is how Visual mode's `viw` / `vaf` / `vaC` work, and it
    // is fully generic: the dispatcher does not branch on which object was
    // resolved. The object returns a half-open `[start, end)`; charwise
    // visual is inclusive of the head, so we place the head one byte before
    // `end`. A later operator (`d` / `y` / `c`) re-extends to `end` via
    // `resolve_grammar_range(Range::Selection)`, matching vim exactly.
    if range.is_empty() {
        return Ok(Effect::None);
    }
    let buffer = document.buffer();
    let head = buffer
        .position_to_byte(range.end)
        .ok()
        .filter(|&b| b > 0)
        .and_then(|b| buffer.byte_to_position(b - 1).ok())
        .unwrap_or(range.start);
    let mut selections = document.selections().clone();
    selections.replace_primary(lattice_protocol::selection::Selection {
        anchor: range.start,
        head,
        visual: None,
    });
    Ok(Effect::SelectionChange(selections))
}

fn execute_operator(
    registry: &CommandRegistry,
    document: &mut Document,
    buffer_id: BufferId,
    cursor: Position,
    invocation: &CommandInvocation,
    entry: &CommandEntry,
    cancel: &CancellationToken,
    env: crate::registry::GrammarEnv<'_>,
) -> GrammarResult<Effect> {
    let operator = require_operator(entry)?;

    // Blockwise visual is dispatched per-row -- but only for
    // operators that opted into it via `blockwise_per_row`. Rectangle
    // ops (`d`, `y`, `c`) want each row's column slice; linewise-
    // style ops (`>`, `<`, `gU`, `gu`, `g~`) want one contiguous
    // range covering anchor..head so the whole change is a single
    // undo unit, matching vim's behavior on visual selections.
    if let Some(Range::Selection) = invocation.range
        && matches!(
            document.selections().primary().visual,
            Some(lattice_protocol::selection::VisualMode::Blockwise)
        )
        && operator.blockwise_per_row
    {
        return execute_operator_blockwise(operator, document, invocation, cancel, env);
    }

    let motion_count = invocation.count_or_default();
    let target_range: ProtoRange = match (&invocation.range, &invocation.target) {
        (Some(grammar_range), _) => {
            resolve_grammar_range(document, grammar_range, cursor, motion_count.get())?
        }
        (None, Some(target)) => resolve_target(
            registry,
            document,
            buffer_id,
            cursor,
            target,
            motion_count,
            cancel,
            env,
        )?,
        (None, None) => return Err(CommandError::MissingTarget),
    };

    let visual_linewise = matches!(invocation.range, Some(Range::Selection))
        && matches!(
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
        cancel,
        indent: env.indent,
        indent_resolver: env.indent_resolver,
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
    cancel: &CancellationToken,
    env: crate::registry::GrammarEnv<'_>,
) -> GrammarResult<Effect> {
    let sel = document.selections().primary();
    let (top_line, bottom_line) = (
        sel.anchor.line.min(sel.head.line),
        sel.anchor.line.max(sel.head.line),
    );
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

    // Snapshot the pre-state of the affected line span so we can
    // collapse the per-row edits into a single batched edit at the
    // end. Block-visual selections always span a contiguous line
    // range, so we capture (top_line, 0) .. (bottom_line, EOL).
    let pre_top = Position::new(top_line, 0);
    let pre_bottom = Position::new(bottom_line, line_byte_len(document.buffer(), bottom_line));
    let pre_range = ProtoRange::new(pre_top, pre_bottom);

    // Run apply per row, bottom-up. Collect every produced Effect.
    let register = invocation.register_or_default();
    let count = invocation.count_or_default();
    let args = invocation.args.clone();
    let mut per_row_effects: Vec<Effect> = Vec::with_capacity(row_ranges.len());
    for r in row_ranges.iter().rev() {
        // Per-row cancellation check: lets the user Esc out of a
        // blockwise op spanning many rows even if a single row's
        // apply doesn't poll.
        cancel.check()?;
        let mut ctx = OperatorContext {
            document,
            range: *r,
            linewise: false,
            register,
            count,
            args: args.clone(),
            cancel,
            indent: env.indent,
            indent_resolver: env.indent_resolver,
        };
        let eff = (operator.apply)(&mut ctx)?;
        per_row_effects.push(eff);
    }
    // Restore top-down order so the merged effect ordering tracks the
    // original row order (matters for some downstream consumers, even
    // though Edits / Yanks themselves don't carry an order beyond
    // their internal vec).
    per_row_effects.reverse();

    // Coalesce the per-row edits into a single batched undo unit.
    // Each row's `apply` may have called `apply_edit` once (delete /
    // change) or zero times (yank); count the AppliedEdits, undo
    // them, and re-emit a single `Edit::replace` covering the
    // affected line span with the post-state text. The user's `u`
    // then reverts the whole rectangle in one step, matching vim.
    let edit_count: usize = per_row_effects.iter().map(count_applied_edits).sum();
    let collapsed_edit = if edit_count > 1 {
        // Capture post-state of the same line span. Line numbers
        // didn't shift -- block-visual operators only modify column
        // slices within rows; they never delete whole rows.
        let post_bottom = Position::new(bottom_line, line_byte_len(document.buffer(), bottom_line));
        let post_range = ProtoRange::new(pre_top, post_bottom);
        let post_text = document.buffer().slice(post_range)?;

        // Rewind every per-row edit, then commit one batched
        // Edit::replace covering the original span.
        for _ in 0..edit_count {
            // Undo errors here would mean the document's undo stack
            // diverged from what we just pushed -- treat as a hard
            // failure so callers see it rather than silently leaving
            // the buffer in a half-rolled-back state.
            document
                .undo()
                .map_err(|_| CommandError::InvalidArgs("blockwise undo coalesce failed"))?;
        }
        let edit = lattice_protocol::edit::Edit::replace(pre_range, &post_text);
        let applied = document
            .apply_edit(edit)
            .map_err(|_| CommandError::InvalidArgs("blockwise batched apply failed"))?;
        Some(applied)
    } else {
        None
    };

    // After a rectangle op the cursor should land at the block's
    // top-left corner -- vim's behavior. The collapsed Edit's
    // `original_range.start` is `(top_line, 0)` (we replace full
    // lines), which would otherwise drag the cursor to column 0.
    // Emit a SelectionChange in the merged effect so the App's
    // apply_effect overrides the handle_edits column. Skip if the
    // operator was yank-only (no edits, cursor untouched).
    let cursor_target = if edit_count > 0 {
        let line_len = line_byte_len(document.buffer(), top_line);
        Some(Position::new(top_line, left_col.min(line_len)))
    } else {
        None
    };

    Ok(merge_blockwise_effects(
        per_row_effects,
        row_contents,
        register,
        collapsed_edit,
        cursor_target,
        document,
    ))
}

/// Sum of the `AppliedEdit`s contained in an Effect (recursing into
/// `Effect::Many`). Used to count how many `apply_edit` calls were
/// made by a single per-row operator dispatch so the blockwise
/// coalesce can rewind exactly that many.
fn count_applied_edits(effect: &Effect) -> usize {
    match effect {
        Effect::Edits(v) => v.len(),
        Effect::Many(inner) => inner.iter().map(count_applied_edits).sum(),
        _ => 0,
    }
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
    collapsed_edit: Option<lattice_core::buffer::AppliedEdit>,
    cursor_target: Option<Position>,
    _document: &Document,
) -> Effect {
    let mut flat: Vec<Effect> = Vec::new();
    for e in per_row_effects {
        flatten_effect(e, &mut flat);
    }

    let mut combined_edits: Vec<lattice_core::buffer::AppliedEdit> = Vec::new();
    // Each entry keeps its `explicit_yank` flag so the collapsed blockwise
    // yank preserves clipboard eligibility (a Visual-block `y` mirrors; a
    // block delete does not).
    let mut yank_registers: Vec<(crate::register::Register, bool)> = Vec::new();
    let mut enter_mode: Option<ModalState> = None;
    for e in flat {
        match e {
            Effect::Edits(edits) => combined_edits.extend(edits),
            Effect::Yank {
                register,
                explicit_yank,
                ..
            } => {
                if !yank_registers.iter().any(|(r, _)| *r == register) {
                    yank_registers.push((register, explicit_yank));
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
    // Prefer the dispatcher-supplied collapsed edit if it built one
    // (multi-row case): the per-row AppliedEdits were rewound and
    // re-emitted as a single batched edit; surfacing the per-row
    // copies here would mislead the host's `handle_edits` cursor
    // logic. Single-edit cases (one-row dispatch, yank-only) keep
    // the per-row Edits.
    if let Some(applied) = collapsed_edit {
        out.push(Effect::Edits(vec![applied]));
    } else if !combined_edits.is_empty() {
        out.push(Effect::Edits(combined_edits));
    }
    // Vim's blockwise paste reads the unnamed register; the operator
    // closures emitted yanks to `primary_register` and possibly to a
    // numbered register (`"0` for yank). Preserve that fan-out, but
    // collapse content to one Blockwise blob.
    if !yank_registers.is_empty() {
        for (reg, explicit_yank) in yank_registers {
            out.push(Effect::Yank {
                register: reg,
                content: joined.clone(),
                kind: YankKind::Blockwise,
                explicit_yank,
            });
        }
    } else if !joined.is_empty() {
        // Defensive: if no yank surfaced (e.g. operator that doesn't
        // yank) but we have content, do nothing.
        let _ = primary_register;
    }
    // Override the cursor to the block's top-left corner (post-edit)
    // so vim's "cursor lands at the start of the visual selection"
    // semantic holds. The collapsed Edit's original_range.start is
    // (top_line, 0) -- without this override the host's handle_edits
    // would drag the cursor to column 0.
    if let Some(pos) = cursor_target {
        out.push(Effect::CursorMove(pos));
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
    buffer_id: BufferId,
    cursor: Position,
    target: &Target,
    count: crate::command::Count,
    cancel: &CancellationToken,
    env: crate::registry::GrammarEnv<'_>,
) -> GrammarResult<ProtoRange> {
    match target {
        Target::Motion(motion_id, args) => {
            let entry = registry
                .entry(motion_id.0)
                .ok_or(CommandError::UnknownCommand)?;
            let motion = require_motion(entry)?;
            let ctx = MotionContext {
                buffer: document.buffer(),
                buffer_id,
                from: cursor,
                count,
                has_explicit_count: false,
                args: args.clone(),
                cancel,
                scope_resolver: env.scope_resolver,
            };
            let r = (motion.apply)(&ctx)?;
            let mut target = r.target;
            // Vim word-motion special case (`:help word-motions`): when a
            // word-forward motion (`w` / `W`) is used with an operator and
            // the last word moved over is at the end of a line, the operated
            // text ends at that line end -- it does NOT reach over the
            // newline into the next line's first word. Fires only for
            // word-forward-class motions and only when the motion actually
            // landed on a later line. (This is the operator path only;
            // plain `w` navigation still crosses lines.)
            if registry.is_word_forward_motion(motion_id.0) && target.line > cursor.line {
                let buffer = document.buffer();
                target = Position::new(cursor.line, line_byte_len(buffer, cursor.line));
            }
            Ok(motion_to_range(
                document.buffer(),
                cursor,
                target,
                motion.exclusive,
                r.linewise,
            ))
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
                cancel,
                scope_resolver: env.scope_resolver,
                comment_syntax: env.comment_syntax,
            };
            (tobj.apply)(&ctx)
        }
        Target::Range(grammar_range) => resolve_grammar_range(document, grammar_range, cursor, 1),
    }
}

fn resolve_grammar_range(
    document: &Document,
    range: &Range,
    cursor: Position,
    count: u32,
) -> GrammarResult<ProtoRange> {
    let count = count.max(1);
    match range {
        Range::Whole => {
            let buffer = document.buffer();
            // CV.3: content space. `:%` covers the buffer's real last
            // line — ropey's raw count would extend a whole-buffer
            // range onto the phantom line after the terminating
            // newline.
            let last_line = buffer.content_line_count().saturating_sub(1);
            let start = Position::ZERO;
            let end = Position::new(last_line, line_byte_len(buffer, last_line));
            Ok(ProtoRange::new(start, end))
        }
        Range::CurrentLine => {
            // Vim's `2dd` / `2yy` / `2>>` / etc.: count expands the
            // linewise extent. Range covers `cursor.line` ..
            // `cursor.line + count - 1`, clamped to the buffer's
            // last addressable line.
            let buffer = document.buffer();
            // CV.3: content space — `2dd` at the end of a file must
            // clamp to the last real line, not the phantom one.
            let last = buffer.content_line_count().saturating_sub(1);
            let start_line = cursor.line;
            let end_line = start_line.saturating_add(count.saturating_sub(1)).min(last);
            Ok(ProtoRange::new(
                Position::new(start_line, 0),
                Position::new(end_line, line_byte_len(buffer, end_line)),
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

/// Turn an operator's `(cursor, motion-target)` pair into the byte range
/// the operator acts on, honouring vim's exclusive/inclusive motion
/// distinction. Ranges are half-open `[start, end)`.
///
/// - **Exclusive** motions (`w`, `b`, `0`, ...) delete up to but not
///   including the target: `[min, max)`.
/// - **Inclusive** motions (`e`, `f`, `t`, `$`, ...) also cover the
///   character *at* the target. For a forward motion that means extending
///   the end one character past the target (so `de` deletes through the
///   last letter of the word, matching vim -- previously the target char
///   was left behind). For a backward inclusive motion the target already
///   is the range start, so the range is `[target, cursor)` unchanged.
fn motion_to_range(
    buffer: &lattice_core::Buffer,
    from: Position,
    to: Position,
    exclusive: bool,
    linewise: bool,
) -> ProtoRange {
    // Linewise motions (`j`/`k`/`gg`/`G`) don't take a charwise
    // inclusive-end adjustment -- their whole-line semantics are handled
    // by the linewise path, and advancing a character here would be
    // meaningless. Exclusive motions and empty ranges are `[min, max)`.
    if exclusive || linewise || to == from {
        let (a, b) = ordered(from, to);
        return ProtoRange::new(a, b);
    }
    if to > from {
        // Forward inclusive: cover the character under the target.
        ProtoRange::new(from, advance_one_char(buffer, to))
    } else {
        // Backward inclusive: target is the range start; the character
        // under the original cursor is not part of the operation.
        ProtoRange::new(to, from)
    }
}

/// Byte position one UTF-8 character past `pos`, clamped to the buffer
/// end. `pos` is assumed to be a char boundary (motion targets always
/// are); a target at end-of-buffer returns `pos` unchanged.
fn advance_one_char(buffer: &lattice_core::Buffer, pos: Position) -> Position {
    let Ok(idx) = buffer.position_to_byte(pos) else {
        return pos;
    };
    let text = buffer.as_string();
    let step = text
        .get(idx..)
        .and_then(|s| s.chars().next())
        .map(|c| c.len_utf8())
        .unwrap_or(0);
    if step == 0 {
        return pos;
    }
    buffer.byte_to_position(idx + step).unwrap_or(pos)
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use std::sync::Arc;

    use super::*;
    use crate::CancellationToken;
    use crate::app_effect::AppEffect;
    use crate::registry::ActionSpec;

    /// Slice 8.i.0 wiring: a `CommandKind::Action` registry entry
    /// flows through `execute()` and surfaces the spec's
    /// `Effect::AppAction(...)` payload. The carrier exists; later
    /// slices populate it from the per-mode keymap modules as the
    /// `bind_legacy` bridge retires.
    #[test]
    fn execute_routes_action_kind_to_action_spec() {
        let mut registry = CommandRegistry::new();
        let id = registry.register_action(
            "test:quit-action",
            "smoke variant for slice 8.i.0",
            ActionSpec {
                apply: Arc::new(|_ctx| Ok(Effect::AppAction(AppEffect::Quit))),
                args_schema: vec![],
            },
        );

        let mut doc = lattice_core::Document::empty();
        let inv = CommandInvocation::of(id);
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match eff {
            Effect::AppAction(AppEffect::Quit) => {}
            other => panic!("expected Effect::AppAction(Quit), got {other:?}"),
        }
    }

    /// `require_action`'s kind-mismatch path: dispatching a motion
    /// id through the `Action` branch (impossible in normal flow,
    /// but the helper is shared) errors with the labeled mismatch
    /// rather than panicking.
    #[test]
    fn action_branch_rejects_non_action_entries() {
        let mut registry = CommandRegistry::new();
        let _ = crate::builtins::populate(&mut registry);
        // Look up a known motion id; pretend-route it through the
        // action helper directly to confirm the require_action
        // gate behaves.
        let motion_id = registry.id_by_name("motion:word-forward").unwrap();
        let entry = registry.entry(motion_id).unwrap();
        let err = crate::registry::require_action(entry).expect_err("motion entry must reject");
        assert!(
            matches!(
                err,
                crate::error::CommandError::KindMismatch { expected, actual }
                    if expected == "action" && actual == "motion"
            ),
            "unexpected error: {err:?}"
        );
    }

    /// Visual-foundation slice: a *bare* text object (no operator)
    /// must set the selection to the object's span rather than
    /// no-op. This is what drives Visual mode's `viw` / `vaw` /
    /// `vaf`. The object returns a half-open `[start, end)`; the
    /// resulting charwise selection is inclusive of the head, so
    /// the head lands one byte before `end` and a later operator
    /// re-extends via `resolve_grammar_range(Range::Selection)`.
    #[test]
    fn bare_text_object_sets_selection_to_object_span() {
        let mut registry = CommandRegistry::new();
        let builtins = crate::builtins::populate(&mut registry);
        let mut doc = lattice_core::Document::from_text("foo bar baz");
        // Cursor on the `b` of "bar" (line 0, byte 4).
        let cursor = Position::new(0, 4);
        let inv = CommandInvocation::of(builtins.inner_word.0);
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match eff {
            Effect::SelectionChange(set) => {
                let p = set.primary();
                // inner-word "bar" = bytes [4, 7); charwise head = 6.
                assert_eq!(p.anchor, Position::new(0, 4), "anchor at word start");
                assert_eq!(p.head, Position::new(0, 6), "head one byte before end");
                assert!(
                    p.visual.is_none(),
                    "bare object leaves the kind to the host"
                );
            }
            other => panic!("expected Effect::SelectionChange, got {other:?}"),
        }
    }

    /// TSM.1: a motion's `apply` can read `ctx.scope_resolver` and call
    /// `scope_toward` -- proving the env threads through
    /// `execute_with_env` -> `execute_motion` -> `MotionContext` exactly
    /// like the existing tree-sitter text-object seam
    /// (`bare_text_object_sets_selection_to_object_span` above / N.1.4b).
    #[test]
    fn motion_context_carries_scope_resolver() {
        use crate::registry::{NavBoundary, NavDir, ScopeResolver};

        struct FixedResolver;
        impl ScopeResolver for FixedResolver {
            fn scope_at(
                &self,
                _l: u32,
                _c: u32,
                _s: &str,
            ) -> Option<lattice_protocol::position::Range> {
                None
            }
            fn scope_toward(
                &self,
                _l: u32,
                _c: u32,
                _s: &str,
                _d: NavDir,
                _b: NavBoundary,
                _n: u32,
            ) -> Option<lattice_protocol::Position> {
                Some(lattice_protocol::Position::new(7, 0))
            }
        }

        // A motion whose apply forwards to the resolver and returns its target.
        let mut registry = CommandRegistry::new();
        let m = registry.register_motion(
            "motion:test-nav",
            "test",
            crate::registry::MotionSpec {
                jump: false,
                exclusive: true,
                args_schema: Vec::new(),
                apply: Arc::new(|ctx| {
                    let p = ctx
                        .scope_resolver
                        .and_then(|r| {
                            r.scope_toward(
                                ctx.from.line,
                                ctx.from.byte,
                                "function.outer",
                                NavDir::Forward,
                                NavBoundary::Start,
                                1,
                            )
                        })
                        .unwrap_or(ctx.from);
                    Ok(crate::registry::MotionResult {
                        target: p,
                        linewise: false,
                    })
                }),
            },
        );

        let mut doc = lattice_core::Document::from_text("fn a() {}\n");
        let resolver = FixedResolver;
        let env = crate::registry::GrammarEnv {
            scope_resolver: Some(&resolver),
            comment_syntax: None,
            syntax: None,
            ..Default::default()
        };
        let eff = execute_with_env(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            CommandInvocation::of(m.0),
            &CancellationToken::never(),
            env,
        )
        .unwrap();
        // The motion resolved to row 7 via the resolver.
        match eff {
            Effect::CursorMove(pos) => {
                assert_eq!(pos.line, 7);
            }
            other => panic!("expected CursorMove, got {other:?}"),
        }
    }
}
