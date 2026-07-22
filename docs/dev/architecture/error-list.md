# Error list

Authoritative design for Lattice's **error list**: a single,
editor-wide, navigable list of source locations (`file : line : col`
+ message + severity) that the user steps through with `:next-error` /
`:previous-error` / `]qq` / `[qq`. This is vim's **quickfix** list,
rebuilt as a **core substrate** decoupled from any producer or buffer —
the user-facing name is "error list"; the vim `:c*` command family and
the `q`-chords are preserved as aliases for muscle memory.

Companion to `compilation-mode.md` (the first producer) and
`design.md` §5.1.1 (position history — a sibling core navigation
substrate). Sequencing lives in
`../operations/slice-plans/compilation-mode.md` (slices CM.2, CM.7, CM.8).

## 1. It is core state, not mode-owned

The error list is read by **generic host dispatch** — the `:next-error`
family and the `]qq` / `[qq` chords — uniformly, regardless of which
buffer is focused or what produced the list. By the substrate-vs-mode
rule (uniform-host consumer ⟹ core, not mode) it is **core `Editor`
state**, shaped like `position_history`:

```
ErrorEntry { path: PathBuf, line: u32 /*0-based*/, col: u32 /*0-based*/,
             severity: ErrorSeverity, message: String }
ErrorList  { entries: Vec<ErrorEntry>, index: usize }
ErrorSeverity { Error, Warning, Info, Note }
```

`ErrorEntry` + `ErrorSeverity` live in **`lattice-protocol`** (so
producers *below* `lattice-host` can construct them and they can ride
inside a typed `AppEffect`); `ErrorList` lives on `Editor` in
`lattice-host` (the `error_list` field) with `set_error_list(entries)`
(replace + reset index) and `error_list() -> &ErrorList`.

This placement is load-bearing: a mode-private list (owned by
`compilation-mode`) would (a) vanish when the producing buffer closes,
and (b) block any *other* producer — project search, future tools —
from ever feeding the same navigation. Core state lets many producers
share one list and one set of motions.

## 2. Navigation is buffer-independent (the vim model)

Once populated, the list is walked from **anywhere** — the producing
buffer (e.g. [`*compilation*`](compilation-mode.md)) need not be
focused or even open, and closing it does **not** clear the list (only
a new run's replace does). This deliberately follows vim, not emacs
(where next-error lives on the compilation buffer). It falls out of §1
for free: the state is on `Editor`, the commands are generic.

### Commands

Readable, emacs-style canonical names lead; vim `:c*` spellings are
aliases (`lattice-host::excommand`). Each steps via the canonical
`Editor::jump_to_file_line_col(path, line, col)` — which records the
jump in position history (§5.1.1):

| Command | Vim alias | Chord | `ErrorTarget` |
|---------|-----------|-------|---------------|
| `:first-error` | `:cfirst` `:cr` | `[Q` | `First` |
| `:previous-error` | `:cprev` `:cp` | `[qq` | `Prev` (wraps) |
| `:previous-error-file` | `:cprevfile` `:cpf` | `[qf` | `PrevFile` (wraps) |
| `:next-error` | `:cnext` `:cn` | `]qq` | `Next` (wraps) |
| `:next-error-file` | `:cnextfile` `:cnf` | `]qf` | `NextFile` (wraps) |
| `:last-error` | `:clast` | `]Q` | `Last` |
| `:error [N]` | `:cc [N]` | | `Jump(Option<usize>)` (1-based) |

`NextFile` / `PrevFile` land on the first entry of the next / previous
**file** — a "file" is a maximal run of consecutive entries sharing a
path (producer output groups a file's locations together). Both
directions land on the *first* entry of the target file (deliberately
unlike vim's `:cpfile`, which lands on the *last* of the previous
file — first-of-file lets a following `:next-error` walk it
top-to-bottom).

**Command vs. chord letters differ by design** — as in vim itself
(command `:cnext`, unimpaired chord `]q`). The commands lead with the
emacs `next-error` vocabulary; the chords keep vim-unimpaired's `q`
(the universally-known quickfix chord): **`[`** backward / **`]`**
forward, doubled **`qq`** one entry, **`qf`** a whole file, capital
**`Q`** the extremes. `[q` / `]q` are therefore prefixes (not bound
directly). `e`/`E` were *not* chosen for the chords — they are taken by
`lattice-multibuffer`'s excerpt / file-boundary navigation. All chords
are **Builtin** grammar (universal navigation over a core substrate,
like `<C-o>` / `<C-i>` over the jump ring), firing in any buffer.

Navigation flows through `AppEffect::ErrorNav { target }` → the host
`do_error_nav` handler; the ex-commands are thin front-ends emitting
that effect.

### No diagnostic fallback

An empty error list echoes `no error list` for **every** target —
error-list commands touch only the error list (vim's `E42: No Errors`).
Diagnostics are **not** coupled in: they own dedicated navigation —
`[d` / `]d` (current-file, mode-owned by `lsp-diagnostics-mode`) and
the `:diagnostics` picker. Keeping the two apart is the mode-ownership
boundary; an earlier empty-list fallback-to-diagnostics was removed
(CM.7) as a boundary blur.

## 3. Producers feed it over a native seam

A producer builds `Vec<ErrorEntry>` and hands it to `set_error_list`.
Producers below `lattice-host` (e.g. `lattice-compilation`) deliver
off-thread via the native `InboundBus → AppEffect::SetErrorList
{ entries }` seam (the host arm calls `set_error_list`) — the same
transport pattern LSP uses for its own async host-state updates,
**not** any plugin path.

Today the sole producer is [compilation mode](compilation-mode.md),
whose tool-agnostic parser turns any CLI tool's `file:line:col` output
into entries. The list is producer-agnostic by construction: project
search (and other tools) can feed the identical list later with zero
change to the navigation above. LSP diagnostics are deliberately **not**
a producer — they keep their own navigation surfaces (§2).

## 4. Three views of one list

The entries are worked three ways, all reading the same `ErrorList`:

- **Step** — `:next-error` / `]qq` … (§2), one entry at a time.
- **Pick** — `:error-list` / `:cl` opens a **fuzzy picker** of the flat
  list (`Editor::do_list_errors`, host-side), reusing the shared
  `PickerSource::LspLocations` + `JumpToLspLocation` accept path — the
  exact mechanism `:diagnostics` (`do_list_diagnostics`) uses, rows
  sourced from `error_list().entries()` instead of the diagnostics layer.
- **Group** — `:problems` (`*problems*`) renders the entries as a
  grouped, editable [multibuffer](multibuffer-views.md) — one excerpt
  per entry under each file's header, edits propagating to source. See
  `compilation-mode.md` §4 for the provider.

The picker and problems-view are the browse surfaces; the `:next-error`
family is the step surface. All three are views of one list.

## 5. Paramount-goal alignment

- **#1 Performance.** Navigation is O(1) list indexing + the existing
  (benched) `jump_to_file_line_col` path; population is off-thread
  (producer side). No UI-thread work.
- **#3 Everything-is-a-buffer.** The list is generic core state (like
  the jump ring); the `*problems*` view is a plain `Multibuffer`
  Document — no kind-branching.

## 6. Rejected alternatives

- **List owned by `compilation-mode`.** Rejected: consumer is generic
  host dispatch, so it is core state (§1); a mode-private list dies
  with its buffer and monopolises the navigation for one producer.
- **List tied to the producing buffer (the emacs model).** Rejected:
  breaks the vim contract that you can `:next-error` from anywhere after
  the buffer is gone — and Lattice's producers (compilation today,
  search later) are many-to-one against the list.
- **Entry type in `lattice-host`.** Rejected: producers below the host
  must construct entries and they ride inside a typed `AppEffect`, so
  the type belongs in `lattice-protocol` (below both).
- **`:d*` diagnostic vocabulary / `e`-prefix chords.** Rejected: kept
  diagnostics on their existing `[d`/`]d` + `:diagnostics` (no parallel
  list), and kept the `q`-chords since `e`/`E` collide with multibuffer
  excerpt navigation and `q` is the recognized quickfix chord.
