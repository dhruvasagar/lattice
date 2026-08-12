# Intra-line diff refinement — slice plan

> **Status: Active.** Opened 2026-08-12. Implements
> [`diff-refinement.md`](../../architecture/diff-refinement.md): show
> *which part* of a changed line changed.

Design owns *what* and *why*; this file owns *when* and *in what
order*.

Builds on [`span-layering.md`](../../architecture/span-layering.md) —
refinement is a new layer on the **background** axis, which is the one
piece that does not exist yet.

## Status

| Slice | Title | Status |
|---|---|---|
| DR.1 | Word-level refinement in `lattice-diff` (pure) | ✅ |
| DR.2 | The background-overlay axis: `RefineSpan` → `Cell.bg` | ✅ |
| DR.3 | Magit diffs publish refinement | ✅ |
| DR.4 | `diff-mode` panes publish refinement | ✅ |
| DR.5 | Region-to-region refinement (unbalanced hunks) | 📝 |

DR.1 and DR.2 are independent and can land in either order; both gate
DR.3. DR.4 is a second consumer proving the mechanism generalised —
slip it without blocking anything. DR.5 replaces DR.1's pairing rule
and is the one remaining correctness gap.

---

## DR.1 — The computation ✅

Pure, in `lattice-diff`. Two strings in, changed ranges out.

- `refine_pair(removed: &str, added: &str) -> Option<(Vec<Range>, Vec<Range>)>`
  — word-level via `imara-diff` (already a dependency, generic over
  token type; no second diff implementation).
- Tokens: runs of `[A-Za-z0-9_]`, every other char its own token.
- `None` when the pair is too dissimilar (refined bytes > ~70% of
  either line) — "everything changed" is what the uniform tint already
  says, and marking it all is noise.
- Pairing lives here too: within a hunk, a maximal run of `-` lines
  followed by a run of `+` lines, paired **by position**, refined only
  when the runs are **equal length**.

**Tests.** A one-character change refines to one range; an identifier
rename refines to the identifier, not its neighbours; unequal runs
refine nothing; wholly-dissimilar lines refine nothing; a pure
addition (no `-` run) refines nothing; multi-byte UTF-8 ranges land on
character boundaries.

**Bench.** Refinement cost for a typical hunk, and the pathological
case (one long minified line) confirming the threshold short-circuits.

## DR.2 — The background axis ✅

The one genuinely new mechanism. Today `Cell.bg` comes from the row's
`DiffSignKind` and nothing can override a sub-range.

- `RefineSpan { start, end, kind }` + `RefineKind { Added, Removed }`
  in `lattice-cells`, beside `StyledSpan`.
- Buffers publish a per-line `Vec<RefineSpan>` alongside their existing
  per-line span list.
- The cells worker applies it when setting `Cell.bg`, **after** the row
  tint so a refined range overrides its row and everything else keeps
  it.
- Theme elements `diff.add.refine.bg` / `diff.remove.refine.bg` — a
  stronger version of the tint they sit inside.

**Do NOT add `bg` to `StyledSpan`** (design §3): it would couple
foreground precedence to background precedence and tax every existing
span producer with a concern it does not have.

**Tests.** A refined range gets the refine bg and its neighbours keep
the row tint; no refine spans ⇒ byte-identical output to today; a
refine span outside the line's byte range is ignored rather than
panicking.

**Renderer parity.** TUI and GPUI in the same patch, per the standing
rule. `grep -rn "RefineSpan" crates/lattice-ui-gpui/` must be non-empty
at the end of this slice.

## DR.3 — Magit publishes it ✅

> **Landed without the `diff.refine` option.** The plan specified one
> (default on). It is not there: refinement rides the existing
> `magit.hunk.syntax-highlight` gate's sibling path and adds no new
> user-facing switch yet. Deferred rather than dropped — add it if
> anyone wants the uniform look back.
>
> **The equal-length pairing rule bites more than expected.** The
> module's own `RUST_DIFF` fixture (replace one line, add another) is a
> 1-vs-2 run and refines *nothing*. That is DR.1 refusing to guess, and
> it is correct, but "replace a line and add another" is a very common
> shape — so refinement will often appear absent. Pinned as its own
> test. This is the evidence for prioritising the deferred
> similarity-scored pairing.

- `hunk_syntax` (which already owns what is inside a magit diff,
  `span-layering.md` §6) computes refinement during the async refresh
  and publishes it with the text.
- Covers all five diff-bearing majors at once — status, diff, commit,
  revision, stash-show — because they already share that helper.
- Option `diff.refine` (default **on**): the feature is the
  convention, and someone who wants the old uniform look opts out.

**Tests.** A status buffer with a one-word change carries a refine
span on that word only; a pure addition carries none; toggling
`diff.refine` off reverts to today's spans exactly.

## DR.4 — `diff-mode` publishes it ✅

The second consumer, and the reason DR.1 lives in `lattice-diff`
rather than in magit.

**Computed at diff time, not render time.** `render_rows` only receives
the BASELINE source, so it cannot pair lines even in principle.
Refinement is therefore filled in `two_way`, where both ropes are in
hand, and carried on the `Hunk` itself — not in a parallel map keyed by
hunk index, so it cannot desync from the ranges it describes. The
overlay reads it; nothing recomputes.

That also means the two consumers share one computation with no
redundancy: magit works from diff *text* through `styled_diff`,
`diff-mode` works from *hunks* through `Hunk.refine`, and both resolve
the same `diff.remove.refine.bg` theme element.

**Scope note.** Only the deletion-block (baseline) side renders today —
that is what `render_rows` emits. The added-side ranges are computed
and carried on the hunk, ready for whichever surface wants them.

**Tests.** A Change hunk carries refinement whose ranges slice the
changed word out of the SOURCE line; an Add carries none; unequal runs
decline (same rule as magit's path, so the two consumers agree); the
vec is aligned with `ranges[0]` so consumers can index by
`line - range.start`, which is what the overlay does.

---

## DR.5 — Region-to-region refinement 📝

**The gap, reported from use (2026-08-12).** A hunk that removes one
line and adds twelve — "rewrite this line, and add a doc comment above
it", one of the commonest shapes there is — renders with no refinement
at all. `refine_runs` returns empty whenever
`removed.len() != added.len()`, so the whole hunk falls back to the
uniform tint.

**DR.1's justification for that rule was factually wrong**, and the
comment saying so is in the code: *"Magit declines the same case."* It
does not. `magit-diff-update-hunk-refinement` hands the hunk's **whole
removed region** and **whole added region** to `smerge-refine-regions`,
which word-diffs the two concatenated texts. Unbalanced hunks are not
a special case there — they are the ordinary case, handled by not
pairing lines in the first place. Confirmed against two screenshots: a
1↔1 hunk refines (as ours does) and a 6↔2 hunk also refines (ours does
not).

The deferred "similarity-scored pairing" idea below is therefore
answered rather than adopted: pairing is the wrong frame. Diff the
regions, then map the byte ranges back onto lines.

- Concatenate the hunk's removed lines and its added lines; word-diff
  the two regions with the existing tokenizer + `imara-diff` pass.
- Map the resulting byte ranges back to per-line ranges on each side.
  This changes the carried shape: `Vec<Option<LineRefinement>>` is
  pair-aligned and cannot express *n* removed against *m* added, so the
  hunk carries per-side, per-line range lists instead.
- Keep the `MAX_REFINED_SHARE` bail-out — "nearly all of it changed"
  is still noise — but evaluate it per region, not per pair.
- **One pipeline, both axes.** The computation stays in
  `compute::fill_refinements`, called from `two_way`, so render and
  refresh keep reading one precomputed answer (DR.4's property) rather
  than each deriving its own.
- **TUI and GPUI in lockstep.** Both consume `Hunk.refine` through the
  same overlay, so the shape change must land in both renderers' read
  sites in the same patch — the audit is
  `grep -rn "refine" crates/lattice-ui-gpui/ crates/lattice-ui-tui/`.

**Tests.** A 1↔12 hunk refines the changed token on the removed line
(the reported case, verbatim); a 6↔2 hunk refines on both sides; the
existing 1↔1 cases keep their current output exactly (DR.5 must be a
superset, not a replacement that shifts balanced results); a wholly
rewritten region still declines; refinement survives a `gr` refresh
with the same spans it had on first render.

---

## Deferred

- **Similarity-scored pairing** for unequal `-`/`+` runs —
  **superseded by DR.5**, which removes pairing rather than improving
  it. Kept here so the reasoning trail is visible: this entry was the
  recorded plan, and it was aimed at the wrong mechanism.
- **Character-level granularity** as an option. Only if word-level
  proves insufficient — the default must stay word-level (design §5).
- **Refinement in the unified `:diff` / patch surfaces** beyond magit
  and `diff-mode`, if any others grow diff rendering.
