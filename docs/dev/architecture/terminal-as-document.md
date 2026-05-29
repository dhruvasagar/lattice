# Terminal Buffer as a Document (Normal/Visual sub-states)

Authoritative design for collapsing the parallel "scrollback
grammar" that grew under `BufferKind::Terminal` into the unified
buffer model. The terminal's Normal-mode view becomes a
synthetic, read-only Document; the central vim grammar operates
on it the same as on any other buffer. The Terminal-Insert
sub-state stays unchanged (PTY-bound).

This document is a *companion* to `terminal-mode.md` (T-series
plan, snapshot mechanics, PTY lifecycle) and to `design.md`
(§5.1 buffer model, §5.2 modal engine, §5.9.8 buffers-as-views).
It does not supersede them — it tightens §5 of `terminal-mode.md`
("Modal sub-states") with the explicit Document semantics that
the recent T3 / T-async slices left implicit.

## 1. The friction this resolves

`terminal-mode.md` §5.1 already states: *"Vim grammar applies to
the scrollback view (NOT the PTY)."* The implementation does
not deliver this. Today, Normal-in-terminal runs a **parallel
grammar** on bespoke state:

- `nav_cursor: Option<(i32, u16)>` on `TerminalBuffer` instead
  of the document's cursor
- `do_terminal_nav_move` / `do_terminal_nav_goto` /
  `do_terminal_nav_word` re-implementing the motions the central
  grammar already provides
- `t.visual: Option<TerminalVisualState>` parallel to the
  document's `SelectionSet`
- `run_terminal_invocation` matching each command id and routing
  to a bespoke helper instead of letting the central dispatcher
  resolve it

The consequence: every new motion / text object / mark / search
feature added to the grammar has to be **re-implemented for
terminals** or it silently degrades. The terminal-side word
motions (w/W/b/B/e/E) shipped 2026-05-27 are the latest example.
`$`, `^`, `0`, `f`/`F`/`t`/`T`, `iw`, `aw`, `i"`, marks, `%`,
sentence / paragraph motions, `g_`, `gj`/`gk` — none of those
work in a terminal buffer, and the only way to land them on the
current trajectory is more parallel implementations.

Two saved invariants are in tension with the status quo:

> Everything is a buffer. -- `CLAUDE.md`, `design.md` §5.9.8

> Buffers must not have kind-specific logic — never branch
> render / motion / scroll on `BufferKind`. -- `feedback_buffers_
> no_special_case.md` (saved feedback)

The terminal is doing exactly this branching, and the
maintenance cost is unbounded. The fix is to honour the
invariant.

## 2. Vision

The terminal buffer has **two views** of its content, selected
by sub-state:

- **Terminal-Insert** — PTY-bound. Keystrokes encode to ANSI →
  `PtyHandle::write`. The alacritty grid is "live"; the
  renderer paints it directly. No Document layer is involved.
  This view does not change.
- **Normal-in-terminal / Terminal-Visual** — Document-bound. A
  synthetic, read-only `Document` is built from the alacritty
  scrollback + visible grid at the moment of Insert → Normal
  transition. The central vim grammar operates on **this
  Document**, exactly as it does for a code buffer. Motions,
  text objects, marks, search, registers, visual selection —
  all reuse the existing implementations without modification.

The renderer continues to paint the alacritty cell grid (one
fast path, one set of colour / attribute semantics, no
re-shaping). The host translates document-space selection
ranges → cell-grid coordinates at publish time. The renderer
consumes the same `visual_range` / `visual_block_extents`
fields it already does — no peer-side change is needed.

## 3. The mechanism

### 3.1 Snapshot lifecycle

```
Terminal-Insert ──(<Esc> / <C-\><C-n>)──▶ build SyntheticDoc ──▶ Normal-in-terminal
                                                ▲                       │
                                                │                       │
                                            (frozen)               (motions, visual,
                                                │                   marks, search,
                                                │                    yank, ...)
                                                │                       │
Normal-in-terminal ──(i / a / I / A)──▶ drop SyntheticDoc ──▶ Terminal-Insert
```

The snapshot is **frozen for the duration of Normal mode.** PTY
output continues to feed alacritty in the background; the
SyntheticDoc does not auto-refresh. Re-entering Insert mode
drops the snapshot and the user catches up to live. This
mirrors vim's `:terminal` semantics and gives the user a stable
target for motions / search / marks (a refreshing buffer is
hostile to mark-based navigation).

### 3.2 SyntheticDoc construction

```rust
fn build_normal_snapshot(t: &TerminalBuffer) -> SyntheticDoc {
	let (top, bot) = t.term.line_bounds();
	let mut rope = Rope::new();
	for line in top..=bot {
		let s = t.term.line_range_text(line, line);
		rope.append(s);  // line_range_text already appends '\n'
	}
	let cursor = translate_cell_to_doc(
		t.term.cursor_line(),
		t.term.cursor_col(),
	);
	SyntheticDoc {
		rope,
		cursor,
		mutability: BufferMutability::ReadOnly,
		origin: SyntheticOrigin::TerminalScrollback {
			buf_id: t.buf_id,
			frozen_at: t.term.snapshot_seq(),
		},
	}
}
```

Build cost is bounded by `terminal.scrollback-lines × cols` —
default 10 000 × 200 ≈ 2 MB. With ropey's batched append
(`from_iter` or pre-sized `Rope::from(&str)` over a single
`String`), build time is well inside Insert→Normal transition
budget. Bench gate: ≤ 2 ms p99 at 10k × 200 (see §6).

Trailing blank cells on each row are **stripped** before
appending — alacritty pads short lines with spaces; without the
strip, `$` lands past the visible content. This matches what
the user sees on-screen.

### 3.3 Coordinate translation

Two translations exist, both at publish-time only:

- **Doc-cursor → cell-grid cursor** (for the renderer's cursor
  highlight). Linear: `cell_row = doc_line - origin_top_line`,
  `cell_col = doc_byte_col` (when scrollback hasn't wrapped a
  single source line across multiple cell rows — the common
  case; wrap awareness is open question §7).
- **Doc selection range → cell-grid range** (for visual
  overlays). Same line-arithmetic; the publisher writes
  `visual_range` / `visual_block_extents` in document coords,
  and an adapter on the published path remaps to cell coords.
  Renderers consume the cell-space values unchanged.

Both translations live in `lattice-host`, not the renderer.
This preserves the rule that renderers don't branch on buffer
kind: they always read cell-space overlay coords.

### 3.4 Read-only mutability

The SyntheticDoc declares `BufferMutability::ReadOnly`. The
existing `Editor::run_read_only_motion` path (used by help / log
buffers) routes operator commands to the "buffer is read-only"
echo. Yank (`y` / `Y`) is a non-mutating operator and works.
Visual + yank gives "copy from terminal scrollback" with vim
range semantics — strictly more capable than the current
bespoke `t.visual` + alacritty-side selection code.

The four operator classes:

| Operator | Behaviour |
|---|---|
| `y` / `Y` | Yank document range into register. Works. |
| `d` / `c` / `>` / `<` / paste / etc. | Read-only echo, no-op. |
| Motions | Move cursor / extend visual. Works. |
| Text objects (`iw`, `aw`, `i"`, ...) | Resolve range against doc. Works. |

### 3.5 What `BufferKind::Terminal` still owns

Lattice does not have buffer-locals (no string-keyed attribute
bag). Each buffer kind is a typed enum variant on
`BufferData` in the registry (`BufferData::Terminal(TerminalBuffer)`,
`BufferData::Document(DocumentEntry)`, etc.). The
`TerminalBuffer` struct is the only durable per-buffer storage,
accessed via `BufferRegistry::with_terminal[_mut](id, |t| …)`.

The terminal buffer kind is **not deleted**. The struct still
owns:

- The alacritty `Term` + PTY handle (live).
- The reader task that feeds parsed bytes to `Term`.
- The active SyntheticDoc when in Normal/Visual — a new
  `Option<Arc<SyntheticDoc>>` field, populated by the
  `TerminalNormalMode` on-enter callback and dropped by the
  on-exit callback (see §3.6).
- Sub-state visible to publish (`terminal_insert_active`
  becomes a derived property of "which mode is currently
  active" rather than a freestanding bool).
- Resize / spawn / kill lifecycle.

**Storage is data-only here.** `TerminalBuffer` does not call
`build_synthetic` itself, does not own the keymap, and does not
decide when to build or drop. Those decisions belong to the
`TerminalNormalMode` minor mode (§3.6), consistent with the
saved invariant *"Modes own their buffers, App is a host"*.

What goes away from `TerminalBuffer`: `nav_cursor`, `t.visual`.
What goes away from the host: `terminal_visual_active`,
`do_terminal_nav_move` / `do_terminal_nav_goto` /
`do_terminal_nav_word`, the `TerminalWordDir` enum +
`terminal_word_step` helper, the motion arms in
`run_terminal_invocation`, and the renderer-side branches that
key off `terminal_visual_active`.

### 3.6 Ownership: `TerminalNormalMode` as the home

The lifecycle, keymap, paint hook, and active-text resolver all
sit on a new minor mode, `TerminalNormalMode`, registered
against the `Mode` trait alongside the existing
`TerminalInsertMode`. The two are mutually exclusive minor
modes under the `terminal-mode` major: exactly one is active
whenever the focused buffer kind is Terminal.

Without the mode, the lifecycle would scatter across
`do_terminal_enter_insert` / `do_terminal_exit_insert` shims on
`Editor` and a kind-branch in the publisher. With the mode,
each concern auto-installs on activation and auto-uninstalls on
deactivation — no per-call-site guards.

**What the mode owns:**

1. **On-enter callback.** Calls
   `lattice_terminal::build_normal_snapshot(t)` via
   `with_terminal_mut`, stores the result as
   `t.synthetic = Some(Arc::new(doc))`, translates the alacritty
   cursor → doc cursor, points the active selection at the new
   doc.
2. **On-exit callback.** Sets `t.synthetic = None` and
   snap-to-live-edge on the alacritty grid. Free — drops the
   `Arc`.
3. **Publish-time hook.** Registers a coord-adapter step in the
   publish pipeline that remaps `visual_range` /
   `visual_block_extents` from doc-space to cell-space whenever
   this mode is active. The hook auto-uninstalls on
   deactivation, so the publisher itself stays buffer-kind-
   agnostic.
4. **Active-text resolver.** When the mode is active, the
   host's `Editor::active_text()` returns the SyntheticDoc
   rope, not the (absent) document rope. This single hook is
   what lets the central grammar's motions / text objects /
   search machinery operate against the scrollback without any
   kind-aware branching.
5. **Keymap.** Only the **transition** keys — `i`, `a`, `I`,
   `A` deactivate self + activate `TerminalInsertMode`.
   `<C-\><C-n>` does the reverse (declared on
   `TerminalInsertMode`, not here). Every other binding —
   motions, text objects, search, marks, visual, registers,
   yank — is inherited from the standard Normal keymap, which
   already flows through mode-resolved central dispatch
   (M.0–M.9 landed). No new motion implementations.
6. **Modal cursor shape.** Driven by the standard
   `ModalState`, not a bespoke `t.cursor_visible` flag. The
   mode just declares "I am a Normal-style mode" and the
   existing cursor-shape resolver handles it.

`TerminalInsertMode` (already exists) is the symmetric peer:
owns PTY-bound encoding, `<C-w>` shell-passthrough,
`<C-\><C-n>` exit, and the `<Esc>` → exit gated by
`terminal.esc-exits`. Its on-enter callback drops
`t.synthetic`; on-exit, it does nothing (the symmetric build
lives on `TerminalNormalMode`).

The asymmetric storage-on-`TerminalBuffer` / behaviour-on-mode
split keeps two saved invariants intact:

- *"Modes own their buffers, App is a host"* — mode hooks call
  through host services (`with_terminal_mut`,
  `Editor::active_text`); no `ensure_*` or `drain_*` shims grow
  on `App`.
- *"Buffers must not have kind-specific logic"* — `TerminalBuffer`
  is data only. The branching that exists today in
  `run_terminal_invocation` is replaced by mode-resolved
  dispatch.

## 4. What is unified, mechanically

Once the SyntheticDoc is in place:

- **Motions** (`h` / `j` / `k` / `l` / `w` / `b` / `e` / `W` /
  `B` / `E` / `0` / `^` / `$` / `g_` / `gg` / `G` /
  `f` / `F` / `t` / `T` / `;` / `,` / `%` / `(` / `)` /
  `{` / `}` / `gj` / `gk` / `H` / `M` / `L`) — all of them, for
  free. They are existing grammar implementations against a
  rope-backed document.
- **Text objects** (`iw`, `aw`, `i"`, `a"`, `i(`, `a(`, etc.) —
  for free.
- **Search** (`/`, `?`, `n`, `N`, `*`, `#`) — for free.
  `SharedTerm::find_match` / `find_all_matches` get retired;
  search runs against the rope. Search highlighting (`hlsearch`)
  flows through the existing `all_matches` publisher.
- **Marks** (`m{a-z}`, `'{a-z}`, `` `{a-z} ``) — for free.
  Local marks land in document space, survive the Normal-mode
  session. Cross-buffer jumps (jumplist) record document-space
  positions tagged with the SyntheticDoc origin; on return,
  the snapshot is rebuilt and the position is re-resolved
  best-effort (if scrollback rolled, fall back to nearest valid
  line).
- **Registers** — yank into named / unnamed / numbered registers
  works via the existing yank pipeline.
- **Visual modes** (charwise, linewise, blockwise) — work via
  `SelectionSet` against the SyntheticDoc. Blockwise lands
  through the renderer-neutral `visual_block_extents`
  (2026-05-27) the document peers already paint.
- **Jump list** (`<C-o>` / `<C-i>`) — uses
  `PositionEntry` against document space (already partially
  done; the SyntheticDoc unifies the coordinate space).
- **Modal cursor shape** — driven by the standard `ModalState`,
  not a bespoke `t.cursor_visible` flag.

## 5. What stays terminal-specific

- **Insert-mode encoding** (`encode::key_to_ansi`, app-cursor-
  keys mode, alt-prefix, F-key tables). Unchanged.
- **PTY lifecycle** (spawn, resize, kill, exit). Unchanged.
- **Reader task** (vte parser → alacritty `Term`). Unchanged.
- **The cell grid as a renderer primitive.** Painting reads the
  alacritty grid; the SyntheticDoc is **never rendered as a
  rope**. The doc exists only to give the grammar a coordinate
  space to operate on.
- **Snap-to-live-edge on Insert entry.** Unchanged. The current
  T3 logic that scrolls alacritty to the live edge on `i`/`a`/
  `I`/`A` stays; the SyntheticDoc is just dropped at the same
  moment.

## 6. Performance posture

- **Hot path (per-keystroke):** Identical to today. Motions run
  on the rope, same as code buffers. No new per-keystroke work.
- **Mode transition (Insert → Normal):** New cost. Rope build
  from `scrollback-lines × cols`. Bench gate `term_snapshot_
  build_p99_ms ≤ 10.0` at 10k × 200 — measured baseline
  ~8.7 ms (T-snap-1, 2026-05-27). Well inside the 60 Hz frame
  budget (16.6 ms); the transition is one-shot so the user
  perceives at most one frame of latency on `<Esc>` / `<C-\><C-n>`.
  The stress observation at 50k × 400 measures ~65 ms — if a
  user opts into that scrollback × cols, the design fallback
  applies: move the rope build off the dispatch path into a
  background task (Insert → Normal returns immediately;
  Normal-mode motions are no-ops until the build lands — same
  shape as cold-start LSP gating).
- **Mode transition (Normal → Insert):** Drop an `Arc`. Free.
- **Memory:** One `Arc<Rope>` per terminal buffer while in
  Normal mode, sized as above. Dropped on re-entry to Insert.
  Long-lived terminal buffers parked in Insert pay nothing.

## 7. Open questions

- **Wrap-awareness in coord translation.** If a single shell
  output line is longer than `cols`, alacritty wraps it to
  multiple cell rows. The SyntheticDoc has one rope line per
  cell row (current spec) or one rope line per source line
  (alternative). The former gives a 1:1 cell-grid mapping; the
  latter gives "vim-correct" `$` and `j`/`k` semantics. Lean
  former (1:1) for v1 — it preserves the user's visual mental
  model. `gj` / `gk` work either way (they're display-line
  motions). Re-visit if it bites.
- **Live-following Normal mode (vim's `terminal.scrollback_
  follow` analogue).** Some users will want "Normal mode that
  auto-refreshes as output arrives." v1 says no: freeze is the
  default. Post-v1 option `terminal.normal_follow = false`
  (default) | `true` could refresh the SyntheticDoc on each
  alacritty snapshot publish. Marks invalidate; jump list
  re-resolves best-effort. Out of scope for the v1 unification.
- **Alt-screen behaviour.** Programs like vim / less / htop
  switch to the alt screen. The SyntheticDoc on Insert → Normal
  transition should snapshot the **primary** screen's
  scrollback, not the alt screen — the alt screen is a transient
  app-owned canvas. Open how `<C-\><C-n>` mid-`htop` behaves;
  probably "alt screen contents at moment of transition, no
  scrollback history". Decide before T-snap-1.
- **Cursor on a wide / zero-width cell.** Alacritty's grid can
  hold double-width and zero-width chars. The rope is byte-
  indexed. Translation needs to know cell width per char.
  Solvable; mark it in T-snap-1 unit tests.

## 8. Slice plan

Sequencing lives in
[`docs/dev/operations/slice-plans/terminal-as-document.md`](../operations/slice-plans/terminal-as-document.md);
authoritative status per slice lives in
[`docs/dev/operations/implementation.md`](../operations/implementation.md).
This fragment owns *what* and *why*; the slice plan owns
*when* and *in what order*.

## 9. Testing strategy

- **Unit tests in `lattice-terminal`**: SyntheticDoc construction
  for grids of varied shape; cursor translation including
  wide-char edge cases; trailing-blank strip; alt-screen
  snapshot semantics.
- **Unit tests in `lattice-host`**: Insert → Normal builds the
  doc; Normal → Insert drops it; central grammar dispatch routes
  motions to the SyntheticDoc when the active buffer is
  Terminal-in-Normal; read-only operators echo correctly.
- **Integration tests via the TUI peer**: `<C-v>` blockwise
  selection on terminal scrollback paints the same rectangle as
  today; search highlights match; marks survive a round-trip.
- **Bench**: `term_snapshot_build_p99_ms` at
  `(10 000, 200)` and `(50 000, 400)`. CI gate at the former;
  the latter is a stress observation.
- **Regression locks** before T-clean-1: side-by-side test that
  every motion currently implemented in the bespoke path lands
  the same cursor position via the central path. Lock holds
  through the deletion slice.

## 10. Risks

- **Snapshot cost spikes** on huge scrollback. Mitigation:
  bench gate + fallback to background build if violated.
  Unlikely in practice (10k × 200 is far inside ropey's
  comfort zone).
- **Coord-translation correctness with wide chars.** Solvable;
  taken seriously in T-snap-1 unit tests.
- **Behaviour drift between bespoke and unified paths.** The
  regression-lock tests are the safety net.
- **Live-output disorientation.** Users may expect Normal mode
  to follow new output. Documented in user-facing terminal
  help that Normal = frozen snapshot (vim convention). Post-v1
  follow-mode opt-in addresses the gap.

## 11. Cross-references

- `terminal-mode.md` §5 (Modal sub-states) — supersede §5.1
  on what "vim grammar applies to scrollback" mechanically
  means; cross-link this doc.
- `mode-architecture.md` — `TerminalNormalMode` and the existing
  `TerminalInsertMode` are mutually-exclusive minor modes under
  the `terminal-mode` major. Reference this doc from the
  bundled-modes section.
- `design.md` §5.9.8 — append a sentence: terminal buffer's
  Normal/Visual sub-state operates on a synthetic Document built
  from scrollback under `TerminalNormalMode`; see this doc.
- `design.md` §5.1 — clarify that synthetic Documents (terminal
  scrollback, help, logs, scratch) share the rope-backed
  buffer abstraction; they differ only in `BufferMutability`
  and the absence of an on-disk file.
- `implementation.md` — add a `## terminal-as-document` block
  tracking T-doc-1 ... T-rich-1 slice status as they land.
