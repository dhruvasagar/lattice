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
- [`../operations/3c-final-b-extension.md`](../operations/3c-final-b-extension.md) — queued perf work

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
| [`render-thread-discipline-remediation.md`](render-thread-discipline-remediation.md) | Phase 5.8.AF.5 X-series | Slices X1/X2/X3/X4/X5 + 3c.final | UI-thread offload of parsing / paint / dispatch |
| [`8i-approach.md`](8i-approach.md) | M3 / Slice 8.i plan | M3 keymap migration | Approach memo for retiring the `bind_legacy` keymap bridge |
| [`m3-binding-census.md`](m3-binding-census.md) | M3 / Slice 8 audit | M3 keymap migration | Inventory of every keybinding for the trie-driven dispatcher port |

## Conventions

Each file's preamble carries a `> **Status: ✅ Completed.**` banner
linking to the closing slice. Body text is left as-shipped — the
historical record is the value.
