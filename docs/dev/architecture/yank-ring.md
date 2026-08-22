# The yank ring and the yank picker (YR)

**Status:** designed 2026-08-03; **not implemented**. Slice plan:
[`../operations/slice-plans/archive/yank-ring.md`](../operations/slice-plans/archive/yank-ring.md).
Referenced as "to be written" by
[`clipboard.md`](clipboard.md) §11, which is where the register/clipboard
split this builds on is defined.

## Why

You yank three things, then want the first one back. Today it is gone:
lattice keeps exactly one unnamed register, so every yank overwrites the
last. Vim answers with a numbered ring, emacs with a kill-ring and
`M-y`. Lattice has neither.

The picker is what makes the difference worth having. A ring you address
by `"1` … `"9` requires remembering *how many yanks ago*; a ring you
address by picking requires only recognising the content. Lattice
already has a picker with fuzzy filtering, marginalia and preview, so
the interesting work is the ring and the seam, not the UI.

## Current state (verified against source 2026-08-03)

**There is no numbered ring.** `Editor::store_yank`
(`lattice-host/src/dispatch.rs`) writes `unnamed_register`, and then
writes `registers[reg]` **only** when a register was explicitly named:

```rust
match register {
    Register::Unnamed | Register::BlackHole => {}
    other => { self.registers.insert(other, entry); }
}
```

So a plain `y` populates the unnamed register alone. Nothing populates
`"0`; nothing shifts `"1` → `"2` on a delete. `Register::Numbered(u8)`
parses (`"3y` works) and can be written explicitly, but no rotation
exists anywhere.

> **Two corrections to [`clipboard.md`](clipboard.md).** §1 states
> "`store_yank` populates the unnamed register + `"0`" — it does not.
> §11 describes this feature as growing "vim's `"0`–`"9` numbered ring
> into a longer history", which presumes a ring that is not there. Both
> were written from vim's model rather than from this code.

What **is** there and is the right foundation:

- `store_yank(register, content, kind, explicit_yank)` — the single
  write seam for every yank and delete. §11 named this correctly.
- `read_register(Option<Register>)` — the single read seam.
- `Register` (`lattice-grammar/src/register.rs`) with `Unnamed`,
  `Named(char)`, `Numbered(u8)`, `System`, `BlackHole`.
- `UnnamedRegister { content, kind }` carrying `YankKind`
  (charwise / linewise / blockwise), which the picker must surface
  because it changes what a paste does.

## Shape: the ring is the source of truth

Because the numbered ring does not exist, this is greenfield — and that
makes the better direction available. Rather than bolting a history
beside vim's numbered registers, **build the ring and project the
numbered registers out of it**:

| Address | Means |
|---|---|
| `"0` | newest **yank** in the ring |
| `"1` … `"9` | the nine newest **deletes**, newest first |
| the picker | any entry, by recognition rather than by count |

That is what vim's registers semantically *are*; vim merely implements
them as parallel state. One store with two projections cannot drift the
way two stores can, and it means the picker and `"1` can never disagree
about what "two deletes ago" was.

**Named registers (`"a`–`"z`) stay separate storage** and are *not*
ring entries — they are deliberate stashes with stable addresses, and
ageing them out of a bounded ring would silently lose them. The picker
shows both (§"The picker" below); the ring holds only history.

### Do deletes enter the ring?

**Yes — and the clipboard mirror stays yank-only.**

This looks like it contradicts [`clipboard.md`](clipboard.md) §5, which
deliberately keeps deletes *out* of the system clipboard: vim's
`unnamedplus` wart is that an incidental `x` clobbers whatever you had
copied from the browser. It does not contradict it, because the two are
different stores with different blast radii:

- The **system clipboard** is shared with every other application. An
  accidental write there destroys something the editor never owned.
- The **yank ring** is internal, bounded, and additive. An `x` landing
  in it costs one slot and destroys nothing.

And "get back the line I just deleted" is among the most common reasons
to open the picker at all — a ring that holds only yanks would decline
the question users most want to ask it. So `store_yank` pushes on both
paths; only the `explicit_yank && clipboard_on` mirror stays as CB.1
defined it.

### Bounds and duplicates

- **Capacity** is a typed option, `yank.ring.size`, default 50. Vim
  keeps 9, emacs 120; 50 is enough that the picker's fuzzy filter is
  the tool you reach for rather than scrolling, and small enough that
  the whole ring is cheap to hold and to render.
- **Consecutive duplicates collapse.** `yy` pressed twice, or a
  re-yank of an unchanged line, otherwise produces two identical rows
  that the picker cannot help you tell apart. Non-consecutive repeats
  are kept: re-yanking something from an hour ago is a real event and
  moving it to the top is the useful behaviour.
- Eviction is oldest-first. A `"0`–`"9` projection reads the newest
  entries, so eviction can never change what a numbered register means.

## The picker

One list, two sources — this is [`clipboard.md`](clipboard.md) §11's
stated intent and it survives review:

- **ring entries** — the history, newest first;
- **named registers** — the live `"a`–`"z`, and `"+` when populated.

so that one keystroke reaches both "the thing I copied a minute ago"
and "the thing I deliberately stashed in `"q`".

| Column | Carries |
|---|---|
| row text | the entry's first line, whitespace-normalised |
| marginalia | the address (`"a`, `"0`) or an age, plus the `YankKind` |
| preview | the full content |

`YankKind` is not decoration: a linewise entry pastes as whole lines and
a charwise one splices at the cursor, so a picker that hid the
distinction would make paste unpredictable at exactly the moment the
user is choosing between similar-looking entries.

## The seam this needs: a picker that returns a value

Every `PickerAcceptOutcome` variant today **performs an action** —
`OpenFile`, `InvokeCommand`, `JumpToLocation`, `OpenPrompt`. None
returns a value to whoever opened the picker. The yank picker needs
exactly that, because what it does on accept depends on where it was
opened from:

| Opened from | Accept should |
|---|---|
| a document | paste the entry at the cursor |
| the `:` line | insert the text into the command line |
| the `/` line | insert it into the search line |
| a prompt | insert it into the prompt |
| **another picker** | insert it into *that* picker's query |

So: `PickerAcceptOutcome::FillCaller { text }`, plus a **return target
recorded when the nested picker opens**.

```rust
enum FillTarget { Document, CommandLine, SearchLine, Prompt { buffer: BufferId }, PickerQuery }
```

**Capture at open time, never resolve at accept time.** This is the
trap, and it is the same one two earlier slices hit: by the time
`FillCaller` fires, the picker has been dismissed and the modal state
that identified the caller is gone. Resolving then would read whatever
context happens to be current — which in the single-level case is
usually right, so it passes a naive test and fails exactly in the
picker-inside-prompt case this feature exists for. Compare MG.32's
`<CR>`, which had to ask the view *before* resolving a path, and
`Effect::CursorMoveIn`, which exists because an async result must name
the buffer its position was computed in.

**This primitive is not yank-specific**, and that is the argument for
building it as its own slice. Its second consumer is already known: a
commit picker offered for any command taking a revision
(`:magit-checkout`, `:magit-find-file`, `:magit-blame-reverse`, `C-c f
v`, and MG.32's `b` branch/revision prompt), which is design.md
Appendix B's "interactive arg specs". Building it inside the yank
picker would mean building it twice.

## Keys: `<C-r>`, not a second concept

Emacs uses `M-y`. Lattice is vim-modal, where the muscle memory for
"insert a register here" is `<C-r>` — and `<C-r>` is currently unbound
in the command line (verified: no handler in
`lattice-ui-tui/src/app/cmdline.rs`).

So one chord with two depths, rather than a vim path and an emacs path
side by side:

| Keys | Does |
|---|---|
| `<C-r>` then a register char | vim's insert-register, unchanged |
| `<C-r><C-r>` | open the yank picker, filling the caller |

`M-y` is additionally bound **inside a picker**, where it is free and
where emacs users will reach for it.

This follows the UX-convention rule rather than heuristic #2: the
surface is user-facing muscle memory, and vim's `<C-r>` and emacs'
`M-y` are both established for the same act. Giving each its own entry
point costs nothing and asking either group to learn the other's would.

## Paramount-goal alignment

| Goal | This feature |
|---|---|
| #1 perf | The ring is a bounded in-memory `VecDeque` written on the existing `store_yank` path; no I/O, no growth with document size. The picker's preview reuses the existing preview path, which is already off the paint path. |
| #2 extensibility | The ring sits behind the register layer, so a future WIT register source composes the same way `clipboard.md` §11 describes. `FillCaller` is a generic picker capability, not a yank one. |
| #3 grammar | `"0`–`"9` gain real backing, so vim's documented register semantics start being true here rather than being parsed and ignored. |
| #4 async | Nothing new is async; the ring is synchronous state on the actor, like `registers` today. |

**UX (higher court):** the failure mode this removes is silent — you
yank, yank again, and the first is gone with no indication it ever
existed. The `<C-r>` unification is chosen so neither editor tradition
has to unlearn anything.

## Rejected alternatives

- **Extend vim's numbered ring in place** (what §11 assumed). There is
  no ring to extend, and building one *then* adding a parallel history
  is two stores that can disagree.
- **Put named registers in the ring.** They would age out. A register
  you deliberately stashed vanishing after fifty yanks is a worse
  failure than not seeing it in the picker.
- **Resolve the fill target at accept time.** Cheaper, and wrong in the
  nested case — see the seam section.
- **A dedicated `M-y`-only binding.** Leaves `<C-r>` unbound and asks
  vim users to learn an emacs chord for something vim already spells.

## Open

- **Blockwise entries in the picker.** A blockwise yank's preview is a
  rectangle; showing it as lines is misleading. Possibly a marginalia
  marker plus a distinct preview mode.
- **Persistence across sessions.** Emacs does not persist the
  kill-ring; vim persists registers via viminfo. Out of scope here;
  it would ride on whatever session-state mechanism lands.
- **Deduplication policy beyond consecutive.** Whether a re-yank should
  move an existing entry to the top or add a second row is settled
  above (move), but if entries grow metadata (source buffer, time) the
  merge rule needs revisiting.

## Cross-references

- [`clipboard.md`](clipboard.md) — the register/clipboard split this
  builds on; §11 is this feature's origin and §5 the yank-only rule
- [`../operations/slice-plans/archive/yank-ring.md`](../operations/slice-plans/archive/yank-ring.md) — sequencing
- [`design.md`](design.md) Appendix B — interactive arg specs, the
  second consumer of `FillCaller`
