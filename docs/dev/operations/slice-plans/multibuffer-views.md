# Multibuffer views — slice plan

Sequencing companion to
[`docs/dev/architecture/multibuffer-views.md`](../../architecture/multibuffer-views.md).
The design fragment is the source of truth for *what* and *why*;
this file owns *when* and *in what order*. Authoritative status
per slice lives in [`../implementation.md`](../implementation.md).

Each slice ships green-on-merge with the four artefacts
CLAUDE.md mandates: architecture documentation (the design
fragment, updated as needed), benchmark coverage where
load-bearing, test coverage of the new scenarios + failure
modes, graceful error handling.

| Slice   | Title                                       | What lands |
|---------|---------------------------------------------|------------|
| **M.0** | Document-as-trait refactor                  | Hoist `Document` to a trait; port today's struct to `RopeDocument`; keep every call site green. The refactor's correctness gate is the existing test suite passing unchanged, plus a bench-no-regression on `editor_render_p99_us` and `edit_dispatch_p99_us` (no new abstraction overhead). No multibuffer code yet. Reviewed independently and merged as its own PR before M.1 starts. |
| **M.1** | `MultibufferDocument` (read-only)           | New `MultibufferDocument` impl of the `Document` trait. Excerpts as `Vec<Excerpt>` with anchored ranges; composed `rope()` view backed by lazy read-through; row-translation cache built on first access, invalidated on source-edit events. Registered in `BufferRegistry`. **Edits are rejected** (read-only mutability) in this slice. Tests: create multibuffer with 3 excerpts across 2 source buffers; `rope().lines()` returns the expected composed content; source-buffer edits propagate to the composed view; closing a source buffer auto-removes orphaned excerpts (per §8 decision). No rendering yet — assertions are via direct Document reads. |
| **M.2** | Excerpt rendering                           | Consumes D.0's virtual-row primitive (if D.0 hasn't landed yet, this slice lands it). Excerpt headers + separators render as virtual rows. `]e` / `[e` / `]E` / `[E` motions registered. Tests: open a multibuffer in a pane, render correctly with headers and separators; motions land on expected rows. Bench: `multibuffer_render_p99_us` ≤ 200µs at 50 visible excerpts. |
| **M.3** | Edit propagation                            | Flip multibuffer to editable. Edit dispatch at multibuffer row → translation lookup → source dispatch → standard pipeline. Boundary clipping per §4. Multi-excerpt selections split into per-excerpt edits in source-ascending order. Undo composes correctly. Tests: edit within an excerpt → source buffer reflects; edit at excerpt boundary clips; multi-excerpt selection delete fires one undo group spanning N source buffers; macros recorded against a multibuffer replay correctly. Bench: `multibuffer_edit_dispatch_p99_us` ≤ 100µs at 1k excerpts. |
| **M.4** | Live updates from source buffers            | Source-buffer edits propagate to the multibuffer view. Anchor-driven excerpt range tracking (existing anchor type handles this). Translation rebuild debounced and run off-thread. Cross-pane consistency: edit in source pane reflects in multibuffer pane on next snapshot. Tests: edit source buffer outside any excerpt — multibuffer unchanged; edit source buffer inside an excerpt — multibuffer's composed view reflects; rapid edits coalesce into one rebuild. Bench: `multibuffer_source_edit_p99_us` ≤ 200µs at 1k excerpts. |
| **M.5** | Expand-context affordance                   | `:multibuffer-expand [n]` / `:multibuffer-contract [n]` ex-commands and the bound keys (`+` / `-` on excerpt header). Translates to anchor-range mutation on the relevant excerpt; translation rebuild fires through the standard path. Tests: expand grows the excerpt; contract shrinks; expand below 1 row is a no-op; expand past the source buffer's end clips. |
| **M.6** | `MultibufferProvider` trait + first consumer | The provider trait + the `MultibufferSubsystem` that owns provider tasks. **First consumer lands in the same slice**: `SearchProvider` — wraps ripgrep, emits initial excerpts from match locations, observes search-query changes and re-emits excerpts. `:search-buffer <pattern>` ex-command opens the result in a multibuffer. Tests: provider lifecycle (create, mutate, close); ripgrep-driven excerpts populate correctly; query change replaces excerpts; search-buffer responds to subscribed event mutations. Bench: `multibuffer_bulk_replace_p99_us` ≤ 5000µs at 1k excerpt replacement. |
| **M.7** | Excerpt fold provider                       | `ExcerptFoldProvider` registers one fold range per excerpt's composed-row range into the existing fold registry. **No new keymaps** — the standard `z*` vocabulary (`za` / `zo` / `zc` / `zR` / `zM`, `foldlevel=N`, `:foldopen` / `:foldclose`) covers excerpts identically to syntactic / marker folds (§6.5). `za` on a row inside an excerpt collapses to the excerpt's M.2 header virtual row. Composition: when a hunk (D.3.f) sits inside an excerpt, the smallest enclosing fold wins on `za`; repeated presses walk outwards. Lands after M.4 (live-updates) so fold ranges stay current as source edits shift anchors. Tests: excerpt fold range matches the composed-row span; `za` toggles without affecting other folds; nested hunk-inside-excerpt prefers innermost on `za`; expand-context (M.5) widens the fold range. |
| **M.8** | File-boundary fold provider                 | `FileBoundaryFoldProvider` registers one fold range per distinct `source: BufferId` covering the union of that file's excerpts. **No new keymaps** — same vocabulary as M.7. Surfaces "collapse all hunks/excerpts in this file to one summary row" as a natural `za` action on the file-header row. Essential for project-wide diff (A.1) + AI multi-file diff (A.2) where a user reviewing 50 files wants a top-down outline. Composition: file > excerpt > hunk nesting on `za`. Lands after M.6 so provider-driven excerpt sets are stable before fold ranges union across them. Tests: file fold range covers excerpts contiguously; `za` toggle collapses all excerpts of one file; multi-file multibuffer with 100 files × 5 excerpts collapses into a 100-row outline at `:set foldlevel=0`. |

Slice sequencing:

- **M.0 is the load-bearing slice** — the trait refactor.
  Lands green standalone; everything else depends on it.
- **M.1** depends on M.0.
- **M.2** depends on M.1 + D.0 (consumes the virtual-row
  primitive; lands D.0 if D.0 hasn't already shipped).
- **M.3** depends on M.2 (need rendering to test edit
  visibility) + M.1.
- **M.4** depends on M.3 (need editable multibuffer to test
  cross-pane edit visibility).
- **M.5** depends on M.4.
- **M.6** depends on M.4 (provider needs editable +
  live-updating multibuffer).
- **M.7** depends on M.4 (fold ranges shift with anchor
  updates; live-update must be stable first).
- **M.8** depends on M.6 (provider-driven excerpt sets must
  be stable before fold ranges union across them).

## N.1 — Narrow mode (follow-on, depends on M.3)

| Slice   | Title       | What lands |
|---------|-------------|------------|
| **N.1** | Narrow mode | `NarrowProvider` (one excerpt with `ExcerptEdgeMode::Strict`); `:narrow` over the active visual region; `:narrow-to-defun` (tree-sitter scope at point); `:narrow-to-paragraph`; `:widen` sugar over `:bd`. Tests: narrow over a line range renders only that range; edits propagate to the source; widen restores; partial-line edges render exactly `[start.col, end.col]`; stacked narrow (narrow within a narrow's output buffer) edits propagate two hops to the real source; two panes narrowed to different ranges of the same source stay live-synced through M.4. No bench gate (composed primitive — perf is covered by M.3 / M.4 gates). |

`N.1` depends on **M.3** (editable multibuffer + edit
propagation) — that's the load-bearing prerequisite for
"edits in the narrowed view save to the original buffer."
Multiple parallel narrows and stacked narrow additionally
rely on **M.4** (live source-edit propagation) for cross-pane
consistency; lands once M.4 is green.

Full design notes live in the design fragment's §10
"Follow-on consumers" appendix (A.5).
