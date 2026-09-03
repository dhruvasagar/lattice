# Slice plan — org habits

Design: [`org-habits.md`](../../architecture/org-habits.md).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

| Slice | Description | Status |
|---|---|---|
| HB.1 | The repeater grammar, parsed **(plugin)** | ✅ |
| HB.2 | Completing a repeating task repeats it **(plugin)** | ✅ |
| HB.2b | …and from the agenda row, through the source seam **(plugin)** | ✅ |
| HB.3 | Completion history from `:LOGBOOK:` **(plugin)** | ✅ |
| HB.4 | The consistency graph — states and glyphs **(plugin)** | ✅ |
| HB.5a | The `annotation` seam — WIT + boundary **(lattice)** | ✅ |
| HB.5b | Annotations become virtual rows **(lattice)** | ✅ |
| HB.5c | Org fills it — the graph appears **(plugin)** | ✅ |
| HB.6 | Derived analytics: streak, rate, weekday pattern | ✅ |
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

### HB.2b — …from the agenda row ✅

**Completing a habit from the agenda took the destructive path**, and now does
not.

An agenda excerpt is ONE line — the headline — so `set_keyword_repeating` read
the subtree from the ACTIVE buffer, got the headline alone, `complete_repeating`
correctly found no planning line to shift, and the fallback wrote a plain
`DONE`. That is not a failure to repeat: it *destroys the habit*, because the
headline stops being scheduled and the completion is never recorded.

The fix reuses `PlanTarget` — the same resolution `s` / `d` already do — rather
than growing a second answer to the same question. `PlanTarget` gains
`line(n, doc)` (generalising the old `read_line_of` from the headline to any
line) and `subtree_end(doc, tree)`: the tree-sitter walk in a file, and behind
the agenda a new `headline::subtree_end_unbounded`, because the source seam
reads one line at a time and will not say how long the document is. Passing
`u32::MAX` to the counted walk would make "no headline follows" a
four-billion-line scan on a keystroke path, so the end is discovered by reading
until the document runs out.

The done-state gate now runs BEFORE the target is resolved, so `TODO` → `NEXT`
costs no host call. The cursor is not translated: the edit lands in the source,
the caret is in the view and stays put.

Deliberately no test in `org_agenda.rs` pinning the OLD answer — that would have
enshrined the destructive behaviour as intended. The org-file case is covered in
`org_structure.rs`, and must not change: a plain `* TODO Ship it` still just
becomes `DONE`.

**The diagnosis this slice carried for two sessions was wrong, and the way it
was wrong is the reusable part.**

It said `row_source(ctx)` → `excerpt_source(view, line)` returned `None` for a
chord-dispatched guest action, on the evidence of an in-buffer probe reporting
`from_source=false`. That reading was never checked against the code path the
chord actually takes: **`set_keyword_repeating` did not call `row_source` at
all.** It never resolved a target, so `from_source` was a property of a
different function than the one that was failing.

The seam is fine, and is now proved fine rather than assumed either way, by five
tests on the lattice side that did not exist:

- `lattice-multibuffer` — the resolver against a view built by `open_scan_view`,
  not a hand-assembled one (the existing unit tests all built their own).
- `lattice-plugin-host/tests/excerpt_source_seam.rs` — a real WASM guest on the
  *sync* grammar seam calling `excerpt-source` with the ids from its own
  context. The store the guest runs in is what decides the answer, and
  `instantiate_grammar_plugin` strips a neighbouring field (`ui`) off that same
  store on purpose, so "every store gets it" needed asserting.
- `boot_regression_pins` — `WiredSeams` now reports the excerpt-source resolver
  and the multibuffer registry, via a new `PluginHost::excerpt_source_wired`.
  An unwired seam answers `none`, which is also its answer for "not composed",
  so a boot-ordering regression here is invisible from the guest.
- `an_agenda_row_can_write_to_its_source.rs` — the id the dispatch gate puts in
  a grammar action's context is the view's.
- `a_plugin_action_on_an_agenda_row_finds_its_file.rs` — all of it at once: real
  `Editor`, real multibuffer view, real dispatch path, real guest, resolver
  wired the way `install` wires it. Excerpts deliberately non-contiguous, so
  composed row 1 is source line 2 and a seam echoing its argument back would
  fail.

Four of those layers were already green *individually* before this slice, and
the chain still could not be said to work — because each test supplied by hand
what the next one produces. That is the shape to watch for, not this particular
seam.

Two notes for the next debugging session here:

- **`logging::log` cannot be used in this plugin.** Calling it makes the
  component import `logging`, which the sync grammar linker deliberately does
  not provide, and every import must resolve on every linker the component
  instantiates against — so the WHOLE plugin stops loading. OC.2 shipped that
  once. `Effect::Echo` is no better from the plugin repo: it lands in the
  message area. What works is an edit into the buffer, read back through
  `source_text` — or, better, the fixture route above, where an `Echo` IS
  readable because the test holds the `Editor`.
- **The test that would have caught this does not exist.** Marking a habit done
  from a real agenda needs org's component inside lattice's test suite, which
  nothing does today. The `multiseam` fixture is the pattern; org would need the
  same treatment, or a shared harness that loads whichever component is on disk.

### HB.3 — completion history ✅

`history::completions` reads a subtree's state-change lines back into a sorted,
unique set of dates. `complete` writes them; this is the inverse, and
deliberately the more permissive half — **emacs wrote most of the lines it will
ever see**, which is exactly why the graph shows real data on day one.

What that permissiveness is for, concretely: org pads its log fields
(`State %-12s from %-12s`), so a parser splitting on single spaces reads every
file in the wild as having no history. Notes continue the line with ` \\`. With
`org-log-into-drawer` off the line sits loose under the planning line rather
than in `:LOGBOOK:`, and requiring the drawer would blank the graph for exactly
the users who turned the option off.

A completion is a state change **into a done keyword** — checked against the
user's sequence, not the literal word `DONE` — so `TODO` → `NEXT` is not one,
and clock lines and notes are ignored rather than guessed at. Reading stops at
the first nested headline: a child's `:LOGBOOK:` is the child's, and rolling it
up would make a parent look better kept than it is.

`:LAST_REPEAT:` is **added and deduplicated**, not used as a conditional
fallback. Deciding when a log counts as "trimmed" is wrong somewhere — a log
holding only older entries is neither empty nor complete — and adding-then-
deduping is correct in both cases and cannot lose a day.

13 tests, including the two round trips that matter: what this plugin writes,
and what emacs writes.

### HB.4 — the graph ✅

`habit_graph::build` gives one `Day` per column over org's 21+7 window.

**Ported from `org-habit.el`, not reconstructed from how the graph looks**, and
that distinction changed the answer. See design §4 for the corrected rule; the
short version is that `alert` is the deadline day itself rather than "today" or
a band, and the design fragment's own table used to imply otherwise. A question
put to Dhruva offered "org's rule" against "the band rule" with both
descriptions wrong about which was which — he chose to match org, and the
deadline-day rule is what org does.

Also ported: the pre-first-completion clear (with the very first done day as
`ready`), per-column recomputation of `due` for all three repeater bases, and
the solid/muted axis — eight theme elements, not four.

Glyphs in two palettes at one cell each per the icon-degradation rule; colours
resolved from the theme palette so `:colorscheme` moves them (OA.16's hardcoded
hex is the lesson). Org fills the cell with a *background*; a terminal cell here
carries a glyph the user reads, so the colour goes on the foreground.

10 tests. One is worth reading before writing another: `a_habit_kept_on_time_
shows_no_overdue_day` first asserted over the whole window and **failed against
correct output**, because the trailing seven columns of even a perfect habit go
green → yellow → red. A future day assumes you have not done it yet.

**Both slices landed in one commit**, deliberately: a parser with no consumer
cannot be warning-clean, and HB.4 is HB.3's only consumer. And both are still
dead code until HB.5 draws them — the wasm build carries 14 `dead_code`
warnings, three of them HB.1's, of which HB.4 now consumes `window_days`.

### HB.5 — drawn under the agenda row ✅

Design: [`org-agenda.md`](../../architecture/org-agenda.md) §5b, which is where
the seam is argued; `org-habits.md` §5 says why it is a virtual row and not
column 50.

The shape was chosen over a general `virtual-rows` producer seam (the
`decorations` shape) on **coordinates, not economy**: that seam hands a guest a
`decoration-context` whose buffer id is the COMPOSED view, and a scan guest
works in source terms — it cannot know where its row lands until the sort has
interleaved every other file's rows. `entry.spans` already documents exactly
that about itself, which is why the annotation crosses at the same point and is
translated by the same host-side machinery. The scan is also already the
trigger, so nothing new is scheduled.

Deliberately not bought: a non-scan plugin still cannot annotate a row. That is
HB.7, deferred.

**HB.5 is the first production emitter of `AnchorPosition::Below`.** Both
renderers handle it and `lattice-cells` orders it, but only under test — so
HB.5b owes an end-to-end assertion rather than a citation of those tests.

#### HB.5a — the seam ✅ *(lattice)*

`wit/scanned-excerpt-source.wit`: a one-line `annotation` record (text +
`list<display-span>`) and `entry.annotation: option<annotation>`. Native peer on
`ScannedExcerpt`, and the boundary conversion in
`lattice-plugin-host/src/scanned_excerpt_source.rs`, validated with `spans`'
granularity: a bad span costs itself, not the annotation; an annotation with no
surviving spans still renders its text; a row never vanishes because its
decoration was malformed.

Adding a field to a WIT record is a breaking change for every guest that
constructs an `entry` — org must add `annotation: None` before it builds again.
That is the cross-repo cost of this slice, and it is one line.

#### HB.5b — annotations become rows ✅ *(lattice)*

`publish_row_annotations`, sibling to `publish_row_spans` and sharing its
translation (`excerpt_start_rows`, so a row's annotation and its fold agree on
where the row is). The result is a `VirtualRowProvider` registered on the view
through `VirtualRowRegistrar` — `clock_report.rs` is the precedent.

Tests owed: composed-index translation with interleaved files (the case a
single-file fixture cannot catch, exactly as `composed_row_spans`' tests
established), a refresh replacing rather than appending, the empty case
registering nothing, and the end-to-end `Below` assertion above.

#### HB.5c — org fills it ✅ *(plugin)*

Compute the graph during the scan the guest already performs: `history` for the
completions, `habit_graph` for the states, glyphs per `ui.nerd_fonts`, spans
naming the eight `org.habit.*` elements. `annotation` stays `none` for every row
that is not a habit — an ordinary TODO grows no second row.

This is what makes HB.3 and HB.4 live: **dead code 14 → 0**.
`repeat::is_habit_range` was deleted rather than kept — it was HB.1's forward
declaration, and HB.5 proves nothing needs it because `window_days` already
defaults MAX to MIN for a repeater with no `/MAX` (design §2).

**What landed beyond the plan, and why.**

- The scan **cache** carries the annotation. A hit skips the guest call
  entirely, so an annotation absent from the on-disk form would make a graph
  appear on a file's first scan and vanish on every later one — read as the
  feature being broken, not as a cache being incomplete. Round-tripped through
  real `put`/`get`, because the failure is the field being dropped somewhere on
  the PATH and a `From` test passes while `put` ignores its argument.
- A slot on an annotation names a **theme element only**, unlike `entry.spans`
  which also takes tree-sitter capture names. A virtual row's cells carry a
  baked colour while a span stays a semantic style the cells worker resolves at
  paint. Stated in the WIT rather than left to be found by getting a black row.
- The provider reads its state **through the service**, not through a captured
  `Arc`, because `set_state` inserts a new one on every open. See the finding
  below.

**Found while building this, not fixed here.** `ClockReportProvider` captures
`Arc<RwLock<ScanViewState>>` in `on_activate`, and `set_state` REPLACES that
Arc on every open. If a manual clockreport toggle survives a `gr`, its report
should freeze at the pre-refresh data. That is a reading of the code, not a
reproduction. The class fix is to make `set_state` write into the existing lock
instead of replacing it, which would also retire HB.5b's indirection. Its own
slice.

### HB.6 — derived analytics ✅

Streak, completion rate and weakest weekday, from HB.3's data, stored nowhere.
Definitions and their rationale are in design §4b.

**The plan said "computed" and stopped**, which left the surface open — and a
computation with no consumer is exactly what left HB.3 and HB.4 dead across two
sessions. Dhruva chose the graph row, so the suffix rides the annotation HB.5
already draws: no new seam, no new crossing, no new trigger, and the same scan
produces both.

The units are the part that needed deciding rather than implementing. `5d` was
in the option preview and is wrong for any non-daily habit, so the streak ships
as a count of kept repetitions (`5×`); the rate's denominator is repetitions
rather than days for the same reason; and the weekday term is suppressed unless
the habit is daily with enough samples, because a `.+3d` habit's weekday
distribution is an artefact of its cadence.

`org.habit-stats` gates it, defaulting on. 16 tests here, 2 more in
`habit_row` pinning that the caption is unspanned so the graph's runs still tile
the graph exactly.

### HB.7 — a general per-row inline annotation slot ⛔

Deferred, with a real design behind it. Today's inline diagnostic is a
bespoke, cursor-line-only end-of-line annotation. Generalised into a per-row
slot any provider can contribute to, it would let the graph sit at a column
like emacs' (halving the habit block's height), and would serve inlay hints,
git blame and lenses.

Deferred rather than dropped because it is a host mechanism needing both
renderers in lockstep, and because HB.5 delivers the feature without it.
