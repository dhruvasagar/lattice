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

The one genuinely invasive change. `Lang` is a `Copy` enum matched in
four crates; a runtime-registered language cannot be a variant. The
migration is `Lang::Plugin(LanguageId)` — an interned id — with the
existing variants kept, so every native `match` arm stays valid and the
new arm is one fallthrough per site. Sixteen sites across four files
(measured against `Lang::Lua`).

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
has been parsed yet, because building one needs the tree-sitter CLI
toolchain. So "the runtimes coexist" is settled; "a wasm grammar parses
correctly and fast enough" is LG.1's job, and remains open. The residual
risk is now performance, not stability.

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
