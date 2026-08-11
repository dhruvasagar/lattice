# LSP references view — slice plan

> **Status: Active.** Opened 2026-08-10. Implements
> [`lsp-architecture.md`](../../architecture/lsp-architecture.md) §17:
> references as an editable multibuffer alongside the existing picker.

Design owns *what* and *why*; this file owns *when* and *in what order*.

Depends on [`refreshable-views.md`](refreshable-views.md) RV.1 (LR.3's
`gr` comes from the shared minor, not a fourth copied keymap entry).
Catalogue entry: A.3 in
[`multibuffer-providers.md`](multibuffer-providers.md).

## Status

| Slice | Title | Status |
|---|---|---|
| LR.1 | `lattice-lsp` → `lattice-multibuffer` dep + provider skeleton | ✅ |
| LR.2 | `:lsp-references` — second terminus on the existing drain | ✅ |
| LR.3 | Refresh — origin-anchored re-query | ✅ |
| LR.4 | ~~Version-skew guard~~ — **superseded**, see below | ⛔ |
| LR.5 | `<C-q>` bulk outcome from the picker | 📝 |

LR.5 is separable and generalises past references — it can slip without
blocking anything above it.

---

## LR.1 — Provider skeleton ✅

- `lattice-lsp/Cargo.toml` gains `lattice-multibuffer`. **Assert the
  direction holds**: `lattice-multibuffer` must not gain an LSP dep.
- `lattice-lsp/src/providers/references.rs`, following
  `lattice-multibuffer/src/providers/problems.rs` in shape.
- `LspReferencesService` + `LspReferencesServiceHandle` keyed by
  `BufferId` (origin + last result set), registered in
  `ServiceRegistry`. **Register and look up under the same `T`** — the
  handle alias, not the inner type (the `TypeId` pitfall).
- `LspReferencesMode` minor, identity marker; `on_activate` seeds
  nothing beyond the marker.
- `open_references_view(activator, locations, origin) -> BufferId`,
  ±2 context lines per excerpt, grouped by file.
- `DocumentClosed` cleanup drops the view's service entry.

**Tests.** 8 locations across 3 files → 8 excerpts under 3 file headers;
closing the view drops the service entry; the view is a plain
`BufferKind::Multibuffer` — it must pass
`multibuffer_is_a_regular_buffer.rs` verbatim.

## LR.2 — The second terminus ✅

- `LspRequest::ReferencesView` data arm (not a new `Effect`, not a new
  host `Action` — §16's grain).
- Ex-command `:lsp-references`, one dashed namespaced alias. No
  collapsed spelling, no generic `references` alias, no new 1–2 letter
  short.
- Its handler closure lives in `lattice-lsp` and returns
  `Effect::Lsp(LspRequest::ReferencesView)`. **The async substrate is
  not touched** — §16 rejected mode-side async on heuristic #1 and that
  stands.
- Pending state records the terminus; `drain_pending_references` routes
  to picker or view accordingly.
- Empty result → echo, no empty view.

**Tests.** `gr` still opens the picker (the regression that matters
most); `:lsp-references` opens the view; both in flight → each lands at
its own terminus; **the view appears without a further keypress** (the
wake test — pressing a key first passes on the broken version too).

**GPUI parity.** No new `Effect` variant, so the classifiers are
unchanged — but confirm with
`grep -rn "LspRequest::ReferencesView" crates/lattice-ui-gpui/` and
record the empty result rather than assuming.

## LR.3 — Refresh ✅

- `LspReferencesMode::refresh_action()` → `action:lsp-references-refresh`.
  `gr` arrives from RV.1's shared minor; **no `chord: "gr"` in this
  crate's view mode**.
- Handler returns `Effect::Lsp(LspRequest::ReferencesViewRefresh)`.
- Substrate reads the **stored origin** from the service, not the live
  cursor — the cursor is inside the multibuffer by then and would
  re-query the wrong symbol.
- Result replaces the view's excerpts in place; cursor stays on the same
  excerpt where it still exists.

**Tests.** Add a call site in a source file, `gr` in the view → the new
excerpt appears; refresh re-queries the origin symbol, not whatever is
under the multibuffer cursor (the bug this slice is shaped to prevent);
refresh with the origin file deleted → warn, leave the view intact.

## LR.4 — Superseded ⛔

**Not built here.** Specifying it surfaced that the slice was wrong in
two ways, both discovered by reading the save path rather than the
query path:

- The failure is **data loss, not a stale offset**. `Document::save` on
  a multibuffer writes every dirty source to disk, so a source that
  changed externally gets silently overwritten with the view's stale
  copy plus the edit.
- It is **not references-specific**. `search` and `problems` load from
  disk identically, and the save path is explicitly shared — its own
  comment says "generic for ALL multibuffer views".

A references-only guard would have left two known-identical data-loss
paths in place. Superseded by
[`multibuffer-stale-sources.md`](multibuffer-stale-sources.md)
(SS series); the references view inherits the fix rather than carrying
a copy.

## LR.5 — `<C-q>` from the picker 📝

The discoverable path, and it generalises past references.

- `translate_picker` (a hardcoded host-side router,
  `lattice-host/src/input.rs:500`) gains `<C-q>` → a bulk-outcome
  action.
- The opener supplies the bulk outcome; `PickerSource`s without one make
  `<C-q>` echo "not available for this picker" rather than doing
  nothing.
- References supplies "open these locations as a view".

**Tests.** `<C-q>` in a references picker opens the view with the
**filtered** result set, not the unfiltered one; a picker with no bulk
outcome echoes; `<C-q>` does not leak to the buffer beneath.

---

## Deferred

- **A chord that opens the view directly.** `gR` is vim's Virtual
  Replace mode — unimplemented in lattice, so binding it would foreclose
  the slot rather than collide with it. Revisit only if LR.5 proves
  insufficiently discoverable in practice.
- **The rest of the LSP provider family** — A.9 workspace symbols, A.8
  refactor preview, A.10/A.11 hierarchies. Each is the same shape as
  LR.1–LR.3; do one first and see what the second one wants to share
  before extracting anything.
