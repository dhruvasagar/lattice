# Plugin-contributed languages

**Status:** design. Slice plan:
[`../operations/slice-plans/plugin-languages.md`](../operations/slice-plans/plugin-languages.md).
First consumer: an org-mode plugin (§7).

## 1. The gap

A plugin can contribute motions, operators, text objects, ex-commands,
modes, keymaps, options, themes, pickers, completion sources, gutter
decorations, sticky context, compilation parsers, `:help` pages and
dashboard sections. It cannot contribute a **language**.

Every grammar lattice supports is a workspace dependency compiled into
the binary, and `Lang` is a closed enum. Adding a language means editing
`lang.rs`, `modes.rs`, `indent.rs` and `format/spec.rs`, shipping its
queries in `lattice-syntax/queries/`, and rebuilding the editor. For an
editor whose second paramount goal is plugin-first extensibility, "which
languages exist" being a compile-time property of the host is the
largest remaining hole in that surface.

This is not a hypothetical want. Org is the forcing case: its grammar is
maintained outside crates.io, its editing model (headline promotion,
TODO cycling, agenda) is a whole subsystem, and bolting all of it into
the host would be exactly the "everything that overlaps an existing
native feature belongs in the crate that owns that domain" inversion
CLAUDE.md warns about — except here the domain does not exist yet, and
should not be the host's.

## 2. The mechanism

**tree-sitter can load a grammar compiled to WebAssembly**, and returns
an ordinary `Language` when it does:

```rust
// tree-sitter 0.26's `wasm` feature, binding_rust/wasm_language.rs
pub fn WasmStore::new(engine: &wasmtime::Engine) -> Result<Self, WasmError>;
pub fn WasmStore::load_language(&mut self, name: &str, bytes: &[u8])
    -> Result<Language, WasmError>;
pub fn Parser::set_wasm_store(&mut self, store: WasmStore) -> Result<(), LanguageError>;
```

`load_language` yields a **real `tree_sitter::Language`**. That single
fact is what makes this design small rather than sprawling: a
wasm-loaded grammar is indistinguishable downstream, so
`HighlightConfiguration`, the folds queries, injections, text-objects,
indents and the incremental reparse path all work **unchanged**. Nothing
in `lattice-syntax` needs to know where a grammar came from.

### 2.1 The host still owns the parse loop

The guest ships the grammar; it does not run it. Parsing stays on the
host's incremental path, at native-ish speed, with the host's existing
snapshot and invalidation model.

This preserves the rejection already recorded in
[`plugin-treesitter-seam.md`](plugin-treesitter-seam.md) §9 — "a
text-only seam (the guest re-parses)… would duplicate the host's live
incremental tree — absurd, slow, and inconsistent with the host's
highlights/folds". That rejection stands. **What changes is only where
the grammar comes from, never who runs it**, and there is no guest call
on the keystroke path at all: the plugin is consulted once, at load.

### 2.2 What a plugin registers

```wit
interface language {
    record language-spec {
        /// `org`. Becomes the language id `:set filetype` and
        /// `:describe-buffer` report, and the key queries are cached under.
        name: string,
        /// `["org"]` — extensions that select this language.
        extensions: list<string>,
        /// The grammar, compiled to wasm (`tree-sitter build --wasm`).
        grammar: list<u8>,
        /// Tree-sitter queries, as source. Absent = that feature is simply
        /// unavailable for this language, never an error.
        highlights: option<string>,
        folds: option<string>,
        injections: option<string>,
        indents: option<string>,
        textobjects: option<string>,
    }

    register-language: func(spec: language-spec) -> result<_, string>;
}
```

Deliberately **data, not callbacks**. A language is a static description;
nothing here needs the guest alive afterwards, so the store is dropped
once registration returns — the same shape, and the same reasoning, as
the `help` seam (CR.3) rather than `dashboard`'s live sections.

### 2.3 `Lang` stops being a closed enum

The one genuinely invasive change. `Lang` is a `Copy + Eq + Hash` enum
matched across several crates; a runtime-registered language cannot be a
variant. The migration is `Lang::Plugin(LanguageName)` — **`Lang`'s
derives constrain the payload**, so an owned `String` here is not an
option. Existing variants stay, so every native `match` arm remains
valid and the new arm is one fallthrough per exhaustive site.

Keeping the variants is deliberate, not conservatism. `comment_syntax`,
`major_mode_id_for_lang` and `FormatSpec::for_lang` get compiler-checked
coverage of every bundled language today; collapsing `Lang` to a bare
newtype over a name would remove all five matches at the cost of letting
a newly-bundled language silently miss its formatter. The enum earns its
keep.

**Size, measured (2026-08-23, LG.2).** An early draft said "sixteen sites
across four files", counting mentions of `Lang::Lua` — one language,
which was the wrong measurement. A later pass gave "4 in `lattice-syntax`
alone" and flagged itself as a lower bound, because cargo stops at the
first failing crate. Fixing the layers one at a time, which is what LG.2
did, gives the **true total: 5 exhaustive matches across 3 files.**

| Site | What the plugin arm does |
|---|---|
| `lang.rs` `label()` | the interned name |
| `lang.rs` `name()` | the interned name |
| `lang.rs` `comment_syntax()` | `None` — LG.3 lets a plugin declare it |
| `modes.rs` `major_mode_id_for_lang()` | `None` — the plugin's own `modes` seam owns it |
| `lattice-format/spec.rs` `for_lang()` | `None` — `formatprg` still applies |

Nothing else in the workspace needed an arm. `indent.rs` and the host's
call sites resolve by name through the registry rather than matching, so
they were already provenance-agnostic.

#### Why the payload is a name, not an index

The design originally specified `LanguageId` as a `Copy` newtype over an
**intern index**. LG.2 changed it to a `Copy` newtype over an interned
`&'static str`, and the reason is a measurement rather than a taste:

`Lang::name()` **is the registry lookup key**, called six times per
highlight invocation (`registry.highlights_query(self.lang.name())`) plus
once each on the folds and indents paths. An index would have put a
process-global table read inside a function that is `&'static str`-pure
today — paramount goal #1, for no gain. And `LangRegistry` is *already*
`HashMap<&'static str, LangConfig>`, so a name-keyed plugin language
joins the map bundled languages live in instead of needing a parallel
index space.

The cost is one leaked string per **distinct** name; `LanguageName::intern`
dedupes, so a plugin reloaded fifty times in a dev session leaks one, not
fifty. Leaking a `&'static str` for a runtime-supplied name has precedent
in-tree — `FormatSpec` does it for user `formatprg` strings.

A useful consequence: a buffer still holding `Lang::Plugin(name)` after
its plugin unloads keeps naming itself correctly. It finds no grammar and
renders as plain text. Nothing dangles, and no kind-branch is needed to
express it.

#### Resolution is process-global, deliberately

Registration and teardown go through an RCU handle with
teardown-by-provenance, exactly as
[`contributable-registries.md`](contributable-registries.md) §2
prescribes. **Reads do not take a handle.** `Lang::detect_from_path` is a
free function with nineteen call sites across `lattice-host`,
`lattice-magit` and `lattice-multibuffer`; threading a handle through
them would make plugin languages visible on some paths and invisible on
others — a two-tier language concept, and the same failure the
"no kind-specific logic" rule forbids for buffers. `Lang::Plugin` has to
be interpretable wherever `Lang::Rust` is, by the same code.
`LangRegistry::standard` is already a process-wide memo in this crate for
a related reason.

The registry is consulted **only after every native arm**, so a plugin
cannot shadow a bundled language by accident — a plugin claiming `rs`
simply never wins. Registering a bundled *name* is refused outright
(`ShadowsBuiltin`), since that would collide in the config map. Whether
deliberate override should be possible is a DB.8-shaped question,
deferred until someone asks.

An empty registry costs one relaxed atomic load and never touches the
`ArcSwap` — `detect_from_path` runs per hunk in magit's diff
highlighting, so the no-plugins case had to be free rather than merely
cheap. Measured in [`benchmarks.md`](../operations/benchmarks.md).

The encouraging half, confirmed: most of the ~150 `Lang::` occurrences
across the tree are *constructions* or `matches!` guards, not exhaustive
matches, so the arm-adding was far narrower than the raw grep suggested.

### 2.4 The registry is live, not a second map

A plugin language needs a `LangConfig` — a compiled grammar plus its
queries — and the obvious place to put one is a second map consulted when
`Lang::Plugin` matches. **That would be a kind-branch in every accessor**
(`highlights_query`, `folds_query`, `indents_query`, `tree_sitter_language`
and four more), which is exactly what the "no kind-specific logic" rule
forbids for buffers and forbids here for the same reason.

So LG.3a made `LangRegistry` itself the RCU value:

- `configs` holds `Arc<LangConfig>`, which makes `LangRegistry: Clone`
  cheap — one refcount bump per language plus a small map. Compiling the
  bundled set costs ~1.2 s and must never be repeated because a plugin
  registered a twentieth language; registration clones the *map*, not the
  queries.
- Each config carries `provenance: Option<u64>` — `None` for bundled.
  Teardown is `retain(|c| c.provenance != Some(id))`, by provenance rather
  than by a token list a caller has to remember to keep.
- `LangRegistry::standard()` returns the **live snapshot**. Every existing
  lookup (`registry.highlights_query(lang.name())`) therefore finds a
  plugin language exactly as it finds `rust`, with no call-site change
  anywhere in the tree.

A snapshot is still immutable under its holder; what changed is that
*which* snapshot you get can differ between calls. A registration mid-read
affects the next lookup, not half of this one — the coherence property
[`contributable-registries.md`](contributable-registries.md) §2 names.

**Registration is atomic across the two registries.** Identity (§2.3) and
grammar live in different places, and either half alone is a broken state:
identity without a config resolves to a language that cannot parse, a
config without identity is a grammar nothing can select. So the queries
are compiled first, the name is claimed second, and the compiled config is
installed only once both have succeeded. A malformed `folds.scm` leaves
the language absent rather than resolvable-but-dead, and a name collision
leaves the winner's grammar untouched.

**Queries compile at registration, not first use.** A typo in a plugin's
`folds.scm` surfaces at load with the offending query named. Compiling
lazily would turn it into "folding silently does nothing in org files",
which is indistinguishable from the feature not existing and surfaces days
later. An *absent* query is not an error — it means that feature is simply
unavailable for that language.

Cost measured in [`benchmarks.md`](../operations/benchmarks.md): a
snapshot is 13.9 ns, against a per-buffer and per-hunk call rate.

## 3. The risk that decides this

**Two wasmtime majors in one binary.** tree-sitter's `wasm` feature
depends on `wasmtime-c-api 36`; the plugin host is on `wasmtime 46`.
They are different crates, so cargo will happily link both — and that is
the problem, not the reassurance:

- **Two JIT runtimes, two sets of signal handlers.** wasmtime installs
  `SIGSEGV`/`SIGBUS` handlers for guard-page traps. Two independently
  initialised runtimes both installing them is a genuine hazard, and the
  failure mode is a mysterious crash under load months later, not a
  compile error now.
- **Binary size.** Two full Cranelift copies.
- **Divergence.** The two versions drift on their own schedules.

**This is a gate, not a footnote.** LG.0 exists to answer it before
anything is built on top. Building the seam first and discovering this
later would be the expensive order.

### 3.1 Outcome (LG.0, 2026-08-22): they coexist

**Answered: the two runtimes coexist.** `crates/lattice-plugin-host/tests/two_wasmtime_runtimes.rs`,
behind `--features lg0-wasm-grammar-spike`.

- Both link. `wasmtime 46` and `wasmtime-c-api 36` (with its own
  Cranelift) compile into one test binary.
- **A real guest trap is still caught with tree-sitter's runtime live.**
  The `grammar-guest` fixture's `traps` motion executes `unreachable`;
  the host still classifies it, trips the quarantine, and returns
  `CommandError::Plugin` rather than aborting the process.
- **Both initialisation orders.** Signal-handler registration is
  order-dependent, so it is tested tree-sitter-first AND host-first —
  the two ways a real session can bring them up (a plugin loading before
  any file is opened, or after).

The tests stay. A wasmtime bump on either side re-opens the question,
and this is what re-answers it.

**What LG.0 did NOT answer**, and is honest to name: no wasm *grammar*
had been parsed at that point, so "the runtimes coexist" was settled
while "a wasm grammar parses correctly and fast enough" was not. That
was LG.1's job, and §4.1 closes it — a wasm grammar loads, parses
identically to the native one, and costs 2.0× cold / 1.25×
incremental. (The belief that building one needs the tree-sitter CLI
toolchain was also wrong; see §4.2.)

**The cost stands regardless.** Two Cranelift copies is real binary
size, and the eventual product build must decide whether the `wasm`
feature is always on or gated behind a cargo feature the way the GPUI
peer is. Deferred to LG.3; noted here so it is not forgotten.

## 4. Performance

A wasm grammar parses slower than a native one — the figure quoted in
the ecosystem is roughly 2–5×. Parsing is off the keystroke path (the
reparse is async, and the renderer never blocks on it), so this does not
hit the keystroke→glyph ratchet directly. But it is not free:

- a large org file's first parse is user-visible as "highlighting
  catches up", which the UX contract permits as eventual consistency but
  which has a limit;
- the incremental reparse budget in `benchmarks.md` is a recorded number
  and this would move it for plugin languages.

So LG.1 lands a bench comparing native vs wasm parse on the same
grammar, and the number goes in `benchmarks.md` **before** the seam is
declared usable. If wasm parsing turns out to be materially worse than
the ecosystem figure, that is a finding, and §6's alternative exists.

### 4.1 Outcome (LG.1, 2026-08-23): 2.0× cold, 1.25× incremental

**Answered, and at the good end of the expected band.** The same
`tree-sitter-md` grammar, loaded natively and from wasm, over the same
input (`crates/lattice-syntax/benches/wasm_vs_native_parse.rs`):

| Corpus | Cold | Incremental reparse |
|---|---|---|
| 16 §§ (3.5 KB) | 2.04× | 1.24× |
| 128 §§ (29 KB) | 2.10× | 1.26× |
| 512 §§ (118 KB) | 2.00× | 1.24× |

Full numbers in [`benchmarks.md`](../operations/benchmarks.md).

Two things matter more than the headline. **The cold ratio is flat at
2.0× across two orders of magnitude** — it does not degrade with file
size, which is what would have made large org files a problem.
And **the incremental reparse is only 1.25×**, because reused subtrees
are manipulated by host-side C on both paths; only the newly-lexed
region runs guest code. The reparse is the path a user waits behind
repeatedly; the 2× is paid once, on open, where the UX contract already
permits "highlighting catches up".

**The residual risk named in §3.1 is therefore closed.** Nothing now
sends this track to §6. What remains open is ordinary implementation:
LG.2's `Lang::Plugin` migration and LG.3's seam.

**Honest scope.** These are parse numbers on one grammar. Parsing is off
the keystroke path, so none of it touches the keystroke→glyph ratchet.
A grammar with a much heavier external scanner would shift the guest/host
mix, in either direction.

### 4.2 The toolchain question, dissolved

The build step was expected to be the awkward part: `tree-sitter build
--wasm` needs emscripten (~1 GB) or a docker daemon, which would make one
of them a prerequisite for contributors, or push grammar-building onto
plugin authors' machines only. It turns out **neither is needed**, and
the reason is worth recording because it is not obvious from
tree-sitter's own documentation.

Reading `tree-sitter/src/wasm_store.c`, a grammar module must be a plain
**wasm side module** — a `dylink.0` custom section, which `wasm-ld
-shared` emits natively; nothing about the format is emscripten-specific.
The store itself supplies `memory`, `__stack_pointer`, `__memory_base`,
`__table_base` and `__indirect_function_table`, and ships a **prebuilt
wasm libc** exporting the 24 symbols listed in
`src/wasm/stdlib-symbols.txt`. So the grammar carries no libc of its own
and needs only *declarations* for the handful of functions it calls.

`scripts/build-wasm-grammar.sh` is that, in full: ~60 lines of generated
headers, clang targeting `wasm32-unknown-unknown`, and `rust-lld` — which
every rustup toolchain already ships, and which dispatches to its wasm
driver when invoked as `wasm-ld`. **The prerequisite is clang + rustup**,
both of which a contributor building this repo already has.

One flag is worth naming because it cost a debugging cycle:
`--Bsymbolic`. Without it the external-scanner entry points stay
preemptible, so LLD emits `GOT.func.tree_sitter_<name>_external_scanner_*`
imports — and the store resolves only its builtins and its libc, so
instantiation fails with `invalid import`. The symbols are defined *in
the module*; the failure is purely about symbol binding, and the error
message points nowhere near the cause.

This does not preclude plugin authors using `tree-sitter build --wasm`;
that stays the documented route for them (LG.6). It means **the repo's
own bench and tests do not depend on it**, and that a plugin author
without emscripten has a supported path.

## 5. Paramount-goal alignment

- **#1 Performance.** No guest call on any hot path — registration is
  once, at load. Parsing stays host-side and incremental. The wasm-parse
  cost is real and gated by a bench rather than assumed.
- **#2 Extensibility.** The point. "Which languages exist" stops being a
  compile-time property of the editor. Every future language can ship as
  a plugin instead of a workspace dependency, and the ones already
  bundled could migrate.
- **#3 Modal editing.** Untouched. A plugin language's text objects and
  motions flow through the same grammar the builtins use.
- **#4 Asynchronicity.** Registration happens on the loader's
  off-boot-thread task; a language appearing mid-session is the same
  eventual-consistency the loader already has.

**UX (higher court).** A file open before its language registers renders
unhighlighted and then gains colour — the eventual-consistency the
contract allows for syntax, and the same behaviour a slow first parse
already produces. It must not reflow or move the cursor.

**Heuristic #6 (crate boundary).** No new crate. The seam lives in
`lattice-plugin-host` (host side) and `lattice-syntax` (the registry
that already owns grammars). `lattice-syntax` gains the `wasm` feature
and the runtime registry; that is a new *mechanism* in the crate that
already owns the domain, which is the test.

## 6. Rejected alternatives

- **Guest re-parses and returns spans.** Already rejected in
  `plugin-treesitter-seam.md` §9, and rejected again here for the same
  reasons plus one more: it would put a guest call on the reparse path.
- **Bundle org natively; plugin ships only behaviour.** Cheaper, and the
  honest fallback if §3's gate fails. Rejected as the primary because it
  leaves the paramount-#2 hole exactly where it is — the next language
  asks the same question again.
- **Plugin ships a native dynamic library.** How several editors do it.
  Rejected outright: loading native code into the process defeats the
  sandbox that is the whole basis of the plugin model (capabilities,
  fuel, crash isolation). A plugin that can `dlopen` is not sandboxed.
- **Grammar by URL, fetched at install.** Orthogonal — the plugin
  manager already resolves and builds sources (PM.5–PM.8), so a grammar
  is just another build artefact. Worth doing later; not part of the
  seam.

## 7. First consumer: org

Org exercises the seam properly rather than thinly, which is what
`plugin-host.md` §12 asks of a new WIT surface:

- **A grammar not on crates.io.** `nvim-orgmode/tree-sitter-org` is the
  maintained fork; the crates.io `tree-sitter-org` (milisims, last
  published October 2022) is its stale ancestor, and its Rust binding
  pins `tree-sitter >= 0.19, < 0.21` against our 0.26 — so it cannot be
  a workspace dependency at all without vendoring. A plugin sidesteps
  that entirely: the grammar is the plugin's build artefact, not ours.
- **Per-level headlines the hard way.** Org's `stars` is ONE node whose
  *text length* is the level, unlike markdown's distinct
  `atx_h1_marker`…`atx_h6_marker`. Per-level capture therefore needs
  `#eq?` text predicates — and **no query in the tree uses one today**,
  so the seam's first consumer also proves predicate support.
- **Variable-font headlines come free.** `heading_scale_split` is not
  markdown-gated: it finds the first run with a resolved `scale > 1.0`,
  and `Style::HeadingN → syntax.heading.N → theme scale` is generic. An
  org query emitting `@text.title.1`…`.6` gets the same two-piece scaled
  rendering — base-size stars, scaled title, one baseline — with **zero
  renderer changes**. This is a good sign about the existing design, and
  worth pinning with a test so it stays true.
- **Folding is queries only.** `(section)`, `(block)`, `(drawer)`,
  `(list)` map onto the same fold pipeline markdown's `(section)` uses.

Org's *editing* model — headline promotion, subtree motion, TODO
cycling, visibility cycling, agenda — is a separate track that uses
seams which already exist (`modes`, `grammar`, `keymap`), and is not
gated on this one.

## 8. Cross-references

- [`plugin-treesitter-seam.md`](plugin-treesitter-seam.md) — the
  read-side seam, whose §9 rejection this design preserves.
- [`plugin-host.md`](plugin-host.md) §5 (seam → registry drain), §12
  (WIT unstable until three real plugins).
- [`contributable-registries.md`](contributable-registries.md) — the
  RCU-handle + teardown-by-provenance pattern the language registry
  follows.
- `crates/lattice-syntax/src/registry.rs` — the `HighlightConfiguration`
  registry a plugin language joins.
