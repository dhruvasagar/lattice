# PU-A — generic popup primitive (sub-slices)

Decomposition of the PU-A slice (`acp-ux-enhancements.md` → "Slice PU-A") into
landable, independently-green steps. **Design ref:** `../../../architecture/popup-api.md`.
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

### PU-A.1b-i — modal set-on-open / restore-on-dismiss ✅
Split out from 1b (the §5 modal defect), landable independently of the primitive.
- `PrevPaneState` gains `modal: ModalState`, set at all three focus-steal capture
  sites (`open_popup`, `focus_help_popup`, `activate_help_in_pane` — the last
  inert, torn down by `do_close_pane`).
- `open_popup` + `focus_help_popup` (the two overlay Steal paths) set
  `ModalState::Normal` on open; `dismiss_popup` restores `prev.modal`. Passive
  floats leave prev `None` and never touch modal.

Tests: `steal_popup_normalizes_modal_and_dismiss_restores_it`,
`passive_popup_leaves_modal_untouched`.

### PU-A.1b-ii — `open_popup_buffer` primitive + `PopupFocus` ✅
The structural core (mapping done — see below). Tests:
`open_popup_buffer_steal_focuses_arbitrary_buffer`,
`open_popup_buffer_passive_does_not_steal_focus`; full help-regression gate
green (2335). The refactor is invisible to the user.
- Add `PopupFocus { Steal, Passive }` to `lattice-core::ui::popup`.
- Add `Editor::open_popup_buffer(BufferId, PopupPlacement, PopupFocus)` — the
  content-agnostic entry (generic mechanics: dismiss-stale, position-history,
  snapshot-active-pane, prev+modal capture, popup_anchor, set popup fields; Steal
  additionally flips `active_buffer`/cursor/scroll/modal). Rework `open_popup`
  (Steal) / `open_floating_popup` (Passive) into thin help callers that
  materialise the help buffer via `register_help_document` + push the back-stack
  + activate the help/hover modes, then delegate. In-pane help stays separate.

**Map (verified):** the help-specific bits are the `active_buffer==Help` reuse
branch + `snapshot_current_popup`/`popup_back_stack`/`swap_popup_content`,
`register_help_document`, and the mode activation (Help-major for Steal;
markdown+help+hover for Passive). Generic bits are everything else. All
production opens funnel through `Editor::display_buffer` (dispatch.rs); no
`lattice-ai` caller exists yet. `open_popup_buffer` takes an already-registered
`BufferId` (aligning with `Effect::OpenSyntheticBuffer`'s "buffer exists; wire
it" shape); each content kind registers its own `BufferData` variant.

### PU-A.2 — move help state into `lattice-help` ✅
`PopupSnapshot` moved from `lattice-host::popup` into `lattice-help` (alongside
`HelpMetadata`, which already lived there) and re-exported from
`lattice-host::popup`, so every `crate::popup::PopupSnapshot` reference stays
valid — a pure type relocation, no logic change. `popup_back_stack` stays a field
on `Editor` (a field can't move crates); its element type now resolves to the
relocated `lattice_help::PopupSnapshot`. 219 popup/help tests green.

### PU-A.3a — rename `Effect::CloseHover` → `Effect::DismissPopup` ✅
Cosmetic de-Help-ification of the dismiss effect (it already calls
`dismiss_popup()` generically). Grammar `effect.rs` + `ex_commands.rs`, both
renderers' effect arms, WIT `types.wit` (`close-hover` → `dismiss-popup`), and
`boundary_effect.rs` `to_wit`/`from_wit` + round-trip test. Compiler-guarded but
touches the WIT contract. `Action::CloseHover` is a separate layer — out of scope.

### PU-A.3b — `Effect::OpenPopup` ✅ (shipped in PU-B, 2026-07-12)
**Decision (2026-07):** deferred out of PU-A because the effect had NO producer or
consumer until the ACP permission menu — landing an `Effect` variant + WIT
round-trip + host handler that nothing exercises end-to-end is untested dead
surface (heuristic #1 / YAGNI). **Landed in PU-B** (`acp-ux-enhancements.md`
PU-B.1 + PU-B.2b-ii) with its consumer: `Effect::OpenPopup { name, mode_id,
placement, focus }` (reshaped from the `{ buffer: BufferId }` design shape — the
emitters have no services to supply an id) wrapping the already-generic
`open_popup_buffer` primitive PU-A.1b-ii landed. The layering payoff
(popup-api.md §4.5) is preserved.

### PU-A.4 — chrome from the buffer name ✅
Title from the buffer's synthetic name, not `help.title`; the `"Esc to dismiss"`
hint stays. GPUI already sourced the title from `name_of(popup_id)` (PU.1b-4b);
this brought the TUI (`render.rs`) into line — it read `help.title`, now reads
`app.buffers().registry.name_of(popup_id)`. `register_help_document` sets
`name: Some(buffer.title)`, so the swap is invisible (PU-A gate). Full
`lattice-ui-tui` suite green (1567).

## Sequencing
1a → 1b → 2 → 3 → 4. 1a and 2 are independent cleanups; 1b is the load-bearing
core the rest build on; 3 and 4 de-Help-ify the effect + chrome surfaces. Help
regressions (`:help`, `q`/`<Esc>`, `<C-o>` back-stack, floating hover) are the
acceptance gate throughout — PU-A must be invisible to the user (popup-api.md §7).
