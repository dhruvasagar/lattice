# Error-list producers — slice plan

> **Status: Active.** Opened 2026-08-10. Implements
> [`error-list.md`](../../architecture/error-list.md) §3.1–§3.3: the
> tagged-source substrate and the language server as the error list's
> second producer.

Design owns *what* and *why*; this file owns *when* and *in what
order*. Cross-reference, don't duplicate.

Related: [`compilation-mode.md`](compilation-mode.md) (CM.2/CM.7/CM.8
built the substrate and its first producer),
[`multibuffer-providers.md`](multibuffer-providers.md) (catalogue entry
A.4 is **struck** by this work — see design §6).

## Status

| Slice | Title | Status |
|---|---|---|
| EP.1 | Tagged sources — `ErrorSource` + scoped slice replace | 📝 |
| EP.2 | Index re-anchoring across refreshes | 📝 |
| EP.3 | LSP producer — subscriber, severity mapping, coalescing | 📝 |
| EP.4 | Policy — option + `:lsp-diagnostics-to-error-list` | 📝 |
| EP.5 | Retire the redundant `:diagnostics` help view | 📝 |

Sequencing is strict: EP.1 before everything (it changes the payload
type), EP.2 before EP.3 (a live feed without re-anchoring is a
regression, not a feature), EP.4 with or immediately after EP.3.

---

## EP.1 — Tagged sources 📝

Turn the whole-list replace into a per-source slice replace.

- `lattice-protocol`: add `ErrorSource { Compilation, Lsp }`
  (`Serialize`/`Deserialize`, it rides inside `AppEffect`).
- `lattice-grammar`: `AppEffect::SetErrorList { source, entries }`.
- `lattice-host`: `ErrorList` holds slices; `set_error_list(source,
  entries)` splices; `entries()` returns the concatenation in fixed
  source order (`Compilation`, then `Lsp`) preserving each producer's
  own order. `step_file`'s "maximal run of consecutive entries sharing
  a path" is unchanged — it operates on the concatenation.
- Call sites: `lattice-compilation/src/service.rs` passes
  `ErrorSource::Compilation`; `boundary_app_effect.rs`'s refusal arm
  keeps refusing (lifting it is a plugin-path decision, not this
  slice); `providers/problems.rs` reads `entries()` unchanged.

**Tests.** Slice write leaves the other slice intact (the clobber
regression — this is the point of the slice); concatenation order is
`Compilation` then `Lsp`; `step_file` still lands on first-of-file
across a two-slice list; existing `ErrorList` unit tests pass with the
source argument threaded through.

**Not in this slice:** no behaviour change is visible to the user —
only one producer exists until EP.3.

## EP.2 — Index re-anchoring 📝

`ErrorList::set` resets `index` to 0. Correct for a new compile run,
wrong for a refresh.

- Splice re-points the index at the same entry, matched on
  `(path, message)`, line-drift tolerant.
- Fallbacks in order: first entry of the same path at-or-after the old
  line → index 0.
- A *new run* (compilation) keeps the reset. The distinction is the
  producer's, expressed as a flag on the splice, not inferred.

**Tests.** Walk to entry 3, refresh with an entry inserted above,
`:cnext` lands on the successor of the *same* entry, not the same
ordinal; entry deleted → nearest-following rule; empty refresh → index
0; compile re-run still resets.

## EP.3 — LSP producer 📝

All new code in `lattice-lsp`.

- Subscribe to the existing `publishDiagnostics` broadcast
  (`lattice-lsp/src/actor.rs` — `diagnostics_subscriber_count` shows
  the seam is already there).
- Maintain the workspace URI → diagnostics map; map
  `DiagnosticSeverity` → `ErrorSeverity`.
- **Coalesce on a ~250ms idle debounce**, one rebuilt `Vec` per quiet
  period, never one push per notification.
- Publish via `SubsystemBoot::inbound::<Vec<ErrorEntry>>` →
  `AppEffect::SetErrorList { source: Lsp, .. }`. `InboundBus`, not
  `TickCallback` — the wake must be structural.

**Tests.** Severity mapping per variant; URI → path conversion
including non-file schemes (skip, don't panic); a burst of N publishes
inside the debounce window yields one push; **entries reach
`*problems*` with no key dispatched** (the wake test — a test that
presses a key first passes on the broken version too).

**Bench.** Rebuild cost at 1k diagnostics; the debounce must hold the
actor-thread contribution flat as publish rate rises.

## EP.4 — Policy 📝

- Option `lsp.diagnostics-to-error-list`, bool, default `true`, group
  `lsp`, dashed spelling.
- Ex-command `:lsp-diagnostics-to-error-list` — one alias, dashed +
  namespaced, per the ex-command naming rule. No collapsed or generic
  spelling.
- One snapshot function, two callers: the command, and the
  option-change handler on `false → true`.
- `true → false` stops the feed and **leaves the slice in place**.
- The command echoes the entry count (the scope-honesty requirement —
  published ≠ scanned).

**Tests.** `false` → publishes don't touch the list, command populates
it; `false → true` populates **without** a further publish or a
keypress; `true → false` leaves existing entries; command under `true`
acts as a forced refresh; the echoed count matches.

**Docs.** `:describe-option` / `:describe-command` metadata; a pointer
from `lsp-architecture.md` to design §3.2.

## EP.5 — Retire `:diagnostics` help view 📝

`dispatch.rs:3335` renders diagnostics as `HelpContent`. Redundant once
the language server feeds `*problems*`. Repoint `:diagnostics` at the
grouped view and delete the help-view path.

Also delete `lattice-lsp/src/help_views.rs::lsp_references_help` —
dead today (definition, no call sites), and (A) will not revive it.

**Tests.** `:diagnostics` opens the grouped view; no `HelpContent`
diagnostics path remains (grep gate).

---

## Deferred

- **Per-source filters** (`:problems lsp` / `:problems compile`) —
  plausible once merged lists have been lived with; speculative now.
- **Lifting the plugin-boundary refusal** of `SetErrorList`. EP.1 makes
  it *possible* (a write can be scoped to its author); whether to allow
  it is a plugin-path decision with its own capability question.
