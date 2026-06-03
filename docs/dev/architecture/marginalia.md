# Marginalia — typed annotations for completion candidates

> **Status (2026-06-03):** proposed; no code yet. Implementation lands as the `MARG.*` slice series — see slice plan at `docs/dev/operations/slice-plans/marginalia.md`. Cross-references existing fragments: `completion-pipeline-unification.md` (the substrate this rides on), `insert-completion.md` (sibling consumer of the candidate-rendering pipeline).

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

## 8. Open questions

- **Should `KeybindingAnnotator` run for non-command candidates too?** File-name candidates could surface `<C-x><C-o>` for open-in-split; option candidates could surface `:set spell`'s bind. Probably yes, but each surface needs its own reverse-cache key — file paths aren't `CommandId`s. Defer until past-v1 unless a clear use case emerges in the slice plan.

- **Annotation ordering when multiple apply.** Today the order is annotator-registration order. With typed annotations, the user might want fixed ordering (kind -> keybinding -> doc) for visual consistency. Likely lands as a sort step in the renderer keyed by a small `display_order` int per variant; punt to a follow-up slice if the default order suffices.

- **GPUI parity.** The current GPUI renderer doesn't paint the annotation column at all (per `completion-pipeline-unification.md` table — "only in cmdline; GPUI render gap"). The MARG slice plan opens with closing that gap because the user-visible result is "GPUI shows the same marginalia" — without it, half the renderer set is missing the feature. Tracked in MARG.3.

- **MRU influence on which binding to display.** For commands with N>1 bindings, "first bound chord" is a stable but arbitrary choice. An alternative: show the chord the user most recently invoked. Would require an MRU per command — heavier than v1 should carry. Stick with first-bound for v1.

## 9. Cross-references

- Slice sequencing + status: `docs/dev/operations/slice-plans/marginalia.md`
- Substrate this rides on: `docs/dev/architecture/completion-pipeline-unification.md`
- Sibling consumer of the candidate-rendering pipeline: `docs/dev/architecture/insert-completion.md`
- Self-documenting help surface (the `:describe-key` / `:describe-command` cousin of this feature): `design.md` §5.11
- Rich minibuffer parent design: `design.md` §5.9.10
