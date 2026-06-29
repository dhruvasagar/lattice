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

Status icons: ✅ done · 🚧 in progress · 🗒 planned.

**Status (2026-06-29):** PU.1 / PU.1b (all sub-slices) / PU.2 / PU.3 /
PU.4 / PU.5 (5a–5d) ✅ complete. Only **PU.6** (cleanup + regression
guard) remains 🗒. PU.3 was delivered as part of PU.5 (ephemeral class
built with its first consumer, completion docs); PU.4 was pre-satisfied by
the PU.1b-3/PU.2 popup unification (hover/signature already ride the
floating-popup compose seam).

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

## PU.1b — Compose seam + markdown handle + delete bespoke ✅ (2026-06-29)

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

#### PU.1b-3 follow-up fix (2026-06-29, `1037976c`) — wrong snapshot source

Dhruva reported help **link/markdown styling vanished** from the floating
popup after PU.1b-3. Root cause: the synthetic popup pane passed
`is_active_buffer = popup_is_focused` to `build_one_pane_cells_input`. But a
help popup is a registry Document that is NEVER `activate_document`'d as
`self.document` (PU.1a) — even focused (State B, `active_buffer == Help`),
`self.document` still points at the buffer UNDERNEATH the popup. So
`is_active_buffer = true` built the popup matrix from the WRONG document's
(empty/stale) snapshot; compose's `display_stale` guard then fell back to
unstyled plain text. Fix: the popup ALWAYS sources snapshot+folds from the
registry handle + `buffer_locals[popup_id]` (`is_active_buffer = false`) —
only the scroll anchor differs by focus. This matches how the main loop
already treats in-pane Help (excluded from `active_doc_active`), which is why
in-pane help (PU.1b-2b) was never affected. Guard:
`floating_popup_composes_link_styling_end_to_end` (TUI — drives the cells
worker for `PaneId::POPUP`, composes like `draw_help_overlay`, asserts a
resolved `Style::Link` span).

### PU.1b-4 — delete bespoke precompute (2A cutover) — sub-carved 4a/4b (2026-06-29)

**Dependency correction (2026-06-29):** the original "once *both* renderers
flip" framing put PU.1b-4 before PU.2, which is self-contradictory — GPUI
flips in PU.2. Both renderers are now flipped (TUI: PU.1b-2b + PU.1b-3;
GPUI: PU.2), so the cells-worker `DisplayMatrix` is the single styling
source for help in both peers.

**Sub-carve (2026-06-29):** the PU.1b-4 scope surfaced two unrelated
deletions of different risk, so it splits:
- **4a** — the dead markdown-highlight precompute (pure deletion of
  now-unread code; no behaviour change).
- **4b** — retiring `popup_help()` / `PopupRenderState.help` (a
  *behaviour-sourcing* rework: `popup_help()` feeds NOT JUST chrome but
  `active_text` / `active_cursor` for a focused State-B popup, so its
  retirement reworks motion/text sourcing AND both renderers' chrome —
  the original text glossed the `active_text` coupling).

#### PU.1b-4a — delete the markdown-highlight precompute ✅ (2026-06-29)

Pure deletion; both renderers already style help from the live
`DisplayMatrix` (markdown via `install_inmemory_syntax`, links via
`ExtraHighlights(link_highlights(&links))`), so the precompute was read by
nobody for rendering. No behaviour change. Green: `lattice-help` 34,
`lattice-host` 643, `lattice-ui-tui` 1504; `cargo build` clean for
`lattice-host`, `lattice-cli` (TUI), `lattice-ui-gpui --features window`,
and `--features gui`. Final grep of all five symbols across `crates/` = 0.

- ✅ **`lattice-help`:** deleted `HelpContent::with_markdown_syntax`, the
  free fn `compute_markdown_highlights`, the `HelpMetadata.highlights`
  field (+ its literal init), and the 5 markdown-highlight tests. Kept
  `overlay_link_styles` + `link_highlights` (the live `ExtraHighlights`
  feed). Pruned now-unused imports.
- ✅ **`lattice-host`:** removed all 24 `.with_markdown_syntax(...)` chain
  calls; dropped `HelpHighlights` from `seed_help_metadata_locals` +
  `snapshot_current_popup`; deleted `Editor::popup_help_highlights`,
  `modes::HelpHighlights`, `PopupRenderState.help_highlights` (field +
  publish + Default); removed the dead `ShowDiagnosticsPopup` severity-span
  precompute (fed only `metadata.highlights`).
- ✅ **`lattice-ui-tui`:** deleted `App::popup_help_highlights` + the
  `HelpHighlights` seeding assert in the locals test.
- ✅ **`lattice-ui-gpui`:** trimmed the stale `help_highlights` comment
  (the read was already gone in PU.2).

#### PU.1b-4b — remove the published `PopupRenderState.help` drift seam ✅ (2026-06-29)

**Re-scoped on discovery (merit-based, heuristic #1).** The original
"delete `popup_help()`" framing proved wrong on contact: `popup_help()` is
a host READ ACCESSOR that reconstructs a transient view FROM the registry
Document on demand — it is NOT a parallel store, so it is not an
everything-is-a-buffer violation. It is woven into ~70 TUI tests (as a
content/title read accessor) PLUS production motion (`motions.rs`), focus
(`focus_help_popup`), and in-pane re-seed paths. Deleting the *function*
would be a ~70-test rewrite that serves NO paramount goal (heuristic #1
forbids rewriting for its own sake).

The actual everything-is-a-buffer seam is the **published
`PopupRenderState.help`** — the parallel `Arc<HelpBuffer>` snapshot a
renderer paints chrome from instead of the registry Document. That is what
this slice removes. Green: `lattice-host` + `lattice-cli` (TUI) +
`lattice-ui-gpui --features window` + `--features gui` all build clean;
suites green (host / ui-tui / ui-gpui).

- ✅ **`PopupRenderState`:** deleted the `help: Option<Arc<HelpBuffer>>`
  field; added `scroll: u32` (the popup's State-A view scroll = genuine
  popup-only view state, NOT in the registry Document, so it IS published).
  `build_render_state` publishes `scroll: self.popup_scroll` instead of
  `help: self.popup_help()...`.
- ✅ **GPUI** (the only consumer of the published `help` field): the popup
  closure now gates on `popup_substate.buffer_id`, sources the title from
  `rs.buffers.registry.name_of(popup_id)`, content/line-count from the
  registry snapshot (already so for content since PU.2), and State-A scroll
  from `popup_substate.scroll`.
- ✅ **TUI:** NO change — the TUI chrome reads the `popup_help()` accessor
  (kept), never the published `help` field. So removing the field doesn't
  touch the TUI renderer.
- ✅ **Kept:** `popup_help()` (host + TUI accessor), `help_content_view`,
  and the `active_text` / `active_cursor` reads through them — all read the
  canonical registry, no drift. A standalone `popup_help()` deletion (with
  the ~70-test migration) is available as a separate cleanup if ever
  wanted, but it carries no paramount-goal payoff.

- **Acceptance (whole PU.1b):** `:set wrap`/`nowrap` changes the help
  popup; folds work inside help; horizontal scroll works inside help
  (proves the HS dependency); the popup's visible content equals
  `compose_pane_lines` for the same buffer + inner rect. (Behaviourally
  satisfied: both renderers paint help through the shared matrix /
  `compose_pane_lines` path.)

## PU.2 — GPUI help parity ✅ (2026-06-29)

The GPUI floating help popup now renders its CONTENT through the shared
`EditorElement` reading the synthetic `PaneId::POPUP` `DisplayMatrix` —
the GPUI peer of PU.1b-3's TUI flip. Only the box (border + title +
separator) stays popup-specific chrome. Green: `cargo build -p
lattice-ui-gpui` (default) + `--features window` both clean; GPUI suite
112 passed (+1). Visual pass on wrap/fold/h-scroll inside the popup left
to a `cargo run --features gui -- --gui` check (HS.1b / PU.1b-1a
precedent — the content now flows through the same matrix the TUI peer
renders, which is already covered).

- ✅ **Geometry hand-off (GPUI runtime → host).** `EditorView` gains a
  `last_popup_dims` field; each frame a floating popup is open AND help
  is not an in-pane leaf, `render` computes the popup's inner `(rows,
  cols)` (`popup_inner_rows` / `popup_inner_cols`, the SAME dims the
  chrome locks the body to) and pushes via the new
  `GpuiApp::set_popup_viewport` (→ `Editor::popup_viewport_{height,width}`
  via `mutate_editor`). Diff-then-send mirrors the pane-viewport loop +
  the TUI runtime's `popup_feedback_inner_dims`; steady-state = zero RPCs.
  This is what makes `build_cells_panes` build the `PaneId::POPUP` matrix
  on the GPUI side (it never fed the geometry before).
- ✅ **Interior through `EditorElement`.** `draw`'s popup closure builds
  one `EditorElement` (approach A — minimal inline construction; decoration
  fields empty/None because a help overlay genuinely has no
  selection/diff/doc-highlight/inlay/diagnostic). Snapshot + text version
  from the registry handle (`is_active_buffer=false` equivalent — the
  popup is never `activate_document`'d, PU.1a); matrices from
  `display_matrix_for_pane(POPUP)` / `matrix_for_pane(POPUP)` with the
  same `version.text` stale guard as every pane; State-B cursor / State-A
  none; help-mode `nonu` + `signcolumn=no` → empty gutter (the text-only
  walk) + `sign_column=false`; wrap from the matrix `wrap_width`.
  `pane_idx = usize::MAX` for a non-colliding `ElementId`.
- ✅ **Empty-gutter walk windowing fix** (`editor_element.rs`). The
  text-only fallback iterated `[scroll, scroll+vh).min(raw_lines.len())`,
  which clamped the END to the window LENGTH and dropped `scroll`-many
  rows once `scroll > 0` (only correct for `scroll == 0`). Rewrote it to
  iterate the window-relative offset `rel` directly (`line_idx = scroll +
  rel`), correct at any scroll. No production caller passed an empty
  gutter before this slice (paint_pane always builds one), so the fix is
  regression-free — the floating popup is its first user.
- ✅ **Deleted:** the ~190-line manual cell/row + chunk-wrap loop and its
  now-orphaned helpers `syntax_color`, `style_at`, `popup_wrap_enabled`
  (+ the unused `SyntaxStyle` import). Wrap is now matrix-driven (always
  on for help, matching the TUI peer), not the `popup.wrap` read.
- **Accepted visual changes** (consistent with PU.1b-2b / PU.1b-3): the
  popup gains compose's `nonumber` left margin behaviour and `~`
  empty-line markers — pixel-equivalent to a `:set nonu signcolumn=no
  wrap` document in a box.
- Test: `set_popup_viewport_writes_editor_geometry_fields` (GPUI — the
  geometry plumbing + zero→1 clamp; the synthetic-pane build is covered
  host-side by PU.1b-3's `floating_popup_gets_synthetic_cells_pane_when_geometry_fed`).

## PU.3 — Ephemeral-buffer class ✅ (2026-06-29, delivered via PU.5a + PU.5c)

The mechanism transient popups need before they can join the registry.

**Folded into PU.5 and built there (2026-06-29).** Investigation while
folding PU.3 into PU.4 (per "build the abstraction with its consumer")
found PU.4's premise was obsolete: hover + signature already live in the
registry as `HelpContent` Documents and GC on dismiss (see PU.4), so they
need NO ephemeral class. The *only* transient content popup still bespoke +
NOT in the registry was the **completion-docs side popup** — the genuine
first consumer — so the ephemeral class was built alongside it (no-
speculative-abstraction rule, heuristic #1).

Deliverables — all landed:
- ✅ **`BufferFlags.ephemeral` marker** + invisible-to-`:ls` /
  `:bn` / `:bp` (PU.5a, `e6177b31`). (`:bn`/`:bp` already skipped
  `listed:false`; the new bits are the marker + the full `:ls` exclusion.)
- ✅ **Create on popup-open, GC on dismiss** (PU.5c, `ef161b5f`):
  `reconcile_completion_docs_buffer` (single chokepoint in
  `run_tick_pending`) creates the ephemeral help-flavoured Document when the
  docs popup opens and `gc_ephemeral_buffer`s it when completion closes —
  the consumer (completion subsystem) owns the lifecycle. Implemented as a
  per-cycle reconcile rather than a literal `Mode::on_activate`/`on_deactivate`
  pair; the ownership + create/GC intent is identical and decouples from the
  ~10 scattered `insert_completion = None` teardown sites (more robust than
  hooking each).
- ✅ **Tests**: `ephemeral_buffers_excluded_from_listed_ids` (registry —
  invisible to `:bn`/`:bp`, still in `sorted_ids` until GC) +
  `completion_docs_reconcile_creates_ephemeral_buffer_and_gcs` (host —
  create / replace-in-place / GC-on-dismiss, ephemeral flag asserted).

## PU.4 — LSP hover through compose (TUI + GPUI) ✅ (2026-06-29, pre-satisfied)

**Already satisfied by the popup unification (PU.1b-3 + PU.2)** — no code
needed. Hover does NOT have a bespoke renderer: `drain_pending_hover`
builds `HelpContent::from_lines("hover", …)` and emits
`RendererSignal::DisplayBuffer(content, category: Hover)`, which
`editor.display_buffer` routes to a cursor-anchored `open_floating_popup`
— the SAME floating popup PU.1b-3 (TUI) and PU.2 (GPUI) flipped to the
compose/matrix seam. So hover content already gets markdown/link syntax
(live `DisplayMatrix`), soft-wrap, and h-scroll, is a registry Document,
and GC's on dismiss via `dismiss_stale_popup_registry`. The "hover-specific
line builder" the original slice text wanted to delete had already been
unified into the `DisplayBuffer`→popup mechanism before PU.1, so there is
nothing left to delete.

- **Acceptance (met):** hover content gets syntax/markdown rendering,
  wrap, and h-scroll (via the shared popup→compose seam); auto-dismiss +
  cursor-motion behaviour unchanged (untouched existing logic).

## PU.5 — Signature help + completion docs through compose ✅ (2026-06-29)

**Signature help: pre-satisfied** — `drain_pending_signature_help` emits the
SAME `DisplayBuffer(content, category: Hover)` path as hover (cursor-anchored
floating popup through the compose seam). No bespoke signature builder.

**Completion docs (the last bespoke content popup) + the ephemeral-buffer
class (PU.3 folded in):** sub-sliced 5a→5d, each landed green.

- ✅ **PU.5a** (`e6177b31`) — `BufferFlags.ephemeral` marker + `:ls`/`:bn`/
  `:bp` exclusion. (`:bn`/`:bp` already skipped `listed:false`; the new bit
  is the `:ls` exclusion + the marker. GC-on-dismiss already existed in
  `dismiss_stale_popup_registry`.)
- ✅ **PU.5b** (`3f678e07`) — `PaneId::COMPLETION_DOCS` sentinel +
  `synthetic_popup_panes()` spec list (the single `PaneId::POPUP` block
  generalized to N overlays; help popup behavior-identical).
- ✅ **PU.5c** (`ef161b5f`) — TUI completion docs through compose. The docs
  popup is backed by an **ephemeral, help-flavoured registry Document**
  (markdown syntax + link `ExtraHighlights` + `nonu`/`signcolumn=no`/`wrap`
  help-mode options for free), reconciled once per cycle from
  `run_tick_pending` (`reconcile_completion_docs_buffer` — single chokepoint:
  create / replace-in-place / GC, not the ~10 scattered teardown sites).
  `draw_insert_completion_docs_popup` flipped from `Paragraph` to
  `compose_pane_lines` reading the `COMPLETION_DOCS` matrix; `draw_frame`
  returns the docs inner dims (cursor-anchored ⇒ computed at the draw site)
  fed back via `set_completion_docs_viewport` (diff-then-send).
- ✅ **PU.5d** (this commit) — GPUI completion-docs surface (NEW — GPUI had
  none). The docs popup renders through the same `EditorElement` +
  `COMPLETION_DOCS` matrix as the floating popup (PU.2 pattern), a
  fixed-width box left of the top-right candidate popup;
  `GpuiApp::set_completion_docs_viewport` + `EditorView.last_completion_docs_dims`
  diff-then-send geometry. GPUI users gain completion docs at all (a real
  parity fill). Visual pass: `cargo run --features gui -- --gui`.
- Note: the completion **candidate list** and pickers are list/selection
  widgets, not document content — out of scope (a separate "list buffer"
  question). This initiative is about *content* popups.

Green: host 594, ui-tui 1504, ui-gpui `--features window` 112; builds clean
incl. `--features gui`. Tests: `ephemeral_buffers_excluded_from_listed_ids`,
`completion_docs_reconcile_creates_ephemeral_buffer_and_gcs`.

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
