# `g/` — the project-search operator (design)

**Date:** 2026-07-21 · **Status:** design, pending implementation

## 1. Summary

Add a native vim-grammar **operator** `g/` that takes any motion or text
object, extracts the text it spans, and triggers a **project search** for that
literal. `g/iw` greps the project for the word under the cursor; `g/i"` for the
quoted string; `v`-select then `g/` for the selection.

Mnemonic: `/` is buffer search; `g` is vim's "global / extended variant"
prefix (`gg`, `g;`, `gU`), so `g/` reads as *"the global (project) counterpart
of `/`"* — easy to remember, distinct from `/`.

`g/` is a deliberate Lattice extension to the vim grammar (vim has no `g/`
operator). It is justified by **paramount goal #3** — the grammar *is* the
public command API and "adding new operators is first-class" — not by
precedent in another editor (heuristic #2).

## 2. Behaviour

- **Operator + motion/text-object composition** (free from the operator
  machinery — every motion/text-object works):

  | Chord | Project-searches for… |
  |---|---|
  | `g/iw` / `g/iW` | word under cursor |
  | `g/i"` `g/i'` ``g/i` `` | quoted string contents |
  | `g/a(` `g/i{` `g/it` | delimited / tag contents |
  | `g/$` `g/e` `g/fx` … | the motion's span |
  | (visual) select → `g/` | the selection |

- **Query semantics:** the extracted text is used as a **literal, case-
  insensitive** substring query — project search's existing defaults
  (`ProjectSearchOptions { regex: false, case_sensitive: false, .. }`). No
  regex escaping is required because the query is treated literally.

- **Fires immediately:** `g/{motion}` opens the streaming `*search:{query}*`
  multibuffer at once, identical to `:search {query}`. (Refinement with
  regex/case toggles remains the province of `:search`; not in scope here.)

- **Edge cases:**
  - *Empty range* (motion selects nothing) → no-op + `debug!`; no empty search
    is launched.
  - *Multi-line motions* (`g/ap`, `g/G`) pass a literal containing newlines,
    which line-oriented search will not match — documented as "aimed at
    single-line text objects", **not** special-cased (YAGNI).
  - *`search` cargo feature off* → the emitted `SearchTrigger` effect is an
    already-existing graceful no-op on that build; `g/` simply does nothing.

## 3. Architecture

The operator reuses the **exact seam `:search` already uses** — it emits
`AppEffect::SearchTrigger { query }`, which the host already routes to
`project_search(...)`. There is therefore **no new host effect variant and no
new dispatch arm**; `g/` and `:search` are identical by construction.

```
g   → Pending::AfterG                    (host trie, existing)
g / → absorb_operator_search             (host: latch the search operator, operator-pending)
{motion} → dispatcher resolves [op, motion] → ProtoRange
          → operator_search(ctx):
                query = ctx.document.buffer().slice(ctx.range)?
                Effect::AppAction(AppEffect::SearchTrigger { query })
          → host apply_app_effect → project_search(query, defaults) → *search:query* multibuffer
```

### Ownership / layer

- The operator **handler body lives in `lattice-grammar`** (the owner of the
  grammar surface), exactly like `operator_yank` / the case operators. It emits
  an already-routed effect; **no `Editor::do_*` host method is added**.
- The `[g, /]` binding sits at **`KeymapLayer::Builtin`** — correct because
  operators are *universal* vim grammar that fire in every buffer (like
  `gu`/`gU`/`g~`), not a mode-scoped chord. (The mode-ownership standing rule
  governs *mode*-owned chords; `g/` is not mode-owned.)

## 4. Implementation surface (3 edits + artefacts)

Anchors are current as of this spec; verify at implementation time.

1. **`crates/lattice-grammar/src/builtins.rs`** — add `operator_search`
   (read-only, mirrors `operator_yank` at ~:2178) and register it via
   `register_operator(name, doc, OperatorSpec { repeatable:false,
   blockwise_per_row:false, .. })`, near the case-operator regs (~:326-355).
   Handler: `let query = ctx.document.buffer().slice(ctx.range)?;
   Ok(Effect::AppAction(AppEffect::SearchTrigger { query }))` — empty/whitespace
   query ⇒ `Ok(Effect::None)` + `debug!`.
2. **`crates/lattice-host/src/actions.rs`** — a 9th operator-prefix action
   `absorb_operator_search` (field ~:168-175, reg ~:986-1030 via
   `register_operator_prefix(...)` ~:1320-1334) pointing at the new
   `OperatorId`.
3. **`crates/lattice-host/src/keymap_normal.rs`** — `handle.bind(layer, mode,
   &[g.clone(), lit_char('/')], CommandInvocation::of(actions.absorb_operator_search),
   source())` next to the case operators (~:484). Verified: `/` is **not** an
   existing `g`-prefix second key — no conflict.

### Four artefacts (per CLAUDE.md)

- **Docs** — extend `docs/user/project-search.md` with a `g/` operator section
  + the use-case table above; cross-reference `docs/user/modal-editing.md`
  (where `/`, `*` live) so the `/` ↔ `g/` relationship is discoverable.
- **Tests** —
  - *grammar*: `g/{motion}` over a fixture buffer yields
    `Effect::AppAction(AppEffect::SearchTrigger { query })` with the correct
    text for `iw`, `i"`, and a visual selection; empty range ⇒ `Effect::None`.
  - *host*: the `[g, /]` chord resolves through the trie, arms the operator,
    and the following motion routes to `SearchTrigger` (mirror an existing
    operator-pending dispatch test).
- **Bench** — operator dispatch is Reflex-class and already covered by the
  existing operator bench; the added work is one O(range) rope slice. **No new
  bench**; noted rather than added.
- **Error handling** — empty-range no-op (`debug!`, no empty scan); feature-gate
  graceful (effect is a no-op when `search` is off); never panics on the hot
  path.

## 5. Rejected / out of scope

- *In-buffer search target* — `/` already exists; the user chose `g/`
  precisely as the distinct project-scoped variant.
- *New dedicated `AppEffect` variant* — rejected on heuristic #1: reusing
  `SearchTrigger` is the genuinely-simpler correct design (identical behaviour
  to `:search`, zero new routing), not risk-aversion.
- *Prefilled editable query line with regex/case toggles* — deferred; that is a
  `:search`-surface enhancement, orthogonal to the operator.
- *Also setting the in-buffer `last_search` register* — YAGNI for v1; can be
  added later if narrowing-locally-after-grep proves common.
