# Archived planning + audit documents

These documents are **historical planning artefacts** for work that
has fully landed. They captured design rationale, slice ordering,
and field-level audits during active migration; once the
corresponding code shipped, their value as "what to do next"
expired. Kept here so the rationale isn't lost (commit messages
summarise the decisions but rarely the alternatives weighed).

The current state of the editor is documented in:

- [`../architecture/design.md`](../architecture/design.md) — live spec
- [`../operations/implementation.md`](../operations/implementation.md) — slice ledger
- [`../operations/benchmarks.md`](../operations/benchmarks.md) — perf snapshots + how to reproduce

If a doc here references types / functions / files that no longer
exist, trust the live spec + the source over the archive.

## Index

| Doc | Originated | Closed by | What it captured |
|-----|------------|-----------|------------------|
| [`phase-5-extraction.md`](phase-5-extraction.md) | Phase 5 plan | Phase 5.7.B / 5.8.* | Planning for splitting `lattice-ui-tui` into renderer-agnostic host + peer renderers |
| [`phase-5-dispatch-extraction.md`](phase-5-dispatch-extraction.md) | Phase 5.5 plan | Phase 5.5.G+ | Planning for `App::apply` → `Editor::dispatch` move |
| [`phase-5b-app-design.md`](phase-5b-app-design.md) | Phase 5.B audit | Phase 5.B.3 | The composition vs generics decision (Option D → Option E) for App layout |
| [`phase-5b-app-fields.md`](phase-5b-app-fields.md) | Phase 5.B.0 audit | Phase 5.B.3 | Field-level inventory of `App` confirming ~99% renderer-agnostic |
| [`3c-final-editor-thread.md`](3c-final-editor-thread.md) | Slice 3c.final plan | Slice 3c.final.E.swap | Design for the Editor-on-its-own-thread pivot |
| [`3c-final-audit.md`](3c-final-audit.md) | Slice 3c.final.A audit | Slice 3c.final.E.swap | Renderer ↔ Editor read/write enumeration that drove slices B/C/D/E |
| [`3c-final-b-extension.md`](3c-final-b-extension.md) | Slice 3c.final.B-extension plan | Slices B.7–B.11 | Five RS-lift slices that retired the last per-frame `read_editor` calls in paint paths |
| [`render-thread-discipline-remediation.md`](render-thread-discipline-remediation.md) | Phase 5.8.AF.5 X-series | Slices X1/X2/X3/X4/X5 + 3c.final | UI-thread offload of parsing / paint / dispatch |
| [`gpui-perf-plan.md`](gpui-perf-plan.md) | GPUI perf plan (A–F) | Slices A.1–A.4, A.2*, B.1, B.2.*, B.4.*, C, D.1, E.1, F + E.2 (dropped) | 19 perf slices: ensure gating, fold O(log), worker pre-paint + inlay weave, overlay buckets, `Arc<[T]>` publish, identity-preserving Arc publish, build-profile tightening |
| [`8i-approach.md`](8i-approach.md) | M3 / Slice 8.i plan | M3 keymap migration | Approach memo for retiring the `bind_legacy` keymap bridge |
| [`m3-binding-census.md`](m3-binding-census.md) | M3 / Slice 8 audit | M3 keymap migration | Inventory of every keybinding for the trie-driven dispatcher port |
| [`keymap-substrate.md`](keymap-substrate.md) | 2026-06-01 | K.2.7 (2026-06-02) | K.2 mode-owned keymap binding substrate — all sub-slices K.2.1–K.2.7 ✅ |
| [`help-prefix.md`](help-prefix.md) | 2026-06-02 | K.3.4 (2026-06-02) | K.3 `<C-h>` help-prefix bindings — K.3.0–K.3.4 ✅ |
| [`marginalia.md`](marginalia.md) | 2026-06-03 | MARG.5 (2026-06-03) | MARG typed marginalia + keybinding column — MARG.1–MARG.5 ✅ |
| [`virtual-rows.md`](virtual-rows.md) | 2026-05-28 | D.0a.1 (2026-05-28) | D.0a virtual-row data layer in `lattice-cells` — D.0a and D.0a.1 ✅ |
| [`pane-groups.md`](pane-groups.md) | D.4 plan | D.4.e | D.4 pane-group primitive (diff side-by-side, scrollbind) — D.4.a–D.4.e ✅ |
| [`ui-tui-refactor.md`](ui-tui-refactor.md) | Phase 5 | R.1.98 | `lattice-ui-tui` decomposition into per-feature App submodules — primary goal achieved |

| [`diff-system.md`](diff-system.md) | D-series plan | D.8 (2026-05-30) | Diff system 18 slices: inline overlay, hunk map, side-by-side, hunk transfer, diffthis grouping ✅ |
| [`multibuffer-views.md`](multibuffer-views.md) | M-series plan | M.8 (2026-06-03) | Multibuffer views 26 slices: excerpt rendering, fold providers, search provider, event subscriptions, mode audit ✅ |
| [`fold-architecture.md`](fold-architecture.md) | F-series plan | F.2 (2026-05-29) | Fold architecture 2 slices: fold-state substrate + provider protocol ✅ |
| [`buffer-local-options.md`](buffer-local-options.md) | O-series plan | O.3 (2026-06-04) | Buffer-local options 3 slices: OptionOverride substrate, resolver, per-buffer FrameView integration ✅ |
| [`kind-agnostic-buffers.md`](kind-agnostic-buffers.md) | K.4-series plan | K.4.11 (2026-06-05) | Kind-agnostic buffer + mode infrastructure 3 phases: Document trait, dispatcher, multibuffer test suite ✅ |
| [`keymap-impl-plan.md`](keymap-impl-plan.md) | K-series plan | K.3.4 (2026-06-07) | lattice-keymap crate + resolution overhaul: K.1 trie dispatcher, K.2 mode-owned binding substrate, K.3 help-prefix ✅ |
| [`mode-ownership-cleanup.md`](mode-ownership-cleanup.md) | MO-series plan | MO.4.c (2026-06-09) | Mode ownership cleanup 6 slices: LSP/oil/snippet keymap migration, gutter decoration migration, status-line infra, Subscription RAII ✅ |
| [`lattice-keymap-crate-design.md`](lattice-keymap-crate-design.md) | 2026-06-06 design | K series ✅ | Design fragment for the lattice-keymap crate (Approach 3, crate-first) — was misplaced in slice-plans/ |
| [`soft-wrap.md`](soft-wrap.md) | W-series plan | W.7 (2026-06-07) | Soft line-wrap 7 slices + W.4.t tab-width: cells geometry, TUI render, GPUI parity, gj/gk motions, live toggle ✅ |
| [`tutor.md`](tutor.md) | T-series plan | T.5 (2026-06-07) | Interactive tutor 5 slices: data types, exercise sidecars, TutorMode skeleton, success evaluation, --tutor CLI flag ✅ |

## Conventions

Each file's preamble carries a `> **Status: ✅ Completed.**` banner
linking to the closing slice. Body text is left as-shipped — the
historical record is the value.
