//! `:refactor-raise` (RF.2d): replace the expression around the node at the
//! cursor with the node.
//!
//! **What is replaced.** The node's parent — unless the parent is a
//! *bracketed* node glued to the sibling before it, like the `(…)` of a call,
//! the `<…>` of a generic or the `[…]` of an index. That bracket is half of
//! a larger expression, and replacing it alone would leave `foo` stuck to
//! the raised text (`f(a, b)` → `fb`). There the parent's parent is replaced:
//! raising `b` out of `f(a, b)` gives `b`, as paredit's raise does. A
//! bracket that stands alone — an array literal, a parenthesised
//! expression — is itself the expression, so raising `b` out of
//! `let v = [a, b];` gives `let v = b;`.
//!
//! Read from the text (brackets, adjacency), never from node kinds, so one
//! rule serves every grammar. The edit is one replacement: one undo step.

use crate::lattice::plugin_host::buffer::Document;
use crate::lattice::plugin_host::tree_sitter::{Node, TreeSnapshot};
use crate::span::{span_of, text, Outcome, Pos};

/// Whether `text` opens with a bracket and closes with its partner.
pub fn is_bracketed(text: &str) -> bool {
    let (Some(first), Some(last)) = (text.chars().next(), text.chars().last()) else {
        return false;
    };
    text.chars().count() >= 2
        && matches!(
            (first, last),
            ('(', ')') | ('[', ']') | ('{', '}') | ('<', '>')
        )
}

/// The nearest ancestor of `n` that spans strictly more text.
fn larger_ancestor(n: &Node) -> Option<Node> {
    let span = span_of(n);
    let mut up = n.parent();
    while let Some(a) = up {
        if span_of(&a) != span {
            return Some(a);
        }
        up = a.parent();
    }
    None
}

/// The node raising `node` replaces.
fn target(doc: &Document, node: &Node) -> Option<Node> {
    let parent = larger_ancestor(node)?;
    let ps = span_of(&parent);
    let glued = parent
        .prev_named_sibling()
        .is_some_and(|s| span_of(&s).end == ps.start);
    if glued && is_bracketed(&text(doc, ps)) {
        larger_ancestor(&parent)
    } else {
        Some(parent)
    }
}

pub fn raise(tree: &TreeSnapshot, doc: &Document, cursor: Pos) -> Outcome {
    let Some(node) = tree.node_at(cursor.into()) else {
        return Outcome::Refuse("no syntax node here");
    };
    let Some(target) = target(doc, &node) else {
        return Outcome::Refuse("nothing to raise into");
    };
    // The root is the whole file: replacing it with one node would delete
    // everything else, which no one means by "raise".
    if target.parent().is_none() {
        return Outcome::Refuse("nothing to raise into — this is already top level");
    }
    if node.is_error() || target.is_error() {
        return Outcome::Refuse("the code here does not parse, so it is left alone");
    }
    let range = span_of(&target);
    Outcome::Edit {
        range,
        text: text(doc, span_of(&node)),
        cursor: range.start,
    }
}

#[cfg(test)]
mod tests {
    use super::is_bracketed;

    #[test]
    fn bracketed_means_a_matching_pair_around_the_whole_text() {
        for t in ["()", "(a, b)", "[1]", "{ x }", "<T>"] {
            assert!(is_bracketed(t), "{t:?}");
        }
        for t in ["", "(", "a(b)", "(a) + (b)x", "(a]", "foo"] {
            assert!(!is_bracketed(t), "{t:?}");
        }
    }
}
