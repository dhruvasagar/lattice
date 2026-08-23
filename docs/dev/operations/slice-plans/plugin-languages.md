# Plugin-contributed languages — slice plan

**Design fragment:**
[`../../architecture/plugin-languages.md`](../../architecture/plugin-languages.md).

**Status:** LG.0 ✅ (2026-08-22) — the runtimes coexist. LG.1 ✅
(2026-08-23) — 2.0× cold, 1.25× incremental; **both gates are now
closed and nothing sends this track to §6.** LG.2 ✅ (2026-08-23) —
`Lang::Plugin` + the runtime registry. LG.3a ✅ (2026-08-23) — the live
`LangRegistry`. LG.3b ✅ (2026-08-23) — wasm grammars actually parse. LG.3c ✅
(2026-08-23) — **the seam is real: a plugin on disk contributes a
language.** LG.4–LG.6 📝.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

## Sequencing

```
LG.0  two-wasmtime feasibility        ← GATE. Everything else is conditional.
  │
LG.1  wasm-grammar bench              ← GATE. Records the parse cost.
  │
LG.2  Lang::Plugin + runtime registry (no WIT yet)
  │
LG.3a the live LangRegistry (grammar + queries, no WIT)
  │   └─ wasm runtime made unconditional (design §3.2)
  │
LG.3b loading grammars from wasm + the stores parsers need
  │
LG.3c the `language` WIT seam + drain + teardown  ← the seam is now real
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
| LG.0 | Prove `wasmtime-c-api` 36 and `wasmtime` 46 coexist | ✅ |
| LG.1 | Bench: wasm grammar vs native parse | ✅ |
| LG.2 | `Lang::Plugin(LanguageName)` + runtime language registry | ✅ |
| LG.3a | Live `LangRegistry`: runtime grammar + compiled queries | ✅ |
| LG.3b | Load grammars from wasm; parser store strategy | ✅ |
| LG.3c | `language` WIT seam, loader drain, teardown | ✅ |
| LG.4 | Org plugin: grammar + per-level headline highlights | 📝 |
| LG.5 | Org plugin: folds | 📝 |
| LG.6 | Docs, benchmarks, ledger | 📝 |

---

### LG.0 — the two-wasmtime gate ✅ (2026-08-22)

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

**Outcome: (a) — they coexist.** Both link; a real guest trap is still
caught and quarantined with tree-sitter's runtime live; proven in BOTH
initialisation orders, since handler registration is order-dependent.
Three tests, kept as the regression guard for the next wasmtime bump.

**Residual, stated rather than glossed:** no wasm *grammar* has been
parsed yet — that needs the tree-sitter CLI toolchain and is LG.1's
job. LG.0 settles stability; performance is still open, which is now
the only thing that can send this track to §6's fallback.

### LG.1 — the parse-cost bench ✅ (2026-08-23)

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

**Outcome: passed, at the good end of the band.** Cold parse **2.0×**,
flat across 3.5 KB → 118 KB — it does not degrade with size, which was
the property that mattered. Incremental reparse **1.25×**, because
reused subtrees are host-side C on both paths and only the newly-lexed
region runs guest code; that is the path a user waits behind repeatedly.
Numbers in `benchmarks.md`; design §4.1.

**The toolchain blocker dissolved rather than being paid.** No
emscripten, no docker, no tree-sitter CLI: tree-sitter's wasm store
ships its own wasm libc and supplies the memory/table/stack imports, so
a grammar is a plain `wasm-ld -shared` side module.
`scripts/build-wasm-grammar.sh` builds one with **clang + rustup only**
(rust-lld dispatches as `wasm-ld`). Design §4.2 records why, and the
`--Bsymbolic` flag that is easy to miss. So this is *not* a contributor
prerequisite and not a plugin-author-only capability — the question
raised when LG.1 was blocked no longer needs an answer.

Shipped: the bench (`crates/lattice-syntax/benches/wasm_vs_native_parse.rs`),
the build script, and `crates/lattice-syntax/tests/wasm_grammar.rs` —
which builds through the script and asserts wasm and native trees are
identical cold **and** after an incremental edit, so the recipe itself
is under test rather than being a README step that rots.

*Residual — CLOSED by LG.3b (2026-08-23).* The `wasm-grammar` feature was
off by default, so a wasmtime bump was caught only by someone opting in.
`tree-sitter/wasm` is now unconditional and both this crate's
`tests/wasm_grammar.rs` and LG.0's `two_wasmtime_runtimes.rs` run in the
default CI path.

### LG.2 — `Lang::Plugin` + the runtime registry ✅ (2026-08-23)

Substrate only, no WIT — it lands green and useful on its own.

- `Lang::Plugin(LanguageName)`, interned. Existing variants stay, so
  every native `match` keeps its arms and each exhaustive site gains one
  fallthrough. *(Planned as `LanguageId` over an intern index and as
  "sixteen sites across four files"; both were corrected on measurement
  — see the outcome below.)*
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

**Outcome.** `crates/lattice-syntax/src/plugin_lang.rs`, 14 tests (LG.3a
took the file to 20).

**The match surface was 5 sites across 3 files**, not the sixteen this
plan estimated — measured by adding a probe variant and fixing the
layers one at a time until the workspace compiled. `indent.rs` needed
nothing: it resolves by name through the registry, so it was already
provenance-agnostic. Design §2.3 carries the table.

**The payload is a name, not an index** — a deviation from what §2.3
specified, taken on merit and recorded there. `Lang::name()` *is* the
registry lookup key (six calls per highlight invocation, plus folds and
indents), so an index would have put a process-global table read inside
a function that is `&'static str`-pure. `LangRegistry` is already
`HashMap<&'static str, LangConfig>`, so a name-keyed language joins the
map bundled ones live in. Cost: one leaked string per *distinct* name,
deduped by `LanguageName::intern`.

**Reads are process-global; writes go through the RCU handle.**
`detect_from_path` has nineteen call sites across three crates, and
threading a handle would have made plugin languages visible on some
paths and invisible on others — a two-tier language concept. Registration
and teardown-by-provenance follow `contributable-registries.md` §2
verbatim. The empty registry costs one relaxed atomic load and never
touches the `ArcSwap`; benched, because `detect_from_path` runs per hunk
in magit's diff highlighting.

*Scope held:* the registry carries identity and selection only. Grammar
and compiled queries join it at LG.3, as an added field rather than a
restructure. So a registered language currently resolves by extension
and then renders as plain text, which is exactly what a language whose
plugin has unloaded should also do — one path, not two.

*Deliberately deferred:* a plugin may not register a bundled language's
name (`ShadowsBuiltin`), and claiming a bundled *extension* is allowed
but never wins, since the native table is consulted first. Whether
deliberate override should be possible is a DB.8-shaped question, left
until someone asks.

### LG.3a — the live `LangRegistry` ✅ (2026-08-23)

Carved out of LG.3 when it became clear the seam and the registry are two
separable problems, and only the seam needs the second wasmtime. LG.3a
lands green with no WIT and no `wasm-grammar` feature, which also means it
is testable with a bundled grammar registered *as if* it came from a
plugin — proving the pipeline is provenance-agnostic without depending on
the thing whose gating is still undecided.

- `LangRegistry` becomes the RCU value itself rather than gaining a
  sibling map. A second map would have meant a kind-branch in all eight
  accessors — the rule that forbids `match buffer_kind` forbids this too.
  Configs move behind `Arc` so `Clone` is a refcount bump per language;
  the ~1.2 s of bundled query compilation is never repeated.
- `LangRegistry::standard()` now returns the live snapshot, so every
  existing `registry.highlights_query(lang.name())` finds a plugin
  language exactly as it finds `rust` — **zero call-site changes**.
- `provenance: Option<u64>` per config; teardown by `retain`. Bundled
  languages carry `None` and are untouchable.
- `plugin_lang::register_with_grammar` is atomic across BOTH registries:
  compile queries → claim the name → install. A bad query leaves the
  language absent rather than resolvable-but-dead; a name collision
  leaves the winner's grammar untouched. Both directions are tested.
- Queries compile at registration, naming the offending file.
- *test:* 20 in `plugin_lang`, the load-bearing one being that a
  runtime-registered language parses AND highlights through the ordinary
  `Syntax` path. *bench:* snapshot cost, since `standard()` is per-buffer
  and per-hunk.

*Carried to LG.3b:* nothing here loads wasm. `GrammarSpec.grammar` is a
`tree_sitter::Language`, and where it came from is not this layer's
business — which is the same property §2.1 relies on.

### LG.3b — loading grammars from wasm ✅ (2026-08-23)

Carved out when probing turned up a constraint the design had not
recorded: a wasm-backed `Language` can only be used by a `Parser` that
**owns a `WasmStore`**, and a parser without one fails outright. That is
not a detail of the seam, it is the parse path — so it lands before the
seam, with the seam reduced to plumbing.

- `wasm_grammar::load` compiles a side module to a `Language` (~102 ms,
  once, at registration). The loading store is dropped: a `Language`
  outlives it.
- Two store strategies, because two call shapes. `Syntax`'s long-lived
  parser gets its own (5 ms per wasm buffer, off the keystroke path);
  injection highlighting — a fresh `Parser` **per injection, per highlight
  call** — borrows a thread-local store and returns it. Twenty fenced
  blocks would otherwise cost 20 × 5 ms on every highlight.
- Native grammars are untouched: both entry points check `is_wasm` first.
- *test:* a wasm-loaded markdown grammar registered as a plugin language
  highlights **identically to the bundled one**, injected inline content
  included. "It produced some spans" would pass with a subtly wrong parse;
  this cannot. Also covers wasm-parent → native-child injection, the shape
  org needs for `#+BEGIN_SRC rust`.
- *bench:* the three store costs, since they are new per-buffer work.

### LG.3c — the `language` WIT seam ✅ (2026-08-23)

**Decision taken (2026-08-23), design §3.2:** `tree-sitter/wasm` is ON
unconditionally — **+5.7 MiB**, measured, with zero runtime cost when
unused. Gating it would have meant a stock build where a user's language
plugin silently does nothing, plus a permanent `#[cfg]` seam CI must build
both ways. The GPUI `gui` feature is not the precedent it looks like:
TUI-only is a real deployment, "no plugin languages" is not. Landed ahead
of the seam, which also put LG.0's and LG.1's guards into the default CI
path.

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
  *(The last two are already covered at the substrate level by LG.3a;
  these go through an actual guest.)*

**Outcome.** `wit/language.wit`, `lattice-plugin-host/src/language_host.rs`,
the loader's `drain_language` + teardown, the `language-guest` fixture, and
`lattice-plugin-loader/tests/language_drain.rs`.

**`grammar-name` was added to the spec**, which the design had not
anticipated: the language name and the grammar's `tree_sitter_<x>` export
are usually the same string but need not be, and lattice's own `sql` on
`tree-sitter-sequel` is the counter-example. Building the fixture is what
surfaced it — the only grammar to hand exports `tree_sitter_markdown`,
and `markdown` is a bundled name that must be refused. Design §2.2.

**The name is not namespaced, unlike `help`.** A language name has to be
the one users type in `#+BEGIN_SRC <name>` and injection queries, so
prefixing it with the plugin id would break the thing it names. Collisions
are refused instead.

**Teardown cannot be skipped.** Every other seam reverses through an
`Option<Handle>` the loader may not have been given; the language registry
is process-global, so `unregister_plugin` is unconditional. A language left
registered would be worse than stale docs — a buffer would keep claiming a
grammar its plugin no longer provides.

*The fixture declares four languages and three of them must fail:* bad
grammar bytes, an uncompilable `folds.scm`, and a squat on `markdown`.
Each must cost only itself, leaving the good one registered and the load
successful. A fixture with only a happy path would prove much less.

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
