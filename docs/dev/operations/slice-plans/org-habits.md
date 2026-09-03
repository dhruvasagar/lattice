# Slice plan — org habits

Design: [`org-habits.md`](../../architecture/org-habits.md).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

| Slice | Description | Status |
|---|---|---|
| HB.1 | The repeater grammar, parsed **(plugin)** | ✅ |
| HB.2 | Completing a repeating task repeats it **(plugin)** | ✅ |
| HB.2b | …and from the agenda row, through the source seam **(plugin)** | ✅ |
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
