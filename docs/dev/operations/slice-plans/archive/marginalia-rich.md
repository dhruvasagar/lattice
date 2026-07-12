# Slice plan: MR — rich per-segment marginalia (file metadata)

**Design:** [marginalia.md §8](../../../architecture/marginalia.md).

**Status:** ✅ complete (2026-06-30) — MR.1–MR.4 landed. The file/dir picker renders eza-style per-bit permission colors, gold size, and green mtime on both peers, theme-driven (recolors live on `:colorscheme`). Four sub-slices. Implements the §8 extension: `Annotation::Styled` (per-segment-colored column cell), eza-style permission/size/mtime theme slots, the file/dir picker as first producer, and closure of the TUI annotation-theme gap left open by MARG.1. Design fragment + bench + per-renderer tests + per-renderer palette ship together per CLAUDE.md heuristic #5.

**Why:** The file/dir picker bakes `perms/size/mtime` into one flat `RawCandidate.display` string painted in a single uncolored run. The user wants the eza/`ls --color` treatment from the reference screenshot — per-bit permission colors, gold size, green mtime — integrated with the theme system, on both renderers. The MARG substrate already carries typed, column-aligned annotations (MARG.1–5) and GPUI already resolves their colors from the theme (T.6); the one missing primitive is coloring *within* a single column cell.

**Critical-path coupling:** MR.1 (TUI theme-wiring) is a prerequisite — without it the new slots have no effect on TUI. MR.2 (the `Styled` variant + slots) is the substrate that unblocks the producer; it lands behind MR.1 so the new variant slots into an already-theme-wired render path on both peers. MR.3 (producer) is the first user-visible slice. MR.4 closes with bench + the remaining cross-peer/colorscheme tests. Strict order: MR.1 → MR.2 → MR.3 → MR.4.

**Cross-renderer discipline:** every slice that touches `lattice-ui-tui` annotation rendering updates `lattice-ui-gpui` in the same patch (CLAUDE.md TUI/GPUI lockstep). End-of-slice audit: `grep -rn "Annotation::Styled\|annotation.*perm" crates/lattice-ui-gpui/ --include="*.rs"` non-empty once MR.2 lands.

## Sequencing

### MR.1 — Close the TUI annotation theme gap ✅

Prerequisite slice. No new feature; existing annotations become theme-driven on TUI (they already are on GPUI via T.6). Touches the TUI candidate render path only.

Changes:

1. `crates/lattice-ui-tui/src/render.rs` (`candidate_to_line`, ~1128) — accept the resolved theme table + `BuiltinElementIds` (threaded from the same `RenderState` the picker draw already loads). Replace `annotation_color(ann, selected)` (hardcoded `Color::Cyan` / `DarkGray` / …) with per-category theme-slot reads against `completion.annotation.{kind,doc,keybinding,source,custom}` (the slots MARG.1 registered), honoring `fg` / `fg_selected`.
2. Delete `annotation_color()` once its last caller is gone (or reduce it to a pure slot-id mapping shared with the resolution helper).
3. Tests: TUI annotation fg for each existing category equals the resolved slot fg (mirror GPUI's `annotation_color_rgb` test shape); a `:colorscheme` swap changes the rendered annotation fg.

Acceptance: workspace check + tests green; existing completion/picker annotations recolor with the active theme on TUI; GPUI unchanged. Bisect-friendly: one commit.

Risk: TUI tests that assert exact hardcoded `Color::*` for annotations must move to resolved-slot assertions. Audit `grep -rn "annotation_color\|Color::Cyan" crates/lattice-ui-tui/`.

### MR.2 — `Annotation::Styled` variant + per-segment theme slots + both-peer rendering ✅

Substrate slice. No producer yet → no user-visible change. Adds the variant (forcing exhaustive handling in both peers) and the new slots.

Changes:

1. `crates/lattice-completion/src/candidate.rs` — add `pub struct AnnotationSegment { pub text: Arc<str>, pub slot: Arc<str> }` and the `Annotation::Styled { category: Arc<str>, segments: Vec<AnnotationSegment> }` variant. `display_text()` returns the concatenated segment text; `category()` returns the `category` field. (§8.2)
2. `crates/lattice-theme/src/registry.rs` — register `completion.annotation.perm.{type,read,write,exec,special,none}`, `completion.annotation.size`, `completion.annotation.mtime` in `register_builtins` with the §8.5 eza-convention defaults; add the matching `ElementId` fields to `BuiltinElementIds` + resolve them in the id-binding pass.
3. `crates/lattice-ui-tui/src/render.rs` (`candidate_to_line`) — on `Styled`, walk `segments` and push one styled span per segment, each resolved against its `slot` (unknown slot → custom/plugin fallback). Uses the MR.1 theme-read path.
4. `crates/lattice-ui-gpui/src/window.rs` (`paint_candidate_row` / `annotation_color_rgb`) — same: one child div per segment, per-segment slot resolution.
5. Tests: `display_text()`/`category()` for `Styled`; `AnnotationColumns::from_visible` width with a `Styled` cell equals its concatenated width and aligns alongside single-variant cells; both peers render a synthetic 3-segment `Styled` annotation with three distinct resolved fgs; unknown slot falls back without panic.

Acceptance: check + tests green on both peers; a synthetic `Styled` annotation renders multi-colored in TUI and GPUI. Bisect-friendly: one commit (variant + slots + both renderers land together — the exhaustive match requires it).

Risk: exhaustive-match breaks anywhere that matches `Annotation` — sweep `grep -rn "match .*Annotation\|Annotation::" crates/ --include="*.rs"` and add the `Styled` arm.

### MR.3 — File/dir picker emits structured perm/size/mtime annotations ✅

Producer slice. The user-visible payoff. Depends on MR.2.

Changes:

1. `crates/lattice-picker/src/picker_sources.rs` — refactor `format_perms(&Metadata) -> String` into a segment builder `perm_segments(&Metadata) -> Vec<AnnotationSegment>` mapping each bit class to its slot per §8.4; keep a thin `format_perms` wrapper only if other callers need the flat form.
2. Same file (dir/file source `init`) — stop composing the flat `"{path} {perms} {size} {mtime}"` `display` string + manual column padding. Set `RawCandidate.display` to the path (matchable text) and push annotations: `Styled{category:"perm", …}`, `Styled{category:"size", [one]}` (slot `…size`), `Styled{category:"mtime", [one]}` (slot `…mtime`). Drop the now-dead `path_width`/`perms_width`/`size_width` manual layout (column geometry comes from `AnnotationColumns`).
3. Files that fail to `stat` push no metadata annotations → blank cells (existing behavior preserved).
4. Tests: `perm_segments` bit→slot mapping incl. symlink / setuid / setgid / sticky / block / char / fifo / socket; the dir/file source produces exactly the three metadata annotations per stattable row and none for an unstattable row; the path remains the candidate `display` (match still works).

Acceptance: opening the file/dir picker shows per-bit-colored perms, gold size, green mtime on both peers, theme-driven (the reference screenshot). Bisect-friendly: one commit.

Risk: any test asserting the old flat `display` string for the file/dir picker must move to annotation assertions. Audit `grep -rn "format_perms\|format_size\|format_mtime" crates/lattice-picker/`.

### MR.4 — Four-artefact close: bench + colorscheme/cross-peer tests ✅

Close-out slice. Depends on MR.3.

Changes:

1. Bench: extend the MARG annotation-render bench (or add `marginalia_styled` if none exists) with a row set carrying `Styled` perm cells; record per-frame resolution cost in `BENCHMARKS.md` so the §8.7 O(visible×segments) claim is examinable in CI.
2. Tests: `:colorscheme` swap recolors file-picker perm/size/mtime marginalia live on both peers; an end-to-end picker-open assertion that a known fixture dir renders the expected per-segment slots.
3. Doc: refresh test/commit counts in this plan and the `marginalia.md` status line; confirm the design fragment matches what shipped (update §8 if anything diverged during build).

Acceptance: bench recorded; colorscheme + e2e tests green; design ↔ code reconciled. Bisect-friendly: one commit.

## Out of scope (deferred)

- Migrating other marginalia producers (buffer list, LSP locations, command palette) to `Styled` — they keep their current single-color annotations; `Styled` is available to them when a multi-colored need arises (§8.2 generalizes `Custom`).
- Per-segment *interaction* (click-to-edit, hover) — `Styled` is paint-only for v1.
- Unicode-width handling for column math — inherited from the existing `chars().count()` limitation (MARG.5); a future pass lands in both sites at once.
