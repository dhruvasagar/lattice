# Concealment

Status: design fragment (2026-08-29). Slice plan:
`../operations/slice-plans/conceal-and-org-links.md`.

Anchors: `display-line.md` (the `DisplayLine` substrate this extends),
`cell-grid-renderer.md` (the two renderer peers), `design.md` §5.6 (rich text —
this closes the carve-out named there), `org-mode.md` §OM.10 (the first
consumer), paramount goal #1.

Markup that exists to be *parsed* does not have to be shown. An org link reads
`[[id:6F398E54-7E63-4492-9EB6-89C8A90E7DD3][Project Kickoff Checklist]]` on
disk and should read `Project Kickoff Checklist` on screen. Concealment is the
display-time elision that makes that true without touching the file.

## Why this exists

`design.md:3414` named it and deferred it:

> **Carved out and kept for v1:** *concealment* — hiding `**`, `[]()`, heading
> `#` markers on lines the cursor is not on. It is what actually makes markdown
> editing feel rich in Vim, Emacs and Helix alike, it costs no shaped path, and
> it works identically on both peers. Not yet implemented (no `conceal` support
> exists in the tree).

Every reference editor has it — vim's `conceallevel`, emacs' invisible-text
overlays, Helix and Zed's markdown rendering — and what the user carries between
them is *visual*, not grammatical. That puts this under the **UX-follows-
convention** rule: the convention is that markup collapses and the described
text remains, and there is nothing to arbitrate about the outcome. What is not
settled by convention, and is the whole of this design, is **who decides which
bytes are concealed** and **where the elision is applied**.

There is a second, sharper reason it can no longer wait. Help buffers already
render links, and they do it by *stripping* `[label](url)` down to `label` when
the buffer is built, keeping byte ranges as metadata
(`lattice-help/src/lib.rs:196`). That works because a help buffer is synthetic
and read-only. It cannot be the answer for a file the user edits: you may not
delete text from someone's document to make it look nicer. Once any editable
buffer wants rendered links — which org does — display-time elision is the only
honest mechanism left.

## What conceal is, precisely

```
source        * See [[id:6F398E54-…][Project Kickoff Checklist]] before Friday.
display       * See Project Kickoff Checklist before Friday.
                    └────────────┬───────────┘
                  the description survives; the target and the
                  brackets around it occupy no display columns
```

Two properties fall out of the picture, and most of the design is arranging for
them:

1. **The display line is shorter than the source line.** Every mapping from a
   source byte to a screen column — cursor, search highlight, selection,
   decorations, marginalia — must agree about that, or they disagree with each
   other and the user sees a caret one place and a highlight another.
2. **Which bytes are hidden is a property of the line's text**, not of the
   cursor and not of the parse tree. That is what makes it cheap, and §"Rejected
   alternatives" is mostly about why the other two candidates are not.

## Where the elision is applied

`DisplayLine.text` is already the final display string — inlays spliced, tabs
expanded, whitespace markers substituted (`display_matrix.rs:75`) — and both
renderers are already thin mappers over it. So **conceal bakes into `text`**.

This is not a convenience. `lattice-ui-tui` and `lattice-ui-gpui` are peers, and
the standing cross-renderer rule exists because parity maintained by discipline
decays. A renderer-side "skip these columns" would be that discipline: two
implementations of the same elision, drifting on the next feature that touches
either. Baking into the substrate makes parity **structural** — a renderer that
does nothing new is already correct.

## Data model

Three changes, all in `display_matrix.rs` and `lattice-cells/src/version.rs`.

**`col_map` goes signed.** Today it is `(source_byte, extra_display_cols)`, an
unsigned count of columns *inserted* ahead of a byte. Conceal removes columns
where inlays add them; they are the same axis with opposite sign, so the field
becomes `(u32, i32)` and `byte_to_combined_col` walks it with
`saturating_add_signed`. One table, not two — a second parallel table would let
the insert and the elide disagree about the same byte, which is the class of bug
that produces a caret half a glyph off.

**A `conceals` list.** `Arc<[(u32, u32)]>`, sorted source-byte ranges. It exists
because the signed walk alone is not sufficient: a byte *inside* a concealed
range has no column of its own, and the walk would hand back a partial one.
`byte_to_combined_col` clamps such a byte to the column of its range's start.
Every consumer that maps source bytes to columns already goes through that one
function, so they all inherit the clamp rather than each re-deriving it.

**A `conceal` axis on `MatrixVersion`.** Painting-class, not rebuild-class — the
distinction `indent`'s doc comment already draws (`version.rs:57`): axes that
change display *text* gate painting, so a version mismatch drops the viewport to
raw text for one frame. That is the correct degradation here and it is the same
frame the mode transition was going to repaint anyway.

## Cursor and coordinates

**Motion semantics are deliberately unchanged.** `l` across a concealed link
walks the source bytes; the caret sits at the link's start column until it
leaves the concealed range, then resumes. This is vim's behaviour under
`conceallevel`, so it is what the muscle memory expects.

The alternative — teaching motions to skip concealed bytes — would put display
state into the grammar. Motions are the public command API (paramount #3), they
are shared with operators, counts, macros and plugins, and making them depend on
what is currently visible would mean `dw` recorded in a macro replays
differently under a different `conceallevel`. The narrow win is not worth that.

## Declaring the rules

The rules are contributed, not hardcoded. `language.wit`'s spec grows:

```wit
/// One display-time elision rule. The host compiles the pattern once at
/// language registration and matches it against each display line during a
/// matrix rebuild.
record conceal-rule {
    /// Regex matched against a single line. Anchoring is the rule's
    /// business; the host adds none.
    pattern: string,
    /// 1-based capture-group indices whose spans are hidden. Group 0 (the
    /// whole match) is rejected at registration — a rule that hides its
    /// entire match is a deletion, not a concealment, and is almost
    /// always a mistake in the pattern.
    hide: list<u32>,
}
```

Org's two rules, which are the whole of its link rendering:

```
(\[\[[^]]+\]\[)[^]]+(\]\])     hide [1, 2]   described link
(\[\[)([^]]+)(\]\])            hide [1, 3]   bare link — target stays visible
```

The bare-link rule leaves the target on screen on purpose: emacs does the same,
and hiding a link whose only text *is* its target would leave nothing to click.

The host learns "hide these capture groups of this pattern". It does not learn
what an org link is — paramount #2 holds by construction, and markdown's `**`,
`__` and `[text](url)` are the same shape with no further host work.

## Scoping and invalidation

The concealed set is a function of `(the language's rules, the modal state)`.

| Modal state | Links render |
|---|---|
| Insert, Replace | raw — the source text, exactly as on disk |
| Normal, Visual, Select, Operator-pending, Command, Search, Prompt | concealed |

**Insert reveals buffer-wide, not just the cursor's line.** This was chosen
explicitly over the vim `concealcursor` default, and the cost is real and worth
stating rather than burying: entering Insert repaints every visible line that
carries a concealed range, so `i` is a viewport-wide visual event rather than a
one-line one.

Two things make it acceptable. It is *caused* — the user pressed `i`, and the
UX contract's target is unrequested pixel change, not requested mode change. And
it is precedented: `:set list` already repaints the viewport on a whitespace-
marker toggle through the same class of axis. The mental model it buys —
**Normal is the reading view, Insert is the editing view** — is worth one
repaint at a mode boundary that is already a visual event.

| Source | Effect |
|---|---|
| Edit | matrix rebuild → conceal recomputed over the covered lines |
| Modal state crossing the insert/non-insert boundary | conceal axis bump → viewport rebuild |
| Language registration / plugin teardown | rules recompiled; axis bump |
| Buffer whose language declares no rules | **nothing at all** — no bump, no match, no cost |

That last row is load-bearing. Mode changes happen constantly and in every
buffer; a conceal axis that bumped globally would put a viewport rebuild on
every `i` in every Rust file in the editor. The axis is gated on the buffer's
language having compiled rules, so a buffer with none never enters the path.

## Per-frame renderer work

None. Conceal is resolved during the matrix rebuild, which is O(viewport lines)
and off the UI thread. Both renderers read `text` + `runs` exactly as they do
today.

The matching cost is one regex pass per rebuilt display line per rule. Org has
two rules; a 200-column line against a linear-time engine is nanoseconds, and it
happens on rebuild, never per frame. The bench (heuristic #5) records rebuild
time for a viewport of link-dense org against the same viewport with rules
disabled, so the axis's cost is visible in `benchmarks.md` rather than asserted
here.

## Failure behaviour

- **A pattern that does not compile** is logged at `warn` once at registration
  and skipped; the language's other rules still apply. A plugin does not lose
  its whole language over one bad regex.
- **A `hide` index naming a group the pattern does not have** is skipped the
  same way, at registration — not per line, where it would log at rebuild rate.
- **`hide: [0]`** is refused at registration with the reason, per the WIT comment
  above.
- **Rules are capped per language** (a small fixed bound) and each pattern is
  length-bounded. Not because the engine backtracks — it does not — but because
  an unbounded rule list turns a rebuild into a linear scan of someone else's
  configuration.
- **Overlapping matches**: the union of concealed ranges is taken, sorted and
  coalesced before the line is built. Two rules hiding overlapping spans produce
  one hidden span, never a double-elision that would corrupt the column map.
- **A stale or absent matrix** already drops the renderers to plain rope text.
  Links are raw for that frame and concealed on the next publish — the correct
  degradation, and invisible beside the fallback already happening.

## Rejected alternatives

**A tree-sitter `@conceal` capture.** This is how Helix and Neovim do it, and it
is the option to beat. It fails here twice. First, concretely: `tree-sitter-org`
does not model links at all — there is no `link` rule in its grammar, and
`[[id:X][Title]]` is undifferentiated `expr` tokens inside `item` or
`paragraph`. There is nothing to capture. Second, and the reason this would stay
wrong even if the grammar were extended: **the tree is absent during a reparse**,
so tree-driven conceal would flicker between concealed and raw as the user
types. That is a pixel change to content the user did not edit, which is a
standing veto. `links.rs` already recorded the same reasoning for not reading
the tree when it finds links.

**A plugin `conceal` seam.** A guest returning ranges for a line range would be
more expressive — arbitrary logic could decide what to hide. It costs a WASM
crossing per viewport rebuild (~25 µs, tolerable) and a new seam (real surface),
and buys expressiveness nothing on the roadmap wants: every conceal rule in
sight, in org and in markdown, is a function of the line's own text. Heuristic
#1 cuts both ways and cuts against this one: the bigger mechanism is not the
better design when no consumer needs the extra power.

**Renderer-side elision.** Cheapest to write, and it makes TUI/GPUI parity a
matter of discipline rather than structure. Rejected under the standing
cross-renderer rule for exactly that reason.

**Stripping at buffer build, as help does.** Correct for synthetic read-only
buffers and unavailable here: the buffer is the user's file.

## Paramount-goal alignment

**#1 Performance.** No per-frame work and no new UI-thread work. The added cost
is a bounded regex pass during an off-thread rebuild that was already happening,
gated off entirely for languages with no rules. Benched by name.

**#2 Extensibility.** The host gains a rule *evaluator*, not a rule. Org
contributes org's patterns through the existing `language` seam; markdown will
contribute markdown's; neither is named in the host.

**#3 Vim modal editing.** Motions are untouched, deliberately — see §Cursor and
coordinates. The mode-scoped reveal is itself modal-editing-shaped: Normal reads,
Insert edits.

**#4 Asynchronicity.** Conceal resolves inside the existing off-thread matrix
build and publishes with it. No new wake, no new staleness axis beyond the
version stamp the matrix already carries.

## Deferred

- **Replacement characters (vim's `cchar`).** Hiding a span and substituting one
  glyph — a markdown horizontal rule as `─`, a checkbox as `☑`. `conceal-rule`
  would grow a parallel `replace-with` list. Not built because every rule in the
  first two consumers is pure elision, and the WIT is regenerated from
  `lattice-wit` (WT.2), so adding the field later reaches guests by
  regeneration rather than by hand.
- **Per-rule reveal policy.** Today the insert/non-insert split is global. A rule
  that wants different behaviour — always concealed, or revealed on the cursor
  line only — would carry its own policy. No consumer has asked.
- **Soft wrap.** `wrap_width` is 0 today (`display-line.md`). A concealed range
  spanning a wrap point is a question that cannot be answered before wrapping
  exists.
- **Conceal in the gutter and in virtual rows.** Both read the display line
  through the same helpers, so they follow automatically for elision; a
  replacement glyph with a different width would need re-checking.
