# Terminal-as-Document — slice plan

Sequencing companion to
[`docs/dev/architecture/terminal-as-document.md`](../../../../architecture/terminal-as-document.md).
The design fragment is the source of truth for *what* and *why*;
this file owns *when* and *in what order*. Authoritative status
per slice lives in [`../implementation.md`](../../implementation.md).

> **Status: ✅ feature-complete (verified 2026-06-17).** T-snap-1 ·
> T-mode-1 · T-grammar-1 (narrow) · T-cursor-1 · T-search-1 · T-marks-1 ·
> T-paint-1 · T-rich-1 all landed (`feat(terminal): T-…` commits) — the
> terminal IS a `Document` buffer: motions, search, marks, text objects,
> and visual all route through the `SyntheticDoc` rope. **T-clean-1** is
> done for its DEAD-code scope (the bespoke *motion* grammar —
> `do_terminal_nav_*` / `TerminalWordDir` / `terminal_word_step` /
> `TermCharClass` — was retired in Phase B.2). The remaining bespoke
> pieces (`do_terminal_enter_visual` + `t.visual` grid-coord selection +
> the published `terminal_nav_cursor`) are **live, not dead** — terminal
> visual selection still uses its own grid-coord substrate. Removing them
> = the **deferred "aggressive Visual flip"** (route terminal visual
> through the `SyntheticDoc` document selection, touching both renderers),
> tracked as a follow-up — see T-grammar-1 (aggressive, deferred).

Each slice ships green-on-merge with the four artifacts CLAUDE.md
mandates: design fragment (updated as needed), tests, bench
(where load-bearing), graceful error handling.

| Slice              | Title                                              | What lands |
|--------------------|----------------------------------------------------|------------|
| **T-doc-1**        | Design fragment + cross-refs                       | The design document; `terminal-mode.md` §5.1 / §5.3 updated to point at it; `design.md` §5.9.8 line for terminal updated to say "Document-on-Normal, owned by `TerminalNormalMode`"; `mode-architecture.md` reference to the two terminal minor modes. No code change. |
| **T-snap-1**       | `SyntheticDoc` construction + tests + bench        | `SharedTerm::build_normal_snapshot() -> SyntheticDoc`. Trailing-blank strip + trailing-newline drop (vim last-line convention). Cursor clamps to trimmed row length. Wide/narrow char handling deferred (§7 open question). Unit tests for: empty grid, ascii, trailing-blank strip, cursor-translate, cursor-clamp-to-trim, scrollback-included, alt-screen-excludes-scrollback, frozen_at, doc↔grid round-trip. Bench `term_snapshot_build` at 10k × 200 + 50k × 400; baseline ~8.7 ms p99 at 10k × 200 (CI gate 10 ms). Pure data layer — no mode hook yet. |
| **T-mode-1**       | Register `TerminalNormalMode` minor mode           | New minor mode with on-enter / on-exit / active-text resolver / transition-only keymap (`i`/`a`/`I`/`A`). On-enter calls `with_terminal_mut(|t| t.synthetic = Some(Arc::new(build_normal_snapshot(t))))`. On-exit drops the Arc + snap-to-live-edge. `TerminalInsertMode`'s on-enter is updated to drop `t.synthetic` symmetrically. Add the `synthetic: Option<Arc<SyntheticDoc>>` field to `TerminalBuffer`. Tests: mode activation builds the doc; deactivation drops it; round-trip preserves no state. |
| **T-paint-1**      | Mode-scoped coord adapter for visual overlays      | `TerminalNormalMode` registers a publish-time hook that remaps `visual_range` / `visual_block_extents` from doc coords to cell-grid coords. Auto-installs on activation, auto-uninstalls on deactivation. Renderers unchanged. Tests: blockwise visual on terminal paints same rectangle as today's bespoke path. |
| **T-grammar-1**    | Central grammar via mode-resolved dispatch         | Two sub-slices: **(narrow, 2026-05-28)** `Editor::active_text()` returns the SyntheticDoc rope when the active buffer is a Terminal with `synthetic.is_some()`; the seam is in place so downstream consumers (T-paint-1, T-search-1, T-marks-1) can read the rope uniformly. The bespoke `run_terminal_invocation` motion arms still drive cursor changes today. **(aggressive, deferred)** Flip the dispatch path so motions / text objects / visual route through `run_document_invocation` against the SyntheticDoc; cursor stored in doc-space; regression-lock tests (w/W/b/B/e/E + h/j/k/l + gg/G land same result as the bespoke implementation) prove equivalence before T-clean-1 deletes the bespoke arms. The aggressive flip requires re-plumbing `self.cursor` semantics for terminal panes — that's a multi-slice arc on its own. |
| **T-search-1**     | `/`, `?`, `n`, `N`, `*`, `#`, `hlsearch` against SyntheticDoc | Drop `SharedTerm::find_match` / `find_all_matches` from the dispatcher; the central search machinery operates on the rope when the mode is active. Tests: search hits in scrollback land at correct doc coords; `hlsearch` paints correctly. |
| **T-clean-1**      | Delete the parallel grammar                        | Remove `nav_cursor` + `t.visual` from `TerminalBuffer`; remove `terminal_visual_active` from the host publish; remove `do_terminal_nav_*`, `TerminalWordDir`, `terminal_word_step`, motion arms in `run_terminal_invocation`, and `do_terminal_enter_visual`. The shims `do_terminal_enter_insert` / `do_terminal_exit_insert` collapse into thin "activate `TerminalInsertMode`" / "activate `TerminalNormalMode`" calls and may inline into the mode-activation path. Net code deletion. Tests stay green. |
| **T-marks-1**      | Marks + jumplist coord unification                 | Local marks (`m{a-z}` / `'{a-z}` / `` `{a-z} ``) operate on SyntheticDoc via central grammar (no mode-specific work). Jumplist `PositionEntry` for terminal entries carries doc coords + `frozen_at` seq; re-entry rebuilds snapshot (via mode reactivation) and resolves position best-effort. Tests: mark survives motion; mark across Insert → Normal round-trip resolves; mark across scrollback roll falls back to nearest line. |
| **T-rich-1** (opt) | Text objects + advanced motions                    | Verify `iw`, `aw`, `i"`, `a"`, `i(`, `a(`, `%`, sentence / paragraph motions, `f`/`F`/`t`/`T` work against SyntheticDoc. Mostly verification + tests; the central implementations already exist. |

Slices are sequenced so each one ships green:

- **T-snap-1** is pure construction + bench; no mode hook yet.
- **T-mode-1** adds the mode + lifecycle but does not yet route
  the grammar through it (motions still flow through
  `run_terminal_invocation`). The mode is active; the
  SyntheticDoc exists; the central grammar isn't using it.
- **T-grammar-1** flips the active-text resolver so the central
  grammar starts using the SyntheticDoc; the bespoke arms in
  `run_terminal_invocation` remain in place as a fallback for
  one slice.
- **T-search-1** moves search across.
- **T-clean-1** deletes the bespoke arms + parallel state once
  both paths are live and the regression locks prove
  equivalence.

**T-paint-1** can land in parallel with **T-grammar-1** /
**T-search-1** — it only depends on T-mode-1.
