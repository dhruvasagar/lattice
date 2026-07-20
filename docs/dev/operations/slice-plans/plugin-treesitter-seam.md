# Plugin tree-sitter seam — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contract: [`../../architecture/plugin-treesitter-seam.md`](../../architecture/plugin-treesitter-seam.md).
> A **v1 foundational** seam (plugins query the parse tree). First consumer:
> [`plugin-auto-pair.md`](plugin-auto-pair.md)'s manual scope query (AP.0.3).

Status icons: ✅ done · 🚧 in progress · 📝 planned. Every non-trivial slice ships
the four artefacts (doc + bench-where-perf-relevant + test incl. failure modes +
graceful error handling).

**Status: ✅ TS.1 ✅ · TS.2 ✅ · TS.3 ✅ (auto-pair manual style, AP.3).**

## Sequencing

**TS.1 → TS.2 → TS.3.** TS.1 stands up the snapshot + node core (enough for
auto-pair's `enclosing`); TS.2 adds queries + the cursor walk (the structural-
plugin class); TS.3 proves it end to end through a real plugin. TS.1 depends on
the grammar-context handle extension (auto-pair AP.0.1 — the tree snapshot rides
the same context as the `document` handle).

## Slices

### TS.1 — the snapshot + node core  🚧
`wit/tree-sitter.wit` (the interface); the host `tree-snapshot` / `node` resources
backed by `Arc<SyntaxSnapshot>` + `tree_sitter::Node`. **Reuse the AP.0.1
resource-into-guest-export wiring** (auto-pair design fragment §5.1): the `with:`
key is `pkg/interface.resource` (dot, not slash), each resource needs its empty
interface-level `Host` impl beside the `HostXxx` trait, and host-owned handles are
`borrow`-passed via `Resource::new_borrow(rep)` + host-side `table.delete`.

**Locked decision — snapshot crosses the `lattice-grammar` layering line
type-erased (option A, 2026-07-20).** `ActionContext` gains `syntax:
Option<Arc<dyn Any + Send + Sync>>` (std `Any`, so `lattice-grammar` keeps its
`protocol`+`core`-only dep set — the same reason `buffer` is a `lattice-core`
type, registry.rs:410). The host (dispatch.rs Action gate) acquires the active
buffer's snapshot via `document_syntax_for(id).map(|h| h.snapshot())` — the O(1)
`ArcSwap::load_full` bump — **at the same instant** it clones the buffer (versions
agree, §7), upcasts to `Arc<dyn Any>`, and threads it via the per-dispatch env;
`execute_action` sets `ActionContext.syntax`. The plugin-host trampoline
(`build_action_spec`) downcasts `Arc::downcast::<SyntaxSnapshot>()` and mints the
`tree-snapshot` borrow resource — so `apply-action` takes
`option<borrow<tree-snapshot>>` (absent = no parse / plain-text buffer) alongside
the AP.0.1 `borrow<document>`. `lattice-plugin-host` gains a `lattice-syntax` dep
(builder = `lattice-host`, consumer = `lattice-plugin-host`; native grammar never
reads it). **Node handles** are represented as a **path of child indices from
root** (`Arc<SyntaxSnapshot>` + `Vec<u32>`) — safe (no self-referential
`Node<'tree>`), unambiguous (byte-range + kind can collide on wrapper nodes), and
O(depth) to resolve; the one sync on-keystroke use (`enclosing`) is a single
ancestor walk.

`root` / `node-at`
(`descendant_for_byte_range`) / `enclosing(pos, kinds)` (the `scope_toward`
precedent) / projection (`kind` / `is-named` / `byte-range` / `is-error`) /
navigation (`parent` / `named-child` / `child-by-field` / siblings / `walk`); the
`tree-sitter` editor-capability gate; wire the `tree-snapshot` handle into the
grammar action context (alongside the AP.0.1 `document` handle, same instant so
their versions agree). **Exit:** a fixture guest resolves the cursor's enclosing
node + walks its named children; a plugin without the grant gets no handle. Bench:
`enclosing(pos, kinds)` on the sync path is a bounded, **parse-free** ancestor
walk — pin it ≪ the grammar Reflex budget.

**Delivered (2026-07-20).** `wit/tree-sitter.wit` (the `tree-snapshot` + `node`
core; queries/cursor deferred to TS.2). Host backing in
`crates/lattice-plugin-host/src/tree_resource.rs`: `TreeSnapshotResource`
(`Arc<SyntaxSnapshot>`) + `NodeResource` (a **path of `child` indices** from root,
re-resolved each call — safe, no self-referential `Node<'tree>`; descent uses
`TreeCursor::goto_first_child_for_point`'s binary search, NOT a linear sibling
scan — see the bench note below). Bindgen `with:`-maps both resources
(`grammar_host.rs`); `HostTreeSnapshot` / `HostNode` impls + `add_to_linker` on
BOTH linkers (`lib.rs`); the trampoline downcasts the type-erased
`ActionContext::syntax` and mints `option<borrow<tree-snapshot>>`
(`grammar_trampoline.rs`). Snapshot crosses the `lattice-grammar` line
**type-erased** as `Arc<dyn Any>` (locked decision above); host acquires it in the
dispatch.rs Action gate the same instant as the buffer (`document_syntax_for(id)
.snapshot()`). Capability gate: `outcome.grant.editor.contains(TREE_SITTER)` — no
grant → `none` even on a parsed buffer. **Tests:** 9 `tree_resource` unit tests
(node-at / enclosing / navigation / no-tree); 3 `tests/tree_seam.rs` end-to-end
(granted query → `rust:block:1`; no-grant → graceful `err`; no-parse → graceful
`err`). **Bench:** `benches/tree_enclosing.rs` — `enclosing` ~1.12 µs on a 2000-fn
file (the linear-scan first cut measured 175 µs, a paramount-#1 violation the
bench caught; the cursor binary-search fix is what makes the parse-free-bounded
claim true). Guest signature: `apply-action` gains `tree: option<borrow<
tree-snapshot>>` across all three grammar guests (auto-pair / grammar-guest /
multiseam-guest); auto-pair ignores it until AP.3.

### TS.2 — queries + cursor  ✅
`compile-query(source)` → `query` (per-language, `err` on malformed / grammar
mismatch); `run-query(query, within)` → `list<capture>` with **host-side**
predicate eval (`#eq?` / `#match?` / `#any-of?` — only surviving captures cross);
the `tree-cursor` resource (`goto-first-named-child` / `goto-next-named-sibling` /
`goto-parent` / `current-node` / `current-field` / `reset`) + `child-by-field`
field-name access. **Exit:** a fixture runs a predicated query over a range and
gets only surviving captures; a cursor walks the tree. No hot-path bench (queries
run off-thread; a whole-tree `run-query` from a sync grammar action is forbidden
by the design — off-thread only).

**Delivered (2026-07-20).** `wit/tree-sitter.wit` grew `compile-query` /
`run-query` on `tree-snapshot`, `walk` on `node`, and the `query` (opaque) +
`tree-cursor` resources + `capture` record. Host backing (`tree_resource.rs`):
`QueryResource { Query, Lang }` (the `Lang` guards a mismatched snapshot →
empty), `CursorResource { snapshot, path }` (same safe path scheme as
`NodeResource`, mutated by `goto-*`). `run-query` uses tree-sitter 0.24's
`QueryCursor::matches` **streaming iterator** with the snapshot's source as the
`TextProvider`, so `#eq?` / `#match?` / `#any-of?` are filtered host-side before
any capture crosses; `within` maps to `set_point_range`; capture nodes get a
`NodeResource` path via `path_of` (walk-up by id — O(depth×siblings), fine
off-thread). Bindgen `with:`-maps both new resources; `HostQuery` (drop only) +
`HostTreeCursor` impls + `compile-query`/`run-query`/`walk` added to the existing
`HostTreeSnapshot`/`HostNode` impls (`lib.rs`). **Tests:** 6 new `tree_resource`
unit tests (compile/run, `#eq?` predicate filtering host-side, malformed-query
err, cursor walk, `current-field`); 2 new `tests/tree_seam.rs` end-to-end (query
→ `1:fname:identifier`; cursor → `true:function_item`). No bench (off the sync
path by design). Guest `apply-action` signature is unchanged — queries/cursors
are called on the existing `tree-snapshot` handle inside the action.

### TS.3 — first structural consumer  ✅
Prove the seam end to end through a real plugin — either auto-pair's `manual`
scope query (its AP.3) or a small structural-motion fixture. **Exit:** a plugin's
structural behavior is driven entirely through the seam, coherent under an edit
(eventual-consistency — the plugin's view catches up a reparse or two later, never
a torn read).

**Delivered (2026-07-20) via AP.3.** auto-pair's `manual` style is the first
structural consumer: its `manual_close` action calls `tree-snapshot.enclosing
(cursor, block-kinds)` to bound the backward `find_pair` scan to the enclosing
lexical scope (line-capped fallback where there's no parse). Proven end to end in
`lattice-plugin-loader/tests/auto_pair_manual.rs` — the real loaded plugin, with
the `tree-sitter` grant, closes the nearest unmatched opener through the seam. A
general enabler landed with it: a grammar guest can now read a config option
(`instantiate_grammar_plugin` wires the shared `ConfigRegistry` into the grammar
store), so auto-pair reads `auto-pair.style` live. See the auto-pair slice plan
for the AP.3 detail.

## Deferred

Incremental match-streaming query cursors, anonymous-node access, tree editing
from WASM (the host owns parsing — likely never), and a higher-level query
builder.
