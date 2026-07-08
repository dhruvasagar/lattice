//! Tree-sitter structural motions — `]f`/`[f`/`]F`/`[F` (function),
//! `]c`/`[c`/`]C`/`[C` (class), `]a`/`[a`/`]A`/`[A` (parameter),
//! `]l`/`[l`/`]L`/`[L` (loop). The motion counterpart to the structural
//! text objects (`text_objects.rs`); both read the same textobjects.scm
//! captures. See docs/dev/architecture/treesitter-motions.md.

use lattice_grammar::registry::{CommandRegistry, MotionResult, MotionSpec};
use lattice_grammar::{MotionId, NavBoundary, NavDir};

/// The sixteen structural motion [`MotionId`]s, returned by
/// [`register_syntax_motions`] so the host can bind their chords
/// (`]f`/`[f`/`]F`/`[F`, `]c`/`[c`/`]C`/`[C`, `]a`/`[a`/`]A`/`[A`,
/// `]l`/`[l`/`]L`/`[L`) into the Normal/Visual/operator-pending
/// motion keymap tables.
#[derive(Debug, Clone, Copy)]
pub struct SyntaxMotionIds {
    pub next_function_start: MotionId,
    pub prev_function_start: MotionId,
    pub next_function_end: MotionId,
    pub prev_function_end: MotionId,
    pub next_class_start: MotionId,
    pub prev_class_start: MotionId,
    pub next_class_end: MotionId,
    pub prev_class_end: MotionId,
    pub next_parameter_start: MotionId,
    pub prev_parameter_start: MotionId,
    pub next_parameter_end: MotionId,
    pub prev_parameter_end: MotionId,
    pub next_loop_start: MotionId,
    pub prev_loop_start: MotionId,
    pub next_loop_end: MotionId,
    pub prev_loop_end: MotionId,
}

/// Register the sixteen structural motions on `registry`. Call once at
/// boot (after the builtin grammar is registered); thread the returned
/// ids into the keymap binder. Calling twice registers duplicates, so
/// call exactly once.
pub fn register_syntax_motions(registry: &mut CommandRegistry) -> SyntaxMotionIds {
    SyntaxMotionIds {
        next_function_start: reg(
            registry,
            "motion:next-function-start",
            "Go to the start of the next function (`]f`). `@function.outer`.",
            "function.outer",
            NavDir::Forward,
            NavBoundary::Start,
        ),
        prev_function_start: reg(
            registry,
            "motion:prev-function-start",
            "Go to the start of the previous (or enclosing) function (`[f`).",
            "function.outer",
            NavDir::Backward,
            NavBoundary::Start,
        ),
        next_function_end: reg(
            registry,
            "motion:next-function-end",
            "Go to the end of the current/next function (`]F`).",
            "function.outer",
            NavDir::Forward,
            NavBoundary::End,
        ),
        prev_function_end: reg(
            registry,
            "motion:prev-function-end",
            "Go to the end of the previous function (`[F`).",
            "function.outer",
            NavDir::Backward,
            NavBoundary::End,
        ),

        next_class_start: reg(
            registry,
            "motion:next-class-start",
            "Go to the start of the next class/type (`]c`). `@class.outer`.",
            "class.outer",
            NavDir::Forward,
            NavBoundary::Start,
        ),
        prev_class_start: reg(
            registry,
            "motion:prev-class-start",
            "Go to the start of the previous (or enclosing) class (`[c`).",
            "class.outer",
            NavDir::Backward,
            NavBoundary::Start,
        ),
        next_class_end: reg(
            registry,
            "motion:next-class-end",
            "Go to the end of the current/next class (`]C`).",
            "class.outer",
            NavDir::Forward,
            NavBoundary::End,
        ),
        prev_class_end: reg(
            registry,
            "motion:prev-class-end",
            "Go to the end of the previous class (`[C`).",
            "class.outer",
            NavDir::Backward,
            NavBoundary::End,
        ),

        next_parameter_start: reg(
            registry,
            "motion:next-parameter-start",
            "Go to the start of the next parameter/argument (`]a`). `@parameter.outer`.",
            "parameter.outer",
            NavDir::Forward,
            NavBoundary::Start,
        ),
        prev_parameter_start: reg(
            registry,
            "motion:prev-parameter-start",
            "Go to the start of the previous parameter (`[a`).",
            "parameter.outer",
            NavDir::Backward,
            NavBoundary::Start,
        ),
        next_parameter_end: reg(
            registry,
            "motion:next-parameter-end",
            "Go to the end of the current/next parameter (`]A`).",
            "parameter.outer",
            NavDir::Forward,
            NavBoundary::End,
        ),
        prev_parameter_end: reg(
            registry,
            "motion:prev-parameter-end",
            "Go to the end of the previous parameter (`[A`).",
            "parameter.outer",
            NavDir::Backward,
            NavBoundary::End,
        ),

        next_loop_start: reg(
            registry,
            "motion:next-loop-start",
            "Go to the start of the next loop (`]l`). `@loop.outer`.",
            "loop.outer",
            NavDir::Forward,
            NavBoundary::Start,
        ),
        prev_loop_start: reg(
            registry,
            "motion:prev-loop-start",
            "Go to the start of the previous (or enclosing) loop (`[l`).",
            "loop.outer",
            NavDir::Backward,
            NavBoundary::Start,
        ),
        next_loop_end: reg(
            registry,
            "motion:next-loop-end",
            "Go to the end of the current/next loop (`]L`).",
            "loop.outer",
            NavDir::Forward,
            NavBoundary::End,
        ),
        prev_loop_end: reg(
            registry,
            "motion:prev-loop-end",
            "Go to the end of the previous loop (`[L`).",
            "loop.outer",
            NavDir::Backward,
            NavBoundary::End,
        ),
    }
}

/// Register one structural motion. `boundary == Start` ⇒ exclusive; `End` ⇒
/// inclusive (so `d]F` eats the closing brace). All are jumps (`<C-o>` returns);
/// jump recording is gated to standalone Normal use by the dispatcher.
fn reg(
    registry: &mut CommandRegistry,
    name: &str,
    doc: &str,
    suffix: &'static str,
    dir: NavDir,
    boundary: NavBoundary,
) -> MotionId {
    let exclusive = matches!(boundary, NavBoundary::Start);
    registry.register_motion(
        name,
        doc,
        MotionSpec {
            jump: true,
            exclusive,
            args_schema: Vec::new(),
            apply: Box::new(move |ctx| {
                let count = ctx.count.get().max(1);
                let target = ctx
                    .scope_resolver
                    .and_then(|r| {
                        r.scope_toward(ctx.from.line, ctx.from.byte, suffix, dir, boundary, count)
                    })
                    .unwrap_or(ctx.from); // graceful no-op: no tree / no match / boundary
                Ok(MotionResult {
                    target,
                    linewise: false,
                })
            }),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_sixteen_distinct_motions() {
        let mut registry = CommandRegistry::new();
        let ids = register_syntax_motions(&mut registry);
        let all = [
            ids.next_function_start, ids.prev_function_start, ids.next_function_end, ids.prev_function_end,
            ids.next_class_start,    ids.prev_class_start,    ids.next_class_end,    ids.prev_class_end,
            ids.next_parameter_start,ids.prev_parameter_start,ids.next_parameter_end,ids.prev_parameter_end,
            ids.next_loop_start,     ids.prev_loop_start,     ids.next_loop_end,     ids.prev_loop_end,
        ];
        let mut seen = std::collections::HashSet::new();
        for id in all {
            assert!(seen.insert(id.0), "duplicate motion id");
            let spec = registry.lookup(id.0).expect("registered");
            assert_eq!(spec.kind, lattice_grammar::CommandKind::Motion);
        }
        assert_eq!(seen.len(), 16);
        // Named + jump=true; start-targets exclusive, end-targets inclusive.
        assert!(registry.id_by_name("motion:next-function-start").is_some());
        assert!(registry.id_by_name("motion:prev-loop-end").is_some());
    }
}
