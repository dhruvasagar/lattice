# Plugin-contributed languages — slice plan

**Design fragment:**
[`../../architecture/plugin-languages.md`](../../architecture/plugin-languages.md).

**Status:** LG.0–LG.6 📝. Nothing started.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

## Sequencing

```
LG.0  two-wasmtime feasibility        ← GATE. Everything else is conditional.
  │
LG.1  wasm-grammar bench              ← GATE. Records the parse cost.
  │
LG.2  Lang::Plugin + runtime registry (no WIT yet)
  │
LG.3  the `language` WIT seam + drain + teardown
  │
LG.4  org plugin: grammar + highlights (per-level headlines)
  │
LG.5  org plugin: folds
  │
LG.6  docs, ledger, user-facing surface
```

**LG.0 and LG.1 are gates, not slices.** They can fail, and failing is a
result: the design's §6 fallback (bundle org natively, plugin ships only
behaviour) exists precisely so a failed gate has somewhere to go. Build
them first — discovering a two-runtime crash after the seam ships would
be the expensive order.

| Slice | Description | Status |
|---|---|---|
| LG.0 | Prove `wasmtime-c-api` 36 and `wasmtime` 46 coexist | 📝 |
| LG.1 | Bench: wasm grammar vs native parse | 📝 |
| LG.2 | `Lang::Plugin(LanguageId)` + runtime language registry | 📝 |
| LG.3 | `language` WIT seam, loader drain, teardown | 📝 |
| LG.4 | Org plugin: grammar + per-level headline highlights | 📝 |
| LG.5 | Org plugin: folds | 📝 |
| LG.6 | Docs, benchmarks, ledger | 📝 |

---

### LG.0 — the two-wasmtime gate 📝

tree-sitter's `wasm` feature pulls `wasmtime-c-api 36`; the plugin host
runs `wasmtime 46`. Both install `SIGSEGV`/`SIGBUS` handlers for
guard-page traps. **Answer this before building anything on it.**

A spike, not a feature: enable `tree-sitter/wasm`, stand up a
`WasmStore`, load one grammar, parse with it, and *while a plugin-host
guest is also running*, force a guest trap (the `grammar-guest`
fixture's `traps` motion already does this on demand) and confirm both
runtimes still behave.

- *paramount:* #1 — a crash under load is the worst possible outcome
  here, and it would not surface in ordinary testing.
- *acceptable outcomes:* (a) they coexist, proven under a forced trap
  from each side; (b) tree-sitter's C API can be pointed at an engine we
  already own, collapsing to one runtime; (c) it does not work, and the
  track stops here in favour of design §6.
- *test:* the spike IS the test, and it stays as a regression guard —
  a wasmtime bump on either side must re-run it.
- *doc:* record the outcome in the design fragment §3, whichever way it
  goes. A failed gate is worth writing down more than a passing one.

### LG.1 — the parse-cost bench 📝

The ecosystem figure for wasm-vs-native tree-sitter parsing is roughly
2–5×. Measure it here rather than inherit it.

- Bench the same grammar both ways on the same input — native
  `tree-sitter-md` against `tree-sitter-md` compiled to wasm — so the
  comparison isolates the loading mechanism rather than the language.
- Cold parse and incremental reparse, since only the second is on a path
  a user waits behind.
- *paramount:* #1. Record in `benchmarks.md` with the ratio, and state
  plainly whether it changes the reparse budget for plugin languages.
- *gate:* if the ratio is materially worse than 5×, say so and re-open
  §6 rather than shipping a language surface that makes large files
  worse.

### LG.2 — `Lang::Plugin` + the runtime registry 📝

Substrate only, no WIT — it lands green and useful on its own.

- `Lang::Plugin(LanguageId)` where `LanguageId` is interned. Existing
  variants stay, so every native `match` keeps its arms; sixteen sites
  across `lang.rs` / `modes.rs` / `indent.rs` / `format/spec.rs` gain
  one fallthrough each.
- A runtime language registry beside `registry.rs`'s
  `HighlightConfiguration` cache, behind the RCU handle pattern
  (`Arc<ArcSwap<…>>`, teardown by provenance) that
  `contributable-registries.md` established — a plugin language must
  disappear on unload like everything else.
- Extension → `Lang` resolution consults the runtime registry after the
  native table, so a plugin cannot shadow a built-in language by
  accident. (Whether it should be *able* to, deliberately, is a DB.8-
  shaped question — deferred until someone asks.)
- *test:* a registered language resolves by extension; unload withdraws
  it; a plugin language and a native one coexist; native resolution is
  unchanged when the registry is empty.

### LG.3 — the `language` WIT seam 📝

- `wit/language.wit` per design §2.2 — `register-language(spec)`, data
  only, no callbacks, guest dropped after registration (the `help` seam
  shape).
- `PluginSeam::Language` + the loader drain; the drain `match` is
  exhaustive, so the compiler demands the arm.
- Queries compile at registration, not first use: a malformed query is
  the plugin author's error and must surface at load with the offending
  query named, not silently disable folding three days later.
- *error handling:* a grammar that fails to load, or a query that fails
  to compile, fails THAT language with a named reason and leaves the
  plugin's other contributions alone.
- *test:* a fixture plugin registers a real grammar and its file type
  highlights; a bad grammar is rejected with a legible message; unload
  withdraws the language and reverts open buffers to plain.

### LG.4 — org: grammar + headlines 📝

The first real consumer.

- Plugin builds `nvim-orgmode/tree-sitter-org` to wasm as part of its
  own build (the plugin manager already builds sources — PM.5–PM.8).
- `queries/highlights.scm` with **per-level** headline capture. Org's
  `stars` is one node whose text length is the level, so this needs
  `#eq?` predicates — **and no query in the tree uses one today**, so
  LG.4 proves predicate support or discovers its absence.
- *test:* `* a` / `** b` / `*** c` produce `Heading1` / `Heading2` /
  `Heading3` spans. **And a GPU test that the headline scales** — design
  §7 claims variable-font headlines come free via `heading_scale_split`
  because it is not markdown-gated; that claim is worth a test, not a
  paragraph.
- *fallback, if `#eq?` is unsupported:* compute the level host-side from
  the stars text. Worse (a host-side language special case) and to be
  taken only if the predicate route is genuinely closed.

### LG.5 — org: folds 📝

`queries/folds.scm` over `(section)`, `(block)`, `(drawer)`, `(list)` —
the same pipeline markdown's `(section)` fold uses, so this is queries
and tests rather than mechanism.

- *test:* folding a headline hides its subtree; a `#+BEGIN_SRC` block
  folds; a drawer folds; `zR`/`zM` behave as they do elsewhere.

### LG.6 — docs, benchmarks, ledger 📝

- `docs/user/plugins.md`: the `language` seam row + how a plugin ships a
  grammar (the `tree-sitter build --wasm` step is the non-obvious half).
- A user doc for org itself — shipped **in the org plugin** via the
  `help` seam (CR.3), not in the lattice binary. Org is the second real
  test of that decision after the core plugins.
- `benchmarks.md`: LG.1's numbers.
- `implementation.md`: the LG.* table; note that Phase 8b gains a second
  reference plugin.

---

## Not in this track

**Org's editing model** — headline promotion / demotion, subtree motion,
TODO and visibility cycling, agenda, tables. That is `org-mode` the
major mode, owning its keymap and handler bodies per the mode-ownership
rule, and it rides seams that already exist (`modes`, `grammar`,
`keymap`). It is gated on nothing here and can proceed in parallel once
the grammar lands.

**Migrating bundled languages to the seam.** Nineteen grammars are
workspace dependencies today. Once this works they *could* ship as
plugins, and eventually should — but doing it before the seam has a real
consumer would be rewriting for novelty, which heuristic #1 forbids as
firmly as it forbids keeping an inferior design.
