# LSP — slice plan

Sequencing + status for the LSP polish work. **Design contracts** live
in [`../../architecture/lsp-architecture.md`](../../architecture/lsp-architecture.md)
§12–§15; the **per-method capability matrix** (LSP 3.17 coverage, every
method's ✅/🚧 status) lives in
[`../../notes/lsp-features.md`](../../notes/lsp-features.md). This file
owns *when* and *in what order*; those own *what* and *why*.

Status legend: ✅ done · 🚧 in progress · 🗒 planned.

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

### L1 — async-result render-wake  🗒
**Design:** lsp-architecture.md §12.
**Problem:** direct-write request tasks and event-bus arrivals never
fire `async_landed`, so semantic-token colour, inlay hints, and
`$/progress` are invisible until the next keystroke runs
`run_tick_pending` + publish.
**Change:**
- Fire `async_landed.notify_one()` from each direct-write request task
  *after* its `insert_for`: `maybe_request_semantic_tokens`
  (`dispatch.rs:~9513`, the reported symptom) first, then
  `maybe_request_inlay_hint`, `_folding_range`, `_code_lens`,
  `_document_color`, `_document_link`, `_document_highlight`,
  `_pull_diagnostics`.
- Route the render-relevant LSP event subscriptions (progress,
  diagnostics/inlay/semantic/code-lens refresh, log) through a
  forwarder that fires `async_landed` on delivery.
- Templates (no LSP task fires it today): the tree-sitter syntax seed
  closure `dispatch.rs:8986-8994`, and the `MultibufferExcerptsReady`
  forwarder `editor_boot.rs:~940`.
**Artefacts:**
- *test* — headless proof that a semantic-token cache write and a
  `$/progress` event each produce a published render state + a
  `paint_request`, with NO keystroke (assert via the cells worker's
  `WorkerDecision` + the paint `Notify`).
- *responsiveness* — `current_thread` runtime coverage (the actor's
  config): the forwarder/task lands the wake without a foreground
  poll.
- *doc* — §12 (done).
- *error handling* — a closed/dropped channel logs (`debug!`) and exits
  the forwarder; never panics, never swallows silently.
**Deps:** none. **Unblocks:** L2/L3 (progress + readiness visibility),
L4 (diagnostic repaint).

### L2 — server lifecycle state  🗒
**Design:** §13.
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

### L3 — status surfaces  🗒
**Design:** §14.
**Change:** collapse the two LSP status-line segments (`LspMode` badge
+ `LspProgressMode` percentage) into one state-driven segment in
`LspMode::status_line_items` (state glyph + id + %); enrich
`help_views::lsp_status_help` with the lifecycle line + active
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

### L4 — diagnostics inline summary + cursor popup  🗒
**Design:** §15. Owned by `lsp-diagnostics-mode`.
**Change:**
- Inline eol summary via the inlay-hint virtual-text span substrate;
  cursor-line scope, ~300 ms idle gate, Insert-mode suppressed; options
  `ui.diagnostics.inline` (`off`/`cursor-line`/`all`) +
  `ui.diagnostics.inline-min-severity`.
- `gl` → `CursorAnchored` popup with full per-line diagnostics
  (severity glyph / message / `source` / `code` / related count),
  reusing the hover popup pipeline. Keymap at
  `KeymapLayer::MinorMode(lsp-diagnostics)`, handler body in the mode's
  crate — no host `Action` variant, no `Editor::do_*`.
- `]d` / `[d` echo the landed diagnostic's message.
**Artefacts:**
- *test* — summary text + cursor-line gating + Insert suppression;
  popup contents for a multi-diagnostic line; option toggles
  (`off`/`cursor-line`).
- *doc* — §15 (done) + `user/lsp.md`.
- *parity* — eol virtual text + popup render in both peers.
- *error handling* — no-diagnostic line → no summary; `gl` on a clean
  line echoes "no diagnostics on line".
**Deps:** L1 (repaint), L3 (mode-surface patterns).

### L5 — inline `all`-lines diagnostics (opt-in)  🗒
**Design:** §15 (`ui.diagnostics.inline = "all"`).
**Change:** extend L4's eol summary to all viewport lines under the
option; O(viewport) fan-out only, never O(file).
**Artefacts:** *test* — viewport-bounded fan-out; *bench* — eol-summary
build stays flat at 100k lines; *doc*; *error handling*.
**Deps:** L4. **Optional** — land only on request.

---

## Cross-references

- Design contracts: `lsp-architecture.md` §12–§15.
- Per-method matrix: `lsp-features.md`.
- Shared render-wake + cells/overlay worker: `incremental-highlight.md`,
  `display-line.md` (the `paint_request` + `WorkerDecision` machinery
  L1 asserts against).
- Mode ownership: `mode-architecture.md` (L4's keymap + handler
  placement).
