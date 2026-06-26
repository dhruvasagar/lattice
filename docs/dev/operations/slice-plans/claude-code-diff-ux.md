# Claude Code openDiff — review-UX overhaul (D-fix series) — slice plan

Sequencing + status for the `:claude` (Claude Code IDE peer) interactive
**openDiff** review UX. **Design contracts** for the diff substrate live in
[`../../architecture/diff-system.md`](../../architecture/diff-system.md) and
[`../../architecture/fold-architecture.md`](../../architecture/fold-architecture.md);
the IDE-peer protocol lives in the (archived) `ide-protocol` plan. This file
owns *when* and *in what order* for the dogfooding fixes Dhruva hit using
`:claude` to review agent-proposed edits.

Status legend: ✅ done · 🟡 partial · 🚧 in progress · 🗒 planned.

**Branch:** `claude-code-diff-ux` (off `main`, independent of the L7 LSP work
on `l7-lsp-nav-mode-ownership`).

**Parked alongside:** the `:diagnostics-buffer` scope WIP (`DiagnosticsListScope`
+ `Effect::ListDiagnostics { scope }`) is stashed (`git stash`, message
"diagnostics-buffer-wip…") to be resumed after the diff UX lands. The stash is
based on the L7 branch (its `effect.rs` hunk sits next to L7's `LspRequest`), so
resume it on a branch that includes L7 (or after L7 merges) to avoid an
`effect.rs` context conflict.

---

## The locked UX (Option A — persistent-panel convention)

The convention shared by VS Code (Copilot/Claude Code), Cursor, and Zed: the
agent panel is **persistent**, and the proposed edit shows as a diff that
**closes on accept/reject**, returning focus to the agent. Mapped to Lattice
(everything-is-a-buffer, panes-via-splits): the `:claude` terminal pane stays
put; the diff opens in fresh splits to its right (`claude | baseline | proposed`);
on resolve the diff panes close and focus returns to claude.

Confirmed with Dhruva 2026-06-25 (Option A over inline-unified-diff (B); B
deferred behind a future `claude.diff-style` option if 3 panes feel cramped on
narrow terminals).

---

## Slices

### D-fix.1 — keep claude, close-on-resolve, refocus  ✅
**Landed (`dff6971f`).** `open_programmatic_diff` records the originating
(claude) pane and opens baseline + proposed in two fresh vsplits to its right
(claude untouched). New `ProgrammaticDiffPanes { origin_pane, diff_panes,
diff_buffers }` + the `programmatic_diff_panes` map (keyed by session primary).
`finish_programmatic_diff_panes` (called from `do_diff_accept` / `do_diff_reject`
after the outcome fires + save lands) closes the diff panes, refocuses the origin
pane (re-syncing the active document so keystrokes — incl. `<Esc>` — reach the
PTY again), and drops the throwaway in-memory buffers. Registration-failure
rollback + manual-close (bdelete) cleanup both de-leak the map.
*Tests:* accept/reject assert the 3-pane layout (claude preserved at slot 0),
close-to-1 + refocus on resolve, throwaway buffers removed; close-tab asserts no
map leak. Host-only (no renderer change).

### D-fix.2 — syntax-highlight both panes  ✅
**Landed (`b1724955`).** The diff buffers were bare in-memory documents with no
language. New `install_inmemory_syntax` mirrors `do_edit`'s syntax install per
buffer (baseline language detected from the OLD file's path — it has no registry
path; proposed from the new path), seeding the buffer's `DocumentSyntax` local
BEFORE activation so `activate_buffer` promotes it (active pane) and
`document_syntax_for` reads it (inactive pane). Host-only — the renderers already
read per-pane syntax (the `:diffsplit` path). Best-effort: no grammar →
`DocumentSyntax(None)`, no panic.
*Test:* `open_programmatic_diff_highlights_both_panes` (standard lang registry →
both panes resolve a handle).

### D-fix.3 — full in-buffer diff highlighting (both panes, theme-based)  🚧
**Scope (Dhruva, 2026-06-26):** NOT just the gutter column — proper in-buffer
diff highlighting (line-background tints) on BOTH panes, colours from the theme.
Confirmed at runtime: D-fix.1/.2 work; the diff shows **no** tints or signs.

**Converged root cause (static trace, no runtime needed):** the entire in-buffer
diff decoration is a SINGLE, active-doc, **current-side** sign map:
- `RenderState.diff.sign_map` = `Editor::diff_signs_for_active()` = the *active*
  buffer's session `sign_map()` (one map, dispatch.rs).
- Row tints: TUI `diff_tint_bg` (render.rs:5128) and GPUI `diff_tint_per_row`
  (window.rs:1639) read that one `rs.diff.sign_map` for EVERY pane (no per-pane
  key) → the baseline pane would tint with the proposed buffer's line numbers.
- Gutter signs: `DiffDecorationData` registered **active-pane-only** (render.rs:3656).
- The `DiffSignMap` itself is computed **current-side only** (`ranges[1]`,
  overlay.rs:161) — there is NO baseline-side map. `Remove` hunks emit no
  current-side sign (deletions surface via filler rows).

So: the left/baseline pane can NEVER show its side; the right/proposed pane only
tints when that one map is populated AND woken. The user seeing *nothing* means
the proposed session's `sign_map` isn't reaching the frame (the async overlay
refresh lands without a paint-wake — the L1/L6 class) and/or the map is empty.

**Plan (5 parts; theme colours retained — `theme.diff_*_line_bg` / GPUI
`resolved_theme`):**
1. **lattice-diff:** compute + publish a **baseline-side** sign map (from
   `ranges[0]`: Remove→red, Change→changed) alongside the current-side map in
   the overlay refresh task; expose `DiffSession::baseline_sign_map()`. Owned by
   the diff subsystem.
2. **render state:** replace the single `diff.sign_map` with a per-buffer map
   `BufferId → Arc<DiffSignMap>` built from `iter_sessions()` (proposed buffer →
   current-side map, baseline buffer → baseline-side map). Keep the active hunk
   count for the modeline.
3. **renderers (TUI + GPUI, lockstep):** row tints + gutter both read the
   per-pane map by the pane's `buffer_id`; `DiffDecorationData` registered for
   EVERY diff pane (drop the active-only gate). Colours stay theme-driven.
4. **compute wake:** the overlay refresh fires a `paint_request` when it lands
   so tints appear off-keystroke (close the L1/L6 gap for the diff axis).
5. **tests + parity:** baseline-side sign geometry test (lattice-diff); per-pane
   render test (both panes tint from their side); grep both renderers for the
   per-pane read. Intra-line/word-level diff (vimdiff `DiffText`) does NOT exist
   today — a separate future stretch, out of D-fix.3 scope.
**Deps:** D-fix.1/.2. Touches lattice-diff (public API) + render-state +
both renderers → cross-crate; walk the approach before bulk edits.

### D-fix.4 — interrupt ergonomics  🗒
`:claude` is a PTY terminal; focusing its pane and pressing `<Esc>` already
forwards `\x1b` to the CLI (native interrupt). D-fix.1 made focus return to
claude on resolve, so this works today. **Optional polish:** a `:claude-interrupt`
ex-command that sends `\x1b` to the claude terminal from any pane (so you don't
have to switch first). Land only if Dhruva wants it.

### D-fix.5 — fold unchanged regions + jump to first change  🗒 LOCKED (C)
**Goal (Dhruva, 2026-06-26):** in a diff, show only the changes — fold the
unchanged code — and auto-scroll to the very first change on open.

**Locked (2026-06-26): option (C)** — a `diff-mode`-owned `UnchangedFoldSource`
applied to ALL diff sessions (vimdiff-style universal `foldmethod=diff`), gated
by `ui.diff.fold-unchanged` (default **on**) + `ui.diff.context` (default **6**,
vimdiff's default), plus auto-scroll to the first change. Rationale (heuristic #1
+ convention-first): the complement fold source belongs in `diff-mode` beside the
existing `HunkFoldSource` — fixing the shared substrate once, not special-casing
the openDiff path (rejected (B) openDiff-only as a half-measure).

**Mode-ownership (Dhruva, 2026-06-26): these are DIFF-SUBSYSTEM changes — owned
by `diff-mode` / `lattice-diff`, not the host** (`feedback_mode_owns_its_surface`):
- `UnchangedFoldSource` → `lattice-diff` (beside `HunkFoldSource`); registered in
  `diff-mode::on_activate` via `FoldOverlayService`, dropped by `DiffModeGuard`.
- `ui.diff.fold-unchanged` + `ui.diff.context` → registered by the diff
  subsystem's `install` (NOT host core options).
- Auto-scroll trigger → diff-mode-driven (it knows the first hunk via the
  session's `HunkIndex`). The host exposes only the generic cursor/scroll
  primitive the trigger calls — the *decision* (scroll to first hunk, which pane)
  is diff-mode's. Acid test: zero new host `Action`/`Effect`/`Editor::do_*`
  bound to this; the diff subsystem contributes the source + options + the
  scroll-target, the host runs the generic fold-compute + cursor-move it already
  has.

**Convention (lead):** vimdiff `foldmethod=diff` folds unchanged lines
automatically (`diffopt context:N`, default 6, keeps N lines around each change)
and positions the cursor at the first diff. VS Code's diff editor "Collapse
Unchanged Regions" (`diffEditor.hideUnchangedRegions.enabled`, default on;
`contextLineCount` default 3, `minimumLineCount` gate) + reveals the first
change. GitHub/GitLab/Zed all collapse unchanged regions with expanders. Strong,
universal convention.

**Key finding:** the existing `HunkFoldSource` (lattice-diff/fold.rs) folds the
**hunks themselves** (collapse a change), the *opposite* of this. D-fix.5 needs a
COMPLEMENT source that folds the unchanged gaps between hunks (minus a context
window), closed-by-default. The two coexist (disjoint regions).

**Shape (locked):**
- New `UnchangedFoldSource` (lattice-diff) — complement of (hunks ± context),
  `closed: true`; registered in `diff-mode::on_activate` via `FoldOverlayService`
  exactly as `HunkFoldSource` is, so ALL diff sessions get it.
- Option-gated: `ui.diff.fold-unchanged` (bool, default on) +
  `ui.diff.context` (uint, default 6 — vimdiff's default), registered by the diff
  subsystem's `install`. A `minimum-gap` guard so tiny gaps aren't folded (VS
  Code's `minimumLineCount`).
- Auto-scroll: diff-mode-driven from the first hunk's current-side line via the
  generic host cursor-move + `do_scroll_cursor_to(Center)` (vim `zz`); for
  openDiff, the proposed (right) pane.
**Artefacts:** design fragment update (diff-system.md / fold-architecture.md),
fold-source test (complement geometry + context), open-positions-at-first-hunk
test, bench n/a (fold compute is O(hunks)), graceful: 0 hunks → no folds, no
scroll.
**Deps:** D-fix.1/.2 (the openDiff buffers); benefits from D-fix.3 first (folding
unchanged code is most useful once the visible changes are tinted, and both hinge
on `diff-mode` actually activating on the openDiff buffers — D-fix.3 candidate (c)).

---

## Cross-references
- Diff substrate: `diff-system.md`, `fold-architecture.md`.
- IDE peer: `ide-protocol` (archived slice plan) + `[[project_ide_protocol_status]]`.
- Shared render-wake / paint-request: `incremental-highlight.md`, `display-line.md`
  (the L1/L6 machinery D-fix.3 must respect).
