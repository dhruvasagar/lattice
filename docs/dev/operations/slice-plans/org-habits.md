# Slice plan — org habits

Design: [`org-habits.md`](../../architecture/org-habits.md).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

| Slice | Description | Status |
|---|---|---|
| HB.1 | The repeater grammar, parsed **(plugin)** | ✅ |
| HB.2 | Completing a repeating task repeats it **(plugin)** | ✅ |
| HB.2b | …and from the agenda row, through the source seam **(plugin)** | 📝 |
| HB.3 | Completion history from `:LOGBOOK:` **(plugin)** | 📝 |
| HB.4 | The consistency graph — states and glyphs **(plugin)** | 📝 |
| HB.5 | Drawn under the agenda row **(cross-repo)** | 📝 |
| HB.6 | Derived analytics: streak, rate, weekday pattern | 📝 |
| HB.7 | A general per-row inline annotation slot | ⛔ |

**HB.2 is the foundation and it is also a bug fix.** Today marking a repeating
task DONE in lattice just writes `DONE`: no shift, no reset, no log entry. That
does not merely fail to repeat — it *destroys the habit*, because the headline
stops being scheduled and the completion is never recorded. Anyone using
lattice on their org files today should know that before they mark one done.

HB.3–HB.5 can be read-only against files emacs already maintains, which is why
the graph will show real history the day it lands (design §1).

---

### HB.1 — the repeater grammar ✅

`org_date.rs` carried repeaters as an opaque trailing string ("A repeater is
carried, not parsed" — its own module doc). That was right for the schedule
prompt, which only has to round-trip one, and is not enough for anything that
must *apply* one.

`repeat.rs` parses the four forms (`+`, `++`, `.+`, and the habit `.+MIN/MAX`)
into a typed `Repeater`, and shifts a date by one. The distinction that
carries the feature: **only `.+` shifts from the completion date.** `+` and
`++` shift from the timestamp already on the line, which is what makes "the
1st, 4th, 7th regardless" different from "three days after I last did it".

`++` advances repeatedly until the result is in the future — the catch-up form.
A naive single shift leaves a monthly task that lapsed for a year still in the
past, so the loop is bounded and tested at the boundary rather than assumed.

Month and year arithmetic clamps rather than overflowing: 31 January + 1 month
is 28 February (29 in a leap year), because org does that and because the
alternative is a date that does not exist.

### HB.2 — completing a repeating task repeats it ✅

Design §3, whole: shift the planning stamps, reset the keyword to
`:REPEAT_TO_STATE:` or the sequence's first non-done keyword, log the
state change into `:LOGBOOK:`, stamp `:LAST_REPEAT:`. One edit over the
subtree, so `u` takes the completion back as one action and neither half can
land alone.

Wired to **both** `TODO_SET` and `TODO_CYCLE`. Routing only the first would
have left `<leader>ot` — the key people actually press on a habit — still
destroying it, which is the half-migration this plan exists to close. The
cycle path reads its target keyword back out of the line `cycle_keyword`
produced rather than re-deriving it, so the sequence rules stay in one place.

The gate is narrow on purpose: `complete_repeating` answers `None` for
anything without a repeater, and the old single-line rewrite runs unchanged.
A test pins that a plain `* TODO Ship it` still just becomes `DONE` — a change
to every completion in every org file must not ride in on a habit fix.

`org.log-into-drawer` is a new option defaulting to **on**, which is not
emacs' default (`org-log-into-drawer` is nil). UX-follows-convention decides
it: essentially every real org config sets it, because loose log lines under a
habit are what people migrate away from once a file has history. Org reads
both, so the deviation costs placement, not compatibility.

### HB.2b — …from the agenda row 📝

**Completing a habit from the agenda still takes the destructive path.**

An agenda excerpt is ONE line — the headline — so the multi-line rewrite finds
no planning line in the composed view and falls back to the plain `DONE`. The
completion has to read and write the **source** through OA.23b's seam instead
of the view, which is a real slice rather than a tweak: it needs the source's
subtree lines out and a multi-line source edit back.

Deliberately no test in `org_agenda.rs` pinning today's answer — that would
enshrine the destructive behaviour as intended. The org-file case is covered in
`org_structure.rs`.

### HB.3 — completion history 📝

Parse `- State "DONE" from "..." [ts]` lines out of a headline's `:LOGBOOK:`
into a set of dates. Also read `:LAST_REPEAT:` as a fallback for habits whose
log was trimmed.

Emacs has been writing these for years, so this slice is what makes the graph
show real data immediately.

### HB.4 — the graph 📝

Per-day state (clear / ready / alert / overdue) from the completion set, the
repeater's MIN/MAX and the scheduled date; glyphs in two palettes per the
icon-degradation rule; colours as mode-owned theme elements so `:colorscheme`
recolours them (OA.16's hardcoded hex is the lesson).

### HB.5 — drawn under the agenda row 📝

A `VirtualRow` anchored Below the habit's row. Needs a seam: the graph is
computed in the guest and the row is registered by the host, so the plugin has
to hand over per-row decorations. That is the cross-repo half and the reason
this is its own slice.

### HB.6 — derived analytics 📝

Streak, completion rate over the window, per-weekday pattern. Computed from
HB.3's data, stored nowhere (design §1).

### HB.7 — a general per-row inline annotation slot ⛔

Deferred, with a real design behind it. Today's inline diagnostic is a
bespoke, cursor-line-only end-of-line annotation. Generalised into a per-row
slot any provider can contribute to, it would let the graph sit at a column
like emacs' (halving the habit block's height), and would serve inlay hints,
git blame and lenses.

Deferred rather than dropped because it is a host mechanism needing both
renderers in lockstep, and because HB.5 delivers the feature without it.
