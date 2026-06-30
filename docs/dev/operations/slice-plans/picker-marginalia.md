# Slice plan: MP + PH — picker marginalia rollout & preview syntax highlighting

**Designs:** [marginalia.md §9](../../architecture/marginalia.md) (MP — annotation columns across all pickers) and [picker-preview-highlight.md](../../architecture/picker-preview-highlight.md) (PH — syntax-highlighted preview text).

**Status:** 📝 planned (2026-06-30). Two independent series. **MP** extends the §8 `Styled` mechanism to every remaining picker source (no candidate-model change). **PH** adds a new `display_spans` field carrying tree-sitter highlight spans for code previews. They touch the same producers but different candidate fields, so they sequence independently; land **MP first** (lower risk — no model change), then **PH**.

**Why:** Only `FilesSource` emits annotations today (MR). Every other picker bakes metadata into flat, hand-width-padded `display` strings (MP fixes this). And code shown in pickers (Grep/Lines/Outline previews) renders in one flat color — PH gives it the buffer's syntax colors. Both ride machinery that already exists: MP reuses `Annotation::Styled` + the theme-slot seam (MR §8); PH reuses `SyntaxSnapshot::highlight_lines` + `resolve_syntax_style` (the main-editor highlight path).

**Cross-renderer discipline:** every slice touching `lattice-ui-tui` candidate rendering updates `lattice-ui-gpui` in the same patch (CLAUDE.md TUI/GPUI lockstep). MP renderer churn is *slot registration only* (the generic `Styled` paint path is unchanged from MR §8.3); PH.1 is the one slice that changes both paint paths (the new `display_spans` overlay + match-highlight composition).

---

## MP — picker marginalia rollout

### MP.1 — Substrate: shared segment builders + new theme slots 📝

No producer change → no user-visible change. Establishes the shared helpers and slot families everything else consumes.

Changes:

1. `crates/lattice-picker/src/picker_sources.rs` (or a new `segments.rs` module) — add free fns `location_segments(path, line, col)`, `status_segments(dirty, active)`, `latency_segment(class)` per [§9.2]. Substrate helpers, not Document trait methods (CLAUDE.md substrate-vs-helper rule — only specific sources consume them).
2. `crates/lattice-theme/src/registry.rs` — register the §9.4 slots (`location.{path,line,col}`, `status.{dirty,active}`, `latency.{reflex,display,background}`, `args`, `buffer-id`, `register`) in `register_builtins` with the dark defaults; add matching `BuiltinElementIds` fields + resolve them in `annotation_slot()` (unknown slot still → `…custom`).
3. `crates/lattice-completion/src/candidate.rs` — add the §9.4 `category_order` entries (`location=8 … register=13`) so mixed column sets align.
4. Tests: each helper maps its input to the right slot (incl. `None` path / zero col); each new slot resolves to a distinct fg on both peers via a synthetic `Styled` candidate; unknown slot → custom fallback (no panic).

Acceptance: workspace check + tests green on both peers; no visible picker change yet. Bisect-friendly: one commit.

Risk: exhaustive `category_order` / `annotation_slot` matches — sweep `grep -rn "category_order\|annotation_slot" crates/`.

### MP.2 — Commands + Snippets pickers ✅

The "doc + extra columns" shape. Depends on MP.1.

Changes (landed):

1. `crates/lattice-picker/src/picker_sources.rs` `CommandsSource` — dropped the 4-column hand-padded `display` (and the now-dead `clip_to`). `display`=command name; emits `Styled{args}`, `DocSnippet`, `Styled{latency}`. Also folded the MP.1 duplicate `LatencyClass` into the canonical `lattice_grammar::command::LatencyClass` (heuristic #1, no duplicate enum) and added `SLOT_ARGS`.
2. `crates/lattice-snippet/src/picker_sources.rs` `SnippetsSource` — `display`=**prefix** (the matchable trigger), name→`Kind`, description→`DocSnippet`. (Design §9.3 said `display`=name; corrected to prefix because the matcher scores `candidate.text` and `match_ranges` index it, so the shown run must stay aligned with the trigger — reconcile §9.3 wording at MP.5.)
3. Tests: `commands_source_emits_marginalia_annotations` asserts the typed annotation set (args=`[<path>]`, latency=`[display]`, doc contains `Write`) and `display`=`write`; MP.1 helper tests cover `latency_segment`.

**Carved out → MP.2b:** the `+ Keybinding` enrichment. `PickerContext` exposes no keybinding reverse-cache and `CommandsSource` holds only the `CommandRegistry`; wiring it needs the reverse-cache threaded through `PickerContext` + `build_picker_context` (shared host plumbing) — a deliberate slice, not a freebie. Deferred to keep MP.2 focused.

Acceptance: `:picker commands` shows colored args/doc/latency; `:picker snippets` shows name/description marginalia, both theme-driven. No GPUI change (no new variant/slot). Green (one pre-existing parallel-test flake in `render.rs`, passes in isolation).

### MP.2b — Commands picker keybinding column 📝

Thread a command→chords reverse-cache through `PickerContext` (host-side, from the active keymap at `build_picker_context`) so `CommandsSource` can emit `Annotation::Keybinding` (slot already exists). Show first-bound chord per marginalia §10. Depends on MP.2.

### MP.3 — Buffers + RecentFiles pickers 📝

Depends on MP.1.

Changes:

1. `BuffersSource` — `display`=path; `Styled{buffer-id}`, `Styled{status}` (dirty/active via `status_segments`), `Kind` (existing slot). Drop the inline `[+]` / `(current)` markers from `display`.
2. `RecentFilesSource` — reuse §8 `metadata_annotations` verbatim (it already has the paths to stat) for perm/size/mtime.
3. Tests: buffers source emits status/kind/id per row, path matchable; recent-files source emits the same three metadata cells as `FilesSource` for a stattable path, none for an unstattable one.

Acceptance: `:picker buffers` shows colored status + kind; `:picker recent` gets the eza-style perm/size/mtime treatment. Bisect-friendly: one commit.

### MP.4 — Location family (Grep, Jumps, Outline, Lines, Marks) 📝

The coordinate pickers, via the MP.1 `location_segments` helper. Depends on MP.1.

Changes:

1. `GrepSource` — `display`=preview (matchable); `Styled{location}` for path:line:col.
2. `JumpsSource` — `display`=buffer label; `Styled{location}`; source-tag (`auto`/`mark`/`plugin`/`<char>`) → categorical slot.
3. `OutlineSource` / `LinesSource` — `display`=symbol name / line text; `Styled{location}` (line only). (PH.2 will add `display_spans` to the same two sources' preview text — keep the line text in `display` for that.)
4. `MarksSource` — `display`=mark name; `Styled{location}`.
5. Tests: each source emits the location cell with the right slots; the matchable text moves to `display` (fuzzy match still works).

Acceptance: all five pickers show a colored location cell; Jumps shows a colored source-tag. Bisect-friendly: one commit.

Risk: tests asserting `"path:line:col preview"` flat strings move to annotation assertions. Audit `grep -rn "hits_to_pairs\|source_tag" crates/`.

### MP.5 — Registers + four-artefact close 📝

Close-out. Depends on MP.2–MP.4.

Changes:

1. `RegistersSource` — `display`=preview; `Styled{register}` for the register name.
2. Bench: extend the §8 styled-cell bench with the location/status/latency families; record in `BENCHMARKS.md` (the §9.5 O(visible×segments) claim, examinable in CI).
3. Tests: `:colorscheme` swap recolors a representative picker's marginalia on both peers (cross-peer + colorscheme).
4. Doc: refresh status icons in this plan + the `marginalia.md` §9 status line; reconcile design ↔ code.

Acceptance: bench recorded; colorscheme/cross-peer tests green; design ↔ code reconciled. Bisect-friendly: one commit.

---

## PH — picker preview syntax highlighting

### PH.1 — Substrate: `display_spans` field + both-peer paint + match composition 📝

No producer → no visible change. The one slice that changes both paint paths. Independent of MP; can run anytime after MP.1 or in parallel.

Changes:

1. `crates/lattice-completion/src/candidate.rs` — add `display_spans: Vec<DisplaySpan>` (`DisplaySpan { range, style: Style }`) to the rendered candidate, default empty. **Verify** `lattice-completion → lattice-cells` (for `Style`) introduces no dependency cycle; if it does, fall back to a slot-key string + syntax-slot resolver (picker-preview-highlight.md §4).
2. `crates/lattice-ui-tui/src/render.rs` `candidate_to_line` — when `display_spans` is non-empty, subdivide the display run by per-byte style lookup (`resolve_syntax_style`), composing with `match_ranges` so **match style wins on overlap** (picker-preview-highlight.md §5); uncovered runs stay row-fg.
3. `crates/lattice-ui-gpui/src/window.rs` `paint_candidate_row` — same composition, per-cell.
4. Tests: a span per `Style` resolves to its `syntax.*` fg on both peers; `:colorscheme` recolors; a char in both a span and a match-range paints the match style; uncovered runs are row-fg; empty `display_spans` = today's plain preview.

Acceptance: synthetic candidate with `display_spans` renders multi-colored, match-highlight overriding, on both peers. Bisect-friendly: one commit.

Risk: the candidate-model field touches `RenderedCandidate`/`from_scored` constructors — sweep `grep -rn "RenderedCandidate" crates/`.

### PH.2 — Lines + Outline producers (cheap path) 📝

The user-visible payoff. Depends on PH.1 (and MP.4, which already moves these sources' line text into `display`).

Changes:

1. `crates/lattice-picker/src/context.rs` — add per-line highlight spans to `ActiveBufferSnapshot` (mirror the `syntax_symbols` precedent).
2. `crates/lattice-host/src/...` `build_picker_context` — pre-collect spans via `snapshot.highlight_lines(lo, hi)` (read-only, no reparse) and populate the new field.
3. `LinesSource` / `OutlineSource` — map the spans for each candidate's line into `display_spans` offset to `display`; no spans → empty (plain preview).
4. Tests: parsed fixture buffer → `LinesSource`/`OutlineSource` emit `display_spans` aligned to `display`; unparsed / no-grammar buffer → none; assert no `highlight_lines` call is reachable from `Render::render` (off-thread proof).

Acceptance: `:picker lines` / `:picker outline` render code previews with the buffer's syntax colors on both peers, recoloring on `:colorscheme`. Bisect-friendly: one commit.

### PH.3 — Grep arbitrary-file highlighting 📝 **post-v1 (design only)**

Deferred per picker-preview-highlight.md §7. Not scheduled. When taken up: a single-line `highlight_line_with_grammar(lang, &str)` helper on the LSP-runtime task, grammar selected by extension, capped to the visible window, plain fallback when no grammar matches. Carrier + render composition (PH.1) unchanged — only the producer differs.

---

## Out of scope (deferred)

- `ThemePickerSource` color swatch — needs an explicit-color segment primitive (marginalia §9.6); deferred until a second consumer needs it.
- Grep preview highlighting (PH.3) — per-hit parsing + grammar cache; post-v1.
- `KeybindingAnnotator` for non-command picker candidates (file open-in-split binds, etc.) — marginalia §10 open question; defer until a use case lands.
- Unicode-width column math — inherited `chars().count()` limitation (MARG.5); a future pass lands in both sites at once.
