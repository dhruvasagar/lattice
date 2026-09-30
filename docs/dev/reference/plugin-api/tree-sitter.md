<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `tree-sitter`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `auto-pair-plugin` (imports), `comment-plugin` (imports), `context-plugin` (imports), `grammar-plugin` (imports), `project-plugin` (imports), `scanned-excerpt-source-plugin` (imports), `treesitter-context-plugin` (imports)

Structural queries for plugins (plugin-treesitter-seam.md). The host already
parses every buffer with tree-sitter (`lattice-syntax`) and publishes an
immutable `SyntaxSnapshot` per buffer; this seam **publishes that snapshot to
a plugin, read-only**, so a WASM plugin can navigate the parse tree exactly
as native structural code does. First consumer: `auto-pair`'s manual style
queries the enclosing lexical scope to bound its backward scan (design §7).

The tree NEVER crosses the boundary — walks execute host-side against the
snapshot's `tree_sitter::Tree`; only *results* (a node projection, a kind
string) cross. A plugin reads a POINT-IN-TIME snapshot: it acquires the
handle alongside the `document` handle from the same dispatch context (same
instant → tree + text versions agree, §7); an edit landing after swaps a
newer snapshot without disturbing the read (the `document`-handle
mutation-under-read discipline, applied to structure). Gated on the
`tree-sitter` editor-capability — no grant, no handle (design §5).

**TS.1 scope:** the snapshot + node core (enough for auto-pair's `enclosing`).
Queries (`compile-query` / `run-query` with host-side predicates) and the
`tree-cursor` walk land at TS.2; see the design fragment §3.3–§3.4 / §10.

## Uses

- [`position`](types.md#record-position) from [`types`](types.md)
- [`range`](types.md#record-range) from [`types`](types.md)

## Functions (1)

### `parse-file`

```wit
parse-file: func(path: string) -> option<tree-snapshot>
```

OT.2: parse a file that is **not an open buffer**, and hand back a
snapshot on the same terms as a buffer's.

Every other snapshot in this interface belongs to a buffer the editor
already parsed. A plugin acting on project files it never opened — org's
capture resolving a `file+headline` target, its refile picker listing
every headline in the project — had no way to get structure, so it
hand-parsed text and diverged from the grammar. That divergence is the
bug class OT.x exists to end, and this is the primitive that ends it for
off-buffer content.

Names no plugin and no language: the extension resolves through the same
registry a buffer's does (native languages first, then plugin-registered
ones), so a plugin gets a tree for `.org` for exactly the reason the
editor would.

**`none`, never a trap**, when any link in the chain is missing: no
`tree-sitter` capability, the path is outside the plugin's `fs:` grant,
the file is unreadable or not UTF-8, the extension maps to no language,
or the parse yields no tree. A caller that cannot tell these apart is
making one decision — "can I read structure here?" — and the answer is
no. `error-parser`'s rule: one bad file must not fail the walk.

**The host reads and parses; only the path crosses.** The tree never
crosses the boundary (§7) and neither does the file's text, so this is
strictly cheaper than `read-file` plus a guest-side scan.

**Cost, stated rather than buried.** Reachable from the SYNC grammar
linker, where the sibling comment says reads are "no I/O, no parse — the
tree is already there". This one is both, so it belongs on explicit user
actions (capture's chord) and NOT in a motion or text object, which fire
per keystroke. `read-file` set the I/O precedent here; this adds the
parse on top of it.

**Example — Parse a file that is not open in a buffer and inspect its root node** · [`crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs)

```rust
let path = match &ctx.args {
    Args::String(s) => s.clone(),
    other => return Err(format!("multiseam: parse-file wants a path, got {other:?}")),
};
let snapshot = tree_sitter::parse_file(&path)
    .ok_or_else(|| format!("multiseam: parse-file returned none for {path}"))?;
let root = snapshot.root();
Ok(vec![Effect::Echo(EchoPayload {
    level: EchoLevel::Info,
    text: format!("{}:{}", root.kind(), root.named_child_count()),
})])
```

## Resources

### resource `tree-snapshot`

A host-owned, point-in-time view of a buffer's parse tree — backed by an
`Arc<SyntaxSnapshot>` (an O(1) `ArcSwap` bump, no parse, no copy). An
`apply-action` receives it as `option<borrow<tree-snapshot>>` (absent
when the buffer has no parse: plain text / parse pending). Every `node`
it hands out is anchored to THIS snapshot.

#### `tree-snapshot.compile-query`

```wit
compile-query: func(source: string) -> result<query, string>
```

TS.2: compile a tree-sitter query (S-expression) against THIS
snapshot's grammar. `err` (with the tree-sitter message) on a
malformed query. The returned `query` is reusable across snapshots of
the same language — compile once, run many.

**Example — Compile a per-language query against the snapshot's grammar** · [`plugins/treesitter-context/src/lib.rs`](../../../../plugins/treesitter-context/src/lib.rs)

```rust
let Some(source) = query_for(&language) else {
    // No query for this grammar. Not an error — the strip simply has
    // nothing to show, and the host caches that as "no scopes".
    return Ok(Vec::new());
};
// Compiled per call rather than cached: the guest has no per-language
// cache slot that survives a call, and this runs once per REPARSE (not per
// keystroke, scroll, or frame), so the cost sits far off every hot path.
// A cache would be the right move only if the producer were re-driven more
// often, and the whole scopes-not-rows split exists to ensure it is not.
let query = tree.compile_query(source)?;
```

#### `tree-snapshot.enclosing`

```wit
enclosing: func(pos: position, kinds: list<string>) -> option<node>
```

The nearest ancestor of `pos` whose `kind` is in `kinds` (the
auto-pair scope query; the native `scope_toward` precedent). `kinds`
empty → the nearest named ancestor. `none` when there's no match / no
parse.

**Example — Bound a backward text scan by the enclosing block node, with a line-capped fallback when there is no tree** · [`plugins/auto-pair/src/lib.rs`](../../../../plugins/auto-pair/src/lib.rs)

```rust
/// The scope text from the enclosing lexical scope's start up to the caret (§7).
/// Uses the tree-sitter seam's `enclosing` to bound the scan; with no parse tree
/// (or no enclosing scope), degrades to a line-capped cursor-backward slice —
/// never a whole-buffer materialization.
fn scope_text_before_cursor(
    ctx: &ActionContext,
    doc: &Document,
    tree: Option<&TreeSnapshot>,
) -> String {
    let scan_start = tree
        .and_then(|t| t.enclosing(ctx.cursor, &scope_kinds()))
        .map(|node| node.byte_range().start)
        .unwrap_or_else(|| Position {
            line: ctx.cursor.line.saturating_sub(200),
            byte: 0,
        });
    doc.get_text_range(Range {
        start: scan_start,
        end: ctx.cursor,
    })
    .unwrap_or_default()
}
```

#### `tree-snapshot.language`

```wit
language: func() -> string
```

The grammar id (e.g. `"rust"`), so a plugin can pick the right query.

**Example — Report the tree's language with the enclosing block's kind and named-child count** · [`crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs)

```rust
let tree = tree.ok_or("multiseam: no tree snapshot")?;
let node = tree
    .enclosing(ctx.cursor, &["block".to_string()])
    .ok_or("multiseam: no enclosing block")?;
Ok(vec![Effect::Echo(EchoPayload {
    level: EchoLevel::Info,
    text: format!(
        "{}:{}:{}",
        tree.language(),
        node.kind(),
        node.named_child_count()
    ),
})])
```

#### `tree-snapshot.node-at`

```wit
node-at: func(pos: position) -> option<node>
```

The smallest NAMED node spanning `pos`
(`Tree::named-descendant-for-point-range`), or `none` when the buffer
is empty / `pos` is out of range.

#### `tree-snapshot.root`

```wit
root: func() -> node
```

The tree root.

#### `tree-snapshot.run-query`

```wit
run-query: func(q: borrow<query>, within: option<range>) -> list<capture>
```

TS.2: run `q` over the whole tree, or `within` a point range. Returns
the surviving captures — the `#eq?` / `#match?` / `#any-of?` predicates
are evaluated HOST-side (against the snapshot's source), so the guest
never re-filters. Empty when `q` was compiled for a different grammar
than this snapshot's (graceful — never a trap).

**Example — Compile a query, run it over the whole tree, and read each capture's name and node** · [`crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs)

```rust
let tree = tree.ok_or("multiseam: no tree snapshot")?;
let query = tree.compile_query("(function_item name: (identifier) @fname)")?;
let caps = tree.run_query(&query, None);
let first = caps
    .first()
    .map(|c| format!("{}:{}", c.name, c.node.kind()))
    .unwrap_or_default();
Ok(vec![Effect::Echo(EchoPayload {
    level: EchoLevel::Info,
    text: format!("{}:{}", caps.len(), first),
})])
```

#### `tree-snapshot.run-query-ranges`

```wit
run-query-ranges: func(q: borrow<query>, within: option<range>) -> list<capture-range>
```

TS.2b: the same query, returning RANGES instead of node handles.

`run-query` mints one `node` resource per capture. A resource is a
table entry with a host-side snapshot bump and a guest-side drop, so
a whole-file structural query pays that per capture — and a
structural query over a large file has tens of thousands of them.
That cost is what forced `treesitter-context`'s `max-file-lines`
guard, and it is pure overhead for the (common) plugin that only
ever reads a capture's extent.

Same predicates, same host-side filtering, same graceful-empty on a
grammar mismatch. `match-index` groups captures that came from ONE
pattern match, so a query can capture a construct and its body
(`@context` + `@context.end`) and the guest can pair them without a
second query or a containment test.

Use `run-query` when the capture must be NAVIGATED (parent, field,
sibling); use this when its extent is the answer.

**Example — Run a whole-file query as plain ranges and pair captures by match index** · [`plugins/treesitter-context/src/lib.rs`](../../../../plugins/treesitter-context/src/lib.rs)

```rust
// `run_query_ranges`, not `run_query`: this is a WHOLE-FILE structural
// query, and the node-returning form pays a resource handle per capture.
// See the module doc — that difference is the file-size ceiling.
let captures = tree.run_query_ranges(&query, None);
let mut scopes: Vec<ContextScope> = Vec::new();
// Captures arrive grouped by match (the host pushes each match's captures
// together and stamps them with one index), so one linear scan pairs each
// `@context` with its `@context.end` — no containment test, which would be
// ambiguous for a construct nested directly inside another.
let mut i = 0;
while i < captures.len() {
    let match_index = captures[i].match_index;
    let mut extent: Option<(u32, u32)> = None;
    let mut body_start: Option<u32> = None;
    while i < captures.len() && captures[i].match_index == match_index {
        let c = &captures[i];
        match c.name.as_str() {
            "context" => extent = Some((c.range.start.line, c.range.end.line)),
            "context.end" => body_start = Some(c.range.start.line),
            // A query may carry captures for its own predicates; anything
            // unrecognised is ignored rather than treated as a scope.
            _ => {}
        }
        i += 1;
    }
    if let Some(extent) = extent {
        scopes.push(scope_from(extent, body_start));
    }
}
// A scope spanning a single line can never be a context: its header cannot
// scroll away while the cursor is still inside it. Dropping them here keeps
// the host's cache (and the resolver's scan) free of entries that can never
// resolve to anything.
scopes.retain(|s| s.scope_end > s.scope_start);
Ok(scopes)
```

### resource `node`

An opaque, navigable handle into the snapshot's tree (design §3.2). Owned
by the guest and dropped when it goes out of scope; each holds its own
snapshot bump so it stays coherent for the call. Projection is cheap and
value-returning; navigation returns a FRESH `node` (or `none`). The tree
itself never crosses — a handle is a host-side `(snapshot, path)` pair.

#### `node.byte-range`

```wit
byte-range: func() -> range
```

The node's `[start, end)` span as byte-columns per line (matching the
native structural objects' `ProtoRange`, N.1.4c).

**Example — Answer a text object with a tree node's `byte-range`, erring when there is no tree** · [`crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs)

```rust
fn apply_text_object(
    c: u32,
    _ctx: TextObjectContext,
    _doc: &Document,
    tree: Option<&TreeSnapshot>,
) -> Result<Range, String> {
    match c {
        // OT.1: the structural peer — org's `ir` / `ar` resolve a subtree,
        // which IS a tree node rather than a star count.
        21 => {
            let tree = tree.ok_or_else(|| "multiseam: text object got no tree".to_string())?;
            Ok(tree.root().byte_range())
        }
        other => Err(format!("multiseam: unknown text-object callback {other}")),
    }
}
```

#### `node.child-by-field`

```wit
child-by-field: func(name: string) -> option<node>
```

The child under the grammar field `name` (e.g. `"body"`), or `none`.

#### `node.is-error`

```wit
is-error: func() -> bool
```

Whether the node is a tree-sitter ERROR node (a parse error).

#### `node.is-named`

```wit
is-named: func() -> bool
```

Whether the node is *named* (a grammar rule) vs an anonymous token.

#### `node.kind`

```wit
kind: func() -> string
```

The node's grammar kind (e.g. `"function_item"`).

**Example — Report the root node's kind when a scan is handed a parse tree beside the text** · [`crates/lattice-plugin-host/tests/fixtures/agenda-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/agenda-guest/src/lib.rs)

```rust
// OT.3: text is always here; the tree comes beside it when the file's
// extension resolves to a registered language. This fixture reports the
// ROOT KIND when it got a tree — something no text scan could produce —
// so the host test can tell the two apart.
if let Some(snapshot) = tree {
    let root = snapshot.root();
    return Ok(ScanResult {
        entries: vec![Entry {
            line: 0,
            end_line: 0,
            group: "tree".to_string(),
            label: format!("tree:{}:{}", root.kind(), root.named_child_count()),
            sort_key: 0,
            spans: Vec::new(),
            // The tree path says nothing about annotations; `none` here
            // keeps this fixture's two branches distinguishable.
            annotation: None,
            emphasis: false,
        }],
        clock,
    });
}
```

#### `node.named-child`

```wit
named-child: func(index: u32) -> option<node>
```

The `index`-th NAMED child (0-based), or `none` past the end.

**Example — Derive one context scope per named child of the root, spanning that child's lines** · [`crates/lattice-plugin-host/tests/fixtures/context-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/context-guest/src/lib.rs)

```rust
// Walk the tree for real. Each named child of the root becomes a scope
// spanning its own lines, with its first line as the header.
let root = tree.root();
let count = root.named_child_count();
let mut scopes = Vec::new();
for i in 0..count {
    let Some(child) = root.named_child(i) else {
        continue;
    };
    let r = child.byte_range();
    scopes.push(ContextScope {
        scope_start: r.start.line,
        scope_end: r.end.line,
        header_start: r.start.line,
        header_end: r.start.line,
    });
}
Ok(scopes)
```

#### `node.named-child-count`

```wit
named-child-count: func() -> u32
```

Count of NAMED children.

#### `node.next-named-sibling`

```wit
next-named-sibling: func() -> option<node>
```

The next NAMED sibling, or `none`.

#### `node.parent`

```wit
parent: func() -> option<node>
```

The parent node, or `none` at the root.

#### `node.prev-named-sibling`

```wit
prev-named-sibling: func() -> option<node>
```

The previous NAMED sibling, or `none`.

#### `node.walk`

```wit
walk: func() -> tree-cursor
```

TS.2: a stateful cursor positioned at this node, for structural walks
without per-step parent/child handle churn.

### resource `query`

TS.2: a compiled tree-sitter query — opaque, owned by the guest (dropped
when it leaves scope), reusable across snapshots of the same language.

### resource `tree-cursor`

TS.2: a stateful walk cursor over the snapshot's tree (design §3.4).
Anchored to one snapshot; `goto-*` move it and report whether they could.

#### `tree-cursor.current-field`

```wit
current-field: func() -> option<string>
```

The grammar field of the current node relative to its parent (e.g.
`"body"`), or `none` (root, or an unnamed field slot).

#### `tree-cursor.current-node`

```wit
current-node: func() -> node
```

The node the cursor currently sits on.

#### `tree-cursor.goto-first-named-child`

```wit
goto-first-named-child: func() -> bool
```

Move to the first NAMED child; `false` (and no move) if there is none.

**Example — Walk the tree with a cursor: descend to the first named child and read its kind** · [`crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs)

```rust
let tree = tree.ok_or("multiseam: no tree snapshot")?;
let cursor = tree.root().walk();
let moved = cursor.goto_first_named_child();
let kind = cursor.current_node().kind();
Ok(vec![Effect::Echo(EchoPayload {
    level: EchoLevel::Info,
    text: format!("{moved}:{kind}"),
})])
```

#### `tree-cursor.goto-next-named-sibling`

```wit
goto-next-named-sibling: func() -> bool
```

Move to the next NAMED sibling; `false` (and no move) if there is none.

#### `tree-cursor.goto-parent`

```wit
goto-parent: func() -> bool
```

Move to the parent; `false` (and no move) at the root.

#### `tree-cursor.reset`

```wit
reset: func(n: borrow<node>)
```

Reposition the cursor onto `n` (must be a node of the same snapshot).

## Types (2)

### record `capture`

```wit
record capture {
    name: string,
    node: node,
}
```

TS.2: one query match capture — the `@name` and the node it bound.

**Fields**

- `name`: `string`
- `node`: [`node`](#resource-node)

### record `capture-range`

```wit
record capture-range {
    name: string,
    match-index: u32,
    range: range,
}
```

TS.2b: a capture reduced to its extent — no resource, no drop.

`match-index` is the ordinal of the pattern match this capture belongs
to WITHIN this call's results (not a stable tree id): captures sharing
one value came from one match of one pattern.

**Fields**

- `name`: `string`
- `match-index`: `u32`
- `range`: [`range`](types.md#record-range)

