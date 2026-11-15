//! Extract variable (RF.4e), natively — with no language server.
//!
//! **One expression or nothing.** The range, trimmed of whitespace and edge
//! separators, must be exactly one named node; a range that is not is
//! refused, as raise refuses one, because extracting part of an expression
//! changes what it computes.
//!
//! **Where the declaration goes.** On its own line before the statement
//! holding the expression, at that statement's indent. The statement is read
//! from the text, not from node kinds: the smallest node strictly around the
//! expression that starts a line, and whose parent starts on an earlier line
//! (or is the file). So `foo(\n    bar + 1,\n)` lifts `bar + 1` above the
//! `let`, not above the argument that happens to start a line.
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

use crate::lattice::plugin_host::buffer::Document;
use crate::lattice::plugin_host::tree_sitter::{Node, TreeSnapshot};
use crate::span::{covering_node, span_of, text, trim_list_edges, Span};

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

/// Whether `node` starts its line: only whitespace before it.
fn starts_line(doc: &Document, node: &Node) -> bool {
    let start = span_of(node).start;
    let line = doc.line(start.line).unwrap_or_default();
    line.get(..start.byte as usize)
        .is_some_and(|before| before.chars().all(char::is_whitespace))
}

/// The statement holding `expr`: the smallest node strictly around it that
/// starts a line and whose parent starts on an earlier line, or is the file.
fn statement_of(doc: &Document, expr: &Node) -> Option<Node> {
    let mut node = expr.parent();
    while let Some(n) = node {
        let parent = n.parent();
        let parent_earlier = match &parent {
            None => return None,
            Some(p) if p.parent().is_none() => true,
            Some(p) => span_of(p).start.line < span_of(&n).start.line,
        };
        if starts_line(doc, &n) && parent_earlier {
            return Some(n);
        }
        node = parent;
    }
    None
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
    let Some(template) = template.or_else(|| declaration_for(&tree.language())) else {
        return Err(format!(
            "no declaration form known for {} — set `refactor.declaration`, e.g. `let {{name}} = {{value}};`",
            tree.language()
        ));
    };
    let value = text(doc, range);
    let Some(decl) = declaration(template, &value) else {
        return Err("`refactor.declaration` must contain both `{name}` and `{value}`".into());
    };
    let Some(statement) = statement_of(doc, &expr) else {
        return Err("no statement around the expression to put the declaration before".into());
    };
    let stmt = span_of(&statement).start;
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

    #[test]
    fn snippet_syntax_is_escaped() {
        assert_eq!(escape_snippet("a$b}c\\d"), "a\\$b\\}c\\\\d");
    }
}
