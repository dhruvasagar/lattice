//! M.2.b.3 (2026-06-01): excerpt-jump motions.
//!
//! Four motions register against `lattice-grammar`'s
//! `CommandRegistry` and bind to `]e` / `[e` / `]E` / `[E` in
//! `multibuffer-mode`'s keymap layer:
//!
//! | Chord | Motion id                          | Behaviour                                     |
//! |-------|------------------------------------|-----------------------------------------------|
//! | `]e`  | `multibuffer.next-excerpt-start`   | Cursor to first row of next excerpt           |
//! | `[e`  | `multibuffer.prev-excerpt-start`   | Cursor to first row of prev excerpt           |
//! | `]E`  | `multibuffer.next-file-boundary`   | Cursor to next excerpt with a different source |
//! | `[E`  | `multibuffer.prev-file-boundary`   | Cursor to prev excerpt with a different source |
//!
//! Handlers capture an `Arc<MultibufferRegistryHandle>` at
//! registration time, look up the typed handle via
//! `ctx.buffer_id`, and compute target rows from the captured
//! excerpts. Operators (`d`, `c`, `y`) compose with these motions
//! automatically — they fall out of `lattice-grammar`'s standard
//! operator+motion machinery, no per-motion plumbing.
//!
//! No-op behaviour: cursor stays put when there is no
//! next / prev excerpt (e.g. cursor in the last excerpt, `]e`
//! has no target).
//!
//! See `docs/dev/architecture/multibuffer-views.md` §3.7 +
//! slice plan M.2.b.3.

use lattice_core::BufferId;
use lattice_grammar::registry::{MotionContext, MotionResult};
use lattice_grammar::{
    CheckCancelled, CommandError, CommandRegistry, GrammarResult, MotionId, MotionSpec,
};
use lattice_protocol::position::Position;

use crate::Excerpt;
use crate::registry::MultibufferRegistryHandle;

/// The four motion ids registered by [`register_multibuffer_motions`].
/// Boot wiring threads this struct into the keymap-layer push so
/// chord bindings reach the right motion ids.
#[derive(Debug, Clone, Copy)]
pub struct MultibufferMotionIds {
    pub next_excerpt_start: MotionId,
    pub prev_excerpt_start: MotionId,
    pub next_file_boundary: MotionId,
    pub prev_file_boundary: MotionId,
}

/// Register the four excerpt-jump motions against `registry` and
/// return their ids. Handlers capture `mb_registry` (cheap Arc
/// clone) so they reach the typed view handle via
/// `ctx.buffer_id` at dispatch time.
///
/// Lives in `lattice-multibuffer` (the crate that owns the
/// excerpt data model). Boot wiring in `lattice-host` calls
/// this after `crate::actions::populate(&mut registry, ...)`.
pub fn register_multibuffer_motions(
    registry: &mut CommandRegistry,
    mb_registry: MultibufferRegistryHandle,
) -> MultibufferMotionIds {
    let mb_a = mb_registry.clone();
    let next_excerpt_start = registry.register_motion(
        "multibuffer.next-excerpt-start",
        "Move cursor to the first row of the next excerpt (`]e`).",
        MotionSpec {
            jump: true,
            exclusive: false,
            apply: Box::new(move |ctx| handle_next_excerpt_start(ctx, &mb_a)),
            args_schema: Vec::new(),
        },
    );

    let mb_b = mb_registry.clone();
    let prev_excerpt_start = registry.register_motion(
        "multibuffer.prev-excerpt-start",
        "Move cursor to the first row of the previous excerpt (`[e`).",
        MotionSpec {
            jump: true,
            exclusive: false,
            apply: Box::new(move |ctx| handle_prev_excerpt_start(ctx, &mb_b)),
            args_schema: Vec::new(),
        },
    );

    let mb_c = mb_registry.clone();
    let next_file_boundary = registry.register_motion(
        "multibuffer.next-file-boundary",
        "Move cursor to the next excerpt whose `source` BufferId differs from the current excerpt's (`]E`).",
        MotionSpec {
            jump: true,
            exclusive: false,
            apply: Box::new(move |ctx| handle_next_file_boundary(ctx, &mb_c)),
            args_schema: Vec::new(),
        },
    );

    let mb_d = mb_registry;
    let prev_file_boundary = registry.register_motion(
        "multibuffer.prev-file-boundary",
        "Move cursor to the previous excerpt whose `source` BufferId differs from the current excerpt's (`[E`).",
        MotionSpec {
            jump: true,
            exclusive: false,
            apply: Box::new(move |ctx| handle_prev_file_boundary(ctx, &mb_d)),
            args_schema: Vec::new(),
        },
    );

    MultibufferMotionIds {
        next_excerpt_start,
        prev_excerpt_start,
        next_file_boundary,
        prev_file_boundary,
    }
}

// ──────────────────────────────────────────────────────────────
// Motion handler shells: look up the typed handle, fall through
// to the pure helpers, wrap into MotionResult.
// ──────────────────────────────────────────────────────────────

fn handle_next_excerpt_start(
    ctx: &MotionContext,
    mb: &MultibufferRegistryHandle,
) -> GrammarResult<MotionResult> {
    let excerpts = excerpts_for_buffer(mb, ctx)?;
    let count = ctx.count.get().max(1);
    let target_row =
        next_excerpt_start_row(&excerpts, ctx.from.line, count).unwrap_or(ctx.from.line);
    Ok(MotionResult {
        target: Position::new(target_row, 0),
        linewise: false,
    })
}

fn handle_prev_excerpt_start(
    ctx: &MotionContext,
    mb: &MultibufferRegistryHandle,
) -> GrammarResult<MotionResult> {
    let excerpts = excerpts_for_buffer(mb, ctx)?;
    let count = ctx.count.get().max(1);
    let target_row =
        prev_excerpt_start_row(&excerpts, ctx.from.line, count).unwrap_or(ctx.from.line);
    Ok(MotionResult {
        target: Position::new(target_row, 0),
        linewise: false,
    })
}

fn handle_next_file_boundary(
    ctx: &MotionContext,
    mb: &MultibufferRegistryHandle,
) -> GrammarResult<MotionResult> {
    let excerpts = excerpts_for_buffer(mb, ctx)?;
    let count = ctx.count.get().max(1);
    let target_row =
        next_file_boundary_row(&excerpts, ctx.from.line, count).unwrap_or(ctx.from.line);
    Ok(MotionResult {
        target: Position::new(target_row, 0),
        linewise: false,
    })
}

fn handle_prev_file_boundary(
    ctx: &MotionContext,
    mb: &MultibufferRegistryHandle,
) -> GrammarResult<MotionResult> {
    let excerpts = excerpts_for_buffer(mb, ctx)?;
    let count = ctx.count.get().max(1);
    let target_row =
        prev_file_boundary_row(&excerpts, ctx.from.line, count).unwrap_or(ctx.from.line);
    Ok(MotionResult {
        target: Position::new(target_row, 0),
        linewise: false,
    })
}

fn excerpts_for_buffer(
    mb: &MultibufferRegistryHandle,
    ctx: &MotionContext,
) -> Result<Vec<Excerpt>, CommandError> {
    ctx.cancel.check()?;
    Ok(mb
        .handle(ctx.buffer_id)
        .map(|h| h.excerpts())
        .unwrap_or_default())
}

// ──────────────────────────────────────────────────────────────
// Pure helpers (unit-testable without grammar plumbing).
// ──────────────────────────────────────────────────────────────

/// Composed-row position of each excerpt's first row, in display
/// order. Equivalent to the prefix sum of `excerpt.line_count()`.
pub fn excerpt_start_rows(excerpts: &[Excerpt]) -> Vec<u32> {
    let mut starts = Vec::with_capacity(excerpts.len());
    let mut cursor: u32 = 0;
    for e in excerpts {
        starts.push(cursor);
        cursor = cursor.saturating_add(e.line_count());
    }
    starts
}

/// Index of the excerpt whose composed-row range contains
/// `cursor_row`. `None` when the cursor sits above the first
/// excerpt (only possible on empty views or row 0 with no
/// excerpts).
pub fn containing_excerpt_index(excerpts: &[Excerpt], cursor_row: u32) -> Option<usize> {
    if excerpts.is_empty() {
        return None;
    }
    let starts = excerpt_start_rows(excerpts);
    let mut found: Option<usize> = None;
    for (i, &start) in starts.iter().enumerate() {
        if start <= cursor_row {
            found = Some(i);
        } else {
            break;
        }
    }
    found
}

/// Composed row of the `count`-th excerpt strictly after the one
/// containing `cursor_row`. `None` when there is no such excerpt.
pub fn next_excerpt_start_row(excerpts: &[Excerpt], cursor_row: u32, count: u32) -> Option<u32> {
    if excerpts.is_empty() || count == 0 {
        return None;
    }
    let starts = excerpt_start_rows(excerpts);
    let current = containing_excerpt_index(excerpts, cursor_row).unwrap_or(0);
    let target_idx = current.checked_add(count as usize)?;
    starts.get(target_idx).copied()
}

/// Composed row of the `count`-th excerpt strictly before the
/// one containing `cursor_row`. `None` when no such excerpt.
pub fn prev_excerpt_start_row(excerpts: &[Excerpt], cursor_row: u32, count: u32) -> Option<u32> {
    if excerpts.is_empty() || count == 0 {
        return None;
    }
    let starts = excerpt_start_rows(excerpts);
    let current = containing_excerpt_index(excerpts, cursor_row)?;
    let target_idx = current.checked_sub(count as usize)?;
    starts.get(target_idx).copied()
}

/// Indices of excerpts that begin a new file group: an excerpt
/// is a file boundary if its `source` differs from the previous
/// excerpt's, or it is the first excerpt.
fn file_boundary_indices(excerpts: &[Excerpt]) -> Vec<usize> {
    let mut bounds = Vec::new();
    let mut prev_source: Option<BufferId> = None;
    for (i, e) in excerpts.iter().enumerate() {
        if prev_source != Some(e.source) {
            bounds.push(i);
            prev_source = Some(e.source);
        }
    }
    bounds
}

/// Composed row of the `count`-th file-boundary excerpt forward
/// of the boundary containing the current excerpt. A "file
/// boundary" is the FIRST excerpt of each file group (per
/// `]E` semantics: jump to the start of the next file in the
/// view).
pub fn next_file_boundary_row(excerpts: &[Excerpt], cursor_row: u32, count: u32) -> Option<u32> {
    if excerpts.is_empty() || count == 0 {
        return None;
    }
    let starts = excerpt_start_rows(excerpts);
    let bounds = file_boundary_indices(excerpts);
    let current = containing_excerpt_index(excerpts, cursor_row).unwrap_or(0);
    // Largest boundary index ≤ current.
    let cur_pos = bounds.iter().rposition(|&b| b <= current)?;
    let target_pos = cur_pos.checked_add(count as usize)?;
    let target_idx = *bounds.get(target_pos)?;
    starts.get(target_idx).copied()
}

/// Composed row of the `count`-th file-boundary excerpt backward
/// of the boundary containing the current excerpt. A "file
/// boundary" is the FIRST excerpt of each file group (per
/// `[E` semantics: jump to the start of the previous file).
pub fn prev_file_boundary_row(excerpts: &[Excerpt], cursor_row: u32, count: u32) -> Option<u32> {
    if excerpts.is_empty() || count == 0 {
        return None;
    }
    let starts = excerpt_start_rows(excerpts);
    let bounds = file_boundary_indices(excerpts);
    let current = containing_excerpt_index(excerpts, cursor_row)?;
    let cur_pos = bounds.iter().rposition(|&b| b <= current)?;
    let target_pos = cur_pos.checked_sub(count as usize)?;
    let target_idx = bounds[target_pos];
    starts.get(target_idx).copied()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_core::BufferId;

    use crate::{Excerpt, ExcerptHeader};

    fn ex(source: BufferId, start: u32, end: u32) -> Excerpt {
        Excerpt::new(source, start, end).with_header(ExcerptHeader::default())
    }

    #[test]
    fn start_rows_are_prefix_sums_of_line_counts() {
        let s = BufferId::next();
        let excerpts = vec![ex(s, 0, 2), ex(s, 5, 7), ex(s, 10, 10)];
        // line_count: 3, 3, 1
        assert_eq!(excerpt_start_rows(&excerpts), vec![0, 3, 6]);
    }

    #[test]
    fn containing_excerpt_walks_the_starts() {
        let s = BufferId::next();
        let excerpts = vec![ex(s, 0, 2), ex(s, 0, 2), ex(s, 0, 1)];
        // starts: [0, 3, 6]
        assert_eq!(containing_excerpt_index(&excerpts, 0), Some(0));
        assert_eq!(containing_excerpt_index(&excerpts, 2), Some(0));
        assert_eq!(containing_excerpt_index(&excerpts, 3), Some(1));
        assert_eq!(containing_excerpt_index(&excerpts, 5), Some(1));
        assert_eq!(containing_excerpt_index(&excerpts, 6), Some(2));
        assert_eq!(containing_excerpt_index(&excerpts, 100), Some(2));
    }

    #[test]
    fn next_excerpt_start_advances_count_excerpts() {
        let s = BufferId::next();
        let excerpts = vec![ex(s, 0, 1), ex(s, 0, 0), ex(s, 0, 2)];
        // starts: [0, 2, 3]
        assert_eq!(next_excerpt_start_row(&excerpts, 0, 1), Some(2));
        assert_eq!(next_excerpt_start_row(&excerpts, 0, 2), Some(3));
        assert_eq!(next_excerpt_start_row(&excerpts, 0, 3), None);
        assert_eq!(next_excerpt_start_row(&excerpts, 2, 1), Some(3));
        // From the last excerpt, no next.
        assert_eq!(next_excerpt_start_row(&excerpts, 3, 1), None);
    }

    #[test]
    fn prev_excerpt_start_backs_off_count_excerpts() {
        let s = BufferId::next();
        let excerpts = vec![ex(s, 0, 1), ex(s, 0, 0), ex(s, 0, 2)];
        // starts: [0, 2, 3]
        assert_eq!(prev_excerpt_start_row(&excerpts, 3, 1), Some(2));
        assert_eq!(prev_excerpt_start_row(&excerpts, 3, 2), Some(0));
        assert_eq!(prev_excerpt_start_row(&excerpts, 3, 3), None);
        assert_eq!(prev_excerpt_start_row(&excerpts, 2, 1), Some(0));
        // From the first excerpt, no prev.
        assert_eq!(prev_excerpt_start_row(&excerpts, 0, 1), None);
    }

    #[test]
    fn next_file_boundary_skips_excerpts_with_same_source() {
        let a = BufferId::next();
        let b = BufferId::next();
        let c = BufferId::next();
        let excerpts = vec![
            ex(a, 0, 1), // composed 0
            ex(a, 0, 0), // composed 2 — same source, skip
            ex(b, 0, 2), // composed 3 — first boundary
            ex(b, 0, 0), // composed 6 — same source as prior
            ex(c, 0, 1), // composed 7 — second boundary
        ];
        assert_eq!(next_file_boundary_row(&excerpts, 0, 1), Some(3));
        assert_eq!(next_file_boundary_row(&excerpts, 0, 2), Some(7));
        assert_eq!(next_file_boundary_row(&excerpts, 0, 3), None);
        // From within the b-source excerpt — next boundary is c.
        assert_eq!(next_file_boundary_row(&excerpts, 3, 1), Some(7));
        // From within the last excerpt — no next.
        assert_eq!(next_file_boundary_row(&excerpts, 7, 1), None);
    }

    #[test]
    fn prev_file_boundary_skips_excerpts_with_same_source() {
        let a = BufferId::next();
        let b = BufferId::next();
        let c = BufferId::next();
        let excerpts = vec![
            ex(a, 0, 1), // composed 0
            ex(b, 0, 2), // composed 2 — first b
            ex(b, 0, 0), // composed 5 — same as prior
            ex(c, 0, 1), // composed 6 — first c
        ];
        // From c, prev boundary is b at composed 2.
        assert_eq!(prev_file_boundary_row(&excerpts, 6, 1), Some(2));
        assert_eq!(prev_file_boundary_row(&excerpts, 6, 2), Some(0));
        assert_eq!(prev_file_boundary_row(&excerpts, 6, 3), None);
    }

    #[test]
    fn empty_excerpts_returns_none() {
        let excerpts: Vec<Excerpt> = Vec::new();
        assert_eq!(next_excerpt_start_row(&excerpts, 0, 1), None);
        assert_eq!(prev_excerpt_start_row(&excerpts, 0, 1), None);
        assert_eq!(next_file_boundary_row(&excerpts, 0, 1), None);
        assert_eq!(prev_file_boundary_row(&excerpts, 0, 1), None);
    }

    #[test]
    fn zero_count_returns_none() {
        let s = BufferId::next();
        let excerpts = vec![ex(s, 0, 1)];
        assert_eq!(next_excerpt_start_row(&excerpts, 0, 0), None);
        assert_eq!(prev_excerpt_start_row(&excerpts, 0, 0), None);
        assert_eq!(next_file_boundary_row(&excerpts, 0, 0), None);
        assert_eq!(prev_file_boundary_row(&excerpts, 0, 0), None);
    }
}
