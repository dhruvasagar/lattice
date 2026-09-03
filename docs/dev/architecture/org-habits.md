# Org habits — repeating tasks, and the consistency graph

Sequencing and status: [`slice-plans/org-habits.md`](../operations/slice-plans/org-habits.md).

A **habit** in org is a repeating task with a *range*: `SCHEDULED: <2026-09-03
Wed .+1d/3d>` on a headline carrying `:STYLE: habit`. It is due a day after you
last did it, and overdue three days after. The agenda draws a colour-coded bar
of the last three weeks so you can see the pattern rather than the next task.

This fragment records what lattice implements, what it deliberately does not,
and why the file format is the constraint.

## 1. The files have another reader

These are the user's own org files, opened by emacs too. That single fact
decides the model: **byte-compatible with `org-habit`, or the feature is a
regression.** A habit lattice marks done must be a habit emacs still
understands, and vice versa — otherwise the graph looks better here while the
files stop round-tripping, which no feature repays.

So: same `:STYLE: habit`, same repeater grammar, same `:LOGBOOK:` entries,
same `:LAST_REPEAT:`. Nothing new is written to the file.

**The richer tracking is derived, not stored.** Streaks, completion rate,
per-weekday patterns and a window longer than org's 21+7 are all computable
from the LOGBOOK that is already there. Inventing file syntax to store what
can be computed would be the inferior design wearing ambition's clothes — and
it would drift the moment a habit is completed in emacs.

A useful consequence: the graph works on **day one against real history**,
because emacs has been writing those LOGBOOK entries for years.

## 2. The repeater grammar

`<date REPEATER>` where the repeater is one of:

| form | meaning | shift base |
|---|---|---|
| `+1d` | every day | the OLD timestamp — may land in the past |
| `++1d` | every day, but catch up | the old timestamp, advanced until future |
| `.+1d` | a day after you did it | **today** (the completion date) |
| `.+1d/3d` | habit range: ready after 1 day, overdue after 3 | today |

Units: `d` day, `w` week, `m` month, `y` year.

The `/MAX` suffix is habit-only and is what the consistency graph's colours
read. A habit without it is still a habit; `MAX` defaults to `MIN`, which
makes every day either done or overdue.

**Only `.+` uses the completion date.** That distinction is the whole reason
`+` and `.+` both exist, and getting it wrong is the difference between "water
the plants every 3 days" and "water the plants on the 1st, 4th, 7th no matter
when you last did".

## 3. What completing a habit does

Org's `org-auto-repeat-maybe`, reproduced:

1. Shift `SCHEDULED` (and `DEADLINE`, if it carries a repeater) per §2.
2. Reset the keyword to `:REPEAT_TO_STATE:` if the headline has one, else the
   first non-done keyword of its sequence.
3. Write a state-change line into the `:LOGBOOK:` drawer:
   `- State "DONE" from "NEXT" [2026-09-03 Wed 09:14]`.
4. Set `:LAST_REPEAT: [2026-09-03 Wed 09:14]`.

**The headline never actually stays DONE**, which is the part that surprises
people reading the code: a repeating task's completion is recorded in the log,
not in the keyword. The graph is built from those log lines, so step 3 is not
bookkeeping — it is the data.

`org-log-into-drawer` decides whether step 3 goes into `:LOGBOOK:` or sits
loose under the headline. Lattice honours the option; the user's config sets
it, and a plugin that ignored it would scatter log lines through files emacs
would then re-file differently.

## 4. The consistency graph

One column per day over a window (org: 21 preceding + 7 following). Each day is
one of four states, and the colour is the whole point. With `due` the day the task becomes ready and `late = due + (MAX - MIN)` the
deadline day:

| state | when | org face |
|---|---|---|
| **clear** | `d < due` — before the habit is due again | `org-habit-clear-face` (blue) |
| **ready** | `due <= d < late` — you may do it | `org-habit-ready-face` (green) |
| **alert** | `d == late` — the deadline day itself | `org-habit-alert-face` (yellow) |
| **overdue** | `d > late` — you missed it | `org-habit-overdue-face` (red) |

**Alert is a day, not a mood.** An earlier revision of this table said "past
MIN, approaching MAX", which reads like a band and invites the guess that
yellow means "today, and you should get on with it". It is neither: `alert` is
the deadline day itself, in every column of the window, past or future, and red
is only ever *past* it. Completing ON the deadline day is completing in time,
so that cell is green. Verified against `org-habit-get-faces`, with
`deadline = scheduled + (MAX - MIN)` from `org-habit-parse-todo`.

A day you completed on is drawn with the completed glyph regardless of state.

**Eight faces, not four.** Each state has a solid and a muted variant. A future
column is muted; so is a past column that was neither missed nor kept. That is
what stops three weeks of ordinary days shouting as loudly as a miss, and it is
why the theme elements come in pairs (`org.habit.ready` /
`org.habit.ready.muted`).

**The trailing columns of a kept habit go green → yellow → red.** A future day
assumes you have not done it yet, so even a perfectly kept habit ends its window
in red. That run is the graph prompting you, not a record of failure — worth
stating because it looks like a bug, and a test asserting "a kept habit shows no
red" fails against correct output.

**`due` moves as you look back.** A past column is coloured by what was true
then, not by today's schedule, or every day before the last completion reads as
overdue. So `due` is recomputed per column from the completion that most
recently preceded it, and how depends on the repeater's base — `.+` counts from
when you did it, `+` from the stamp, `++` replays its catch-up hops. All three
branches are ported.

Colours resolve from the theme registry as elements the mode owns
(`org.habit.clear`, `.ready`, `.alert`, `.overdue`), so `:colorscheme`
recolours the graph — the lesson OA.16's hardcoded hex left behind.

Glyphs follow the icon-degradation rule: a Nerd-Font palette and a BMP
fallback occupying the same cell width, so the column geometry does not shift
on toggle.

## 5. Where it is drawn

A `VirtualRow` anchored **Below** the habit's agenda row — the mechanism the
clock report and the magit headerline already use.

Not org's inline column-50 placement, and the cost is stated rather than
hidden: the habit block is twice as tall as emacs'. Org can write at column 50
because its agenda line is generated text; a lattice agenda row is a **real
excerpt of the source line**, and appending to it would mean editing the file.

The denser answer is a general per-row inline-annotation slot — today's inline
diagnostic is a bespoke, cursor-line-only version of exactly that. Generalising
it would serve inlay hints, blame and lenses too, and would let the graph sit
at a column like org's. That is a host mechanism in both renderers and is
deliberately deferred; see the slice plan.

## 6. What is not built

- **`org-habit-show-habits-only-for-today`** and the other display toggles —
  options with no consumer yet.
- **Habits in a non-agenda buffer.** The graph is an agenda-row decoration; an
  org file shows the raw `SCHEDULED` line, as emacs does.
- **`3× a week` targets.** A real gap in org-habit, and not fixable in the file
  format without inventing syntax (§1). The honest approximation is a derived
  analytic over the completion history — a rate, not a schedule.
