# PU-A — generic popup primitive (sub-slices)

Decomposition of the PU-A slice (`acp-ux-enhancements.md` → "Slice PU-A") into
landable, independently-green steps. **Design ref:** `../../architecture/popup-api.md`.
PU-A is the largest/riskiest of the follow-on slices — it refactors a working,
widely-used surface and ships no user feature, so each sub-slice lands green and
separately for clean bisection.

The goal (popup-api.md §3): a popup shows a *registered buffer* at a placement
with a focus mode; it owns no content, behaviour, or result. Today
`open_popup`/`open_floating_popup` are `HelpContent`-typed and branch on
`BufferKind::Help`.

## Sub-slices

### PU-A.1a — dismiss correctness + rename ✅
Self-contained §5 fixes that don't need the primitive yet.
- `dismiss_popup`: State-A dismiss no longer clobbers `active_buffer` to
  `Document` — a floating popup over oil / file-tree / dashboard kept its prior
  kind (State A never flips `active_buffer`, so the clobber was pure bug).
- Rename `prev_pane_for_help` → `prev_pane_for_popup` (field + all sites across
  lattice-host + lattice-ui-tui).

Tests: `state_a_dismiss_preserves_underlying_buffer_kind`,
`state_b_dismiss_restores_prev_pane` (non-regression).

### PU-A.1b — `open_popup_buffer` primitive + `PopupFocus` 📝
The structural core. Requires full mapping of `register_help_document`,
`swap_popup_content`, `snapshot_current_popup`, `activate_help_in_pane`, and the
async floating openers first.
- Add `PopupFocus { Steal, Passive }` to `lattice-core::ui::popup`.
- Add `Editor::open_popup_buffer(BufferId, PopupPlacement, PopupFocus)` — the
  content-agnostic entry. Rework `open_popup` (Steal) / `open_floating_popup`
  (Passive) into thin `open_help_popup` callers that materialise the help buffer
  + push the help back-stack, then delegate.
- `PrevPaneState` gains `modal: ModalState`; `PopupFocus::Steal` sets
  `ModalState::Normal` on open; `dismiss_popup` restores `prev.modal`. (Needed so
  a popup opened mid-Insert — the ACP menu, PU-B — gets its bindings; carefully
  traced against every capture/teardown site.)

### PU-A.2 — move help state into `lattice-help` 📝
Move `PopupSnapshot` + `HelpMetadata` + `popup_back_stack` out of `lattice-host`
into `lattice-help` (they are help's `<C-o>` history, not popup state).

### PU-A.3 — effects: `OpenPopup` + `DismissPopup` 📝
- `Effect::OpenPopup { buffer, placement, focus }` (grammar).
- Rename `Effect::CloseHover` → `Effect::DismissPopup` (generic already).
- Both renderers' effect-classifier arms (lockstep, exhaustive match).
- WIT `effect` variant + `to_wit`/`from_wit` arms (`boundary_effect.rs`).

### PU-A.4 — chrome from the buffer name 📝
Title from the buffer's synthetic name, not `help.title`; the `"Esc to dismiss"`
hint stays. Both TUI + GPUI in lockstep.

## Sequencing
1a → 1b → 2 → 3 → 4. 1a and 2 are independent cleanups; 1b is the load-bearing
core the rest build on; 3 and 4 de-Help-ify the effect + chrome surfaces. Help
regressions (`:help`, `q`/`<Esc>`, `<C-o>` back-stack, floating hover) are the
acceptance gate throughout — PU-A must be invisible to the user (popup-api.md §7).
