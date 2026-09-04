# `C-c C-c` and entry properties — slice plan

> **Status: Active.** Opened 2026-09-04. Implements
> [`org-mode.md`](../../architecture/org-mode.md) §5.4–§5.5.

Design owns *what* and *why*; this file owns *when* and *in what order*.

Spans two repos. Slices marked **(plugin)** land in
`~/src/dhruvasagar/lattice-org-plugin`; **(host)** ones in `lattice`.

## Status

| Slice | Title | Status |
|---|---|---|
| OE.0 | Where a `:PROPERTIES:` drawer goes — resolve it against the grammar | 📝 |
| OE.1 | A property writer that can create a drawer **(plugin)** | 📝 |
| OE.2 | `org-set-property` **(plugin)** | 📝 |
| OE.3 | `org-ctrl-c-ctrl-c` — org's own arms **(plugin)** | 📝 |
| OE.4 | The table arm, on `table-mode`'s own layer **(host)** | 📝 |

OE.0 blocks OE.1 (it decides the helper's insertion point) and nothing else.
OE.2 needs OE.1. OE.3 and OE.4 are independent of both and of each other —
OE.4's decline falls through to whatever org's major has, including nothing, so
it can land first or last.

---

## OE.0 — Where a `:PROPERTIES:` drawer goes 📝

**A question before a slice, because two places in this tree disagree and one
of them is writing to users' files.**

`agenda.rs`'s module header says org's grammar puts `plan` before
`property_drawer`, so a `SCHEDULED:` line genuinely comes first. `clock.rs`
walks `["plan", "property_drawer"]` in that order to find where a `LOGBOOK`
goes. But `roam_index::id_drawer_insert` documents "org requires the drawer to
be the first thing under its headline" and inserts at `headline_line + 1`
unconditionally — so `:org-roam-id-create` on a scheduled headline puts the
drawer *above* the planning line.

Both cannot be right. Resolve against **tree-sitter-org's grammar**, which is
the authority available in-tree, and against what org itself writes: a fixture
with `SCHEDULED:` and a drawer, parsed, with the node order asserted.

- If the drawer belongs after the plan, `id_drawer_insert` has a live bug and
  OE.1 fixes both callers on one helper. A regression test goes on the roam
  side naming it, because the symptom is a file that reads fine to a human and
  parses wrong.
- If it belongs first, `clock.rs`'s walk order is the thing to re-read, and
  OE.1 inherits `headline_line + 1`.

Either way the answer is written into the helper's doc comment with the
evidence, so the third caller does not re-litigate it.

**Not a code slice.** It ends in a finding plus a test fixture; the fix (if
any) rides OE.1.

---

## OE.1 — A property writer that can create a drawer **(plugin)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.5.

Generalise `roam_index::id_drawer_insert` into `properties::set_entry_property`
— given a line accessor, a headline line, a key and a value, answer the edit
that makes `:KEY: value` true of that entry.

Three cases, and the third is what the `:ID:` version cannot do:

| State | Edit |
|---|---|
| No drawer | insert a whole `:PROPERTIES:` / `:KEY:` / `:END:` block |
| Drawer, key absent | insert the line just inside the opener |
| Drawer, key present | **replace that line** |

`id_drawer_insert` answers `None` for the third case, and it is right to: a
second `:ID:` produces a file org cannot read, so refusing is the safe answer
for identity. A second `:CATEGORY:` is just a stale line, and refusing to set a
property the user asked to set would be a command that silently does nothing.
So the two callers want different answers to the same question, and the shape
is one helper returning the replace-range with an `id_drawer_insert` that keeps
its refusal by *checking first* — not by the helper guessing which caller it
has.

**The accessor stays an accessor.** A drawer is a handful of lines under its
headline; materialising the file to look at four of them scales the cost with
the document, which is the shape `headline.rs` already sets and the reason this
helper is testable without an editor.

**Tests:** the three cases above; a drawer that is never closed (stop at the
next headline rather than attaching the property to the wrong entry — the trap
`id_drawer_insert` already guards); indentation matched to the drawer it joins;
a key differing only in case (org is case-insensitive here, and writing a
second `:id:` beside `:ID:` is the failure this shares with the unterminated
case). Plus whatever OE.0 decided, as a fixture with a planning line.

---

## OE.2 — `org-set-property` **(plugin)** 📝

Emacs' `C-c C-x p`: ask for a name, ask for a value, write it into the entry's
drawer.

**Two hops, on the established shape.** `Effect::OpenPrompt` names an action
the host dispatches with what was typed; that action returns the *second*
prompt, and its submit writes the edit. `org-set-tags` and
`org-roam-dailies-goto-date` are both this already. The key is carried in the
second prompt's own action arguments rather than stashed in guest memory —
same reason OR.11b carries a title through a menu: nothing clears a stash, and
two concurrent prompts would read each other's.

Bindings: `<C-c><C-x>p`, emacs' own, which fits beside the clock family already
at `<C-c><C-x><C-i>` / `<C-c><C-x><C-o>`. Plus `<leader>op` for the vim-native
spelling, and `:org-set-property` — which is also the form a future `init.rs`
would call.

**Not on a headline is a message, not a no-op.** The command needs an entry;
`org-roam-id-create`'s wording is the precedent ("not inside a headline — an
`:ID:` goes on an entry").

**Deferred, with the reason:** completion over property keys already in the
corpus (emacs completes; ours would need a key index, and the picker seam has
no cursor — the same constraint OR.9 hit), and `org-delete-property`. Delete is
a small ex-command on the same helper, but emacs' chord for it is not something
to invent from memory — it ships when someone checks, or it ships as an
ex-command with no chord at all.

**Tests:** a headline with no drawer gains one; an existing key is replaced,
not duplicated; the value survives spaces and a colon (`:URL: http://x`, the
case a naive `split_once(':')` writer gets wrong in both directions); the
cursor stays on the headline; a non-headline line echoes and edits nothing —
asserted on the buffer TEXT, since "no edit recorded" passes on a build that
wrote to the wrong line.

---

## OE.3 — `org-ctrl-c-ctrl-c`, org's own arms **(plugin)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.4.

`<C-c><C-c>` on the org major, dispatching on the cursor's context:

| Context | Does |
|---|---|
| Headline | set tags (the `org-set-tags` body, not a copy) |
| Checkbox / list item | toggle it, updating the parent cookie |
| Statistics cookie (`[/]`, `[%]`) | recount it |
| Anything else | echo what it saw |

**The arms call the same bodies the chords do**, reached as functions rather
than as command names — a guest cannot invoke a registered command, and
inlining a second copy of the tags prompt is how two spellings of one verb
start to drift.

**`<C-c>` stays safe because this is the first TERMINAL binding under it**, and
it must stay the only one: `<C-c>a`, `<C-c>c`, `<C-c>n…` and `<C-c><C-x>…` are
all prefixes, and a terminal `<C-c><C-c>` is not a prefix of any of them. If a
later chord starts `<C-c><C-c>…`, this binding kills it silently — the trie
answers `Bound` at the first binding and never looks at children (OA.18 paid
for that rule twice).

**Capture keeps finalize by layering, not by exclusion.** `org-capture-mode` is
a minor on an `org-mode` major and binds `<C-c><C-c>` to `org-capture-finalize`;
a minor's layer wins over its major's, so a capture buffer files-and-closes and
every other org buffer dispatches. The comment at that binding today says the
chords "cannot go on `org-mode`" — it means the *capture* chords cannot, and
this slice should not be read as contradicting it.

**The fallback echo is the slice's user-visible contract**, so it is tested
like one: on a plain body line the buffer is unchanged AND a message names the
situation. Emacs' wording is the model — say what it *cannot* do here, not
"unhandled".

**Tests:** one per arm, asserting the buffer text; the fallback pair above;
`<C-c><C-c>` in a capture buffer still finalizes (the layering, which is the
one thing here that no unit test would catch); and — since this is org's first
terminal `<C-c>` chord — that `<C-c>a` and `<C-c><C-x><C-i>` still resolve.

---

## OE.4 — The table arm, on `table-mode`'s own layer **(host)** 📝

`table-mode` binds `<C-c><C-c>` to `ALIGN`, declining when the cursor is not in
a table.

**Not an arm of org's dispatcher, and that is the point.** `action:table-align`
lives in `lattice-mode` because TB.1 made pipe tables shared with markdown; org
cannot invoke it (no `Effect::Invoke`) and must not re-implement it. Binding it
on the mode that owns tables keeps the chord and the body together — the
standing rule — and gives markdown the same key for free, which emacs' org
users will press there too.

**It rides a mechanism that was broken when it was last written about.**
`Effect::Declined` re-resolves with the declining layer removed; it used to
re-translate the trailing chord alone, so a declined `<C-c><C-c>` would have run
a bare `<C-c>`. `dispatch.rs` now preserves the prefix and peels ONE layer per
decline, and its comment cites this exact two-layer shape. So the slice is
mostly a binding — and entirely a test.

**Tests:** in a table, `<C-c><C-c>` aligns; on a line outside one, it declines
and org's dispatcher runs instead (the composition, asserted end to end with
both modes active — a test with only `table-mode` present passes on a build
where the fall-through goes nowhere); in a markdown buffer it aligns and, with
no org layer beneath, does nothing further without error.

**Ordering note:** if OE.4 lands before OE.3 the decline falls through to
nothing, which is correct behaviour and an unobservable one. The composition
test therefore belongs to whichever of the two lands second, and the plan says
so here rather than leaving it to be noticed.
