# Org-mode as a plugin — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contracts live in
> [`../../architecture/org-mode.md`](../../architecture/org-mode.md) —
> the mode decomposition, the keymap rationale, the agenda seam shape,
> rejected alternatives, paramount-goal alignment.
> The ledger entry is
> [`../implementation.md`](../implementation.md) §"Org-mode as a plugin":
> the four generic host changes, the acid test, and the findings worth
> carrying past org.
>
> **Not archivable.** OM.6b and OM.11 are open work — 📝 rather than ⛔
> since 2026-08-26, when XF shipped the cross-file write primitive they
> were blocked on. A planned slice is still open work, so this plan stays
> active until both go green (the archiving rule in CLAUDE.md).

**Status:** OM.A3 ✅ (2026-08-25) — **the agenda is done.** OM.A1 the
seam, OM.A2 org's semantics, OM.A3 the view's two modes. Only OM.14
(docs / ledger / site) and the two ⛔ cross-file-write slices remain.

Earlier: OM.13 ✅ (2026-08-25) — **the outliner and tables are done.**
OM.8 ✅ checkboxes + cookies; OM.9 ✅ timestamps; OM.10 ✅ links;
OM.12 ✅ `org-table-mode`; OM.13 ✅ rows and columns. OM.11 and OM.6b were
blocked on the same cross-file write primitive; **XF shipped it on
2026-08-26** and both are now ordinary plugin work. The agenda group
(OM.A1–A3) is done.

Earlier: OM.7 ✅ tasks, in a second mode. OM.0 ✅ seam drain order is
structural (the gate found a real bug rather than confirming the status quo);
OM.1 ✅ the registry language index; OM.2 ✅ majors cross the `modes` seam;
OM.2b ✅ `<leader>` (carved mid-build — it did not exist); OM.3 ✅
promote/demote; OM.4 ✅ headline motions; OM.4b ✅ `dar` / `d]]` reach the
plugin's grammar; OM.5 ✅ `<Tab>` routing; OM.6 ✅ subtree move / meta-return /
toggle heading — **archive carved out as OM.6b (unblocked by XF)**; OM.7 ✅
`org-todo-mode` (TODO / priority / tags). Next: OM.8, checkboxes + cookies.

**From OM.6 on the plugin lives in its own repo**
([`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin)),
moved out of `examples/org-plugin` by 7ba51c7. Slices from here land as
commits THERE and touch lattice only when a seam has to change — OM.6 did
not.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

## Where this sits

Phase **8b** (bundled / reference plugins) — `implementation.md` already
names [`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin) as its second reference plugin alongside
`auto-pair` and `treesitter-context`.

**Except OM.0–OM.2, which retire a Phase-8 deferral.** The
*"majors are Phase 8"* text lives in three places and each must be
corrected as the slices land, not left to drift:

- `crates/lattice-plugin-host/src/mode_host.rs` (the reject arm + its
  test `a_major_kind_is_rejected_in_phase_7`),
- `wit/modes.wit` (header comment + the `mode-kind` doc),
- `slice-plans/plugin-loader.md` PL8.G and
  `slice-plans/plugin-host.md` PH7.11's deferral note.

## Sequencing

```
OM.0  drain-order gate  ← cheap, blocks everything below
  │
OM.1  ModeRegistry language index (native only, no WASM)
  │
OM.2  modes seam accepts `major` + target-language   ← org-mode activates
  │
OM.2b `<leader>` expansion (carved mid-build — it did not exist)
  │
OM.4b plugin grammar contributions reach operator-pending
      (carved mid-build: motions AND text objects need one mechanism)
  │
  ├── outliner ──────────────────────────────────────────────┐
  │   OM.3   promote / demote (headline + subtree)            │
  │   OM.4   headline motions + ih/ah/is/as text objects      │
  │   OM.5   <Tab>/<S-Tab> routing + the decline chain        │
  │   OM.6   subtree move, meta-return, toggle heading         │
  │   OM.6b  archive subtree  📝 unblocked by XF                │
  │   OM.7   org-todo-mode: TODO cycling, priority, tags      │
  │   OM.8   checkboxes + statistics cookies                  │
  │   OM.9   timestamps                                       │
  │   OM.10  links                                            │
  │   OM.11  refile (own picker-source) + capture             │
  │                                                            │
  ├── tables ────────────────────────────────────────────────┤
  │   OM.12  org-table-mode: align + cell motion              │
  │   OM.13  row / column move + insert                       │
  │                                                            │
  └── agenda ────────────────────────────────────────────────┘
      OM.A1  agenda seam WIT + host provider, trivial guest
      OM.A2  org agenda semantics in the guest
      OM.A3  org-agenda-mode: act from agenda, gr, headerline
  │
OM.14  docs, ledger, site nav
```

**Why the agenda is last despite being the marquee feature.** Nothing in
the outliner or tables depends on it, and it is the only slice group
adding a WIT interface. Landing it after the rest means the ABI addition
is made **once**, informed by what org turned out to need, rather than
guessed at OM.3 and amended twice. The design risk that would have
justified fronting it — can multibuffer express date grouping across
files? — was retired during design by reading the excerpt model
(fragment §6.1), so there is no gate left to fail early.

| Slice | Description | Status |
|---|---|---|
| OM.0 | Gate: a plugin's `grammar` seam drains before its `modes` seam | ✅ |
| OM.1 | `ModeRegistry` language index + `Mode::target_language` | ✅ |
| OM.2 | `modes` seam accepts `major`; `target-language` field | ✅ |
| OM.2b | `<leader>` bind-time expansion + `keymap.leader` | ✅ |
| OM.3 | Promote / demote headline + subtree | ✅ |
| OM.4 | Headline motions (`]]` / `[[` / `g{`) | ✅ |
| OM.4b | Plugin motions + text objects reach operator-pending, in the mode's layer | ✅ |
| OM.5 | `<Tab>` / `<S-Tab>` routing + the decline chain | ✅ |
| OM.6 | Subtree move, meta-return, toggle heading | ✅ |
| OM.6b.0 | `document.path()`: a guest learns which file it is editing | ✅ |
| OM.6b | Archive subtree to `<file>_archive` | 🚧 |
| OM.7 | `org-todo-mode`: TODO cycling, priority, tags | ✅ |
| OM.8 | Checkboxes + statistics cookies | ✅ |
| OM.9 | Timestamps (`<C-a>` / `<C-x>`) | ✅ |
| OM.10 | Links: open at point | ✅ |
| OM.11 | Refile + capture | 📝 unblocked by XF |
| OM.12 | `org-table-mode`: align + cell motion | ✅ |
| OM.13 | Row / column move + insert | ✅ |
| OM.A1 | `agenda-source` seam + host provider | ✅ |
| OM.A2 | Org agenda semantics in the guest | ✅ |
| OM.A3 | `org-agenda-mode`: act from agenda, `gr`, headerline | ✅ |
| OM.14 | Docs, ledger, site nav | ✅ |

Every slice ships four artefacts (CLAUDE.md heuristic #5): doc, bench
where a hot path is touched, tests covering the failure mode as well as
the happy path, graceful error handling. One slice, one commit,
committed as it goes green, `scripts/precommit.sh <crate>` before each.

---

## The prerequisite slices

### OM.0 — the drain-order gate ✅ (2026-08-24)

`mode-keymap-binding` resolves `command` against the `CommandRegistry`
**at registration**. Org binds `<leader>ol` → `action:org-demote`, and
org registers that action itself through the `grammar` seam. If the
loader drains `modes` before `grammar` for a single plugin, every org
binding skips — logged, but invisible to the user.

- Read the loader's per-plugin drain order; assert it with a test.
- If the order is wrong, fixing it **is** this slice.
- *paramount:* #2 — a silently-empty keymap is the extensibility failure
  that looks like a plugin bug.
- *test:* a multi-seam fixture whose mode binds a chord to its own
  grammar-registered action; assert the binding resolves after load.
- *doc:* fragment §3.4 records the answer either way.

**This is a gate, not a slice.** It can come back "already correct", and
that is a result worth pinning with a test so a loader refactor cannot
silently reverse it.

**Outcome: the order was NOT guaranteed.** The drain loop walked
`manifest.provides` verbatim (`lattice-plugin-loader/src/lib.rs`), so the
order came from a **guest-authored TOML file** — and both bundled
multi-seam manifests carried a hand-written comment telling the next
author to put `grammar` before `modes`. A load-bearing invariant
enforced by prose inside guest input is enforced by discipline, which is
what the codebase's own rules forbid.

Fixed structurally rather than by validation: `PluginSeam::drain_rank()`
ranks the seams by their real registration dependencies and the loader
stable-sorts before draining, so manifest order becomes cosmetic. Ties
keep the author's ordering. Rejected: rejecting a badly-ordered manifest
at load — it still makes correctness the plugin author's job, and the
author cannot see the dependency from where they are standing.

Ranks: `config`/`theme`/`logging` → `language` → `grammar` → `modes` →
`keymap` → everything else. Recorded in `drain_rank`'s own doc comment,
which is where a new `PluginSeam` variant's author will meet it.

Tests: `lattice-plugin-loader/tests/seam_drain_order.rs` (3) loads the
`multiseam-guest` fixture — whose mode binds Normal `x` to its **own**
grammar action — under three `provides` permutations. Note what the
regression test asserts on the way past: the mode itself registers in
every order, and only its *binding* silently vanishes. That is what made
the bug invisible. Plus 3 unit tests on `drain_rank` in `manifest.rs`
pinning the ordering itself, so a new seam variant cannot be added
without deciding where it drains.

The two bundled manifests' ordering warnings are now false and were
rewritten to say what is actually true.

### OM.1 — the language index ✅ (2026-08-24)

Native only; no WASM, no org.

- `Mode::target_language() -> Option<String>`, defaulting to `None`,
  beside the existing `target_buffer_kind`.
- `ModeRegistry` indexes it at register-time; `find_major_for_lang`
  beside `find_major_for_kind`.
- `resolve_major_mode` consults the index for `Document` buffers before
  falling through to `text-mode`.
- *test:* a test-only native major declaring `target_language =
  "org"`; a `Lang::Plugin("org")` buffer activates it. Failure modes:
  two majors claiming one language (last-registered loses, logged), a
  language nothing claims (falls to `text-mode` as today).
- *doc:* fragment §3.2, including the note that native language majors
  can migrate onto this later and that the migration is **not** in
  scope.
- *no bench:* registration and activation are off the keystroke path.

**Landed 2026-08-24.** `Mode::target_language() -> Option<&str>` (a
`&str`, not `Option<&'static str>` like `mirrors_option`, because a
plugin mode's language name is an owned runtime field — borrowing from
`self` serves both without allocating). `ModeRegistry` gained
`lang_index: HashMap<String, ModeId>`, keyed by name because a plugin
language's identity IS its name; the host has no enum arm for it.
Populated in `register_inner`, first-claim-wins with a `warn` on the
second, freed in `unregister` — every rule `kind_index` already had.

**One deliberate divergence from `kind_index`:** a claim from a MINOR is
refused and warned rather than indexed. H.2 chose to respect whatever a
mode declares, but a minor claiming a *language* is reachable from
plugin input in a way a mis-declared kind never was, and indexing it
would install the minor AS the buffer's major.

`resolve_major_mode` gained a third layer between the kind index and
`text-mode`. The built-in table is consulted **first**, so a plugin
claiming `"rust"` cannot take `rust-mode`'s language — tested. The
lookup passes `lang.name()` unconditionally rather than matching on
`Lang::Plugin`, since `name()` is the canonical registry key for every
arm and IS the interned identity for a plugin one; no plugin-specific
branch exists in the resolver.

Tests: 6 in `registry.rs` (resolve, unclaimed, first-wins, minor
refused, unregister frees + reclaim, kind/lang independence) and 5 in
`lattice-host/src/modes.rs` (the headline plugin-language resolve; the
graceful fallback when the `language` seam loads without a major; the
built-in table winning; kind still beating language; a minor not
becoming the major). Native only — no WASM, no org.

### OM.2 — majors over the seam ✅ (2026-08-24)

- `mode_host.rs` accepts `major`; delete the reject arm and rewrite
  `a_major_kind_is_rejected_in_phase_7` into its positive counterpart.
- `mode-declaration` gains `target-language: option<string>`.
- Correct the three deferral sites listed under "Where this sits".
- The org plugin declares `org-mode` with an **empty keymap** — the
  point of this slice is activation, not behaviour.
- *test (the headline assertion, and the one that fails today):*
  opening a `.org` file activates `org-mode` as the major. On the
  `emacs_keys_as_component.rs` harness — boot a real `Editor`, wire a
  tempdir loader to its live registries, load the component. Failure
  modes: a major with no `target-language` is manual-only; a major
  whose id lacks the `-mode` suffix is still rejected.
- *doc:* fragment §3.1, §3.3.

**Landed 2026-08-24.** `mode-kind::major` is accepted;
`mode-declaration.target-language` crosses as `option<string>`. `PluginMode`
carries its kind and language instead of hard-coding `ModeKind::Minor`.

**Two things the slice found that the plan had not listed**, both of which
would have been silent:

1. **`bind_mode_keymap` hard-coded `KeymapLayer::MinorMode(id)`.** A major's
   chords would have landed in a minor layer under its own name — resolving,
   but at the wrong priority, so a minor could no longer refine its major.
   The layer now follows the kind. `KeymapCapability::OwnedLayer` was extended
   to authorise `MajorMode(id)` as well: the capability names a mode and a mode
   has one layer, so scoping it to minors would have meant giving a plugin
   major a *broader* capability to do a narrower thing.
2. **Teardown removed only `MinorMode(id)`.** An unloaded plugin major would
   have leaked its keymap layer. Teardown now reads the mode's kind BEFORE
   unregistering, since afterwards the registry no longer knows.

A `target-language` on a MINOR is dropped with a warning at both layers — in
`register_plugin_mode` (so the message names the plugin's mode) and again in
the registry (so no other caller can bypass it).

The org plugin now composes a third world (`modes-plugin`) and declares
`org-mode` with `target_language: Some("org")` and a deliberately **empty
keymap** — this slice is about a plugin language having a major at all.

Tests: 4 unit in `mode_host.rs` (major registers as a major and claims its
language; a major with no language claims nothing; a minor's claim is dropped
while the mode still registers; a major's keymap lands in its own gated
`MajorMode` layer), 1 new e2e in `mode_source.rs` through a real guest, and
`lattice-host/tests/org_major_mode.rs` (2) which walks the whole path with the
REAL reference plugin: discover → load → `language` + `modes` drain → `:e
notes.org` → the editor's generic activation resolves `org-mode`. Its peer
asserts the degradation: a language-only install opens org files in
`text-mode`, highlighted and foldable, which is a good outcome rather than an
error. The `modes-guest` fixture gained a major and a language-greedy minor so
the seam's own suite covers both without needing the out-of-workspace org
build.

**Acid test:** zero `Editor::` methods, zero host `Action` variants, no
`BufferKind::Org`, no `Lang` arm. `org_major_mode.rs` asserts the behaviour;
the absence is visible in the diff.

The three "majors are Phase 8" deferral sites were corrected rather than left
to drift: `mode_host.rs`'s reject arm (and its test, rewritten into its
positive counterpart), `wit/modes.wit`'s header and `mode-kind` doc, and the
two bullets in `plugin-host.md`.

### OM.2b — `<leader>` expansion ✅ (2026-08-24)

**Carved mid-build, because the approved keymap could not have worked.**
Every org chord is `<leader>o…`, and probing the parser directly gave:

```
"<leader>oh" => Err(UnknownName { name: "leader", at: 0 })
"<Space>oh"  => Ok(3)
```

`<leader>` does not exist in lattice. It appears in
`keymap-architecture.md:352` as an aspirational example and was never
built. Every org binding would have been skipped with a warning: the
plugin loads, `org-mode` activates, the chords do nothing — the same
silent shape OM.0 fixed, reached from a different direction.

Decided with Dhruva: **bind-time expansion**, not a live-rebindable
layer. `keymap.leader` (default `<Space>`) is read once at boot and
`<leader>` is expanded in `try_bind_chord_string` — the single choke
point every plugin mode, `register-binding` and init.rs binding funnels
through. Rejected: hardcoding `<Space>o…` in org, which would take from
the user the exact choice leader exists to give them and need migrating
later; and full live re-expansion, which means layers must remember they
were registered with a leader and re-push on change (the
`emacs-keys-prefix` shape, worth doing when someone wants `:set`).

`<Space>` rather than vim's `\` — vim's default is an artifact of which
keys were free in 1991, and nvim-orgmode's documented bindings assume
space. The standing "UX follows convention" rule.

**Consequences stated rather than hidden:** expansion is bind-time, so a
`:set keymap.leader` after boot does not move bindings that already
landed; `set_leader` is therefore called FIRST in the keymap boot block,
not merely early. `try_unbind_chord_string` expands too, or a plugin
could bind `<leader>x` and never reverse it on unload.

Tests: 5 in `lattice-keymap` (both spellings expand anywhere in a
sequence including twice; a bound leader chord resolves under the
expanded form; `set_leader` changes later bindings; unbind-by-the-same-
string round-trips; a malformed leader degrades to `InvalidChord`
per-binding rather than panicking). Plus a drift pin in `lattice-host`
holding `keymap.leader`'s default equal to `DEFAULT_LEADER` — the
literal is duplicated because `lattice-config` does not depend on
`lattice-keymap` and should not gain the dependency for a default
string.

## The outliner

Each slice: chord → plugin action → `Effect::Edits`, dispatched through
a real `Editor`, with the failure path tested (malformed headline,
cursor outside any headline, missing target).

### OM.3 — promote / demote 📝
`<leader>oh` / `ol` / `oH` / `oL`. First slice where the plugin edits.
Exit: all four work through real chord dispatch, and demoting past the
theme's six-level ramp keeps level-6 styling without error.
*bench:* the action round-trip, against the `< 5 µs p99` grammar gate.

### OM.4 — headline motions ✅ (2026-08-24)
`]]` / `[[` / `g{`, kept verbatim from nvim-orgmode: `]` and `[` are trie
prefixes rather than terminal bindings, so unlike `>>` / `<<` these
transplant unchanged.

**The seam needed extending, as its own doc-comment predicted.**
`apply-motion` took no `document` handle, so a motion could not read the
buffer — and finding the next headline is reading lines. `grammar.wit`
already named this: *"text-reading motions (structural / word motions) can
reuse the same handle when a motion signature needs it; AP.0.1 wires the
action path only."* `apply-motion` now takes `borrow<document>`, minted in
`build_motion_spec` exactly as `build_action_spec` does (the native
`MotionContext` already carries `buffer`, so no native context changed).
No tree: a `MotionContext` carries a `ScopeResolver`, not a
`SyntaxSnapshot`, so there is nothing to mint — the slice that needs one
adds it. Every grammar-world guest's signature updated (grammar,
multiseam, auto-pair, treesitter-context, org).

*Motions, not actions,* so they compose with operators and take counts —
which is the whole distinction (paramount #3). `jump: true` (a headline
jump is somewhere `<C-o>` should return from) and `exclusive: true` (`d]]`
deletes up to but not including the next headline). A motion at the edge
resolves to the cursor's own line rather than erroring: an `err` is logged
and no-ops, which is right for a broken motion and wrong for `]]` at the
last one — `}` at the end of a buffer stays put.

*Why `g{` is not just `[[`:* from the second of two level-3 siblings, `[[`
reaches the sibling; `g{` must skip it to the level-2 parent. Pinned by
`parent_skips_siblings_where_prev_headline_would_not`.

**Two gaps found while testing, carried to OM.4b rather than glossed:**

* **`d]]` does not work.** Operator+motion paths are bound explicitly at
  `KeymapLayer::Builtin` from a hardcoded `motion_rows` table
  (`keymap_normal.rs:1467`), so a plugin motion bound in Normal is not
  reachable after an operator. This is the SAME gap that blocks plugin
  text objects, with the same fix.
* **Counts are not covered here.** A control assertion showed `3j` on a
  NATIVE motion also resolving as one step through this harness — count
  accumulation lives at the App layer, above `Editor::dispatch_chord`. The
  gap is the harness, not the plugin; covering it means driving
  `lattice-ui-tui`'s `press`.

Tests: 13 unit in the plugin (motions walk every level; parent skips
siblings; parent is `None` outside any headline) and 11 in
`org_structure.rs`, 3 of them new for motions.

### OM.4b — plugin grammar reaches operator-pending ✅ (2026-08-24)

**Carved mid-build at OM.4.** Two separate-looking gaps turned out to be
one mechanism:

* A plugin **text object** can be registered (`register-text-object`
  lands it in the `CommandRegistry`, `apply-text-object` works) but no
  chord ever reaches it. Rows come from the hardcoded `text_object_rows`
  table, expanded across every operator × `i`/`a` into
  `KeymapLayer::Builtin`.
* A plugin **motion** has exactly the same problem after an operator, from
  the `motion_rows` table.

The workaround is not merely ugly, it is inexpressible:
`mode-keymap-binding` maps a chord to a command *name*, while both cases
need `CommandInvocation::of(operator).with_target(Target::{Motion,TextObject}(id))`
— an operator *plus* a target, which the WIT cannot say.

And `Builtin` would be the wrong layer regardless: org's objects must
exist only in org buffers, so the rows belong in `MajorMode(org-mode)`.

- Contributions gain a chord; the host generates the operator × `i`/`a` ×
  chord rows into the **mode's own layer**, not `Builtin`.
- *chords:* `ih` / `ah` headline, `ir` / `ar` subtree — **not** `is`/`as`,
  which the design fragment originally said: `s` is already vim's
  *sentence* object, and nvim-orgmode itself uses `ir`/`ar`. Following the
  convention we already chose fixes the collision.
- *exit:* `das` deletes a subtree, `yah` yanks a headline, and `d]]`
  deletes to the next headline — all through the ORDINARY operators, with
  no org-specific chord.
- *also here:* the count coverage OM.4 could not reach, at the App layer.

**Home decided with Dhruva (2026-08-24): a host-side post-load pass.**
`lattice-host` walks a newly-registered plugin mode's bindings, finds the
ones naming a motion or text object, and expands them into operator rows
in that mode's own layer using its own operator table. The table stays
put, and the framing is right: the host applies its UNIVERSAL operator
vocabulary to a contribution, exactly as it already does for builtins,
while the plugin still declares only chord + command.

Rejected: threading the operator prefixes down through `LoaderServices`
(leaks a host concept two crates down and adds another service that can be
left unwired — the `NotWired` failure family); and moving the row tables
upstream into `lattice-grammar` / `lattice-keymap` (the best long-term
shape if they belong there, but a refactor touching every builtin binding
site, landing inside an org slice where it does not belong).

**Groundwork already surveyed, so the next session starts from facts:**

- `keymap_normal.rs:2047` `operator_prefix(op, builtins)` maps an
  `OperatorId` to its chord path — `d`, `c`, `y`, `>`, `<`, `=`, `gU`,
  `gu`, `g~`, `g/`. This is the enumeration the pass expands over.
- `keymap_normal.rs:1441` `register_operator_bindings` is ALREADY `pub`
  for exactly this shape of reason — N.1.3 made it public "so boot can
  wire a *provider-contributed* operator's chord", the narrow `zn`
  precedent. A provider-contributed motion/text-object is the mirror case.
- `CommandRegistry` can classify: `CommandRegistration::kind()` →
  `CommandKind::{Motion, TextObject, Operator, Action, ExCommand}`
  (`registry.rs:596`). So the pass routes on what the command IS rather
  than needing a new WIT field — **OM.4b requires no WIT change**, which
  was not obvious when it was carved.
- **The one missing primitive:** there is no per-layer binding accessor on
  `KeymapHandle`. `KeymapTrie::walk_bindings` exists but only on a trie,
  with no way to fetch one layer's. The pass needs (chord, command) pairs
  for a given mode's layer, so that accessor is the first thing to build.
- A text-object chord must NOT keep its Normal terminal binding —
  `bind_mode_keymap` writes one today and a text object invoked standalone
  in Normal is meaningless. The pass replaces it rather than adding
  beside it. Visual-mode bindings are the third row set (`var` extends the
  selection), alongside operator-pending.

### OM.5 — `<Tab>` routing and the decline chain ✅ (2026-08-24)
Exit: `<Tab>` cycles on a headline and falls through to jump-list-forward
elsewhere. **Tests the multi-layer decline chain explicitly** (fragment
§4.3) — with `org-table-mode` stubbed to always decline, the chain must
still reach the builtin. If `Declined` does not chain past two layers,
that is a design review, not a workaround.
*bench:* the decline path by name — the one org path costing a guest call
on keystrokes that do nothing.

**Landed 2026-08-24.** `<Tab>` → `org-cycle`, which routes to
`AppEffect::CycleFoldAtCursor` on a headline and returns
`Effect::Declined` anywhere else. `<S-Tab>` → `org-global-cycle`, which
does NOT decline: a whole-buffer cycle is meaningful wherever the cursor
sits. No behaviour was reimplemented — `CycleFoldAtCursor` was written for
org and its doc comment says so; this slice is routing.

**Two coverage limits, stated rather than asserted around.**

*Folds are not asserted.* Structure-driven folds come from a landed
tree-sitter parse, which is asynchronous, so `editor.folds` is empty in
this harness whatever `<Tab>` did. `CycleFoldAtCursor`'s behaviour is
covered natively and predates the plugin; what OM.5 adds and what the test
covers is the routing. The decline case asserts the buffer is untouched —
which rules out the failure that would actually bite (the chord falling
all the way through and inserting a literal tab).

*The two-hop chain is not yet end-to-end.* The fall-through happens during
effect application, below this harness. What IS asserted is the layering
that makes it possible: a MINOR layer above `org-mode` takes `<Tab>` and
gives it back when inactive, which is exactly how `org-table-mode` will
refine its major. The two-hop case lands with OM.12, when the second layer
is real — shipping a stub mode that exists only to make a test pass would
be worse than saying this plainly.

*No bench.* The decline path's cost is the grammar round-trip, already
ratcheted at `< 5 µs p99` and independent of which guest answers — the
same reasoning recorded at OM.3.

Tests: 2 in `org_structure.rs` — `<Tab>` cycles on a headline and leaves
the buffer untouched off one; a higher layer can take `<Tab>` from
`org-mode` and hand it back.

### OM.6 — subtree move, meta-return, toggle heading ✅ (2026-08-24)
`<leader>oK` / `oJ` / `<leader><CR>` / `<leader>o*`. Landed in the plugin
repo (`fed1662`); no lattice change — the actions ride the `grammar` seam
and `org-mode`'s own keymap layer, exactly as OM.3 established.

Archive (`<leader>o$`) carved out as **OM.6b** — see below.

Three refusals carry the slice, each a silent restructure avoided:

- **Sibling ≠ adjacent headline.** From a level-2 headline the previous
  headline is often a level-3 child of an earlier sibling; swapping with
  it interleaves two trees. Both scans stop at a shallower headline, so a
  move stays inside one parent.
- **`<leader><CR>` inserts after the whole subtree.** The naive reading
  (next line) puts the new sibling in front of the headline's children
  and adopts them. Emacs's `-respect-content` reading is the safe one.
- **`toggle_heading` inherits the enclosing level**, so a note under
  `** Two` becomes a `**` sibling rather than escaping its section.

**`Effect::None` is not `Effect::Declined`** — the finding worth carrying
forward. These actions first returned `Declined` when there was nothing
to move, by analogy with OM.5's `<Tab>`. A declined chord is re-resolved
with the mode's layer removed, and for a MULTI-KEY sequence that runs the
trailing key alone: `<leader>oJ` executed vim's `J` and joined two lines.
`<leader>o*` would run `*`, `<leader><CR>` would run `<CR>`.

The rule: decline only a chord that is genuinely SHARED. `<Tab>` has a
native meaning to fall through to and still declines. A chord behind a
plugin-owned prefix has nothing underneath it and must consume the key.

⚠️ OM.3's `shift` still returns `Declined` on a refused level-1 promote
and in a headline-less buffer. Not visibly broken — the trailing keys are
`h`/`l`/`H`/`L`, all harmless motions — but it is the same latent shape.
Fix when OM.7 touches that file.

Tests: 10 new (4 unit, 6 integration through real chord dispatch); 51 in
the plugin repo total.

### OM.6b — archive subtree ⛔ blocked
`<leader>o$`. `org-archive-subtree` moves a subtree into `<file>_archive`,
and **no effect in the WIT surface writes to a file other than the
buffer's own**. `edits` / `apply-edit` target `ctx.buffer_id`;
`open-buffer-at` changes what is focused but does not compose with a
follow-on edit in a defined order.

So this is not plugin work — it needed a host decision first. Three
shapes were recorded:

1. **A new effect** (`append-to-file` or an explicit
   `edit-in-buffer(path, edits)`), which adds ABI surface for one
   command and needs `fs:write` capability gating.
2. **Archive in place** — `org-toggle-archive-tag`'s `:ARCHIVE:` tag plus
   a fold, which is a real org feature and needs nothing new, but is a
   *different* command from the one `<leader>o$` names in every other
   org implementation.
3. **`<leader>o$` opens the archive file** and leaves the move to the
   user, which is honest but not the feature.

**Decided 2026-08-25: (1).** (2) loses the standing convention rule —
it renames the command users came for — and does nothing for refile or
capture, which is the larger half. (3) is not the feature. (1) protects
paramount-#2 in the place org actually exposed a gap: the next plugin
that needs to write beside itself inherits it.

Design: [`../../architecture/cross-file-writes.md`](../../architecture/cross-file-writes.md).
Slice plan: [`cross-file-writes.md`](cross-file-writes.md).

**Unblocked 2026-08-26.** `Effect::WriteToFile` ships, gated on
`fs:write`. This is now ordinary plugin work: `<leader>o$` returns a
`write-to-file` naming `<file>_archive`, with the subtree's span as the
`cut`, and the manifest asks for `fs:write` over the org directory.

### OM.6b.0 — the document's own path ✅ (2026-08-26)

XF shipped the effect; this shipped the *address*. `org-archive-subtree`
files into `<file>_archive` — a name derived from the source file — and a
grammar action could not learn its own path. `action-context` carries
`cursor` and `buffer-id`; the `document` resource read text, length and
lines; `host-services` walks but does not ask; `project::root-for-buffer`
answers with the root, not the file. `write-to-file`'s `path` is
"absolute, or relative to the editor's working directory", which is the
wrong anchor the moment the org file is not under the cwd.

**On the resource, not the context.** `document.path() -> option<string>`
rather than `action-context.path`. A guest asking "which file am I in"
always holds a `document`, and on a context the same field would have to
be re-added to `motion-context`, `text-object-context` and
`ex-command-context` in turn — each dispatch paying the projection whether
or not the guest reads it. Snapshot semantics like every other method
there: the path as of the handle's mint.

**The native half is where the cost decision was.** None of the three
grammar contexts carried a path, so all three grew one — the trampoline
mints a `document` on each seam, and a `path()` that answered only on the
action path would read as "unsaved buffer" everywhere else, which is a
silent wrong answer rather than a missing feature.

`MotionContext` and `TextObjectContext` are borrowing contexts, so theirs
is `Option<&Path>` and free. `ActionContext` is owned, and a `PathBuf`
clone there is an allocation on the keystroke path — actions include
`undo`, paste and open-line. So `Document::path` was retyped
`Option<Arc<PathBuf>>` and the context takes an Arc bump.
`Document::path()` still hands out `Option<&Path>`, so no caller outside
`lattice-core` changed; `DocumentSnapshot::from_document` stopped
allocating a fresh `PathBuf` per publish, which is once per commit.

Tests: 2 through the real fixture guest — a guest names `<its own
file>_archive` with nothing about the target supplied by the test, and a
buffer with no file answers `none` rather than inventing one. Two
fixture-count assertions moved with it (`grammar_source.rs`,
`unload_reload.rs`); a third callback in the fixture is the proof.

### OM.7 — `org-todo-mode` ✅ (2026-08-24)
Second mode. TODO cycling (`<leader>ot` / `oT`), priority
(`<leader>o,`), tags (`<leader>o:`). Keyword sequence is
`org.todo-keywords` via the `config` seam. Landed in the plugin repo
(`bf7c4f1`); no lattice change.

Exit criterion met: `ActivationPolicy::Majors(["org-mode"])` scopes the
minor, and its chords resolve with that mode active and nowhere else.

**A plugin minor is INERT until enabled, and that trips everyone once.**
`auto_activatable_minors` filters on enablement (CI.3), so the mode
registered correctly and simply never activated. The manifest's
`default_mode` is what publishes `ModeEnablementRequested`; without it
there is no gate and no enablement. Two per-tick drains then have to run
IN ORDER:

  1. `drain_mode_enablement` (from `default_mode`) — before the buffer
     opens, or step 2 finds the mode disabled;
  2. `drain_minor_activation` (from `MajorEntered`) — after it, or the
     `Majors` policy reads an empty major and refuses.

Tests drive `run_tick_pending` on both sides of the open rather than
hand-picking the two.

**Second WIT seam for the plugin: `config`.** Options are auto-namespaced
by plugin id, so the guest registers `todo-keywords` and the user sets
`org.todo-keywords`. Read per keystroke, not cached — `:set` must land on
the next press, and caching would need an `OptionChanged` subscription to
stay honest.

⚠️ **App-layer test debt, now with a second creditor.** `<leader>o:` is a
two-hop prompt flow and NEITHER hop is reachable from an `Editor`-level
test: `Effect::OpenPrompt` is applied by the renderer (so the chord
surfaces as `Action::Invoke` with the effect consumed inside it), and
dispatching an invocation *with args* needs `handle_action`, which is
`pub(crate)`. Coverage is split — `todo.rs` unit-tests the tag logic, an
integration test asserts the chord and the named submit action are both
registered — but the round trip wants the `lattice-ui-tui` test file that
already owes counts (`3]]`), `dar` end-to-end and the two-hop decline
chain. That file is now the single place four things are waiting on.

Tests: 16 new (11 unit, 5 integration); 70 in the plugin repo.

### OM.8 — checkboxes + cookies ✅ (2026-08-25)
`<C-Space>`; the nearest ancestor cookie recalculates in the SAME edit, so
one `u` puts both back — a list showing `[2/3]` above one ticked box is a
worse state than either end. The tally runs against the buffer as it WILL
be, or the cookie lags a keypress behind the box.

Cookies count direct children only (a grandchild is already reflected in
its own parent's box) and keep their form — `[n/m]` stays a ratio, `[p%]`
a percentage, truncated so 100% means complete. `* [ ] x` at column 0 is a
headline, not a bullet: accepting it would let `<C-Space>` rewrite a
heading.

### OM.9 — timestamps ✅ (2026-08-25)
`<C-a>` / `<C-x>` on the component under the cursor; anywhere else in the
stamp means the day.

**The one action in the plugin that should decline.** These are vim's
increment / decrement — a genuinely shared chord — so org shadows them
only where a timestamp is. Everything behind `<leader>o` consumes instead.
Note lattice has **no increment command yet**, so today the decline
resolves to nothing; the first test asserted `41`→`42` and failed on that.
Recorded rather than papered over: when increment lands this composes for
free, which consuming would have foreclosed.

Date maths is hand-rolled (a `chrono`-shaped dep in a wasm guest buys a
dozen lines). The weekday is recomputed every edit — a stamp whose day
name disagrees with its date is worse than none. Date entry via
`OpenPrompt` was not needed and is not built.

### OM.10 — links ✅ (2026-08-25)
`<leader>oo`, exactly the three routes planned. The internal case searches
this buffer and matches the headline TITLE exactly, so a TODO keyword or
priority on the target does not interfere; an unresolved reference echoes
and leaves the cursor put, because jumping to the wrong heading is worse
than not jumping. Resolved by title comparison rather than a tree search —
the tree can be absent, and this runs on a keystroke.

### OM.11 — refile + capture ⛔ BLOCKED (2026-08-25)

Blocked on the **same missing primitive as OM.6b**, and confirmed rather
than assumed: no effect in the WIT surface writes to a file other than the
buffer's own. `apply-edit` takes a `target` buffer id the guest cannot
learn for a file it has not opened; `invoke-command` exists only as a
*picker-accept outcome*, not as an effect, so there is no way to chain
"open that file" into "now edit it".

Refile's primary meaning is moving a subtree to ANOTHER file, and capture
writes to a designated capture file, so both need it. A same-file-only
refile is expressible (one `ApplyEdit` over the enclosing span) but is a
different, much smaller feature than the slice describes.

**Decided together with OM.6b** (2026-08-25) and **shipped 2026-08-26**:
a `write-to-file` effect, host-mediated and `fs:write`-gated. Design:
[`../../architecture/cross-file-writes.md`](../../architecture/cross-file-writes.md);
slice plan: [`cross-file-writes.md`](cross-file-writes.md).

Refile is now expressible: pick a target (its own picker source), return
`write-to-file` with the subtree as the `cut`. So is capture — the effect
creates a missing target, which is capture's first run. What is NOT
supplied by XF and remains this slice's own work: capture's template flow
(a prompt, a target picker) and refile's target-selection UI.

Note what stays org's own work afterwards: the cross-file *write*
unblocks, but capture's template flow (a prompt, a target picker) is
this slice's, not XF's. Until then the outliner is complete for
everything that stays within one file.

## Tables

### OM.12 — `org-table-mode` ✅ (2026-08-25)
Third mode. `<Tab>` / `<S-Tab>` align and step a cell; `<leader>o|`
aligns in place.

**The decline chain finally has two hops, and only because of the
inline-media work.** Table-mode's `<Tab>` declines off a table and reaches
org-mode's headline cycle. That required lattice `b9f6e3f6`, which fixed
the dispatcher to peel ONE keymap layer per decline; before it, a decline
dropped every mode layer at once and would have skipped org-mode entirely
— exactly what OM.5's comment claimed already worked and did not.

Alignment is whole-table and one edit (a column is as wide as its widest
cell). A ragged row is padded rather than refused — mid-edit is when a
table IS ragged, and when the key is most wanted.

### OM.13 — row / column move + insert ✅ (2026-08-25)
`<leader>t…`, using the outliner's directional letters so one mnemonic
covers subtrees and rows. Whole-table re-align in the same edit; the caret
follows what it moved.

Four refusals protect structure a key would otherwise destroy silently: a
separator will not move (a rule marks a section), the last row and column
will not delete, and a ragged row is padded before a column swap or it
keeps its columns transposed against every other row. A refusal CONSUMES
— the user is in a table and meant a table command.

## The agenda

### OM.A1 — the seam ✅ (2026-08-25)

`wit/agenda-source.wit`, the `AgendaSourceRegistry` native seam, the
plugin-host bridge, the loader drain, and `providers::agenda` — the walk,
the cross-file sort, the excerpt build, `:agenda`.

**The design's WIT needed two amendments, both recorded in the fragment
rather than smuggled in.**

1. **`extensions: func() -> list<string>`.** §6.2 said the host walks
   "`.org` only", which puts a filetype in the host and contradicts §10's
   own acid test. The source declares what it wants offered, resolved once
   at load. Rejected: offering every project file to every source (the
   full text of every file in the tree crossing the boundary — the
   producer-cost §7 warns about), and reading the extensions off the
   plugin's `language` seam (couples two independent contributions).
2. **`group` is a KEY, not a label.** As sketched, `group` and `label`
   were redundant and `group`'s "empty = same as previous" reading was
   unimplementable: a guest cannot know which of its rows lands first once
   the cross-file sort interleaves them. The host now compares keys after
   sorting and titles the first row of each run.

`scan` also gained a `result<_, string>` return — one malformed file must
not fail the agenda, and without it there was no way for the guest to say
so.

**No trigger machinery was added, because PV.1 already built it.** The
provider registers an opener on the generic provider-view seam and
`:agenda` emits `AppEffect::OpenProviderView`. Zero `Editor::` methods,
zero host `Action` variants, zero dispatch arms — the acid test, checked
by grep as well as by prose.

**The whole scan finishes before anything is appended**, unlike
`providers::search`. The agenda's order is *global*: a row from the last
file may belong at the top. Progress moves to the headerline instead.
Re-sorting and rewriting every row per batch was rejected outright — a
whole-viewport restyle is a UX-rules veto, not a trade-off.

**Two bugs found on the way past, both pre-existing and both silent.**

- `WiredSeams` never reported `media_registry`, so a boot-ordering
  regression there would have degraded `drain_media` to a `NotWired` skip
  with nothing asserting otherwise. Added alongside `agenda_registry` —
  adding the sibling and leaving this one silent would be
  aligned-by-silence.
- **The loader never published its media-registry teardown snapshot.**
  `unload` mutates a `&mut` clone of the `ArcSwap`'s contents and the
  loader stored back `commands` / `pickers` / `modes` / `decorations` /
  `contexts` but not `media` — so unloading a media plugin unregistered
  its producer from a clone that was then dropped, and the plugin kept
  contributing images until the next reload. Both registries are stored
  back now.

Tests: 12 in `providers::agenda` (cross-file sort; stable order on equal
keys; one header per group run; a recurring group getting a fresh header;
rows of one file sharing one source document; a rejected file skipped
while the scan continues; an empty agenda still reaching a terminal
headerline; `begin` exactly once per scan; the extension filter; `~`
expansion; service round-trip), 9 in `lattice-mode::agenda_source` +
`plugin-host::agenda_source` (registry replace-on-reload, teardown
idempotence, claim matching, entry validation, extension normalisation),
and 5 in `tests/agenda_source.rs` driving the real `agenda-guest` fixture
— including the one that matters most, that **`begin` really resets the
guest's per-scan state**, which a single-scan test cannot see.

*bench:* `agenda_scan/scan_200_files` — the host half (walk, extension
filter, batched reads, cross-file sort, excerpt build) against a native
fake producer. The guest round-trip is already ratcheted by the grammar
gate and is independent of which guest answers.

### OM.A2 — org semantics ✅ (2026-08-25)

`agenda.rs` in the plugin repo; `begin`/`scan` become a shell over it.

A row is an OPEN headline carrying a date, from `DEADLINE:` or
`SCHEDULED:` on the planning line or an active `<…>` stamp on the
headline. Three refusals carry as much of the slice as the rule: an
INACTIVE `[…]` stamp is never a row (counting them drags every logbook
line and every `CLOSED:` in); a DONE headline is not a row however dated
(an agenda listing what you finished is a log); and only the line
immediately below a headline is its planning line — one under a CHILD
belongs to the child, and dating the parent with it would jump the user
to the wrong line.

`end_line` runs to the planning line, so a row shows its date rather than
a bare title. That is the field OM.A1's trivial guest had no use for.

Ordering is one packed `i64` because the ABI carries one number: epoch
day, then kind (deadline → scheduled → bare stamp), then priority with
unprioritised LAST — unranked rather than urgent. The epoch day is
Hinnant's `days_from_civil`, twenty lines where `chrono` in a wasm guest
would be a dependency tree.

`group` (ISO date) and `label` (rendered header) stay separate: the host
compares KEYS after its sort, so a key reading "Today" would merge two
days if a scan straddled midnight. The label is relative to a `today`
captured once in `begin`, alongside the keyword set — `:set
org.todo-keywords` landing mid-scan would change what counts as done
halfway through a project.

Exit met: the e2e corpus is written relative to the day it runs, and
`home.org` supplies both the first row and the last — an ordering only
the cross-file sort produces. Tomorrow's group is drawn from two files
under one header.

Tests: 15 in `agenda.rs` (the calendar, including 1900 / 2000 / pre-epoch;
the three refusals; the ordering packing; key vs label) plus the
rewritten e2e.

### OM.A3 — the view's modes ✅ (2026-08-25)

**The design gave one mode what two own, and the plugin could not have
had the whole of it.** `gr` refresh means re-running the HOST's walk —
`AppEffect::OpenProviderView`, whose plugin surface is *deliberately*
withheld (`boundary_app_effect.rs`) pending the capability model for
which providers a plugin may trigger. A plugin `gr` could bind the chord
and not do the work.

It is the better split on merit anyway: the second agenda-source plugin
inherits refresh instead of re-deriving it, which is the copied-keymap
failure the minor-mode rule forbids one layer up. So:

- **`agenda-view-mode`** — native, in `providers/agenda.rs`, the
  `ProjectSearchMode` shape verbatim. `refresh_action()` returns `Some`,
  which is the whole `gr` wiring (the cascade keys on that, NOT on an
  `implies()` entry — RV.1 made it one line precisely so a forgotten
  list entry could not kill the chord silently). The body lives on the
  mode.
- **`org-agenda-mode`** — the plugin's, carrying the TODO chords and
  their handler bodies.

**One ABI addition, and §"Why the agenda is last" is what reserved the
right to make it:** `view-mode: func() -> option<string>`. No
`ActivationPolicy` can say "the buffer this provider just built" —
`majors(["multibuffer-mode"])` would fire org's chords in search results
and magit diffs — so the source names a minor and the provider activates
it. The host learns an id and never learns what the chords do.

`gr` re-scans **the root the view already shows**, read back from the
per-view state. A refresh that silently re-targets itself is not a
refresh (PD.9's mistake, avoided rather than repeated).

**Partial-and-honest got a second half.** §8 said a trap mid-scan leaves
what it collected; a bare row count would still make a partial agenda
look exactly like a complete one that had fewer rows. A source is dropped
after three CONSECUTIVE failures — the quarantine signature, where
scattered bad files reset the counter — and the terminal headerline says
`— partial: N source(s) stopped responding`. Isolated skips are reported
separately (`(3 file(s) skipped)`).

Exit met, all four: a `<leader>ot` typed in the agenda writes the source
file; `gr` rescans the view's own root; the headerline reports progress
and completion; a dead source leaves partial rows and says so.

Tests: 3 more in `providers::agenda` (the dead-source drop with its call
budget; scattered failures NOT dropping a healthy source; the `gr`
target-and-body contract), 1 in `lattice-mode` (view-mode dedup), and the
plugin repo's `changing_a_todo_state_in_the_agenda_writes_the_source_document`
— which asserts the mode was activated before typing, because without
that the chord resolves to nothing and the text assertion fails with no
clue why.

**A test-harness bug worth recording**, because it presented as a broken
feature for half an hour: `org_agenda.rs`'s plugin manifest omitted
`"grammar"` from `provides`, so no action names existed, every keymap
binding was skipped with a `warn` nobody was capturing, and `<leader>ot`
fell through to vim's `o` + an inserted `t`. `org_major_mode.rs` has the
same omission and does not care (it asserts activation, not chords). A
mode whose bindings all silently vanish looks exactly like a mode that
did not activate.

## OM.14 — docs, ledger, site ✅ (2026-08-25)

- **`doc/org.md`** gained an Agenda section and its closing *"Tables and
  the agenda are not here yet"* is gone — tables had been shipped for a
  day and the claim was already false when the agenda landed. What
  replaces it is the honest cut list: injection is missing, and
  everything that writes to another file is blocked rather than pending.
- **`README.md`** was stale from before the repo split — a relative link
  into `../../docs/`, a `cd examples/org-plugin`, the wrong artefact
  name, and a `provides = ["language", "help"]` that named two of seven
  seams. Its "everything else org needs … is a separate track, gated on
  nothing here" paragraph described work that has since landed.
- **`implementation.md`**: the Phase 8b row (📝 *design underway; no
  crate built* → 🟡 three plugins shipped) and a new §"Org-mode as a
  plugin" carrying the four generic host changes, the acid test, the
  three findings worth keeping, and what the cross-file block actually
  is.
- **Cross-refs** in both directions, plus a pointer from the design
  fragment to the three sections it amends mid-build.
- **`docs/user/agenda-view-mode.md`** + `nav.toml` + sync landed with
  OM.A3 rather than here: the mode-has-a-help-page class guard fails the
  build without it, which is the right place for that to be caught.

## The acid test, as an assertion

Asserted in tests rather than claimed in prose, at OM.2 and again at
OM.A3:

- **zero** `Editor::` method additions,
- **zero** new variants in the host's `Action` enum,
- no `BufferKind::Org`, no `Lang::Org`, no org branch in either renderer.

The three host changes (OM.0–OM.2) are generic — a language index, a WIT
field, a lifted restriction. None of them names org.
