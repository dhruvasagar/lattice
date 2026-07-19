# Plugin observability + manager — deep code review

**Reviewed 2026-07-18/19**, an adversarial multi-agent review of the
freshly-landed plugin work — the observability stack (PO.1–PO.5) and the
`:plugins` manager view (PL8.H) — none of which had been through review. Two
review passes (host crate; manager + trace crates), each fanned across
dimensions (correctness / isolation / ABI / hot-path / tests) with every raised
finding **adversarially verified** (a second agent tried to refute it) before it
counted. This document records the confirmed findings, what was fixed, and what
was deliberately deferred, so the deferred items aren't lost and the invariants
the review leaned on stay visible.

Companion: [`plugin-host-architecture.md`](plugin-host-architecture.md) — the
PH7.x conformance review this extends into the Phase-8 loader/observability
surface.

## Invariants the review checked

> **Observability is never on the editor hot path.** Trace records stream via the
> event bus (the LSP-log precedent); formatting + buffer append happen off-thread.
> The one synchronous seam (grammar) gates on a per-plugin relaxed-atomic
> `HotGate` that costs ≈0 when off (benched, PO.3). A mode's `on_activate` seed +
> its `PluginTracePushed` drain run OFF the actor thread.

> **A plugin is untrusted; nothing it declares may escape its sandbox.** The
> capability grant + fuel/epoch budget + crash-quarantine bound it; the per-plugin
> writable data mount must stay inside the plugin's own dir.

> **Observability must never crash the editor.** A poisoned trace mutex is
> recovered, not propagated; a missing service degrades to empty/no-op, never a
> panic.

> **Modes own their full surface.** A provider crate's chords carry both the
> binding and the handler body; zero host `Editor::` methods, zero host `Action`
> variants.

## Findings — host (`lattice-plugin-host`)

14 confirmed (4 refuted). Fixes in `8a1725c0`, `02489f13`.

| Sev | Finding | Disposition |
|---|---|---|
| **CRITICAL** | `manifest.id` (untrusted) was joined **unsanitized** into the per-plugin data-dir path, which is mounted **writable** into the guest — so `id = "/etc/cron.d"` / `"../../.ssh"` relocated the writable mount outside the sandbox, defeating the capability model. | **Fixed** — `is_safe_plugin_id()` (single `Component::Normal`) rejected at parse (`InvalidId`) AND re-checked at the join (defense-in-depth; unsafe id → no mount). + 3 tests. |
| High | Grammar quarantine trip + re-trip short-circuit never exercised through a real guest. | **Fixed** — a `traps` fixture motion (guest panic → wasm trap) drives the trap branch; asserts one `PluginCrashed`/Error trace + short-circuit. |
| High | Tracer poisoned-mutex recovery (`lock() → into_inner`) had zero coverage. | **Fixed** — a panicking-publisher test proves recovery, no re-panic. |
| Medium | `PluginTracer::forget_plugin` never called in production → every unload/reload leaked the plugin's ring + gate (ids monotonic → unbounded). | **Fixed** — loader `unload` calls `forget_plugin`. |
| Medium | `run_callback`'s poisoned-guest-lock degradation untested. | **Deferred** (hard to poison the guest mutex in a test). |
| Medium | `logging::Host::log` None-`log_ctx` debug-drop untested. | **Fixed** — no-tracer-wired test. |
| Medium | `TraceOutcome::Denied` is constructed by no seam — capability-denied host calls produce no trace record. | **Deferred** — instrumenting capability denials is a new increment, not a fix. |
| Low | Async-seam guest-errs record as `Debug`/`Ok` (dropped) while grammar records `Warn`; the grammar comment falsely claimed it mirrors async. | **Fixed** — comment corrected (the sync seam sees the inner `Result::Err`; the async seams don't). |
| Low | `EpochTicker::spawn` `.expect()` panicked host construction if the OS refused a thread. | **Fixed** — returns `PluginHostError::EpochTicker`, propagated. |
| Low | config/mode/keymap worlds lacked the `logging` import (no Layer-2 channel). | **Fixed** — extended `logging` to all 8 async worlds + stamped `log_ctx` in their spawn paths. |
| Low | The event-delivery WIT-projection-failed log was `warn!` (fans to `*messages*`), firing per-crash when a wildcard sub matches host-internal `PluginCrashed`. | **Fixed** — downgraded to `debug!` per the log-levels rule. |
| Low | Sync grammar linker's exclusion of `logging` asserted only by construction. | **Deferred** — no meaningful test (grammar world can't import it). |
| Low | Concurrent gate-republish race untested. | **Deferred**. |
| Low | `map_log_level` Critical→Error / Trace mapping unexercised. | **Fixed** — fixture emits both; asserted. |

Also noted (partial-run, low): the async seams do avoidable per-delivery work
(`Instant::now` + record build even when a dropped `Debug` record follows) — they
lack the `HotGate` short-circuit the grammar seam got in PO.3. Off the hot path;
**deferred** as an efficiency polish (retrofit the `HotGate` mirror onto the async
actors).

## Findings — manager + trace (`lattice-plugin-manager`, `lattice-plugin-trace`)

8 confirmed (4 refuted). Fixes in `c589786f`.

| Sev | Finding | Disposition |
|---|---|---|
| **High** | The trace-buffer seed formatted the whole ring (up to 10k records) **synchronously on the actor thread** during `on_activate` — only the write was spawned (paramount #1 violation). | **Fixed** — one off-thread task does the seed snapshot+format AND the tail; nothing document-proportional touches the actor thread. |
| Medium | `:plugin` reload reordered `plugin_status()` (unload removes, load appends), leaving the `:plugins` cursor→index mapping stale → a follow-up `r`/`x`/`t`/`T` targeted the WRONG plugin. | **Fixed** — `plugin_status()` sorts by name; a reloaded row stays in place. |
| Medium | `plugin_row_at` out-of-bounds / header-line no-op paths untested. | **Deferred** (needs a loaded-plugin fixture). |
| Medium | Missing-service no-op contract on the handlers untested. | **Fixed** — no-panic degradation test for all 6 handlers. |
| Medium | `PluginManagerMode::on_activate` degradation ladder untested. | **Deferred** (needs a runtime/ctx harness). |
| Low | `actions.rs` docstring claimed unload runs off the actor thread (it's a sync teardown). | **Fixed** — corrected. |
| Low | Records pushed between the seed snapshot and the subscription were lost (seed/tail gap). | **Fixed** — subscribe before seeding (a possible benign duplicate instead of a loss). |
| Low | `resolve_filter`'s loader-present `Plugin(id)` branch untested. | **Deferred** (needs a loaded-plugin fixture). |

## Deferred items (open follow-ons)

These are genuine gaps the review surfaced; none is a live defect, and each is a
new increment rather than a fix:

- **Emit `TraceOutcome::Denied`** from the capability gate (host-services `walk`,
  etc.) so denials are observable in the trace.
- **Retrofit the `HotGate` short-circuit onto the async seams** so a dropped
  `Debug` record costs nothing (parity with the grammar seam).
- **Coverage** needing a loaded-plugin fixture: `plugin_row_at` bounds, the
  manager `on_activate` degradation ladder, `resolve_filter`'s `Plugin(id)`
  branch, the `run_callback` poisoned-guest-lock path, the concurrent
  gate-republish race.

## Design-doc audit (same review campaign)

A parallel audit checked `design.md` + the architecture fragments against the
implementation for material drift: **15 confirmed drifts** (rendering trait/
substrate divergence, a false ">10% regression" CI-gate claim, the
bounded→unbounded mailbox switch, the protocol-enum sketches, the Phase-8
roadmap, the plugin-host perf table, the WIT-validation risk rows). All 15 were
reconciled into `design.md` / `implementation.md` (commits `7410c6ef`,
`d20cf160`, `35a71763`, `f75e68b2`, `69b1d6aa`). **Caveat:** 28 further raised
findings lost their adversarial verifier to a session usage limit mid-run and
were *dropped* (neither confirmed nor applied) — resuming the audit workflow
would verify them and likely surface a few more real drifts.
