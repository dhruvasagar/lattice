<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `context`

**Direction:** guest implements this interface · **Capability:** none (pure data / dispatch) · **Worlds:** `context-plugin` (exports), `treesitter-context-plugin` (exports)

The structural-**context** producer API (treesitter-context.md, TC.2): the
scopes a pane pins above its text once their own header lines have scrolled
away — the `nvim-treesitter-context` / sticky-scroll idea.

**Scopes cross, not rows.** A `context-scope` is a pure function of the parse
tree, so the host caches the set per parse version and resolves "which of
these apply to THIS pane right now" itself, per pane, per frame
(`lattice_cells::context::resolve_context`, TC.1). Returning finished rows
instead would put a WASM call on the scroll path and give the host a cache
keyed on the cursor — one that thrashes by construction. Paramount #1.

**Producer, async, host-cached** — the `decorations` shape (PH7.9), for the
same reason: the host calls `context-scopes` OFF the render path on a trigger
(a completed reparse), caches the result, and every later read is native. The
guest never runs on a keystroke.

## Uses

- [`context-request`](types.md#record-context-request) from [`types`](types.md)
- [`context-scope`](types.md#record-context-scope) from [`types`](types.md)
- [`tree-snapshot`](tree-sitter.md#resource-tree-snapshot) from [`tree-sitter`](tree-sitter.md)

## Functions (1)

### `context-scopes`

```wit
context-scopes: func(req: context-request, tree: option<borrow<tree-snapshot>>) -> result<list<context-scope>, string>
```

Produce the structural context scopes for a buffer.

`tree` is the buffer's point-in-time parse snapshot, handed in the way
`grammar.apply-action` hands it (TS.1) rather than acquired by the guest:
call-scoped access keeps the `tree-sitter` capability meaning "the tree
you were given" instead of "any buffer's tree, any time". It is `none`
when the buffer has no parse (plain text, or a parse still pending), and
a guest with nothing to work from should return an empty list.

Async — a produce call suspends the guest and never the render path. The
guest is expected to run a whole-buffer `run-query` here, which is why
this must not be synchronous.

Graceful (§8): an `err` is logged and the buffer KEEPS its previously
cached scopes rather than being cleared. A failed refresh must not blank
the strip — a transient error would otherwise read as the feature
breaking. Same contract as `decorations.gutter-decorations`.

**Example — Produce sticky-context scopes from the tree, bounded by a size option** · [`plugins/treesitter-context/src/lib.rs`](../../../../plugins/treesitter-context/src/lib.rs)

```rust
fn context_scopes(
    req: ContextRequest,
    tree: Option<&TreeSnapshot>,
) -> Result<Vec<ContextScope>, String> {
    if req.line_count == 0 {
        return Ok(Vec::new());
    }
    // Bound the work BEFORE running the query. Returning empty (not `err`)
    // is deliberate: the host caches an empty set, which is the truth —
    // this file has no context — rather than keeping a stale set from
    // whatever was open before.
    let max_lines = get_option("max-file-lines")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(DEFAULT_MAX_FILE_LINES);
    if max_lines > 0 && req.line_count > max_lines {
        return Ok(Vec::new());
    }
    // No parse (plain text, or one still pending) is a normal state the
    // host caches as "no scopes" — never an error, which would make it keep
    // the previous buffer's structure.
    let Some(tree) = tree else {
        return Ok(Vec::new());
    };
    scopes_from_tree(tree)
}
```

