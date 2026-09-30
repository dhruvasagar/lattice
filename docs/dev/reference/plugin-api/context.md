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

