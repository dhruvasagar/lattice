# Refreshable views — slice plan

> **Status: Active.** Opened 2026-08-10. Implements
> [`mode-architecture.md`](../../architecture/mode-architecture.md) §5.5:
> `gr` as one shared chord over a `Mode::refresh_action()` declaration,
> replacing three copied keymap entries.

Design owns *what* and *why*; this file owns *when* and *in what order*.

## Status

| Slice | Title | Status |
|---|---|---|
| RV.1 | `Mode::refresh_action()` + `refreshable-view-mode` + generic dispatch | 📝 |
| RV.2 | Retrofit the three existing copies | 📝 |
| RV.3 | Close the gaps — `*problems*`, narrow | 📝 |

RV.1 before RV.2 (the seam must exist to migrate onto). RV.3 is
independent of RV.2 and can land either side of it.

---

## RV.1 — The seam 📝

- `lattice-mode`: `fn refresh_action(&self) -> Option<ActionId>` on the
  `Mode` trait, defaulting to `None`. Mirror it on the object-safe
  companion trait alongside `keymap()` / `action_handlers()`.
- `refreshable-view-mode` minor in `lattice-mode`: one keymap entry,
  `gr` → `action:view-refresh`, at `KeymapLayer::MinorMode`. **Never
  Builtin** — `gr` in a source buffer is LSP references and must stay
  that way.
- Generic handler: walk active modes **minors first in activation
  order, then major**; first `Some` wins; dispatch through
  `ActionHandlerRegistry`. More than one declaration → first wins,
  rest at `debug!`.
- `None` for every active mode → echo `nothing to refresh here`.
- Activation: the cascade activates the shared minor when any activated
  mode returns `Some`. Fall back to `implies()` only if the predicate
  proves awkward to evaluate in the cascade — and record why, because
  the fallback costs a second thing to remember.

**Tests.** Precedence: a multibuffer whose provider minor declares beats
its major; a major-only declaration resolves; no declaration echoes the
message and does **not** swallow the key; two declarations → first wins
and the second is logged; the shared minor is not active on a plain
source buffer, so `gr` there still reaches LSP references (the
regression that matters most).

## RV.2 — Retrofit 📝

Two lines each: delete the `gr` `keymap_entry!`, add `refresh_action()`.
Handler bodies do not move.

| Crate | Mode | Action |
|---|---|---|
| `lattice-magit` | `magit-core-mode` | `magit-refresh` |
| `lattice-compilation` | `compilation-mode` | `compilation-recompile` |
| `lattice-multibuffer` | `providers::search` | `search-refresh` |

**Tests.** `gr` still refreshes each of the three (behaviour-preserving
— any user-visible change here is a regression); no `chord: "gr"`
remains outside the shared minor and `lattice-lsp`'s nav mode (grep
gate, so copy number four cannot land quietly).

## RV.3 — Close the gaps 📝

- `*problems*` (`providers::problems`) — refresh re-reads the current
  `ErrorList` and rebuilds excerpts. Interacts with
  [`error-list-producers.md`](error-list-producers.md): once the
  language server feeds the list live, refresh is the manual peer of
  that feed.
- Narrow (`providers::narrow`) — decide on merit whether refresh means
  anything for a one-excerpt view of a live buffer. If it does not,
  leaving `refresh_action()` as `None` is a *correct* answer now that
  the absence is spoken aloud rather than silent.

**Tests.** `gr` in `*problems*` picks up entries added since the view
opened.

---

## Deferred

- **`q` = close as a second shared chord.** Same shape, but `q` varies
  far more per view (macro-record in a source buffer, close in magit,
  unbound in compilation / search / problems) and narrow has no obvious
  close semantics. Evaluate separately; do not assume it generalises
  because `gr` did.
