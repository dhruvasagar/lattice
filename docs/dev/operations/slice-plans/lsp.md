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
