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

### D-fix.3 — diff red/green hunk highlighting  🗒
**Symptom:** the openDiff panes show no added/removed tints or gutter signs.
**Not** missing computation — `register_pane_group_diff` wires `BufferSource` per
pane → `register_with_sources` (hunk compute) → diff-mode + overlay/filler
providers. So it is a rendering/activation gap. Candidates (need a runtime trace
to disambiguate):
- **(a)** the per-render `DiffDecorationData` service isn't populated for these
  panes;
- **(b)** the async hunk compute lands without an off-keystroke paint-wake (the
  L1/L6 class — `paint_request` only fires on a content-changing cells decision);
- **(c)** `diff-mode` doesn't actually activate on the in-memory buffers (it
  gates BOTH gutter signs and row tints — start here).
**Plan:** run the GPUI peer, trigger one openDiff, observe → pick the cause →
fix at the activation/decoration seam (NOT a renderer kind-gate). Ship the
fix with a test + TUI/GPUI parity if any renderer arm changes.

### D-fix.4 — interrupt ergonomics  🗒
`:claude` is a PTY terminal; focusing its pane and pressing `<Esc>` already
forwards `\x1b` to the CLI (native interrupt). D-fix.1 made focus return to
claude on resolve, so this works today. **Optional polish:** a `:claude-interrupt`
ex-command that sends `\x1b` to the claude terminal from any pane (so you don't
have to switch first). Land only if Dhruva wants it.

### D-fix.5 — fold unchanged regions + jump to first change  🗒 (proposed)
**Goal (Dhruva, 2026-06-26):** in a diff, show only the changes — fold the
unchanged code — and auto-scroll to the very first change on open.

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

**Proposed shape (confirm before coding):**
- New `UnchangedFoldSource` (lattice-diff) — complement of (hunks ± context),
  `closed: true`; mode-owned by `diff-mode` (so ALL diff sessions get it, like
  vimdiff's universal `foldmethod=diff`), registered via `FoldOverlayService`
  exactly as `HunkFoldSource` is.
- Option-gated: `ui.diff.fold-unchanged` (bool, default on) +
  `ui.diff.context` (uint, default 6 — vimdiff's default). A `minimum-gap` guard
  so tiny gaps aren't folded (VS Code's `minimumLineCount`).
- Auto-scroll: on diff open, move the cursor to the first hunk's current-side
  line + `do_scroll_cursor_to(Center)` (vim `zz`); for openDiff, in the proposed
  (right) pane.
**Artefacts:** design fragment update (diff-system.md / fold-architecture.md),
fold-source test (complement geometry + context), open-positions-at-first-hunk
test, bench n/a (fold compute is O(hunks)), graceful: 0 hunks → no folds, no
scroll.
**Deps:** D-fix.1/.2 (the openDiff buffers). **Open question for Dhruva:**
all-diffs (diff-mode-owned, vimdiff-style) vs openDiff-only.

---

## Cross-references
- Diff substrate: `diff-system.md`, `fold-architecture.md`.
- IDE peer: `ide-protocol` (archived slice plan) + `[[project_ide_protocol_status]]`.
- Shared render-wake / paint-request: `incremental-highlight.md`, `display-line.md`
  (the L1/L6 machinery D-fix.3 must respect).
