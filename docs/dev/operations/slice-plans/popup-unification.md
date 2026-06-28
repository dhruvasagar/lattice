# Popup unification — slice plan

Design fragment: `docs/dev/architecture/popup-unification.md` (the
*what* and *why* — chrome-vs-content split, the registry-buffer model,
the rejected ad-hoc-snapshot alternative). This file owns the *when*
and *in what order*.

Goal: every popup's **content** renders through `compose_pane_lines`
(TUI) / `EditorElement` (GPUI); only the **box** (chrome) stays
popup-specific. Outcome: folding, soft-wrap, horizontal scroll,
syntax, and decorations work in popups for free, and no popup path
contains bespoke text-layout code.

Status icons: ✅ done · 🚧 in progress · 🗒 planned. All slices below
are 🗒 (not started).

## Sequencing rationale

Help first (PU.1/PU.2): it is the most-used popup and the one with the
most divergence (`manually_wrap_lines`, `draw_help_in_pane`, the GPUI
manual cell loop), and it already owns a rope-backed `Buffer`, so it
proves the registry-buffer seam end-to-end with the least new content
modelling. Transient popups (hover/signature/docs) follow once the
seam + the ephemeral-buffer class exist.

## Locked decision (2026-06-27): full conversion (α) + 2A highlights

The PU.1 internal shape is **α — full conversion**: help becomes an
actor-backed synthetic Document outright (`BufferData::Help` carries a
`DocumentEntry`, exactly like `BufferData::Messages`). `HelpBuffer`
storage is retired; content lives once in the Document; the popup's
view state (scroll/cursor) routes through the same `self.cursor` /
`self.scroll` it already uses when focused; motions come from the
normal grammar path; and the `active_text` / `active_cursor` /
`active_buffer_id` `BufferKind::Help` branches are replaced by
**focus-state routing** (a focused popup → `popup_buffer`) that names
no kind. Chosen over the dual-backed transitional (β, duplicates
content — a fresh drift seam) and the single-rope-keep-motions
intermediate (γ); α is the endgame with no half-migration residue
(paramount #3, heuristic #1, `feedback_mode_owns_its_surface`).

Highlights take path **2A**: help is a real markdown Document, so its
`DisplayMatrix` is built by the cells worker from a live markdown
`SyntaxHandle` like any document — the `with_markdown_syntax`
precompute + `popup_help_highlights` read path are deleted, not
special-cased in compose (which 2B would do, re-introducing a K.4
kind-branch + leaving the highlight source as drift). Pixel-identical
today because `with_markdown_syntax` already runs the same grammar.

Sub-slices: **PU.1a** (storage/cursor/motion/kind-branch conversion,
green, no visual change — bespoke renderers keep painting via a
reconstructed `HelpBuffer` *view* built from the Document) → **PU.1b**
(compose seam + markdown handle + delete bespoke renderers) →
**PU.1c** (mop up any residual focus-routing edge cases the render
switch exposes). PU.2 (GPUI parity) unchanged.

## PU.1a — Help → actor-backed Document ✅

Landed 2026-06-28. Help content is now `BufferData::Help(DocumentEntry)`
— an actor-backed synthetic Document seeded by
`Editor::register_help_document`, exactly like `*messages*`. Title →
registry `name`; links/anchors/highlights → `buffer_locals`. The popup
view state (scroll/cursor when help is NOT focused, plus the focus
stash) moved off the retired `HelpBuffer.{scroll,cursor}` registry
fields onto `Editor::{popup_scroll,popup_cursor}` (a faithful
relocation of the prior registry-cursor behaviour — `snapshot_active_pane`
syncs them identically). `popup_help()` survives as a view
reconstructor (`BufferRegistry::help_content_view` + the popup stash) so
the bespoke renderers paint unchanged this slice; PU.1b deletes both.
`active_text` / `active_cursor` / `active_buffer_id` route through
`popup_buffer`/`popup_help()` (Document-backed), and HelpBuffer's motion
methods + their tests are gone (motions come from the grammar path).
No visual change; full suite green (lattice-help 39, lattice-host 590,
lattice-ui-tui 1499, 0 failed).

Two pre-existing branch breakages were fixed while landing this slice
(both unrelated to popup unification): the GPUI `EditorElement::paint`
`self.wrap_width` → `prepaint.wrap_width` field error from HS.1b (broke
`--features window`), and a stale `lattice-help` markdown test asserting
`##` → `Heading1` (the T-series per-level heading query makes it
`Heading2`).

- Add `SyntheticDocVariant::Help` → `BufferData::Help(DocumentEntry)`;
  spawn/seed help content as a Document (mirror
  `ensure_named_synthetic_doc_with_variant`). Title → registry `name`;
  metadata (links/anchors/highlights) → `buffer_locals` (already the
  `HelpLinks`/`HelpAnchors`/`HelpHighlights` slots).
- Rewrite the registry accessors (`help`/`help_mut`/`with_help`/
  `with_help_mut`/`help_with_title`/`contains_help`/`help_ids_sorted`)
  onto the `DocumentEntry`. `popup_help()` survives PU.1a as a
  **view reconstructor** (builds a transient `HelpBuffer` value from
  the Document snapshot + popup scroll/cursor) so the bespoke
  renderers need zero change this slice — PU.1b deletes both.
- Rewrite the popup state machine off `HelpBuffer` storage:
  `open_popup` / `open_floating_popup` / `open_help_in_pane` /
  `swap_popup_content` / `snapshot_current_popup` (back-stack) /
  `dismiss_popup` / `focus_help_popup`.
- Replace the `BufferKind::Help` branches in `active_text` /
  `active_cursor` / `active_buffer_id` with focus-state routing.
- Delete `HelpBuffer`'s motion methods + their lattice-help tests
  (motions now come from the grammar path).
- **Acceptance:** no visual change; `:help` / `:describe-*` / hover /
  back-stack (`<C-o>`) / dismiss-on-Esc all behave as before; full
  test suite green.

## PU.1b — Compose seam + markdown handle + delete bespoke 🚧

PU.1b is sub-carved into five sequenced slices (1a → 1 → 2 → 3 → 4)
after the seam investigation surfaced two render states + three open
forks the original PU.1b text glossed over.

**Reorder (2026-06-28):** the markdown `SyntaxHandle` attach moved to
the *front* (now PU.1b-1), ahead of the renderer flips. The original
order put the flips first and "kept `with_markdown_syntax` highlights
for now", but that is incompatible with the locked **2A** decision: once
a renderer reads the cells-worker `DisplayMatrix` (what `compose_pane_lines`
does), the only K.4-clean highlight source is a live grammar handle on
the buffer — sourcing highlights from the bespoke `with_markdown_syntax`
precompute would force a kind-branch in compose (the rejected 2B). So the
handle must be attached *before* either renderer flips, or the flip
regresses help's markdown colour. Attaching it first is invisible
(bespoke renderers still read `HelpHighlights`), so it lands as its own
green slice. The precompute is deleted last (PU.1b-4), once nothing reads
it.

### Locked decisions (2026-06-28)

- **Fork 1 — State-A floating popup gets a *real* DisplayMatrix.** The
  floating help popup is an overlay, not a pane, so the cells worker
  builds no per-pane `DisplayMatrix` for it. Per the design fragment §4
  (ad-hoc-snapshot path REJECTED), the consistent fix is to give the
  popup buffer cells-worker coverage (a hidden/virtual pane-like
  registration keyed by a synthetic pane id) so BOTH states route
  through `compose_pane_lines` reading a real matrix. Confirmed by
  Dhruva. Protects paramount #3 (no parallel resolution path).
- **Fork 3 — gutter/signs/numbers are OPTION-derived; the renderer is
  kind-blind.** The renderer must NEVER branch on "is this help / a
  popup / a pane / a tab." Line-number gutter, diagnostics-severity
  column and diff-sign column are all derived from resolved options;
  help-mode is just a mode that sets those options to clean values. A
  regular buffer with `:set nonu signcolumn=no` MUST render identically
  to a help popup. This is `feedback_buffers_no_special_case` (K.4)
  applied to gutter geometry — see `feedback_render_is_option_derived`.
  - Sign-column model = **single vim-convention `signcolumn` option**
    (A) gating both the diag + diff columns, default **`yes`**
    (always-reserve = today's no-flicker behaviour), help-mode sets
    `no`. Chosen over granular per-column options (B) on heuristic #2's
    UX-convention carve-out + heuristic #1 (simpler long-term). `auto`
    deferred (re-introduces layout shift).

### PU.1b-1a — option-driven gutter/signs infra ✅ (2026-06-28)

Pure infra; NO help-render change, NO visual change (default `yes`
keeps every document byte-identical). Makes `signcolumn` work for
regular buffers and lets help-mode declare clean values. Green:
config 138 (signcolumn 5), host 590, lattice-mode (help-mode count→5),
TUI 1500 (existing compose tests byte-identical + 1 new), GPUI gutter 5
(+1 new) — GPUI `--features window` builds clean. GPUI visual pass on
`:set signcolumn=no` left to a `cargo run --features gui` check
(HS.1b precedent; the default-`yes` path is byte-identical by
construction, so existing GPUI rendering cannot regress).

- ✅ `SignColumn` value type (`yes`/`no`, default `yes`) +
  `signcolumn`/`scl` option registered (`crates/lattice-config/src/
  signcolumn.rs`, `core_options.rs`, `lib.rs`). 5 unit tests.
- ✅ `OptionCache.sign_column` (state.rs) + `rebuild_option_cache`
  (`resolved_option::<SignColumnOption>().reserved()`) + Default.
- ✅ TUI per-buffer accessors `sign_column_for` / `wrap_lines_for`
  (`app/options.rs`, mirror `show_line_numbers_for`); `FrameView`
  gains `sign_column: bool` (resolved in BOTH `from_app` + `for_buffer`)
  and `for_buffer.wrap_lines` fixed to per-buffer (`wrap_lines_for`).
- ✅ `compose_pane_lines`: a single `sign_columns_width(view)` helper
  (= `DIAG_GUTTER_WIDTH + DIFF_SIGN_GUTTER_WIDTH` when `view.sign_column`
  else `0`) gates ALL sites: `buffer_w`, the cursor-position
  `wrap_width` + `col`, and the per-line prefix vec
  `vec![severity_cell, diff_sign_cell]` + the wrap-continuation prefix
  → empty vec when `no`.
- ✅ **GPUI parity (same patch):** `EditorElement.sign_column` (sourced
  from the active doc's `option_cache.sign_column`, mirroring how GPUI
  reads `foldenable`) gates `gutter_chars`, `format_gutter_text`,
  `build_gutter_runs`, `shaped_continuation_gutter` /
  `push_wrapped_doc_row` (`editor_element.rs`).
- ✅ help-mode `options()`: `Number = false` + `SignColumnOption =
  SignColumn::No` (count-3 test → 5).
- ✅ Tests: `signcolumn_no_and_nonumber_drops_sign_and_number_columns`
  (TUI compose) + `gutter_text_signcolumn_no_drops_severity_and_diff_cells`
  (GPUI); default-`yes` body stays byte-identical (existing suite).

### PU.1b-1 — attach markdown SyntaxHandle to help ✅ (2026-06-28)

Landed `30c7aa87`. Green: lattice-host 590, lattice-ui-tui 1501 (+1).
Pure foundation; NO renderer change, NO visual change (bespoke
renderers still read `HelpHighlights`). `register_help_document`
(`crates/lattice-host/src/synthetic_buffers.rs`) attaches a live
markdown `SyntaxHandle` via
`install_inmemory_syntax(id, &text, Path::new("help.md"))` so the cells
worker builds the help `DisplayMatrix` from the SAME grammar
`with_markdown_syntax` precomputes — the seam PU.1b-2/-3 flip the
renderers onto. `install_inmemory_syntax` widened to `pub(crate)`.

- Attach is at initial registration only. The swap seam
  (`replace_owned_document_text`: back-stack / link-follow / in-pane
  re-seed) reuses the id but does NOT re-attach — PU.1b-2 must confirm
  the matrix refreshes after a swap (re-attaching there if the
  version-bump reparse alone doesn't suffice).
- Test: `help_buffer_gets_markdown_syntax_handle` (TUI) asserts
  `document_syntax_for(help_id).is_some()` after open.
- No GPUI / `Effect` / `DiffSignKind` / `host_theme` surface touched
  (host-side buffer registration only) — parity rule N/A this slice.

### Fork 4 — help-link styling (locked 2026-06-28, Option D)

The 2A "pixel-identical" claim overlooked help **links**:
`extract_links_and_clean` strips the `[label](target)` markup *before*
the markdown grammar runs (the user sees only `label`), then
`overlay_link_styles` pushes `Style::Link` spans onto the label ranges.
The cells-worker `DisplayMatrix` is built from the grammar on the
*stripped* text, so it has no link spans — flipping in-pane help to the
matrix would drop link styling (a visible regression). **Option D
(matrix static-span merge)** chosen over Option A (per-renderer overlay
band): help-link styling is *static* (content-fixed), so it belongs in
the matrix's single styling source, not a dynamic per-frame overlay.

> **UX:** pixel-identical, no regression (links keep `Style::Link`).
> **Paramount #1 (perf):** merge runs once off-thread on matrix build,
> O(visible lines), gated to buffers that carry the local; UI thread
> does zero extra work.
> **Paramount #3 (everything-is-a-buffer):** property-derived generic
> `ExtraHighlights` local any buffer can carry — precedent is K.4.7
> `excerpt_syntax` feeding the same build. No kind-branch.
> **Heuristic #1 (long-term fit):** the display-line model's invariant
> is "the matrix is the single source of glyph styling"; static styling
> belongs in it, dynamic styling stays an overlay. Genuinely-better fit.
> **Parity (`feedback_tui_gpui_parity`):** best — both renderers read
> the shared matrix, so the merge needs zero GPUI code (no drift seam).

### PU.1b-2a — generic ExtraHighlights matrix-merge infra ✅ (2026-06-28)

Pure infra, INVISIBLE (no buffer carries the local yet → byte-identical
for every pane). The generic seam Fork 4 needs.

- ✅ `ExtraHighlights(Vec<Vec<StyledSpan>>)` buffer-local (`modes.rs`),
  property-derived (owner `text-mode`, not help).
- ✅ `build_display_matrix` gains an `extra_spans` param; the
  `highlight_range` closure PREPENDS them onto the grammar spans (wins
  `style_at_byte`'s first-match), incl. a no-grammar branch so a
  handle-less buffer still shows static styling. `merge_extra_spans` +
  `extra_spans_version` helpers.
- ✅ `PaneCellsInputs.extra_spans`; `build_cells_panes` reads the local
  and folds `extra_spans_version` into `MatrixVersion::syntax` so a
  re-seed invalidates the cache. Incremental rebuild gated off when
  `extra_spans` is non-empty (it reuses lines verbatim).
- ✅ GPUI parity FREE (Option D): both renderers read the merged matrix;
  no GPUI code touched.
- ✅ Test: `extra_spans_merge_into_display_matrix_runs` (host) — a Link
  span reaches the matrix runs and overrides; empty lines untouched.

### PU.1b-2b — flip in-pane help (State B) to compose ✅ (2026-06-28)

In-pane help now renders through the generic document compose path
(matrix carries markdown colour from PU.1b-1 + link styling from the
PU.1b-2a seam). Green: lattice-host 591, lattice-ui-tui 1503 (+2),
lattice-help 39, 0 failed.

- ✅ `BufferKind::Help` added to the `build_cells_panes` filter so
  in-pane help panes get a `DisplayMatrix` like any document.
- ✅ `seed_help_metadata_locals` is now the SINGLE seed/attach point
  (called on initial open AND all swaps: `register_help_document`,
  `pop_popup_back`, `swap_popup_content`, `open_help_in_pane` re-seed).
  It seeds `ExtraHighlights` from `lattice_help::link_highlights(&links)`
  AND re-attaches the markdown `SyntaxHandle` from the buffer's current
  text — so a link-follow / `<C-o>` swap refreshes both grammar colour
  and link styling. The explicit attach in `register_help_document`
  (PU.1b-1) was removed (the swap seam `replace_owned_document_text`
  stays syntax-agnostic).
- ✅ `help_pane_render` forwards to `draw_buffer` /
  `draw_inactive_document` (the provider stays only for
  `help_pane_status`). **Deleted** `draw_help_in_pane`,
  `draw_inactive_help`. A help pane is now pixel-equivalent to a
  `:set nonu signcolumn=no wrap` document.
- ✅ **In-pane help now WRAPS** (help-mode already sets `Wrap = true`,
  with the stated intent "long help bodies should wrap"; the bespoke
  `draw_help_in_pane` ignored it). This is an improvement aligned with
  the declared option, not a regression — long help lines wrap instead
  of overflowing.
- ✅ **GPUI parity FREE.** `build_cells_panes` is shared host code, and
  GPUI renders non-terminal panes through the generic `EditorElement`
  (no bespoke in-pane-help path — only the *floating* popup, PU.1b-3, is
  bespoke). So GPUI in-pane help reads the same matrix and flips for
  free — the parity rule satisfied via the shared content layer, no
  GPUI patch. (`feedback_tui_gpui_parity`.)
- ✅ Tests: `help_in_pane_seeds_link_extra_highlights` +
  `help_in_pane_swap_reseeds_links_and_syntax` (TUI).

### PU.1b-3 — floating popup (both states) through compose ✅ (2026-06-28)

Fork 1 solved: the floating help popup now renders its CONTENT through
the shared `compose_pane_lines` reading a real cells-worker
`DisplayMatrix`, in BOTH State A (popup shown, doc focused) and State B
(focus moved into the popup). Only the box (border + title + terminal
cursor) stays popup-specific chrome. Green: lattice-host 592 (+1),
lattice-ui-tui 1503 (net: −1 deleted `render_help_line` test, +2 new),
lattice-help 39, GPUI `--features window` clean.

- ✅ **Synthetic-pane coverage (host).** `PaneId::POPUP` (reserved
  `u32::MAX` sentinel, `lattice-core`) keys a synthetic
  `PaneCellsInputs` entry that `build_cells_panes` appends when a
  floating popup is open, NOT shown in-pane, and the renderer has fed
  back geometry (`popup_viewport_width > 0`). The per-leaf input
  builder was extracted into `Editor::build_one_pane_cells_input` so the
  real-pane and popup paths share ONE builder (no drift — heuristic #1).
  State B sources snapshot/folds/scroll from the live `self.*`
  (popup is the active buffer); State A from the registry +
  `popup_scroll`. `wrap = true` (help-mode's declared `Wrap`; matches
  the retired unconditional wrap).
- ✅ **Geometry hand-off (renderer → host).** New Editor fields
  `popup_viewport_{height,width}` + `App::set_popup_viewport`, fed each
  frame from the runtime loop via `render::popup_feedback_inner_dims`
  (diff-then-send, mirror of `set_pane_viewport`). The renderer is the
  sizing authority (`popup_outer_size` over the same buffer area
  `draw_help_overlay` paints into), so the matrix width and the painted
  box agree. One un-sized frame on open → plain-text fallback (correct
  wrapped text; colour eventual — UX-acceptable eventual consistency).
- ✅ **`draw_help_overlay` interior flipped** to `compose_pane_lines`
  with a `PaneId::POPUP` ctx + `FrameView::for_buffer(popup_id)` (so
  help-mode's `nonu`/`signcolumn=no`/`wrap` drive layout regardless of
  which buffer is active under the popup). Terminal cursor placement
  (State B) now uses the compose-aware `cursor_screen_position_at`.
- ✅ **Deleted:** `manually_wrap_lines`, `render_help_line` (+ its
  test), `wrap_aware_cursor_offset`, `display_rows_for_len`,
  `clamp_to_char_boundary`, `help_render_data`, `style_to_tui`, the
  `HELP_WRAP_MARKER`/`_WIDTH` consts, and the unused `lattice_syntax::Style`
  import. (`render_help_line` was listed under PU.1b-4 but became dead
  the moment the interior flipped, so it landed here.)
- **Accepted visual changes** (consistent with PU.1b-2b's in-pane flip):
  the floating popup gains compose's 2-cell `nonumber` left margin and
  `~` empty-line markers below short content — a help popup is now
  pixel-equivalent to a `:set nonu signcolumn=no wrap` document in a box.
- **GPUI parity:** the synthetic-pane infra is shared host code; the
  GPUI *floating-popup* paint flip stays PU.2 (the bespoke
  `window.rs` overlay still ignores the synthetic matrix). No GPUI code
  touched; parity rule satisfied via the shared content layer (same
  shape as PU.1b-2b).
- Tests: `floating_popup_gets_synthetic_cells_pane_when_geometry_fed`
  (host — synthetic pane keyed to popup buffer, gated on geometry,
  always-wrap) + `popup_feedback_inner_dims_only_for_open_floating_popup`
  (TUI — feedback helper Some only for an open floating popup, inner
  width = outer − 2).

### PU.1b-4 — delete bespoke precompute (2A cutover complete) 🗒

Nothing reads the precompute path once both renderers flip. **Delete**
`with_markdown_syntax`, `popup_help_highlights`, `popup_help` (the view
reconstructor — note draw_help_overlay still uses it for chrome
title/line-count/State-A scroll, so its retirement is the work here),
and the `HelpHighlights` buffer-local seeding. (`render_help_line` +
`help_render_data` already deleted in PU.1b-3.)

- **Acceptance (whole PU.1b):** `:set wrap`/`nowrap` changes the help
  popup; folds work inside help; horizontal scroll works inside help
  (proves the HS dependency); the popup's visible content equals
  `compose_pane_lines` for the same buffer + inner rect.
- Tests: a "help content == compose_pane_lines" equivalence test; the
  wrap-toggle + fold + h-scroll behaviours inside the popup.

## PU.2 — GPUI help parity 🗒

Same seam in the GPUI peer (parity rule — same patch class as PU.1).

- Route the GPUI help popup interior through `EditorElement` with the
  popup's inner rect + scroll/leftcol/cursor.
- **Delete** the ~270-line manual cell/row + chunk-wrap loop in
  `window.rs` (~3405–3670). Keep the box chrome.
- **Acceptance:** GPUI help renders byte-equivalent content to a
  regular pane; visual pass on wrap/fold/h-scroll inside the popup
  (`cargo run --features gui -- --gui`).

## PU.3 — Ephemeral-buffer class 🗒

The mechanism transient popups need before they can join the registry.

- `BufferFlags { listed: false, hidden: true }` + an **ephemeral**
  marker; create on popup-open, garbage-collect on dismiss. Never
  appears in `:ls`, never churns the listed set.
- Lifecycle hooks: the owning mode's `on_activate` creates it, dismiss
  drops it (mirrors how transient state is owned today).
- Tests: an ephemeral buffer is invisible to `:ls` / `:bn` / `:bp` and
  is removed from the registry on dismiss.

## PU.4 — LSP hover through compose (TUI + GPUI) 🗒

- Back the hover popup with an ephemeral buffer (PU.3) carrying the
  hover markdown; route its content through the seam.
- Delete the hover-specific line builder (both renderers).
- **Acceptance:** hover content gets syntax/markdown rendering,
  wrap-toggle, and h-scroll; auto-dismiss + cursor-motion behaviour
  unchanged.

## PU.5 — Signature help + completion docs through compose 🗒

- Same ephemeral-buffer + seam treatment for signature help and the
  completion documentation popup (both renderers).
- Delete their bespoke content paths (the completion-docs plain
  `Paragraph`, the signature line builder).
- Note: the completion **candidate list** and pickers are list/
  selection widgets, not document content — out of scope for this
  initiative (their unification, if any, is a separate "list buffer"
  question). This initiative is about *content* popups.

## PU.6 — Cleanup + regression guard 🗒

- Grep-gate (CI) asserting no popup path calls a bespoke content
  renderer — mirrors the `Effect::*` / `DiffSignKind::*` GPUI-parity
  grep in the TUI/GPUI-parity rule.
- The verbatim "a popup is a regular buffer in a box" test across all
  popup kinds (K.4-style), analogous to
  `crates/lattice-host/tests/multibuffer_is_a_regular_buffer.rs`.
- Confirm the four-artefacts set landed per slice (design fragment
  here, tests per slice, perf covered by the existing compose benches
  — popups compose only their inner rect, no new hot-path cost).

## Dependencies & cross-references

- **Horizontal scroll** (`docs/dev/architecture/horizontal-scroll.md`
  §5) is the forcing function: PU.1 is where h-scroll first reaches a
  popup. Done (HS.1–HS.3) and merged on the `horizontal-scroll` branch.
- **Synthetic buffers / Group-1 set** — `feedback_synthetic_buffers`;
  help stays `HelpBuffer`-flavoured.
- **K.4 / no kind-specific rendering** — `feedback_buffers_no_special_case`;
  the rule this initiative satisfies for popups.
- **TUI/GPUI parity** — `feedback_tui_gpui_parity`; each renderer slice
  lands in the same patch class.
