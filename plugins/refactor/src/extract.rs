//! Extract variable (RF.4e), natively — with no language server.
//!
//! **One expression or nothing.** The range, trimmed of whitespace and edge
//! separators, must be exactly one named node; a range that is not is
//! refused, as raise refuses one, because extracting part of an expression
//! changes what it computes.
//!
//! **Where the declaration goes** (RF.4i, refactor.md §9.1). Before the
//! innermost `@refactor.statement` holding the expression, at its indent.
//! The plugin's `refactor.scm` names, per language, which nodes those are
//! (the children of a block) and which code does not run exactly once, in
//! order, when its statement runs (`@refactor.hoist-barrier.<why>`: the
//! right of `&&`, a branch, a loop's condition, a closure, a comprehension,
//! a default value). Lifting an expression out of one of those, past the
//! statement, would run it when it did not run before, so it is refused,
//! naming the construct. So is an expression that reads a name `locals.scm`
//! says is defined inside the statement before it: lifted, it would read
//! that name before its definition.
//!
//! RF.4e's text rule ("the smallest node that starts a line") is gone, not
//! kept as a fallback: it is what put a declaration inside a `match` arm.
//! A language with no `refactor.scm` is refused by name.
//!
//! **How it is declared.** A per-language template (`let {name} = {value};`),
//! or the buffer's `refactor.declaration` option when set, so a user or a
//! mode can say what a language this table does not know uses. A language
//! in neither is refused, named.
//!
//! **One snippet edit.** The declaration and the use are one replacement,
//! expanded as a snippet whose name placeholder is linked at both sites: the
//! name is selected, and typing it types it twice. One undo step.
//!
//! Extract FUNCTION is not here, on purpose: it needs the free variables,
//! what the code returns and the types of both, and guessed from text it
//! silently changes behaviour (refactor.md §9). That is the language
//! server's job, RF.4g.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::lattice::plugin_host::buffer::Document;
use crate::lattice::plugin_host::tree_sitter::{Query, TreeSnapshot};
use crate::span::{covering_node, span_of, text, trim_list_edges, Span};

/// `refactor.scm`'s source for the language named `language`, or `None`
/// for one native extract does not know. TSX shares TypeScript's.
pub fn refactor_source(language: &str) -> Option<&'static str> {
    Some(match language {
        "rust" => include_str!("../queries/rust/refactor.scm"),
        "python" => include_str!("../queries/python/refactor.scm"),
        "ruby" => include_str!("../queries/ruby/refactor.scm"),
        "go" => include_str!("../queries/go/refactor.scm"),
        "javascript" => include_str!("../queries/javascript/refactor.scm"),
        "typescript" | "tsx" => include_str!("../queries/typescript/refactor.scm"),
        "cpp" => include_str!("../queries/cpp/refactor.scm"),
        "java" => include_str!("../queries/java/refactor.scm"),
        "lua" => include_str!("../queries/lua/refactor.scm"),
        _ => return None,
    })
}

thread_local! {
    /// Each language's `refactor.scm`, compiled once per plugin instance.
    static QUERIES: RefCell<HashMap<String, Rc<Query>>> = RefCell::new(HashMap::new());
}

/// `language`'s compiled `refactor.scm`, compiling it the first time.
fn refactor_query(tree: &TreeSnapshot, language: &str) -> Result<Rc<Query>, String> {
    if let Some(q) = QUERIES.with(|m| m.borrow().get(language).cloned()) {
        return Ok(q);
    }
    let source = refactor_source(language).ok_or_else(|| {
        format!(
            "native extract does not know {language}'s statements — a language server can do this"
        )
    })?;
    let query = Rc::new(
        tree.compile_query(source)
            .map_err(|e| format!("refactor.scm for {language} does not compile: {e}"))?,
    );
    QUERIES.with(|m| {
        m.borrow_mut()
            .insert(language.to_string(), Rc::clone(&query))
    });
    Ok(query)
}

/// The placeholder name the declaration starts with, selected for overtyping.
const DEFAULT_NAME: &str = "value";

/// How `language` declares a variable, as a template over `{name}` and
/// `{value}`. `None` for a language that cannot without a type (C) or that
/// reads a variable differently from how it names it (bash's `$name`).
pub fn declaration_for(language: &str) -> Option<&'static str> {
    Some(match language {
        "rust" => "let {name} = {value};",
        "python" | "ruby" => "{name} = {value}",
        "go" => "{name} := {value}",
        "javascript" | "typescript" | "tsx" => "const {name} = {value};",
        "cpp" => "auto {name} = {value};",
        "java" => "var {name} = {value};",
        "lua" => "local {name} = {value}",
        _ => return None,
    })
}

/// `text` with the characters a snippet body gives meaning to escaped, so
/// copied code expands as itself.
pub fn escape_snippet(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '$' | '}' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The declaration `template` makes for `value`, as snippet text: the name
/// is placeholder 1, the value escaped. `None` when the template lacks
/// `{name}` or `{value}`.
pub fn declaration(template: &str, value: &str) -> Option<String> {
    if !template.contains("{name}") || !template.contains("{value}") {
        return None;
    }
    // The template is escaped first, so its literal `$` stays literal; its
    // two holes then read `{name\}` / `{value\}`. The value goes in last,
    // through a marker, so a `{name}` inside the copied code is never
    // mistaken for the hole.
    let escaped_template = escape_snippet(template)
        .replace("{value\\}", "\u{0}")
        .replace("{name\\}", &format!("${{1:{DEFAULT_NAME}}}"));
    Some(escaped_template.replace('\u{0}', &escape_snippet(value)))
}

/// The facts around an expression, as plain spans: what `refactor.scm`
/// and `locals.scm` say. Gathered through the tree; decided on here, so the
/// decisions are tested without one.
pub mod facts {
    use crate::span::Span;

    /// `refactor.scm`'s captures that touch the expression.
    #[derive(Debug, Default)]
    pub struct Structure {
        pub statements: Vec<Span>,
        /// Each barrier with its `<why>`.
        pub barriers: Vec<(Span, String)>,
    }

    /// `locals.scm`'s captures inside the statement, references already
    /// net of definitions and members (the contract's rule).
    #[derive(Debug, Default)]
    pub struct Locals {
        pub definitions: Vec<(Span, String)>,
        pub references: Vec<(Span, String)>,
    }

    /// The innermost statement holding `expr`.
    pub fn statement_for(expr: Span, structure: &Structure) -> Result<Span, String> {
        structure
            .statements
            .iter()
            .copied()
            .filter(|s| s.covers(expr))
            .min_by_key(|s| {
                (
                    s.end.line - s.start.line,
                    s.end.byte.wrapping_sub(s.start.byte),
                )
            })
            .ok_or_else(|| {
                "no statement around the expression to put the declaration before".into()
            })
    }

    /// Refuse when a barrier inside `statement` holds `expr`: lifted past
    /// the statement, it would run when it did not before.
    pub fn check_barriers(
        expr: Span,
        statement: Span,
        structure: &Structure,
    ) -> Result<(), String> {
        let inner = structure
            .barriers
            .iter()
            .filter(|(b, _)| *b != statement && statement.covers(*b) && b.covers(expr))
            .min_by_key(|(b, _)| {
                (
                    b.end.line - b.start.line,
                    b.end.byte.wrapping_sub(b.start.byte),
                )
            });
        match inner {
            Some((_, why)) => Err(format!("not extracted: {}", reason(why))),
            None => Ok(()),
        }
    }

    /// Refuse when `expr` reads a name defined inside `statement` before
    /// it: lifted, it would read that name before its definition.
    pub fn check_reads(expr: Span, statement: Span, locals: &Locals) -> Result<(), String> {
        let defined: Vec<&str> = locals
            .definitions
            .iter()
            .filter(|(d, _)| statement.start <= d.start && d.end <= expr.start)
            .map(|(_, name)| name.as_str())
            .collect();
        let read = locals
            .references
            .iter()
            .find(|(r, name)| expr.covers(*r) && defined.contains(&name.as_str()));
        match read {
            Some((_, name)) => Err(format!(
                "not extracted: it reads `{name}`, which is defined earlier in the same statement"
            )),
            None => Ok(()),
        }
    }

    /// What a barrier's `<why>` means, for the refusal.
    pub fn reason(why: &str) -> &'static str {
        match why {
            "short-circuit" => "it is on the right of a short-circuit (`&&`, `||`, `and`, `or`, `??`), which does not always run",
            "branch" => "it is in a branch that does not always run",
            "loop" => "it is in a loop's condition, step or body, which runs every time round",
            "closure" => "it is inside a closure or lambda, which runs when called, not here",
            "comprehension" => "it is inside a comprehension, which runs once per item",
            "default" => "it is a default value, which runs only when nothing is passed",
            _ => "it is in code that does not run exactly once with its statement",
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::span::{Pos, Span};

        fn sp(l0: u32, b0: u32, l1: u32, b1: u32) -> Span {
            Span {
                start: Pos { line: l0, byte: b0 },
                end: Pos { line: l1, byte: b1 },
            }
        }

        #[test]
        fn the_innermost_statement_wins() {
            let s = Structure {
                statements: vec![sp(0, 0, 5, 1), sp(2, 4, 2, 30), sp(9, 0, 9, 5)],
                barriers: vec![],
            };
            assert_eq!(statement_for(sp(2, 10, 2, 15), &s), Ok(sp(2, 4, 2, 30)));
            assert!(statement_for(sp(7, 0, 7, 3), &s).is_err(), "none holds it");
        }

        #[test]
        fn a_barrier_counts_only_inside_the_statement() {
            let stmt = sp(1, 4, 1, 40);
            let expr = sp(1, 20, 1, 25);
            // A closure body around the statement: the declaration goes inside it.
            let outside = Structure {
                statements: vec![stmt],
                barriers: vec![(sp(0, 0, 3, 1), "closure".into())],
            };
            assert!(check_barriers(expr, stmt, &outside).is_ok());
            // The right of `&&` inside the statement holds it.
            let inside = Structure {
                statements: vec![stmt],
                barriers: vec![(sp(1, 18, 1, 30), "short-circuit".into())],
            };
            let why = check_barriers(expr, stmt, &inside).unwrap_err();
            assert!(why.contains("short-circuit"), "{why}");
            // One beside it does not.
            let beside = Structure {
                statements: vec![stmt],
                barriers: vec![(sp(1, 30, 1, 38), "branch".into())],
            };
            assert!(check_barriers(expr, stmt, &beside).is_ok());
        }

        #[test]
        fn a_read_of_a_name_defined_earlier_in_the_statement_is_refused() {
            let stmt = sp(0, 0, 0, 30);
            let expr = sp(0, 20, 0, 25);
            let locals = Locals {
                definitions: vec![(sp(0, 5, 0, 6), "y".into()), (sp(0, 26, 0, 27), "z".into())],
                references: vec![
                    (sp(0, 21, 0, 22), "y".into()),
                    (sp(0, 23, 0, 24), "z".into()),
                ],
            };
            let why = check_reads(expr, stmt, &locals).unwrap_err();
            assert!(why.contains("`y`"), "{why}");
            // `z` is defined after the expression: lifting does not move a read
            // before its definition.
            let after = Locals {
                definitions: vec![(sp(0, 26, 0, 27), "z".into())],
                references: vec![(sp(0, 23, 0, 24), "z".into())],
            };
            assert!(check_reads(expr, stmt, &after).is_ok());
            // A name defined before the statement is fine.
            let before = Locals {
                definitions: vec![],
                references: vec![(sp(0, 21, 0, 22), "y".into())],
            };
            assert!(check_reads(expr, stmt, &before).is_ok());
        }

        #[test]
        fn every_why_the_queries_use_has_a_reason() {
            for why in [
                "short-circuit",
                "branch",
                "loop",
                "closure",
                "comprehension",
                "default",
            ] {
                assert_ne!(reason(why), reason("unknown"), "{why}");
            }
        }
    }
}

/// What `refactor.scm` says around `expr`: the statements holding it and
/// the barriers touching it.
fn structure(tree: &TreeSnapshot, query: &Query, expr: Span) -> facts::Structure {
    let mut out = facts::Structure::default();
    for cap in tree.run_query_ranges(query, Some(expr.into())) {
        let span = Span::from(cap.range);
        if cap.name == "refactor.statement" {
            out.statements.push(span);
        } else if let Some(why) = cap.name.strip_prefix("refactor.hoist-barrier") {
            out.barriers
                .push((span, why.trim_start_matches('.').to_string()));
        }
    }
    out
}

/// What `locals.scm` says inside `statement`, references net of
/// definitions and members.
fn locals_in(
    tree: &TreeSnapshot,
    doc: &Document,
    statement: Span,
) -> Result<facts::Locals, String> {
    // @example tree-sitter.tree-snapshot.language-query: Use the editor's compiled locals query, and refuse plainly when the language has none
    let Some(query) = tree.language_query("locals") else {
        return Err(format!(
            "no locals.scm for {} — a language server can do this",
            tree.language()
        ));
    };
    let captures = tree.run_query_ranges(&query, Some(statement.into()));
    // @end-example
    let mut out = facts::Locals::default();
    let mut not_reads = Vec::new();
    let mut refs = Vec::new();
    for cap in captures {
        let span = Span::from(cap.range);
        match cap.name.as_str() {
            "local.definition" => {
                not_reads.push(span);
                out.definitions.push((span, text(doc, span)));
            }
            "local.member" => not_reads.push(span),
            "local.reference" => refs.push(span),
            _ => {}
        }
    }
    out.references = refs
        .into_iter()
        .filter(|r| !not_reads.contains(r))
        .map(|r| (r, text(doc, r)))
        .collect();
    Ok(out)
}

/// What extracting `range` comes to: the span to replace (the statement's
/// start to the expression's end) and the snippet body replacing it.
pub fn extract(
    tree: &TreeSnapshot,
    doc: &Document,
    range: Span,
    template: Option<&str>,
) -> Result<(Span, String), String> {
    let range = trim_list_edges(doc, range);
    if range.start >= range.end {
        return Err("nothing to extract in an empty range".into());
    }
    let expr = match covering_node(tree, range) {
        Some(node) if span_of(&node) == range => node,
        _ => {
            return Err(
                "that range is not one expression — select a whole one (`<M-o>` grows a selection node by node)"
                    .into(),
            )
        }
    };
    if expr.is_error() || expr.parent().is_none() {
        return Err("the code here does not parse, so it is left alone".into());
    }
    let language = tree.language();
    let query = refactor_query(tree, &language)?;
    let Some(template) = template.or_else(|| declaration_for(&language)) else {
        return Err(format!(
            "no declaration form known for {} — set `refactor.declaration`, e.g. `let {{name}} = {{value}};`",
            language
        ));
    };
    let value = text(doc, range);
    let Some(decl) = declaration(template, &value) else {
        return Err("`refactor.declaration` must contain both `{name}` and `{value}`".into());
    };
    let expr_span = span_of(&expr);
    let around = structure(tree, &query, expr_span);
    let statement = facts::statement_for(expr_span, &around)?;
    facts::check_barriers(expr_span, statement, &around)?;
    facts::check_reads(expr_span, statement, &locals_in(tree, doc, statement)?)?;
    let stmt = statement.start;
    let line = doc.line(stmt.line).unwrap_or_default();
    let indent = line.get(..stmt.byte as usize).unwrap_or_default();
    let before = text(
        doc,
        Span {
            start: stmt,
            end: range.start,
        },
    );
    let body = format!("{decl}\n{indent}{}$1", escape_snippet(&before));
    Ok((
        Span {
            start: stmt,
            end: range.end,
        },
        body,
    ))
}

#[cfg(test)]
mod tests {
    use super::{declaration, declaration_for, escape_snippet};

    #[test]
    fn the_declaration_names_placeholder_one_and_escapes_the_value() {
        assert_eq!(
            declaration("let {name} = {value};", "a + 1").as_deref(),
            Some("let ${1:value} = a + 1;")
        );
        assert_eq!(
            declaration("{name} := {value}", "m[\"${x}\"]").as_deref(),
            Some("${1:value} := m[\"\\${x\\}\"]"),
            "copied code expands as itself"
        );
        assert_eq!(declaration("let x = {value};", "1"), None, "no name");
        assert_eq!(declaration("let {name};", "1"), None, "no value");
    }

    #[test]
    fn a_template_literal_dollar_survives() {
        // A template that itself uses `$` (PHP's `$name`) keeps it literal.
        assert_eq!(
            declaration("${name} = {value};", "1").as_deref(),
            Some("\\$${1:value} = 1;")
        );
    }

    #[test]
    fn languages_without_a_plain_declaration_are_not_guessed() {
        assert_eq!(declaration_for("rust"), Some("let {name} = {value};"));
        assert_eq!(declaration_for("go"), Some("{name} := {value}"));
        assert_eq!(declaration_for("c"), None, "needs a type");
        assert_eq!(declaration_for("bash"), None, "reads it as `$name`");
    }

    /// The declaration table and the `refactor.scm` set name the same
    /// languages: a form with no statement facts, or facts with no form,
    /// is a language extract half-knows.
    #[test]
    fn the_declaration_table_and_the_queries_agree() {
        for lang in [
            "rust",
            "python",
            "ruby",
            "go",
            "javascript",
            "typescript",
            "tsx",
            "cpp",
            "java",
            "lua",
            "c",
            "bash",
            "css",
            "html",
            "json",
            "markdown",
            "sql",
            "toml",
            "wit",
            "yaml",
        ] {
            assert_eq!(
                declaration_for(lang).is_some(),
                super::refactor_source(lang).is_some(),
                "{lang}"
            );
        }
    }

    #[test]
    fn snippet_syntax_is_escaped() {
        assert_eq!(escape_snippet("a$b}c\\d"), "a\\$b\\}c\\\\d");
    }
}
