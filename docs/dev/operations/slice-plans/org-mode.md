# Org-mode as a plugin — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contracts live in
> [`../../architecture/org-mode.md`](../../architecture/org-mode.md) —
> the mode decomposition, the keymap rationale, the agenda seam shape,
> rejected alternatives, paramount-goal alignment.

**Status:** OM.0 📝 — nothing landed yet.

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
| OM.0 | Gate: a plugin's `grammar` seam drains before its `modes` seam | 📝 |
| OM.1 | `ModeRegistry` language index + `Mode::target_language` | 📝 |
| OM.2 | `modes` seam accepts `major`; `target-language` field | 📝 |
| OM.3 | Promote / demote headline + subtree | 📝 |
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

### OM.0 — the drain-order gate 📝

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

### OM.1 — the language index 📝

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

### OM.2 — majors over the seam 📝

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
