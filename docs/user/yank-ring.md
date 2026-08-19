---
summary: "yank-ring: every yank and delete kept in a bounded, recency-ordered history. `<C-r>{reg}` inserts a register, `<C-r><C-r>` picks from the ring, and the pick lands wherever you opened it from."
related: [modal-editing, picker]
---

# yank-ring

Vim gives you `""` and twenty-six named registers, and the problem with
both is that you have to decide *before* you copy. Yank three things
without naming a register and the first two are gone.

The yank ring keeps them. Every yank **and every delete** pushes an
entry; the ring holds the most recent `yank.ring.size` of them, newest
first.

## Using it

In **Insert mode**:

| Keys | Does |
|---|---|
| `<C-r>` then a register char | Insert that register's contents — vim's insert-register. `<C-r>"` for the unnamed one, `<C-r>+` for the system clipboard. |
| `<C-r><C-r>` | Open the yank picker: the ring newest-first, then your named registers, filtered as you type. |

In **any picker**, `<C-r>` opens the yank picker over it and appends your
pick to that picker's query — so you can filter `:files` by something you
copied a minute ago instead of retyping it. `<Esc>` in the yank picker
returns you to the list you were filtering rather than closing both.

`:picker yank-ring` opens the same list from anywhere.

**The pick lands where you opened it from.** That is the part worth
knowing: open the picker while editing and the text is inserted at your
cursor; open it from the `:` line and it goes into the command line; from
a search or a prompt, into that. Opening it from *inside another picker*
adds the text to that picker's query rather than replacing the picker.

Normal-mode `<C-r>` is still redo. Only Insert mode gains a meaning here,
which is where vim puts it too.

Ring entries show their position and whether they are linewise or
charwise, because the two paste differently and you are usually choosing
between rows that look alike.

## Still to come

`"0`–`"9` do **not** read from the ring yet — they are still the old
behaviour. Wiring them up so `"0` is the newest yank and `"1`–`"9` the
nine newest deletes is a later slice.

## Deletes go in too

This is the deliberate difference from the system clipboard, which takes
yanks only.

The two stores have different blast radii. The clipboard is shared with
every other application on your machine, so vim's `unnamedplus` wart —
where an incidental `x` clobbers the URL you copied from your browser —
destroys something the editor never owned. The ring is internal and
bounded: an `x` landing in it costs one slot and destroys nothing.

And "get back the line I just deleted" is one of the most common reasons
to go looking through history at all. A ring holding only yanks would
decline the question you most want to ask it.

## Duplicates

Two rules, and they are different on purpose:

- **A consecutive repeat collapses.** Holding `yy`, or re-yanking an
  unchanged line, would otherwise fill the picker with identical rows it
  could not help you tell apart.
- **A non-consecutive repeat is promoted.** Re-yanking something from an
  hour ago is a real event, and moving it to the top is the useful
  answer — it is what you are about to paste.

Kind counts as part of an entry's identity: the same text yanked
linewise and charwise pastes differently, so both are kept.

## Size

```
:set yank.ring.size=50      " the default
:set yank.ring.size=0       " disable the ring
```

Vim keeps 9, emacs 120. 50 is enough that the picker's filter is the tool
you reach for rather than scrolling, and small enough that the whole ring
stays cheap to hold.

The value is read when an entry is pushed, so lowering it takes effect
on your next yank rather than at your next restart. Eviction is
oldest-first.

`"_` (the black hole) pushes nothing, as it does nowhere else.

## See also

- [`modal-editing`](help:modal-editing#registers-macros) — the named
  registers, which still work exactly as they did; the ring is addressed
  by recency rather than by name, and the two do not compete.
