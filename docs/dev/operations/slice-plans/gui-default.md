# GUI by default — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contract: [`../../architecture/gui-default.md`](../../architecture/gui-default.md)
> (the peer contract; GD.9 extends it with the flip).
> Closes the ledger's `5.last` row in [`../implementation.md`](../implementation.md).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred · ❌ dropped.
Every non-trivial slice ships the four artefacts (doc + bench-where-perf-relevant
+ test incl. failure modes + graceful error handling).

**Status: 🚧 GD.0 ✅, GD.0b ✅, GD.1 ✅, GD.2 ✅, GD.3 ✅, GD.6a ✅, GD.6b ✅, GD.5a ✅, GD.7a ✅, GD.7b ✅, GD.8a ✅, GD.5b ✅, GD.6d ✅, GD.7d ✅, GD.9a ✅, GD.8b ✅, GD.6c ✅, GD.5c ✅, GD.8c ✅, GD.7c ✅; GD.4 📝; GD.9b ❌.** Target release: **0.9.8**.

## What this is

`lattice` with no flag opens the GPUI window; `--tui` opens the terminal
renderer. Decided 2026-10-11. The ledger had scheduled the flip for "after
parity + a release cycle as opt-in"; this plan does parity and the flip in one
release, with the flip as the last slice so it can slip without the rest.

The flip is one line in `main.rs`. The plan is everything that has to be true
before that line is honest.

## What the 2026-10-11 audit found

Two read-only passes over both peers (nothing was run, so every entry below is
a claim to verify in the slice that fixes it — a slice that finds its gap was
not real says so and is marked ❌ with the reason).

The paint side is close. The behaviour side is not, and for a structural
reason: the TUI reaches the host through `Editor::dispatch_fused`, which runs
the post-dispatch tail in the actor; GPUI calls bare `Editor::dispatch` and
re-implements a subset of that tail by hand. Each tail step added host-side
since reached one peer. `maybe_reparse_syntax` was caught that way in
`53dac8dc`; `sync_keymap_overlays`, `maybe_adopt_owner_write` and the
effect-ordering of follow-up actions are still missing. The same shape repeats
for the idle drain and for renderer signals.

So the order below is: make the contract shared (GD.0, GD.1), make it tested
against both peers (GD.2), then close the surface gaps (GD.3–GD.8), then flip
(GD.9).

## Sequencing

```
GD.0 shared tail ─► GD.1 idle drain + signals ─► GD.2 peer conformance suite
                                                        │
        ┌───────────────┬───────────────┬───────────────┼───────────────┐
        ▼               ▼               ▼               ▼               ▼
   GD.3 window     GD.4 text       GD.5 completion  GD.6 buffer     GD.7 terminal,
   lifecycle       input (IME)     + `:` line paint text paint      modeline, rows
        └───────────────┴───────────────┴───────┬───────┴───────────────┘
                                                ▼
                                      GD.8 small behaviours
                                                ▼
                                          GD.9 the flip
```

GD.0–GD.2 are strictly ordered. GD.3–GD.7 are independent of each other; the
order given is by user-visible severity. GD.9 depends on all of them.

## Slices

### GD.0 ✅ — one dispatch tail, shared

GPUI's `dispatch_action` adopts `Editor::dispatch_fused`.

- Fused path: one actor crossing per plain key (was two or more). Retires the
  "GPUI parity — deferred" note in [`input-latency.md`](input-latency.md) § I.7.
- Non-fused path: effects, then queued + returned signals, then each follow-up
  action through the full `dispatch_action` (so an effect lands before the next
  action runs — macro replay, autopilot chains), then the tail in ONE crossing:
  `ensure_cursor_visible` (skipped on popup dismiss) → `maybe_reparse_syntax` →
  `sync_keymap_overlays` → `run_tick_pending`, then queued renderer effects.
- `sync_keymap_overlays` is the fix for completion-popup and snippet-tabstop
  keys: nothing else activates `completion-popup-mode` / `active-snippet-mode`.
- `Render::render` stops dispatching. The per-frame
  `dispatch_action(EnsureCursorVisible)` goes; the tail and
  `set_viewport_height` own cursor visibility, as in the TUI. This is a
  CLAUDE.md forbidden pattern (dispatch + tick drain inside `render`), so it is
  relocation, not polish.
- The `PickerAccept` / `PickerDismiss` / `TransientDismiss` early-return arms
  stop skipping the tail.

Landed 2026-10-11. Four tests in `lattice-ui-gpui`, each run against the
pre-GD.0 dispatch and seen to fail there: a typed character is one actor
crossing (was four); a motion is at most three; the completion popup's mode
follows the popup; a replayed macro types into the buffer it opened. The
picker early-return arms are gone outright — the host already handled all
three actions, so the generic path covers them.

Not seen: the GUI itself. `Render::render` no longer clamps the cursor, on the
argument that the tail and the viewport setters cover every way the cursor or
viewport moves. That argument is by reading; scroll-follows-cursor needs eyes.

Left in `render`, deliberately: `set_viewport_height` and `SetCellMetrics`
still dispatch. Both are gated on the value changing, so they fire on a resize,
not per frame, and `SetViewportHeight`'s action arm carries the vim `scroll`
reset that the actor's typed command does not.

### GD.0b ✅ — fuse on host-complete effects

Found by GD.0's own test. `dispatch_fused` declined the fuse whenever
`outcome.effects` was non-empty, and a motion leaves `Effect::CursorMove` there
though the host has applied it. So only typed characters fused; every motion
and operator took the multi-crossing path in BOTH peers, which is not what
`input-latency.md` § I.7 said.

Landed 2026-10-11, after GD.2 so the conformance suite could hold it.
`lattice_host::effect_completeness::effect_is_host_complete` is an exhaustive
match over `Effect` with no wildcard; `dispatch_fused` fuses when every
returned effect answers `true`. Eleven variants do — the ones the editing hot
path produces and for which both peers' arm is a no-op (`CursorMove`,
`CursorMoveIn`, `SelectionChange`, `EnterMode`, `Edits`, `Yank`, `Echo`,
`ClearSearchHighlight`, `RecordJump`, `None`, `Declined`). The other 128 answer
`false`, including many that would qualify: `false` is always safe, and nobody
holds a key down on `:describe-option`.

Both peers still run every returned effect through their arms on the fused
path. For a host-complete effect that is a no-op, and it means a variant
wrongly answered `true` runs late rather than never.

Conformance scenario `a_motion_and_an_operator_are_one_actor_crossing_each`:
`j` and `x` are one crossing each in both peers (the GUI was three, and the
TUI's non-fused path makes more than that).

### GD.1 ✅ — idle drain, quit, and dropped signals

- Idle wake drains `pending_renderer_effects` (TUI: `drain_async_pending`). Off
  the render path: the bridge task does it, not `render`.
- `should_quit` is observed wherever it can be set (mouse, async), not only at
  the end of `on_key_down`.
- Signals GPUI drops today: `ToggleMode`'s return, `:g`'s `renderer_signals`,
  the `DisplayBuffer` cascade, `NerdFontsToggled` (must re-render every glyph
  buffer — the icon-degradation rule).
- `load_persistent_config` moves out of `GpuiApp::new` so GPUI unit tests stop
  reading the developer's real config; its boot signals are applied.

Test it the way it fails: the async result is visible with NO further action
dispatched.

Landed 2026-10-11.

- The host publishes `lifecycle.renderer_effects_pending`. The window's repaint
  wake reads it and crosses the actor only when it is set, so the drain is free
  on the wakes a keystroke causes. The TUI still crosses on every idle repaint
  (`drain_async_pending`); it can adopt the flag, and GD.2 is where both peers
  get one test for it.
- Quit is checked on every repaint wake as well as after a key.
- `GpuiApp::new` no longer loads the user's config or snippets;
  `window::run` calls `load_user_environment`, which applies the load's
  signals.
- Tests: a queued effect is applied with nothing dispatched; a nerd-font toggle
  reaches the open file tree.

Not tested: the quit-on-wake path and the three signal cascades (`:toggle-mode`,
`:g`, `DisplayBuffer`) — each is now a call to the existing signal handler, and
needs a mode whose activation raises a signal to observe. Not seen: any of it in
a window.

### GD.2 ✅ — peer conformance suite

One set of scenarios, driven through each peer's own key and dispatch entry,
asserting the same editor state. Scope is behaviour (state after a key
sequence, actor crossings), not pixels.

Landed 2026-10-11 as `lattice_host::peer_conformance`, behind the
`peer-conformance` feature that each peer enables from `[dev-dependencies]`.
A peer implements `PeerUnderTest` and writes
`lattice_host::peer_conformance_tests!(Peer)`; the scenario list is in the
host's macro, so a new scenario reaches both peers without either being
touched. Decided on heuristic #1 over a separate crate depending on both peers:
a peer exposes an in-process editor only under its own `cfg(test)`, and a
second crate would need an actor-backed, async-settling harness for every
scenario.

Seven scenarios, green in both peers: keys edit the buffer; a typed character
is one actor crossing; the cursor stays in view; the completion popup's layer
follows the popup; a replayed macro types into the buffer it opened; a queued
effect is applied on an idle wake; quitting is published. GD.0's and GD.1's
GUI-only copies of four of these were removed in favour of the shared ones.

The decline-and-fall-through walk was not covered at first: the TUI's sat
inline in `runtime.rs`, above the chord entry the suite drives. GD.8b moved it
and added a scenario for it, the ninth.

### GD.3 ✅ — window lifecycle

Landed 2026-10-11.

- **Close button, Cmd+Q, the Quit menu item** all go through
  `GpuiApp::request_close`, which is `:qa`: an unsaved buffer refuses, the
  reason is echoed, and the window stays. Plugin stores are flushed before the
  window goes. When the last window closes the application quits.
- **Files from the OS.** `Application::on_open_urls` queues the path and wakes
  the window (the callback has no app context and can fire before the window
  exists); a file dropped on the window opens directly. Both are `:e <path>`.
- **`--tutor [N]`** reaches `lattice_ui_gpui::run`.
- **Window title** is the active buffer's file name, computed on the repaint
  wake and handed to the platform from `render` only when it changed.

Tests cover the logic: the close guard both ways, an OS-opened file and its
title, URL-to-path parsing including the malformed cases, and the tutor flag.

**None of the GPUI wiring has been seen running.** It compiles against gpui
0.2.2; that is all. Specifically unverified: that `on_window_should_close`
returning `false` keeps the window on each platform; that `cmd-q` reaches the
action before `on_key_down` sees it; that `on_open_urls` fires for a `.app`
launch; that a drop on the root element delivers `ExternalPaths`.

Left out: the menu is the application menu with Quit only. Edit items (cut,
copy, paste, undo) need actions that route into the editor's registers and
undo, which is its own piece of work — noted under GD.8. File associations in
the bundle's `Info.plist` are a packaging change — GD.9.

### GD.4 📝 — text input

IME, dead keys, composition, and the layouts where `Keystroke.key` is not the
character typed. Needs a GPUI `InputHandler`. Establish first (by running it)
what `Keystroke.key` delivers for shifted symbols and non-US layouts, and
macOS option-as-meta.

### GD.5 — completion and command-line paint

**GD.5a ✅ — the completion popup at the cursor** (2026-10-11). The
insert-completion popup was pinned to the window's top-right corner. It is
anchored to the completion's anchor (where the replaced text starts, so it does
not slide as you type), below the row or above it when it does not fit, and
kept inside the window. The docs panel sits beside it, right if there is room
and left otherwise. The placement is one pure function shared with the hover
popup, which is what the tests cover.

Not seen in a window, and two things about it are approximate by construction:
the popup's size is estimated before layout (candidate rows + footer), and the
horizontal origin is `pane padding + byte × cell width` — it does not add the
gutter's width or a split pane's offset. The hover popup has always had that
second limitation; sharing the function means fixing it once fixes both, and it
needs eyes.

**GD.5b ✅ — the `:` line's caret and hints** (2026-10-11). The window's
command line showed the typed text and **no caret at all** (the audit had it as
"pinned to the end"; there was none). It now draws a bar at the cursor's byte in
the `:` line, the search line, a prompt, and on the cursor's row of the expanded
band — the focused editing buffer's cursor, which was already published.

The hints that trail the line — `press a chord`, `(chord)`, the completion
`(n/m)`, a live error, the parameter hint — were inline in the terminal's draw
function and absent from the window. They are content, so they moved to
`lattice_host::cmdline_row::trailing_hints`, which both peers call; each peer
only decides how a `HintKind` looks. The window's hint colours come from the
theme (`comment` for the dim ones) instead of a fixed grey.

The terminal's caret column was a byte count; it is `cmdline_row::caret_cols`
now, which fixes it sitting one cell right per multi-byte character before it.

Gate: targeted — the host's `cmdline_row::` module, the GPUI bottom-row tests,
the TUI's command-line tests. **The caret element has not been seen**: it is a
zero-width box holding an absolutely positioned bar, and whether it lines up
with the text is exactly what a screenshot would tell.

**GD.5c ✅** (2026-10-12):

- **Ghost text.** The suffix and its placement rule (cursor at the end of its
  line) are published as `CompletionRenderState::ghost_suffix`, computed once
  per publish by `Editor::completion_ghost_text_at_cursor`. The terminal reads
  it; it used to call into the editor with a blocking actor round-trip from
  its draw function on every frame the cursor sat at a line's end. The window
  draws it as an end-of-row overlay, dim and italic, in the fold summary's
  tone — the same mechanism as the fold summary, so it cannot widen the row.
- **Popup origin.** The focused pane's `paint` records where its text starts
  in window pixels (`TextOrigin`: pane bounds plus gutter width, the only
  place both are known), and the completion and hover popups are placed from
  it through `text_cell_px`. That includes the gutter, a split pane's offset
  and `leftcol`. The reader sees the previous frame's origin; before the first
  paint the old single-pane estimate stands in.
- **The band's first line is coloured** by the `:` line's decoration spans,
  as the terminal's band is. Later lines are plain in both.

Not seen running. Known approximations: the popup's column is still the
anchor's byte offset, not its character count, so text with multi-byte
characters before the anchor puts the popup too far right; rows are still
`row × estimated row height`, which is wrong below a heading-scaled row
(GD.7c's subject).

### GD.6 — buffer text paint

Split when the slice started: the four items turned out to live in three
different places.

**GD.6a ✅ — `relativenumber`** (2026-10-11). Renderer-only. The gutter row
carries an optional distance, measured from the pane's own cursor (an inactive
pane's, not the focused one's), blank on the cursor's line and off on a
multibuffer, as in the TUI. Tested at the formatter: the distance is printed in
the same cells the number was. Not seen in a window.

**GD.6b ✅ — LSP semantic tokens** (2026-10-11). Decided: baked into the
display matrix host-side, not merged per renderer (paramount #1 — no merge on
either paint path — and the parity rule). `semantic_spans.rs` turns the tokens
covering the window being built into spans layered over the grammar's and under
a buffer's static extra spans; `MatrixVersion` gained a `semantic_tokens` axis,
rebuild-class like `syntax`, stamped by `TokenStamps` (an O(1) "is this the same
`Arc`" check on the actor — the tokens themselves are read only in the worker).
The TUI's render-time merge and its fixed terminal colours are gone; token
colours now come from the theme's syntax elements in both peers.

Two behaviours worth knowing. A new token set takes the full window build, never
the incremental one, and an untouched line comes out equal — tested, because a
recolour of unedited text was the risk named when this was chosen. And an edit
rebuilds its line with the *stale* tokens when the line count did not change
(typing inside a line keeps its colours) and without them when it did (stale
tokens would be on the wrong lines).

Gate: targeted — `lattice-cells` whole, the host's `cells_worker` /
`semantic_spans` / `render_state` / `display_matrix` modules and the multibuffer
integration test, the TUI's `render::` and `cells_render::` modules. The whole
`lattice-host` and `lattice-ui-tui` suites were not run. Not seen in a window.

**GD.6d ✅ — semantic-token modifiers** (2026-10-11). `deprecated`
(strikethrough), `readonly` / `static` (italic) and `defaultLibrary` (dim) are
back, in both peers. A display run holds one `Style`; a modifier is a second
layer on text that already has one, so it rides the run's `flags`, which
already existed for the splice bits. `cell_flags` gained `STRIKETHROUGH` and a
`STYLE_MODS` mask; `semantic_spans::modifier_spans_for_line` produces the byte
ranges per row (independent of the token's type — a deprecated token of a type
with no style of its own is still struck through); the three consumers of a
run — the host's cell projection, GPUI's `display_run_to_synthetic_cell`, the
TUI's `display_run_to_style` — OR the run's style bits onto what its style
resolves to. GPUI paints the strike on both its text-run and per-glyph paths.

Also closes a hole GD.6b left: the two-row conceal-reveal rebuild coloured from
the grammar alone, so a buffer with both conceal rules and semantic tokens now
takes the full build instead.

Gate: targeted — `lattice-cells` whole, the host's semantic and worker tests,
the TUI's `cells_render::`, GPUI's `cells_paint::` / `paint_cells::` (one red
there, `a_run_with_no_foreground_inherits_default_fg_not_black`, which predates
this branch). The strike's vertical position in the window is a guess — not
seen.

**GD.6c ✅ — highlight kinds and strikethrough** (2026-10-12).

- **Kinds.** `OverlayLayer::DocHighlight` carries a host
  `DocHighlightKind` (text / read / write), and the published highlight list
  is `DocHighlightSpan { range, kind }`. The kind is folded into the overlay
  version, so a response that changes only a kind rebuilds the bucket. The GUI
  resolves `doc_highlight.{read,write,text}`; it drew every occurrence as a
  read. The TUI keeps its per-row walk for this layer because it also paints
  inactive panes, which have no bucket.
- **Substitute preview is struck through in the GUI.** The overlay quad's
  colour carries a flag bit (`OVERLAY_STRIKE`); `paint` fills the background
  as before and, after the text body, draws a line across flagged quads in the
  text colour. It works the same whichever path painted the glyphs. A flag on
  the colour and not a field because the quad is a tuple that the wrap and
  heading splitters and every push site already carry.

❌ **The current match's foreground — dropped.** The audit listed it as a
terminal behaviour the window lacks. It is not one: theme-system T.6 retired
the foreground recolour, and the current match is a background tint from
`search.current` in both renderers.

Not seen running: the strike line's height (`OVERLAY_STRIKE_ROW_FRACTION`) is
a guess, and on a heading-scaled row it follows the row's height, not the
glyphs'.

### GD.7 — terminal, modeline, scaled rows

Split when the slice started, for the same reason as GD.6.

**GD.7a ✅ — terminal cell attributes** (2026-10-11). Bold, italic, dim and
strikethrough are painted (only reverse and underline were); underline is a
text decoration and no longer a bottom border. Search hits are underlined and
keep their colours; the current match and the Visual selection invert — the
TUI's three styles, where this peer had one. The paint decision is a pure
function (`terminal_run_paint`) and is what the tests cover. Not seen in a
window; in particular that a bold run keeps the cell grid aligned depends on
the terminal font's bold face having the regular face's advance.

**GD.7b ✅ — modeline labels for help, file-tree and oil panes** (2026-10-11).
Decided: the owning mode contributes the label as modeline content (mode
ownership; paramount #2 — a plugin reaches the same slot with a
`ModelineElementUpdate`). The label is `core.path`'s content for that buffer in
the modeline store; `modeline::pane_path_segment` prefers it over the path, for
the pane's committed buffer. The text is each mode crate's
(`lattice_listing::{file_tree,oil}::modeline_label`,
`lattice_help::modeline_label`), written through the generic
`Editor::set_buffer_label` at the one place each mode's state is already
written (`set_file_tree_root`, `set_oil_dir`, `seed_help_metadata_locals`). The
TUI's `PaneStatusFn` table and its three functions are gone, and with them the
`provider_label` parameter both peers threaded into the resolver.

One thing this is not: those three chokepoints are `Editor` methods. The
listing and help lifecycles are host-driven today, and this slice did not move
them; it put the label text with the mode and the write next to the state.

Gate: targeted — the host's `modeline::` module, the TUI's status / modeline /
tree / oil / help tests, `cargo check` on the GPUI crate. Not seen in a window.

**GD.7d ✅ — oil's pending-changes marker** (2026-10-11). The TUI's oil label
ended in ` [+]` when the listing had unapplied edits, computed per frame by
rendering the whole buffer to a string and comparing it to the snapshot — on
the paint path. GD.7b dropped that. It is back, from the buffer's ordinary
modified flag, which first had to be made true for these buffers: an owner
write left the document dirty and nothing marked it clean, so an oil listing
read as modified from the moment it was filled.

`Document::mark_clean` (core) declares the current text the clean baseline
without writing it; `ActorMsg::MarkClean` and a defaulted
`lattice_runtime::Document::mark_clean` carry it (the default is a resolved
no-op — a multibuffer derives `dirty` from its sources and has no baseline to
move). `replace_owned_buffer` and `replace_owned_document_text` call it, awaited,
so the flag is right by the time the dispatch publishes. `pane_path_segment`
then appends ` [+]` to a mode-given label when the buffer is modified. Undoing
back to the listing clears it, as undoing back to a save does.

A behaviour change beyond the label: every buffer filled by an owner *replace*
(listings, trees, help, exports) now reads clean until the user edits it.
Append-written streams (`*messages*`, logs) are untouched — they are named
synthetics, which the path segment already exempts from the marker.

Gate: targeted — `lattice-runtime` whole, the new
`oil_modeline_marks_pending_changes` test, the host and TUI tests around dirty
state, listings, help, quit and owner writes. Not seen in a window.

**GD.7c ✅** (2026-10-12) **— heading-scaled rows.** `leftcol` was already
honoured (HS.1b), so the slice is the height half: theme-system F.1.

A title row is `title_scale` line-heights tall in the window, and the host's
scroll walks charged it one. IM.1 had already turned those walks into a
line-height budget (`RowWeights`); nothing told them about headings.

- `heading_scale_split` moved from `lattice-ui-gpui::cells_paint` to
  `lattice-host::display_matrix` (re-exported, so the paint is unchanged).
  The paint and the scroll now read one answer.
- `Editor::heading_heights` hands a walk the built display rows and the
  resolved theme. `bottom_anchored_scroll` and `weighted_doc_line_cost` ask
  it for any line that has no explicit weight.
- The gate is `Editor::cell_metrics`: only a peer that draws pixels
  publishes it. The terminal returns before loading anything, so its walks
  cost what they did.

Decided (Dhruva, 2026-10-12): the host computes it, for the lines in the
built display window — paramount #1 (off the UI thread, on rows that are
already built) and heuristic #1 (the scale is derivable host-side; the
arithmetic existed). Rejected: the window reporting measured heights back
(a layout/scroll feedback loop, and a renderer only knows its viewport);
whole-document weights from the syntax tree (per-edit work for a gain the
next matrix rebuild gives anyway).

Known limit: a line with no built row costs one row until the matrix is
rebuilt around it, so a jump far outside the window settles one rebuild
later. A wrapped heading is drawn at base size and costs its rows.

Gate: targeted — `a_scaled_heading_costs_its_title_scale_only_where_rows_scale`
and the IM.1 weight tests in `lattice-host`, the heading-split tests in
`lattice-ui-gpui`. Not seen in a window.

### GD.8 — small behaviours

**GD.8a ✅** (2026-10-11):

- `<Esc>` dismisses a popup only while the document has focus. With focus
  inside the popup it takes the keymap path, so a search in there is cancelled
  before the popup is. Same gate as the TUI.
- `<C-l>` is acknowledged, on the repaint wake and only when one is pending.
- The window sends its width in columns when it changes, so pane geometry
  stops assuming 120.

**❌ `ui.mouse` in the GUI — dropped.** The audit listed it as ignored. It is
ignored on purpose, and the option's own documentation says so: `ui.mouse`
exists because a terminal loses its native click-drag selection while Lattice
reads the mouse, and a window that owns its input takes nothing away by
listening. Decided by `core_options.rs` and `docs/user/mouse.md`, not by this
plan.

**GD.8b ✅** (2026-10-12):

- **The fall-through walk is behind an entry the suite drives.** The TUI's
  translate → apply → peel loop moved out of `runtime.rs` into
  `App::press_chord`, which the input loop and the conformance adapter both
  call; the GUI's `dispatch_chord` already had that shape. The shared scenario
  `a_declined_chord_falls_through_to_the_layer_below` (table-mode's `<Tab>`
  outside a table reaches insert-tab) replaces the GUI-only copy. The TUI's
  chord path now reads `chord_capture` and the completion layer from published
  state, as the GUI does; both were the same computation read live.
- **Popup colours.** The audit's entry was broader than the code: pickers,
  popups and the completion menu already resolve through the theme, with
  literals only as fallbacks for an unset element. The one surface that
  bypassed it was the modeline hover tooltip (two dark-palette constants),
  which now takes the popup background and the modeline's text colour.

Not seen running: the tooltip on a light theme.

**GD.8c ✅ — an Edit menu on macOS** (2026-10-12). Undo, Redo, Cut, Copy,
Paste, with Cmd-Z / Cmd-Shift-Z / Cmd-X / Cmd-C / Cmd-V bound on macOS only
(elsewhere the platform key is Super and a user's `<D-…>` bindings should reach
the editor).

Decided (Dhruva): each item **is the vim command it names**, against the `+`
register, through the editor's own dispatch — it protects paramount #3 (the
grammar is the command API; the menu is one more front-end) and leaves one
clipboard path. The window does not touch the OS clipboard itself.
`edit_menu_actions` is the whole mapping, a pure function:

- Undo / Redo — `u` / `<C-r>`.
- Copy / Cut — `"+y` / `"+d` on the selection; nothing without one.
- Paste — `"+p`, the same as vim (Dhruva: deviating offers no value): after
  the cursor in Normal, over the selection in Visual; `<C-r>+` where text is
  being typed (Insert, Replace, `:`, search).

Building the menu showed that put itself was thin: only `p` / `P` in Normal.
The rest of the family — a count, `gp` / `gP`, `]p` / `[p`, Visual `p` / `P`,
`:put` — landed beside this slice as host work (see the changelog), which is
what lets Paste replace a selection.

Not seen running: that the Cmd bindings reach the actions before
`on_key_down` sees the keystroke, and that the menu appears.

### GD.9 — the flip

**GD.9a ✅ — `lattice` opens the window** (2026-10-11). `lattice-cli`'s
`renderer::choose` is the whole decision, a pure function with a test per row:
`--tui` → terminal; `--gui` → window or an error; a build without the `gui`
feature → terminal; a macOS `.app` bundle → window; a reachable display →
window; otherwise terminal. Decided: with no display a bare `lattice` falls
back to the terminal on its own (the convention — it is what `emacs` does — on a
surface where convention leads), and `--gui` is never downgraded. The table and
how "reachable" is judged are in the design fragment §1.

Cargo's default feature set is unchanged (headless), so CI and `cargo test`
need no display libraries, and a source build without `--features gui` is still
a terminal editor. `install.sh` already installs the GUI build where one is
published, so most installs get the new default without a packaging change.
`--help`, the README, the install and troubleshooting pages, the site's landing
page and FAQ, and the installer's messages say so.

**This landed ahead of the open parity slices at Dhruva's direction; it does
not close them.** GD.4, GD.5b, GD.5c, GD.6c, GD.6d, GD.7c, GD.7d, GD.8b and GD.8c each
describe something the window does not do that the terminal does (or, for
GD.6d and GD.7d, something both lost on the way to sharing it). They block the
0.9.8 release, not this commit.

Not verified: the fallback on a real SSH session or a Linux console, and
whether a GPUI launch with a *wrong* "yes" fails cleanly or hangs. Only the
decision function is tested.

**GD.9b ❌ — release archive names.** Dropped for 0.9.8 (Dhruva, 2026-10-11):
the names stay — `lattice-*` terminal-only, `lattice-gui-*` the window build.
`install.sh` already installs the window build where one is published, so the
default reaches users without renaming assets that other things fetch by name.

## Verification that needs a person

Several of these cannot be checked from a headless session: IME, popup
placement, window close, light-theme colours, anything about how it looks. Each
such slice tests the logic underneath and says in its commit what was not seen.
