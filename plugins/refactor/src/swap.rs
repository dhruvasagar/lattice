//! `:refactor-swap-next` / `-prev` (RF.2c): move the item at the cursor past
//! its neighbour in a list.
//!
//! **Which node is "the item".** The smallest named node at the cursor that is
//! an element of a *list*: a node whose gap to a neighbouring named sibling is
//! a list separator ([`is_list_separator`]) — a comma, a semicolon, or
//! whitespace that holds a line break. On `a` in `f(a + 1, b)` that is
//! `a + 1`, not `a`: the gap between `a` and `1` is ` + `, which separates
//! operands, not items. Read from the text, never from a node kind, so the
//! same rule finds arguments, parameters, array elements, struct fields and
//! the statements of a block in every grammar.
//!
//! **One edit.** The swap is a single replacement of the span from the first
//! item it touches to the last, rebuilt with the items rotated and every gap
//! between them kept verbatim: `, ` stays where it was, and so does the line
//! break and indent between two statements. One edit is one undo step and one
//! reparse, and there is no second range to keep valid.

use crate::lattice::plugin_host::buffer::Document;
use crate::lattice::plugin_host::tree_sitter::{Node, TreeSnapshot};
use crate::span::{advance, span_of, text, Outcome, Pos, Span};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Next,
    Prev,
}

/// Whether `gap`, the text between two sibling nodes, separates list items.
pub fn is_list_separator(gap: &str) -> bool {
    let t = gap.trim();
    t == "," || t == ";" || (t.is_empty() && gap.contains('\n'))
}

/// The text between `a` and a later `b`.
fn gap(doc: &Document, a: Span, b: Span) -> String {
    text(
        doc,
        Span {
            start: a.end,
            end: b.start,
        },
    )
}

fn sibling(n: &Node, dir: Dir) -> Option<Node> {
    match dir {
        Dir::Next => n.next_named_sibling(),
        Dir::Prev => n.prev_named_sibling(),
    }
}

/// The smallest named node at `cursor` that is an item of a list.
fn list_item(tree: &TreeSnapshot, doc: &Document, cursor: Pos) -> Option<Node> {
    let mut node = tree.node_at(cursor.into());
    while let Some(n) = node {
        if n.is_named() {
            let r = span_of(&n);
            let after_prev = n
                .prev_named_sibling()
                .is_some_and(|p| is_list_separator(&gap(doc, span_of(&p), r)));
            let before_next = n
                .next_named_sibling()
                .is_some_and(|s| is_list_separator(&gap(doc, r, span_of(&s))));
            if after_prev || before_next {
                return Some(n);
            }
        }
        node = n.parent();
    }
    None
}

/// Move the item at `cursor` past `count` of its neighbours in `dir` — fewer
/// when the list ends first, so `5` on the second-last item moves it to the
/// end rather than refusing.
pub fn swap(tree: &TreeSnapshot, doc: &Document, cursor: Pos, dir: Dir, count: u32) -> Outcome {
    let Some(item) = list_item(tree, doc, cursor) else {
        return Outcome::Refuse("nothing to swap here — the cursor is not on an item of a list");
    };
    if item.is_error() {
        return Outcome::Refuse("the code here does not parse, so it is left alone");
    }
    let item_span = span_of(&item);

    // The neighbours it moves past, nearest first, each still separated from
    // the one before it like a list item.
    let mut passed: Vec<Span> = Vec::new();
    let mut last = item_span;
    let mut next = sibling(&item, dir);
    while passed.len() < count.max(1) as usize {
        let Some(s) = next else { break };
        let ss = span_of(&s);
        let between = match dir {
            Dir::Next => gap(doc, last, ss),
            Dir::Prev => gap(doc, ss, last),
        };
        if !is_list_separator(&between) {
            break;
        }
        if s.is_error() {
            return Outcome::Refuse("the code here does not parse, so it is left alone");
        }
        passed.push(ss);
        last = ss;
        next = sibling(&s, dir);
    }
    if passed.is_empty() {
        return Outcome::Refuse(match dir {
            Dir::Next => "no next item to swap with",
            Dir::Prev => "no previous item to swap with",
        });
    }

    // Slots in buffer order, and which slot's text lands in each.
    let k = passed.len();
    let (slots, order): (Vec<Span>, Vec<usize>) = match dir {
        // [item, s1..sk] → [s1..sk, item]
        Dir::Next => (
            std::iter::once(item_span).chain(passed).collect(),
            (1..=k).chain(std::iter::once(0)).collect(),
        ),
        // [sk..s1, item] → [item, sk..s1]
        Dir::Prev => (
            passed
                .into_iter()
                .rev()
                .chain(std::iter::once(item_span))
                .collect(),
            std::iter::once(k).chain(0..k).collect(),
        ),
    };
    let landing = match dir {
        Dir::Next => k,
        Dir::Prev => 0,
    };

    let texts: Vec<String> = slots.iter().map(|s| text(doc, *s)).collect();
    let region = Span {
        start: slots[0].start,
        end: slots[k].end,
    };
    let mut out = String::new();
    let mut cursor_at = region.start;
    for (i, &from) in order.iter().enumerate() {
        if i == landing {
            cursor_at = advance(region.start, &out);
        }
        out.push_str(&texts[from]);
        if i < k {
            out.push_str(&gap(doc, slots[i], slots[i + 1]));
        }
    }
    Outcome::Edit {
        range: region,
        text: out,
        cursor: cursor_at,
    }
}

#[cfg(test)]
mod tests {
    use super::is_list_separator;

    #[test]
    fn list_separators_are_commas_semicolons_and_line_breaks() {
        for gap in [",", ", ", " ,\n    ", ";", "\n    ", "\n\n"] {
            assert!(is_list_separator(gap), "{gap:?} separates items");
        }
        for gap in [" + ", " = ", "", " ", ".", " -> ", ": "] {
            assert!(!is_list_separator(gap), "{gap:?} does not");
        }
    }
}
