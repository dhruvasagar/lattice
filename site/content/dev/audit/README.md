+++
title = "Audits"
+++


Point-in-time investigations of a subsystem's design — written when a bug or a
"this smells redundant" hunch turns into a full trace of how something actually
works. An audit captures the **invariant** a subsystem relies on, the paths
that honour it, and the anomaly that motivated the write-up, so the next person
(or the next bug) starts from the map instead of re-deriving it.

Distinct from the neighbouring docs:

- `../architecture/*` — the stable *what* and *why* of a design.
- `../operations/slice-plans/*` — the *when* and *in what order* of building it.
- `audit/*` — *"is this actually designed correctly, and where are the sharp
  edges?"* — a snapshot of a real trace, dated, kept even after the anomaly is
  fixed because the invariant it documents is load-bearing.

Each audit names the invariant explicitly. If a future change violates one,
that's the signal to re-read the audit before "just patching it."

## Index

- [`effect-dispatch.md`](../effect-dispatch/) — how an `Effect` reaches its host
  and renderer appliers; the "everything in `out.effects` was already
  host-applied" invariant.
- [`terminal-wide-char-ghosting.md`](../terminal-wide-char-ghosting/) — why
  terminal panes ghosted under auto-scroll; the "one grid column in → one
  display column out; never emit a `WIDE_CHAR_SPACER` cell as a character"
  invariant.
- [`plugin-host-architecture.md`](../plugin-host-architecture/) — conformance
  review of the WASM plugin host (PH7.x) against the paramount goals + the three
  extension goals (grammar, modes, rich UI); the three invariants (same
  registries; WASM off the keystroke path with grammar the one bounded sync
  exception; guest emits data, host owns draw/dispatch) + 8 findings across the
  pending seams.
- [`plugin-observability-review.md`](../plugin-observability-review/) — adversarial
  code review of the plugin observability stack (PO.1–5) + the `:plugins` manager
  (PL8.H); the invariants (tracing off the hot path; sandbox-tight capability
  model; observability never crashes the editor; modes own their surface) + the
  confirmed findings (a CRITICAL `manifest.id` sandbox escape, a trace-ring leak, a
  seed-on-the-actor-thread violation, …), what was fixed, and the deferred
  follow-ons. Includes the parallel design.md drift-audit summary.
