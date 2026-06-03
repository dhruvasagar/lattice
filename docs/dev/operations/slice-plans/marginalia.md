# Slice plan: MARG — typed marginalia for completion candidates

**Design:** [marginalia.md](../../architecture/marginalia.md).

**Status:** 🗒 planned. No code yet. Recommended sequencing below.

**Why:** Today `RenderedCandidate.annotations: Vec<String>` is untyped; every annotation renders in one of two hardcoded colors (`Gray` if row selected, `DarkGray` otherwise) regardless of meaning. The user wants a keybinding column on `:` line command completion (vertico+marginalia style) AND color-coding by category. The MARG series lands the typed-annotation substrate first, then layers the keybinding annotator on top, then closes the GPUI render gap so peer renderers stay at parity.

Critical-path coupling: MARG.1 unblocks MARG.2 (keybinding annotator needs the `Annotation::Keybinding` variant). MARG.3 (GPUI parity) can run in parallel with MARG.2 once MARG.1 lands. MARG.4 (design fragment + bench + tests) closes the slice series with the four-artefact discipline.

## Sequencing

### MARG.1 — Typed `Annotation` enum + theme slots + migrate existing annotators 🗒

Substrate slice. No new feature visible to the user. Touches the completion-pipeline + theme + TUI renderer in lockstep.

Changes:

1. `crates/lattice-completion/src/candidate.rs` — introduce `enum Annotation` with variants `Kind(Arc<str>)`, `DocSnippet(Arc<str>)`, `Keybinding(SmallVec<[KeyChord; 2]>)`, `Source(Arc<str>)`, `Severity(lattice_protocol::Severity)`, `Custom { text: Arc<str>, slot: Arc<str> }`. Replace `pub annotations: Vec<String>` with `pub annotations: Vec<Annotation>` on `RenderedCandidate`.
2. `crates/lattice-completion/src/builtins/annotators.rs` — migrate `KindLabelAnnotator` to push `Annotation::Kind(...)`; migrate `DocSnippetAnnotator` to push `Annotation::DocSnippet(...)`. No behavior change.
3. `crates/lattice-theme/` (or wherever theme slots live) — add `theme.completion.annotation_*` slots per §5 of the design. Each slot carries `fg` + `fg_selected` (current renderer's two-color flip generalized).
4. `crates/lattice-ui-tui/src/render.rs:1056` (`candidate_to_line`) — replace the joined-string rendering with per-variant rendering: match the variant, look up the theme slot, format the payload, push the styled span. Joiner spaces inherit row style.
5. Tests: keep existing annotator unit tests; add coverage for each new variant's renderer output (smoke check that `Annotation::Kind("foo")` renders with the `annotation_kind` slot's fg, etc.).

Acceptance: workspace check + tests green. Visible diff: existing annotations now color-coded (kind = grey, doc = cyan dim) instead of both being the same grey. User-facing release note: "completion annotations are now color-coded by category."

Risk: tests that grep stdout for annotation text by exact string formatting may break if the variant's formatter differs (e.g., the joiner). Bisect-friendly: one commit.

### MARG.2 — `KeybindingAnnotator` + reverse keymap-cache 🗒

Feature slice. Depends on MARG.1.

Changes:

1. `crates/lattice-mode/` (or `lattice-host/keymap_trie.rs` — wherever the trie build lives) — add `KeymapReverseCache: HashMap<CommandId, SmallVec<[KeyChord; 2]>>`. Build during the trie-build pass; same invalidation surface as the trie itself. Storage strategy per §6 of the design.
2. `crates/lattice-completion/src/annotators.rs` — register `KeybindingAnnotator` for command-kind candidates. The annotator calls `ctx.keymap_reverse.lookup(cmd_id)` and pushes `Annotation::Keybinding(chords)` if non-empty.
3. `crates/lattice-completion/src/pipeline.rs` — extend `AnnotatorContext` with `keymap_reverse: &KeymapReverseCache`. Wire it through the pipeline's `run()` call sites.
4. Tests: integration test that `:dele` completion now shows the `<leader>d` keybinding (or whatever the user-config has bound to delete) in the annotation column with `theme.completion.annotation_keybinding` style.

Acceptance: keybinding visible in `:` line popup for bound commands. Bisect-friendly: one commit.

Risk: the reverse-cache build cost on trie rebuild needs a smoke benchmark — sub-ms for typical keymaps per §6 design, but worth verifying once. Land the bench in MARG.4 or here, mark with `#[cfg(bench)]`.

### MARG.3 — GPUI parity for annotation column 🗒

Closes the GPUI render gap noted in `completion-pipeline-unification.md` ("Annotations: only in cmdline; GPUI render gap"). Required by `feedback_tui_gpui_parity` — peer renderers track in lockstep with the TUI; this slice was on the deferred list and MARG triggers landing it.

Changes:

1. `crates/lattice-ui-gpui/src/lib.rs` (or wherever the cmdline-completion popup paints) — add annotation column rendering. Same per-variant match + theme-slot lookup + styled-text emit as the TUI peer's `candidate_to_line`.
2. Theme propagation: `GpuiApp.host_theme` already carries the theme — extend the conversion to surface the new `annotation_*` slots.
3. Tests: GPUI-side render test (or bench-internals smoke) that the annotation column paints with the right styled-text run.

Acceptance: GPUI cmdline-completion popup shows the same color-coded annotations as the TUI. Both renderers paint the keybinding column. Bisect-friendly: one commit.

### MARG.4 — Bench + plugin-extensibility readiness 🗒

Closes the slice series. Discipline per CLAUDE.md decision-making heuristic #5: "Non-trivial design changes ship four artefacts together" — code (MARG.1-3), doc (this fragment), tests (in each slice), and bench (here).

Changes:

1. `crates/lattice-completion/benches/annotation_pipeline.rs` — micro-bench: 1000 candidates, 3 annotators each, measure pipeline throughput. Asserts the per-call overhead stays under the budget (TBD: target < 50us for the 1000-candidate case).
2. `docs/dev/architecture/marginalia.md` §7 plugin extensibility — verify the `Annotation::Custom { text, slot }` escape hatch is reachable from outside `lattice-completion`. Add a smoke test that an external annotator pushing `Custom { slot: "annotation_plugin" }` renders with the fallback slot.
3. Verify `BENCHMARKS.md` lists the new bench.
4. Status flip: MARG slice plan moves to ✅; cross-reference updated in the design fragment.

Acceptance: bench passes; plugin escape hatch verified; slice series closed.

## Cross-references

- Design fragment: [marginalia.md](../../architecture/marginalia.md)
- Substrate: [completion-pipeline-unification.md](../../architecture/completion-pipeline-unification.md) (note in the GPUI render-gap row that MARG.3 closes it)
- Standing rules touched: `feedback_tui_gpui_parity` (MARG.3), CLAUDE.md decision heuristic #5 four-artefact rule (MARG.4)
