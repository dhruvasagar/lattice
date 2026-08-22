# Yank ring + yank picker — slice plan (YR)

Design fragment:
[`../../architecture/yank-ring.md`](../../architecture/yank-ring.md).
Planned 2026-08-03. **YR.1, YR.3, YR.4, YR.5 landed 2026-08-19; YR.2 and YR.6 landed 2026-08-22.** Complete.

| Slice | Scope | Depends | Status |
|---|---|---|---|
| YR.1 | The ring itself — `YankRing`, `store_yank` push, cap + dedupe, `yank.ring.size` | — | ✅ |
| YR.2 | `"0`–`"9` projected out of the ring | YR.1 | ✅ |
| YR.3 | `PickerAcceptOutcome::FillCaller` + `FillTarget` captured at open | — | ✅ |
| YR.4 | The `yank-ring` picker source (ring + named registers) | YR.1, YR.3 | ✅ |
| YR.5 | Keys: `<C-r>` insert-register, `<C-r><C-r>` / `<C-r>`-in-picker open the picker | YR.4 | ✅ |
| YR.6 | Second consumer: `ArgSpec.picker` + `<C-x><C-o>` on the `:` line | YR.3 | ✅ |

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

**Landed 2026-08-19.** Two notes for whoever picks up YR.2:

- `YankRing` sits in `lattice-host/src/state.rs` beside `UnnamedRegister`,
  not in `lattice-grammar` as this plan said — `UnnamedRegister` is not in
  `lattice-grammar` and the grammar does not read the ring.
- Capacity is passed to `push` rather than held on the ring, so
  `yank.ring.size` is read per push. That is what makes lowering it take
  effect on the next yank instead of at the next restart, and it keeps the
  ring free of a config dependency.

The CB.1 guard is the one that matters: `a_delete_reaches_the_ring_but_not_the_clipboard`,
paired with `a_yank_still_reaches_the_clipboard` so it is measuring the
yank/delete distinction rather than a clipboard that never works. It uses
the `FakeClipboard` `Editor::boot` already registers — `Editor::services`
is an `Arc` and immutable after boot, so a test installing its own spy
would assert against a clipboard the code under test never writes to.

User doc `docs/user/yank-ring.md`, in the site's Editing section.

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

## YR.6 — the second consumer: argument pickers ✅ (2026-08-22)

**The premise moved between planning and execution, and the slice was
re-scoped on evidence rather than built as written.**

As planned (2026-08-03): "a commit picker for any command taking a
revision — `:magit-checkout`, `:magit-find-file`,
`:magit-blame-reverse`, `C-c f v`, and MG.32's `b` prompt."

**MG.53 (2026-08-19) delivered that**, by a different route: five
git-noun picker sources (`REVISION_`/`COMMIT_`/`REF_`/`TAG_`/
`REMOTE_PICK_SOURCE`) plus a `{}` placeholder (`picked_line`) routing
through `PickerAcceptOutcome::InvokeCommand`. That is
*pick-then-execute*, and it covers every example named above. Two other
targets were never consumers: magit's transient `Value` args are `-n`
(a count) and `--author` (a pattern), neither of which wants a revision.

**What was actually missing**, and it was countable: **0 of magit's 20
`ArgSpec`s offered anything at all.** `:magit-checkout ma<Tab>` got no
help while `REVISION_PICK_SOURCE` already knew the answer. The gap was
not "pick a revision" but "type one by hand and get nothing".

**The deferred decision, settled.** The plan left "generic seam vs wire
the magit prompts" open, because it depended on how many non-magit
arguments want it. The deciding fact turned out to be different: the
sources already exist as **picker** sources, and `ArgSpec.completion`
names **completion** sources — two registries, kept apart deliberately
by `completion-pipeline-unification.md` slice 7d.1. Reusing
`completion` would have meant a second implementation of "list
branches" that must not drift from the first. So: the generic seam,
`ArgSpec.picker`.

Shipped:
- `ArgSpec.picker` + `with_picker()`, mirrored over WIT so a plugin can
  declare one. Composable with `completion` — the two answer the same
  question at different weights.
- `<C-x><C-o>` on the `:` line (vim's omni-completion chord; the keymap
  already used the `<C-x>` prefix for `<C-x><C-e>`).
- `Editor::do_open_arg_picker`, capturing BOTH the fill target and the
  byte range the pick replaces, at open.
- Eight magit arguments wired to their existing pickers.

**The trap, stated so it is not rediscovered** — the sibling of YR.3's:
`FillTarget::CommandLine` *inserts*, which is right for `<C-r><C-r>`
where nothing is replaced. An argument picker opens part-way through
typing the argument, so an insert turns `:magit-checkout ma` + `main`
into `mamain`. The replace range is captured at open for the same reason
the target is. **A test that opens on an empty argument passes against
the broken version**, so the prefix case is pinned separately.

Also fixed: `do_picker_dismiss` cleared `picker_fill_target` only on the
stashed-picker branch, so a dismissed fill-picker left its capture set.

**Follow-up landed the same day:** the picker opens already filtered by
the typed prefix, so the chord continues the user's typing instead of
discarding it. Two constraints, both of which are the interesting part:

- **Non-live sources only.** A live source owns its filtering and
  refetches via `on_query_changed`; a query set without firing that
  would paint a filter the list has not been filtered by — worse than
  an empty query, because it looks applied.
- **An unmatchable prefix is dropped, not honoured.** Stranding someone
  on an empty list one keystroke after they asked to see their options
  inverts the whole point, and a sha fragment or a typo the fuzzy
  matcher dislikes is exactly when people reach for the picker.

---

## Cross-references

- [`../../architecture/yank-ring.md`](../../architecture/yank-ring.md) — design (what + why)
- [`../../architecture/clipboard.md`](../../architecture/clipboard.md) — §5 yank-only clipboard rule, §11 this feature's origin
- [`archive/clipboard.md`](archive/clipboard.md) — the CB series, incl. the 2026-08-03 picker-paste fix
