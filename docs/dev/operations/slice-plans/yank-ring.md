# Yank ring + yank picker — slice plan (YR)

Design fragment:
[`../../architecture/yank-ring.md`](../../architecture/yank-ring.md).
Planned 2026-08-03; **nothing implemented**.

| Slice | Scope | Depends | Status |
|---|---|---|---|
| YR.1 | The ring itself — `YankRing`, `store_yank` push, cap + dedupe, `yank.ring.size` | — | 📝 |
| YR.2 | `"0`–`"9` projected out of the ring | YR.1 | 📝 |
| YR.3 | `PickerAcceptOutcome::FillCaller` + `FillTarget` captured at open | — | 📝 |
| YR.4 | The `yank-ring` picker source (ring + named registers) | YR.1, YR.3 | 📝 |
| YR.5 | Keys: `<C-r>` insert-register, `<C-r><C-r>` / `M-y` open the picker | YR.4 | 📝 |
| YR.6 | Second consumer: commit picker on revision arguments | YR.3 | 📝 |

**YR.3 is independent of YR.1/YR.2** and can land first or in parallel —
it is a picker capability, not a yank one. If the revision-picker work
(YR.6) becomes the priority, YR.3 → YR.6 is a complete path that never
touches the ring.

---

## YR.1 — the ring

`YankRing` beside `Register` / `UnnamedRegister` in `lattice-grammar`;
an instance on `Editor` next to `registers`. `store_yank` is already the
single write seam for every yank *and* delete, so the push goes there
and nothing else changes.

- Bounded `VecDeque`, capacity from a new typed option
  `yank.ring.size` (default 50 — see the design fragment for why not
  vim's 9 or emacs' 120).
- **Deletes push too**; only the clipboard mirror stays yank-only. The
  reasoning is in the design fragment and it is the one decision here
  most likely to be re-litigated, so it is written down rather than
  implied by the code.
- Consecutive duplicates collapse; a non-consecutive repeat moves the
  existing entry to the top rather than adding a row.
- Eviction oldest-first.

**Tests:** push on yank and on delete; the clipboard is *not* written on
delete (guards CB.1's property, which this slice is most at risk of
breaking); cap eviction; consecutive-dedupe; non-consecutive promotion;
`BlackHole` pushes nothing.

**No bench.** A bounded `VecDeque` push on an existing synchronous path.
Say so rather than adding a bench that measures nothing.

---

## YR.2 — the numbered registers become a view

`"0` reads the newest yank; `"1`–`"9` read the nine newest deletes.
`read_register` resolves `Numbered(n)` through the ring instead of
through `registers`.

**This is where vim semantics start being true.** They parse today and
do nothing, so the risk is not regression but surprise: someone may be
relying on `"3y` writing an explicit slot. Explicit writes to a numbered
register should keep working — decide during the slice whether an
explicit `"3y` shadows the projection or is rejected, and pin whichever
with a test.

**Tests:** `"0` after a yank; `"1` after a delete; `"1`→`"2` shifting on
a second delete; eviction never changes what `"9` means; a yank does not
disturb `"1`–`"9`.

---

## YR.3 — `FillCaller`, and the target captured at open

The primitive. `PickerAcceptOutcome::FillCaller { text }` plus:

```rust
enum FillTarget { Document, CommandLine, SearchLine, Prompt { buffer: BufferId }, PickerQuery }
```

recorded **when the nested picker is opened**, not resolved when it
accepts.

> **The trap, stated so it is not rediscovered.** At accept time the
> picker has been dismissed and the modal state that identified the
> caller is gone. Resolving then reads whatever context is current —
> which in the single-level case is usually the right answer, so it
> passes a naive test and fails exactly in the picker-inside-prompt case
> the feature exists for. MG.32's `<CR>` (ask the view before resolving
> the path) and `Effect::CursorMoveIn` (name the buffer the position was
> computed in) are the same shape.

**Tests must include a nested case**, because the single-level case
passes against the broken implementation. Open a picker from a prompt,
accept, assert the prompt received the text and the document did not.

**No user-visible change in this slice** — it ships with no consumer.
That is deliberate: its two consumers (YR.4, YR.6) are in different
subsystems, and building it inside either would mean building it twice.

---

## YR.4 — the picker source

`yank-ring`, listing ring entries and live named registers in one list.
Marginalia carries the address or age plus the `YankKind`; the preview
shows full content.

`YankKind` in the marginalia is not decoration — linewise and charwise
entries paste differently, and hiding that makes paste unpredictable at
the moment the user is choosing between similar-looking rows.

Accept emits `FillCaller` when a target was captured, and pastes at the
cursor otherwise.

**Registration** goes through `lattice_picker::PickerRegistry` in the
owning crate, and its inventory test lives in that crate — see the
2026-08-03 note in
[`archive/clipboard.md`](archive/clipboard.md) and
`lattice_magit::picker_sources`'s inventory test for why a global list
in `lattice-ui-tui` is the wrong shape.

**Tests:** both sources appear; `YankKind` reaches the marginalia;
accept-in-document pastes with the right kind; accept-from-a-picker
fills the caller's query.

---

## YR.5 — keys

| Keys | Does |
|---|---|
| `<C-r>` then a register char | vim's insert-register |
| `<C-r><C-r>` | open the yank picker, filling the caller |
| `M-y` inside a picker | the same |

`<C-r>` is currently unbound in the command line (verified 2026-08-03,
no handler in `cmdline.rs`), so nothing is displaced.

**Tests:** `<C-r>a` inserts `"a`'s contents; `<C-r><C-r>` opens the
picker; the picker's accept lands in the line it was opened from; `M-y`
in a picker does the same; neither leaks into the document.

---

## YR.6 — the second consumer: revision arguments

A commit picker offered for any command taking a revision —
`:magit-checkout`, `:magit-find-file`, `:magit-blame-reverse`, `C-c f v`,
and MG.32's `b` branch/revision prompt, which is the shape that most
obviously wants it.

`CommitPickSource` already exists (MG.23j) but is reachable only as a
whole command invocation (`:picker magit-commit <ex-command>`), not as a
way to fill in an argument. With YR.3 it becomes both.

The generalisation — an `ArgSpec` naming a picker source, so the prompt
for that argument offers it — is design.md Appendix B's "interactive arg
specs". Decide during the slice whether to land the generic seam or wire
the magit prompts directly first; the design fragment does not settle it
because the answer depends on how many non-magit arguments want it.

---

## Cross-references

- [`../../architecture/yank-ring.md`](../../architecture/yank-ring.md) — design (what + why)
- [`../../architecture/clipboard.md`](../../architecture/clipboard.md) — §5 yank-only clipboard rule, §11 this feature's origin
- [`archive/clipboard.md`](archive/clipboard.md) — the CB series, incl. the 2026-08-03 picker-paste fix
