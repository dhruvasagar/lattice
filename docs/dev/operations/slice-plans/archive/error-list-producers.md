# Error-list producers — slice plan

> **ARCHIVED 2026-08-15.** EP.1–EP.6 complete. Verified against source, not
> status icons, before filing. The design fragment (if any) stays in
> `docs/dev/architecture/` — only the slice plan moved.

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
| EP.1 | Tagged sources — `ErrorSource` + scoped slice replace | ✅ |
| EP.2 | Index re-anchoring across refreshes | ✅ |
| EP.3 | LSP producer — layer hook, severity mapping, coalescing | ✅ |
| EP.4 | Policy — option + `:lsp-diagnostics-to-error-list` | ✅ |
| EP.5 | ~~Retire the `:diagnostics` help view~~ — **rescoped**, see below | ✅ |
| EP.6 | References as a third producer, opt-in | ✅ |

Sequencing is strict: EP.1 before everything (it changes the payload
type), EP.2 before EP.3 (a live feed without re-anchoring is a
regression, not a feature), EP.4 with or immediately after EP.3.

---

## EP.1 — Tagged sources ✅

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

## EP.2 — Index re-anchoring ✅

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

## EP.3 — LSP producer ✅

All new code in `lattice-lsp`.

- **Landed against `DiagnosticsLayer`, not the raw broadcast.** The
  layer already maintains the merged workspace URI → diagnostics map
  *and* a revision counter, so coalescing is "did the revision move?"
  rather than a hand-rolled dedupe over `subscribe_diagnostics`. One
  fewer subscriber and no second copy of the map.
- Map `DiagnosticSeverity` → `ErrorSeverity`; an absent severity is
  treated as `Error` (under-reporting a real error is the worse
  failure).
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

## EP.4 — Policy ✅

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

## EP.5 — Rescoped ✅

**`:diagnostics` is NOT retired. The original premise was wrong twice.**

First, the surface was misidentified. `dispatch.rs:3335` is the **`gl`
line-diagnostics popup** (`Effect::ShowDiagnosticsPopup`, diagnostics on
the cursor's line via the hover pipeline) — a different feature, and not
redundant with anything. `:diagnostics` is `do_list_diagnostics`, a
**fuzzy picker** over the LSP snapshot.

Second, even the picker is not redundant. `:error-list` browses the
*merged* list; `:diagnostics` browses the *LSP-only* set — and with
`lsp.diagnostics-to-error-list = false` it is the **only** way to browse
diagnostics at all. Retiring it would have removed the browse surface
for exactly the users who opted out of the feed.

What was genuinely dead and is now deleted:
`lattice-lsp/src/help_views.rs::lsp_references_help` — a definition with
no call sites (`gr` has always opened a picker, not a help view).

**Lesson recorded rather than quietly fixed:** this slice was specified
from a grep of the string `"diagnostics"` rather than from reading the
call path. Two of its three claims did not survive contact.

## EP.6 — References as a third producer ✅

Design: [`error-list.md`](../../architecture/error-list.md) §3.2b.
Builds on LR.2's terminus routing.

- `ErrorSource::References` in `lattice-protocol`.
- Option `lsp.references-to-error-list`, bool, default **`false`**
  (diagnostics default on; references do not — see the design for why).
- Ex-command `:lsp-references-to-error-list` — a third terminus on the
  references drain, not a cache snapshot: there is no standing
  references state to pull from.
- The terminus becomes an enum (`Picker` / `View` / `ErrorList`),
  replacing LR.2's bool.
- Option on ⇒ any references query also pushes the `References` slice,
  whatever its terminus. Severity `Info`; write kind `NewRun`.

**Tests.** A references push leaves compile and LSP slices intact (the
clobber regression, per source); option off ⇒ `gr` does not touch the
list but the command does; the terminus enum does not leak between
requests; severity is `Info`.

---

## Deferred

- **Per-source filters** (`:problems lsp` / `:problems compile`) —
  plausible once merged lists have been lived with; speculative now.
- **Lifting the plugin-boundary refusal** of `SetErrorList`. EP.1 makes
  it *possible* (a write can be scoped to its author); whether to allow
  it is a plugin-path decision with its own capability question.
