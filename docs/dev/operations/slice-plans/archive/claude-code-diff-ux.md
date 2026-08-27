# Claude Code openDiff — review-UX overhaul (D-fix series) — slice plan

> **✅ ARCHIVED COMPLETE (2026-06-26).** All slices D-fix.1–D-fix.6 landed.
> Design contracts live on in `diff-system.md` (§6.5.1 unchanged-region
> folding) + `fold-architecture.md`; the IDE-peer status is in the
> `project_ide_protocol_status` memory. Kept for the slice history + the
> execution-time corrections (D-fix.5 both-sides, D-fix.6 session-scoping).

Sequencing + status for the `:claude` (Claude Code IDE peer) interactive
**openDiff** review UX. **Design contracts** for the diff substrate live in
[`../../../../architecture/diff-system.md`](../../../../architecture/diff-system.md) and
[`../../../../architecture/fold-architecture.md`](../../../../architecture/fold-architecture.md);
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

### D-fix.3 — full in-buffer diff highlighting (both panes, theme-based)  ✅
**Landed:** 3a (`b2cf708d`) — publish the sign map for EVERY session at the
`recompute_blocking` choke point (was inline-only). 3b (`b4e32042`) — both-pane,
theme-based: baseline-side sign map (`compute_baseline_diff_sign_map`) +
`diff.remove.line` theme element (reuses `diff.deletion.bg`, all 21 themes
covered) + per-buffer `RenderState.diff.sign_maps` read per-pane by both
renderers (drop the active-doc-only gate, removed lines tint red) + a wake
forwarder for pane-group recomputes (off-keystroke). Tests: recompute publishes
sign map; baseline-side sign geometry. Green: 239 diff + 569 host + 53 theme;
GPUI `--features window` (parity). Intra-line/word diff stays a future stretch.

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

### D-fix.4 — `:claude-interrupt` (forward `<Esc>` to the CLI)  ✅
**Landed (`a4abf0d9`).** **Correction (Dhruva, runtime):** pressing `<Esc>` in
the claude terminal is consumed by the terminal's modal layer (Insert→Normal —
the desired vim flow), so it NEVER reaches the PTY. There was therefore no way
to interrupt the running agent — `:claude-interrupt` is **required**, not polish.
New host-owned `Effect::TerminalInput(Vec<u8>)` → `do_terminal_input` (active
pane's terminal PTY); `:claude-interrupt` emits `TerminalInput([0x1b])`.
Renderer parity (both no-op classifier lists). Flow: in the claude terminal,
`<Esc>` → Normal, then `:claude-interrupt` forwards the interrupt. Test:
`claude_interrupt_emits_esc_terminal_input`. Green: 58 claude-code + 569 host +
1490 TUI; GPUI `--features window`.

### D-fix.5 — fold unchanged regions + jump to first change  ✅ (C, both-sides)
**Landed (2026-06-26): 5a `2f4a8182`, 5b/c `307d42c1`, 5d/e `afc1502b`.**

**Goal (Dhruva, 2026-06-26):** in a diff, show only the changes — fold the
unchanged code — and auto-scroll to the very first change on open.

**Execution-time correction (both-sides, confirmed 2026-06-26):** the locked
"register exactly as `HunkFoldSource`" wording was a *matches-pattern*
justification and hid a latent UX bug — `HunkFoldSource` registers on the
session's PRIMARY only (`lookup`) and folds `ranges[1]` (current side); it's
invisible today because hunk folds are open-by-default. A `closed`
unchanged-fold on one side only would desync the scroll-bound side-by-side
panes (`HunkRowMapper` aligns rows). Convention-first (vimdiff/VSCode fold BOTH
sides) + UX-higher-court → register on EVERY participant, each folding its own
slot. Dhruva chose this over primary-only / defer.

**Shipped:**
- **5a** — `DiffSession.slot_line_counts` (published at the `recompute_blocking`
  choke point); `DiffSubsystem::participant_slot` (the buffer's `ranges` slot,
  resolved from either side via the secondary index) + `first_change_line`.
- **5b/c** — `UnchangedFoldSource` (slot-aware complement of hunks ± context,
  `closed`; pure `compute_unchanged_folds` geometry + `>=2`-line floor =
  VS Code `minimumLineCount`; reads `ui.diff.*` live). `HunkFoldSource` made
  slot-aware. Options `ui.diff.fold-unchanged`=on + `ui.diff.context`=6 (Display
  group, `lattice_config::options!` in lattice-diff). `ConfigRegistry::get_int_by_name`.
  `diff-mode::on_activate` registers BOTH sources for EVERY participant
  (`lookup_session_for` + `participant_slot`, default slot 1 for sources-less
  sessions); `DiffModeGuard::drop` removes both.
- **5d** — `Editor::recompute_folds_for_buffer` (the missing substrate: fold an
  INACTIVE buffer + stash `DocumentFolds`) + `refresh_diff_folds` in
  `run_tick_pending` (revision-gated via `diff_fold_seen_revisions`) so folds
  appear off-keystroke when the async recompute publishes.
- **5e** — `open_programmatic_diff` computes the diff synchronously (for
  `first_change_line`) + auto-scrolls the proposed (active) pane to the first
  change via the generic cursor-move + `do_scroll_cursor_to(Center)` — zero new
  Action/Effect/`do_*` (acid test held).

**Renderer parity:** none needed — folds flow through the shared per-pane
`DocumentFolds` → cells worker → `DisplayMatrix` fold-elision pipeline; both TUI
+ GPUI project the already-elided matrix (GPUI `editor_element` "doesn't reach
into fold caches at paint time"). D-fix.5 touches no effect classifier / sign
render / match arm.

**Tests:** lattice-diff — slot-count publish, participant-slot both sides,
first-change per side, per-side hunk fold, complement geometry
(leading/trailing/merge/min-gap/empty-range-anchor), toggle-off, default-on.
host — auto-scroll positions the proposed cursor; `recompute_folds_for_buffer`
stashes closed folds on an inactive baseline. Green: 251 lattice-diff, 47 host
diff tests.

_Original locked spec (C) retained below for the design rationale._

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

### D-fix.6 — CLI-side reject/close tears down the diff  ✅ (session-scoped)
**Landed (2026-06-26).**

**Execution-time redesign (Dhruva, 2026-06-26): scope by connection, not
`tab_name`.** Two corrections to the original plan:
1. **Multi-session isolation (hard requirement).** Multiple agent sessions (and
   other AI sessions) can each have openDiffs in flight. A close from session A
   must tear down ONLY A's diffs, never B's. The original "match by tab_name"
   can't scope `closeAllDiffTabs` and risks cross-session interference.
2. **Don't key on presentation.** A diff might show as a tab today, a
   window/split/inline tomorrow — keying teardown on `tab_name` is a shape
   assumption that rots. The **connection id IS the association**: a close from
   a connection tears down the diff session(s) that connection opened,
   regardless of where/how they're displayed.

**Shipped:**
- **conn_id plumbing** — the server assigns a unique `conn_id` per WS connection
  (sequential counter; `0` reserved for the shared/boot context + non-IDE
  producers), stamped into a per-connection `DispatchContext` clone
  (`serve_connection`). `openDiff` tags its `ProgrammaticDiffRequest.origin_session`
  with it; the host stores it on `ProgrammaticDiffPanes.origin_session`.
- **session-scoped teardown** — `Editor::do_close_session_diffs(origin_session)`
  rejects every programmatic diff whose `origin_session` matches (fires each
  bound oneshot `Reject` → the agent's blocked `openDiff` returns
  `DIFF_REJECTED`, then `finish_programmatic_diff_panes`). `0` and unknown
  connections match nothing (no stray sweep). Presentation-agnostic.
- **two host effects** — `Effect::CloseSessionDiffs { origin_session, tab_name }`
  (`close_tab`: reject the connection's diffs, else legacy active-buffer
  file-close via `tab_name` — the ONLY remaining `tab_name` use) +
  `Effect::CloseAllSessionDiffs { origin_session }` (`closeAllDiffTabs`:
  diff-only). Both renderer effect-classifiers (TUI + GPUI) carry the new
  host-applied no-op variants (parity, the `TerminalInput` precedent).
- **inbound + tool** — `InboundKind::CloseTab` gains `origin_session`; new
  `CloseAllDiffTabs`; the `closeAllDiffTabs` MCP tool registered + routed
  (`writes::close_all_diff_tabs`). The diff-vs-buffer decision moved host-side
  (only the host knows the open diffs + their `origin_session`).

**Tests:** the isolation guarantee (two connections; closing conn 1 rejects only
its diff, conn 2's session + oneshot intact); the `0`/unknown-connection guard;
inbound mapping (`close_tab`→`CloseSessionDiffs`, `closeAllDiffTabs`→
`CloseAllSessionDiffs`); the tool catalog. Green: 64 claude-code, 573 host lib;
all four Effect-consuming crates (host, TUI, GPUI, claude-code) compile.

**Provisional wire-shape (UNVERIFIED — validate live):** I could not confirm
against a live CLI whether "no" sends `close_tab` (and with which `tab_name`) or
`closeAllDiffTabs`. BOTH are handled defensively, and both reduce to "reject this
connection's diffs," so the exact message + label don't matter for correctness;
worst case (a message we don't handle) is a graceful no-op. Confirm the real
frames when dogfooding.

_Original symptom / root-cause / plan retained below._

**Symptom (Dhruva, runtime):** saying "no" in the claude CLI does NOT behave
like `:diff-reject` — the diff buffers don't close. Editor→agent works
(`:diff-accept`/`:diff-reject` fire the bound oneshot → `FILE_SAVED`/
`DIFF_REJECTED`); the **agent→editor** direction is the gap.

**Root cause:** the CLI's close path arrives as `close_tab { tab_name }`, but
`inbound.rs` only acts when `tab_name == the active buffer's file PATH` — and
openDiff's `tab_name` is the label `"openDiff"`, not a path → never matches →
nothing happens. Even if it matched it emits a generic `BufferDelete`, not the
programmatic-diff teardown (fire `DiffOutcome::Reject` on the oneshot +
`finish_programmatic_diff_panes` to close both panes + refocus claude). The real
CLI may also send `closeAllDiffTabs` (unhandled → `METHOD_NOT_FOUND`, dropped).

**Plan:** match an open programmatic diff by its session/tab (not active-path);
route the CLI close → reject the bound oneshot + `finish_programmatic_diff_panes`;
handle `closeAllDiffTabs` the same way. Owned by `lattice-claude-code` (the
inbound bus) + the host teardown primitive. **Provisional wire-shape:** confirm
what the live CLI actually sends on "no" (`close_tab` vs `closeAllDiffTabs`) —
the same validation item flagged in the ide-protocol status.
**Deps:** D-fix.1 (`finish_programmatic_diff_panes`).

---

## Cross-references
- Diff substrate: `diff-system.md`, `fold-architecture.md`.
- IDE peer: `ide-protocol` (archived slice plan) + `[[project_ide_protocol_status]]`.
- Shared render-wake / paint-request: `incremental-highlight.md`, `display-line.md`
  (the L1/L6 machinery D-fix.3 must respect).
