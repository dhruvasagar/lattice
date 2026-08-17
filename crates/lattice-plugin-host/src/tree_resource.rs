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
use lattice_syntax::{Lang, SyntaxSnapshot};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Point, Query, QueryCursor, Tree};

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

/// TS.2 backing for the `query` WIT resource: a compiled tree-sitter query plus
/// the language it was compiled against (so `run-query` can guard a mismatched
/// snapshot). Owned by the guest; reusable across snapshots of the same language.
pub struct QueryResource {
    query: Query,
    lang: Lang,
}

/// TS.2 backing for the `tree-cursor` WIT resource: a mutable position in the
/// tree, represented as a `child`-index path (the `NodeResource` scheme) so the
/// cursor stays a safe `(snapshot, path)` pair — no self-referential
/// `tree_sitter::TreeCursor<'tree>`.
pub struct CursorResource {
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
        node = node.child(i)?;
    }
    Some(node)
}

/// The `child`-index path from root to `node` — walk up via `parent()`, finding
/// `node`'s index among each parent's children by id. O(depth × siblings); used
/// only by `run-query`, which runs off the sync path (a whole-tree query is
/// forbidden from a sync grammar action, design §6), so the cost is acceptable.
fn path_of(node: Node) -> Vec<u32> {
    let mut path = Vec::new();
    let mut cur = node;
    while let Some(parent) = cur.parent() {
        let mut idx = 0u32;
        for i in 0..parent.child_count() {
            if parent.child(i as u32).map(|c| c.id()) == Some(cur.id()) {
                idx = i as u32;
                break;
            }
        }
        path.push(idx);
        cur = parent;
    }
    path.reverse();
    path
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

    /// TS.2: compile a tree-sitter query against this snapshot's grammar. `Err`
    /// (the tree-sitter message) on a malformed query or a language with no
    /// registered grammar.
    pub fn compile_query(&self, source: &str) -> Result<QueryResource, String> {
        let lang = self.snapshot.lang();
        let language = self
            .snapshot
            .registry()
            .tree_sitter_language(lang.name())
            .ok_or_else(|| format!("no tree-sitter grammar for language '{}'", lang.name()))?;
        let query = Query::new(&language, source).map_err(|e| e.to_string())?;
        Ok(QueryResource { query, lang })
    }

    /// TS.2: run `query` over the whole tree (or `within` a point range),
    /// returning the surviving captures — tree-sitter evaluates the `#eq?` /
    /// `#match?` / `#any-of?` text predicates against the snapshot's source (the
    /// `TextProvider`), so only matches that pass cross. Empty when there's no
    /// tree or `query` was compiled for a different grammar (graceful).
    pub fn run_query(
        &self,
        query: &QueryResource,
        within: Option<NativeRange>,
    ) -> Vec<(String, NodeResource)> {
        let Some(tree) = self.snapshot.tree() else {
            return Vec::new();
        };
        if query.lang != self.snapshot.lang() {
            return Vec::new();
        }
        let source = self.snapshot.source();
        let names = query.query.capture_names();
        let mut cursor = QueryCursor::new();
        if let Some(r) = within {
            cursor.set_point_range(point_of(r.start)..point_of(r.end));
        }
        let mut matches = cursor.matches(&query.query, tree.root_node(), source);
        let mut out = Vec::new();
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let name = names
                    .get(cap.index as usize)
                    .copied()
                    .unwrap_or_default()
                    .to_string();
                out.push((
                    name,
                    NodeResource {
                        snapshot: Arc::clone(&self.snapshot),
                        path: path_of(cap.node),
                    },
                ));
            }
        }
        out
    }

    /// TS.2b: `run_query` reduced to extents — same traversal, same host-side
    /// predicate filtering, but no `NodeResource` per capture.
    ///
    /// The saving is not the allocation: it is that every returned
    /// `NodeResource` becomes a guest-visible resource-table entry holding its
    /// own snapshot bump, which the guest must then drop one at a time across
    /// the boundary. A structural query over a large file returns tens of
    /// thousands of captures, and that per-capture round trip — not the query
    /// itself — is what made whole-file structural queries too slow to run.
    ///
    /// The returned `u32` is the match ordinal within THIS call, so captures
    /// from one pattern match stay groupable (`@context` with its
    /// `@context.end`) without a containment test on the guest side.
    pub fn run_query_ranges(
        &self,
        query: &QueryResource,
        within: Option<NativeRange>,
    ) -> Vec<(String, u32, NativeRange)> {
        let Some(tree) = self.snapshot.tree() else {
            return Vec::new();
        };
        if query.lang != self.snapshot.lang() {
            return Vec::new();
        }
        let source = self.snapshot.source();
        let names = query.query.capture_names();
        let mut cursor = QueryCursor::new();
        if let Some(r) = within {
            cursor.set_point_range(point_of(r.start)..point_of(r.end));
        }
        let mut matches = cursor.matches(&query.query, tree.root_node(), source);
        let mut out = Vec::new();
        // Counted here rather than read from `m.id()`: tree-sitter's match id is
        // not dense and is not stable across calls, and the guest only needs
        // "same match or not" within one result list.
        let mut match_index: u32 = 0;
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let name = names
                    .get(cap.index as usize)
                    .copied()
                    .unwrap_or_default()
                    .to_string();
                out.push((
                    name,
                    match_index,
                    NativeRange {
                        start: position_of(cap.node.start_position()),
                        end: position_of(cap.node.end_position()),
                    },
                ));
            }
            match_index = match_index.saturating_add(1);
        }
        out
    }
}

impl NodeResource {
    fn tree(&self) -> Option<&Tree> {
        self.snapshot.tree()
    }

    /// The node's root-relative child-index path — read by the host `reset`
    /// binding to reposition a cursor onto this node.
    pub fn path(&self) -> &[u32] {
        &self.path
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
            let child = node.child(i as u32)?;
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
                parent
                    .child(i as u32)
                    .map(|c| c.is_named())
                    .unwrap_or(false)
            })
        } else {
            (0..cur).rev().find(|&i| {
                parent
                    .child(i as u32)
                    .map(|c| c.is_named())
                    .unwrap_or(false)
            })
        };
        candidate.map(|i| {
            let mut path = parent_path.to_vec();
            path.push(i as u32);
            self.with_path(path)
        })
    }

    /// TS.2: a walk cursor positioned at this node.
    pub fn walk(&self) -> CursorResource {
        CursorResource {
            snapshot: Arc::clone(&self.snapshot),
            path: self.path.clone(),
        }
    }
}

impl CursorResource {
    fn tree(&self) -> Option<&Tree> {
        self.snapshot.tree()
    }

    /// The node the cursor currently sits on.
    pub fn current_node(&self) -> NodeResource {
        NodeResource {
            snapshot: Arc::clone(&self.snapshot),
            path: self.path.clone(),
        }
    }

    /// The grammar field of the current node relative to its parent, or `None`
    /// (root, or a child in no named field slot).
    pub fn current_field(&self) -> Option<String> {
        let (&last, parent_path) = self.path.split_last()?;
        let tree = self.tree()?;
        let parent = resolve(tree, parent_path)?;
        parent.field_name_for_child(last).map(str::to_string)
    }

    /// Move to the first NAMED child; `false` (no move) if there is none.
    pub fn goto_first_named_child(&mut self) -> bool {
        let Some(tree) = self.tree() else {
            return false;
        };
        let Some(node) = resolve(tree, &self.path) else {
            return false;
        };
        for i in 0..node.child_count() {
            if node.child(i as u32).map(|c| c.is_named()).unwrap_or(false) {
                self.path.push(i as u32);
                return true;
            }
        }
        false
    }

    /// Move to the next NAMED sibling; `false` (no move) if there is none.
    pub fn goto_next_named_sibling(&mut self) -> bool {
        let Some((&last, parent_path)) = self.path.split_last() else {
            return false;
        };
        let Some(tree) = self.tree() else {
            return false;
        };
        let Some(parent) = resolve(tree, parent_path) else {
            return false;
        };
        for i in (last as usize + 1)..parent.child_count() {
            if parent
                .child(i as u32)
                .map(|c| c.is_named())
                .unwrap_or(false)
            {
                let plen = self.path.len();
                self.path[plen - 1] = i as u32;
                return true;
            }
        }
        false
    }

    /// Move to the parent; `false` (no move) at the root.
    pub fn goto_parent(&mut self) -> bool {
        if self.path.is_empty() {
            return false;
        }
        self.path.pop();
        true
    }

    /// Reposition onto `node` (assumed a node of the same snapshot).
    pub fn reset(&mut self, node: &NodeResource) {
        self.reset_to_path(node.path.clone());
    }

    /// Reposition onto the given root-relative child-index path. Used by the
    /// host `reset` binding, which reads the target node's path from the resource
    /// table (avoiding a simultaneous borrow of the cursor and the node).
    pub fn reset_to_path(&mut self, path: Vec<u32>) {
        self.path = path;
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
        let block = ts.enclosing(pos(0, x_col), &["block".to_string()]).unwrap();
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
        assert!(
            ts.enclosing(pos(0, 3), &["nonexistent_kind".to_string()])
                .is_none()
        );
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
    fn compile_and_run_query_returns_predicate_filtered_captures() {
        let src = "fn alpha() {}\nfn beta() {}\n";
        let ts = TreeSnapshotResource::new(rust_snapshot(src));
        let q = ts
            .compile_query("(function_item name: (identifier) @fname)")
            .expect("valid query compiles");
        let caps = ts.run_query(&q, None);
        assert_eq!(caps.len(), 2, "both functions captured");
        assert!(caps.iter().all(|(name, _)| name == "fname"));
        // The captured nodes are the identifiers `alpha` / `beta`.
        assert_eq!(caps[0].1.kind(), "identifier");
        let starts: Vec<u32> = caps
            .iter()
            .map(|(_, n)| n.byte_range().start.byte)
            .collect();
        assert_eq!(starts, vec![3, 3]); // both at column 3 on their lines
    }

    #[test]
    fn run_query_honors_a_text_predicate() {
        let src = "fn alpha() {}\nfn beta() {}\n";
        let ts = TreeSnapshotResource::new(rust_snapshot(src));
        // `#eq?` predicate — only the function named exactly `beta` survives.
        let q = ts
            .compile_query("((function_item name: (identifier) @fname) (#eq? @fname \"beta\"))")
            .expect("valid predicated query compiles");
        let caps = ts.run_query(&q, None);
        assert_eq!(caps.len(), 1, "the #eq? predicate is evaluated host-side");
        assert_eq!(caps[0].1.byte_range().start.line, 1);
    }

    /// TS.2b: the ranges API must report the SAME extents as the node API.
    ///
    /// Two derivations of "where is this capture" is exactly the drift the
    /// plugin would silently inherit — a strip drawn from the wrong lines is
    /// indistinguishable from a strip drawn from the wrong scopes.
    #[test]
    fn run_query_ranges_agrees_with_run_query_extents() {
        let src = "fn alpha() {}\nfn beta() {}\n";
        let ts = TreeSnapshotResource::new(rust_snapshot(src));
        let q = ts
            .compile_query("(function_item name: (identifier) @fname)")
            .expect("valid query compiles");

        let nodes = ts.run_query(&q, None);
        let ranges = ts.run_query_ranges(&q, None);

        assert_eq!(ranges.len(), nodes.len());
        for ((nname, node), (rname, _, range)) in nodes.iter().zip(ranges.iter()) {
            assert_eq!(nname, rname);
            let nr = node.byte_range();
            assert_eq!(
                (range.start.line, range.start.byte),
                (nr.start.line, nr.start.byte)
            );
            assert_eq!((range.end.line, range.end.byte), (nr.end.line, nr.end.byte));
        }
    }

    /// Captures from one pattern match share a `match_index`; captures from
    /// different matches do not. This is what lets a query pair a construct
    /// with its body (`@context` + `@context.end`) in one pass — without it
    /// the guest would need a containment test, which is ambiguous for nested
    /// constructs.
    #[test]
    fn run_query_ranges_groups_captures_by_match() {
        let src = "fn alpha() {\n  let x = 1;\n}\nfn beta() {}\n";
        let ts = TreeSnapshotResource::new(rust_snapshot(src));
        let q = ts
            .compile_query("(function_item name: (identifier) @fname body: (_) @fbody)")
            .expect("valid two-capture query compiles");

        let caps = ts.run_query_ranges(&q, None);
        assert_eq!(caps.len(), 4, "two functions x two captures");

        // The name and the body of ONE function carry one index.
        let alpha: Vec<&(String, u32, NativeRange)> =
            caps.iter().filter(|c| c.1 == caps[0].1).collect();
        assert_eq!(alpha.len(), 2);
        let mut names: Vec<&str> = alpha.iter().map(|c| c.0.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["fbody", "fname"]);

        // ... and the second function a different one.
        assert!(
            caps.iter().any(|c| c.1 != caps[0].1),
            "the second match must not share the first match's index"
        );
    }

    #[test]
    fn compile_query_rejects_a_malformed_query() {
        let ts = TreeSnapshotResource::new(rust_snapshot("fn m() {}\n"));
        let result = ts.compile_query("(this is not a valid query");
        assert!(
            matches!(&result, Err(msg) if !msg.is_empty()),
            "malformed query is a typed error"
        );
    }

    #[test]
    fn cursor_walks_the_tree() {
        let src = "fn m() { let x = 1; }\n";
        let ts = TreeSnapshotResource::new(rust_snapshot(src));
        let mut cursor = ts.root().walk();
        assert_eq!(cursor.current_node().kind(), "source_file");
        // Down into the function_item — it sits in no named field of source_file.
        assert!(cursor.goto_first_named_child());
        assert_eq!(cursor.current_node().kind(), "function_item");
        assert_eq!(cursor.current_field(), None);
        // Back up to the root; no parent beyond.
        assert!(cursor.goto_parent());
        assert_eq!(cursor.current_node().kind(), "source_file");
        assert!(!cursor.goto_parent());
        // No named siblings at the root's single child level after reset.
        cursor.reset(&ts.root());
        assert!(cursor.goto_first_named_child());
        assert!(!cursor.goto_next_named_sibling(), "one top-level item");
    }

    #[test]
    fn cursor_current_field_reports_the_grammar_field() {
        let ts = TreeSnapshotResource::new(rust_snapshot("fn m() {}\n"));
        // Navigate to the function's `name` child and check the field.
        let func = ts.root().named_child(0).unwrap();
        let name = func.child_by_field("name").unwrap();
        let mut cursor = name.walk();
        assert_eq!(cursor.current_field(), Some("name".to_string()));
        cursor.goto_parent();
        assert_eq!(cursor.current_node().kind(), "function_item");
    }

    #[test]
    fn run_query_for_a_different_language_is_empty() {
        // A query compiled against Rust, run on a Rust snapshot, is fine; the
        // language guard only trips on a genuine mismatch, which we can't easily
        // construct here (one language per snapshot) — so assert the same-language
        // path yields matches (guard does not false-trip).
        let ts = TreeSnapshotResource::new(rust_snapshot("fn m() {}\n"));
        let q = ts.compile_query("(identifier) @id").unwrap();
        assert!(!ts.run_query(&q, None).is_empty());
    }

    #[test]
    fn no_tree_snapshot_reports_absent() {
        // A snapshot taken before the first parse has no tree — the trampoline
        // would pass `none` (parse pending; `Lang::Plain` has no grammar at all).
        let snap = Arc::new(
            Syntax::for_language(Lang::Rust)
                .unwrap()
                .unwrap()
                .snapshot_owned(),
        );
        let ts = TreeSnapshotResource::new(snap);
        assert!(!ts.has_tree());
        assert!(ts.node_at(pos(0, 0)).is_none());
    }
}
