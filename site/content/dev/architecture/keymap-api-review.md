+++
title = "Keymap API design review — 2026-06-02"
+++

# Keymap API design review — 2026-06-02

One-shot review after the K.1 → K.4 arc landed. Evaluates the
keymap substrate (`lattice-protocol::chord`,
`lattice-host::keymap_*`, `lattice-mode::keymap_entry`) against
the four paramount goals and the five decision heuristics in
`CLAUDE.md`. Companion to `keymap-architecture.md`: that doc
owns the design and contracts; this doc owns the honest
inspection of how the implementation holds up after the K-arc
churn.

## Inputs to the review

- `crates/lattice-protocol/src/chord.rs` — 760 LOC. `KeyChord`
  (8 bytes), `ChordPattern`, parser, formatter, `KeyMods`
  bitfield, `SpecialKey` enum.
- `crates/lattice-host/src/keymap_trie.rs` — 603 LOC. `KeymapTrie`,
  `BoundCommand`, `LookupResult`, `KeymapLayer`.
- `crates/lattice-host/src/keymap_registry.rs` — 1722 LOC.
  `KeymapHandle`, `KeymapRegistry`, capability gating, layered
  merge, per-keystroke composite fold (K.1.c).
- `crates/lattice-host/src/keymap.rs` + `chord.rs` — re-export
  shims for the K.2.1 / K.2.4.A.0.1 relocations.
- `crates/lattice-mode/src/keymap_entry.rs` — 664 LOC.
  Catalog + `keymap_entry!` macro + `default_keymap()`.
- `crates/lattice-host/src/keymap_mode_contributions.rs` —
  631 LOC. Mode-contribution resolver (`translate_mode_keymaps`).
- `crates/lattice-host/src/keymap_{normal,insert,visual,replace,
  terminal,help}.rs` — per-mode binding registration calls.
- `crates/lattice-ui-tui/benches/keymap.rs` — 557 LOC of
  criterion benches.

## Verdict at a glance

The keymap substrate is **structurally aligned** with all four
paramount goals. The K-arc tightened the architecture rather
than papering over it: the protocol relocation (K.2.1) gave
mode crates a dependency floor for chord values; the mode-
contribution path (K.2.4 + K.2.4.A.0) made declarative IS the
convention; the K.3.5 / K.4.1.b public APIs codified
"functionality backed by APIs" rather than "functionality
backed by binding chains."

Remaining friction is **performance bookkeeping** (the K.1.c
per-keystroke composite fold needs a bench to keep the budget
visible) and **drift opportunities** (`KeymapEntry::doc` is
`Option`; `KeymapRegistry::default()` exists as test-only dead
weight; the K.1.c minor-fast-path still allocates a sort vec
when there are no minors).

## Goal-by-goal evaluation

### Paramount #1 — Performance

**Hot path shape.** A user keystroke flows through:

```
KeyEvent
  → KeyChord (~few register ops; bench: keychord_from_event_*)
  → TranslateContext { keymap: &KeymapHandle, ... }
  → KeymapHandle::lookup_with_context(mode, &chords, active_minors)
       → ArcSwap::load (always_on)              ← wait-free
       → ArcSwap::load (minor_mode_tries)        ← wait-free
       → if active_minors.is_empty():
            always_on.by_mode[mode].lookup(chords)
         else:
            composite = KeymapTrie::new()
            composite.merge_over(always_on.by_mode[mode])
            for mode_id in active_minors:
                composite.merge_over(minors[mode_id][mode])
            composite.lookup(chords)
  → Action
  → handle_action(editor, action, out)
```

**Numbers from existing benches** (`docs/dev/operations/benchmarks.md`,
TUI `criterion`):

- `KeyEvent → KeyChord` plain: sub-100 ns.
- `keymap_trie_lookup_single`: ~17 ns.
- `keymap_trie_lookup_three_chord`: ~43 ns.
- `keymap_handle_lookup_single`: ArcSwap load + HashMap get +
  trie walk; bench measures end-to-end.
- `dispatch_translate_full_two_chord`: full `translate()`
  round-trip through the production-shape handle.

The one-frame ceiling (8.3 ms at 120 Hz) leaves ~8.3 ms of headroom; the
keystroke path consumes single-digit microseconds end to
end. Well within the ceiling.

**Open performance gap — K.1.c minor-mode composite fold.**

`lookup_with_context` does a full trie build via
`KeymapTrie::new() + merge_over(base) + merge_over(each minor)`
*every keystroke* when ≥1 minor mode is active. Before this
review the bench coverage didn't surface this shape; the
analytical estimate (one merge_overlay × N minors) put the
3-minor cost at ~1.5 µs.

**Measured (2026-06-02, criterion `--quick`):**

| Bench | Time | Notes |
|---|---|---|
| `keymap_handle_lookup_with_one_minor` | **980 ns** | 1 active minor, 1-chord |
| `keymap_handle_lookup_with_two_minors` | **1.07 µs** | 2 active minors, 1-chord |
| `keymap_handle_lookup_with_three_minors_three_chord` | **1.18 µs** | 3 active minors, 3-chord (worst realistic) |
| `keymap_handle_lookup_empty_minors_with_layers_registered` | **36.4 ns** | Fast-path: 3 minors registered, active=[] |

The worst realistic case (3 minors × 3-chord) lands at
**1.18 µs / keystroke** — 99.986% under the one-frame ceiling
(8.3 ms at 120 Hz). The fast-path branch (empty `active_modes`)
holds at ~36 ns even with three minor layers registered,
proving the `if active_modes.is_empty()` short-circuit in
`lookup_with_context` does its job: registered-but-inactive
minors don't tax buffers that don't use them.

**Conclusion: the K.1.c per-keystroke fold does not hamper
performance** at any plausibly-realistic active-minor count.
The memoized-composite-cache follow-up (R4) is correctly
classified as designated-future-work, not load-bearing. If a
future change pushes typical buffers above 3-4 active minors
OR the worst-case row regresses past ~10 µs, R4 promotes to
load-bearing; the bench in `BENCHMARKS.md` makes that
threshold visible to CI.

**Open performance gap — `lookup` always sorts.**

`KeymapHandle::lookup` (the no-context legacy entry point) is
called by every TUI input dispatch that doesn't yet thread
`active_modes` through (notably the completion-popup and
chord-capture short-circuits in `compute_normal_action`). It
loads `minor_mode_tries`, collects every key into a Vec,
sorts the Vec, and forwards. Even when the minor-map is
empty it still allocates and sorts a Vec of zero length —
trivial in absolute terms (~50 ns) but it's an unconditional
allocation on a path doc'd as "wait-free."

**Action:** branch *before* the Vec build:

```rust
let minors = self.registry.minor_mode_tries.load();
if minors.is_empty() {
    let always_on = self.registry.merged.load();
    return match always_on.by_mode.get(&mode) {
        Some(trie) => trie.lookup(chords),
        None => LookupResult::Unbound,
    };
}
let mut sorted: Vec<ModeId> = minors.keys().copied().collect();
sorted.sort();
self.lookup_with_context(mode, chords, &sorted)
```

Single-digit nanosecond saving but it preserves the contract
the docstring already promises.

**Future work — memoized composite cache.**

A `HashMap<(BindingMode, ActiveModesFingerprint), Arc<KeymapTrie>>`
keyed on the ArcSwap generation + sorted-mode-id-tuple would
make the K.1.c path zero-allocation in steady state (most
buffers don't churn their active_modes between keystrokes;
the cache would hit). Not urgent — sub-2 µs is fine — but
worth flagging in `keymap-architecture.md` §N as a designated
follow-up so it doesn't get rediscovered later as an emergency.

### Paramount #2 — Extensibility

**WIT is the canonical API; capability gating is in place.**

The `KeymapCapability` enum (`Full`, `User`, `MinorMode`,
`OwnedLayer { mode_id }`) mirrors the four WIT capability
variants the plugin host will surface. Every capability-gated
write API (`try_bind`, `try_unbind`, `try_push_layer`,
`try_pop_layer`) funnels through one `capability_allows`
check. The mapping from a WIT manifest's capability
declaration to a `KeymapCapability` value is a thin
translation step the plugin host will own — the registry's
job is just to enforce the layer scope, which it does.

**`OwnedLayer { mode_id }` matches emacs `(:map foo-mode-map ...)`
semantics.** A plugin or `init.rs` block that wants to add a
binding *only* while diff-mode is active obtains a capability
keyed on `ModeId::new("diff-mode")` and binds into the
`KeymapLayer::MinorMode(ModeId::new("diff-mode"))` layer. The
binding lives + dies with the mode's activation lifecycle
(idempotent-on-`ModeId` push via K.1.b). This is the cleanest
"mode-scoped binding" shape the keymap arc has produced.

**Programmatic chord dispatch (K.4.1.b) closes the
extensibility loop.** Before K.4.1.b, the only entry point a
plugin had into the chord pipeline was `dispatch_blocking`,
which takes a `CommandInvocation` — i.e. plugins could call a
command but not simulate a key press. With `Editor::dispatch_chord`
they can:

- Build chord sequences programmatically (testing, macros,
  scripting).
- Replay recorded chords (macros store `CommandInvocation`,
  but other replay shapes — keystroke-mimicked tutorial mode,
  vim-style `:normal` — want the chord shape).
- Inject from non-keyboard input sources (e.g. a future MIDI-
  controller plugin or voice-command bridge feeding chord-
  shaped commands into the same translate + handle_action
  pipeline the TUI uses).

The shape is intentional: `partial_chord` is `&mut Vec` rather
than living inside `Editor`, so the caller controls multi-key
sequence state. App still uses `translate` directly for user
keyboard (it has more App-side surface state — picker overlay,
snippet, terminal mode — to thread).

**K.3.5 codified the principle** that any feature wired to a
keymap binding should be backed by a public API the binding
*calls*, not by a binding that re-implements the API's
behaviour inline. `arm_missing_arg_prompt` is the lesson: the
help-prefix binding shape (`<C-h>k` → `:describe-key <chord>`)
needed the same missing-arg cmdline-arming behaviour that
`:describe-key` (typed bare) did; the fix was a shared public
method, not a parallel implementation in `keymap_help.rs`.

**Action — `KeymapEntry::doc: Option<&'static str>` should be
non-optional.** Today the macro accepts entries with no `doc`
field, which produces silently-empty `:describe-key` output.
Plugin authors will absolutely write bindings without doc
strings unless the type forces them; make it `&'static str`
and default to `""` only inside `keymap_entry!` with a
`#[deprecated]` warning if the field is omitted. (Or: keep
`Option` but assert at boot that no `Builtin`-layer entry has
`None`.) This is paramount-#2 hygiene — extensibility isn't
just "can plugins bind?", it's "can users discover what a
plugin's bindings do without reading source?"

**Action — help-prefix bindings live in `keymap_help.rs` at
`Builtin`** rather than via the mode-contribution path. This
was deliberate (built-in vim grammar IS the floor; everyone
shadows it) but `keymap-architecture.md` should cross-
reference the K.3 decision explicitly so future readers
don't see it as a violation of K.2.4's "modes own their
keymaps" convention. It isn't — the help *commands* are
built-ins by design — but the asymmetry deserves one
paragraph in the design doc.

### Paramount #3 — Vim modal editing

**The grammar IS the public API.** Operators, motions, text
objects, ex-commands all flow through one `CommandInvocation`.
The keymap doesn't know about "operator vs motion" — it just
binds chord paths to `CommandInvocation`s, and the grammar's
parser composes them. Adding a new motion is one
`register_simple` + one keymap_entry row.

**Extensible motions / text objects.** A future tree-sitter
motion (`]f` for next-function, `]c` for next-class) plugs in
exactly the way `motion:next-paragraph` does today:

1. Register the motion's `CommandInvocation` with
   `register_simple` (one row).
2. Append a `keymap_entry!` row in either the catalog or a
   mode-contribution table.
3. Done.

No fork in the dispatcher; no new dispatch arm.

**Macros record `CommandInvocation` sequences, not chords.**
Confirmed by the macro module (`crates/lattice-host/src/macros.rs`).
Replay survives keymap rebinds — the user changes `dd → kill-line`,
their old `dd`-recorded macro still runs `motion:delete-line`.
This is exactly the §5.2 unification.

**Open question — wildcard captures + composability.** The
trie's `ChordPattern::CharLiteral` captures a single bare
char into `captured: Vec<char>`. The captured chars need to
reach the `CommandInvocation` as arguments — currently this
is wired via `BoundCommand::command` carrying an invocation
shape that consumes the captures. Inspect: the
`MissingArgPrompt` path (K.3.5) is the missing-piece for
arg-shape mismatches; this looks consistent but a test
exercising `f<char>` with the `find_char` command's `:char`
arg would harden the contract. The trie tests cover wildcard
capture mechanically; the end-to-end test that the captured
char *reaches the command* is implicit.

**Action:** add an integration test that binds
`[lit('f'), CharLiteral]` to a `find_char` command with one
`:char` arg slot, dispatches `f x`, and asserts the
invocation's `args[0]` is `Args::Char('x')`. This pins the
wildcard-to-arg plumbing as a regression target.

### Paramount #4 — Asynchronicity

**Reads are wait-free.** `ArcSwap::load` plus trie walk; no
mutex; no I/O. The K.1.c mode-aware path adds a
`ArcSwap::load` (the minor-tries cell) plus the per-tick
fold, but no synchronisation primitive. A write concurrent
with a read sees the read finish on the pre-write Arc; the
write installs the new Arc; the next read picks up the new
state.

**Writes are mutex-routed.** `inner.lock()` covers the per-
layer mutation + merged-cell rebuild; no I/O inside the lock.
The lock is uncontended in practice (writes are infrequent:
boot enumeration, mode push/pop on UI events, `:bind` /
`:unmap` user commands). The unwrap (`.expect("registry
mutex")`) on every write is fine — mutex poisoning means a
panic happened inside a prior write, at which point we want
the next write to abort loudly rather than silently degrade.

**UI thread does no work the dispatcher doesn't authorise.**
Confirmed: no I/O, no parsing, no shaping in the keymap path.
The keymap registry sits in `lattice-host` (the editor
process); the TUI and GPUI adapters are read-only consumers.

**Open question — `KeymapRegistry::default()` is dead weight.**
Constructs an unwrapped instance; can't be used by `KeymapHandle`
(which expects `Arc<KeymapRegistry>`); exists only to satisfy
the `Default` derive. Remove it; tests that want a registry
go through `KeymapHandle::new()` (which already does the
`Arc` wrap). One paragraph less of "is this called?"
confusion.

### Decision heuristics

**#1 Best long-term fit beats easy implementation.** The
K.2.1 protocol relocation is the canonical example — moving
chord primitives to `lattice-protocol` was more LOC churn
than the alternative (keep them in `lattice-host`, have mode
crates depend on host) but it gives mode crates the
dependency floor they need. The user surfaced this directly;
the slice landed without backwards-compat shims for the old
import paths (just re-export shims with a "retire next
release" note). Right call.

**#2 Evaluate against paramount goals, not against other
editors.** The five-layer model is *informed* by emacs's
keymap-layering and vim's `:nmap <buffer>` but justified by
the paramount goals: layer scope = paramount-#2 capability
boundary; per-keystroke fold = paramount-#3 mode-scoped
composition; ArcSwap = paramount-#1 + #4 wait-free reads.
The doc names which paramount goal each design choice
protects (mostly via inline rationale comments). Good.

**#3 User-suggested options as input, not the menu.** The
K.2.4 / K.2.4.A.0 carving is the example. User asked "if we
have `keymap_entry!` can we use that within modes?" — the
right answer wasn't yes/no but "let's pull the catalog into
`lattice-mode` so modes contribute via the same declarative
shape the builtin catalog uses." That's the K.2.4.A.0.1
relocation. Slice planner did this; review confirms it.

**#4 Confirm the plan before non-trivial work.** Both K.3
(Option A vs B, Option 2 vs 1) and K.4.1 (foundation slice
vs full chord-dispatch suite) hit the "ask first" rail. The
K.4.1 case is the cleanest example: original scope was 12
chord-dispatch tests; investigation surfaced that the helper
they needed didn't exist; pivoted to K.4.1.a + K.4.1.b with
a public `dispatch_chord` API as the K.4.1.b deliverable.

**#5 Non-trivial design changes ship four artefacts
together.** Mixed. The K-arc shipped:

- Code: ✅ in every slice.
- Tests: ✅ for K.1.b, K.1.c, K.2.4, K.2.4.A.0, K.3.0-K.3.5,
  K.4.1.a / K.4.1.b. The K.2.5 multibuffer-mode contribution
  has unit tests + the K.4.1.b integration tests now exercise
  the full keymap → translate → handle_action loop on a
  multibuffer-mode buffer.
- Docs: ✅ for K.2 (keymap-architecture.md §11.2.1 + §11.2.2);
  ✅ for K.3 (§12.3 with the Option 2 decision); ✅ for K.4
  (multibuffer-is-a-regular-buffer.md K.4 audit). One gap:
  `keymap-architecture.md` doesn't yet have a §"K.1.c per-
  keystroke fold" section codifying the mode-aware lookup
  contract. K.1.c shipped with the doc-comment on
  `lookup_with_context` but no design fragment.
- Benches: ⚠ partial. The keystroke-path benches cover
  `translate_full_*` end-to-end + trie + handle, but the
  K.1.c `lookup_with_context` shape with active minors is
  not benched. K.4.1.b's `dispatch_chord` is not benched.
  Trivial writes (`bind`, `push_layer`) are not benched
  beyond the merge_overlay primitive. These are gaps the
  *implementation* shipped without; this review's
  recommendation is to close them with one criterion file
  update.
- Error handling: ✅ `KeymapError::CapabilityDenied` +
  `InvalidChord`; mutex poisoning panics by design; the
  dispatch_chord drain loop has the right termination
  condition.

So heuristic-#5 compliance is **3 of 4 artefact axes
complete**; the bench axis needs the gap closed. This
review's "Benchmarks" task (next slice) is the close.

## Concrete follow-ups (no scope creep into this review)

| # | What | Where | Heuristic |
|---|------|-------|-----------|
| R1 | ~~Add `lookup_with_context` benches~~ ✅ landed 2026-06-02 (4 rows; worst case 1.18 µs / 3 minors / 3-chord, 99.985% under budget) | `crates/lattice-ui-tui/benches/keymap.rs` | #5 (bench axis) |
| R2 | Add `dispatch_chord` end-to-end bench (deferred — translate + handle_action already benched separately; dispatch_chord is the sum) | same | #5 |
| R3 | Branch `lookup` before sorting an empty minors map | `keymap_registry.rs` line ~365 | #1 (perf) |
| R4 | Memoized composite cache (designated follow-up, not now) | `keymap_registry.rs` | #1 future |
| R5 | `KeymapEntry::doc: Option → &'static str` (or boot-time assertion) | `lattice-mode/src/keymap_entry.rs` | #2 (discoverability) |
| R6 | Cross-reference K.3's "built-ins live at Builtin layer" decision | `keymap-architecture.md` §12 | #5 (docs) |
| R7 | §"K.1.c per-keystroke fold" design fragment | `keymap-architecture.md` | #5 (docs) |
| R8 | Wildcard-capture → command-arg integration test | new `keymap_wildcard_arg.rs` test | #3 (vim grammar) |
| R9 | Remove `KeymapRegistry::default()` dead Default impl | `keymap_registry.rs` | hygiene |

R1 + R2 are the immediate follow-up — the user's directive
named benchmarks explicitly. Rest are queued.

## Net assessment

The K-arc moved the keymap substrate from "works, but the
mode story is implicit" to "works, and the mode story is the
convention." The four paramount goals are protected by
structurally-aligned mechanisms (ArcSwap wait-free reads,
capability-scoped writes, declarative mode contributions,
public chord dispatch). The decision heuristics show up in
the slice-by-slice trace: K.2.1 chose long-term fit over
ease (#1); K.2.4 surfaced an option the user hadn't proposed
(#3); K.3 + K.4.1 paused before non-trivial work to confirm
(#4); the artefact discipline (code + test + doc) held on 3
of 4 axes — only the bench axis needs closing, and the
follow-up that closes it is the next slice.

Nothing in this review surfaces a paramount-goal violation
or a heuristic-#1 reversal that demands a re-slice. The
findings are bench gaps + cosmetic cleanups + one minor perf
sharpening on a path well inside budget. Ship-ready by the
honest standard.
