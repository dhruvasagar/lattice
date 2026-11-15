//! `:refactor-split` / `:refactor-join` (RF.2e): one item per line, or all on
//! one.
//!
//! **Comma lists only.** The node is the smallest named node at the cursor
//! with two or more named children and a comma between every pair — an
//! argument list, parameters, an array, a tuple, a struct literal — read from
//! the text, not from node kinds. Statement blocks and other lists whose
//! layout is the language's business are refused: a wrong split there is
//! worse than none.
//!
//! **The plugin never decides a language's style.** What comes before the
//! first item (`(`, `vec![`) and after the last (`)`, `]`) is kept, and so is
//! a trailing comma: present before, present after; absent, absent. The
//! indent is the buffer's own (`shiftwidth` / `expandtab`, read through
//! `get-option-in-buffer`, so Go's tabs stay tabs). Joining adds no padding
//! inside the delimiters, so `{ a, b }` split and joined comes back as
//! `{a, b}` — the one convention it cannot know.

use crate::lattice::plugin_host::buffer::Document;
use crate::lattice::plugin_host::tree_sitter::{Node, TreeSnapshot};
use crate::span::{span_of, text, Outcome, Pos, Span};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    /// One item per line.
    Split,
    /// All items on one line.
    Join,
}

/// The items of a comma list `n`, or `None` when `n` is not one of two or
/// more.
fn comma_items(doc: &Document, n: &Node) -> Option<Vec<Span>> {
    let count = n.named_child_count();
    if count < 2 {
        return None;
    }
    let items: Vec<Span> = (0..count)
        .filter_map(|i| n.named_child(i))
        .map(|c| span_of(&c))
        .collect();
    let all_commas = items.windows(2).all(|w| {
        let gap = Span {
            start: w[0].end,
            end: w[1].start,
        };
        text(doc, gap).trim() == ","
    });
    all_commas.then_some(items)
}

/// The smallest comma list around the cursor.
fn comma_list(tree: &TreeSnapshot, doc: &Document, cursor: Pos) -> Option<(Node, Vec<Span>)> {
    let mut node = tree.node_at(cursor.into());
    while let Some(n) = node {
        if n.is_named() {
            if let Some(items) = comma_items(doc, &n) {
                return Some((n, items));
            }
        }
        node = n.parent();
    }
    None
}

/// The leading whitespace of `line`.
fn indent_of(doc: &Document, line: u32) -> String {
    let text = doc.line(line).unwrap_or_default();
    text.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Lay out the comma list at `cursor`. `unit` is one level of indentation.
pub fn reshape(
    tree: &TreeSnapshot,
    doc: &Document,
    cursor: Pos,
    layout: Layout,
    unit: &str,
) -> Outcome {
    let Some((list, items)) = comma_list(tree, doc, cursor) else {
        return Outcome::Refuse(
            "not in a comma-separated list — split and join act on arguments, parameters, arrays and the like",
        );
    };
    if list.is_error() {
        return Outcome::Refuse("the code here does not parse, so it is left alone");
    }
    let whole = span_of(&list);
    let (first, last) = (items[0], items[items.len() - 1]);
    let open = text(
        doc,
        Span {
            start: whole.start,
            end: first.start,
        },
    );
    let after = text(
        doc,
        Span {
            start: last.end,
            end: whole.end,
        },
    );
    let (trailing, close) = match after.trim_start().strip_prefix(',') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, after.trim_start()),
    };
    let open = open.trim_end();
    let bodies: Vec<String> = items.iter().map(|s| text(doc, *s)).collect();

    let out = match layout {
        Layout::Split => {
            let base = indent_of(doc, whole.start.line);
            let mut out = format!("{open}\n");
            for (i, body) in bodies.iter().enumerate() {
                let comma = if i + 1 < bodies.len() || trailing {
                    ","
                } else {
                    ""
                };
                out.push_str(&format!("{base}{unit}{body}{comma}\n"));
            }
            out.push_str(&format!("{base}{close}"));
            out
        }
        Layout::Join => {
            let comma = if trailing { "," } else { "" };
            format!("{open}{}{comma}{close}", bodies.join(", "))
        }
    };
    if out == text(doc, whole) {
        return Outcome::Refuse(match layout {
            Layout::Split => "already one item per line",
            Layout::Join => "already on one line",
        });
    }
    Outcome::Edit {
        range: whole,
        text: out,
        cursor: whole.start,
    }
}

/// One indent level from the buffer's `shiftwidth` / `expandtab`, as the
/// formatted strings `get-option-in-buffer` returns.
pub fn indent_unit(shiftwidth: Option<&str>, expandtab: Option<&str>) -> String {
    if expandtab == Some("false") {
        return "\t".into();
    }
    let width = shiftwidth
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|w| (1..=32).contains(w))
        .unwrap_or(4);
    " ".repeat(width)
}

#[cfg(test)]
mod tests {
    use super::indent_unit;

    #[test]
    fn indent_unit_follows_the_buffer_options() {
        assert_eq!(indent_unit(Some("2"), Some("true")), "  ");
        assert_eq!(indent_unit(Some("8"), Some("false")), "\t");
        assert_eq!(
            indent_unit(None, None),
            "    ",
            "no registry: lattice's default shiftwidth"
        );
        assert_eq!(indent_unit(Some("x"), Some("true")), "    ");
    }
}
