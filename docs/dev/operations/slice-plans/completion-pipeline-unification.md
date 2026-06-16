# Completion-pipeline unification — slice plan

Sequencing companion to
[`docs/dev/architecture/completion-pipeline-unification.md`](../../architecture/completion-pipeline-unification.md).
The design fragment is the source of truth for *what* and *why*;
this file owns *when* and *in what order*. Authoritative status
per slice lives in [`../implementation.md`](../implementation.md).

> **Status: ✅ complete (verified-from-source 2026-06-17).** Slices 1–8
> landed, plus the full 7a–7d arc and beyond (`feat(slice 3c.unify…)`
> commits): ranker-stack, MRU promotion, picker-via-pipeline, option-doc
> annotators, GPUI annotation render; **7a–7b** unified the picker sources
> onto `lattice_completion::CandidateGenerator` and migrated all 10
> first-party sources with typed `accept_action` (`7b.0`–`7b.6`); **7c**
> `CompletionRegistry` stores `SourceRegistration` bundles (`9a3a5f5e`);
> **7d** picker-registry cutover (`090d4db3` accept-dispatch via
> `DefaultAcceptHandler`, `839e009b` dual-registry lookup); **7g** typed
> `AcceptAction` drives picker preview; **slice 8** benchmarks
> (`b48e7315`); LSP stateful pickers stamped (slices 9–16). One pipeline,
> one source contract — paramount #2 satisfied.
>
> **Deliberately descoped (not pending):** 7e/7f source *relocation* —
> `a037a337` decided to KEEP `lattice-picker` as the picker-surface crate
> rather than move sources. **Follow-up arc:** slice 17 (`PickerAction`
> retirement, `046e2d24`) is queued separately. Neither blocks completion.

| #   | Slice                                       | Effort | Description |
|-----|---------------------------------------------|--------|-------------|
| 1   | `3c.unify.ranker-stack`                     | small  | `Pipeline::ranker` → `Pipeline::rankers: Vec<>`. Composable rankers. Builtin `ScoreRanker` unchanged. Tests updated. |
| 2   | `3c.unify.mru-promotion`                    | small  | `MruRanker` moves from `lattice-picker` to `lattice-completion`. Picker's `mru_bonuses` field plumbed into a `MruRanker` instance at filter time. Drops picker's inline `combined = score + bonus` arithmetic. |
| 3   | `3c.unify.picker-via-pipeline`              | medium | `Picker::filter()` rewritten to call `Pipeline::run` with a `PreSuppliedGenerator` adapter that yields `raw: Vec<RawCandidate>`. Inline filter loop deleted. **Net deletion**, not addition. |
| 4   | `3c.unify.option-doc-annotator`             | small  | `OptionType::enumerate_with_docs()` extension + default impl. `OptionValueDocAnnotator` + `OptionNameDocAnnotator`. Wired into the cmdline pipeline for option-name + option-value slots. |
| 5   | `3c.unify.option-docs-builtin`              | small  | Concrete per-value docs added to built-in options that have enumerable values (foldmethod, foldenable, picker.display, ...). |
| 6   | `3c.unify.gpui-annotation-render`           | small  | GPUI picker overlay + cmdline-completion strip render annotations right-aligned (TUI already does). |
| 7a  | `3c.unify.picker-generator-trait-unify`     | small  | Make picker sources implement `lattice_completion::CandidateGenerator` (either via trait merger or thin adapter). The picker's own `PickerSourceGenerator` retires (or becomes an alias). |
| 7b  | `3c.unify.first-party-source-migration`     | medium | Migrate the 10 first-party picker sources (`FilesSource`, `BuffersSource`, `CommandsSource`, `LinesSource`, `JumpsSource`, `RegistersSource`, `MarksSource`, `GrepSource`, `OutlineSource`, `RecentFilesSource`) to the unified trait. |
| 7c  | `3c.unify.source-registration-bundle`       | small  | Add `SourceRegistration { name, generator, matcher_override, ranker_overrides, annotator_extras, ui_hints }` + `CompletionRegistry::register_source`. Per-source override storage. First-party sources re-register through the bundle. |
| 7d  | `3c.unify.picker-registry-cutover`          | small  | `:picker <name>` lookup routes through `CompletionRegistry::source_by_name`. Picker stops maintaining its own registry. Single source of truth. |
| 8   | `3c.unify.benchmarks`                       | small  | Per-slice perf check: pipeline-vs-inline overhead at picker scales (5k, 50k candidates) recorded in `benchmarks.md`. Catches regressions before they ship. |

Slices 1-3 are the architectural core. Slices 4-6 deliver the
marginalia improvement. Slices 7a-7d set up the plugin contract
AND make it real (first-party sources flow through it). Slice 8
enforces no perf regression.

**Slice 7 estimate revision** (2026-05-21): originally scoped as
one "medium" slice. Honest assessment is four sub-slices. The
substrate alone (just `SourceRegistration` + `register_source`)
without migrating first-party sources is vaporware — first-party
sources stay on `PickerSourceGenerator`, plugins would land on
`SourceRegistration`, that's two patterns, paramount goal #2
fails ("ONE WIT shape, not two"). Doing 7a-7d together is the
only way the substrate is actually load-bearing.

Each slice ships green and on its own — none depend on a later
slice landing. Slices 1-3 land in order (each depends on the
previous); 4-6 land in order; 7 can land anytime after 3; 8 is
a continuous obligation.
