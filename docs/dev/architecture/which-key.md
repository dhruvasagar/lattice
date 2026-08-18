# Which-key

Authoritative design for Lattice's pending-chord discoverability
surface: hold a prefix, and after a short idle delay a popup shows what
can come next, derived from the *live composite keymap* the dispatcher
itself walks.

This document is a *companion* to `design.md` (§5.2 modal editing
engine, §5.11 introspection and help), to `keymap-architecture.md` (the
layer model and the trie this reads), and to `popup-api.md` (the
content-agnostic overlay it renders through).

Sequencing, slice IDs and status live in
`docs/dev/operations/slice-plans/which-key.md`.

## 1. Why this exists

Paramount goal #3 makes the vim grammar the public command API, and
paramount goal #2 lets every mode and plugin extend it. Those two
together have a consequence nobody wrote down: **the command surface
grows without bound, and nothing scales discoverability with it.**

`:describe-bindings` (`<C-h>K`) is the closest existing answer, and it
has two problems. It requires knowing to ask — no help arrives at the
moment you have actually forgotten something, mid-chord, with `<C-w>`
already pressed. And it is *wrong*: it composes its answer from the
static `KeymapEntry` catalog plus each active mode's declared
contributions, not from the merged trie. A mode that shadows a builtin
chord is not reflected, so the view can advertise a binding that will
not fire.

Which-key answers the question at the moment it is asked, and answers
it from the trie.

## 2. The one correctness property

> The popup is derived from the same composite trie the dispatcher
> walks. Never from the static catalog.

`KeymapHandle::lookup_with_context` builds a per-keystroke composite:
the cached always-on merge (`Builtin + MajorMode + User + Buffer`),
then each active mode's gated trie overlaid in activation order, so
minors win over the major and later minors win over earlier ones
(`keymap-architecture.md` §2; `registry.rs:625-659`).

`continuations_with_context` folds **identically** — the same
composition, differing only in the terminal step, where `lookup`
resolves a binding and `continuations` reports a node's children. Any
other construction reintroduces the `:describe-bindings` bug in a more
visible place, and the regression test for it is explicit: activate a
mode that shadows a builtin chord, assert the popup shows the mode's
label and that the builtin does not also appear.

## 3. Ownership

Four pieces. No new crate, and no `Editor::` method added anywhere —
`subsystem_boot.rs` states the acid test this design is held to:

> Adding a subsystem touches the host in exactly that one place and
> zero host internals (no `Editor::` method, no host `Action` variant).

| Piece | Home | Responsibility |
|---|---|---|
| Resolver | `lattice-keymap/src/which_key.rs` | Prefix → continuation model; grid layout. Pure, sync. |
| Idle-gate primitive | `lattice-mode/src/idle_gate.rs`, `SubsystemBoot::idle_gate` | Generic armed-deadline registry. |
| Subsystem | `lattice-mode/src/modes/which_key.rs` | `install(boot)`, options, arming, `Effect::OpenPopup`. |
| Placement | `lattice-core/src/ui/popup.rs` + both renderers | `PopupPlacement::PaneBottom`. |

The split follows the substrate-vs-mode-helper rule in `CLAUDE.md`:
the resolver's only consumer is which-key's own handler, so it is a
**helper function in the owning crate**, not a `Document` trait method
and not host machinery. `lattice-mode` already depends on
`lattice-keymap` and on `lattice-config`, so the design adds no
dependency edges.

Heuristic #6 governs the crate question: which-key *extends* the
keymap rather than introducing a new mechanism, so it lives in the
crate that already owns the trie, the layers and the resolution.

## 4. The data model

```
KeymapTrie::continuations(&[KeyChord]) -> Option<NodeView>

NodeView {
    children: Vec<(KeyChord, &TrieNode)>,
    wildcard: bool,
    terminal: Option<&Arc<BoundCommand>>,
}
```

`None` when the prefix is unbound. `terminal` is `Some` for a node that
is *both* bound and a prefix — vim's `d`-is-an-operator-and-a-prefix
case — which the popup reports in its footer rather than as a row.

```
WhichKeyModel {
    prefix: Vec<KeyChord>,
    mode: BindingMode,
    entries: Vec<Entry>,
    wildcard: Option<Entry>,
    truncated: usize,
}

Entry { chord: KeyChord, label: String, kind: Terminal | Prefix(count), layer: KeymapLayer }
```

### 4.1 Label resolution

Four rungs, first hit wins. The chain is **total** — the last rung
always produces a string, so a label is never blank and the path never
panics:

1. the static `KeymapEntry.doc` matching this full chord path in this
   binding mode (the curated one-liners),
2. the `CommandRegistry` doc for the bound `CommandId`,
3. the command's registered name,
4. `<unbound>`.

`Prefix` entries have no bound command and render `+N`, which is
`which-key.nvim`'s convention for an unlabelled prefix. This is why
prefix/group labels can be deferred without structural cost: the entry
kind already carries the count, and a label slot can be filled later.

A wildcard node resolves its label from the *wildcard subtree's*
binding, so `f` renders one row — `{char}  find char forward` — with no
new metadata anywhere. Rendering an empty grid there instead would read
as a broken popup, which is the failure this case exists to prevent.

### 4.2 Ordering

Under the default `which-key.sort = key`, a fixed collation: digits,
lowercase, uppercase, punctuation, special keys, then modifier-bearing
chords. Under `sort = label`, entries order by label with that same
collation as the tiebreak for equal labels.

An explicit total order is required either way, because `children` is a
`HashMap` — without one the grid would reshuffle between openings of
the same prefix, which is a worse discoverability surface than no popup
at all.

## 5. Lifecycle

### 5.1 The idle-gate primitive

```
SubsystemBoot::idle_gate(name, handler) -> IdleGateHandle
IdleGateHandler = Box<dyn FnMut() -> Vec<Effect> + Send>
IdleGateHandle::arm(tokio::time::Instant) / disarm()
```

The actor keeps **one** pinned sleep, reset each loop iteration to the
earliest armed deadline (or an hour out when none is armed — the
existing disarm idiom); on fire it runs every gate whose deadline
elapsed, applies their `Effect`s, republishes, and pings
`paint_request`. RAII registration mirrors `TickCallbackRegistration`,
so a deactivated subsystem contributes no timer.

This is the third instance of a shape this codebase has twice decided
is right. `tick_callback.rs`'s module doc names the alternative as the
smell it exists to kill:

> rather than adding an `Editor::drain_<x>` method + an
> `Option<Receiver>` field per subsystem, a mode owns its channel and
> registers a closure that drains it.

A deadline field per subsystem is that same smell in the time domain,
and `Editor::inline_diag_deadline` is the existing instance of it. See
§9 for why that migration is sequenced separately rather than bundled.

Incidentally the primitive should be marginally *cheaper* than today:
the actor currently resets the sleep unconditionally every iteration,
where a registry can skip the reset when the minimum deadline has not
moved.

### 5.2 Arming

`publish_render_state` publishes `PartialChordPending { chords,
binding_mode, active_modes, pane_width }` when the tuple changes.
Which-key subscribes, stashes the payload, and arms its gate.

The event type is declared with `register_event!` in `lattice-keymap`
— every field is a `lattice-keymap` or `lattice-protocol` type, and the
crate that owns the chord vocabulary is the honest home for an event
about chords. The host publishes it; which-key is one subscriber among
possible future ones.

**The payload rides on the event rather than being read back**, and
that is load-bearing for correctness: tick callbacks run *before* the
publish in both actor arms, so a design where which-key read the
published state would observe the previous keystroke's prefix and arm
one keystroke late.

`pane_width` on the event is what keeps §6 small. Because the width is
known at build time, the grid is laid out in `lattice-keymap` and the
popup's content is ordinary buffer text — everything-is-a-buffer holds,
no new render model crosses into either peer, and the column algorithm
is unit-testable with no renderer at all. The cost is that a pane
resize while the popup is up leaves a stale grid, so resize dismisses
it. For a transient hint that is the right trade.

### 5.3 Firing and dismissal

The gate elapses, the handler builds the model from its stashed payload
and its captured `KeymapHandle`, and emits the buffer write plus
`Effect::OpenPopup { name: "*which-key*", mode_id: "which-key-mode",
placement: PaneBottom, focus: Passive }`.

`PopupFocus::Passive` is State A in `popup-api.md` §4.1: the document
keeps focus, the caret, and the modal state. **Every keystroke
continues to flow to the trie unchanged.** This is the property that
makes the feature safe — see §7.

Dismissal: the chord resolves or aborts (the same event fires with
empty `chords` → disarm + close), the buffer or focus changes, the pane
resizes, or `<Esc>`. While the popup is already visible a *growing*
prefix re-renders immediately without re-arming — the delay is paid
once per chord, not once per key, which is the difference between
which-key and a stutter.

## 6. Layout and placement

`layout_grid(entries, width, opts) -> Vec<String>` in `lattice-keymap`.
A pure function returning plain text; no renderer type crosses into it.

- Cell is `{key}{gap}{label}`; the key column is sized to the widest
  key, the label column to the widest label.
- `columns = clamp(1, (width - margins) / (cell + gap), max-columns)`.
- **Column-major fill** — down, then across. Row-major would place
  `a b c` across the top and `d e f` on row two, defeating a scan for a
  letter in a sorted list. `ls` and emacs `which-key` fill column-major
  for the same reason.
- Elastic truncation: when the natural cell width yields fewer than two
  columns, truncate *labels* with `…` until two fit, then stop. **A key
  is never truncated** — a wrong key is worse than a missing label.
- Overflow past `max-height` becomes a `+N more` tail row, not a
  scrollbar. The popup is a hint, not a browser.
- The header is the prefix in vim notation; the footer carries
  `+N more` and, when the prefix node is also terminal, the note that
  it is bound on its own.

`PopupPlacement::PaneBottom` is a new variant: full width of the active
pane, anchored to its bottom edge, height = content + border, capped by
`which-key.max-height` and hard-capped at half the pane so it can never
swallow the buffer. Bottom-anchored full-width is what emacs
`which-key` and `which-key.nvim` both do; per the UX-convention rule
that is the muscle-memory default, and it is also the only placement
where the column count is predictable.

Per the cross-renderer rule the variant lands in one patch: the TUI's
popup-geometry branch in `render.rs`, and GPUI's `popup_outer_dims_px`
/ `popup_inner_height_rows` in `window.rs`. End-of-slice audit:
`grep -rn "PopupPlacement::PaneBottom" crates/lattice-ui-gpui/ --include="*.rs"`
— an empty grep means GPUI was missed.

Keys and labels want distinct faces or the grid reads flat: two theme
elements (`which-key.key`, `which-key.label`) applied as spans by a
`which-key-mode` major on the popup buffer, falling back to the
existing popup faces when unregistered. **Unverified:** that the span
mechanism reused here is the one help-mode uses for links. Confirm
during the slice; if it is not, monochrome v1 is acceptable and
per-cell styling becomes a follow-up.

## 7. Key routing, and why the popup is passive

The popup owns no keystrokes. Every key continues to resolve against
the trie exactly as it does with no popup open.

The alternative — a transient-style takeover with `<C-n>` / `<C-p>`
navigation — was rejected on paramount goal #3. Its navigation keys
would shadow real continuations on a per-prefix basis: hold `<C-w>` and
the continuation set contains `n` and `p`; hold `g` and it contains
`j`, `k`, `e`. A hint that changes what a chord does is a deviation
from vim that has not been chosen, and one that fails *differently* for
every prefix is the worst shape of that failure.

An opt-in escalation — one key promoting the same continuation list
into the existing transient/picker, where navigation is safe because it
was asked for — is the intended follow-up. It is deliberately **not**
in v1; see §9.

## 8. Options and failure modes

Declared with `lattice_config::options!` in `which_key.rs`.

| Option | Default | Note |
|---|---|---|
| `which-key.enabled` | `true` | `:which-key-toggle` flips it, mirroring `:context-toggle` |
| `which-key.delay` | `300` ms | `0` = immediate |
| `which-key.max-height` | `12` rows | hard-capped at half the pane |
| `which-key.max-columns` | `6` | |
| `which-key.modes` | `normal,visual,operator-pending,insert` | which binding modes participate; unknown ids are skipped with a `debug!` line, following the `dashboard.sections` precedent |
| `which-key.sort` | `key` | or `label`; see §4.2 |

Failure modes are log-and-skip, never panic, all at `debug!` — this is
keystroke-adjacent, and `info!` fans out to `*messages*`:

- the prefix vanished during the delay (a mode deactivated, `:map`
  rebuilt the trie) → disarm, no popup;
- zero continuations after filtering → no popup, rather than an empty
  box;
- a `CommandId` missing from the registry → the label chain falls
  through to `<unbound>`; the chain is total by construction;
- a pane narrower than ~20 columns → suppress, because a single column
  of truncated labels is worse than nothing;
- popup-open failure → log, disarm.

## 9. Scope boundaries

Three things are deliberately outside v1. Each is deferred with a
reason, not dropped.

**The inline-diagnostic gate migration.** `Editor::inline_diag_deadline`
is the bespoke instance of §5.1's primitive and should not survive.
But its arm decision runs *inside* `publish_render_state`, reading
`config`, `modal` and `cursor.line`, and there is no cursor-moved typed
event anywhere in the tree — `lattice-runtime`, `lattice-lsp` and
`lattice-mode` were all checked. Finishing the migration therefore
means publishing a `CursorSettled { line, modal }` event for the LSP
subsystem to subscribe to, plus surgery on working code. That is a
tracked follow-up slice, not a prerequisite: gating a discoverability
feature behind an LSP refactor is backwards.

**Escalation into the transient.** Because the popup is passive, keys
go to the document and no mode layer can claim the escalation key. What
it actually needs is the keystroke that would *abort* the pending chord
— today a silent no-op — which means a second generic primitive
(`SubsystemBoot::unbound_chord_handler`, consulted by dispatch on
`Unbound` with a non-empty partial chord). That primitive is
defensible on its own merit, with "did you mean `gd`?" as an obvious
second consumer, but building it before the grid has been used is
guessing at whether the menu is wanted.

**A root listing.** `:which-key` with nothing pending would list every
top-level binding. Explicitly not on idle: a popup appearing while the
user sits reading a file violates the keystroke UX contract.

## 10. Paramount-goal alignment

**#1 Performance.** Nothing expensive is on the keystroke path. An
ordinary keystroke pays a tuple compare that short-circuits on two
empty slices — single-digit ns. A *prefix* keystroke pays the payload
build, one typed-event publish and one `arm()`: ~100–250 ns, roughly
one more `dispatch_translate_full_operator_motion` (168 ns, measured),
and **~0.003% of a 120 Hz frame**. For calibration, IN.2 predictive
indent ships 8.1 µs on this path — ~0.1% of a frame — and was accepted;
which-key's worst case is ~30× smaller and lands only on prefix keys.
The model build (~2–10 µs) runs on the actor thread 300 ms after the
user stopped typing, on no latency path at all. The UI thread does
nothing new.

**#2 Extensibility.** Every mode's and plugin's contributed chords
appear automatically, because the resolver reads the composite trie
rather than any registration-time list. A plugin that binds a chord
gets which-key coverage with no additional work.

**#3 Extensible vim modal editing.** Strict vim semantics are
preserved exactly: the popup is passive, so no chord's meaning changes.
Operator-pending coverage walks the `OperatorPending` trie, which is
where the grammar is least discoverable and the feature pays most.

**#4 Asynchronicity.** The delay is an armed deadline on the editor
actor, never a UI-thread timer. The popup reaches the screen through
the actor's fire-republish-`paint_request` path, so it appears without
a keypress — the failure mode `CLAUDE.md`'s async pitfall exists to
prevent, and the one the lifecycle tests assert against directly.

**UX (the higher court).** The popup changes pixels the user did not
edit — but only after they held a prefix and waited, which is a
user-initiated request for help, and it is dismissable and
configurable. Nothing in the buffer moves; the overlay is anchored to
the pane's bottom edge.

## 11. Testing strategy

Resolver (`lattice-keymap`, unit): continuations come from the trie and
not the catalog; **a minor mode shadowing a builtin chord shows the
mode's label and the builtin does not also appear**; inactive modes
absent; a terminal-and-prefix node reported in the footer; a wildcard
node yields one `{char}` row labelled from the wildcard subtree; each
of the four label rungs including the `<unbound>` terminal; collation
stable across two builds from a `HashMap`-backed trie.

Grid (`lattice-keymap`, unit): 40 / 80 / 120 / 200 columns;
column-major order; the two-column truncation floor; `+N more`
accounting; keys never truncated.

Lifecycle (`lattice-host`, integration). Per the async pitfall these
**must assert without dispatching another key** — a test that presses
something first passes on the broken version too:

- press `g`, advance the clock past the delay, assert the popup is
  visible with no further keystroke;
- `g` then `d` before the delay → the popup never appeared and `gd`
  fired;
- popup up, prefix grows → re-renders immediately, no second delay;
- chord resolves, and `<Esc>` → popup gone;
- `which-key.enabled = false` → the gate never arms;
- `d` shows the `OperatorPending` trie's motions, not Normal's;
- Insert `<C-x>` shows continuations; dropping `insert` from
  `which-key.modes` suppresses it.

Idle-gate primitive (`lattice-mode`, unit): two gates armed → the
earlier fires first; disarm cancels; RAII drop deregisters; an
unchanged minimum deadline skips the reset.

## 12. Benchmarks

Extending the existing keymap bench target:

- `which_key::continuations_{no_modes,three_minors}` — should track the
  existing `keymap_handle_lookup_*` rows;
- `which_key::layout_grid/{10,40,80}` at 120 columns;
- `which_key::partial_chord_publish_unchanged` — **the keystroke-path
  row**, which must stay in the low-ns range, recorded in
  `BENCHMARKS.md` beside IN.2's 8.1 µs for calibration.

## 13. Adjacent finding: the composite-fold cost

Not caused by this feature, but surfaced while sizing it, and relevant
because `continuations_with_context` becomes a second caller.

```
keymap_trie_lookup_{single,two,three}_chord        23.4 / 60.5 / 76.3 ns
keymap_handle_lookup_with_{one,two,three}_minors    1.78 / 1.93 / 2.21 µs
```

A **76× multiplier at one minor mode, 29× at three**, on the keystroke
path today. The cause is `lookup_with_context` (`registry.rs:641-658`)
rebuilding the composite trie by full recursive `merge_over` on every
keystroke whenever any mode is active — which, with magit, diff,
snippet or emacs-keys in play, is the normal case rather than the
exception. At 2.21 µs it is 0.027% of a frame, so it is not urgent; but
it is pure recomputation of a value that changes only when the
active-mode set or the trie changes.

The fix serves both callers: cache the composite keyed by
(active-mode-set, binding-mode) behind the same `ArcSwap` idiom
`merged` already uses, invalidated by `derived_dirty`. **This stands on
its own merit and is not a which-key prerequisite** — recorded here so
it is not lost.

## 14. Cross-references

- `keymap-architecture.md` — the layer model, the trie, `resolve_trace`.
- `popup-api.md` §4.1 — `PopupFocus::Steal` vs `Passive` (State A/B).
- `boot-composition.md` §3 — `SubsystemBoot`, and why async results
  must reach the screen without a keypress.
- `magit.md` §8 — `TransientSpec`, the escalation target in §9.
- `docs/dev/operations/slice-plans/which-key.md` — sequencing.
