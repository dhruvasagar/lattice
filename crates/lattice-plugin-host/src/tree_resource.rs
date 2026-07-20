//! TS.1 host backing for the `tree-snapshot` / `node` WIT resources
//! (plugin-treesitter-seam.md §3). The host owns the parse tree; a plugin gets
//! read-only handles and calls back for the structure it needs — the tree never
//! crosses the boundary (the `document`-handle model, applied to structure).
//!
//! **Snapshot backing.** A `tree-snapshot` wraps an `Arc<SyntaxSnapshot>` (an
//! O(1) `ArcSwap` bump; `lattice-syntax` already runs reparses off-thread). It
//! is immutable, so a handle stays coherent with its own point-in-time tree even
//! as edits land underneath (§7). The trampoline mints it only when the snapshot
//! actually has a `tree()` — so a handed-over resource always resolves.
//!
//! **Node backing = a path of child indices from the root.** A `NodeResource`
//! is `(Arc<SyntaxSnapshot>, Vec<u32>)` where the vec is `Node::child` indices
//! (ALL children, incl. anonymous) from root to the node. Every method
//! re-resolves the node against the snapshot's tree. This is deliberately NOT a
//! stored `tree_sitter::Node<'tree>` (which borrows the `Tree` and can't be a
//! `'static` resource) and NOT a byte-range + kind (which collide on wrapper
//! nodes sharing a span). The path is safe and unambiguous; resolution is
//! O(depth), and the one sync on-keystroke use (`enclosing`) is a single
//! ancestor walk over an already-parsed tree — no parsing on the dispatch thread.

use std::sync::Arc;

use lattice_protocol::position::{Position, Range as NativeRange};
use lattice_syntax::SyntaxSnapshot;
use tree_sitter::{Node, Point, Tree};

/// Backing for the `tree-snapshot` WIT resource: a point-in-time parse tree.
pub struct TreeSnapshotResource {
    snapshot: Arc<SyntaxSnapshot>,
}

/// Backing for a `node` WIT resource: a path of `Node::child` indices from the
/// tree root (empty = root), re-resolved against `snapshot` on each call.
pub struct NodeResource {
    snapshot: Arc<SyntaxSnapshot>,
    path: Vec<u32>,
}

fn point_of(pos: Position) -> Point {
    Point {
        row: pos.line as usize,
        column: pos.byte as usize,
    }
}

fn position_of(p: Point) -> Position {
    Position {
        line: p.row as u32,
        byte: p.column as u32,
    }
}

/// `a <= b` in (row, column) order — avoids relying on `Point: Ord`.
fn point_le(a: Point, b: Point) -> bool {
    (a.row, a.column) <= (b.row, b.column)
}

/// Walk `path` from the tree root, returning the node it names (or `None` if any
/// index is stale — cannot happen for an immutable snapshot, but never panics).
fn resolve<'t>(tree: &'t Tree, path: &[u32]) -> Option<Node<'t>> {
    let mut node = tree.root_node();
    for &i in path {
        node = node.child(i as usize)?;
    }
    Some(node)
}

/// Descend from the root into the child whose span contains `point`, recording
/// each `child` index, until no child contains it — the smallest node spanning
/// `point`, plus its path. `None` when `point` is outside the root.
///
/// Uses a `TreeCursor`: `goto_first_child_for_point` binary-searches the child
/// list (O(log fanout) per level), so a file with thousands of top-level items
/// costs a handful of comparisons, NOT a linear sibling scan — the bound the
/// sync-path perf claim rests on (§6). The primitive returns the first child
/// *ending past* `point` (which may start after it — a gap between siblings), so
/// the landed child is verified to actually contain `point` before descending.
fn descend_to_point(tree: &Tree, point: Point) -> Option<Vec<u32>> {
    let root = tree.root_node();
    if !(point_le(root.start_position(), point) && point_le(point, root.end_position())) {
        return None;
    }
    let mut cursor = tree.walk();
    let mut path = Vec::new();
    while let Some(idx) = cursor.goto_first_child_for_point(point) {
        let child = cursor.node();
        // Gap between siblings: the child starts past `point` → not a descent.
        if !(point_le(child.start_position(), point) && point_le(point, child.end_position())) {
            break;
        }
        path.push(idx as u32);
    }
    Some(path)
}

impl TreeSnapshotResource {
    /// Wrap an immutable syntax snapshot as a `tree-snapshot` backing.
    pub fn new(snapshot: Arc<SyntaxSnapshot>) -> Self {
        Self { snapshot }
    }

    /// Whether the snapshot carries a parse tree. The trampoline mints a
    /// resource only when this holds, so every resource method resolves.
    pub fn has_tree(&self) -> bool {
        self.snapshot.tree().is_some()
    }

    /// A fresh `NodeResource` at `path` anchored to this snapshot.
    fn node_at_path(&self, path: Vec<u32>) -> NodeResource {
        NodeResource {
            snapshot: Arc::clone(&self.snapshot),
            path,
        }
    }

    /// The tree root.
    pub fn root(&self) -> NodeResource {
        self.node_at_path(Vec::new())
    }

    /// The grammar id (e.g. `"rust"`).
    pub fn language(&self) -> String {
        self.snapshot.lang().name().to_string()
    }

    /// The smallest NAMED node spanning `pos` — descend to the smallest node,
    /// then walk up to the nearest named ancestor (the root is always named).
    /// `None` when there's no tree / `pos` is out of range.
    pub fn node_at(&self, pos: Position) -> Option<NodeResource> {
        let tree = self.snapshot.tree()?;
        let mut path = descend_to_point(tree, point_of(pos))?;
        while !path.is_empty() {
            let node = resolve(tree, &path)?;
            if node.is_named() {
                break;
            }
            path.pop();
        }
        Some(self.node_at_path(path))
    }

    /// The nearest ancestor of `pos` (inclusive) whose `kind` is in `kinds` — the
    /// auto-pair scope query (the native `scope_toward` precedent). `kinds` empty
    /// → the nearest named ancestor (i.e. `node_at`). `None` when no ancestor
    /// matches / no tree.
    pub fn enclosing(&self, pos: Position, kinds: &[String]) -> Option<NodeResource> {
        let tree = self.snapshot.tree()?;
        let mut path = self.node_at(pos)?.path;
        loop {
            let node = resolve(tree, &path)?;
            let matched = if kinds.is_empty() {
                node.is_named()
            } else {
                kinds.iter().any(|k| k == node.kind())
            };
            if matched {
                return Some(self.node_at_path(path));
            }
            if path.is_empty() {
                return None;
            }
            path.pop();
        }
    }
}

impl NodeResource {
    fn tree(&self) -> Option<&Tree> {
        self.snapshot.tree()
    }

    fn with_path(&self, path: Vec<u32>) -> NodeResource {
        NodeResource {
            snapshot: Arc::clone(&self.snapshot),
            path,
        }
    }

    /// The node's grammar kind (empty string if the path can't resolve — never
    /// panics; an immutable snapshot always resolves).
    pub fn kind(&self) -> String {
        self.tree()
            .and_then(|t| resolve(t, &self.path))
            .map(|n| n.kind().to_string())
            .unwrap_or_default()
    }

    pub fn is_named(&self) -> bool {
        self.tree()
            .and_then(|t| resolve(t, &self.path))
            .map(|n| n.is_named())
            .unwrap_or(false)
    }

    pub fn is_error(&self) -> bool {
        self.tree()
            .and_then(|t| resolve(t, &self.path))
            .map(|n| n.is_error())
            .unwrap_or(false)
    }

    /// The node's `[start, end)` span as byte-columns per line (matching the
    /// native structural objects' `ProtoRange`). A zero range if unresolved.
    pub fn byte_range(&self) -> NativeRange {
        let z = Position { line: 0, byte: 0 };
        self.tree()
            .and_then(|t| resolve(t, &self.path))
            .map(|n| NativeRange {
                start: position_of(n.start_position()),
                end: position_of(n.end_position()),
            })
            .unwrap_or(NativeRange { start: z, end: z })
    }

    /// The parent node, or `None` at the root.
    pub fn parent(&self) -> Option<NodeResource> {
        if self.path.is_empty() {
            return None;
        }
        let mut path = self.path.clone();
        path.pop();
        Some(self.with_path(path))
    }

    pub fn named_child_count(&self) -> u32 {
        self.tree()
            .and_then(|t| resolve(t, &self.path))
            .map(|n| n.named_child_count() as u32)
            .unwrap_or(0)
    }

    /// The `index`-th NAMED child, mapped to its `child` (all-children) index so
    /// the path stays in one indexing scheme.
    pub fn named_child(&self, index: u32) -> Option<NodeResource> {
        let tree = self.tree()?;
        let node = resolve(tree, &self.path)?;
        let mut seen = 0u32;
        for i in 0..node.child_count() {
            let child = node.child(i)?;
            if child.is_named() {
                if seen == index {
                    let mut path = self.path.clone();
                    path.push(i as u32);
                    return Some(self.with_path(path));
                }
                seen += 1;
            }
        }
        None
    }

    /// The child under grammar field `name` (e.g. `"body"`), or `None`.
    pub fn child_by_field(&self, name: &str) -> Option<NodeResource> {
        let tree = self.tree()?;
        let node = resolve(tree, &self.path)?;
        for i in 0..node.child_count() {
            if node.field_name_for_child(i as u32) == Some(name) {
                let mut path = self.path.clone();
                path.push(i as u32);
                return Some(self.with_path(path));
            }
        }
        None
    }

    pub fn next_named_sibling(&self) -> Option<NodeResource> {
        self.named_sibling(true)
    }

    pub fn prev_named_sibling(&self) -> Option<NodeResource> {
        self.named_sibling(false)
    }

    /// Scan the parent's children after (`forward`) or before this node for the
    /// nearest named sibling. The root has no siblings.
    fn named_sibling(&self, forward: bool) -> Option<NodeResource> {
        let (&last, parent_path) = self.path.split_last()?;
        let tree = self.tree()?;
        let parent = resolve(tree, parent_path)?;
        let cur = last as usize;
        let candidate = if forward {
            (cur + 1..parent.child_count()).find(|&i| {
                parent.child(i).map(|c| c.is_named()).unwrap_or(false)
            })
        } else {
            (0..cur).rev().find(|&i| {
                parent.child(i).map(|c| c.is_named()).unwrap_or(false)
            })
        };
        candidate.map(|i| {
            let mut path = parent_path.to_vec();
            path.push(i as u32);
            self.with_path(path)
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use lattice_syntax::{Lang, Syntax};

    fn rust_snapshot(src: &str) -> Arc<SyntaxSnapshot> {
        let mut syntax = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        syntax.parse(src);
        Arc::new(syntax.snapshot_owned())
    }

    fn pos(line: u32, byte: u32) -> Position {
        Position { line, byte }
    }

    #[test]
    fn root_is_the_source_file_and_language_is_rust() {
        let snap = rust_snapshot("fn main() {}\n");
        let ts = TreeSnapshotResource::new(snap);
        assert!(ts.has_tree());
        assert_eq!(ts.language(), "rust");
        assert_eq!(ts.root().kind(), "source_file");
        assert!(ts.root().is_named());
        assert!(ts.root().parent().is_none());
    }

    #[test]
    fn node_at_resolves_the_smallest_named_node() {
        // `fn main() { let x = 1; }` — cursor on `x`.
        let src = "fn main() { let x = 1; }\n";
        let ts = TreeSnapshotResource::new(rust_snapshot(src));
        let x_col = src.find('x').unwrap() as u32;
        let node = ts.node_at(pos(0, x_col)).unwrap();
        // The identifier under the cursor.
        assert_eq!(node.kind(), "identifier");
        assert!(node.is_named());
        let r = node.byte_range();
        assert_eq!(r.start, pos(0, x_col));
        assert_eq!(r.end, pos(0, x_col + 1));
    }

    #[test]
    fn enclosing_finds_the_named_scope_by_kind() {
        // Cursor inside the block; `enclosing` for `block` returns the `{ … }`.
        let src = "fn main() { let x = 1; }\n";
        let ts = TreeSnapshotResource::new(rust_snapshot(src));
        let x_col = src.find('x').unwrap() as u32;
        let block = ts
            .enclosing(pos(0, x_col), &["block".to_string()])
            .unwrap();
        assert_eq!(block.kind(), "block");
        let r = block.byte_range();
        // The block spans the braces.
        assert_eq!(r.start, pos(0, src.find('{').unwrap() as u32));
        assert_eq!(r.end, pos(0, (src.rfind('}').unwrap() + 1) as u32));
    }

    #[test]
    fn enclosing_with_no_matching_kind_is_none() {
        let src = "fn main() {}\n";
        let ts = TreeSnapshotResource::new(rust_snapshot(src));
        assert!(ts
            .enclosing(pos(0, 3), &["nonexistent_kind".to_string()])
            .is_none());
    }

    #[test]
    fn enclosing_empty_kinds_is_the_nearest_named() {
        let src = "fn main() { let x = 1; }\n";
        let ts = TreeSnapshotResource::new(rust_snapshot(src));
        let x_col = src.find('x').unwrap() as u32;
        let node = ts.enclosing(pos(0, x_col), &[]).unwrap();
        assert_eq!(node.kind(), "identifier");
    }

    #[test]
    fn named_child_navigation_and_count() {
        // The source_file's first named child is the function_item.
        let ts = TreeSnapshotResource::new(rust_snapshot("fn main() {}\n"));
        let root = ts.root();
        assert_eq!(root.named_child_count(), 1);
        let func = root.named_child(0).unwrap();
        assert_eq!(func.kind(), "function_item");
        assert!(root.named_child(1).is_none());
        // The function's parent is the root.
        assert_eq!(func.parent().unwrap().kind(), "source_file");
    }

    #[test]
    fn child_by_field_resolves_grammar_fields() {
        // `function_item` has a `name` field (the identifier `main`).
        let ts = TreeSnapshotResource::new(rust_snapshot("fn main() {}\n"));
        let func = ts.root().named_child(0).unwrap();
        let name = func.child_by_field("name").unwrap();
        assert_eq!(name.kind(), "identifier");
        assert_eq!(name.byte_range().start, pos(0, 3));
        assert!(func.child_by_field("no_such_field").is_none());
    }

    #[test]
    fn named_siblings_walk_in_both_directions() {
        // Two statements in a block: `let a = 1;` and `let b = 2;`.
        let src = "fn m() { let a = 1; let b = 2; }\n";
        let ts = TreeSnapshotResource::new(rust_snapshot(src));
        let a_col = src.find('a').unwrap() as u32;
        // The `let_declaration` enclosing `a`.
        let first = ts
            .enclosing(pos(0, a_col), &["let_declaration".to_string()])
            .unwrap();
        let second = first.next_named_sibling().unwrap();
        assert_eq!(second.kind(), "let_declaration");
        // `b` is in the second declaration.
        let b_col = src.find('b').unwrap() as u32;
        assert!(second.byte_range().start.byte <= b_col);
        // Walk back.
        let back = second.prev_named_sibling().unwrap();
        assert_eq!(back.byte_range().start, first.byte_range().start);
        assert!(first.prev_named_sibling().is_none());
    }

    #[test]
    fn no_tree_snapshot_reports_absent() {
        // A snapshot taken before the first parse has no tree — the trampoline
        // would pass `none` (parse pending; `Lang::Plain` has no grammar at all).
        let snap = Arc::new(Syntax::for_language(Lang::Rust).unwrap().unwrap().snapshot_owned());
        let ts = TreeSnapshotResource::new(snap);
        assert!(!ts.has_tree());
        assert!(ts.node_at(pos(0, 0)).is_none());
    }
}
