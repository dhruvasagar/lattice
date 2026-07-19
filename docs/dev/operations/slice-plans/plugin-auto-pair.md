# auto-pair — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contract: [`../../architecture/plugin-auto-pair.md`](../../architecture/plugin-auto-pair.md).
> The first bundled 8b plugin. Its prerequisites (AP.0.x) are **general** host
> capabilities; AP.0.3 is the tree-sitter seam, which has its own slice plan
> ([`plugin-treesitter-seam.md`](plugin-treesitter-seam.md)).

Status icons: ✅ done · 🚧 in progress · 📝 planned. Every non-trivial slice ships
the four artefacts (doc + bench-where-perf-relevant + test incl. failure modes +
graceful error handling).

**Status: 🚧 in progress — AP.0.1 ✅, AP.1.0 ✅, AP.1 ✅, AP.2 ✅ (open + close);
AP.3 / AP.4 planned.**

> **Mode enablement moved out (2026-07-19).** `auto-pairs-mode` is now
> **available-but-off**, not self-activating — the user enables it via `init.rs`.
> That model (available-vs-enabled, `enable-mode`, the `plugin-loaded` event,
> init-first ordering) lives in its own fragment:
> [`../../architecture/config-and-init.md`](../../architecture/config-and-init.md)
> + [`config-and-init.md`](config-and-init.md) (slices CI.1–CI.6). auto-pair is
> CI.5's consumer; AP.4 (bundle) ships the mode off by default. The earlier
> `enable-mode`-at-top-level / re-activation-event / pending-value-store sketches
> are **superseded** by that fragment.

## Sequencing — two waves

The `auto` style needs only AP.0.1 (buffer read); the `manual` style adds AP.0.2
(fall-through) + AP.0.3 (tree-sitter scope). So the build lands the pipeline proof
+ the default style first, then the foundation + the flagship style.

```
Wave 1 (pipeline proof + auto style, shippable):
  AP.0.1 ──► AP.1.0 ──► AP.1 ──► AP.2

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

### AP.1.0 — multi-seam feasibility + superset linkers  ✅
The blocker AP.1 surfaced: one plugin `.wasm` provides the SYNC grammar seam AND
the async modes/config seams, but a component's import set is fixed while grammar
instantiates against a sync linker and modes/config against the async linker.
**Resolved:** every seam import host-func is sync (only exports are async), so
BOTH linkers become supersets — the async linker gains grammar+buffer, the sync
grammar linker gains modes+config (NOT logging: the combined world omits it,
preserving "no logging on the grammar hot path"). The loader already
re-instantiates per `provides` entry, so no loader-model change. **Landed:** the
`multiseam-guest` fixture (one component, three seams) + `tests/multiseam.rs`
compiles it once and drains each seam from the same artifact — all register.
Isolation cost (grammar linker now carries the sync modes/config register funcs,
inert unless imported) accepted for single-artifact multi-seam plugins.

### AP.1 — crate scaffold + registration  ✅
`plugins/auto-pair/` guest (`wasm32-wasip2`, `wit-bindgen`, world
`auto-pair-plugin`) + `plugin.toml` (id `auto-pair`,
`provides = ["grammar","modes","config"]` — **modes**, not keymap: the mode owns
its keymap per the mode-ownership rule; grammar BEFORE modes so the keymap
resolves the plugin's own actions; **no capabilities**). Registers the pairing
actions (one per opener/closer — a mode keymap binding carries no args, so the
action can't otherwise know which pair fired), the `auto-pairs-mode` `global`
minor mode owning the insert-mode keymap, and the `auto-pairs-style` /
`auto-pairs-close-key` options (behavior is option-gated in the handlers — no
`OptionChanged` re-binding; the keymap set stays stable across `:set`).
**Landed:** `tests/auto_pair.rs` (in the loader crate) discovers + loads it
through the real loader; all three seams' contributions register (actions with
`SourceLayer::Plugin` provenance, the mode owning a gated `MinorMode` keymap, the
options in the config registry). Scaffold ships the round-bracket pair + backspace
with **no-op action bodies**; AP.2 fills the `auto` behavior.

### AP.2 — `auto` style (open + close-skip)  ✅
The `auto` behavior for the round-bracket pair, reading the buffer via AP.0.1.
Required a small **edit-model extension** (general host API, not auto-pair-
specific): `action-context` gains `buffer-id` (the `target` an action names in an
`apply-edit`), and `apply-edit-payload.cursor` changed from `option<u32>` (a row)
to `option<position>` (column-precise) so an action can park the caret *between*
an inserted pair — the native `Effect::ApplyEdit.cursor` became `Option<Position>`
(the ~7 native diff/ai callers pass `Position::new(row, 0)`, behaviour-preserving).
**Landed:** `(` → `()` caret-between (a precise-cursor `apply-edit`); `)` before a
`)` steps over via `selection-change` (pure caret move, no spurious edit), else
inserts `)`. Asserted end-to-end through the loaded plugin (`tests/auto_pair.rs`
dispatches the real guest and checks the effects). Graceful: an out-of-range read
falls through to insert.

**Backspace deferred to Wave 2** (was in AP.2's original exit): deleting the empty
pair on `<BS>` needs the action to DECLINE to the builtin backspace when the caret
isn't inside a pair (AP.0.2 fall-through). Binding `<BS>` without that forces the
plugin to reimplement normal backspace (grapheme deletion, line-joins) —
reinventing the builtin (heuristic #1 forbids). It lands with AP.0.2. **Milestone:
a shippable, default-on auto-pair (open + close) without the tree-sitter seam.**

## Wave 2

### AP.0.2 — declining / fall-through bindings  📝
General host prereq (design fragment §5.2). A "declined" action outcome that
resumes keymap resolution at the next lower layer — so the manual close key falls
through when nothing is unmatched (requirement #4: completion nav / newline / a
user remap still run). **Also unblocks auto-`<BS>`** (deferred from AP.2): the
backspace action declines to the builtin when the caret isn't inside an empty
pair, instead of reimplementing normal backspace. **Exit:** a plugin action
declines → a lower-layer / builtin binding for the same chord runs; accepts → it
doesn't. Then bind `<BS>` → `auto-pair-backspace` (delete the empty pair, else
decline).

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
no grant). Mode ships **off by default** (available-but-off, per
[`config-and-init.md`](config-and-init.md) CI.3/CI.5) — the shipped default
`init.rs` enables it with `on_plugin_loaded("auto-pair") → enable_mode(…)`, which
the user can remove. **Exit:** a fresh editor with the default init.rs auto-pairs
out of the box; `:plugins` lists auto-pair; removing the `enable_mode` line leaves
it loaded but inert; `:set auto-pairs-style=manual` flips the style live.

## Deferred to v2

Wrap-selection (opener with a Visual selection surrounds it), word-boundary /
string-comment suppression (auto), per-language pair tables + a user add/remove
API. (Lexical-scope scanning via tree-sitter is **v1** — AP.0.3.)
