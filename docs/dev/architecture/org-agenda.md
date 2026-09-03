# The org agenda as a dashboard

> **Where the code is.** Everything this page describes is implemented in
> [`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin), a **separate repository**. It
> is a WASM Component plugin: nothing here is compiled into the editor, and
> lattice has no `BufferKind::Org`, no `Lang::Org` arm and no `Editor::`
> method for any of it. What lives in *this* tree is the seams the plugin
> contributes through — see [`plugin-host.md`](plugin-host.md).
>
> **§5 and §5b are the exception, and it matters.** The `display-span` colour
> channel and the `annotation` row are lattice's contract with *any*
> `scanned-excerpt-source` provider — the scan view is a
> [`lattice-multibuffer`](multibuffer-views.md) mechanism and knows nothing
> about org. Org is its first and loudest consumer, not its owner. A reader
> looking for "what does the seam guarantee" wants those two sections; a reader
> looking for "what does the agenda do" wants the rest.

`org-mode.md` §6 establishes that the agenda is a multibuffer and why. This
fragment covers what it becomes: a view with agenda-aware colour, layered
display modes that each own one concept, a tags/todo query language, and
`org-agenda-custom-commands`.

Slice plan: [`slice-plans/org-agenda.md`](../operations/slice-plans/org-agenda.md).
User documentation: [`org.md`](../../user/org.md), and the plugin's own
`doc/org.md` for the full reference.

---

## 1. Three defects, and only one of them is cosmetic

The view reads as bland, and that word hides three separate causes with three
separate fixes. Naming them apart matters, because two are correctness bugs
wearing a styling complaint's clothes.

**The group header contract is not the one the substrate implements.**
`build_excerpts` gives the first row of a date group its label and every later
row `String::new()`, documented on the assumption that "an empty
`ExcerptHeader.title` renders no header row". `compose_header_rows`
(`crates/lattice-multibuffer/src/lib.rs:2740`) dedups on **`excerpt.source`**,
not on the title, and `header_cells` renders a titleless, pathless header as
`[untitled]`. A date group interleaves files by design, so every file change
inside one group emits another header row. One group drawn from two files
renders:

```
["2026-08-31 Mon (today)", "[untitled]", "[untitled]"]
```

This is a substrate bug, not an org bug — the agenda is the first provider to
want title-run grouping rather than source-run grouping, and it asked for it in
a comment instead of in code.

**Nothing paints the agenda as an agenda.** Rows are coloured by the *source
file's* tree-sitter grammar, resolved per excerpt. An agenda therefore looks
like org text that happens to be out of order. No keyword colour, no priority
emphasis, no tag treatment — not because the channel is missing (§5) but
because nothing writes to it.

**Rows are multi-line.** The guest runs `end_line` out to the planning line so
`SCHEDULED:` shows beneath the headline. That was a reasonable default when the
agenda had no other way to surface a date; it is not what a dashboard wants.

---

## 2. The substrate holds, and the alternative is worse than it looks

A magit-status-style synthetic buffer was considered and rejected. The
multibuffer's rows are live ranges into source `Document`s, which is what makes
`<CR>` land on the right file and what makes a TODO cycled in the agenda write
through to the source. A synthetic buffer is plain text and has to rebuild
both.

The decisive evidence is that both in-repo views which gave this up rebuilt it
*worse*. Magit carries a parallel `SectionIndex`. Compilation abandoned a line
map altogether and **re-parses the cursor line's text**, with its own comment
explaining that a line→entry map is not reliable under interleaving. The agenda
interleaves by construction. Buying look-and-feel with the thing the agenda
most depends on is the wrong trade, and §5 shows the look-and-feel was never
what the substrate was withholding.

---

## 3. Composition: two row kinds, and the boundary between them

A multibuffer row is an excerpt — `RowEntry` has exactly one variant, and
composition is line-copying out of source documents, so no synthesized text can
enter. Content mutation is `append_excerpts` (at the end) or
`replace_excerpts` (all of it); there is no per-section mutation, so **excerpt
content has exactly one producer** and two contributors would clobber each
other.

That constraint looked like it would force a section-keyed mutation API. It
does not, because the computed content does not belong in excerpts at all:

| Content | Kind | Owner |
|---|---|---|
| Headline rows | Excerpt | the agenda provider |
| Section / date headers | Virtual row | the agenda provider |
| Timeline strip | Virtual row | `org-agenda-timeline-mode` |
| Clock report | Virtual row | `org-agenda-clockreport-mode` |
| Log entries | Virtual row | `org-agenda-log-mode` |

A timeline strip and a clock summary have no source range and nothing to jump
to. `VirtualRow` already models exactly that, and models it far more richly
than its two current uses suggest: multi-line (`height > 1`), per-cell
foreground and background, per-column font scale, and an `Annotation` anchor
whose documented meaning is "content that scrolls with its anchor and paints no
backdrop of its own".

`register_virtual_row_provider` (`crates/lattice-mode/src/activator.rs`) keys
by `ProviderId` and dedups by id. The multibuffer registers two providers
today; nothing limits it to two. So each display mode registers its own in
`on_activate` and owns its rows outright, with no contention over the excerpt
list and no new mutation API.

**The cost, stated plainly:** virtual rows are display-only. You cannot put the
cursor on a clock-report line and act on it. Making those lines actionable
means excerpts over a synthetic pathless source, which is reachable natively
but not from a plugin — the guest excerpt record carries a `path` and the host
drops any row whose file it cannot read. That is deferred, not designed away;
§9 records what it would take.

---

## 4. Layering, and why these are modes rather than options

Log mode, clock report and the time grid are *modes* in emacs, toggled from one
dispatch menu, and they are modes here for the same reason the standing rules
would demand anyway: each owns a keymap entry, a toggle, a lifecycle and a body
of content, and "shared behaviour is a minor mode, never a copied keymap".

Each is `ActivationPolicy::Manual`, activated on the agenda view, and each
registers its virtual-row provider and any fold source in `on_activate` — the
magit-status pattern. `org-agenda-mode` remains the base: it owns the view's
identity, `foldlevel=0`, fold cycling (§6) and the TODO/priority chords it
already has.

Toggles follow `evil-org-agenda`, which is the authority for this repo's org
keys the way `evil-collection-magit` is for magit's:

| Chord | Effect |
|---|---|
| `gD` | view-mode dispatch transient (log, clock report, time grid, span) |
| `cr` | toggle `org-agenda-clockreport-mode` directly |
| `gr` / `gR` | refresh |

---

## 5. Colour: one seam, reusing the record that already solves it

The styling channel already reaches multibuffer views.
`PendingSyntheticHighlights::store_and_wake` feeds a buffer's `ExtraHighlights`
local, and the cells worker merges those over the per-excerpt syntax spans,
prepending them so they win first-match precedence. This is the same pipeline
magit-status paints with. Nothing about it is `BufferKind`-gated. The agenda
simply never writes to it.

So the host *could* paint the agenda today with no new seam at all. It should
not, and the reason is paramount goal #2 rather than taste: agenda colour is
org semantics — which keyword is a not-done state, which tag is a context,
which priority is urgent — and AG.1 already deleted the host's generic
`:agenda` on the grounds that it was "the host naming an org feature
generically and the plugin having no way to fix it". Reintroducing that as
colour would repeat it in a form that is harder to see.

The seam is a reuse, not an invention. `display-span` already exists for picker
rows:

```wit
record display-span { start: u32, end: u32, slot: string }
```

Critically it names a style by **string slot**, resolved host-side against the
theme, so org's already-registered `org.todo.*` elements resolve with no new
style vocabulary and no colour crossing the boundary. Carrying
`list<list<display-span>>` back from the guest's scan — or a
`publish-buffer-spans` import routed into `PendingSyntheticHighlights` — closes
the whole gap.

The division that results is the one to hold on to: **generic substrate
defects and capabilities are host-side, because every provider gets them; org
semantics stay in org.** The `[untitled]` fix and richer header cells serve
search, diff and references too. Keyword colour serves nobody but org.

---

## 5b. Annotations: what hangs *under* a row (HB.5)

Colour (§5) answers "how is this row painted". A habit's consistency graph asks
a different question — "what else does the guest know about this row that is not
in the row's own text" — and it cannot be answered the same way, because an
agenda row is a **verbatim excerpt of a source line**. Org writes its graph at
column 50 because its agenda line is generated text; ours is the file, and
appending to it would mean editing the file (`org-habits.md` §5).

So the answer is a `VirtualRow` anchored `Below` the row, and the seam question
is only where the guest's contribution crosses.

### It rides the entry, for the same reason `spans` does

```wit
record annotation {
    text: string,
    spans: list<display-span>,
}
```

`entry` gains `annotation: option<annotation>`. Not a new producer seam, and the
argument is coordinates rather than economy.

A general `virtual-rows` producer — the shape `decorations` uses, and the honest
generalisation — would hand the guest a `decoration-context` carrying a buffer
id and a line count. For a multibuffer **that is the composed view**, and a scan
guest works in source terms: it cannot know where its row lands until every
other file's rows have been interleaved by the sort. That is not a new
observation; it is exactly what `entry.spans` already documents about itself,
and it is why spans are line-relative and translated host-side. An annotation is
the same kind of thing arriving from the same place, so it crosses at the same
point and is translated by the same machinery.

The scan is also already the trigger. A producer seam would need the host to
decide *when* to re-run it; the graph changes when the file changes or the day
rolls over, which is when the agenda rescans. Nothing new to schedule, and
nothing new on a hot path (paramount #1).

**What this deliberately does not buy.** A plugin that is not a scan-view
provider still cannot annotate a row. That is HB.7's general per-row slot,
deferred with a real design behind it — and building the general mechanism for
one consumer is the failure mode heuristic #1 names. When a second consumer
appears, the general seam is the right answer and this one is not in its way:
the entry field is a producer *contribution*, and a host-side registry could
merge both.

### Shape, and what is refused

- **One line, not many.** The graph is one row. A `list<annotation>` is the
  obvious widening when something needs it; heights and scroll interactions are
  not worth inventing for a consumer that does not exist.
- **`spans` over the annotation's own text**, byte offsets into `text`,
  resolved through the same `name_to_style_with_theme` path — so org's eight
  `org.habit.*` elements reach the row with the active colourscheme applied and
  no colour crosses the boundary.
- **Validated, never trusted.** Same rule as `spans`, and the same granularity:
  a bad span costs itself, not the annotation; an annotation whose spans are all
  bad still renders its text. A row must never vanish because its decoration was
  malformed.

### Host side

`publish_row_annotations` is `publish_row_spans`' sibling and shares its
translation: `excerpt_start_rows` gives each row's composed index, so a row's
annotation and its fold agree on where that row is. The rows become a
`VirtualRowProvider` registered on the view through `VirtualRowRegistrar` —
`clock_report.rs` is the precedent, a scan-view provider already doing exactly
this.

**HB.5 is the first production emitter of `AnchorPosition::Below`.** Both
renderers handle it and `lattice-cells` orders it, but only under test, so the
slice owes an end-to-end check rather than a citation of those tests.

---

## 6. `<Tab>` cycles the block

The agenda opens collapsed (`foldlevel=0`, declared on `org-agenda-mode`) and
has no `<Tab>` to open it again. `org-cycle` is bound on the `org-mode`
**major**; the agenda's major is `multibuffer-mode`, so it never fires there,
and the only fold keys available are the core `z` chords.

`<Tab>` cycles the block under the cursor and `<S-Tab>` cycles all blocks —
`org-cycle` / `org-global-cycle` semantics, applied to the structure the agenda
actually has. With headline-only rows (§1) there is no subtree inside a row to
cycle, so blocks are the only meaningful granularity.

This is a deliberate deviation from `evil-org-agenda`, which binds bare `<tab>`
to `org-agenda-goto`. That command stays reachable on `g TAB`, where the same
config also binds it. The deviation is recorded here rather than left to look
like an oversight.

---

## 7. Rows carry tags, which the query language needs first

`Row` is `{ line, end_line, date, priority, keyword }`. It carries **no tags and
no properties**, and the scan extracts neither. Tag matching is therefore not a
parser bolted onto `Filter`; it needs the row model and the tree-sitter scan
extended first, and that ordering is the reason custom commands are not the
small item they look like.

The match grammar is scoped to what real configurations use:

```
"NOTE"                              a tag
"-CANCELLED/!"                      exclude a tag; not-done keywords only
"-CANCELLED/!NEXT"                  not-done AND keyword NEXT
"-CANCELLED+WAITING|HOLD/!"         (-CANCELLED AND +WAITING) OR (HOLD)
"STYLE=\"habit\""                   property equality
```

So: `+`/`-` conjunction, `|` alternation, a `/` TODO section with `!` and
explicit keywords, property equality. Org's fuller syntax — regexp tags
(`{^work}`), numeric property comparison, `LEVEL=` — is out until something
asks for it.

Like `Filter`, the parsed match is **data, not a closure**, for the reason
`Filter` already records: a filter that is data can be parsed from a config
file, and a closure cannot.

Emacs `org-agenda-skip-function` entries are out of scope permanently in this
form. They are arbitrary elisp with no equivalent, and a named-predicate seam
for them is speculative until someone writes one.

---

## 8. Custom commands, and two menus that are not the same menu

`org.agenda-custom-commands` is a string option whose value is TOML, shaped
like `org.agenda-sections` and for the identical reason — an option is
`boolean | integer | string`, and a list of records cannot reach one otherwise.
One option serves `lattice.toml` and `init.rs` alike with no second seam.

```toml
[[command]]
key = " "
description = "Agenda"

  [[command.section]]
  title = "Tasks to Refile"
  match = "REFILE"

  [[command.section]]
  title = "Waiting and Postponed"
  match = "-CANCELLED+WAITING|HOLD/!"
  todo-only = true
```

Failure behaviour is inherited wholesale from `org.agenda-sections`: a
malformed set falls back to the built-ins with the parse error ridden onto the
first section's title, and one unusable entry is skipped and named rather than
failing the set. The reasoning there — that an empty agenda and a
correct-but-empty agenda are indistinguishable, and "you have no tasks" is the
worst thing this view can say incorrectly — applies unchanged.

**Two menus, deliberately.** Emacs separates them and so does this:

- **The dispatcher** lists custom commands and opens one. It takes
  `<leader>oa` / `C-c a`, which today open the default agenda directly. That
  is a behaviour change, and it is the emacs-faithful one: `C-c a` has always
  been "choose an agenda".
- **The view-mode dispatch** (`gD`) toggles display modes *within* an open
  agenda. It changes how you are looking, not what at.

Both are transients. No new mechanism is involved: `lattice-picker`'s transient
system exists, magit is its first consumer, and org already ships two menus
through the WIT `transient-source` seam. The one constraint is that
`transient-source::id()` is one per guest, so the agenda menus branch inside
org's existing `build` on `args` — the shape capture's two menus already
established.

---

## 9. Deferred, with the cost recorded

- **Actionable computed rows.** Timeline and clock-report lines are virtual and
  therefore inert. Making them actionable needs excerpts over a synthetic
  pathless source: natively that is `add_source` with a `Document::from_text`,
  but the guest seam has no arm for it — `multibuffer-view-excerpt` carries a
  `path` and the host drops rows whose file it cannot read. A `text` arm plus
  routing in `plugin_view.rs` is the smallest version.
- **Per-section fold rules.** `FoldGrouping` is one enum fixed at view
  creation, so a timeline section and a headline section cannot fold by
  different rules. Plugin-owned views additionally hardcode
  `FoldGrouping::SourceFile`, so a plugin cannot even request the grouping the
  agenda needs.
- **Skip functions.** See §7.

---

## 10. Paramount-goal alignment

**#1 Performance.** Virtual-row providers are `collect()`-on-demand and
versioned; the scan already runs off-thread under `spawn_blocking`. The added
work is span computation over rows already in hand, which is O(visible rows),
not O(corpus). No new UI-thread work.

**#2 Extensibility.** The seam in §5 is what this fragment is most careful
about: org's colours stay org's, so a future org release restyles its own
agenda without touching the host. The alternative — host-side org semantics —
was cheaper and is rejected on this ground.

**#3 Modal editing.** Unchanged. Rows stay excerpts, so the full grammar keeps
working on them and write-through keeps landing in the right file.

**#4 Asynchronicity.** Unchanged; the display modes read state the scan already
produced.

**UX (the higher court).** Headline-only rows and the `[untitled]` fix both
*remove* pixels the user did not ask for. The one deliberate regression is the
`<leader>oa` behaviour change in §8, taken because muscle memory from emacs
runs the other way.
