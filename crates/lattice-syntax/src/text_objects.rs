//! N.1.4c: structural (tree-sitter) text objects -- the universal
//! `af`/`if` (function), `ac`/`ic` (class), `aa`/`ia` (parameter),
//! `al`/`il` (loop) grammar objects. They are first-class citizens of
//! Lattice's vim grammar (compose with every operator: `daf`, `vic`,
//! `yaa`, and the `zn` narrow operator's `znaf`), owned here in
//! `lattice-syntax` per the locked N.1 design.
//!
//! Each object's `apply` reads the cursor's enclosing scope from
//! `ctx.scope_resolver` -- the host threads the active buffer's
//! `SyntaxSnapshot` (which impls [`lattice_grammar::ScopeResolver`],
//! N.1.4b) into the [`lattice_grammar::registry::TextObjectContext`] via
//! `Document::dispatch_with_scope_resolver`. The resolver returns the
//! byte-precise `[start, end)` span of the smallest matching
//! `textobjects.scm` capture (`@function.outer`, `@loop.inner`, ...).
//!
//! Graceful failure: with no resolver (a plain-language buffer with no
//! syntax tree) or no enclosing match (cursor outside any function /
//! class / ...), `apply` returns an *empty* range at the cursor, so the
//! paired operator no-ops -- matching vim, where `daf` with no enclosing
//! function does nothing.

use lattice_grammar::TextObjectId;
use lattice_grammar::registry::{CommandRegistry, TextObjectSpec};
use std::sync::Arc;

/// The eight structural text-object [`TextObjectId`]s, returned by
/// [`register_syntax_text_objects`] so the host can bind their chords
/// (`af`/`if`/`ac`/`ic`/`aa`/`ia`/`al`/`il`) into the operator-pending
/// and Visual text-object keymap tables (mirrors how the builtin
/// objects' ids drive `register_text_object_resolutions`).
#[derive(Debug, Clone, Copy)]
pub struct SyntaxTextObjectIds {
    pub around_function: TextObjectId,
    pub inner_function: TextObjectId,
    pub around_class: TextObjectId,
    pub inner_class: TextObjectId,
    pub around_parameter: TextObjectId,
    pub inner_parameter: TextObjectId,
    pub around_loop: TextObjectId,
    pub inner_loop: TextObjectId,
}

/// Register the eight structural text objects on `registry`. Call once
/// at boot (after the builtin grammar is registered); thread the
/// returned ids into the keymap binder. Calling twice registers
/// duplicates, so call exactly once.
pub fn register_syntax_text_objects(registry: &mut CommandRegistry) -> SyntaxTextObjectIds {
    SyntaxTextObjectIds {
        around_function: reg(
            registry,
            "text-object:around-function",
            "A function -- the whole `fn` / `def` / declaration including its \
             body (vim-grammar `af`). Resolved from the tree-sitter \
             `@function.outer` capture.",
            "function.outer",
        ),
        inner_function: reg(
            registry,
            "text-object:inner-function",
            "Inner function -- the body block / suite (`if`). \
             `@function.inner`.",
            "function.inner",
        ),
        around_class: reg(
            registry,
            "text-object:around-class",
            "A class / type -- struct / enum / union / trait / impl / class \
             including its body (`ac`). `@class.outer`.",
            "class.outer",
        ),
        inner_class: reg(
            registry,
            "text-object:inner-class",
            "Inner class -- the body (field / declaration list) (`ic`). \
             `@class.inner`.",
            "class.inner",
        ),
        around_parameter: reg(
            registry,
            "text-object:around-parameter",
            "A parameter / argument (`aa`). `@parameter.outer`.",
            "parameter.outer",
        ),
        inner_parameter: reg(
            registry,
            "text-object:inner-parameter",
            "Inner parameter / argument (`ia`). `@parameter.inner`.",
            "parameter.inner",
        ),
        around_loop: reg(
            registry,
            "text-object:around-loop",
            "A loop -- for / while / loop including its body (`al`). \
             `@loop.outer`.",
            "loop.outer",
        ),
        inner_loop: reg(
            registry,
            "text-object:inner-loop",
            "Inner loop -- the body block / suite (`il`). `@loop.inner`.",
            "loop.inner",
        ),
    }
}

/// Register one structural object whose `apply` forwards the cursor to
/// `ctx.scope_resolver.scope_at(line, byte, capture_suffix)`. The
/// `'static` suffix is moved into the boxed closure.
fn reg(
    registry: &mut CommandRegistry,
    name: &str,
    doc: &str,
    capture_suffix: &'static str,
) -> TextObjectId {
    registry.register_text_object(
        name,
        doc,
        TextObjectSpec {
            apply: Arc::new(move |ctx| {
                Ok(ctx
                    .scope_resolver
                    .and_then(|r| r.scope_at(ctx.at.line, ctx.at.byte, capture_suffix))
                    .unwrap_or_else(|| lattice_protocol::position::Range::empty(ctx.at)))
            }),
            args_schema: Vec::new(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_eight_distinct_text_objects() {
        let mut registry = CommandRegistry::new();
        let ids = register_syntax_text_objects(&mut registry);
        let all = [
            ids.around_function,
            ids.inner_function,
            ids.around_class,
            ids.inner_class,
            ids.around_parameter,
            ids.inner_parameter,
            ids.around_loop,
            ids.inner_loop,
        ];
        let mut seen = std::collections::HashSet::new();
        for id in all {
            assert!(seen.insert(id.0), "duplicate text-object id");
            let spec = registry.lookup(id.0).expect("registered");
            assert_eq!(spec.kind, lattice_grammar::CommandKind::TextObject);
        }
        assert_eq!(seen.len(), 8, "expected 8 distinct structural objects");
        // Resolvable by their dashed namespaced names.
        assert!(registry.id_by_name("text-object:around-function").is_some());
        assert!(registry.id_by_name("text-object:inner-loop").is_some());
    }
}
