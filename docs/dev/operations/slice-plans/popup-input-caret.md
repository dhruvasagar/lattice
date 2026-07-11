# Popup input & caret fixes

Three pre-PU-A defect fixes in the popup surface, found while scoping PU-A
(`popup-api.md`). Each is a symptom of the same half-migration the popup-api
generalisation exists to finish: the popup is "active" for `active_buffer` /
`active_buffer_id()` / cursor / scroll, but **not** for the caret's matrix
source, the fold hot-slot, or global-chord suppression — so those slices leak to
the buffer behind the popup.

**Status:** ✅ complete (PIC.1, PIC.2, PIC.3). **Design ref:** `../../architecture/popup-api.md` (§5
defects). **Feeds:** PU-A (`acp-ux-enhancements.md`). **Ships no user feature** —
these are correctness fixes; the acceptance gate is "popup behaves like a focused
buffer, background is never touched."

## Root causes (verified)

- **Caret drift.** The popup body + cursorline compose from the `PaneId::POPUP`
  matrix, but the caret row (`render.rs` `buffer_line_to_visible_row_with`,
  ~5872) walks the **top-level** `display_matrix` = `display_matrix_for(document_buffer_id)`
  (the background doc) plus the active *document* pane's virtual rows. `open_popup`
  never repoints `document_buffer_id` (dispatch.rs:21730-21736), so the caret sums
  the wrong per-line wrap widths — error compounds per line, diverging downward.
  TUI only; GPUI computes the caret from the popup matrix and disables the popup
  cursorline.
- **`z<Space>`/`z<Tab>` leak.** The read-only-help action guard (dispatch.rs:2006)
  only consumes `action_is_document_mutation` actions; `CycleFoldAtCursor` /
  `CycleFoldsGlobal` are absent (1892-1897), so they reach their handlers, which
  mutate `self.folds` — a hot-slot keyed to `document_buffer_id` (the background
  doc), never swapped for the popup. `zo`/`zc`/`za` are in the list, hence "not
  all bindings" leak.
- **`gt` fires + popup follows.** `gt`→`NextTab` is a universal Builtin chord with
  no popup gate; `popup_buffer` is a global overlay field outside `pane_tree`, so a
  tab swap leaves it intact and it re-renders over the new tab.

## Why not "popup owns a keymap layer"

The keymap trie has **no consume-all**: a mode layer only shadows chords it binds;
unbound chords always fall through to Builtin (`registry.rs:494-527`). `HelpMode`
binds zero chords. Exclusive/selective input capture in lattice is done at the
**dispatch seam** (the existing read-only-help action guard) or a `translate`
intercept — both already popup-aware (`active_buffer == Help`) and therefore
self-scoping (they stop applying the instant the popup closes). The gate below
extends the guard we already have, so motions/scroll keep flowing through Builtin
onto the popup's cursor while world-escaping actions are consumed.

---

## Slice PIC.1 — Popup caret reads the POPUP matrix ✅

**Depends on:** nothing. **Renderer:** TUI (GPUI already correct — it computes
the popup caret from the popup matrix and disables the popup cursorline).

| File | Change |
|---|---|
| `crates/lattice-ui-tui/src/render.rs` | `cursor_screen_position_at` / `buffer_line_to_visible_row_with` take a `pane_id` and read that pane's `display_matrix_for_pane` + virtual-rows matrix + sticky rows, instead of the top-level `cells.display_matrix` and `tree.active().id`. The popup caret call site passes `PaneId::POPUP`; all document callers pass `tree.active().id` (byte-identical to the top-level matrix — behaviour-preserving). |

Test (`lattice-ui-tui`): `focused_popup_caret_row_matches_cursorline_row` — in a
focused popup whose line 0 wraps into several segments over a single-segment
background line, the caret for line 1 lands on the cursorline row via the POPUP
matrix; the `doc_y < cursorline_row` assertion pins the pre-fix drift source.
Full TUI suite green (1567).

**Risk:** low. Isolated to the popup caret path; no input or state changes.

## Slice PIC.2 — Popup input gate ✅

**Depends on:** PIC.1 (independent, but sequenced after so a caret regression
bisects cleanly). **Renderer-agnostic** (host `handle_action`; GPUI inherits it).

| File | Change |
|---|---|
| `crates/lattice-host/src/dispatch.rs` | Added `CycleFoldAtCursor`/`CycleFoldsGlobal` to `action_is_document_mutation` (bug 2 — they mutate `self.folds`, so the existing read-only-help guard now consumes them like `zo`/`zc`/`za`). Added `action_escapes_focused_popup` (tab + pane-tree nav) and a second guard: when a popup is focused (`active_buffer == Help && popup_buffer.is_some()`), those escaping actions are silently consumed (bug 3). Gated on `popup_buffer.is_some()` so in-pane Help / Dashboard (real panes) keep normal nav. Motions/scroll/search/dismiss/follow-link flow through onto the popup's cursor. |

Whitelist-vs-denylist: motions dispatch via `Action::Invoke(grammar)` (opaque at
the guard), so a *pure* popup-safe whitelist is not cleanly expressible there —
the implemented form consumes the enumerable world-escaping set and lets
everything else through. Net user-visible behaviour matches the whitelist intent:
only popup-meaningful keys act.

**Deferred to PU-A:** `open_popup` modal normalization (`self.modal = Normal` on
open + save/restore via `PrevPaneState`). Not needed for bugs 2 & 3 (both
Normal-mode); it only matters for a popup auto-opened mid-Insert (the ACP menu,
PU-B), and doing it right pulls in the `PrevPaneState { modal }` + `dismiss_popup`
restore that belongs to the PU-A generalisation.

Tests (`lattice-host`): `fold_cycles_are_document_mutations`,
`escape_predicate_covers_tab_and_pane_nav_only`,
`popup_focused_consumes_tab_switch`, `tab_switch_works_without_a_focused_popup`
(non-regression), `popup_focused_consumes_fold_cycle_as_read_only`. Full host
suite green (762).

**Risk:** medium. Touches the dispatch guard, a hot path — but the guard is
already the sanctioned popup-aware seam; the change is additive and self-scoping.

## Slice PIC.3 — Fold the defects into the design ✅

**Depends on:** PIC.1, PIC.2.

| File | Change |
|---|---|
| `docs/dev/architecture/popup-api.md` | Added the three (caret-matrix source, `self.folds` popup mismatch, global-chord suppression) to §5 "Defects the generalisation must fix", framed as facets of the same half-migration, and noted the action-guard as the input-gate seam so PU-A's full generalisation inherits them. |

**Risk:** none (docs).

---

## Sequencing

PIC.1 → PIC.2 → PIC.3. Each lands green and separately. PIC.1 is pure render and
independent; PIC.2 is the input correctness core; PIC.3 records the design debt.
The `active_minor_modes` → `active_buffer_id()` scoping alignment (a real latent
inconsistency vs `dispatch_chord`, but one that also shifts Terminal/Oil/FileTree
scoping) is **deferred to full PU-A** — not needed for these three, and it widens
blast radius.
