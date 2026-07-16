+++
title = "Picker preview syntax highlighting (PH)"
+++


**Status:** ✅ complete (2026-07-01). PH.1 (substrate: `DisplaySpan` carrier + both-peer paint + match composition), PH.2 (Lines/Outline active-buffer producers + per-frame bench), and PH.3 (Grep arbitrary-file highlighting) all landed — `:picker lines` / `:picker outline` / `:picker grep` render syntax colors on both peers, recoloring on `:colorscheme`. The §4 dependency-cycle question resolved toward the primary semantic-`Style` design (`lattice-cells` is a leaf crate → no cycle). Slice plan: `docs/dev/operations/slice-plans/archive/picker-marginalia.md` (PH series).

A sibling surface to marginalia (`marginalia.md`), not part of it. Marginalia colors the trailing annotation *columns* (perm/size/location, §8–9 there). This feature colors the **source code inside the row** — the `preview` of a Grep hit, the line text of `:picker lines`, the symbol context of `:picker outline` — using tree-sitter, so a code snippet in a picker reads like it does in the buffer.

## 1. Vision

Picker rows that contain source code render that code with the same syntax colors as the editor. Default (no tree-sitter grammar for the language, or an unparsed file) is the current single-color preview; supported-and-parsed languages get full highlighting. The colors come from the **existing syntax theme namespace** (`syntax.keyword`, `syntax.string`, …), so a `:colorscheme` swap recolors picker previews and buffers in lockstep, and no new theme slots are invented for code text.

## 2. The two surfaces are different

| | Marginalia (`marginalia.md`) | Preview highlight (this doc) |
|---|---|---|
| What | trailing annotation columns | the `display` run itself (the matchable text) |
| Color source | `completion.annotation.*` slots | `syntax.*` slots (capture → `Style`) |
| Data carrier | `Vec<Annotation>` (exists) | new `display_spans` field (new) |
| Composes with | column layout | **fuzzy match-highlight** (same text run) |

Because the colored text *is* the fuzzy-match target, preview highlight must compose with match highlighting; marginalia never touches the display run, so it doesn't.

## 3. Paramount-goal alignment

| Goal | This feature |
|---|---|
| #1 perf | **Load-bearing constraint.** The renderer must do zero tree-sitter work. Spans are produced off the UI thread at candidate-build time and ride on the candidate as data; the render path is a per-span color lookup, O(visible-row chars). For Lines/Outline this is *free of new parsing* — a read-only query against the active buffer's already-parsed tree (`SyntaxSnapshot::highlight_lines`, `&self`, no reparse). |
| #2 extensibility | Reuses the carry-semantic-style / resolve-at-seam pattern that marginalia §3 established; `resolve_syntax_style` is the existing editor resolver. |
| #3 grammar | Neutral. |
| #4 async | Lines/Outline sources already build inline on the editor-actor thread (off the render thread); Grep (PH.3, deferred) builds on the LSP runtime. The span build stays wherever the source already runs. |

## 4. Data model

A candidate gains an optional styled overlay for its `display` run. The closest precedent is `Annotation::Styled` (marginalia §8.2): carry the *semantic* style as data, resolve to a theme color only at the render seam — never bake a color.

```rust
/// A styled run within a candidate's `display` text. Carries a
/// semantic syntax `Style` (a closed enum), NOT a resolved color —
/// the renderer resolves it via `resolve_syntax_style` at paint, so
/// `:colorscheme` recolors live. Ranges are byte offsets into `display`.
pub struct DisplaySpan {
    pub range: Range<usize>,
    pub style: Style, // lattice-cells semantic style (capture-name independent)
}
```

- Carried on the rendered candidate alongside `annotations` (e.g. `display_spans: Vec<DisplaySpan>`, empty = today's plain preview).
- **Why semantic `Style`, not a slot-key `Arc<str>` (as `AnnotationSegment` uses):** syntax styles are a *closed* enum with an existing `Style → ElementId` map (`syntax_element_id`); annotation slots are *open* (plugins name arbitrary slots) and so use strings. Type-safety + direct resolution win for the closed set. Cost: `lattice-completion` gains a dependency on the crate defining `Style` (`lattice-cells`). PH.1 verifies this introduces no dependency cycle; if it does, fall back to a slot-key string with a syntax-slot name resolver.

## 5. Rendering contract (both peers)

Both peers already build multi-span/multi-cell display rows by walking `match_ranges` (TUI `candidate_to_line`, GPUI `paint_candidate_row`). Preview highlight subdivides those runs further by a per-byte style lookup, exactly as the `Annotation::Styled` branch already resolves `seg.slot`.

**Composition rule — match-highlight wins on overlap.** A character that is both syntax-colored and inside a fuzzy `match_range` paints with the match style (distinct fg + bold), not the syntax color. Convention across Telescope / fzf-lua / VSCode: the user must see *what matched* first; syntax color applies to the non-matched runs. (UX rule: lead with cross-editor convention for muscle-memory surfaces.) Ranges no `DisplaySpan` covers paint in the row foreground — i.e. today's "plain" preview, so **no new plain theme slot is needed**.

Resolution: `lattice_host::ui::theme::resolve_syntax_style(resolved, ids, style)` (re-export of `lattice-syntax`'s `theme_style`). Both peers already call this on the main editor path; preview highlight reuses it.

## 6. Producer — Lines / Outline (the cheap path)

The active buffer's parsed tree is live in the host as `self.syntax` (`Arc<ArcSwap<SyntaxSnapshot>>`). Today `PickerContext` / `ActiveBufferSnapshot` (`lattice-picker/src/context.rs`) exposes the raw buffer and pre-collected `syntax_symbols`, but **not** highlight spans. Mirroring the `syntax_symbols` precedent, the host pre-collects per-line highlight spans at `build_picker_context` time and exposes them on `ActiveBufferSnapshot`:

```rust
// host-side, build_picker_context — read-only query, no reparse:
let spans: Vec<Vec<StyledSpan>> = snapshot.highlight_lines(lo, hi)?; // per visible/candidate line
```

`LinesSource` and `OutlineSource` then map the spans for each candidate's line into `DisplaySpan`s offset to the `display` text (line text after the `lineno:` prefix is dropped — the line number moves to a `location` marginalia cell per `marginalia.md` §9.3). A line with no spans (blank, or a `Style::None` run) → empty `display_spans` → plain preview.

This adds no parsing: `highlight_lines` is a `tree_sitter::QueryCursor` pass over the cached tree, and these sources already run synchronously on the editor-actor thread.

## 7. Grep across arbitrary files (PH.3) ✅

Grep previews come from `rg`/`ag`/`grep` over **arbitrary files**, which are *not* the active buffer and have no parsed tree. Highlighting them requires, per hit: (a) select a grammar by file extension, (b) parse the previewed line under that grammar, (c) map captures to `Style`. This is genuine per-hit parsing work — but `GrepSource` already builds candidates off-thread (`spawn_blocking`), so it's safe there.

**Shipped shape (producer-only — carrier + composition §4–5 unchanged):**

- `GrepPreviewHighlighter` trait in `lattice-picker` (abstract). The crate keeps **no** `lattice-syntax`/`lattice-cells` production dependency, so "the picker can't parse on the render thread" stays a *compile-time* fact, not discipline. The host injects the impl (trait-injection — the same seam every host-only picker capability uses).
- `SyntaxGrepHighlighter` (host) selects a grammar by extension (`Lang::detect_from_path`) and parses the single preview line under a **per-language cached** `Syntax` (`Mutex<HashMap<Lang, Option<Syntax>>>`). The expensive part — compiling a grammar's highlight query — is reused across hits and across live-grep keystrokes; only the short line re-parses per hit. No grammar match → plain preview.
- The highlighting runs **inside** `GrepSource::spawn_grep`'s `spawn_blocking` closure (with the grep), so the per-hit parse never lands on an async runtime worker. The preview text *is* the candidate `display`, so spans are display-relative — no offset math.

Rejected the original "cap by visible-window" sketch: with the per-language query cache, a short-line parse per hit is cheap enough that highlighting the whole (bounded `picker.grep.max-hits`) result set keeps spans ready the instant results seat, avoiding a second async re-decoration pass. Injections are not run (single-line context). Heuristic #1: the cheap active-buffer paths (PH.2) shipped first and proved the UX before this took on per-hit parsing.

## 8. Rejected alternatives

- **Highlight in the renderer.** Run tree-sitter while painting picker rows. Violates paramount goal #1 (UI-thread parsing) outright. Rejected.
- **Reuse the marginalia `Annotation` mechanism for code text.** Annotations are trailing columns; the preview is the display run, and it must compose with match-highlight in the same text. Different surface — forcing code text into a right-hand column would lose the inline reading the feature exists for. Rejected (see §2).
- **Bake resolved colors into the candidate at produce time.** Breaks live `:colorscheme` (marginalia §3, same reasoning). `DisplaySpan` carries semantic `Style`, resolved at the seam — explicitly not this.
- **Ship Grep highlighting in v1.** Per-hit parsing + grammar cache is real surface area for a preview nicety; the active-buffer pickers deliver most of the value at a fraction of the risk. Deferred, not dropped (§7).

## 9. Tests + artefacts

- `DisplaySpan` resolution: a span with each `Style` resolves to its `syntax.*` slot fg on both peers; `:colorscheme` swap recolors.
- Composition: a char in both a `DisplaySpan` and a `match_range` paints the match style, not the syntax color; non-matched syntax runs keep their color; uncovered runs are row-fg.
- Producer: `LinesSource`/`OutlineSource` emit `display_spans` aligned to `display` for a parsed fixture buffer; an unparsed / no-grammar buffer emits none (plain preview, no panic).
- Off-thread: assert span production happens in the source build, not the render path (no `highlight_lines` call reachable from `Render::render`).
- Bench: a picker-render bench row set with `display_spans` populated, recording per-frame resolution cost (the §3 O(visible chars) claim, examinable in CI).

## 10. Cross-references

- Sibling surface (annotation columns), shares the candidate-render pipeline + colorscheme machinery: `docs/dev/architecture/marginalia.md` (esp. §3 slot-key invariant, §9 picker rollout)
- Slice sequencing: `docs/dev/operations/slice-plans/archive/picker-marginalia.md`
- Syntax highlight substrate (`SyntaxSnapshot::highlight_lines`, `StyledSpan`, `Style`): `lattice-syntax`, `lattice-cells/src/style.rs`
- Capture → theme resolution reused at the render seam: `lattice-syntax/src/theme_style.rs` (`resolve_syntax_style`, `syntax_element_id`)
- Main-editor proof that `highlight_lines` against the live snapshot is the per-frame path: `lattice-host/src/cells_worker.rs`
