# Slice plan — `table-mode`

Design: [`table-mode.md`](../../architecture/table-mode.md).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

| Slice | Description | Status |
|---|---|---|
| TB.0 | `<leader>` expands on the native mode-keymap path | ✅ |
| TB.1 | `table-mode`: the shared minor, at org's parity | ✅ |
| TB.2 | The org plugin sheds the generic surface **(plugin)** | ✅ |
| TB.3 | Richness beyond parity — emacs' org-table surface | ✅ |
| TB.4 | Realign on field exit + the Insert-mode surface | ✅ |

TB.0 is a prerequisite, not a courtesy: `table-mode` inherits org's
`<leader>t…` set and would have shipped it dead.

TB.1 and TB.2 are deliberately two commits in two repositories, in that order.
Landing the plugin's removal first would take the feature away from org users
until the native mode arrived; landing it after means both bind `<Tab>` for
the length of one commit, which is a transient the plugin commit resolves.

---

### TB.0 — `<leader>` on the native mode-keymap path ✅

OM.2b made `<leader>` expand at bind time, at the single choke point every
*string-bound* binding funnels through — `try_bind_chord_string`, which is the
plugin-mode, plugin `register-binding` and `init.rs` route.

The **native** mode-keymap route does not go through it. It parsed
`entry.chord` raw, so `<leader>` reached `parse_chord_sequence` as an unknown
special key, the parse failed, and the binding was dropped with a `warn`.

No native mode had used `<leader>` yet, which is the only reason this had
never fired. Found before writing `table-mode` rather than after, which is why
it is a one-line fix instead of a debugging session. The test asserts the trie
holds `<Space>z` — the substituted default — rather than merely that something
is bound: a literal `<leader>` sitting in the trie is a binding nobody can
type, and would pass a weaker assertion.

### TB.1 — `table-mode` ✅

The shared minor, at parity with what `org-table-mode` carried: `<Tab>` /
`<S-Tab>` cell walk, align, row up/down, column left/right, insert and delete
row and column. Design §1–§7 carries the reasoning; what follows is what the
slice actually did.

**Three modules, one engine.** `layout.rs` (HP.1, already here) keeps the
unattended whole-document pass `lattice-help` uses. `model.rs` adds the
interactive view — table-at-caret, dialect-preserving render — and `edit.rs`
the structural operations, both built on `layout`'s own `cells` /
`visible_width` / `render_row` rather than a second copy. `mode.rs` is the
mode, its keymap and its bodies.

**One behaviour change to the existing engine.** An explicit `:---` left
marker used to be dropped on render, since left is the default and the output
is identical. Fine for generated help pages, wrong the moment the same engine
realigns the user's own file: a `:` they typed vanishing from a git diff is a
change they did not make. `Align::LeftMarked` round-trips it; the alignment it
means is unchanged, and the help-page pass gets the fidelity for free.

**The layering is the tested half.** `crates/lattice-host/tests/table_mode_layering.rs`
pins that `<Tab>` declines outside a table (so org's cycle and the builtin
jump still get it) and that every `<leader>t…` chord returns `Effect::None`
instead — because a decline re-runs a multi-key chord's trailing key alone,
and `<leader>tdc` declining would fire `c` and sit waiting for a motion. The
same file asserts each chord's *target* rather than merely that something is
bound: `translate_mode_keymaps` drops an entry whose command name does not
resolve, so a typo between keymap and registration is a silent keymap.

### TB.2 — the org plugin sheds the generic surface ✅

`src/table.rs` and the eleven generic chords + actions are gone from the org
plugin. `org-table-mode` stays registered with an **empty keymap** as org's
table home (design §5) — until `#+TBLFM:` lands it is a declared placeholder,
which is worth saying plainly rather than papering over. Deleting it and
re-adding it then would rename an entry in the plugin's mode list to buy
nothing; keeping it is what makes the intent of the split legible.

The callback ids 21..=31 are left as a **gap** rather than reused: a callback
id is a wire value between the guest's registration and the host's dispatch,
and shifting the ones above it to close a hole would silently re-point every
action after it.

**The plugin's suite found two real bugs in TB.1**, which is the argument for
doing the two slices in this order rather than merging them:

1. `rewrite` computed the replaced range's end line from the RENDERED result.
   Align and the two swaps do not change the row count, so old span and new
   agreed and it looked correct — then insert addressed a line past the
   buffer's end and did nothing, and delete left its last row behind. Every
   unit test asserted the rendered *text* and none the range, which is exactly
   why they all passed. `the_replaced_range_is_the_old_table_not_the_new_one`
   now asserts it in both directions.
2. A `| a | b |` line inside `#+BEGIN_SRC` was taken for a table. OT.7 found
   this in the plugin and answered it with the tree; that answer does not
   survive the move, because `lattice-mode` has no tree-sitter dependency and
   should not grow one to ask about `#+BEGIN_SRC`. Counting delimiters from
   the top of the file is the same answer in `O(lines)` of `starts_with` —
   once per chord, not per frame.

`tab_off_a_table_falls_through_to_the_headline_cycle` passes unmodified, which
is the assertion that the decline chain survived having one of its links move
to another crate.

**One chord DID change, and TB.1's commit message said otherwise.** It claimed
org users keep the same keys; that is true of the `<leader>t…` family and
false of align, which was `<leader>o|` and is now `<leader>t|`. The move is
right — `<leader>o…` is org's namespace and a generic table mode should not
squat in it, and align sitting with the rest of the table family is more
coherent than org's old split across two prefixes — but it is a break in
muscle memory and belongs in the record rather than in a surprise.

**And TB.2's verification was weaker than reported.** The plugin's suite reads
`target/wasm32-wasip2/release/`, and that component was days stale, so the
"108 passed" ran the OLD binary. Two tests pressing the removed `<leader>o|`
went unnoticed — one failing honestly, one PASSING because an unbound chord
leaves the buffer unchanged and "unchanged" is what it asserted. Both fixed in
the plugin repo. The lesson generalises past this plan: a suite that loads a
built artefact is not verified by `cargo test` alone.

### TB.3 — beyond parity ✅

Six operations and one behaviour change, all generic — every one of them is
as true of a markdown table as an org one, which is why they land here rather
than in `org-table-mode`.

| Chord | Operation |
|---|---|
| `<Tab>` at the last cell | **adds a row** and lands in it |
| `<leader>t-` | Insert a horizontal rule below |
| `<leader>ts` / `<leader>tS` | Sort this section by the column at the caret, ascending / descending |
| `<leader>tb` | Blank the cell |
| `<leader>ty` | Copy the cell down, incrementing a trailing number |
| `<leader>tT` | Transpose the table |

**`<Tab>` adds a row at the end**, which is emacs' behaviour and how people
actually build a table — type a row, Tab, type the next. `Table::next_cell`
still answers `None` there rather than inventing it: "add a row" and "leave
the table" are both defensible, and the choice belongs to the surface. The
model kept the seam TB.1 left it.

**Sorting is per SECTION, and picks its comparator from the data.** Per
section because a rule separates a header from a body, and a sort that crossed
it would drag the header into the middle of the data. Numeric when every
non-empty value in the column parses as a number, case-insensitive
lexicographic otherwise — emacs prompts `a`/`n`/`t`, and a prompt on a single
chord is a question with an obvious answer in nearly every real table, where
getting it wrong is one undo. `10` sorting after `9` is the single most
noticeable way a sort can be wrong, and it is exactly what lexicographic order
gets backwards.

Empty cells sort **last in both directions**: an empty cell is the absence of
a value rather than a small one, so floating it to the top of a descending
sort would bury the rows you asked to see. The caret follows the row it was
on, because you sorted to see where your row went.

**Copy-down increments the trailing integer** (`Q3` → `Q4`, `09` → `10` with
its width kept), which is what makes it a series filler rather than a
duplicator. It creates the row below when the caret is on the last one —
stopping at the bottom would fail exactly when you are filling a column
downwards.

**Transpose drops the rules, and cannot do otherwise.** A rule separates row
groups; after the swap those groups are columns, and there is no horizontal
line that means what it meant. Emacs drops them too. Stated in the code so it
does not read as a bug in a diff.

**The rule fallback is the one place the dialect is not in the buffer.**
Design §2 says the table says which dialect it is — and a table with *no rule*
has not said. A new rule copies the style of one the table already has; only a
ruleless table needs a default, and the file's **extension** answers it (`.org`
→ `+`, everything else → `|`). Crude but true: the thing that decides whether
`|---+---|` is idiomatic is whether this is an org file. Not an option (§2's
reason — a second source that can disagree), and not the buffer's major
either, which from a grammar action body would mean a `lattice-syntax`
dependency in `lattice-mode` or a new `ActionContext` field, a large edge for
one character. A host test pins that an existing rule beats the file type.

All six chords sit behind `<leader>t`, so like their TB.1 peers they
**consume** outside a table rather than declining — a decline re-runs the
trailing key alone, and `<leader>tS` would fire `S`.

### TB.4 — realign on field exit, and the Insert-mode surface ✅

Design §8 carries the reasoning and the measurements. Three things happened
here.

**The measurement said cost is not the constraint.** A 5-row table realigns in
**7.5 µs** — under 0.1% of a 120 Hz frame; a 500-row one in 829 µs; a
not-a-table line costs **22 ns**. What vetoes per-keystroke realign is the
keystroke UX contract, not the clock: a realign rewrites every row, so per
keystroke it is by construction a pixel change to content the user did not
edit. The bench lands anyway (`crates/lattice-mode/benches/table.rs`) so the
next person does not re-litigate this as a performance question.

**The premise was wrong, and that is recorded rather than quietly dropped.**
This slice was written as "emacs realigns a table on every edit inside it".
Emacs does not — `org-table-align` is called from `org-table-next-field`,
`org-table-previous-field` and `org-return`, i.e. **when you leave a field**.
Type into a cell in emacs and the table stays ragged until you `TAB` out. So
the emacs-parity answer and the contract-safe answer are the same answer, and
the debounce this slice was reserved to argue about is not needed.

**Same keys, different results by modal state.**

| Chord | Normal | Insert |
|---|---|---|
| `<Tab>` / `<S-Tab>` | Next / previous cell, realigning | Same, **staying in Insert** |
| `<CR>` | *unbound* | Next **row**, same column; adds a row at the bottom |
| `<Esc>` | *unbound* | Realign, then fall through to the native exit-insert |

`<Tab>` reuses the Normal bodies verbatim — moving to a cell and realigning is
one operation, and it emits no `Effect::EnterMode`, so Insert is simply kept.
A second pair of handlers to say that would be the duplication this mode
exists to end.

`<CR>` is Insert-only, and the asymmetry is the design: in Normal it already
means something a table row wants (first non-blank of the next line); in
Insert it means "split this line", which inside a table row is never what you
meant. It keeps the COLUMN where `<Tab>` wraps — that is the difference
between filling a column and filling a row — and adds a row at the bottom, so
`<CR>` is how you enter a table's contents. You leave with `<Esc>`.

`<Esc>` carries `fall_through: true`. Without it the binding would trap the
user in Insert inside every table, which is about the worst outcome available;
with it the mode realigns and never hardcodes what `<Esc>` natively means.
`active-snippet-mode`'s `<Esc>` is the precedent.

**No edit when nothing changed.** Every body now compares the rendered table
to the buffer and emits `Effect::CursorMove` instead of `Effect::ApplyEdit`
when they match. Without it, `<Tab>` through an aligned table pushes an undo
entry per cell and `<Esc>` one per Insert exit — undo steps for edits that
changed nothing, which makes `u` stop meaning "undo my last change".

**One test was wrong before the code was.** The popup-precedence check first
asserted against `resolve_trace`, which lists every hit in enumeration order
for `:describe-key` and says nothing about which one wins — it reported
`table-mode` beating the completion popup and looked like a real UX bug. The
ranking path is `lookup_with_context`, which folds gated layers in
`active_modes` order; the test goes through that now and pins both directions
(popup up → popup wins; popup down → the table gets `<Tab>` back).

### Not this mode's

- **Formulas (`#+TBLFM:`)** and recalculation — org's, so `org-table-mode`'s.
  The reason that mode is still registered.
- Rectangle cut/copy/paste, coordinate overlays, `org-table-create-with-table.el`
  — org-specific or niche enough that nothing has asked.
