+++
title = "Select mode (vim Select) — design fragment"
+++

# Select mode (vim Select) — design fragment

**Status:** designed (SN.3d), not yet implemented. Slice sequencing lives in
the [mode-activation slice plan](../operations/slice-plans/archive/mode-activation)
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
a `selection_is_active()` helper (`is_visual() || is_select()`) is added for
the *subset* of `Range::Selection`-default callers that genuinely also fire in
Select.

**Do NOT blanket-rename every `is_visual()` caller to
`selection_is_active()`.** In Select, printables overtype — you cannot invoke
an operator, so most operator-dispatch callers stay Visual-only. The only path
that runs a Normal-style operator against a *Select* selection is `<C-o>`
(one-shot Normal, post-MVP). A blanket replace is therefore a silent
behaviour change at Visual-only sites. Audit each `is_visual()` caller and
convert per-site, with a one-line reason; the default is "leave as
`is_visual()`".

**Blast radius (heuristic #4 — confirm the impact surface).** Adding a
`ModalState` variant is not "just another arm". There are ~677 `ModalState::`
use-sites across the tree; several are *exhaustive* matches that hard-break the
build until they gain a `Select` arm (e.g. `lattice-host/src/cursor_shape.rs`
matches all variants with no catch-all). The quieter risk is the
`ModalState::Visual(kind)` sites that *should* treat `Select(kind)` identically
(selection render, cursor shape, `Range::Selection` default) but won't
fail to compile if they silently ignore it — a Visual arm without a sibling
Select arm is a latent bug. The slice plan carries the audit + parity test as
explicit work (see slice plan SN.3d.0 and the §9 audit item).

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
selections; Select mode owns "a keystroke here means overtype".

**Pin the edit shape — it is ONE replace-range edit, not delete-then-insert.**
The overtype emits a single edit that replaces the selection span with the
typed char (the char becomes the replacement text), plus `EnterMode(Insert)`
with the cursor placed after the char. Concretely a single
`Effect::Edits([replace(span → c)])`, **not** `Effect::Many([delete, insert])`
(two effects risk two undo units). `Document::apply_edit_batch` already lands a
batch as one undoable unit, so a single `u` undoes the whole overtype. Describe
the behaviour as "delete + insert" for the user, but implement it as one edit.

Note `translate_select` is **genuinely new dispatch logic**, not "`dispatch_visual`
plus a tweak": `dispatch_visual` has no literal-printable fallthrough (an unbound
printable in Visual is a no-op). The right reference for the fallthrough is
`dispatch_insert`'s `literal_text_fallback`, mapped to the replace-and-insert
edit above.

## 4. Keymap + dispatch

- A `BindingMode::Select` chord table. Motions and selection-extending chords
  are conceptually shared with Visual but must be registered under the Select
  layer too. Today Visual bindings are registered *imperatively* in
  `register_visual_bindings` (`lattice-host/src/keymap_visual.rs`); a
  hand-copied Select layer would drift the moment a Visual motion is added.
  **Decision (LOCKED): duplicate the registration in a parallel
  `register_select_bindings`, guarded by a parity test** asserting every
  `BindingMode::Visual` motion/extension binding has a `BindingMode::Select`
  counterpart. The parity test — not a shared source list — is the drift guard:
  it fails loudly the moment the two layers diverge, keeps each registration
  function readable on its own, and avoids a speculative shared-list abstraction
  before a second consumer exists (heuristic #1 — no rewrite-for-its-own-sake;
  the test carries the merit). `<Esc>`, `<C-g>`, `<C-o>` are the mode-control
  chords. **`<C-g>` is reserved in both Visual and Select for the toggle** —
  confirm it is not already bound in Visual before wiring (it appears free
  today; vim's Normal `<C-g>` file-info is unrelated and unaffected).
- Entry chords `gh` / `gH` / `g<C-h>` register under the **`AfterG`** table
  (the `g` prefix), not the Visual/Select layers; confirm `AfterG` accepts a
  ctrl-modified second key for `g<C-h>`.
- Bare printable characters are **not** individually bound; the Select
  dispatcher's fallthrough (mirroring `dispatch_insert`'s
  `literal_text_fallback`) maps "an unbound printable in Select" to the
  replace-and-insert edit (§3).
- **Select consults active minor-mode keymaps (SN.3d.4).** Like
  `dispatch_insert` — and unlike `dispatch_visual`, whose minor-mode
  layer push is still outstanding — `translate_select` takes
  `active_minor_modes` and looks the chord up against the active minor
  layers FIRST. It intercepts only a winner on a `KeymapLayer::MinorMode`
  layer (a base-table `Bound` is a motion/text-object the native path
  owns). A `fall_through` minor binding runs its mode action and then
  chains the *native* Select action for the same chord — so the snippet
  `<Esc>` clears the session and then falls through to the hardcoded
  `<Esc>` → `ExitSelect`. This is what makes a mode that focuses a span
  (the snippet placeholder default, §6) own its full chord surface in
  Select, not just Insert — no half-migration
  (`feedback_mode_owns_its_surface`).
- Dispatch threads through the renderer-neutral host entry point
  (`lattice-host/src/input.rs` `translate`, shared by both the TUI and GPUI
  chord adapters), with a new `ModalState::Select(kind) => translate_select(...)`
  arm beside the existing `Visual(kind) => dispatch_visual(...)` arm — so the
  *dispatch* hub is one new arm, but see §2 (blast radius) for the many other
  `ModalState` match sites that also need a Select arm.

## 5. Rendering + introspection

- **Selection highlight:** Select renders the selection exactly like Visual
  (same decoration path) — the user sees the placeholder/field highlighted.
  TUI + GPUI parity (`feedback_tui_gpui_parity`, same patch): both peers
  already render `Visual` selections, so the Select arm reuses each path —
  TUI per-kind predicate in `lattice-ui-tui/src/render.rs` (~the `match v.kind`
  reversed-cell block), GPUI decoration-background path keyed on `ModalState`
  in `lattice-ui-gpui/src/window.rs` + `editor_element.rs`. **Gotcha:** there
  are two `VisualKind` types — `lattice_grammar::VisualKind` and
  `lattice_terminal::VisualKind` (the render path converts grammar→terminal);
  the Select render arm needs the same conversion plumbed.
- **Cursor shape:** `cursor_shape.rs` is an *exhaustive* match and will not
  compile without a `Select(_)` arm. Decision: `Select(_) => Block`
  (Visual-parity) — the selection conveys the mode; the cursor matches Visual.
- **Status line:** `-- SELECT --` / `-- SELECT LINE --` / `-- SELECT
  BLOCK --`, beside the existing `-- VISUAL --` family.
- **`:describe-mode select`** + the help catalog document the mode and its
  chords like every other modal state (§5.11).
- **Macros:** macros record `CommandInvocation` sequences, not keystrokes
  (design.md). The printable→overtype is a dispatcher fallthrough, not a bound
  command, so it must emit a *recordable* invocation (the same way Insert's
  literal-text path does) — otherwise replay can't reproduce a Select overtype.
  Covered by a record+replay test (§9).

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

**The real interaction risk lives here.** Overtyping a focused `${1:default}`
replaces the whole "default" span in one edit, then lands in Insert. The
snippet session's marker/range tracking must (a) resync tabstop 1's range to the
new text and (b) ripple to its mirrors. This is the same marker-adjust path as
typing-in-Insert-at-a-placeholder, but the whole-span replace is the stress
case — it is the thing most likely to break and must be tested directly (§9).
Because **SN.3e (buffer-keyed session) lands first**, the Select-entry
`Effect::SelectionChange` must target the *reconciled* buffer, not a global
slot.

**Navigation + leave must stay live in Select (SN.3d.4).** Selecting the
default is only half the flow: the user must still be able to `<Tab>` /
`<S-Tab>` *past* a placeholder (keeping its default) and `<Esc>` to leave the
snippet — while a placeholder is Select-focused. The snippet minor mode
therefore registers the same `<Tab>` / `<S-Tab>` / `<Esc>` bindings for
`BindingMode::Select` as it does for Insert, and Select dispatch consults
active minor-mode keymaps (§4) so they fire. Without this the bindings would be
dead the instant a default-bearing placeholder selected — the snippet would own
its surface in Insert but not in Select, exactly the half-migration
`feedback_mode_owns_its_surface` forbids. `<Esc>` is `fall_through`: clear the
session, then `ExitSelect` → Normal.

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
- **Impact-surface audit (heuristic #4):** enumerate every exhaustive
  `ModalState` match that needs a `Select` arm (`cursor_shape.rs`, the
  `input.rs` dispatch hub, GPUI `window.rs`, TUI `render.rs`, the TUI `app/*`
  band); and audit each `ModalState::Visual(kind)` site for whether it needs a
  sibling `Select(kind)` arm. Ship a **parity test**: Select behaves like
  Visual everywhere geometry-only behaviour is expected (selection render,
  cursor shape, `Range::Selection` default), and every `BindingMode::Visual`
  motion/extension binding has a `BindingMode::Select` counterpart.
- **Tests:** state-machine transitions (enter from Normal/Visual/programmatic;
  printable → **single replace-edit + Insert** with single-undo; motion
  extends; `<Esc>` → Normal; `<C-g>` ↔ Visual); empty-placeholder → cursor;
  snippet focus enters Select over a multi-char default whose tabstop has ≥1
  mirror — assert the mirror updates **and** a subsequent `<Tab>` lands on the
  correct post-overtype range; macro record+replay of a Select overtype
  reproduces it; failure modes (printable in an empty selection is a plain
  insert).
- **TUI + GPUI parity:** selection-render + status-line + cursor-shape arms in
  lockstep, with the grammar→terminal `VisualKind` conversion plumbed on the
  Select render arm.
- **Graceful:** Select with a degenerate (zero-width) selection behaves as
  Insert-at-cursor, never panics.

## See also

- [keymap-architecture.md](keymap-architecture) — layer model + dispatch
  the Select `BindingMode` plugs into.
- [design.md §5.2](design) — modal-state catalog (Select added there).
- [mode-activation slice plan](../operations/slice-plans/archive/mode-activation)
  — SN.3d sequencing; SN.3e (buffer-keyed snippet session) lands first.
