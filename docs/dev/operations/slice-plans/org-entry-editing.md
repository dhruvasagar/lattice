# `C-c C-c` and entry properties — slice plan

> **Status: Active.** Opened 2026-09-04. Implements
> [`org-mode.md`](../../architecture/org-mode.md) §5.4–§5.5.

Design owns *what* and *why*; this file owns *when* and *in what order*.

Spans two repos. Slices marked **(plugin)** land in
`~/src/dhruvasagar/lattice-org-plugin`; **(host)** ones in `lattice`.

## Status

| Slice | Title | Status |
|---|---|---|
| OE.0 | Where a `:PROPERTIES:` drawer goes — resolve it against the grammar **(plugin)** | ✅ |
| OE.1 | A property writer that can create a drawer **(plugin)** | ✅ |
| OE.2 | `org-set-property` **(plugin)** | ✅ |
| OE.3 | `org-ctrl-c-ctrl-c` — org's own arms **(plugin)** | ✅ |
| OE.4 | The table arm, on `table-mode`'s own layer **(host)** | ✅ |

OE.0 blocked OE.1 (it decided the helper's insertion point) and nothing else.
OE.2 needs OE.1. OE.3 and OE.4 are independent of both and of each other —
OE.4's decline falls through to whatever org's major has, including nothing, so
it can land first or last.

OE.1 is smaller than planned: OE.0 shipped the insertion-point fix and
`drawer_line_for` with it, so what remains is the three-case writer over an
already-correct answer.

---

## OE.0 — Where a `:PROPERTIES:` drawer goes **(plugin)** ✅

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.5.

**Planned as a question, landed as a bug fix.** The plan said the answer would
ride OE.1; it does not, because the answer turned out to be that
`:org-roam-id-create` has been damaging users' files, and carrying a known
defect across two slices to keep a sequencing note intact is not a trade worth
making. The generalisation OE.1 describes now starts from a correct helper.

**The grammar is unambiguous.** tree-sitter-org's `section` rule is a SEQ:
`headline, [plan], [property_drawer], [body], subsection*`. The plan comes
first, so `agenda.rs`'s module header and `clock.rs`'s walk order were right
and `id_drawer_insert`'s "org requires the drawer to be the first thing under
its headline" was wrong.

**Confirmed by parsing, not by reading the rule.** The grammar's own corpus has
no case combining a plan with a drawer, so the fixtures were parsed against the
pinned grammar directly (`parser.c` built natively, three inputs dumped). What
they show is worse than a style deviation:

| Input | `plan` field | `SCHEDULED:` parses as |
|---|---|---|
| plan, then drawer | present | `plan → entry → timestamp` |
| drawer, then plan | **absent** | a `paragraph` in `body`, no timestamp |

`agenda.rs:419` reads the date from `section.child_by_field("plan")`, so an
`:ID:` minted on a scheduled TODO **moved it out of the dated agenda into the
undated block**. The file still reads correctly to a human — which is why the
defect survived a slice that had tests, a review and a doc comment asserting
the opposite.

Two further findings from the same fixtures, neither this slice's to fix:

- **A plan is ONE line here.** `SCHEDULED:` on one line and `DEADLINE:` on the
  next parses only the first; the second is body prose, so its date is
  invisible to the agenda. Org itself accepts both. That is the pinned
  grammar's behaviour and moves when the grammar does — recorded in
  `org-mode.md` §5.5 rather than worked around.
- **Indentation is not a factor**: an indented plan and drawer parse the same.
  So the insertion point needs no loop and no whitespace rule — one line past
  the headline, plus one more if that line is planning.

**The fix.** `roam_index::drawer_line_for` is that answer as a function, public
because OE.1's writer needs the same one and two derivations of "where does the
drawer go" would drift silently. `planning::parse` decides what a planning line
is, reusing its deliberate strictness: a prose line that merely mentions
`SCHEDULED:` must not push the drawer past it.

**Tests:** four in `roam_index`, and their value is that they were **verified
to fail with the fix reverted** — the drawer lands below the plan; an existing
drawer below a plan is found and extended rather than duplicated; an `:ID:`
below a plan is still recognised (before the fix the walk began on the plan
line, saw no `:PROPERTIES:`, and would have opened a second drawer on a
scheduled node); and prose mentioning `SCHEDULED:` does not move it.

Gate: 761 green across the org plugin (17 suites), fmt clean, no new clippy
warnings.

---

## OE.1 — A property writer that can create a drawer **(plugin)** ✅

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

**The insertion point is settled** — `roam_index::drawer_line_for`, from OE.0.
Do not re-derive it here; that is the drift the helper was made public to
prevent.

**Landed with OE.2, in one commit**, under the standing exception: the writer
has no caller until the command exists, so alone it is `dead_code` — and
`#[allow]`ing that to preserve a slice boundary is the wrong way round. The
commit says which slice absorbed what.

`properties.rs`, 12 tests: the three cases; an unterminated drawer stopping at
the next headline; indentation matched to the drawer it joins; a key differing
only in case; a value containing colons (a writer that split the LINE on `:`
truncates `http://…` in both directions); and org's `%-10s %s` padding, so a
property this writes sits flush with the ones org wrote.

---

## OE.2 — `org-set-property` **(plugin)** ✅

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

**The key rides `buffer-name`, not a stash** — and the plan's "second
prompt's own action arguments" was not available: `open-prompt-payload` has no
argument slot. The host hands a plugin's submit action `[typed-text,
buffer-name]` (OC.3a), which is the channel capture already smuggles its
template key through. The instinct against a `thread_local` was right for
capture's stated reason: `<Esc>` dispatches NOTHING, so nothing would ever
clear it and the next `org-set-property` would inherit an abandoned key.

Bound on `<C-c><C-x>p` and `<leader>op` in a file AND in the agenda, where
emacs also binds it — the agenda works for OA.25's reason, `PlanTarget`
resolving a row to the headline in its SOURCE file.

**Tests:** four end-to-end in `org_planning.rs` — the full chain including the
smuggle (no unit test can see whether the key survives the hop); setting twice
replacing rather than duplicating; an empty name backing out with the buffer
asserted unchanged; and the agenda case writing the source file without
displacing its planning line.

---

## OE.3 — `org-ctrl-c-ctrl-c`, org's own arms **(plugin)** ✅

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

**No statistics-cookie arm**, though the plan listed one. Emacs spells that
`C-c #` (`org-update-statistics-cookies`), and a cookie lives on a headline or
a parent list item — both of which already have an arm, and the toggle already
rewrites every ancestor's cookie as a side effect. A third arm would have been
inventing a binding org does not have.

**Bound on the MAJOR, and the first attempt got that wrong.** It went on
`org-todo-mode` — a MINOR — where it competed with `org-capture-mode`'s
finalize minor-to-minor, an order neither mode controls. Capture lost:
`C-c C-c` in a capture buffer opened a TAGS prompt on the template's headline
and the note was never written. On the major, a minor's layer beats it and
capture files, which is what the design said and what the code now does.

**What caught it is the test change, not the test.** `finalize_capture` had
always dispatched `org-capture-finalize` BY NAME; it presses the chord now.
A by-name dispatch passes however the layers resolve, which is exactly the
class this slice could break.

**Tests:** three in `org_structure.rs` (checkbox arm, prose fallback asserting
the buffer text AND the message, and the fall-through from `table-mode`), one
in `org_planning.rs` (the headline arm, which is prompt-shaped and belongs
with that file's subject), and `finalize_capture` now proving the layering on
every capture test that uses it.

**A harness trap found on the way**, worth recording because it cost the most
time here: `org_planning.rs`'s `press_raw` re-dispatches the action that
`dispatch_chord` already ran. For a prompt-opener — that file's subject —
running twice is harmless. For a TOGGLE it flips the box back, so the buffer
looks untouched and the arm looks broken. `<C-Space>` behaves identically
there, which is what proved the dispatcher innocent. Mutation-shaped tests
belong in `org_structure.rs`, whose `press` dispatches the chord and nothing
else.

---

## OE.4 — The table arm, on `table-mode`'s own layer **(host)** ✅

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

One keymap entry: `<C-c><C-c>` → `REALIGN`, not `ALIGN`. The difference
between those two actions is exactly what this chord needs — `REALIGN` is
registered `shared`, so it returns `Effect::Declined` outside a table and the
dispatcher re-resolves one layer down onto org's arms. `ALIGN` consumes,
which is right for `<leader>t|` (nothing beneath a leader prefix) and would
have made `C-c C-c` dead everywhere but a table. Markdown gets the key for
free, which is the point of the mode being shared.

**Tests split by where each half belongs.** The in-table realign and the
decline are host-side in `table_mode_layering.rs`, where `table-mode` is
certainly active; the composition — declines, and org's checkbox arm runs
instead — is in the plugin, since it is the only place both layers exist. A
fixture with a table in it, so the decline is about the CURSOR and not about
whether the mode is on at all.

Gate: 116 green in `org_structure`, 29 in `table_mode_layering`.
