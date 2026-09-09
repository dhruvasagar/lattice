# Which-key — slice plan

Sequencing for `docs/dev/architecture/which-key.md`. The design fragment
owns *what* and *why*; this file owns *when* and *in what order*.

Design fragment: `docs/dev/architecture/which-key.md` (committed
`25a6fdfa`, no code at the time).

## Status

| Slice | What | Design § | Status |
|---|---|---|---|
| WK.1 | Resolver — `NodeView`, `continuations_with_context`, model + labels + collation | §2, §4 | ✅ |
| WK.2 | `layout_grid` — pure column-major grid | §6 | ✅ |
| WK.3 | Idle-gate primitive — `lattice-mode/src/idle_gate.rs`, `SubsystemBoot::idle_gate`, actor wiring | §5.1 | ✅ |
| WK.4 | `PartialChordPending` event + host publish | §5.2 | ✅ |
| WK.5 | `PopupPlacement::PaneBottom` — core + BOTH renderers | §6 | ✅ |
| WK.6 | The subsystem — options, mode, arming, model, `Effect::OpenPopup`, dismissal | §3, §5.3, §8 | ✅ |
| WK.7 | Lifecycle tests — asserted without a further keystroke | §11 | ✅ |
| WK.8 | Docs + benches — user page, nav, `BENCHMARKS.md` rows, ledger | §12 | ✅ |
| WK.9 | Key emphasis — `RenderedGrid` spans → `ExtraHighlights` | §6 | ✅ |

## Note on `continuations` (DK.4)

`KeymapHandle::continuations` landed with DK.4 (`d516ab8a`) for
`:describe-key`, and it is **not** the query which-key wants. It is
deliberately *activation-agnostic and all-layers* — describe-key answers
for every key, including chords owned by modes that are not active here,
and reports each layer separately with an `[active]` / `[inactive]` flag.

Which-key must answer the opposite question: what fires **here**, right
now, from the same composite the dispatcher walks (design §2, the one
correctness property). So WK.1 adds `continuations_with_context`, a
sibling that folds exactly as `lookup_with_context` does. The two share
the trie-level primitive and nothing above it.

Sharing the *rendering* would be the mistake to avoid: describe-key wants
the whole subtree with provenance, which-key wants the immediate children
with labels. Same trie, different questions.

## Slices

### WK.1 — Resolver ✅

`crates/lattice-keymap/src/trie.rs`:

	KeymapTrie::node_view(&self, prefix: &[KeyChord]) -> Option<NodeView>

`NodeView` is **owned**, not borrowed. It has to be: the composite trie
that `continuations_with_context` folds is a local temporary, so a view
holding `&TrieNode` could not be returned from it. Owned also keeps
`TrieNode` private, which it is today.

	NodeView { children: Vec<ChildView>, wildcard: Option<ChildView>,
	           terminal: Option<Arc<BoundCommand>> }
	ChildView { chord: KeyChord, binding: Option<Arc<BoundCommand>>,
	            descendants: usize }

`terminal` is the design's `d`-is-an-operator-and-a-prefix case → footer,
not a row. `descendants` drives `+N` for a `Prefix` entry.

`crates/lattice-keymap/src/registry.rs`:

	KeymapHandle::continuations_with_context(mode, chords, active_modes)
	    -> Option<NodeView>

Folds identically to `lookup_with_context` — same always-on fast path,
same overlay order — differing only in the terminal step.

`crates/lattice-keymap/src/which_key.rs`: `WhichKeyModel`, `Entry`,
`EntryKind`, `Sort`, `build_model`, and the four-rung label chain (§4.1)
+ the collation (§4.2).

Tests: continuations come from the trie not the catalog; a minor
shadowing a builtin shows the mode's label and the builtin does not also
appear; inactive modes absent; terminal-and-prefix in the footer;
wildcard yields one `{char}` row labelled from the wildcard subtree; each
label rung including `<unbound>`; collation stable across two builds from
a `HashMap`-backed trie.

### WK.2 — Grid ✅

`layout_grid(model, width, opts) -> Vec<String>` in `which_key.rs`. Pure;
no renderer type crosses in. Column-major, elastic label truncation with
a two-column floor, keys never truncated, `+N more` tail.

Tests: 40 / 80 / 120 / 200 columns; column-major order; the truncation
floor; `+N more` accounting; keys never truncated.

### WK.3 — Idle-gate primitive ✅

`crates/lattice-mode/src/idle_gate.rs` + `SubsystemBoot::idle_gate`.
RAII registration mirroring `TickCallbackRegistration`. The actor keeps
one pinned sleep reset to the earliest armed deadline.

`Editor::inline_diag_deadline` is deliberately NOT migrated (design §9) —
its arm decision runs inside `publish_render_state` and needs a
`CursorSettled` event that does not exist. The new registry runs beside
it; the actor grows one `select!` arm, not a rewrite of the existing one.

Tests (`lattice-mode`, unit): two gates armed → the earlier fires first;
disarm cancels; RAII drop deregisters; an unchanged minimum deadline
skips the reset.

### WK.4 — `PartialChordPending` ✅

Declared in `lattice-keymap` (every field is a keymap/protocol type),
published from `publish_render_state` when the tuple changes. The payload
**rides on the event** — tick callbacks run before the publish, so a
subscriber reading published state would arm one keystroke late.

### WK.5 — `PopupPlacement::PaneBottom` ✅

`lattice-core/src/ui/popup.rs` + the TUI's popup geometry in `render.rs`
+ GPUI's `popup_outer_dims_px` / `popup_inner_height_rows` in
`window.rs`. One patch, per the cross-renderer rule. Audit:
`grep -rn "PopupPlacement::PaneBottom" crates/lattice-ui-gpui/ --include="*.rs"`.

### WK.6 — The subsystem ✅

`crates/lattice-mode/src/modes/which_key.rs`: `install(boot)`, the six
options (§8), `which-key-mode` major on the popup buffer, subscribe →
stash → arm, gate handler → build → buffer write → `Effect::OpenPopup {
placement: PaneBottom, focus: Passive }`, and the dismissal set. No
`Editor::` method and no host `Action` variant — but TWO lines in the
install list rather than one; see the deviations below.

### WK.7 — Lifecycle tests ✅

`lattice-host` integration, per design §11. Each asserts **without
dispatching another key**.

### WK.8 — Docs + benches ✅

User page + `nav.toml` + sync; `BENCHMARKS.md` rows including the
keystroke-path `partial_chord_publish_unchanged`; ledger entry in
`implementation.md`; flip this table's icons.

### WK.9 — Key emphasis ✅

Follow-up after using it: the grid read flat. `layout_grid` now returns
`RenderedGrid { lines, spans }`; `WhichKeyMode::on_activate` maps the
spans onto `Style::HelpKey` / `Style::Markup` and publishes them through
`PendingSyntheticHighlights`, the same path magit's buffers use. No
renderer change — both peers already paint `ExtraHighlights`.

Design §6's two new theme elements were not added; see the design
fragment for why reusing `HelpKey` is the better answer.

Tests: spans cover the key and not its alignment padding; the header
prefix is spanned; `+N` is a distinct kind; no span points past its line
at any width; and — host-side — the spans actually reach the popup
buffer's `ExtraHighlights`, which is the only assertion that would have
caught the `ServiceRegistry` `T`-mismatch this slice nearly shipped.

## As-built deviations

Three places where the implementation departed from the design fragment.
Each is a fact about the tree the design did not have, not a change of
mind about the design.

**WK.6 — the install is two host calls.** Design §3 quotes the acid test:
"adding a subsystem touches the host in exactly that one place". Which-key
cannot, because of boot ordering. The MODE registry freezes at
`editor_boot.rs:733`; the COMMAND registry handle the label chain needs
for rungs 2–3 only exists after `freeze_command_registry` (line 1081). A
single call must sit on one side of that gap: register the mode and get
`None` for the command service — which degrades every label to
`<unbound>` *silently* — or resolve the services and panic registering
into a frozen mode registry. So `install(boot)` registers the mode early
and `wire(boot, grid)` wires the lifecycle late. The ownership half of
the acid test still holds: no `Editor::` method, no host `Action`
variant, no host-side handler body.

**WK.5 — the WIT enum gained a variant.** `PopupPlacement` crosses the
plugin boundary (`boundary_effect.rs`) and `wit/types.wit` carries a
mirror whose doc comment says so. `pane-bottom` was added there rather
than collapsed to `centered` at the crossing: a mirror that silently is
not one is a trap, and a placement reachable natively but not from a
plugin is an arbitrary gap in the canonical API (paramount #2). The
design fragment does not mention this boundary; the exhaustive match
found it.

**WK.7 — `fire_idle_gates` had to absorb the popup effects.** The first
run of the lifecycle suite had the gate firing and no popup.
`Effect::OpenPopup` is deliberately renderer-coupled: `apply_effect_host`
pushes it to `out.effects` for the peers' tail (pinned by
`open_popup_effect_routes_to_renderer_tail`). A gate fires on an
OFF-KEYSTROKE arm, where nothing drains that tail — the TUI peer has no
`signal_rx` consumer at all. So `fire_idle_gates` now absorbs
`OpenPopup` / `DismissPopup` host-side, which is the same fix and the
same reasoning as AW.4's `absorb_async_display_signals`. Any future idle
gate emitting a renderer-coupled effect inherits it.

The two tests that caught this are the two that assert without a
follow-up keystroke; the three that check arming state passed on the
broken build.
