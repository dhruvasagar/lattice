# LSP — slice plan

Sequencing + status for the LSP polish work. **Design contracts** live
in [`../../architecture/lsp-architecture.md`](../../architecture/lsp-architecture.md)
§12–§15; the **per-method capability matrix** (LSP 3.17 coverage, every
method's ✅/🚧 status) lives in
[`../../notes/lsp-features.md`](../../notes/lsp-features.md). This file
owns *when* and *in what order*; those own *what* and *why*.

Status legend: ✅ done · 🟡 partial · 🚧 in progress · 🗒 planned.

---

## Completed (historical)

Phases 4.1–4.5 landed the wire layer, navigation, edits, decorations,
and workspace ops — see the per-method matrix in `lsp-features.md` for
exact status. This slice plan starts from the Phase-4-polish gaps the
matrix's ✅ rows hide.

> **Caveat on the matrix.** `$/progress`, `textDocument/semanticTokens`,
> and `textDocument/inlayHint` are marked ✅ in `lsp-features.md` (the
> wire + cache + render code all exist), but are functionally invisible
> until **L1** lands — they update buffer state without firing the
> render-wake, so they never repaint off-keystroke. The matrix tracks
> wire/cache coverage; L1 tracks whether the result reaches a frame.

---

## Active slices

### L1 — async-result render-wake  ✅
**Design:** lsp-architecture.md §12. Carved into **L1a** (semantic
tokens), **L1b** (inlay hints), **L1c** (progress/refresh-event
forwarders), and the **L1-tail** (the remaining direct-write request
tasks). All landed.

- **L1a** (`d8661aff`) — `maybe_request_semantic_tokens` fires
  `async_landed.notify_one()` after each cache write (data + authoritative
  empty); the `Err`/cancelled arm keeps the prior overlay (no double
  publish, `feedback_decorations_update_in_place`).
- **L1b** (`b4d20334`) — same for `maybe_request_inlay_hint`.
- **L1c** (`cc232d31`) — render-relevant LSP **events** (inlay /
  semantic / diagnostic / code-lens `*/refresh`) routed through
  `wake_on` forwarders in `editor_boot.rs` (fire `async_landed` on
  delivery); `$/progress` + `serverStatus` reach the screen via the
  `lattice_lsp::modeline` forwarder (ML.3c) instead of their own wake.
- **L1-tail** (2026-06-21) — `maybe_request_{folding_range,
  document_link, code_lens, document_color, document_highlight}` fire
  the wake on their **data-landed** arm only (`Ok(Some) if !empty`), and
  `maybe_request_pull_diagnostics` on a `Full` outcome (not `Unchanged`
  / `Err`-collapsed `Empty`). **Decision (Option X):** wake only the
  data branch, leaving the `_ =>` empty/cancel write untouched — a
  superseded request can't flicker the decoration, and for the
  cursor-scoped `document_highlight` keeping prior on cancel would be
  *wrong* (stale highlights at the old position). The Err-keep-prior +
  immediate-clear split (as L1a/b have it) is a separate
  decoration-stability concern, not bundled here.

**Artefacts:** *test* — the §12 wake mechanism is covered by the
existing off-keystroke push-path test (`dispatch.rs:~27833`); the
per-task `notify_one` is the identical one-liner L1a/L1b shipped
**without** a per-task test (the spawned task can't fire without a mock
server). *doc* — §12; this reconciliation. *error handling* — wake only
on real writes; closed forwarder channels exit cleanly (L1c). **Deps:**
none. **Unblocked:** L2/L3 (landed), L4 (diagnostic repaint).

### L2 — server lifecycle state  ✅
**Landed:** **L2a** (`ed55eb30`) receives rust-analyzer
`experimental/serverStatus`; **L2b** (`6824cffc`) renders the persistent
✓/⟳/✗ readiness from it. The full `LspServerState` enum below is the
original design; the shipped form is the readiness projection L2a/b
needed — revisit the richer state machine only if L3/L4 require it.

**Design (original):** §13.
**Change:** add `LspServerState` (`Spawning` / `Initializing` /
`Ready` / `Indexing { title, pct }` / `Failed { reason }`) to the
supervisor, keyed by `(workspace, server_id)`, published via an
`ArcSwap` snapshot; emit `LspServerStateChanged` on each transition
(fires the §12 wake). `Indexing` projects the existing `$/progress`
accumulator; falls back to `Ready` when the active token ends. Expose
the Ready/indexed edge for re-request triggers.
**Artefacts:**
- *test* — transitions spawn → init → ready → indexing → ready, and
  → failed on handshake error; `Indexing` tracks the highest-priority
  token's percentage.
- *doc* — §13 (done).
- *bench* — n/a (O(1) state flips).
- *error handling* — handshake / pipe failure → `Failed { reason }`,
  logged `info!` (user-actionable).
**Deps:** L1 (so transitions repaint).

### L3 — status surfaces  🟡 partial (L3-lite ✅; rest re-scoped)
**Design:** §14.
**Landed:** **L3-lite** (`f8e9fca7`) — explicit ready/indexing glyph in
the LSP status segment.

> **⚠ design stale.** The original change below — "collapse the LSP
> status segments into `LspMode::status_line_items`" — is **obsolete**:
> ML.3d **retired** `Mode::status_line_items`, and ML.3c already made the
> LSP badge a registered **modeline element** (`lattice_lsp::modeline`,
> PaneLocal Right/5, fed by the relocated `LspProgressStore`). So the
> "collapse into the trait" work cannot happen and is unneeded — the
> single state-driven segment now lives in the modeline element system.
> Remaining L3 scope, re-cast: enrich `:lsp-status` help with the
> lifecycle line + active progress, and live-refresh it. Re-confirm
> against the modeline path before picking this up.

**Change (original, partly obsolete):** collapse the two LSP status-line
segments (`LspMode` badge + `LspProgressMode` percentage) into one
state-driven segment in `LspMode::status_line_items` (state glyph + id +
%); enrich `help_views::lsp_status_help` with the lifecycle line + active
progress; make `:lsp-status` live-refresh via the log-buffer tail
wiring. Glyphs degrade per the icon-palette rule.
**Artefacts:**
- *test* — segment text per state; `:lsp-status` reflects a transition
  without reopen.
- *doc* — §14 (done) + `user/lsp.md` status note.
- *parity* — TUI + GPUI status render in lockstep; end-of-slice grep
  for any new status glyph in `lattice-ui-gpui`.
- *error handling* — missing status service → empty segment, no panic
  (ServiceRegistry `Arc`/`TypeId` lookup must match registration).
**Deps:** L2.

### L4 — diagnostics inline summary + cursor popup  🚧
**Design:** §15. Carved into **L4a** (inline eol summary, host-plumbed
decoration — sibling to inlay-hints/gutter/underline) and **L4b** (`gl`
popup + `]d`/`[d`, mode-owned because they add chords + handlers).

#### L4a — inline end-of-line summary
The summary is a *passive decoration* in the same family as inlay
hints, the gutter severity column, and the inline underline — all
host-plumbed today (cache/state on `Editor` → `RenderState` → renderer
reads). It adds **no chord, no `Action` variant, no `Editor::do_*`**, so
the mode-ownership acid test is satisfied while the timing plumbing
stays with the host that owns `cursor` / `modal` / the actor loop. The
"what to show" formatter lives in `lattice-lsp` next to the layer.

- **L4a.1** (`bb66f33b`) ✅ — `ui.diagnostics.inline`
  (`off`/`cursor-line`/`all`) + `ui.diagnostics.inline-min-severity`
  (`error`/`warning`/`info`/`hint`) typed options under a new
  `Diagnostics` group; value types + `rank()` matching the
  `diagnostics_layer` `severity_rank`. Self-contained.
- **L4a.2** (2026-06-21) ✅ — host-side cursor-line summary compute +
  idle gate. (1) `DiagnosticsLayer::inline_line_summary(uri, line,
  min_rank) -> Option<InlineDiagnosticSummary>` in `lattice-lsp` (most-
  severe qualifying message, first line, truncated + ` +N`). (2) Idle
  gate on `Editor` (`inline_diag_{line,deadline,visible}`) recomputed in
  `update_inline_diag_gate` from `publish_render_state`: `off`/Insert/
  Replace disarm; a new cursor line arms a 300 ms deadline; the armed
  line stays put on in-place edits. (3) A pinned-sleep `select!` arm in
  the editor actor (mirroring the LSP actor's `flush_sleep`) fires the
  gate on idle → republish + cells wake. (4) `build_render_state`
  publishes `DiagnosticsRenderState::inline_summary = Some((line,
  summary))` only while visible. *Tests:* 5 formatter
  (`lattice-lsp::diagnostics_layer`) + 4 gate-transition
  (`lattice-host::render_state`). No renderer touch yet → no parity
  obligation this slice.
- **L4a.3** (2026-06-21) ✅ — render the published `inline_summary` as
  trailing eol virtual text, severity-themed by `severity_rank` +
  italic, active pane only. **TUI:** `splice_virtual_text_into_spans`
  at `map_ob(line_len)` in the compose loop (sibling to the inlay
  splice). **GPUI:** a per-frame `ShapedLine` overlay painted at
  `text_origin_x + shaped_text[row].width` — **decision (B):** NOT
  spliced into the cells grid (which would churn the
  decoration-retention cell cache on every cursor move, re-breaking
  `project_decoration_retention_status`); the summary is cursor-
  transient interaction state, so it lives with the cursor/underline
  per-frame overlays. Both peers gate the render on lsp-diagnostics-
  mode (TUI `view.lsp_diagnostics_enabled`; GPUI the active buffer's
  `active_minor_modes`) so it tracks the gutter/underline visibility —
  the host publishes unconditionally, the renderers gate. *Tests:* 2
  TUI compose (`render::tests` — renders on cursor line, suppressed
  when the mode is off). *Parity:* grep for `inline_diag_summary` in
  `lattice-ui-gpui` is non-empty. *Doc:* `user/lsp.md` (four surfaces).
  *Known seam:* GPUI's pre-existing diagnostic **underline** is NOT
  mode-gated (a separate parity gap, untouched here).

#### L4b — cursor popup + jump echo  (2026-06-21) ✅
**Owned by `lsp-diagnostics-mode`**, promoted from the `lsp_sub_mode!`
marker to a full `Mode`. "Mode-owned as much as possible" (Dhruva):
- **Keymap** (`LspDiagnosticsMode::keymap()`, scoped by K.1.c): `gl` →
  `action:lsp-diagnostic-popup`; `]d` / `[d` → the **existing**
  `ex:diag-next` / `ex:diag-prev` ex-commands (the mode owns the
  *binding*; reusing the shared jump rather than duplicating it).
- **Handler** (`LspDiagnosticsMode::action_handlers()` closure, in
  `lattice-lsp`): reads the cursor line's diagnostics via a new
  `DiagnosticsQuery` service (`DiagnosticsQueryHandle`; host impl
  `HostDiagnosticsQuery` resolves `buffer_id → uri → layer` over the
  published render state), formats them (`format_diagnostic_popup_lines`
  — glyph / message / `source:code` / `+N related`), and returns the
  new `Effect::ShowDiagnosticsPopup { lines }`.
- **Host boundary:** `Effect::ShowDiagnosticsPopup` renders through the
  hover popup pipeline (`HelpContent` → `DisplayBufferRequest`,
  `CursorAnchored`); empty → echoes "no diagnostics on line". `]d`/`[d`
  echo is added to the shared `do_next/prev_diagnostic` (so `:cnext` /
  `:diag-next` echo too). **No host `Action` enum variant, no
  `Editor::do_*` bound to a chord** — `action:lsp-diagnostic-popup` is a
  command-name registration with a dead `Effect::None` apply (the
  `ActionHandlerRegistry` closure intercepts), exactly the
  `snippet-expand` pattern.
- **Effect blast radius:** the new variant was threaded through both
  renderers' exhaustive effect classifiers (host-handled no-op list +
  the mutate/yank classifiers) per `feedback_tui_gpui_parity`.

*Tests:* 5 `lattice-lsp` (keymap owns the 3 chords; only the popup
handler is contributed; formatter glyph/source/code/related/empty) + 1
`lattice-ui-tui` (jump echoes the landed message). *doc:* §15 +
`user/lsp.md` (four surfaces + the `gl`/`]d`/`[d` table). *parity:* the
popup reuses the hover `DisplayBuffer` path → renders in both peers with
no peer-specific code. **Deps:** L1 (repaint), L3 (mode-surface patterns).

### L5 — inline `all`-lines diagnostics (opt-in)  🗒
**Design:** §15 (`ui.diagnostics.inline = "all"`).
**Change:** extend L4's eol summary to all viewport lines under the
option; O(viewport) fan-out only, never O(file).
**Artefacts:** *test* — viewport-bounded fan-out; *bench* — eol-summary
build stays flat at 100k lines; *doc*; *error handling*.
**Deps:** L4. **Optional** — land only on request.

### Diagnostics polish (landed 2026-06-21, post-L4b)

Dogfooding fixes on the L4 surfaces — all committed:

- **Preview centring** (`92dcbc90`) — `gr` / references / grep /
  `:picker lines` previews landed the match at the viewport BOTTOM.
  `preview_accept_action`'s `JumpInBuffer` / `JumpToFileLocation` arms
  (`dispatch.rs`, the generic picker-preview engine — NOT lsp-specific)
  now call `do_scroll_cursor_to(ScrollPos::Center)` after moving the
  cursor (vim `zz`).
- **Inline summary eol positioning** (`efda8692`) — the summary landed
  mid-line. GPUI now uses the cell matrix's painted `col_count()` ×
  advance (the cursor's EOL x) for unwrapped rows + `rposition` for the
  last wrap segment; TUI splices at `usize::MAX` (append after inlays).
- **Diagnostics repaint without a cursor move** (`efda8692`) — pushed
  `publishDiagnostics` (incl. the CLEAR when an error is fixed) updated
  `DiagnosticsLayer` but never woke the render loop (only pull-mode
  `*/refresh` had a `wake_on` forwarder). `DiagnosticsLayer::apply` now
  fires a render-wake `Notify` (`set_wake`, wired to `async_landed` in
  `editor_boot.rs`). **This is the same no-wake class as L6 below.**
- **`gl` popup severity colours** (`520bc051`) — 4 `Style::Diagnostic*`
  variants → `syntax_element_id` → the gutter/underline severity
  elements; the mode-owned `gl` handler bakes a whole-line severity
  highlight per popup line.

### L6 — off-keystroke paint gate: a publish must request a frame  ✅

**Design:** lsp-architecture.md §12 ("The second gap").

**Original framing (disproved).** L6 was first spec'd as a "server-ready
render-wake" — the theory that the first `K`/nav after a server goes
ready no-ops because the readiness edge doesn't fire `async_landed`.
Tracing the code disproved every link of that hypothesis: the mode gate
(`active_modes`) is set **synchronously** by the cascade's sync prefix
(`registry.rs` `activate_minor` / `record_implies_cascade`); the URI is
registered **eagerly** at open; `servers_for(uri)` only ever returns
**post-`initialize`** servers (`actor::spawn` awaits the handshake); and
the hover/nav result already drains off-keystroke in **both** peers via
the X1b `paint_request` → `run_tick_pending` bridge. The `K` bug was
already fixed; firing a wake on the ready edge would have been a no-op
patch on a wrong premise.

**The real bug (what Dhruva hit dogfooding).** The `lsp ⟳ … %` / `lsp ✓`
progress badge — and any diagnostics **overlay** change (the
"undo → diagnostic disappears" report) — updated `RenderState` but did
**not** repaint until a keystroke. Root cause: `async_landed` lands the
data in the snapshot, but the only off-keystroke repaint triggers are the
cells / virtual-rows workers, which fire `paint_request` *only* on a
content-changing `WorkerDecision`. A surface those workers don't own
(modeline badge, diagnostics overlay, popup, minibuffer) never requested
a frame. A paramount-#4/#1 violation, not the gate/readiness story.

**Fix (Option A — single change-detection point).**
- `build_render_state` stamps `RenderState::paint_revision`
  (`Editor::compute_paint_revision`): a content hash over every
  render-visible surface the cells / virtual-rows workers do NOT own
  (the overlay set in `lattice-cells/src/version.rs` + modeline /
  minibuffer / popup / echo / tabs / lifecycle chrome). Identity-
  preserving sub-states fold by `Arc` pointer; the rest by value, so a
  no-op publish yields a **stable** revision.
- `publish_render_state` returns whether the revision moved; the actor's
  **off-keystroke** arms (`async_landed`, inline-diag timer) fire
  `paint_request` when it did. The keystroke path is untouched (it
  already paints via the input wake) — this dodges a per-keystroke extra
  `build_render_state` through the GPUI paint bridge, and the revision
  gate stops that bridge's `run_tick_pending` → re-publish from spinning.
- `DiagnosticsLayer::snapshot_revision()` feeds the diagnostics axis
  (each `apply` swaps the snapshot `Arc`), so a pushed diagnostics change
  (the undo case) repaints off-keystroke.
- `ModelineService::update` / `clear` are now **equivalence-gated** — an
  unconditional `rcu` re-stored a fresh content `Arc` every publish
  (`sync_diff_modeline_element` re-applies the diff element each build),
  which both churned allocations and broke the content pointer as a
  change signal. Now the pointer means "the modeline actually changed".

**Artefacts:** *code* — `compute_paint_revision` + `paint_revision` field
+ bool-returning `publish_render_state` + the two actor arms +
`snapshot_revision` + the modeline equivalence-gate. *tests* —
`publish_render_state_paint_gate_tracks_non_cell_changes` +
`publish_render_state_suppressed_during_batch_returns_false`
(`lattice-host`), `snapshot_revision_changes_on_apply_stable_otherwise`
(`lattice-lsp`). *doc* — §12 "The second gap". *perf* — O(1) hash per
publish; keystroke path unchanged (`keystroke_publish_ratchet` green).
*parity* — host-side only; both renderers consume `paint_request`
identically (no peer-specific code). **Deps:** L1/L2.

### L7 — LSP nav surface → full mode-ownership  🗒

**Goal:** finish the half-migration. `gd` / `gD` / `gy` / `gI` / `gr` /
`gx` / `K` have their KEYMAP in the mode (`lsp_mode_keymap_entries()`,
`modes.rs:455`, `cmd: "action:lsp-*"`) but their HANDLERS are host
ActionIds in `actions.rs` (`ids.lsp_hover_request`, `lsp_definition_request`,
… ~266/452/1267). L4b made `gl`/`]d`/`[d` the FIRST fully mode-owned LSP
surface; this slice brings the nav surface to the same standard
(`feedback_mode_owns_its_surface`).

**The complication (why this needs design, not a mechanical move):**
unlike `gl` (a synchronous read of the local diagnostics layer), the nav
actions fire ASYNC LSP requests (hover → popup, definition → jump,
references → picker). The L4b shape (handler closure returns an `Effect`
synchronously) doesn't map 1:1. Open questions to resolve first:
- What is the right Effect/handler shape for an async request? Options:
  (a) handler returns an `Effect::Lsp<Request>` the host spawns +
  drains (the request substrate `maybe_request_*` / `drain_pending_*`
  stays host — it IS legitimate shared substrate, like the cells
  worker); (b) richer mode-side async via the M-async Guard machinery.
  Decide which keeps the acid test (zero `Editor::do_*` bound to a
  chord, zero new host `Action` variants) without duplicating the async
  request plumbing.
- The existing `actions.rs` `lsp_*_request` ActionIds: do they become
  command-name-only registrations with dead applies (the `snippet-expand`
  / `lsp-diagnostic-popup` pattern), with the real body moving to
  `LspMode::action_handlers()` closures? Or do they stay as the
  host-substrate request triggers? This is the crux.
- Reuse from L4b: the `DiagnosticsQuery`-style service pattern for any
  data the closures need (cursor/buffer/uri via the render-state
  snapshot); the Effect-classifier threading discipline.

**Files:** `lattice-lsp/src/modes.rs` (add `LspMode::action_handlers()`);
`lattice-host/src/actions.rs` (the `lsp_*_request` ActionIds);
`dispatch.rs` (the async request + drain substrate); both renderers'
Effect classifiers if new Effects are added.

**Acid test:** a future provider crate adds an LSP-ish chord with ZERO
`Editor::` additions in `lattice-host` and ZERO new host `Action`
variants. **Deps:** L4b (the pattern). **Confirm the Effect/handler
shape with Dhruva before coding** — present the (a)/(b) options mapped
to the heuristics.

---

## Cross-references

- Design contracts: `lsp-architecture.md` §12–§15.
- Per-method matrix: `lsp-features.md`.
- Shared render-wake + cells/overlay worker: `incremental-highlight.md`,
  `display-line.md` (the `paint_request` + `WorkerDecision` machinery
  L1 asserts against).
- Mode ownership: `mode-architecture.md` (L4's keymap + handler
  placement).
