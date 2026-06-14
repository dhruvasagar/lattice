# Select mode (vim Select) — design fragment

**Status:** designed (SN.3d), not yet implemented. Slice sequencing lives in
the [mode-activation slice plan](../operations/slice-plans/mode-activation.md)
(SN.3d). This file is the stable *what* + *why*.

## 1. What it is

Select mode is a **first-class vim modal state**, a sibling of Visual mode.
It has the same *selection extent* as Visual — charwise / linewise /
blockwise — but inverted *typing* semantics:

- **Visual:** printable keys are commands (operators, motions). `d` deletes,
  `w` extends by a word.
- **Select:** a printable key **replaces the whole selection with that
  character and drops into Insert mode**. Motions still extend the
  selection; the chord vocabulary that isn't "type a character" stays.

This is real vim (`:help Select-mode`). Vim enters it via `gh` / `gH` /
`g<C-h>` (charwise / linewise / blockwise), or by toggling from Visual with
`<C-g>`; GUI/`mouse`/`selectmode` selections start there too. The defining
behaviour — *"select something, type to overtype it"* — is exactly what
LSP/VSCode snippet placeholders want, but it is **not snippet-specific**:
rename-style overtype, template fields, "select the word and replace it",
and any plugin that wants select-and-replace all reach for it.

**Decoupling is the whole point (per the user, 2026-06-14).** Select mode is
built as a core grammar feature with zero knowledge of snippets. Snippets are
one *consumer*: focusing a non-empty placeholder enters Select mode over the
placeholder span. The two never reference each other.

## 2. Data model

Mirror Visual, which already carries the three sub-kinds:

```rust
// lattice-grammar/src/modal.rs
pub enum ModalState {
    Normal,
    Insert,
    Visual(VisualKind),
    Select(VisualKind),   // NEW — same extent kinds as Visual
    OperatorPending,
    Command,
    Search(SearchDirection),
    Replace,
}
```

`VisualKind` (`Charwise` / `Linewise` / `Blockwise`) is reused verbatim —
the selection *geometry* is identical to Visual; only keystroke dispatch
differs. `ModalState` gains an `is_select()` predicate beside `is_visual()`;
a `selection_is_active()` helper (`is_visual() || is_select()`) backs the
callers that supply `Range::Selection` as a default range, so an active
Select selection is a valid operator target too.

Keymap side (`lattice-keymap/src/binding_mode.rs`):

```rust
pub enum BindingMode {
    …
    Visual,
    Select,   // NEW — its own chord table / layer
    …
}
```

`Select` is its own `BindingMode` rather than a flag on `Visual` because the
**dispatch of a bare printable key fundamentally differs** (Visual: command
lookup; Select: replace-and-insert). Sharing the table and branching on a
flag at every printable would be the kind-gate the architecture forbids;
distinct binding modes keep the difference in the lookup, not in scattered
`if select` checks.

## 3. Transitions (the state machine)

Entry:

| From | Trigger | Result |
|---|---|---|
| Normal | `gh` / `gH` / `g<C-h>` | `Select(Charwise/Linewise/Blockwise)`, anchor at cursor |
| Visual(k) | `<C-g>` | `Select(k)` — toggle, selection preserved |
| (any) | `Effect::EnterMode(Select(k))` + a selection | programmatic entry (how snippets use it) |

Exit / within:

| In | Key | Result |
|---|---|---|
| Select(k) | printable `c` | delete selection → enter `Insert` → insert `c` (one undo step) |
| Select(k) | `<Esc>` | collapse selection → `Normal` |
| Select(k) | `<C-g>` | toggle back to `Visual(k)` |
| Select(k) | motion (`w`, `j`, …) | extend the selection (Visual-identical), stay in Select |
| Select(k) | `<C-o>` | one-shot Normal command, return to Select (vim parity — optional, post-MVP) |

The **printable → replace + Insert** step is the load-bearing new behaviour
and it lives entirely in the Select dispatch path (`translate_select` +
the host handler), NOT in `do_insert_text`. Insert mode stays unaware of
selections; Select mode owns "a keystroke here means overtype". The replace
+ insert is a single coalesced edit so one `u` undoes the whole overtype.

## 4. Keymap + dispatch

- A `BindingMode::Select` chord table. Motions and selection-extending
  chords are shared with Visual conceptually but registered under the Select
  layer (the catalog can generate both from one source to avoid drift —
  decided at implementation time). `<Esc>`, `<C-g>`, `<C-o>` are the
  mode-control chords.
- Bare printable characters are **not** individually bound; the Select
  dispatcher's fallthrough (mirroring `dispatch_insert`'s
  `literal_text_fallback`) maps "an unbound printable in Select" to the
  replace-and-insert action.
- Dispatch threads through the same `translate(ctx, event)` entry point as
  every other mode (`ModalState::Select => translate_select(...)`), so the
  renderer/runtime integration is unchanged — Select is just another arm.

## 5. Rendering + introspection

- **Selection highlight:** Select renders the selection exactly like Visual
  (same decoration path) — the user sees the placeholder/field highlighted.
  TUI + GPUI parity: both peers already render `Visual` selections; the
  Select arm reuses that path (one match arm each), no new render surface.
- **Status line:** `-- SELECT --` / `-- SELECT LINE --` / `-- SELECT
  BLOCK --`, beside the existing `-- VISUAL --` family.
- **`:describe-mode select`** + the help catalog document the mode and its
  chords like every other modal state (§5.11).

## 6. How snippets consume it (SN.3d's actual payoff)

`snippet_group_cursor_effect` (`lattice-snippet`) currently emits
`Effect::SelectionChange(cursor)`. It changes to: for a **non-empty**
placeholder (`${1:default}`), emit "enter Select mode over the span" using
*existing* effects — `Effect::Many([SelectionChange(start..end as a
charwise selection), EnterMode(Select(Charwise))])`. For an **empty**
placeholder (`$1`, zero-width), keep the bare `Insert`-mode cursor (nothing
to overtype). No new snippet↔grammar coupling: the snippet only knows
"select this range in Select mode"; Select mode owns the overtype.

This makes the existing doc/comment claim — "keep typing inside a
placeholder to overtype the default" (`docs/user/completion.md`, the
`active-snippet-mode` doc-comment) — finally true.

## 7. Rejected alternatives

- **A flag on Visual (`Visual { kind, select: bool }`) instead of a distinct
  state.** Rejected: the printable-key dispatch diverges completely, so every
  Visual keystroke path would branch on the flag — a kind-gate smell. A
  distinct `ModalState`/`BindingMode` keeps the divergence in the lookup.
- **No Select mode; snippets flip modal → `Visual` on focus and rely on
  Visual `c`.** Rejected: Visual `c` needs an explicit `c`, not "type to
  replace"; it also leaves the user "in Visual" mid-snippet with the full
  operator vocabulary live, which is the wrong affordance. And it would
  couple the snippet feature to a workaround instead of building the real
  vim primitive.
- **Add "overtype a selection" to Insert's `do_insert_text` directly.**
  Rejected: that bolts selection-replace onto Insert (a behaviour Insert
  shouldn't own) and gives no mode to *show* the selection or to scope the
  overtype — exactly the state Select mode exists to model. It also wouldn't
  serve the non-snippet use cases.

## 8. Paramount-goal alignment

> **UX (higher court):** select-and-overtype is the cross-editor convention
> for placeholders/rename/template fields (`feedback_convention_first`);
> building the real mode (not a snippet hack) makes it behave consistently
> wherever it's used.
> **Paramount #3 (extensible vim modal editing):** Select is *strict vim*
> grammar — a modal state vim itself ships. Adding it is first-class modal
> work, the grammar IS the public command API. It composes with operators
> (`Range::Selection`), counts, and the keymap layers like every other mode.
> **Paramount #2 (extensibility):** a reusable primitive — plugins and future
> features (rename, template fields) get select-and-replace for free, with no
> per-feature host wiring.
> **Heuristic #1 (long-term fit):** the genuinely-better design is the real
> mode, not a snippet-coupled overtype patch; the rejected alternatives are
> the quick fixes.
> **Heuristic #2 (paramount, not other editors):** justified on strict-vim
> grammar (#3), not "VSCode does it" — VSCode is cited only for the UX
> convention, which is the higher-court concern.

## 9. Surface this slice ships (four-artefacts)

- **Design:** this fragment.
- **Tests:** state-machine transitions (enter from Normal/Visual/programmatic;
  printable → replace+Insert with single-undo; motion extends; `<Esc>` →
  Normal; `<C-g>` ↔ Visual); empty-placeholder → cursor; snippet focus enters
  Select over a multi-char default and a mirror edit still ripples; failure
  modes (printable in an empty selection is a plain insert).
- **TUI + GPUI parity:** selection-render + status-line arms in lockstep.
- **Graceful:** Select with a degenerate (zero-width) selection behaves as
  Insert-at-cursor, never panics.

## See also

- [keymap-architecture.md](keymap-architecture.md) — layer model + dispatch
  the Select `BindingMode` plugs into.
- [design.md §5.2](design.md) — modal-state catalog (Select added there).
- [mode-activation slice plan](../operations/slice-plans/mode-activation.md)
  — SN.3d sequencing; SN.3e (buffer-keyed snippet session) lands first.
