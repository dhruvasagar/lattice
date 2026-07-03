# Audits

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

- [`effect-dispatch.md`](effect-dispatch.md) — how an `Effect` reaches its host
  and renderer appliers; the "everything in `out.effects` was already
  host-applied" invariant.
