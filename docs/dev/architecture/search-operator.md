# The `g/` search operator (developer reference)

`g/` is a native vim-grammar **operator** that takes any motion or text object,
extracts the text it spans, and triggers a **project search** for that literal.
`g/iw` greps the project for the word under the cursor; `g/i"` for the quoted
string; a Visual selection then `g/` searches the selection.

Sequencing + status live in the slice plan
[`../operations/slice-plans/search-operator.md`](../operations/slice-plans/search-operator.md).
Project-search internals it drives are in
[`multibuffer-views.md`](multibuffer-views.md) §3.7 (the `SearchProvider`); the
grammar/operator machinery is [design.md §5.2](design.md).

> **Mnemonic.** `/` is buffer search; `g` is vim's "global / extended variant"
> prefix (`gg`, `g;`, `gU`). `g/` therefore reads as *"the global (project)
> counterpart of `/`"* — easy to recall, distinct from `/`. `g/` is a
> deliberate **Lattice extension** to the vim grammar (vim itself has no `g/`
> operator); it is justified by paramount goal #3 — the grammar *is* the public
> command API and adding operators is first-class — not by precedent in another
> editor.

---

## 1. Grammar surface

`g/` is an operator in the same `g`-prefix family as the case operators
(`gu` / `gU` / `g~`). It composes with **every** motion and text object for free
through the operator-pending machinery:

| Chord | Project-searches for… |
|---|---|
| `g/iw` / `g/iW` | word under cursor |
| `g/i"` `g/i'` `` g/i` `` | quoted-string contents |
| `g/a(` `g/i{` `g/it` | delimited / tag contents |
| `g/$` `g/e` `g/fx` … | the motion's span |
| (Visual) select → `g/` | the selection |

The operator is **read-only** — it never mutates the buffer. Its shape mirrors
yank (`operator_yank`): read the range's text, emit an effect, apply no edit.

## 2. Contract + data flow

`g/` reuses the **exact seam `:search` already uses**. The operator emits
`AppEffect::SearchTrigger { query }`; the host already routes that effect to
`project_search(...)`. There is **no new host effect variant and no new dispatch
arm** — `g/{motion}` and `:search {text}` are identical by construction.

```
g   → Pending::AfterG                         (host trie, existing)
g / → absorb_operator_search                  (host: latch the operator, operator-pending)
{motion} → dispatcher resolves [op, motion] → ProtoRange
          → operator_search(ctx):
                query = ctx.document.buffer().slice(ctx.range)?
                Effect::AppAction(AppEffect::SearchTrigger { query })
          → host apply_app_effect → project_search(query, defaults)
          → streaming *search:{query}* multibuffer
```

**Query semantics.** The extracted text is the query verbatim — a **literal,
case-insensitive** substring, matching project search's existing defaults
(`ProjectSearchOptions { regex: false, case_sensitive: false, .. }`). No regex
escaping is required because the query is treated literally; regex/case toggles
remain the province of `:search` and are out of scope for the operator.

## 3. Ownership + keymap layer

- The **handler body lives in `lattice-grammar`** (`operator_search` in
  `builtins.rs`), the owner of the grammar surface — exactly like
  `operator_yank` and the case operators. It emits an already-routed effect;
  **no `Editor::do_*` host method is added** and **no new host `Action` variant**.
- The `[g, /]` binding sits at **`KeymapLayer::Builtin`**. This is correct
  because operators are *universal* vim grammar that fire in every buffer (like
  `gu` / `gU`), not a mode-scoped chord — so the mode-ownership standing rule
  (which governs *mode*-owned chords) does not apply. The host's only addition
  is a 9th operator-prefix arming action, following the established
  `register_operator_prefix` pattern used by the other eight operators.

## 4. Edge cases

- **Empty range** (motion selects nothing) → `Effect::None` + `debug!`; no empty
  search is launched.
- **Multi-line motions** (`g/ap`, `g/G`) pass a literal containing newlines,
  which the line-oriented project search will not match — documented as "aimed
  at single-line text objects", **not** special-cased (YAGNI). The operator is
  most useful with small text objects (`iw`, `i"`, `a(`, …).
- **`search` cargo feature off** → the emitted `SearchTrigger` is an
  already-existing graceful no-op on that build; `g/` simply does nothing. Never
  panics on the hot path.

## 5. Paramount-goal alignment + rejected alternatives

- **#1 performance.** The added work on dispatch is one O(range) rope slice
  (`Buffer::slice`); the scan itself already runs off the UI thread via
  `project_search`'s spawned task. No UI-thread work is added.
- **#3 extensibility.** `g/` is a first-class native operator — the "adding
  operators is first-class" case — reusing the unified command/effect seam that
  ex-commands and grammar share.
- **#4 asynchronicity.** The search runs on the existing off-thread scan; the
  operator only produces a query.

**Rejected:**
- *In-buffer search target* — `/` already exists; `g/` is deliberately the
  distinct project-scoped variant (the whole reason for the `g` prefix).
- *A dedicated new `AppEffect` variant* — rejected on heuristic #1: reusing
  `SearchTrigger` is the genuinely-simpler correct design (identical behaviour
  to `:search`, zero new routing), not risk-aversion.
- *Prefilled editable query line with regex/case toggles* — deferred; that is a
  `:search`-surface enhancement, orthogonal to the operator.
- *Also writing the in-buffer `last_search` register so `n`/`N` narrow locally
  after the grep* — YAGNI for v1; addable later if it proves common.

## See also

- [`multibuffer-views.md`](multibuffer-views.md) §3.7 — the `SearchProvider` /
  `project_search` path `g/` drives.
- [`design.md §5.2`](design.md) — the modal-editing engine / unified grammar +
  command dispatch the operator plugs into.
- [`../operations/slice-plans/search-operator.md`](../operations/slice-plans/search-operator.md)
  — slice plan (SO-series sequencing + status).
- [`../../user/project-search.md`](../../user/project-search.md) — user-facing
  docs (extended with the `g/` use-case table).
