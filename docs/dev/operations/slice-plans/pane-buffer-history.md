# Pane buffer history — slice plan

**Status:** 📝 planned. Design fragment:
[`../../architecture/pane-buffer-history.md`](../../architecture/pane-buffer-history.md).

Per-pane back/forward over visited buffers: `<C-6>` back, `<C-7>`
forward, `:history pane-buffers` picker. Splitting a pane starts a
fresh trail.

| Slice | Status | What |
|---|---|---|
| PBH.1 | ✅ | Data model + side table + GC reconciliation |
| PBH.2 | ✅ | Recording at the activation chokepoint |
| PBH.3 | ✅ | Walk back / forward + the `<C-6>` / `<C-7>` chords |
| PBH.4 | ✅ | `pane.buffer-history-size` option + eviction |
| PBH.5 | 📝 | `:history pane-buffers` picker source |
| PBH.6 | 📝 | Docs + cross-renderer parity |

---

## PBH.1 — Data model, side table, GC ✅

`PaneHistoryEntry` / `PaneBufferHistory` and the
`pane_buffer_history: HashMap<PaneId, PaneBufferHistory>` field on
`Editor`.

- New module `crates/lattice-host/src/pane_history.rs` — the structure
  plus its pure operations (`push`, `back`, `forward`, `truncate`,
  `evict`). Pure and unit-testable without an `Editor`.
- `Editor::reconcile_pane_history()` — retain only keys present in
  `PaneTree::leaves`. Called after tree-mutating operations.

**Do NOT** add a field to `PaneState`: it is `Copy` and
`split_active` copies it field-wise, so history would be inherited by
the new pane — the one behaviour this feature must not have. See
design §4.

**Tests.** Push/back/forward/truncate on the bare structure; a fresh
`PaneId` has no entry; `reconcile` drops closed panes and keeps live
ones; `collapse_to_active` (which drops *siblings*, not the active)
is covered too — it is the removal path most likely to be missed.

**The acceptance test for the headline requirement:** split a pane
with a non-trivial history, assert the new pane's history has exactly
one entry (its current buffer) and the original's is untouched.

**Landed as:** `crates/lattice-host/src/pane_history.rs` (18 pure unit
tests) + `crates/lattice-host/tests/pane_buffer_history.rs` (6
integration tests). `Editor::pane_buffer_history` side table,
`Editor::reconcile_pane_history`, `Editor::active_pane_history_mut`.

**Decision made during the slice — where `reconcile` is called.** The
plan said "after a tree-mutating operation", but there are ~10
`close_active` / `collapse_to_active` / `split_active` call sites in
`dispatch.rs`, and hooking each is the stale-enumeration failure this
design already rejects for GC itself. Reconcile therefore runs at the
top of `active_pane_history_mut` — the one path that *reads* history,
so it cannot be missed and costs O(panes) only when history is
actually touched rather than per keystroke.

A closed pane's entry lingers until the next history operation. That
is bounded and harmless: the map is keyed by `PaneId`, so a dead
pane's entry can never be read, and any navigation in a surviving
pane clears it. An entry only exists if its pane navigated, so the
worst case is "panes navigated-then-closed since the last navigation".

## PBH.2 — Recording ✅

Push in `Editor::activate_buffer_only`
(`crates/lattice-host/src/dispatch.rs`), alongside the existing
`push_position_history` call and under the same
`if id != self.active_pane_buffer_id()` guard.

- On leaving a buffer, update the *current* entry's `cursor`/`scroll`
  from the pane, then push the new buffer.
- Lazily seed the map entry with the pane's current buffer when the
  pane has none, so the first `<C-6>` has an origin.

**Tests.** A buffer switch records; re-activating the same buffer does
not; the outgoing cursor is captured on the entry being left.

**Preview regression test (load-bearing).** Open a picker, preview
several buffers, cancel — assert the pane's history is unchanged.
Previews route through `set_preview_override` and never reach this
chokepoint (design §5), so this test pins an existing architectural
property that a future refactor could silently break.

**Landed as:** `Editor::record_pane_history_visit` (called from the
existing `if id != self.active_pane_buffer_id()` guard in
`activate_buffer_only`, beside `push_position_history`) and
`Editor::capture_outgoing_pane_position` (the no-push variant PBH.3's
walk uses). 5 further tests in
`crates/lattice-host/tests/pane_buffer_history.rs` (11 total).

**Capture-on-departure.** The new entry's own cursor/scroll is a
placeholder; the real position is stamped onto an entry when something
*leaves* it. That keeps one rule — "record where you were before
moving" — shared by the recording path and the walk, instead of trying
to guess where activation will land the cursor before it has happened.
PBH.3's walk must call `capture_outgoing_pane_position` before moving,
or a walked-past entry keeps a stale position.

**Cap.** Uses `pane_history::DEFAULT_PANE_BUFFER_HISTORY_SIZE` (100),
the single place the bound is spelled, so PBH.4's swap to the typed
option is one edit rather than a hunt.

## PBH.3 — Walk + chords ✅

`Editor::pane_history_back()` / `pane_history_forward()`, bound to
`<C-6>` / `<C-7>` at `KeymapLayer::Builtin`, `BindingMode::Normal`
(no owning mode — universal pane machinery).

- The walk switches buffers **without recording** — otherwise forward
  is unreachable. Route through a path that bypasses PBH.2's push, or
  set an explicit in-walk suppression flag.
- Restore the entry's `cursor` + `scroll`.
- At either end: echo, no wrap.
- Entries whose buffer left the registry are skipped and dropped as
  the walk passes them.

**Chord note.** `<C-6>` / `<C-7>` are the only spellings that match in
the TUI: terminals send 0x1E / 0x1F and crossterm 0.28.1 maps
`b'\x1C'..=b'\x1F'` to `Char('4'..'7') + CONTROL`, so a `<C-^>`
binding would never fire. See design §7.

**Tests.** back/forward round-trip; walking then opening a new buffer
truncates the forward tail; walk does not itself record; cursor is
restored; both ends echo rather than wrap; a deleted buffer is skipped;
panes walk independent trails. Test the chords with `press()`, not by
calling the handler — a `BindingMode` arm that swallows the key is
invisible to a handler-level test (see
`modal-states-need-a-dispatch-arm`).

**Landed as:** `AppEffect::PaneHistoryBack`/`Forward` (+ WIT variants
and boundary mappings), `Action::PaneHistoryBack`/`Forward`,
`action:pane-history-back`/`-forward`, `Editor::do_pane_history`, the
`walking_pane_history` suppression flag, bindings in `keymap_normal.rs`
beside `<C-o>`/`<C-i>`, and catalog rows so `:keymap` documents them.
6 walk tests (17 in the integration file) + 3 chord-dispatch tests.

**A pre-existing bug this slice uncovered — `<C-digit>` was
unreachable.** The chords bound correctly and still did nothing. The
count-prefix branch in `lattice-host/src/input.rs` tested `chord.key`
alone, so `Char('6') + CTRL` matched `to_digit` and was swallowed as a
count *before the trie was consulted*. That broke **every `<C-digit>`
chord in Normal mode**, not just these; emacs-keys' `<C-x>2` survived
only via the `prefix_resolves_chord` exception, which is why nobody had
hit it. Fixed by adding `chord.mods.is_empty()` to the guard, pinned by
`ctrl_digit_is_a_chord_not_a_count` and its paired
`plain_digit_is_still_a_count` so neither direction regresses alone.
The `press()`-not-handler rule is what caught it: a handler-level test
would have passed throughout.

**Second correction, caught in self-review.** The walk's liveness prune
first used `buffers.document_ids_sorted()`, which filters to
`BufferData::Document(_)` — so it would have silently dropped every
in-pane synthetic buffer (help, oil, file tree) from a trail, directly
contradicting PBH.2's rule that those legitimately belong there. Now
`buffers.kind_of(id).is_some()`, kind-agnostic. The two rules have to
agree and the narrow one is the reflex spelling.

## PBH.4 — Size option ✅

`pane.buffer-history-size`, typed `usize`, default 100, customizable
through `:set` / `:customize`.

- Evict oldest past the cap; shift `cursor` down with the entries so
  the current position does not drift onto a different buffer.

**Landed as:** a new `Pane` option group + `pane_options.rs`
(`pane.buffer-history-size`, `i64`, default 100), read through
`Editor::pane_buffer_history_size()` with a clamp to ≥1.

**Two corrections this slice forced, both raised by the user.**

1. **`activate_buffer_only` was NOT the single funnel** PBH.2 claimed.
   `do_edit`'s already-open branch and the `<C-o>` position-history
   walk both call `activate_document` directly, so
   `:e <already-open-file>` and cross-buffer jumps changed what the
   pane showed without being recorded. Fixed by recording inside
   `activate_document` as well; the overlap is a no-op by construction
   because `push` ignores a visit to the buffer already current. The
   `activate_buffer_only` call stays for non-document kinds, which
   never reach `activate_document`.
2. **`:bd` now purges eagerly** across *every* pane's trail, not just
   the active one — deletion is global, and only the walking pane
   prunes lazily. The lazy prune stays as the net for other
   buffer-dropping paths.

   Considered routing this through the `DocumentClosed` event instead
   (the bus has that variant). Rejected: the delete path already reaps
   its sibling side tables inline (`on_disk_fingerprints`,
   `autoread_pending`, …), so an async subscriber would be the odd one
   out and would leave a window where a walk could hit a dead entry.
   The bus has no buffer-*switch* event at all, so recording could not
   move there regardless — and it would be async relative to the
   cursor state capture-on-departure needs synchronously.

**Tests.** Eviction at the cap; `cursor` still points at the same
logical entry after eviction; `:set pane.buffer-history-size` takes
effect on the next push.

## PBH.5 — `:history pane-buffers` 📝

- `PaneBufferHistorySource` in `crates/lattice-picker/src/picker_sources.rs`,
  modelled on `CommandHistorySource`; newest-first; current entry
  marked.
- `pane_buffer_history` on `PickerContext` (owned vec, built per
  picker-open like `position_history`).
- `:history` arg mapping `pane-buffers` → `pane-buffer-history`, and
  `pane-buffers` added to the `gen:history-kinds` completion source
  (`crates/lattice-host/src/editor_boot.rs`).
- Accept **moves `cursor`** to the chosen entry rather than pushing —
  random-access walk, not a new visit.

**Tests.** `:history pane-buffers` opens with the pane's entries;
`:history <Tab>` offers `pane-buffers`; accepting an entry moves the
cursor rather than appending; the picker reflects the *active* pane's
history, not a global one.

## PBH.6 — Docs + parity 📝

- `docs/user/buffers.md` — a "Walking a pane's buffer history"
  section; this is where users look for `<C-6>`.
- `docs/user/help.md` / `ex-commands.md` — `:history pane-buffers`.
- `docs/user/modal-editing.md` — the two chords in the Normal-mode
  reference.
- **GPUI parity in the same patch**: map Ctrl+6 / Ctrl+7 to the same
  `KeyChord`s. GPUI delivers key `"6"` + ctrl modifier rather than a
  control byte, so the TUI path does not prove it.

**End-of-slice audit:**
`grep -rn "pane_history\|PaneBufferHistory" crates/lattice-ui-gpui/ --include="*.rs"`
— empty grep means GPUI was missed.

---

## Not doing

- **No benchmark.** Every operation is O(1) amortised on a ≤100-entry
  `Vec` behind a keystroke; `reconcile` is O(panes). Nothing here is
  on a per-frame or per-char path. Deliberate omission under the
  four-artefact rule, not an oversight.
- **No vim `<C-^>` alternate-file toggle.** A toggle cannot be half of
  a back/forward pair. Nothing is lost — no `:b#` exists today. If
  wanted later it lands as its own named command rather than
  overloading a directional key with hidden state.
- **No cross-pane or persisted history.** Per-pane and in-memory only;
  history dies with its pane.
