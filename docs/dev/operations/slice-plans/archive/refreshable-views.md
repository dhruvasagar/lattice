# Refreshable views — slice plan

> **Status: ✅ COMPLETE (RV.1–RV.3, closed 2026-08-21).** Opened
> 2026-08-10. Implements
> [`mode-architecture.md`](../../../../architecture/mode-architecture.md) §5.5:
> `gr` as one shared chord over a `Mode::refresh_action()` declaration,
> replacing the copied keymap entries.
>
> Archivable. The "Deferred" section at the foot is **not** an open
> slice: `q`-as-a-second-shared-chord was evaluated and explicitly
> declined ("do not assume it generalises because `gr` did"), which is a
> rejected alternative, not postponed work.

Design owns *what* and *why*; this file owns *when* and *in what order*.

## Status

| Slice | Title | Status |
|---|---|---|
| RV.1 | `Mode::refresh_action()` + `refreshable-view-mode` + generic dispatch | ✅ |
| RV.2 | Retrofit the existing copies (five, not three — see below) | ✅ |
| RV.3 | Close the gaps — `*problems*`, narrow | ✅ |

> **This table read `📝` for RV.2 and RV.3 until 2026-08-21, while both
> sections below already read ✅ and both were implemented.** Verified
> against source: `Mode::refresh_action` (`lattice-mode/src/mode.rs`),
> `refreshable_view_mode.rs`, the auto-activation in `registry.rs`, the
> seven consumers that declare a target, and
> `lattice-host/tests/gr_is_declared_once.rs`. Recorded rather than
> quietly fixed, because a status table that disagrees with its own body
> is the failure mode the archiving rule warns about — icons drift, and
> the drift is what makes a plan look open when it is done.

> **RV.1 landed 2026-08-10.** Two things the design got wrong and the
> code corrected (both fixed in `mode-architecture.md` §5.5):
> `Option<ActionId>` does not exist — actions are `&'static str` names,
> so the signature matches `mirrors_option()`; and **resolution had to
> move host-side**, because `ActionContext` carries no active-mode set
> (it lives on `Editor`, not the `ServiceRegistry`), so a mode-side
> handler cannot do the walk. `Editor::resolve_refresh_action` is the
> sibling of `resolve_invocation_runner` — same walk, different table.

RV.1 before RV.2 (the seam must exist to migrate onto). RV.3 is
independent of RV.2 and can land either side of it.

---

## RV.1 — The seam ✅

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

## RV.2 — Retrofit ✅

Two lines each: delete the `gr` `keymap_entry!`, add `refresh_action()`.
Handler bodies did not move.

| Crate | Mode | Action |
|---|---|---|
| `lattice-magit` | `magit-core-mode` | `magit-refresh` |
| `lattice-compilation` | `compilation-mode` | `compilation-recompile` |
| `lattice-multibuffer` | `providers::search` | `search-refresh` |
| `lattice-plugin-manager` | `plugins-mode` | `plugins-refresh` |
| `lattice-notify` | `notifications-mode` | `notification-refresh` |

> **There were five copies, not three.** `lattice-plugin-manager` and
> `lattice-notify` also declared their own `gr`; neither was in the
> design's inventory, and that inventory was written *by reading the
> code*. They surfaced only because the retrofit grep swept the whole
> tree. So the count stands at five duplicated and two missing
> (`*problems*`, narrow) — and an inventory taken by hand missed 40% of
> the duplicates. Precisely what "a gap in a copied set does not
> announce itself" predicts, applied to the audit as well as the code.

`providers::search` lost its `keymap()` entirely — `gr` was its only
chord.

**Tests.** `gr` still refreshes each (behaviour-preserving; any
user-visible change here is a regression), and
`lattice-host/tests/gr_is_declared_once.rs` walks the tree asserting
`chord: "gr"` appears only in `refreshable_view_mode.rs` and
`lattice-lsp/src/modes.rs`.

**Why that test greps source rather than driving keys.** A behavioural
test cannot catch this bug class: it would have passed on all five
copies, and passed just as happily on the two views that had none. The
property worth pinning is "declared once", so the test asserts that
directly. Copy number six is a failing test, not a discovery months
later.

## RV.3 — Close the gaps ✅

**`*problems*` refreshes in place.** `AppEffect::ProblemsRefresh` →
`refresh_problems_view`, which rebuilds sources + excerpts from the
current `ErrorList` and swaps them atomically via `replace_excerpts`.

It is **not** a re-fire of `ProblemsOpen`: `create_multibuffer_view`
mints a fresh `BufferId` on every call, so re-opening would strand the
view the user is looking at and add a second `*problems*` buffer. A test
pins the buffer id across a refresh, and another pins that exactly one
buffer is ever inserted.

An empty or all-unreadable refresh **leaves the view untouched** and
echoes. Blanking the buffer the user is reading in order to say "no
results" is the wrong trade.

Interacts with [`error-list-producers.md`](error-list-producers.md):
once the language server feeds the list live (EP.3), refresh becomes the
manual peer of that feed — and the one that still matters when
`lsp.diagnostics-to-error-list = false`.

**Narrow declares no refresh, deliberately.** It is one excerpt over a
*live* buffer, subscribed to the source's `DocumentChanged` events, so
it recomposes as the source is edited and is never stale — there is
nothing a refresh could do. `None` is now a statable answer rather than
an oversight, precisely because the shared `gr` echoes "nothing to
refresh here" instead of swallowing the key.

**Tests.** Refresh picks up entries added since the view opened
(including in a file the view had never seen); the buffer id survives;
no second buffer is inserted; empty and all-unreadable refreshes leave
the view intact.

---

## Deferred

- **`q` = close as a second shared chord.** Same shape, but `q` varies
  far more per view (macro-record in a source buffer, close in magit,
  unbound in compilation / search / problems) and narrow has no obvious
  close semantics. Evaluate separately; do not assume it generalises
  because `gr` did.
