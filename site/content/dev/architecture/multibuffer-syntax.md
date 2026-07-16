+++
title = "Multibuffer Per-Excerpt Syntax Highlighting (K.4.7)"
+++

# Multibuffer Per-Excerpt Syntax Highlighting (K.4.7)

**Status:** implemented (2026-06-08)  
**Slice plan:** `docs/dev/operations/slice-plans/archive/multibuffer-is-a-regular-buffer.md`, slice K.4.7

## Problem

A multibuffer view composes N source files into one virtual document. The composed
snapshot carries the view's name (`*search:foo*`) — so `Lang::detect_from_path`
always returns `Lang::Plain`, and every row renders unstyled regardless of source
language. The highlight path the cells worker uses (a single `SyntaxHandle` per
pane) cannot express "row 0–4 are Rust, row 5–9 are Markdown".

## Options considered

**Option B — one-shot, per-pane full-file re-parse.** Build a scratch `Syntax` from
the entire composed text on each pane publish. Rejected: O(file) parse per render
cycle violates paramount-goal #1 (no O(file) work in the hot path); also loses the
incremental-reparse benefit because the composed text changes identity whenever any
source edits.

**Option C — register source docs in `BufferRegistry`.** The source docs in a
multibuffer are only in `MultibufferState.sources`, not in `BufferRegistry`, so no
existing `SyntaxHandle` exists for them. Adding them to the registry would pollute
`:ls`, `:bn`, `:bp`, and mode-activation with sources the user never opened
directly — violating the "everything is a buffer" contract.

## Chosen design: Option A — per-source long-lived `SyntaxHandle`

Each source document in `MultibufferState.sources` that has a detectable language
gets its own `SyntaxHandle`. Handles are created eagerly:

- **At `add_source` time** if `lang_registry` is already wired.
- **At `set_lang_registry` time** retroactively for sources already in state (the
  common production path: `new(sources, …)` is called first, then the host calls
  `set_lang_registry` after `create_multibuffer_view`).

`SyntaxHandle::seeded(syntax)` is used so the constructor degrades gracefully in
synchronous test contexts (initial parse snapshot kept; incremental-reparse worker
absent but not required for the test assertion). In production, `Handle::try_current()`
inside `seeded` succeeds and the worker runs.

### Data model

```rust
// MultibufferState (lattice-multibuffer/src/lib.rs)
source_syntax: HashMap<BufferId, Arc<SyntaxHandle>>

// MultibufferInner
lang_registry: OnceLock<Arc<LangRegistry>>
```

### Cells-side contract

`dispatch.rs::build_cells_panes` reads the multibuffer registry per pane:

```rust
let excerpt_syntax: Arc<[ExcerptSyntax]> = services
    .get::<MultibufferRegistryHandle>()
    .and_then(|r| r.handle(buffer_id))
    .map(|mb| mb.excerpt_syntax_entries()
        .into_iter()
        .map(|(cs, ce, ss, h)| ExcerptSyntax { composed_start: cs,
                                               composed_end: ce,
                                               source_start: ss,
                                               handle: h })
        .collect::<Vec<_>>().into_boxed_slice().into())
    .unwrap_or_else(|| Arc::from([]));
```

The `syntax` axis of `MatrixVersion` folds in all excerpt handle versions (XOR) so
a per-source background reparse invalidates the cells cache and triggers a rebuild.

### Worker highlight path

`cells_worker::highlight_range_multibuffer` assembles per-excerpt spans:

```
fn highlight_range_multibuffer(
    excerpt_syntax: &[ExcerptSyntax],
    lo: u32,           // composed row, inclusive
    hi: u32,           // composed row, exclusive
) -> Option<Vec<Vec<StyledSpan>>>
```

Returns `None` when `excerpt_syntax` is empty (single-document pane falls through
to the existing `syntax_handle` path). For each excerpt that overlaps `[lo, hi)`,
translates composed-row coordinates to source-row coordinates, calls
`snap.highlight_lines(src_lo, src_hi)`, and writes spans into the result indexed
relative to `lo`. Rows not covered by any excerpt keep an empty span list
(plain-text fallback — correct, because separator/header virtual rows carry no
source content).

`recompute_pane`'s `highlight_range` closure checks `excerpt_syntax` first:

```
if let Some(spans) = highlight_range_multibuffer(&pane.excerpt_syntax, lo, hi) {
    return Some(spans);
}
// single-document path ...
```

### `ExcerptSyntax` struct (render_state.rs)

```
pub struct ExcerptSyntax {
    pub composed_start: u32,   // inclusive, composed-space
    pub composed_end:   u32,   // inclusive, composed-space
    pub source_start:   u32,   // first source row mapped to composed_start
    pub handle:         Arc<SyntaxHandle>,
}
```

Non-multibuffer panes carry `excerpt_syntax: Arc::from([])` (zero-cost empty slice).

## Paramount-goal alignment

| Goal | Impact |
|------|--------|
| #1 Performance | O(viewport) highlight remains: `highlight_range_multibuffer` clips per-excerpt to `[lo, hi)`. No O(file) work on the hot path. |
| #2 Extensibility | Provider-contributed sources (search, LSP references) automatically get highlights when the host passes `lang_registry`. No per-provider code. |
| #3 Modal editing | Unchanged. |
| #4 Asynchronicity | SyntaxHandle workers run on the tokio runtime; `snapshot()` is wait-free ArcSwap read. |

## UX contract

Per the `feedback_decorations_update_in_place` standing rule: rows keep their
existing colours during an async reparse (the stale snapshot is still coloured);
only the edited row loses colour momentarily if its excerpt's version falls behind.
This is the same behaviour as single-document panes — no new regression.
