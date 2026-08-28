# Plugin tree-sitter seam — structural queries for plugins

> **Design fragment.** Contracts, data model, rationale, rejected alternatives,
> paramount-goal alignment. Slice sequencing is §11 (splits to a slice-plan file
> if it grows). Sibling fragments: [`plugin-host.md`](plugin-host.md) (the seam
> spine + capability model), [`treesitter-motions.md`](treesitter-motions.md) +
> [`tree-sitter-text-objects.md`](tree-sitter-text-objects.md) (the *native*
> structural surface this publishes to WASM),
> [`plugin-auto-pair.md`](plugin-auto-pair.md) (its first consumer — the
> enclosing-scope query that bounds the manual-pair scan).
>
> **Status: ✅ built (2026-07-20) — TS.1 + TS.2 + TS.3.** A **v1**
> foundational seam (promoted from a later refinement, 2026-07-19, Dhruva): a
> truly customizable, tree-sitter-driven editor needs plugins to query the parse
> tree early. This is the seam that makes paramount goal #3 real — structural
> motions / text objects / folds / refactors / rainbow-delimiters as plugins.
> TS.1 landed the snapshot + node core (`root` / `node-at` / `enclosing` +
> projection/navigation), the `tree-sitter` capability gate, and the sync-path
> `enclosing` query wired onto the grammar action context. TS.2 added the full
> query surface (`compile-query` / `run-query` with host-side `#eq?`/`#match?`/
> `#any-of?` predicate eval, §3.3) and the `tree-cursor` walk (§3.4). TS.3 landed
> the first structural consumer end to end — auto-pair's `manual` style (AP.3)
> bounds its `find_pair` scan to the `enclosing` scope through the seam. See the
> slice plan for what shipped.

## 1. Why

Paramount goal #3 names "future tree-sitter-driven variants" of motions / text
objects as first-class. lattice already parses **every** language with tree-sitter
(`lattice-syntax`); the structural surface — enclosing scope, node-at-cursor,
query captures — is implemented natively and drives the built-in structural
motions/text-objects. Without a plugin seam over it, that entire class of
extension is closed to plugins, and a plugin that reasons about *structure* (not
just bytes) has to re-implement a parser in WASM — absurd when the host holds a
live, incremental, per-language tree.

The seam **publishes the host's tree to plugins, read-only**, so a WASM plugin can
query and navigate it exactly as native structural code does. First consumer:
`auto-pair`'s manual style (§7 of that fragment) queries the enclosing lexical
scope to bound its backward scan — the difference between a whole-buffer scan that
stalls on large files and an O(scope) one. The class it unlocks is much larger:
structural motions (`]f` / `[c`), text objects (`iaf`, `if`), fold providers,
symbol outlines, rainbow-delimiters, structural refactors.

## 2. The model — a point-in-time tree snapshot (the hazard is already solved)

`lattice-syntax` runs reparses **off-thread** and publishes a `SyntaxSnapshot`
(owning the tree-sitter `Tree`, itself Arc-internal) via **`ArcSwap`**
(`handle.rs`). That is the whole concurrency story the seam needs:

> **A plugin reads a *point-in-time* tree snapshot.** It acquires a
> **tree-snapshot handle** (an `Arc<SyntaxSnapshot>` bump — O(1), no copy);
> every node / cursor / query it derives is anchored to *that* snapshot. The
> buffer editing underneath swaps a newer snapshot without disturbing the read —
> the exact mutation-under-read discipline the `document` handle already uses for
> rope text (plugin-host.md §4.2). A handle held across an edit stays **coherent
> with its own snapshot** (possibly stale by a reparse or two — the
> eventual-consistency the UX contract permits for non-edited structure), never
> torn.

Queries and walks execute **host-side** against the snapshot; only **results**
cross the WASM boundary (a capture list, a node projection). The tree never
crosses — the `document.slice` model, applied to structure.

## 3. The API surface (v1 — the fuller set)

Confirmed v1 scope: enough for auto-pair *and* the first structural-plugin wave,
including query predicates, field names, and a cursor walk.

### 3.1 The tree-snapshot handle

A plugin with the `tree-sitter` capability (§5) gets a `tree-snapshot` resource
off its seam context (the grammar action/motion context — the `document`-handle
extension, [`plugin-auto-pair.md`](plugin-auto-pair.md) §5.1 — carries the tree
snapshot alongside the document). From it:

- `root() -> node` — the tree root.
- `node-at(pos: position) -> option<node>` — the smallest named node spanning
  `pos` (`Tree::descendant_for_byte_range`).
- `enclosing(pos, kinds: list<string>) -> option<node>` — the nearest ancestor of
  `pos` whose `kind` is in `kinds` (the auto-pair scope query; the `scope_toward`
  native precedent). `kinds` empty → the nearest *named* ancestor.
- `language() -> string` — the grammar id (so a plugin can pick the right query).

### 3.2 The `node` resource (opaque handle + projection)

A `node` is a host-owned handle into the snapshot — navigable and lazy (the tree
never crosses). Projection (cheap, value-returning):

- `kind() -> string`, `is-named() -> bool`, `byte-range() -> range`,
  `is-error() -> bool`.

Navigation (each returns a fresh `node` handle, or `none`):

- `parent()`, `named-child(index)`, `named-child-count()`,
  `child-by-field(name: string)`, `next-named-sibling()`, `prev-named-sibling()`,
  `walk() -> tree-cursor` (§3.4).

### 3.3 Queries (with predicates)

- `compile-query(source: string) -> result<query, string>` — compile a
  tree-sitter query (S-expression) against the snapshot's language; `err` on a
  malformed / grammar-mismatched query. Compiled once, reusable across snapshots
  of the same language.
- `run-query(query, within: option<range>) -> list<capture>` where
  `capture = { name: string, node: node }` — run over the whole tree or a byte
  range. **Predicates** (`#eq?`, `#match?`, `#any-of?`) are evaluated **host-side**
  so only surviving captures cross (the plugin never re-filters).
- `run-query-ranges(query, within: option<range>) -> list<capture-range>` where
  `capture-range = { name: string, match-index: u32, range: range }` — the same
  query, returning EXTENTS instead of node handles.

  Every `node` a query returns is a resource: a host table entry holding its own
  snapshot bump, which the guest drops one at a time across the boundary. For a
  whole-file structural query that is tens of thousands of round trips, and it
  is what makes the node form superlinear — `treesitter-context` measured 287 ms
  at 5k lines and a TRAP past 20k, versus 9 ms and 28 ms for the same query in
  ranges form. `match-index` groups captures from one pattern match so a query
  can capture a construct and its body and the guest can pair them without a
  second query or a containment test.

  **Choose by what you need:** `run-query` when a capture must be NAVIGATED
  (parent, field, sibling); `run-query-ranges` when its extent is the answer. A
  whole-file query almost always wants the latter.

### 3.4 The `tree-cursor` resource (walk)

For structural walks without repeated parent/child handle churn:

- `goto-first-named-child() -> bool`, `goto-next-named-sibling() -> bool`,
  `goto-parent() -> bool`, `current-node() -> node`,
  `current-field() -> option<string>`, `reset(node)`.

## 4. Node / cursor lifetime

Nodes, cursors, and queries are resource handles **tied to the tree-snapshot
handle's lifetime**. Dropping the snapshot invalidates its derived handles (the
resource-table drop discipline; a use-after-drop is a host-side `err`, never a
UB). A plugin that wants structure across an edit re-acquires a fresh snapshot —
cheap (an `Arc` bump) — rather than holding a stale one.

## 5. Capability

Gated on the **`tree-sitter` editor-capability** (already a manifest
`editor_capabilities` value — `capability.rs`, alongside `lsp` / `folds` /
`diagnostics`). Read-only: the seam exposes *queries*, never tree mutation (the
host owns parsing). A plugin without the grant gets no `tree-snapshot` handle.
Bundled plugins are pre-granted; user-installed plugins declare it and the tier
gates it.

**Registering a grammar does not imply the grant.** A plugin can contribute a
language through the `language` seam and still have no `tree-sitter` capability,
in which case it registers a grammar the host parses with and it cannot read the
result. Nothing fails; every handle is simply `none` and the plugin falls back to
whatever it does without structure. Org shipped in exactly that state until OT.4
— an org grammar, an org `folds.scm`, an org `highlights.scm`, and a guest that
had never once seen a tree. The gate is doing its job; the manifest has to ask.

### 5.1 How the handle reaches a seam — two dispatch paths, not one

`GrammarEnv::syntax` is populated on **two** separate routes, and a consumer
added to one of them does not automatically get the other:

| Route | Built in | Consumers |
|---|---|---|
| The **Action gate** | `Editor::dispatch_invocation` | grammar actions (`apply-action`) |
| The **document actor** | `Editor::dispatch_blocking` → `DispatchEnv` → `actor.rs` | motions, operators, text objects |

TS.1 wired only the first, because grammar actions were then the only consumer,
and the actor arm carried a hard `syntax: None` with a comment saying so. OT.1
gave motions and text objects the same handle in the WIT, the trampoline and the
guest — but not in `DispatchEnv`, which had no field to carry it. The seam read
as wired end to end and delivered `none` on every real keystroke.

Two things made that survivable for a whole slice, and both are worth naming
because they generalise past this seam:

* **The tests built the env by hand.** `tree_seam.rs` constructs
  `GrammarEnv { syntax: Some(&snapshot), .. }` directly, which proves the guest
  and the trampoline and says nothing about who fills the field in production.
  A seam test that supplies its own context tests everything except the gate.
* **`scope_resolver` looks like it already covers this.** It does not: it is
  deliberately erased to one question for the native structural objects, and the
  concrete `SyntaxSnapshot` cannot be recovered from it to mint a resource. The
  two fields coexist for that reason.

A composed multibuffer view is the one place the answer is legitimately `none`:
an excerpt list spans several files, so there is no single snapshot to hand over
(the composed↔source mapping is what `ComposedScopeResolver` exists to apply).

## 6. Performance

- **Host-side execution.** Queries + walks run against the host's `Tree`; only
  results cross. A `run-query` returns a capture list (name + node handle), not
  the tree. Predicate filtering is host-side.
- **Resources are the cost, not the traversal.** The dominant cost of a large
  query is one resource handle per capture — table entry, snapshot bump, guest
  drop — not tree-sitter's work. `run-query-ranges` exists for exactly this and
  turned a superlinear-then-trapping call into a linear ~1.4 us/line one. When a
  seam is slow at scale, count the resources crossing before optimising the
  algorithm.
- **O(1) snapshot acquire.** The handle is an `Arc<SyntaxSnapshot>` bump; no parse,
  no copy.
- **Hot-path discipline.** Most structural queries are off-keystroke (motions,
  folds, outlines run on demand / off-thread). The one on-keystroke use —
  auto-pair's `enclosing(pos, kinds)` on the **sync** grammar action — is a single
  bounded ancestor walk over an already-parsed tree (`descendant_for_byte_range` +
  parent chain), far inside the Reflex budget; it does **no** parsing (the tree is
  already there). A plugin must not run a *whole-tree* `run-query` from a sync
  grammar action; that belongs off-thread (the async seams / a task).

## 7. The mutation-under-read hazard (in full)

Tree-sitter's `Tree` is reparsed incrementally off-thread and republished; a
plugin holding a `node` from snapshot *N* while the buffer advances to *N+1* still
sees a coherent tree — snapshot *N*'s — with byte ranges valid *for N*. It is
never a torn "node points at byte 100 but the text shifted" state, because the
node and the text slice a plugin reads both come from the same point-in-time
snapshot pair (tree + rope, acquired together). The rule the seam enforces:
**acquire the tree snapshot and the document handle from the same context at the
same instant**, so their versions agree; a plugin that mixes an old node with a
new document slice is reading across versions and gets the host's version-mismatch
`err`.

## 8. Paramount-goal alignment

- **#3 Extensibility of the grammar.** *The* enabling seam — structural motions /
  text objects / folds / refactors as plugins, the "tree-sitter-driven variants"
  the goal names. The built-in grammar stays native; this opens the *extension*
  path onto the same tree.
- **#1 Performance.** Host-side query execution + O(1) snapshot acquire + results-
  only crossing; the sole sync use is a bounded, parse-free ancestor walk.
- **#4 Asynchronicity.** Whole-tree queries run off-thread; the snapshot's
  ArcSwap publish is the existing off-thread reparse pipeline, untouched.
- **UX (higher court).** Read-only + eventual-consistency: a plugin's structural
  view catches up a reparse or two later, exactly like syntax colour — never a
  torn read, never a stall.

## 9. Rejected alternatives

- **Cross the whole tree to the guest.** Rejected: a large tree is huge to
  serialise, and it is immediately stale; the resource-handle + host-side-query
  model crosses only what the plugin asks for, lazily, coherent with its snapshot.
- **A text-only seam (the guest re-parses).** Rejected: the guest would ship a
  tree-sitter grammar + parse in WASM, duplicating the host's live incremental
  tree — absurd, slow, and inconsistent with the host's highlights/folds.
- **Stateless per-position host calls (no node handles).** Rejected (the
  node-model question): without navigable handles every relationship (`parent`,
  sibling) needs a fresh by-position host call, which cannot express "the named
  child of *this* node"; structural plugins need to walk *from* a node.
- **Raw `tree_sitter` FFI exposed to WASM.** Rejected: leaks host ABI + lifetimes
  across the sandbox; the WIT records/resources are the stable, capability-gated,
  language-agnostic contract (paramount #2 — WIT is the API).

## 10. WIT sketch (illustrative)

```wit
interface tree-sitter {
    resource tree-snapshot {
        root: func() -> node;
        node-at: func(pos: position) -> option<node>;
        enclosing: func(pos: position, kinds: list<string>) -> option<node>;
        language: func() -> string;
        compile-query: func(source: string) -> result<query, string>;
        run-query: func(q: borrow<query>, within: option<range>) -> list<capture>;
        run-query-ranges: func(q: borrow<query>, within: option<range>) -> list<capture-range>;
    }
    resource node {
        kind: func() -> string;
        is-named: func() -> bool;
        is-error: func() -> bool;
        byte-range: func() -> range;
        parent: func() -> option<node>;
        named-child-count: func() -> u32;
        named-child: func(index: u32) -> option<node>;
        child-by-field: func(name: string) -> option<node>;
        next-named-sibling: func() -> option<node>;
        prev-named-sibling: func() -> option<node>;
        walk: func() -> tree-cursor;
    }
    resource tree-cursor {
        current-node: func() -> node;
        current-field: func() -> option<string>;
        goto-first-named-child: func() -> bool;
        goto-next-named-sibling: func() -> bool;
        goto-parent: func() -> bool;
        reset: func(n: borrow<node>);
    }
    resource query { /* opaque compiled query */ }
    record capture { name: string, node: node }
}
```

## 11. Slices

Sequencing, exit criteria, and status live in the slice plan
([`../operations/slice-plans/archive/plugin-treesitter-seam.md`](../operations/slice-plans/archive/plugin-treesitter-seam.md)):
**TS.1** the snapshot + node core (enough for auto-pair's `enclosing`) → **TS.2**
queries + the cursor walk (the structural-plugin class) → **TS.3** a first
structural consumer end to end.

**Deferred:** query cursors with incremental match streaming, anonymous-node
access, tree editing from WASM (the host owns parsing — likely never), and a
higher-level query builder.
