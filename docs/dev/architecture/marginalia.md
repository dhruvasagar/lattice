# Marginalia — typed annotations for completion candidates

> **Status (2026-06-03):** §§1–7 landed as the `MARG.*` slice series (typed `Annotation` enum, keybinding annotator, GPUI parity, MARG.5 column layout) plus T.6 (GPUI annotation colors read from theme). Closed slice plan: `docs/dev/archive/marginalia.md`. Cross-references existing fragments: `completion-pipeline-unification.md` (the substrate this rides on), `insert-completion.md` (sibling consumer of the candidate-rendering pipeline).
>
> **Status (2026-06-30):** §8 (rich per-segment marginalia) **landed** as the `MR.1–MR.4` slice series — `Annotation::Styled` colors a single column cell per-segment (eza-style permission bits), the file/dir picker is the first file-metadata producer (perms / size / mtime), and the remaining TUI theme-wiring gap is closed. Marginalia recolors live on `:colorscheme` on both peers; bench in `benchmarks.md` (MR §8). Slice plan: `docs/dev/operations/slice-plans/archive/marginalia-rich.md`.

## 1. Vision

Emacs's vertico + marginalia pattern: each completion candidate carries one or more annotations rendered to its right, color-coded by category. The most useful expression of this — for a modal, plugin-first editor — is showing the **keybinding** that fires a command directly in the `:` line picker. The user trying to remember "what was the command for X?" sees the chord they could've typed instead, and learns the keymap by doing.

Lattice today has the annotation column but doesn't carry annotation categories — every annotation is the same color regardless of meaning. Two consequences:

1. The **keybinding column** the user wants for command discovery has nowhere to live with its own color slot.
2. The **existing annotations** (`(motion)`, `(command)`, doc snippets) all collapse to one undifferentiated grey, hurting scannability — you can't quickly find the doc string vs. the kind tag by color.

This fragment defines the substrate that color-codes annotations by category, and registers the keybinding annotator as the first new beneficiary.

## 2. Paramount-goal alignment

| Goal | How marginalia serves it |
|---|---|
| #1 perf | Annotation rendering is O(visible-candidates × annotations-per-candidate); cap ~30 visible rows × ~3 annotations = 90 lookups per popup-open. Style lookup is O(1) theme-slot read. Reverse keymap-cache lookup is O(1) hash. No measurable cost. |
| #2 extensibility | **Load-bearing.** The typed `Annotation` enum is the contract plugins extend. Future WASM-plugin annotators contribute new variants via the registered-extension surface; each variant's theme slot composes with the user's palette without plugin code touching colors. |
| #3 grammar | Neutral — the marginalia substrate doesn't touch the vim grammar. The keybinding annotator *consumes* the grammar's chord vocabulary (showing `<C-w>v` for `:split-pane-vertical`); it doesn't introduce new motions. |
| #4 async | Neutral — annotators run synchronously inside the completion pipeline. The reverse keymap-cache is built off the UI thread (rebuilt when the keymap-trie rebuilds; same wake path). |

## 3. Architectural choice

Three shapes for the typed-annotation data model. Final choice: **(a) tagged enum** — `Annotation::Kind(String) | DocSnippet(String) | Keybinding(SmallVec<KeyChord>) | Source(String) | Severity(Severity) | ...`. Renderer matches the variant, reads `theme.completion.annotation_<variant>` for style, formats the payload to text per-variant.

The directions debated and rejected:

- **(b) Style-tagged string struct.** `struct Annotation { text: String, kind: AnnotationKind }`. Functionally identical to (a) for color-coding; difference is semantic richness. The keybinding variant flattens `Vec<KeyChord>` to a string at annotation time, losing the structured chord info. Loses future affordances: "click chord to edit binding," "show conflict warning next to ambiguous chords," "highlight the prefix-matching chord during partial-chord state." The flattening cost shows up later when those affordances want to walk the chord list — at which point the variant needs to be split back out. Build the right shape now.
- **(c) Pre-styled spans.** `annotations: Vec<Vec<StyledSpan>>` — annotators do theme lookup themselves and produce styled spans. Pushes theme concern into every annotator crate. Cross-cutting theme changes (palette switch, dark/light mode toggle) become invasive: every annotator needs to re-render. Centralizing theme at the render seam is the substrate-side responsibility.

### Why (a) against the paramount goals

| Goal | (a) against it |
|---|---|
| #1 perf | Same as (b) — one match arm + one theme lookup + one format call per annotation. Variant payload is `SmallVec<KeyChord; 2>` for Keybinding (inline-stored up to 2 chords), `Arc<str>` for string variants — no boxing on the hot path. |
| #2 extensibility | Stronger than (b). The variant payload IS the contract. Plugin annotators can return rich data (a `Severity` for diagnostic-suggestion candidates, a `LspKind` for completion-item candidates) and the renderer handles formatting + styling. (b) forces every annotator to pre-format. |
| #3 grammar | Neutral. |
| #4 async | Same as (b). Synchronous annotator pipeline. |

## 4. Data model

```rust
/// One annotation attached to a completion candidate.
/// Variants are open-by-versioning: adding a variant is a
/// minor-version bump; removing one is breaking.
#[derive(Debug, Clone)]
pub enum Annotation {
    /// Category label like `(motion)`, `(command)`, `(file)`.
    /// Today's `KindLabelAnnotator` output.
    Kind(Arc<str>),

    /// First line of a command's doc string.
    /// Today's `DocSnippetAnnotator` output.
    DocSnippet(Arc<str>),

    /// Chord(s) bound to this command, if any.
    /// Empty `SmallVec` is invalid — annotators should not
    /// emit this variant when no chord binds.
    Keybinding(SmallVec<[KeyChord; 2]>),

    /// Provenance: which crate / mode / user-config defined
    /// this command. `Arc<str>` because most candidates share
    /// the same source label (`"builtin"`, `"lsp"`,
    /// `"user-init"`).
    Source(Arc<str>),

    /// For diagnostic-suggestion / code-action candidates.
    /// Renderer maps to the diagnostic theme slot palette so
    /// hint / info / warn / error severities are visually
    /// distinct from the same enum used in the gutter.
    Severity(lattice_protocol::Severity),

    /// Escape hatch for plugin-contributed annotations that
    /// don't fit any built-in variant. Plugins pre-format
    /// their text and pick a theme slot key by name. The
    /// renderer resolves the slot at paint time; unknown
    /// slots fall back to `theme.completion.annotation_plugin`.
    /// This is the v1 plugin contract — a typed `Plugin`
    /// variant per plugin lands when WASM-plugin annotators
    /// arrive (post-v1).
    Custom { text: Arc<str>, slot: Arc<str> },
}
```

Replaces the current `pub annotations: Vec<String>` field on `RenderedCandidate`:

```rust
pub struct RenderedCandidate {
    pub raw: RawCandidate,
    pub score: MatchScore,
    pub match_ranges: Vec<Range<usize>>,
    pub annotations: Vec<Annotation>,  // Vec<String> -> Vec<Annotation>
}
```

### Rendering contract

The renderer is the only consumer of the variant tags. Per annotation:

1. Match the variant; pick the theme slot.
2. Format the payload to a display string (per-variant logic — `Keybinding` formats chords via the canonical chord-display function shared with `:describe-key`; `Severity` formats as `[error]` / `[warn]` / `[hint]`; etc.).
3. Push a `StyledSpan` with the formatted text + the resolved style.

Multiple annotations on one candidate are joined with two spaces (current behavior, preserved). Each span carries its own style; the joiner spaces inherit the row style (selected vs. unselected background).

**Column alignment across rows (MARG.5).** Annotations render in *category columns*, not as a per-row free-flowing list. Before painting the visible batch, the renderer builds a `lattice_completion::AnnotationColumns` from the visible candidate set: one column per category present in any visible row, each column's width = the max `display_text().chars().count()` across visible rows that carry that category. Columns are ordered by a fixed display rank (`keybinding → kind → doc → source → custom`, matching the registered annotator order). Each row then walks the columns in order: for a column whose category the row has, it paints the annotation padded to the column width; for a column the row *lacks*, it paints a blank cell of the column width. The result: a candidate with no keybinding renders an empty keybinding cell so its kind/doc columns stay vertically aligned with rows that do have a keybinding. The width math lives in `lattice-completion` (not the renderer crates) so the two peer renderers — TUI ratatui spans and GPUI element-tree — compute identical column geometry from one source. When no visible candidate carries any annotation, `AnnotationColumns::is_empty()` is true and the renderer skips the annotation column (including the leading pad) entirely.

## 5. Theme integration

New theme slots under `theme.completion.annotation_*`:

| Slot                                    | Default (dark theme)         | Used by                                   |
|-----------------------------------------|------------------------------|-------------------------------------------|
| `theme.completion.annotation_kind`      | `DarkGray` (current default) | `Annotation::Kind`                        |
| `theme.completion.annotation_doc`       | `Cyan` dim                   | `Annotation::DocSnippet`                  |
| `theme.completion.annotation_keybinding`| `Yellow`                     | `Annotation::Keybinding`                  |
| `theme.completion.annotation_source`    | `Magenta` dim                | `Annotation::Source`                      |
| `theme.completion.annotation_severity_*`| reuse `theme.diagnostic.*`   | `Annotation::Severity` (one slot per sev) |
| `theme.completion.annotation_plugin`    | `Blue` dim                   | `Annotation::Custom` fallback             |

Rationale on the keybinding default: `Yellow` matches the help-mode's chord-highlight slot (`theme.help.chord`), keeping the chord visual consistent across `:describe-key` output and the completion popup. Users coming from vim's `:map`-listing tradition expect chord text to stand out yellow-or-bright.

**Selected-row contrast:** today the renderer flips annotation fg between `Gray` (selected row's bg is `DarkGray`) and `DarkGray` (unselected row's bg is the popup default). With typed annotations, each slot's `selected` and `unselected` variants are theme-defined — the `Yellow` keybinding on the selected row's `DarkGray` bg needs the brighter `Yellow` shade, while the unselected row can use the dimmer one. Theme slots carry both `fg` and `fg_selected`.

## 6. The keybinding annotator

```rust
pub struct KeybindingAnnotator;

impl Annotator for KeybindingAnnotator {
    fn annotate(&self, candidate: &mut RenderedCandidate, ctx: &AnnotatorContext) {
        let Some(cmd_id) = candidate.command_id() else { return };
        let chords = ctx.keymap_reverse.lookup(cmd_id);
        if chords.is_empty() { return; }
        candidate.annotations.push(Annotation::Keybinding(chords));
    }
}
```

`AnnotatorContext` is the existing annotator-pipeline context, extended with a `keymap_reverse: &KeymapReverseCache` reference.

### Reverse-cache shape

`KeymapReverseCache` is a `HashMap<CommandId, SmallVec<[KeyChord; 2]>>` populated from the keymap trie at trie-build time. Same invalidation surface as the trie itself — every `:map` / `:unmap` / user-config reload that rebuilds the trie also rebuilds the reverse cache.

Build cost: O(N) where N is the bound-chord count. Sub-ms for typical keymaps (<= 500 chords). Runs off the UI thread.

Storage: most commands have 0 or 1 chords; a tiny minority have 2 (different layers binding the same command); `SmallVec` keeps both cases inline-stored. Past 2, the SmallVec spills to heap — acceptable for the rare case (`:write` bound to `:w`, `<leader>w`, `<C-s>`, ...). The display rule for multi-binding commands is "show the first bound chord" — the alternates are reachable via `:describe-command` which already lists every binding.

## 7. Plugin extensibility

Phase 4 (WASM plugin host) gets two extension points:

1. **Plugin annotators** register against `CompletionRegistry` and receive `AnnotatorContext` over the host-call boundary. They produce `Annotation::Custom { text, slot }` — the host doesn't trust plugin-supplied text to be pre-formatted but does trust the slot key.
2. **Plugin theme slots** can declare new `annotation_*` slots that user themes can override. Resolution falls back to `annotation_plugin` for unknown slots, so a missing slot is a paint warning, not a panic.

Pre-Phase-4 (today): the `Annotation::Custom` escape hatch is reserved for in-tree extension crates that want to ship without a variant of their own. Default annotators (`Kind`, `DocSnippet`, `Keybinding`, `Source`) cover the cases on the table.

## 8. Rich per-segment marginalia (file metadata) — 2026-06-30 extension

Extends §4. MARG.1–5 + T.6 give typed annotations, shared column layout, and (GPUI-side) theme-resolved colors. They lack exactly one thing: **per-segment coloring within a single column cell**. This section adds it and registers the file/dir picker as the first producer of file-metadata marginalia (permissions, size, mtime) with eza-style per-bit permission colors.

### 8.1 The gap

Every §4 variant maps to exactly one theme slot, i.e. one color per column cell. The file/dir picker wants a `drwxr-xr-x` permission column where each bit class is its own color (eza / `ls --color` convention) — many colors inside *one* column. The single-slot model can't express that.

### 8.2 Data model — `Annotation::Styled`

```rust
/// One run of marginalia text sharing a theme slot. Carries a
/// slot KEY, never a resolved color — theme resolution stays at
/// the render seam (the §3 invariant; this is NOT the rejected
/// pre-styled-spans alternative, which baked colors at produce
/// time).
pub struct AnnotationSegment {
    pub text: Arc<str>,
    pub slot: Arc<str>,
}

enum Annotation {
    // … §4 variants …
    /// A column cell colored per-segment. The permission string
    /// is one segment per bit class; any future multi-colored
    /// field (size+unit, path head/tail, …) reuses it. `category`
    /// keys the column exactly like the single-variant `category()`.
    Styled { category: Arc<str>, segments: Vec<AnnotationSegment> },
}
```

- `display_text()` returns the segments concatenated → `AnnotationColumns` width math (MARG.5) is **unchanged**; styled and single-variant cells share the same column grid.
- `category()` returns `category` → column layout unchanged.
- `Styled` is the multi-slot generalization of `Custom { text, slot }`. `Custom` stays for single-color plugin annotations.

### 8.3 Rendering contract (both peers)

On `Styled`, a renderer emits one span/div per segment, each resolved against that segment's `slot`, instead of one styled cell. Unknown slot falls back to the custom/plugin annotation color (paint warning, not panic). Width and alignment come from `display_text()` as for every other annotation, so a styled perm cell lines up with a plain `size` cell in the next column. GPUI extends `paint_candidate_row` / `annotation_color_rgb`; TUI extends `candidate_to_line` (see §8.6).

### 8.4 Producer — file/dir picker

`crates/lattice-picker/src/picker_sources.rs` stops baking the flat `"{path} {perms} {size} {mtime}"` string into `RawCandidate.display`. Per row it emits structured annotations:

- `Styled { category: "perm", segments }` — `format_perms` is refactored to yield `(char, slot)` per bit instead of a flat `String`. **The bit→slot policy lives once, here** — renderers stay dumb.
- `Styled { category: "size", segments: [one] }` (slot `completion.annotation.size`).
- `Styled { category: "mtime", segments: [one] }` (slot `completion.annotation.mtime`).

The path remains the candidate text (matched + match-highlighted as today). A file that fails to stat emits no metadata annotations → blank cells, preserving today's behavior. Per-bit slot policy:

| permission char | slot |
|---|---|
| type: `d` `l` `b` `c` `p` `s` `-` (leading) | `…perm.type` |
| `r` | `…perm.read` |
| `w` | `…perm.write` |
| `x` | `…perm.exec` |
| `s` `S` `t` `T` (setuid / setgid / sticky) | `…perm.special` |
| `-` (absent bit) | `…perm.none` |

### 8.5 Theme slots

New slots under the existing `completion.annotation.*` namespace (pickers are completion candidates):

| Slot | Default (dark) | Used by |
|---|---|---|
| `completion.annotation.perm.type` | blue | leading type char |
| `completion.annotation.perm.read` | yellow | `r` |
| `completion.annotation.perm.write` | red | `w` |
| `completion.annotation.perm.exec` | green | `x` |
| `completion.annotation.perm.special` | magenta | setuid / setgid / sticky |
| `completion.annotation.perm.none` | overlay / dim | absent bit `-` |
| `completion.annotation.size` | peach / gold | size column |
| `completion.annotation.mtime` | green | mtime column |

Defaults track eza / `ls --color` (UX rule: permission colors are cross-tool muscle memory; lead with convention). Every slot is colorscheme-overridable and carries `fg` + `fg_selected` per §5's selected-row contrast rule.

### 8.6 Close the TUI theme gap

MARG.1's TUI `candidate_to_line` still resolves annotation colors through the hardcoded `annotation_color()` — the §5 "queued" note closed only on the GPUI side (T.6). This extension threads the resolved theme table + element ids into `candidate_to_line` and replaces `annotation_color()` with theme-slot reads, for **all** annotation categories, not just the new ones. After it, both peers resolve every marginalia color from the theme and a `:colorscheme` swap recolors marginalia live on both. This is a prerequisite — without it the new perm/size/mtime slots would have no effect on TUI.

### 8.7 Paramount-goal alignment (extension)

| Goal | This extension |
|---|---|
| #1 perf | Resolution is O(visible × segments); a perm cell is ≤11 segments, ~30 rows ⇒ ~330 O(1) slot reads per picker frame. No measurable cost; the MARG annotation-render bench is extended with a styled-cell case. |
| #2 extensibility | **Load-bearing.** `Styled` is the contract any producer (multibuffer, LSP, future WASM plugin) uses for multi-colored marginalia — `(text, slot)` data, zero renderer changes per new field. |
| #3 grammar | Neutral. |
| #4 async | Neutral — the stat walk already runs in the picker source's init (off the UI thread); the perm→segment build stays there, never on the render thread. |

### 8.8 Rejected alternatives (extension)

- **Typed semantic variants** (`Permissions(mode)`, `Size(u64)`, `Mtime(SystemTime)`): type-safe, but each new multi-colored field needs a new variant *and* match arms in BOTH peers, and the eza format + color policy leaks into renderer code (duplication across TUI/GPUI). Sacrifices #2. `Styled` keeps the policy in the shared producer and the renderers as pure slot-resolvers.
- **Pre-styled spans**: already rejected in §3 — bakes colors at produce time and breaks live `:colorscheme`. `Styled` carries slot keys, not colors, so it is explicitly *not* this.

### 8.9 Tests + artefacts

- `format_perms` → segments: each bit class maps to its slot; symlink / setuid / setgid / sticky / block / char / fifo / socket edge cases.
- `AnnotationColumns` width with a `Styled` cell equals its concatenated `display_text()` width; alignment holds when styled and non-styled rows mix in one column set.
- TUI + GPUI: a `Styled` perm annotation renders N spans/divs, each with its slot's resolved fg; a `:colorscheme` swap recolors them on both peers.
- Graceful: unstattable file → no metadata cells (no panic); unknown slot → fallback color.
- Bench: extend the MARG annotation-render bench with a styled-cell row set.

## 9. Picker marginalia rollout (MP) — 2026-06-30 extension

§8 added the `Styled` mechanism and made the file/dir picker its first producer. Every *other* picker source still bakes its metadata into a flat, hand-width-padded `display` string and emits zero annotations. This section rolls the §8 treatment across the remaining pickers so marginalia is uniform across picker surfaces (paramount goal #3 — picker rows stop being per-source snowflakes).

### 9.1 Why this is purely additive

Pickers run **no annotators**: a source stamps `RawCandidate.annotations` directly and the candidate carries it straight through (`RenderedCandidate::from_scored`). So for pickers, *the source IS the annotation provider*. Enrichment is therefore local to each source — build typed/`Styled` annotations instead of a flat string, register any new theme slot, add a `category_order` entry. No new enum variant, no annotator wiring: §8's `Styled` already expresses every case here, and both peers already resolve it generically (§8.3). Renderer churn is limited to registering the new slots; the paint paths are untouched.

### 9.2 Shared segment builders

The coordinate-class pickers (Grep, Jumps, Outline, Lines, Marks) and the future LSP locations/symbols picker all format `path:line:col` (or `line:col`) by hand. The policy lives once, as free functions in `lattice-picker` that the sources import (the §8 lesson — `perm_segments` keeps the bit→slot policy in one place, renderers stay dumb):

```rust
/// A colored location cell: dim path, distinct line/col.
fn location_segments(path: Option<&str>, line: u32, col: u32) -> Vec<AnnotationSegment>;
/// Buffer status: dirty/active markers as their own slots.
fn status_segments(dirty: bool, active: bool) -> Vec<AnnotationSegment>;
/// Command latency class → its slot.
fn latency_segment(class: LatencyClass) -> AnnotationSegment;
```

These are **substrate helpers, not Document trait methods** — they are consumed only by specific picker sources, never by generic host machinery (the CLAUDE.md substrate-vs-helper rule). A source builds `Styled { category, segments }` from them.

### 9.3 Per-picker mapping

| Picker source | Today (flat `display`) | After (annotations; `display` becomes matchable text only) |
|---|---|---|
| `CommandsSource` | name │ args-hint │ doc │ latency-tag (4 hand-padded cols) | `display`=name; `Styled{args}`, `DocSnippet`(existing `doc` slot), `Styled{latency}`, **+ `Keybinding`** (slot exists, flat display omits it) |
| `BuffersSource` | `#id path [+] kind (current)` | `display`=path; `Styled{buffer-id}`, `Styled{status}` (dirty/active), `Kind`(existing slot) |
| `RecentFilesSource` | path only | `display`=path; reuse §8 `metadata_annotations` (perm/size/mtime) verbatim — free |
| `GrepSource` | `path:line:col  preview` | `display`=preview (matchable); `Styled{location}` via `location_segments` |
| `JumpsSource` | `[tag] buf:line:col` | `display`=buf label; `Styled{location}`; source-tag → categorical slot |
| `OutlineSource` | `lineno: name` | `display`=name; `Styled{location}` (line only) |
| `LinesSource` | `lineno: text` | `display`=text; `Styled{location}` (line only) |
| `MarksSource` | `'name line:col` | `display`=name; `Styled{location}` |
| `RegistersSource` | `"name preview` | `display`=preview; `Styled{register}` (name) |

`SnippetsSource` (`lattice-snippet`): `display`=**prefix** (the trigger the user types — the matcher scores `candidate.text` and `match_ranges` index it, so the shown run must stay aligned with the trigger), name→`Kind`, description→`DocSnippet`. `RegistersSource`: `display`=register contents, name (`"a`)→`Styled{register}`. `ThemePickerSource` is deferred (§9.6).

### 9.4 New theme slots

Under the existing `completion.annotation.*` namespace, registered in `registry.rs` with `BuiltinElementIds` fields + `annotation_slot()` arms (unknown slot still falls back to `…custom`, §8.3):

| Slot | Default (dark) | Used by |
|---|---|---|
| `completion.annotation.location.path` | overlay / dim | location path head |
| `completion.annotation.location.line` | yellow | line number |
| `completion.annotation.location.col` | overlay / dim | column number |
| `completion.annotation.status.dirty` | red | buffer `[+]` |
| `completion.annotation.status.active` | green | current-buffer marker |
| `completion.annotation.latency.reflex` | green | `[reflex]` commands |
| `completion.annotation.latency.display` | blue | `[display]` commands |
| `completion.annotation.latency.background` | orange | `[background]` commands |
| `completion.annotation.args` | subtext | command arg-hint |
| `completion.annotation.buffer-id` | overlay / dim | buffer `#id` |
| `completion.annotation.register` | purple | register / mark name |

`category_order` (§4) gains entries after the §8 file-metadata slots (perm=5, size=6, mtime=7): `location=8`, `status=9`, `latency=10`, `args=11`, `buffer-id=12`, `register=13`. Each picker uses a subset; the global order keeps mixed sets aligned.

### 9.5 Paramount-goal alignment (rollout)

| Goal | This rollout |
|---|---|
| #1 perf | Same O(visible × segments) resolution as §8. Sources already run off the UI thread (sync sources on the editor-actor thread, live sources on the LSP runtime); the segment build moves there, never onto the render thread. |
| #2 extensibility | Inherits §8 — every picker now speaks the same `(text, slot)` annotation contract; a future producer reuses the shared builders. |
| #3 grammar | Neutral. |
| #4 async | Neutral — no new threading; producers already spawn off-thread (Grep on the LSP runtime, the rest inline on the actor thread). |

### 9.6 Deferred — `ThemePickerSource` color swatch

A real color *swatch* for `:colorscheme` is the one item needing a primitive extension: the swatch color is per-candidate data (the theme's own accent), not a fixed theme slot — which conflicts with the §3 invariant "slot key, never a baked color." Deferred per heuristic #1 (don't add the primitive until a second consumer needs it). When wanted, it lands as its own design note (an explicit-color segment variant, or a swatch-by-theme-id resolver). The theme picker may still gain categorical metadata cells (dark/light variant) under existing slots in the meantime.

### 9.7 Tests + artefacts (rollout)

- `location_segments` / `status_segments` / `latency_segment`: each input maps to its slot; `None` path / zero col edge cases.
- Each migrated source produces the expected annotation set per row and keeps the matchable text in `display` (fuzzy match still works) — assert against annotations, not the old flat string.
- Both peers: a synthetic candidate for each new family resolves to distinct fgs; `:colorscheme` swap recolors; unknown slot → custom fallback (no panic).
- Bench: extend the §8 styled-cell bench with the location/status/latency families.

## 10. Open questions

- **Should `KeybindingAnnotator` run for non-command candidates too?** File-name candidates could surface `<C-x><C-o>` for open-in-split; option candidates could surface `:set spell`'s bind. Probably yes, but each surface needs its own reverse-cache key — file paths aren't `CommandId`s. Defer until past-v1 unless a clear use case emerges in the slice plan.

- **Annotation ordering when multiple apply.** Today the order is annotator-registration order. With typed annotations, the user might want fixed ordering (kind -> keybinding -> doc) for visual consistency. Likely lands as a sort step in the renderer keyed by a small `display_order` int per variant; punt to a follow-up slice if the default order suffices.

- **GPUI parity.** The current GPUI renderer doesn't paint the annotation column at all (per `completion-pipeline-unification.md` table — "only in cmdline; GPUI render gap"). The MARG slice plan opens with closing that gap because the user-visible result is "GPUI shows the same marginalia" — without it, half the renderer set is missing the feature. Tracked in MARG.3.

- **MRU influence on which binding to display.** For commands with N>1 bindings, "first bound chord" is a stable but arbitrary choice. An alternative: show the chord the user most recently invoked. Would require an MRU per command — heavier than v1 should carry. Stick with first-bound for v1.

## 11. Cross-references

- Slice sequencing + status (closed MARG.1–5): `docs/dev/archive/marginalia.md`
- Slice sequencing for §8 (rich per-segment marginalia): `docs/dev/operations/slice-plans/archive/marginalia-rich.md`
- Slice sequencing for §9 (picker rollout) + preview highlighting: `docs/dev/operations/slice-plans/archive/picker-marginalia.md`
- Sibling design — syntax-highlighted picker preview text (a separate surface from marginalia columns): `docs/dev/architecture/picker-preview-highlight.md`
- Substrate this rides on: `docs/dev/architecture/completion-pipeline-unification.md`
- Sibling consumer of the candidate-rendering pipeline: `docs/dev/architecture/insert-completion.md`
- Self-documenting help surface (the `:describe-key` / `:describe-command` cousin of this feature): `design.md` §5.11
- Rich minibuffer parent design: `design.md` §5.9.10
