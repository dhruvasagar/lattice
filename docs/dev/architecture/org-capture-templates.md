# Org capture templates: one shape, three axes

> **Where the code is.** Everything on this page is implemented in
> [`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin), a
> **separate repository**. Nothing here compiles into the editor: there is no
> `BufferKind::Org`, no `Editor::` method, and — importantly for this design —
> **no host change of any kind**. `host-services.read-file` and
> `Effect::WriteToFile { anchor: Line(…) }` already exist and already carry
> everything this needs.

**Status:** 📝 designed, not built. Slice plan:
[`../operations/slice-plans/org-capture-templates.md`](../operations/slice-plans/org-capture-templates.md).

Supersedes the two-shape declaration that
[`org-capture.md`](org-capture.md) §2 and [`org-roam.md`](org-roam.md) §6 shipped
independently. Sections 1–7 of `org-capture.md` still describe how a template is
chosen, expanded and targeted; what changes here is **what a template is**.
Orthogonal to [`org-capture-drafts.md`](org-capture-drafts.md), which changes the
capture *buffer's substrate* and not the template shape — the two can land in
either order.

## 1. The split was never org's, and the comment saying it was is wrong

Both trees carry the same claim. `roam_templates.rs:5-17` and the user init's
`RoamTemplate` doc comment say it in nearly the same words:

> A different shape from `Template` above, and **the difference is org's rather
> than ours**: a capture template says WHERE its text lands — a file, a headline
> under it — and a roam template cannot, because the destination is a file that
> does not exist yet.

That is not what emacs does. `org-roam-capture-templates`
(`org-roam/org-roam-capture.el:35`) documents the **same four-element tuple** as
`org-capture-templates` — `keys`, `description`, `type`, `template` — and its
docstring reproduces org-capture's entry-type list verbatim, `table-line`
included. The only roam-specific part is a list of extra *keywords*:

```elisp
(defconst org-roam-capture--template-keywords
  (list :target :id :link-description :call-location :region))
```

Roam adds a **target kind** (`file+head`, which interpolates a filename and seeds
a head if the file is absent) and a few props. It never forks the template type.

The plugin's own runtime already agrees. OR.14's reuse note records the
narrowing: the pending-capture state used to carry a whole
`capture_templates::Template` and `capture_finalize` read exactly two fields from
it, so it now carries a **`CaptureDestination`** instead. Roam's flow runs
through the *same* `open_capture_buffer` (`lib.rs:5518`) and the *same*
`capture_finalize` (`lib.rs:5786`). Pipeline: unified. Declaration: forked.

The fork has already cost something measurable. `body-file` — a template body
read from a file — is a property of the **body**, not of the destination, and it
landed on the roam shape only because OR.14 happened to be scoped to roam. A user
whose capture template wants a file-sourced body cannot have one, for no reason
that survives being stated out loud.

**Heuristic #1 applies as written.** The rewrite is not small and the existing
shapes work. Neither is a reason: the specific technical advantage is that three
independent axes stop being entangled with one destination question, which is
what lets `table-line`, `file+datetree` and `body-file` each land once instead of
twice.

## 2. The three axes

| Axis | Field | Answers |
|---|---|---|
| **What** is inserted | `type` | `entry`, `table-line` (later: `item`, `checkitem`, `plain`) |
| **Where** it lands | `target` | `file`, `file+headline`, `file+olp`, `file+datetree`, `file+head` |
| **Where the text comes from** | `body` / `body-file` | inline string, or a file read at draft time |

They are independent in the real sense: every combination is meaningful. A
`table-line` can go into a plain file or under a datetree; a `body-file` can feed
an `entry` or a roam note; a `file+head` target can hold either type. Emacs's
tuple decomposes the same way, and that decomposition surviving twenty years of
org is the evidence that it cuts along the grain.

`org.capture-templates` and `org.roam-capture-templates` stay **two options
holding one type** — again emacs's factoring (`org-capture-templates` and
`org-roam-capture-templates` are separate variables of the same shape). Which
list a template sits in decides which menu reaches it. The alternative — one list
with roam entries identified by having a `file+head` target — was rejected: it
makes menu membership an inference from an unrelated field, which is the
aligned-by-silence failure the buffer-kind rule already forbids elsewhere.

## 3. `type` — what gets inserted

An `Enum` of unit variants, which the config schema expresses directly
(`Schema::Enum(Vec<String>)`, `wit/config.wit:85-91`).

- **`entry`** — today's behaviour and the default. A whole-line insert at the
  resolved line, or at end-of-file.
- **`table-line`** — a row inserted into a table at the target. New in this
  design; §5 has the semantics.

`item`, `checkitem` and `plain` are named here so the axis is understood as
open, and are **not** in scope. They are cheap once `type` exists and expensive
to retrofit if `table-line` is special-cased instead of typed.

## 4. `target` — where it lands

A schema **cannot** express a tagged union: `Schema` is
`Scalar | Enum | List | Record`, and the derive rejects any enum variant carrying
fields (`lattice-plugin-sdk-derive/src/lib.rs:419-426`) and unions outright
(`:302`). So `target` is a Record with an explicit `kind` discriminant plus
optional fields, resolved guest-side into a real Rust enum.

```toml
target = { kind = "file+datetree", file = "…", sub-olp = ["Urge / Habit Episode Tracker"] }
```

| `kind` | Extra fields | Meaning |
|---|---|---|
| `file` | — | Append at end of file. Today's `Target::File`. |
| `file+headline` | `headline` | After that headline's whole subtree. Today's `Target::FileHeadline`. |
| `file+olp` | `olp: list<string>` | The same, addressed by full outline path, for non-unique headings. |
| `file+datetree` | `olp?`, `sub-olp?`, `tree-type?` | Today's date node, created if absent. |
| `file+head` | `head?` | Roam's: `file` is `${…}`-interpolated, `head` seeds the file when absent. |

**`kind` is optional and its absence means today's inference** — `headline`
present ⇒ `file+headline`, else `file`. That is not a compatibility hack; it is
the shipped semantics of the existing two-variant record, kept as the default so
no existing `org.capture-templates` entry changes. Every *new* kind requires
`kind` to be written, so the tag is declared rather than guessed, and an illegal
combination (`file+datetree` carrying a `headline`) is a **named skip** rather
than a silent reinterpretation — the `roam_templates::from_declared:139-157`
idiom, which is also how `body` + `body-file` together is reported.

### 4.1 `sub-olp` is a deliberate departure from emacs

Emacs's `file+olp+datetree <file> "Heading" …` puts the outline path **above**
the datetree: the date nodes are created *under* that path. There is no built-in
combinator for the opposite — today's date node, then descend into a named child
— and `table-line`'s search is explicitly bounded to *"from point to the end of
current heading body"* (`org-capture.el:260`), so a date node containing
`** Daily Overview` and `** Urge / Habit Episode Tracker` cannot have its second
table addressed at all. In emacs you would write a `(file+function …)` locator.

So `olp` keeps emacs's meaning (above the tree) and **`sub-olp` names the
extension** (below the date node). Two fields rather than one overloaded one,
because a ported emacs config must not silently mean something else.

The alternative — giving `table-line` a table-selector property instead of
extending the target — was rejected on heuristic #1: an outline path is org's
existing addressing vocabulary and composes with every future entry type,
whereas a `table-line`-only selector would need re-inventing the moment `item`
lands.

### 4.2 Datetree shape, and the re-levelling it forces

`org-datetree.el:138-158` builds three levels, and the day node carries a weekday
name:

```org
* 2026
** 2026-09 September
*** 2026-09-16 Wednesday
```

`tree-type` selects the grouping (`day` default, `week`, `month`); the day node
is therefore **level 3**, and a template's own sections belong at level 4.

Lattice's capture inserts a body **verbatim** at a line boundary. The user init
already documents what that costs:

> Emacs's `file+headline` files the entry as a CHILD and re-levels it; here the
> body is inserted verbatim at the line after the headline's subtree, so a `*`
> heading would close the `Vocabulary` subtree and land as its sibling instead
> of inside it.

Against a datetree that is not a wart, it is a correctness failure: a template
whose first line is `* How to use this` would terminate the day node and land as
a sibling of `* 2026`, silently reorganising the file. **`entry` therefore
re-levels**: the body's shallowest heading is shifted so it becomes a child of
the target node, and every deeper heading shifts by the same delta so relative
structure is preserved. A body with no headings is unaffected.

This is scoped to `entry` under a heading-bearing target. `Target::File` appends
at top level and has nothing to re-level against.

## 5. `table-line` semantics

Faithful to `org-capture-place-table-line` (`org-capture.el:1469`), with the
search scope widened by §4.1.

1. **Scope.** A heading-bearing target scopes the search to that heading's body —
   `sub-olp` is what lets the heading be a descendant of the date node rather
   than the date node itself. A bare `file` target searches the whole file and
   the first table wins.
2. **Find the table.** The first org table in scope. Emacs explicitly skips
   `table.el` tables; lattice has no `table.el`, so this reduces to "the first
   run of lines whose first non-space character is `|`".
3. **Absent table: create it.** Emacs inserts `|   |\n|---|\n` and uses that.
   Lattice does the same. A capture that silently did nothing because the section
   had no table yet is the worse failure — it loses the note.
4. **Placement.** Three modes, in emacs's precedence order:
   - `table-line-pos = "II-1"` — relative to the *n*th hline group, signed delta.
     The regex `\(I+\)\([-+][0-9]+\)` is emacs's and is kept verbatim so a ported
     spec means the same thing.
   - `prepend = true` — the first data line after the first hline.
   - default — end of the table.
5. **The row itself.** The expanded body is trimmed; if it does not already look
   like a table row it is prefixed with `"| "`. A multi-line body inserts
   multiple rows.

**No alignment pass.** Org realigns the table after insertion; lattice does not,
and will not in this design. Alignment is a table-editing feature that belongs to
org-mode's table support, not to capture, and doing it here would mean capture
rewriting lines the user did not capture — which the keystroke UX contract
forbids for the editing case and which is at best surprising for the file case.
A ragged row is correct org and reads fine; `:org-table-align` (when it exists)
is the fix.

### 5.1 Why this needs no new host reach

`capture_effects` (`lib.rs:5664`) **already reads the whole target file** through
`host_services::read_file` (`:5699`) before writing, and the insertion point is
already a **line number**, not a byte offset — `Insertion::{Append, AtLine(u32)}`
(`capture_target.rs:38`), mapped to `FileAnchor::Line(l)`. A table row is a whole
line. So `table-line` is a second resolver over text already in hand: no new host
call, no new capability grant, no parser.

## 6. `body` / `body-file`

Lifted from roam unchanged, and generalised to every template. `body-file` reads
the body from a file at **draft time** rather than at config-read time, because
the path may carry interpolation that depends on the node being made
(`roam_templates::resolve_body:206`). The two are mutually exclusive; both set is
a named skip, and blank counts as absent for both.

For a non-roam template there is no `${…}` pass, so the path is tilde- and
environment-expanded only.

**Tilde expansion moves to the target path too.** The user init carries this
warning today:

> Paths are ABSOLUTE rather than `~/…`: the `file+headline` target reads the file
> through `host-services.read-file`, which does not expand a tilde. A `~/…`
> headline target still writes to the right file (the write path expands) but
> **silently loses the headline and appends at the end**.

That is a live trap — a write that lands in the wrong place with no error — and
`file+datetree` and `table-line` both inherit it, because both read before they
write. `roam_templates.rs:213-216` already tilde-expands before its read, so the
helper exists and is simply not applied on the target path. Fixing it is part of
this design rather than a separate bug, because shipping two more read-before-
write target kinds onto an unfixed one multiplies the trap.

## 7. What roam adds, and what it stops owning

**Adds:** the `file+head` target kind, and the `${title}` / `${slug}` / `${id}`
interpolation pass, which runs before the `%` pass exactly as it does today
(`roam_capture::expand_fields:56`) and stays roam-only — a non-roam capture has
no node, so there is nothing to interpolate from.

**Stops owning:** `body-file`, which becomes every template's; and the claim that
a roam template is a different kind of thing, which §1 retires.

**Unchanged:** roam's menu, its title prompt, its id minting, its
`CaptureDestination::appending_to` hand-off, and `C-c C-k` creating nothing.

## 8. Failure philosophy

Same as the rest of org (`org-capture.md` §8, `org-roam.md` §6): **a template
that cannot be resolved is skipped and named, never a trap and never a panic.**
Concretely —

| Situation | Behaviour |
|---|---|
| `body` and `body-file` both set | Skip, named in `skipped` |
| `kind` names a shape whose required field is absent | Skip, named, with the field |
| `kind` carries a field it does not take | Skip, named — not a silent ignore |
| `body-file` missing or unreadable | Warn with key + resolved path; capture does not open |
| `sub-olp` path not found under the date node | Warn and fall back to the date node's own body |
| Table absent at a `table-line` target | Create `|   |\n|---|\n` (§5.3) |
| `table-line-pos` names an hline group that does not exist | Warn, fall back to end-of-table |

The `sub-olp` fallback deserves its reason: the alternative is refusing the
capture, and the note the user just typed is the thing at risk. Landing it in the
day's node with a warning keeps it, in a place they will see it.

## 9. Configuration: `init.rs` is the primary home

Both homes stay supported and both go through the same schema — that is CI.7's
whole shape, and `org-capture.md` §2 already argues it. What changes is emphasis:
these templates are **declared in `init.rs`**, because the structured shape is
where a Rust value earns its keep. A misspelled field, a `kind` that is not in
the enum, a `target` missing its `file`: each is a compile error at the
declaration site instead of a warning in the log at capture time.

`lattice.toml` remains able to express the same set, unchanged and untested by
this design beyond a round-trip case.

The reference declaration this design is written against:

```rust
// The day's tracker: the whole template file, filed under today's date node.
Template {
    key: "h".into(),
    description: Some("habit tracker: today".into()),
    type_: EntryType::Entry,
    body: None,
    body_file: Some("~/src/dhruvasagar/org-files/templates/habit-tracker.org".into()),
    target: Target {
        kind: Some(TargetKind::FileDatetree),
        file: "~/src/dhruvasagar/org-files/habit-tracker.org".into(),
        ..Default::default()
    },
    ..Default::default()
}

// One episode: a row appended to today's Episode Tracker table.
Template {
    key: "he".into(),
    description: Some("habit: episode row".into()),
    type_: EntryType::TableLine,
    body: Some(
        "| %^{Time / Situation} | %^{Thoughts} | %^{Urge 0-10} \
         | %^{STOP + NOTICE} | %^{DELAY} | %^{ACTION} | %^{After 0-10} |".into(),
    ),
    target: Target {
        kind: Some(TargetKind::FileDatetree),
        file: "~/src/dhruvasagar/org-files/habit-tracker.org".into(),
        sub_olp: Some(vec!["Urge / Habit Episode Tracker".into()]),
        ..Default::default()
    },
    ..Default::default()
}
```

`type` is a Rust keyword, hence `type_`; the derive kebab-cases it to `type` on
the wire, the same way `clock_in` already crosses as `clock-in`.

**`Template` and `Target` derive `Default` as well as `ConfigShape`**, which
today's declarations do not. That is not cosmetic: `Target` goes from two fields
to six, of which at most two are set by any one `kind`, and spelling four
`None`s at every call site is how a declaration acquires noise that hides its
meaning. `..Default::default()` makes the fields a template *does* set the only
ones a reader sees. `EntryType` defaults to `Entry` and `TargetKind` to `None`
(the §4 inference), so a default-constructed template is exactly today's
behaviour.

## 10. Migration

Asymmetric, and the roam half is breaking. Stated plainly rather than softened:

- **`org.capture-templates` does not change.** `kind` absent keeps today's
  inference (§4). Existing entries load untouched.
- **`org.roam-capture-templates` breaks.** Its bare `file` field becomes
  `target = { kind = "file+head", file = … }`. Eight templates in the reference
  init need rewriting once. A transitional alias was considered and rejected:
  the shapes would then disagree about where a destination is declared, which is
  precisely the confusion §1 is retiring, and the migration is one edit to one
  file that the author of both is performing.
- **`skipped` gains entries for anything that does not resolve** — which is how a
  half-migrated config announces itself rather than silently capturing to the
  wrong place.

## 11. Rejected alternatives

**Add `body-file` to `RawTemplate` and stop.** Answers the original question and
nothing else. It leaves two shapes to maintain, puts `file+datetree` and
`table-line` on capture only, and guarantees a second migration when the shapes
are eventually unified. Heuristic #1: the smaller change is not the better
design, and "the rewrite is big" is not a reason.

**One template list, roam entries inferred.** Menu membership becomes a guess
from an unrelated field. Rejected in §2.

**Section-first file layout, to reuse emacs's `file+olp+datetree` unmodified.**
Would need no `sub-olp` at all: `* Urge / Habit Episode Tracker` → `** 2026-09-16`
→ the table. Rejected on **UX as the higher court** — a day's data fragments
across three datetrees, and there is no single node that is "today", which is
the entire point of a daily tracker. Its only argument was emacs compatibility,
which heuristic #2 calls data rather than justification.

**Aligning the table after a `table-line` insert.** Rejected in §5.

## 12. Paramount-goal alignment

**UX (higher court):** improves it. A capture that lands in the wrong place
because a tilde was not expanded (§6), or that silently reorganises a file
because a body was not re-levelled (§4.2), are both live today and both fixed
here.

**Protects #2 (extensibility).** The entry-type and target-kind axes are open
sets declared in a schema. `item`, `checkitem`, `file+regexp` become additions
rather than redesigns — and the same shape is what a future plugin-contributed
target kind would register into.

**Protects #3 (the grammar is the API).** `type` and `target` are org's own
vocabulary, not lattice coinages. A user who knows `org-capture-templates`
already knows this.

**Sacrifices nothing in #1 (performance).** No new host call, no new grant, and
the added work happens on an explicit user action that already reads the target
file. Nothing lands on a typing or render path.

**#4 (asynchronicity) untouched.** All of this is guest-side, on the grammar
seam, where `host-services.read-file` is the mandated read (WASI `std::fs`
panics there — `lib.rs:5688-5694`).

## 13. Not in scope

- `item` / `checkitem` / `plain` entry types (§3).
- Table alignment (§5).
- `:time-prompt` — capturing to a date other than today.
- `file+regexp`, `clock`, `here`, `function` targets.
- Savable capture drafts — that is
  [`org-capture-drafts.md`](org-capture-drafts.md) (CD.1–CD.8, unbuilt), which
  changes the buffer substrate and is independent of everything here.
