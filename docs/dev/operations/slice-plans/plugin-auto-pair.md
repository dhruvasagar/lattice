# auto-pair — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contract: [`../../architecture/plugin-auto-pair.md`](../../architecture/plugin-auto-pair.md).
> The first bundled 8b plugin. Its prerequisites (AP.0.x) are **general** host
> capabilities; AP.0.3 is the tree-sitter seam, which has its own slice plan
> ([`plugin-treesitter-seam.md`](plugin-treesitter-seam.md)).

Status icons: ✅ done · 🚧 in progress · 📝 planned. Every non-trivial slice ships
the four artefacts (doc + bench-where-perf-relevant + test incl. failure modes +
graceful error handling).

**Status: 🚧 in progress — AP.0.1 ✅ (the `document`-handle prereq); AP.1+ planned.**

## Sequencing — two waves

The `auto` style needs only AP.0.1 (buffer read); the `manual` style adds AP.0.2
(fall-through) + AP.0.3 (tree-sitter scope). So the build lands the pipeline proof
+ the default style first, then the foundation + the flagship style.

```
Wave 1 (pipeline proof + auto style, shippable):
  AP.0.1 ──► AP.1 ──► AP.2

Wave 2 (foundation + manual style):
  AP.0.2 ─┐
  TS.1→TS.2→TS.3 (tree-sitter seam, its own plan) ─┴─► AP.3
                                                        │
  AP.1..AP.3 ─────────────────────────────────────────►AP.4 (bundle)
```

## Wave 1

### AP.0.1 — grammar-context `document` handle + cursor  ✅
General host prereq (design fragment §5.1). `action-context` carried only
`args`/`register`/`count`. Extended `apply-action` with a `borrow<document>` +
`action-context.cursor`, mirroring the picker seam (`init(ctx)` takes a
`document`) — the **first host-owned resource to cross into a guest export**,
resolving the deferred bindgen modeling subtlety (the `with:` resource key uses
`buffer.document`, not `buffer/document` — recorded in §5.1). Native
`ActionContext` carries an owned `Buffer` (O(1) rope clone; keeps
`lattice-grammar` off `lattice-runtime`), the trampoline mints the snapshot +
lends the borrow per dispatch. **Landed:** the `grammar-guest` `read-at-cursor`
action reads a slice + the cursor round-trips (`grammar_source.rs`); out-of-range
degrades to a typed `err` (no trap); a `perf_ratchet` case pins the added
snapshot/table cost inside the grammar Reflex budget (10µs debug). All four
artefacts shipped (doc §5.1 + ratchet + tests + graceful error).

### AP.1 — crate scaffold + registration  📝
`plugins/auto-pair/` guest (`wasm32-wasip2`, `wit-bindgen`) + `manifest.toml`
(id `auto-pair`, `provides = ["grammar","keymap","config"]`, **no capabilities**);
register the pairing actions, the `auto-pairs-style` / `auto-pairs-close-key`
options + the `OptionChanged` subscription, and the style-appropriate insert-mode
bindings. **Exit:** loads via the loader; contributions register with
`SourceLayer::Plugin` provenance.

### AP.2 — `auto` style  📝
Open-insert + close-skip + backspace-deletes-the-empty-pair (reads via AP.0.1).
**Exit:** `(` → `()` caret-between; `)` before `)` steps over; backspace in `()`
deletes both — asserted through the loaded plugin. **Milestone: a shippable,
default-on auto-pair without the tree-sitter seam.**

## Wave 2

### AP.0.2 — declining / fall-through bindings  📝
General host prereq (design fragment §5.2). A "declined" action outcome that
resumes keymap resolution at the next lower layer — so the manual close key falls
through when nothing is unmatched (requirement #4: completion nav / newline / a
user remap still run). **Exit:** a plugin action declines → a lower-layer / builtin
binding for the same chord runs; accepts → it doesn't.

### AP.0.3 — tree-sitter query seam  📝
The enclosing-scope query that bounds the manual scan (design fragment §5.3, §7).
Its own design + slice plan: [`plugin-treesitter-seam.md`](plugin-treesitter-seam.md)
(TS.1 core → TS.2 queries+cursor → TS.3 first consumer). Auto-pair's `manual`
style is a candidate TS.3 consumer.

### AP.3 — `manual` style  📝
The `find_pair` port (design fragment §3) + the close key + the fall-through
(AP.0.2), scanning **only the enclosing lexical scope** (AP.0.3 / §7), with the
cursor-backward `document.slice` fallback where there's no parse tree. **Exit:**
with `auto-pairs-style=manual`, the close key closes the nearest unmatched open in
the scope (inside-out on repeat; symmetric pairs + `<`/`>` handled), falls through
when nothing is unmatched, and stays bounded on a large buffer.

### AP.4 — bundling  📝
Ship `auto-pair.wasm` compiled-in / in `core-plugins/`, pre-granted at boot (needs
no grant). **Exit:** a fresh editor auto-pairs out of the box; `:plugins` lists it;
`:set auto-pairs-style=manual` flips it live.

## Deferred to v2

Wrap-selection (opener with a Visual selection surrounds it), word-boundary /
string-comment suppression (auto), per-language pair tables + a user add/remove
API. (Lexical-scope scanning via tree-sitter is **v1** — AP.0.3.)
