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
use lattice_protocol::Position;
use lattice_protocol::position::Range;
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
        around_parameter: reg_with(
            registry,
            "text-object:around-parameter",
            "A parameter / argument with its separator (`aa`): the trailing \
             `, `, or the leading one for the last item. `@parameter.outer`.",
            "parameter.outer",
            with_separator,
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
    reg_with(registry, name, doc, capture_suffix, |range, _| range)
}

/// [`reg`], with the resolved span then reshaped from the buffer's text.
/// `aa` uses it to take its separator; the resolver stays a pure
/// "which node" question.
fn reg_with(
    registry: &mut CommandRegistry,
    name: &str,
    doc: &str,
    capture_suffix: &'static str,
    shape: fn(Range, &dyn Fn(u32) -> Option<String>) -> Range,
) -> TextObjectId {
    registry.register_text_object(
        name,
        doc,
        TextObjectSpec {
            apply: Arc::new(move |ctx| {
                Ok(ctx
                    .scope_resolver
                    .and_then(|r| r.scope_at(ctx.at.line, ctx.at.byte, capture_suffix))
                    .map(|range| shape(range, &|line| ctx.buffer.line(line)))
                    .unwrap_or_else(|| Range::empty(ctx.at)))
            }),
            args_schema: Vec::new(),
        },
    )
}

/// RF.4b′.2: `aa` is the item with ONE of its separators, so `daa` leaves
/// a well-formed list. The trailing one (`,` and the blanks after it on
/// that line) when there is one, as `daa` on `b` in `f(a, b, c)` leaves
/// `f(a, c)`. Else the leading one (the `,` and the blanks between it and
/// the item), so on the last item `f(a, c)` becomes `f(a)`. An only item
/// has neither and is the item alone. That is the nvim-treesitter-
/// textobjects and targets.vim result.
///
/// Only blanks on the item's own line are taken, never a line break: in a
/// list laid out one item per line, `daa` removes the item and its comma
/// and leaves the line structure for `dd` to own. Separators are `,`
/// in every language the object covers; the scan is over ASCII bytes, so
/// a byte column is never split inside a character.
fn with_separator(range: Range, line: &dyn Fn(u32) -> Option<String>) -> Range {
    let blank = |b: u8| b == b' ' || b == b'\t';
    if let Some(text) = line(range.end.line) {
        let bytes = text.as_bytes();
        let mut j = range.end.byte as usize;
        while j < bytes.len() && blank(bytes[j]) {
            j += 1;
        }
        if bytes.get(j) == Some(&b',') {
            j += 1;
            while j < bytes.len() && blank(bytes[j]) {
                j += 1;
            }
            return Range {
                start: range.start,
                end: Position::new(range.end.line, j as u32),
            };
        }
    }
    if let Some(text) = line(range.start.line) {
        let bytes = text.as_bytes();
        let mut i = (range.start.byte as usize).min(bytes.len());
        while i > 0 && blank(bytes[i - 1]) {
            i -= 1;
        }
        if i > 0 && bytes[i - 1] == b',' {
            return Range {
                start: Position::new(range.start.line, (i - 1) as u32),
                end: range.end,
            };
        }
    }
    range
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

    /// RF.4b′.2: which separator `aa` takes.
    #[test]
    fn around_parameter_takes_one_separator() {
        // (line, item start, item end, expected start, expected end)
        let one = |text: &str, from: u32, to: u32| {
            let text = text.to_string();
            let r = with_separator(
                Range {
                    start: Position::new(0, from),
                    end: Position::new(0, to),
                },
                &|l| (l == 0).then(|| text.clone()),
            );
            (r.start.byte, r.end.byte)
        };
        // First and middle items: the trailing `, `.
        assert_eq!(one("f(a, b, c)", 2, 3), (2, 5));
        assert_eq!(one("f(a, b, c)", 5, 6), (5, 8));
        // The last item: the leading `, `.
        assert_eq!(one("f(a, b, c)", 8, 9), (6, 9));
        // An only item: itself.
        assert_eq!(one("f(a)", 2, 3), (2, 3));
        // A trailing comma before a line break: the comma, not the break.
        assert_eq!(one("    b,", 4, 5), (4, 6));
        // Blanks before the comma are crossed (`a ,`).
        assert_eq!(one("f(a , b)", 2, 3), (2, 6));
        // Across lines: trailing on the end line, leading on the start line.
        let lines = ["f(g(", "  x), y)"];
        let r = with_separator(
            Range {
                start: Position::new(0, 2),
                end: Position::new(1, 4),
            },
            &|l| lines.get(l as usize).map(|t| t.to_string()),
        );
        assert_eq!((r.start, r.end), (Position::new(0, 2), Position::new(1, 6)));
    }
}
