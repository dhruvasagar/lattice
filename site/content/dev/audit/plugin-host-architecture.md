+++
title = "Plugin host architecture — conformance audit"
+++


**Audited 2026-07-12**, prompted by "a thorough review before proceeding past
PH7.7a — does the plugin host conform to our design philosophy, and do all
decisions fit the decision heuristics?" The audit measures the plugin host
(design fragment `../architecture/plugin-host.md`, slice plan
`../operations/slice-plans/plugin-host.md`, crate `lattice-plugin-host`, `wit/`)
against the four paramount goals, the five heuristics, and the **stated purpose
of the extension system**:

> Give developers a mechanism to build extensions that **extend the editor's core
> ideas** (grammar), **provide modes** (conforming to the mode architecture), and
> **build rich user interfaces by leveraging infrastructure already in the core** —
> not by rebuilding it.

**Verdict: the spine is sound and strongly heuristic-aligned.** The design's
central bet — *fast path stays native, everything else registers into the same
registries via the same `SubsystemBoot` seam, zero parallel plugin universe* — is
the right one and is honoured concretely today (PH7.0–7.6 + 7.7a). The findings
below are not "the architecture is wrong"; they are **sharp edges** where a landed
decision (F1), an unproven integration (F2, F5), an under-designed seam (F3, F4),
or documentation drift (F7) will bite the next slice or the first real plugin if
left unnamed. Three are load-bearing enough to resolve before their slice lands.

---

## The three invariants the plugin host rests on

An audit names its invariants so the next change knows what it must not break.

### I1 — No parallel path. Plugins register into the *same* registries.

A plugin is just another subsystem that installs through the same
`install(&mut impl SubsystemBoot)` seam `lattice_lsp` / `lattice_diff` /
`lattice_multibuffer` already use (fragment §5; `subsystem_boot.rs:51`). Picker,
completion, grammar, events, modes, config all go through the existing
`Arc<dyn Trait>` insert paths. **Acid test: a new plugin adds ZERO `Editor::`
methods and ZERO new `Action`/`Effect` variants.** This is "everything is a
buffer / one dispatch" applied to extensibility, and it is the single most
important thing the design gets right. A parallel plugin registry is explicitly
rejected (§11). *If a future seam ever needs a bespoke `Editor::do_plugin_x`, the
seam is wrong, not the rule.*

### I2 — WASM is off the keystroke hot path — with exactly one bounded exception.

Fragment §1/§3: "we earn extensibility *around* the hot-path state machines via
traits, never *inside* them via WASM dispatch"; "there is no synchronous path
from UI input to plugin code." This holds for **every seam except grammar**
(picker, completion, events, decorations, ui, config are all async / off-render /
data-emitting). Grammar (PH7.7) is the deliberate exception — a motion must
resolve **synchronously** to compose with its operator — and is therefore the
**one synchronous-on-keystroke seam**, bounded by fuel + epoch. The invariant is
best stated in its true, exact form: **the renderer never calls WASM (absolute);
the actor calls WASM synchronously only for grammar, under a Reflex-class budget;
everything else is async.** See F1 — the fragment does not yet say this, and the
budget is not yet Reflex-class.

### I3 — The guest emits data; the host owns the draw call, the dispatch, and the async plumbing.

Every §4 boundary resolution reduces to this: closures → guest exports the host
calls by id (§4.1); `Future`/`Stream` → host owns the loop, guest returns batches
(§4.3); the closed `Effect` enum crosses as a typed variant, never an opaque blob
(§4.4); decorations → per-line data the renderer reads, never draw calls (§7).
This is what keeps paramount #1 structural rather than disciplinary. *Any seam
that lets a guest issue a draw call, hold a keystroke-hot-path pointer, or hand
the host a tokio type violates I3.*

---

## Goal-conformance matrix

| Stated goal | Seam | State | Conformance |
|---|---|---|---|
| Extend core ideas (grammar) | `grammar` (PH7.7) | 7.7a landed (mirrors + projection) | **Strong** in shape (same `register_*`, `SourceLayer::Plugin`, same `CommandRegistry`). Sharp edges: **F1** (sync-path budget), **F4** (tree-sitter reach). |
| Provide modes (mode architecture) | `modes` (PH7.11) | stub `interface modes {}` | **Intended, unproven.** The integration point for every other seam; the mode-ownership acid test cannot be validated until a real plugin mode exists. **F2**, **F5**. |
| Rich UI on core infra | `ui` + `decorations` (PH7.9) | stubs | **Right philosophy** (emit data, reuse buffer/popup/decoration infra, no per-frame WASM), **least designed.** **F3**. |

The reference plugin (`fuzzy-finder`, PH7.4) validates **only the picker seam**.
Goals 2 and 3 have no landed validator yet — which is legitimate Phase-7 scope
(§12 defers modes-as-components + the plugin manager to Phase 8), but means the
two goals the user names most explicitly are the two least de-risked.

---

## Findings

Each finding is mapped per the CLAUDE.md heuristic rule.

### F1 (High) — The grammar seam is synchronous-on-keystroke, but its budget is a ~1-second lifecycle budget, not a Reflex-class one.

**The issue.** PH7.7 (locked) runs a plugin motion/operator/text-object `apply`
**synchronously on the dispatch thread** — correct, because a motion must return a
`MotionResult` inline to compose with `d{motion}` and to keep dot-repeat/macros
synchronous (async grammar would break operator∘motion atomicity and the
keystroke contract). Fuel + epoch bound the runaway case. **But** the current
`PluginBudget` default is `fuel: 1e9`, `epoch_deadline: 1_000` ticks × `1ms`
ticker = **~1 second** (`lib.rs:116-141`). That budget is sized for `activate` and
async producers. On the *synchronous grammar path* it means a plugin motion —
legitimately or maliciously — can occupy the actor thread for up to ~1s before
the epoch trap fires, stalling that keystroke far past the one-frame ceiling
(8.3ms @120Hz). The <5µs gate (§7) bounds the *boundary marshalling* (measured
~40ns in PH7.7a), **not** the guest's compute.

> **UX (higher court):** a plugin motion that hangs a keystroke for hundreds of ms
> is exactly the "plugin stalls a keystroke" failure the whole host is built to
> prevent — it just prevents it everywhere *except* the one place a plugin runs
> synchronously. Losing here loses regardless of goal scores.
> **Paramount #1:** the sync seam needs a **Reflex-class** fuel/epoch budget
> (sub-frame, target ≤1–2ms wall-clock so a trap fires well inside the frame),
> distinct from the lifecycle/async budget. Grammar is the seam that touches the
> keystroke, so grammar is the seam that must carry the tightest budget.
> **Paramount #3:** built-ins stay native and never pay it; only a user-invoked
> plugin motion does — so the cost is opt-in and scoped, which is what makes the
> sync exception acceptable *at all*.
> **Heuristic #1 (long-term fit):** the sync trampoline is the genuinely-better
> design (models the seam as what it is); the missing piece is a per-seam budget,
> not a redesign.
> **Heuristic #3 (third option):** async grammar was considered and rejected
> (breaks atomicity/replay); a *separate sync engine* vs *sync calls on the shared
> engine* is a 7.7b/c implementation choice, not a philosophy fork.

**Recommendation.** (1) PH7.7c introduces a **Reflex-class `PluginBudget`** for
grammar `apply`/`parse_args` (tight epoch, e.g. a few ms ceiling; fuel tuned so a
real structural motion fits). (2) Reconcile the docs: fragment §3 ("no
synchronous path from UI input to plugin code") and §4.1 ("the trampoline is
async + fuel-bounded") are **superseded** by the sync fork and must carry a
`Superseded (PH7.7)` note stating the grammar exception explicitly (the §3
cache-supersession note is the precedent). This is the invariant I2 in its exact
form.

### F2 (High) — Mode ownership is the integration point for every seam and is entirely unvalidated.

**The issue.** "Provide modes" is the user's second goal, and a conforming mode
owns its *full* surface — keymap, decorations, gutter decorations, completion
sources, action handlers, options, capabilities, activation policy, `on_activate`
lifecycle (the `DynMode` method set is 15+ methods, `mode.rs:455-489`). PH7.11
mirrors the `Mode` trait and registers `Arc<dyn DynMode>` into the same
`ModeRegistry`, which is the *right* shape (I1). But: (a) PH7.11's stated
dependencies are only PH7.7 + PH7.9 — a fully-conforming mode also needs PH7.8
(subscriptions) and PH7.10 (options), so the dependency list understates the
integration; (b) the mode-ownership rule (no `ensure_x_buffer` /
`Editor::do_mode_x`; handler bodies live in the mode) is *asserted* in §5 but
cannot be *proven* until a plugin mode composes all seams — and the only
validator, `fuzzy-finder`, is a picker source, not a mode. This is precisely the
"half-migration" failure mode the standing rule guards against, and it is the
least-de-risked part of the design.

> **Paramount #2 + the mode-ownership standing rule:** the acid test (a plugin
> mode adds ZERO `Editor::` methods, keymap only at `MinorMode(id)`, handlers in
> the guest) must be a *test*, not a doc claim.
> **Heuristic #1:** better to prove the composite now (even against a trivial
> synthetic plugin mode) than to discover at Phase 8 that the independently-sliced
> seams don't compose without a host-side shim.

**Recommendation.** Keep PH7.11 in Phase-7 scope as *declaration + registration*
(as planned), but (1) correct its dependency list to PH7.7–7.10, and (2) add a
Phase-7 or early-Phase-8 **synthetic plugin-mode integration test** that wires a
minor mode with a keymap + one decoration + one subscription + one option and
asserts the ZERO-`Editor::`-methods acid test — the mode analogue of what
`fuzzy-finder` did for pickers.

### F3 (Medium) — The rich-UI seam is the right philosophy with almost no design yet.

**The issue.** `ui.wit` and `decorations.wit` are one-line stubs. The philosophy
is correct and load-bearing (I3: emit data, renderer reads cached, no per-frame
WASM; §7 gate <50µs). But "rich UIs leveraging core infrastructure" needs the
*contribution shape* designed: how a plugin declares a status/headerline segment,
a gutter decoration provider, a popup, a notification, a sprite set — and, per I1,
that these route through the **same** buffer/popup/decoration infrastructure
native modes use (a plugin popup is a popup buffer under a mode, not a bespoke
plugin-popup path). It must also honour *decorations update in place, never
flicker* (the async-refresh contract): a plugin decoration provider's mid-refresh
state must never blank unchanged lines.

> **Paramount #1 + no-UI-thread-work:** the host builds the decoration snapshot
> off the render path; the plugin never draws. Non-negotiable.
> **Heuristic #3 (third option):** the naive "give plugins a canvas" is rejected
> up front; the real design work is the *data vocabulary* (segment/decoration/
> popup records) + routing them through existing infra.

**Recommendation.** Before PH7.9 executes, write the ui/decoration *contribution
data model* into the fragment (§5 currently only names the interface). Explicitly
state that plugin popups/notifications reuse the popup-buffer + notification infra
(no parallel path) and that plugin gutter/inlay decorations honour the
update-in-place contract.

### F4 (Medium) — Grammar's reach stops short of the most valuable extensions.

**The issue.** PH7.7a's context projections deliberately omit the tree-sitter
`scope_resolver` and `comment_syntax` env (they are host-owned trait objects, and
`ExCommandContext.range` is a recursive grammar `Range` that cannot cross). That
is the correct v1 line — but it means a plugin can only build motions/text-objects
that compute from **raw buffer text + cursor**, not **structural** ones. Yet the
design elsewhere calls tree-sitter-driven motions/text-objects "first-class
future" (paramount #3), and structural motions are exactly the high-value grammar
extension a developer would reach for. So the seam named to serve goal #1 serves
its *least* interesting half in v1.

> **Paramount #3:** honest scoping, not a violation — but the gap should be named
> so it is a *planned* next seam, not a silent ceiling.
> **Heuristic #2:** the resolution (a `ScopeResolver` host-services-style callback
> the guest reaches through the `document` handle) is anchored on our own
> tree-sitter substrate, not on any editor precedent.

**Recommendation.** Record, in the fragment and the PH7.7 slice, that a
`scope-resolver` guest→host callback (the `host-services` pattern, gated on a
parse being available) is the planned follow-on that unlocks structural plugin
motions — so F4 is a tracked seam, not an accidental limit. Not required for
PH7.7d's exit (a text/position motion validates the trampoline).

### F5 (Medium) — Plugin *major* modes collide with the keymap write-gate.

**The issue.** The mode architecture separates **major** (content-type identity:
rust, markdown) from **minor** modes. A plugin language mode is a *major* mode.
But the capability model grants `KeymapCapability::MinorMode`, which permits
writes to `MinorMode(_)`/`Buffer` and **denies `MajorMode`** (§6;
`registry.rs:132`). So the default plugin grant cannot contribute a major mode's
keymap. Whether plugin major modes are (a) supported via a broader trust-tier
grant, (b) restricted to bundled plugins, or (c) out of scope for v1 is
**unspecified** — a real ambiguity for "provide modes."

> **Paramount #2 + #3:** a plugin that adds a new language (major mode) is a
> flagship extensibility case; the write-gate that protects universal vim grammar
> must not also silently forbid legitimate major-mode contribution.
> **Heuristic #3:** the third option — a distinct `MajorMode` keymap capability
> gated on trust tier, separate from the "can't shadow Builtin" gate — likely
> resolves it without weakening the Builtin protection.

**Recommendation.** PH7.11 must state the plugin major-vs-minor mode policy
explicitly and, if major modes are supported, define the capability that permits a
`MajorMode(id)` keymap layer (distinct from the Builtin/User denial that protects
core vim grammar).

### F6 (Low–Medium) — The `&'static str` intern (`Box::leak`) seam is unbounded under hot-reload.

**The issue.** `boundary_picker::intern` and `boundary_grammar::intern` leak each
plugin-supplied spec string as `&'static str` (native spec metadata is
`&'static str`). Bounded by *loaded-source count* today — fine. But PH7.12's
reload/hot-swap seam re-registers on every reload, so the leak grows unbounded
across reloads (the intern comments already flag this as "the PH7.12 quarantine").

> **Heuristic #1:** an arena/`Box::leak`-per-plugin freed on teardown, or interning
> into a per-plugin string pool dropped with the plugin, is the durable fix — not a
> reason to block 7.7/7.9 now, but it must land *with* the reload seam, not after.

**Recommendation.** PH7.12 owns this: tie interned spec strings to the plugin's
lifetime (freed on `deactivate`/reload) rather than `'static`.

### F7 (Low) — The design fragment has drifted from landed reality.

**The issue.** The fragment header still reads "📝 design (2026-07-01). Greenfield
— no `lattice-plugin-host` crate, no `wit/`, no `plugins/` … exists yet," but
PH7.0–7.6 + 7.7a have shipped. §4.1 describes an **async** grammar trampoline that
PH7.7 replaced with a **sync** one. Several "PH7.x lands…" notes are now past
tense. Per the "separate design from slice plan" rule, the fragment carries the
stable *what/why* and must be updated *when the design itself changes* — the sync
fork is such a change and has not been written back.

**Recommendation.** A refresh pass: update the status header to reflect landed
slices; add the `Superseded (PH7.7)` note to §3/§4.1 (folds into F1); convert
stale future-tense notes. Low effort, prevents a future contributor from building
on the wrong (async) grammar assumption.

### F8 (Low) — High-frequency event delivery needs a coalescing/filtering discipline.

**The issue.** PH7.8 delivers each `Event`, cloned + serialized, to a subscribing
guest's `on-event` export **as a separate task**. Correct for isolation ("a slow
handler never delays other subscribers"). But for high-frequency events
(cursor-moved, per-keystroke) with N subscribers, per-event task spawn at held-key
rates (~30Hz) is real overhead, and observation-only means no back-pressure.

> **Paramount #1/#4:** the `subscribe(filter, …)` shape already lets a plugin
> narrow what it receives; the discipline to add is host-side coalescing of
> high-frequency events and a documented filter granularity, so a plugin cannot
> accidentally opt into a task-per-keystroke firehose.
> **Correct call already made:** before-class veto/mutation stays out (observation
> only) — that keeps the event path off the synchronous keystroke path (I2),
> consistent with the whole design.

**Recommendation.** PH7.8 documents the event filter granularity and coalesces
high-frequency event classes before fan-out; keep observation-only for v1.

---

## Future-slice philosophy review (PH7.8–7.12)

| Slice | Fit | Note |
|---|---|---|
| **7.8 events/hooks** | ✅ Strong | Hooks≡autocmds≡typed subscriptions is a core unification; fills the reserved `SubscriptionTarget::Plugin`; async task-per-event (off keystroke). Observation-only is the right v1 line (keeps I2). Watch **F8**. |
| **7.9 decorations + ui** | ✅ Philosophy / ⚠️ under-designed | Emit-data-not-draw is exactly I3; gate <50µs. Serves goal #3 but the contribution data model isn't designed and must reuse core popup/decoration infra + update-in-place. **F3**. |
| **7.10 config/options** | ✅ Strong | Same `ConfigRegistry`, typed options + `:customize` uniformity, values-as-strings crossing, change events via existing `EventPublisher`. Cleanest of the pending seams; no concern. |
| **7.11 modes** | ⚠️ Integration risk | The composite of 7.7–7.10; the mode-ownership acid test lives or dies here. Dep list understated; major-mode capability unresolved. **F2**, **F5**. |
| **7.12 crash/lifecycle/reload** | ✅ Strong | Crash isolation + quarantine + reload seam + fuzz is the right capstone; owns the intern-leak fix. **F6**. |

**Cross-cutting read:** every pending seam except grammar is async / off-render /
data-emitting, so I2 holds cleanly across 7.8–7.12 with grammar as the sole,
now-explicit exception. I1 (same registries, `SubsystemBoot`) and I3 (emit data,
host owns draw/plumbing) hold uniformly. The philosophy is coherent end-to-end;
the risks are **budget precision** (F1), **composite validation** (F2), and
**under-specified UI vocabulary** (F3), not a structural mismatch.

---

## Corrections to land (prioritised)

1. **F1 — PH7.7c: Reflex-class grammar `PluginBudget`** + fragment §3/§4.1
   supersession note (the sync-grammar invariant I2). *Blocks 7.7c.*
2. **F2 — PH7.11 dependency correction (7.7–7.10) + a synthetic plugin-mode
   integration test** proving the mode-ownership acid test. *Before 7.11.*
3. **F3 — ui/decoration contribution data model** written into the fragment
   before 7.9 executes; state reuse-core-infra + update-in-place explicitly.
4. **F5 — PH7.11 major-vs-minor plugin-mode capability policy** stated
   explicitly.
5. **F4 — track the `scope-resolver` guest→host callback** as the planned
   structural-grammar follow-on (fragment + PH7.7 slice).
6. **F7 — fragment refresh pass** (status header + stale future-tense).
7. **F6 — PH7.12 owns the plugin-lifetime intern fix.**
8. **F8 — PH7.8 documents event filter granularity + high-frequency
   coalescing.**

None of these invalidates PH7.7a or blocks continuing to PH7.7b. F1 is the one
that must be resolved *within* the grammar slice (7.7c); the rest attach to their
own slices.
