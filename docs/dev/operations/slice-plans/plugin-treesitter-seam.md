# Plugin tree-sitter seam — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contract: [`../../architecture/plugin-treesitter-seam.md`](../../architecture/plugin-treesitter-seam.md).
> A **v1 foundational** seam (plugins query the parse tree). First consumer:
> [`plugin-auto-pair.md`](plugin-auto-pair.md)'s manual scope query (AP.0.3).

Status icons: ✅ done · 🚧 in progress · 📝 planned. Every non-trivial slice ships
the four artefacts (doc + bench-where-perf-relevant + test incl. failure modes +
graceful error handling).

**Status: 📝 all planned.**

## Sequencing

**TS.1 → TS.2 → TS.3.** TS.1 stands up the snapshot + node core (enough for
auto-pair's `enclosing`); TS.2 adds queries + the cursor walk (the structural-
plugin class); TS.3 proves it end to end through a real plugin. TS.1 depends on
the grammar-context handle extension (auto-pair AP.0.1 — the tree snapshot rides
the same context as the `document` handle).

## Slices

### TS.1 — the snapshot + node core  📝
`wit/tree-sitter.wit` (the interface); the host `tree-snapshot` / `node` resources
backed by `Arc<SyntaxSnapshot>` + `tree_sitter::Node`. **Reuse the AP.0.1
resource-into-guest-export wiring** (auto-pair design fragment §5.1): the `with:`
key is `pkg/interface.resource` (dot, not slash), each resource needs its empty
interface-level `Host` impl beside the `HostXxx` trait, and host-owned handles are
`borrow`-passed via `Resource::new_borrow(rep)` + host-side `table.delete`. `root`
/ `node-at`
(`descendant_for_byte_range`) / `enclosing(pos, kinds)` (the `scope_toward`
precedent) / projection (`kind` / `is-named` / `byte-range` / `is-error`) /
navigation (`parent` / `named-child` / `child-by-field` / siblings / `walk`); the
`tree-sitter` editor-capability gate; wire the `tree-snapshot` handle into the
grammar action context (alongside the AP.0.1 `document` handle, same instant so
their versions agree). **Exit:** a fixture guest resolves the cursor's enclosing
node + walks its named children; a plugin without the grant gets no handle. Bench:
`enclosing(pos, kinds)` on the sync path is a bounded, **parse-free** ancestor
walk — pin it ≪ the grammar Reflex budget.

### TS.2 — queries + cursor  📝
`compile-query(source)` → `query` (per-language, `err` on malformed / grammar
mismatch); `run-query(query, within)` → `list<capture>` with **host-side**
predicate eval (`#eq?` / `#match?` / `#any-of?` — only surviving captures cross);
the `tree-cursor` resource (`goto-first-named-child` / `goto-next-named-sibling` /
`goto-parent` / `current-node` / `current-field` / `reset`) + `child-by-field`
field-name access. **Exit:** a fixture runs a predicated query over a range and
gets only surviving captures; a cursor walks the tree. No hot-path bench (queries
run off-thread; a whole-tree `run-query` from a sync grammar action is forbidden
by the design — off-thread only).

### TS.3 — first structural consumer  📝
Prove the seam end to end through a real plugin — either auto-pair's `manual`
scope query (its AP.3) or a small structural-motion fixture. **Exit:** a plugin's
structural behavior is driven entirely through the seam, coherent under an edit
(eventual-consistency — the plugin's view catches up a reparse or two later, never
a torn read).

## Deferred

Incremental match-streaming query cursors, anonymous-node access, tree editing
from WASM (the host owns parsing — likely never), and a higher-level query
builder.
