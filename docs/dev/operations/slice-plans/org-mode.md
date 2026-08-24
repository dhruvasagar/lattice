# Org-mode as a plugin — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contracts live in
> [`../../architecture/org-mode.md`](../../architecture/org-mode.md) —
> the mode decomposition, the keymap rationale, the agenda seam shape,
> rejected alternatives, paramount-goal alignment.

**Status:** OM.3 ✅ (2026-08-24) — **org edits.** OM.0 ✅ seam drain order is
structural (the gate found a real bug rather than confirming the status quo);
OM.1 ✅ the registry language index; OM.2 ✅ majors cross the `modes` seam;
OM.2b ✅ `<leader>` (carved mid-build — it did not exist); OM.3 ✅
promote/demote. Next: OM.4, motions and text objects.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

## Where this sits

Phase **8b** (bundled / reference plugins) — `implementation.md` already
names `examples/org-plugin` as its second reference plugin alongside
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
  ├── outliner ──────────────────────────────────────────────┐
  │   OM.3   promote / demote (headline + subtree)            │
  │   OM.4   headline motions + ih/ah/is/as text objects      │
  │   OM.5   <Tab>/<S-Tab> routing + the decline chain        │
  │   OM.6   subtree move, meta-return, toggle heading, archive│
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
| OM.4 | Headline motions + `ih`/`ah`/`is`/`as` text objects | 📝 |
| OM.5 | `<Tab>` / `<S-Tab>` routing + the decline chain | 📝 |
| OM.6 | Subtree move, meta-return, toggle heading, archive | 📝 |
| OM.7 | `org-todo-mode`: TODO cycling, priority, tags | 📝 |
| OM.8 | Checkboxes + statistics cookies | 📝 |
| OM.9 | Timestamps (`<C-a>` / `<C-x>`, date prompt) | 📝 |
| OM.10 | Links: open at point | 📝 |
| OM.11 | Refile (plugin's own picker-source) + capture | 📝 |
| OM.12 | `org-table-mode`: align + cell motion | 📝 |
| OM.13 | Row / column move + insert | 📝 |
| OM.A1 | `agenda-source` seam + host provider | 📝 |
| OM.A2 | Org agenda semantics in the guest | 📝 |
| OM.A3 | `org-agenda-mode`: act from agenda, `gr`, headerline | 📝 |
| OM.14 | Docs, ledger, site nav | 📝 |

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

### OM.4 — motions and text objects 📝
`]]` / `[[` / `g{`; `ih` / `ah` / `is` / `as` via `register-text-object`.
Exit: `das` deletes a subtree and `yah` yanks a headline through the
**ordinary** operators — no org-specific chord in either.

### OM.5 — `<Tab>` routing and the decline chain 📝
Exit: `<Tab>` cycles on a headline and falls through to jump-list-forward
elsewhere. **Tests the multi-layer decline chain explicitly** (fragment
§4.3) — with `org-table-mode` stubbed to always decline, the chain must
still reach the builtin. If `Declined` does not chain past two layers,
that is a design review, not a workaround.
*bench:* the decline path by name — the one org path costing a guest call
on keystrokes that do nothing.

### OM.6 — subtree move, meta-return, toggle, archive 📝
`<leader>oK` / `oJ` / `<leader><CR>` / `<leader>o*` / `<leader>o$`.

### OM.7 — `org-todo-mode` 📝
Second mode. TODO cycling (`<leader>ot` / `oT`), priority
(`<leader>o,`), tags (`<leader>o:`). Keyword sequence is
`org.todo-keywords` via the `config` seam.
Exit: the minor activates only on `org-mode` buffers, and its chords
resolve nowhere else.

### OM.8 — checkboxes + cookies 📝
`<C-Space>`; parent `[1/3]` / `[33%]` recalculation in the same edit
batch, so one undo step.

### OM.9 — timestamps 📝
`<C-a>` / `<C-x>` on the component under the cursor. Declines to the
builtin increment elsewhere. Date entry is `Effect::OpenPrompt`.

### OM.10 — links 📝
`<leader>oo`: file → `OpenBufferAt`, `http(s)` → `OpenExternalUri`,
internal `*Headline` → `CursorMove` after a tree search.

### OM.11 — refile + capture 📝
Refile's target chooser is a **picker the plugin registers itself**
through the existing `picker-source` seam, opened with
`Effect::OpenPicker`. Capture writes through `OpenBufferAt` + `edits`.
Exit: the outliner is complete — an org file is editable, not merely
readable.

## Tables

### OM.12 — `org-table-mode` 📝
Third mode. Alignment + cell motion; `<Tab>` / `<S-Tab>` / `<CR>`.
Extends OM.5's chain with its real (non-stub) implementation. Column
widths computed guest-side from the table's parse; alignment lands as
one `edits` batch.

### OM.13 — row / column move + insert 📝

## The agenda

### OM.A1 — the seam 📝
`wit/agenda-source.wit` + the host provider: walk `.org` (bounded,
`fs:read`-gated), read off-thread, `scan` per file, stable-sort by
`sort-key`, `append_excerpts`, publish `MultibufferExcerptsReady`, drive
the headerline. Guest is trivial — one entry per headline.
Exit: excerpts appear in a multibuffer.
*bench:* scan throughput per file.

### OM.A2 — org semantics 📝
TODO / `SCHEDULED:` / `DEADLINE:` parsing, date arithmetic, grouping via
first-excerpt-titled / rest-empty, sort keys.
Exit: a 3-file fixture produces a date-grouped agenda in the right order.
Failure: a malformed file is skipped and the scan continues.

### OM.A3 — `org-agenda-mode` 📝
Fourth mode, a **minor** the provider activates on the view — the
`ProjectSearchMode` shape (`providers/search.rs:912`).
Exit: changing a TODO state **in the agenda** writes the source file;
`gr` rescans; the headerline reports progress and completion; a trap
mid-scan leaves partial excerpts and an honest headerline.

## OM.14 — docs, ledger, site 📝

- `examples/org-plugin/doc/org.md` — its closing *"Editing. Headline
  promotion and demotion… none of that is here"* section stops being
  true and is rewritten.
- `examples/org-plugin/README.md`, `implementation.md` (Phase 8b row +
  an org-mode section), design fragment ↔ slice plan cross-refs.
- Zola site: `nav.toml`, sync, search — a `docs/` change is not finished
  until the site carries it.

## The acid test, as an assertion

Asserted in tests rather than claimed in prose, at OM.2 and again at
OM.A3:

- **zero** `Editor::` method additions,
- **zero** new variants in the host's `Action` enum,
- no `BufferKind::Org`, no `Lang::Org`, no org branch in either renderer.

The three host changes (OM.0–OM.2) are generic — a language index, a WIT
field, a lifted restriction. None of them names org.
