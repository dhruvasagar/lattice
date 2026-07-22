# Popup API

A first-class, content-agnostic overlay surface that modes own and plugins will
inherit.

**Status:** implemented (PU-A, PU-B complete; popup_focused field delivered
2026-07-22). Referenced by `docs/dev/operations/slice-plans/acp-ux-enhancements.md`
(slices PU-A, PU-B).

---

## 1. Why this exists

`wit/ui.wit` already reserves the surface:

> The UI-contribution surface (design.md §9.4 `ui`): status/gutter segments,
> **popups**, notifications, sprite sets. Guest→host emits data only — never
> draw calls (§7, paramount #1).

And `crates/lattice-ui-tui/src/app/popup.rs` already states the contract:

> A popup is a rectangular UI surface drawn over the buffer area. Inside the
> popup a buffer renders the same way it would inside any other window / split /
> tab; the popup itself is content-agnostic and provides no key bindings of its
> own. Mode-specific behaviour comes from the buffer's major mode, exactly as it
> would in a split.

Before PU-A and the `popup_focused` migration, neither statement was true of the
code. `Editor::open_popup` took a `lattice_help::HelpContent`, branched on
`BufferKind::Help`, maintained a `popup_back_stack` for help's `<C-o>`, and
stashed focus in a field named `prev_pane_for_help`. Every popup was a help popup
wearing a different hat. The focus-detection mechanism — `active_buffer == Help` —
conflated State-B detection (does the popup have keyboard focus?) with content
identity (is the buffer kind Help?). **PU (popup unification) and the
`popup_focused` migration (2026-07-22) decoupled the two axes:**
- **State-B detection** → `Editor::popup_focused` (a dedicated bool, published in
  `ActiveDocumentRenderState.popup_focused`)
- **Content identity** → `BufferKind::Help` (unchanged; still the kind for
  document-shaped help/popup surfaces)

The immediate consumer is the ACP permission menu (`acp-ux-enhancements.md`
§4.1), which must show an agent-supplied option list. It could be bolted on as a
new `HelpContent`-shaped special case. That is the wrong trade: the next two
consumers (LSP code actions, `:confirm`) would pay it again, and the WASM plugin
surface would inherit a Help-shaped API it can never use.

## 2. What is already right

The rendering half is genuinely generic and is not touched by this design.

`Editor::synthetic_popup_panes` registers `popup_buffer` under the sentinel
`PaneId::POPUP`, and both renderers paint its interior through the shared
`compose_pane_lines` seam — the TUI at `render.rs` (`draw_help_overlay`, whose
body is a comment admitting "a help popup is now pixel-equivalent to a
`:set nonu signcolumn=no wrap` document in a box"), GPUI through its own
`popup_outer_dims_px` / `popup_inner_height_rows` path. Parity exists.

`popup_buffer` is already an `Option<BufferId>` — a *registered* buffer, not an
owned blob. Everything-is-a-buffer already holds for the live popup.

The two focus states already exist, unnamed:

- **State A** — `open_floating_popup`: the popup floats, `active_buffer` is
  untouched, `prev_pane_for_help` stays `None`. Hover and signature help.
- **State B** — `open_popup`: `active_buffer` flips to the popup buffer, whose
  own major mode receives the keystrokes. `:help`, `:apropos`, `:options`.

There is no special key routing for popups. `q` / `<Esc>` closing a help popup is
`HelpMode`'s keymap layer, not a popup feature. That is the whole reason this
design can exist.

## 3. The model

> A popup shows a registered buffer at a placement. It owns no behaviour, no
> content, and no result.

Three consequences, each load-bearing:

The focus-state axis is tracked by `Editor::popup_focused` (a dedicated bool,
replacing the old `active_buffer == BufferKind::Help` pattern which conflated
State-B detection with content identity). Published in
`ActiveDocumentRenderState::popup_focused` so both renderers read it directly.

**No behaviour.** Keys go to the buffer's major mode, which the mode that opened
the popup also owns. A permission menu's `1`/`2`/`3` bindings live in
`ai-permission-mode`, exist only while that buffer is focused, and vanish when it
is dismissed. This is what "transient" means here — not a temporary keymap layer
grafted onto the host, but bindings that belong to a buffer that stops existing.

**No content.** The effect carries a `BufferId`. The opener registers a synthetic
buffer with a major mode (the `OpenSyntheticBuffer { name, mode_id }` precedent)
and hands over its id. The popup's title is the buffer's name, because naming a
buffer is already the buffer's business.

**No result.** A menu must deliver its choice somewhere, but not through the
popup. The buffer's major mode owns the outcome. `ai-permission-mode` calls
`ConversationStore::resolve_permission(id, option_id)`, which already holds the
`oneshot::Sender<PermissionOutcome>` that answers the ACP request; the option id
is an agent-supplied `String`.

This last point is why the picker was rejected as the vehicle. The picker owns
`accept`, so it needs a closed `RoutingPayload` enum, and every new consumer must
add a variant in the host and picker crates — the host wiring the action while
the mode owns the body, which is exactly the half-migration the standing rules
forbid. A surface that owns nothing needs no enum, and plugins inherit it free.

## 4. Surface

### 4.1 Core types (`lattice-core::ui::popup`)

`PopupPlacement { Centered, CursorAnchored }` exists unchanged. Add:

```rust
/// Whether opening the popup moves focus into it. Names the distinction the
/// code already makes between `open_popup` (B) and `open_floating_popup` (A).
pub enum PopupFocus {
	/// Focus moves to the popup buffer; its major mode receives keys.
	Steal,
	/// The popup floats; the underlying buffer keeps focus and the caret.
	Passive,
}
```

### 4.2 Host primitive (`lattice-host`)

One content-agnostic entry point replaces the two Help-typed ones:

```rust
impl Editor {
	pub fn open_popup_buffer(
		&mut self,
		buffer: BufferId,
		placement: PopupPlacement,
		focus: PopupFocus,
	) -> Vec<RendererSignal>;

	pub fn dismiss_popup(&mut self);  // already idempotent

	// State-B detection: dedicated bool (replaces old
	// `active_buffer == BufferKind::Help` conflation).
	pub popup_focused: bool;

	// Content accessor: works for any document-shaped buffer
	// in the popup slot (Help, Document, Messages, ...).
	pub fn popup_buffer_content(&self) -> Option<lattice_core::Buffer>;
}
```

`Editor::open_help_popup(HelpContent, placement)` remains, as a caller: it
materialises the help buffer, pushes its own back-stack entry, and delegates.
`PopupSnapshot` and its `metadata: HelpMetadata` field move into `lattice-help`,
where they always belonged — they are help's `<C-o>` history, not popup state.

State-B detection uses `popup_focused`; `active_buffer_id()` and the buffer-kind
match arms in `active_text()` / `active_cursor()` are fallbacks for in-pane Help
(State-A equivalent), gated behind the `popup_focused` early return. The
renderer reads `ad().popup_focused` directly — no re-derivation from
`buffer_kind`.

### 4.3 Mode-facing effects (`lattice-grammar`)

```rust
Effect::OpenPopup { buffer: BufferId, placement: PopupPlacement, focus: PopupFocus }
Effect::DismissPopup
```

`DismissPopup` is a **rename of the existing `Effect::CloseHover`**, which
already calls `dismiss_popup()` generically in both renderers. It was a
hover-flavoured name on a generic action; there is no second variant to add.

Both renderers match `Effect` exhaustively, so a missed arm is a compile error,
not a silent parity divergence.

### 4.4 Async openers

A mode's background task cannot hold `&mut Editor`, and does not need to. The
sanctioned seam already exists and this design adds nothing to it:

`drain_tick_callbacks` is documented as "the single generic host primitive that
replaces per-subsystem `drain_<x>` methods: a mode owns its channel + drain body
and registers the closure". It runs inside `run_tick_pending`, which the editor
actor invokes from its `async_landed` arm — i.e. whenever an async task wakes it,
with no keystroke. The ACP conversation drain already relies on this to repaint
streamed output.

So: the drain signals its tick callback, the tick callback returns
`Effect::OpenPopup { .. }`, and the host applies it. No new inbound bus, no new
oneshot bridge.

### 4.5 Plugin-facing (deferred)

`wit/ui.wit` grows `open-popup` / `dismiss-popup`, gated on a `ui:popup`
capability in `CapabilityGrant`, following the `try_push_layer` template:

```rust
fn try_open_popup(&mut self, cap: UiCapability, ..) -> Result<(), CapabilityDenied>;
```

Data-only, so paramount #1 holds: the guest names a buffer and a placement; the
host draws.

This does **not** land with the native slices. A plugin popup that returns a
value needs the host→plugin async result ABI (PH7.5), which is unbuilt. The
native mode path is unblocked because a mode's popup returns nothing — §3.
The layering is the point: when PH7.5 lands, the WIT interface wraps a primitive
that is already generic, rather than a Help-shaped one that has to be rewritten.

## 5. Defects the generalisation fixed

These were pre-existing bugs, surfaced by asking a Help-only surface to be
generic. Each is fixed in the primitive, not worked around by the caller.

**State-B detection conflated with content identity.** The old pattern
`active_buffer == BufferKind::Help` served double duty: detecting that the popup
overlay has keyboard focus (State B) AND that the buffer kind is help. The
`popup_focused` bool (2026-07-22) decouples them. State-B detection now reads
`editor.popup_focused` / `ad().popup_focused`; content-identity checks
(`pane_tree.active().buffer == BufferKind::Help`) remain unchanged.

**Modal state is neither set on open nor restored on dismiss.** (PU-A, fixed)
`PrevPaneState` captures `{ buffer, buffer_id, cursor, scroll }`; `open_popup`
flips `active_buffer` without touching `Editor::modal`, and `dismiss_popup`
restores the four fields and leaves `modal` wherever the popup left it.

Nothing hits this today because every popup is opened by an explicit user command
from Normal, so `modal` happens to already be correct. The ACP permission menu
auto-opens while the user may be mid-prompt **in Insert**, and hits both halves
at once: the menu's `1`/`2`/`3` bindings never fire, because Insert takes the
keystroke first and tries to self-insert into a read-only buffer; and on dismiss
the user is returned to the prompt in whatever mode the popup left behind.

`PopupFocus::Steal` now sets `ModalState::Normal` as part of moving focus —
the popup buffer's mode is a Normal-mode surface, like every other buffer you
land on — and `PrevPaneState` gained `modal: ModalState`, which `dismiss_popup`
restores. A user typing at the ACP prompt is put back in Insert, at their
caret, with their partial prompt intact.

`PopupFocus::Passive` touches neither, by definition.

**State-A dismissal restores the wrong buffer kind.** (PU-A, fixed) When focus
never moved into the popup, `dismiss_popup` set `active_buffer =
BufferKind::Document` unconditionally. A floating popup opened over oil, the file
tree, or the dashboard restored focus to `Document`. Now snaps the actual prior
`active_buffer`.

**Chrome is Help-flavoured in both renderers.** The title comes from
`help.title` and the hint is a hardcoded `"Esc to dismiss"` string
(`render.rs:1878` and the GPUI peer). Title becomes the buffer's synthetic name.
The hint stays `"Esc to dismiss"` — it is true of any dismissible popup, and
YAGNI says do not parameterise it until a popup wants a different one.

**`prev_pane_for_help`** is renamed `prev_pane_for_popup`. (PU-A)

The next three were found while scoping PU-A and fixed ahead of it (slice plan
`../operations/slice-plans/archive/popup-input-caret.md`). Each is the same half-migration
seen from a different axis: the popup is "active" for `active_buffer` /
`active_buffer_id()` / cursor / scroll, but some slice of state still reads
`document_buffer_id` (the buffer behind the overlay).

**Caret walked the background document's matrix.** (PIC.1, fixed) The popup body
and cursorline compose from the `PaneId::POPUP` matrix, but the TUI caret summed
wrap-segment / virtual-row heights from the top-level `display_matrix` (=
`document_buffer_id`, which `open_popup` never repoints) — so a wrapping line
above the cursor drifted the caret below the cursorline, the error compounding per
line. Fixed by threading `PaneId::POPUP` into `cursor_screen_position_at` /
`buffer_line_to_visible_row_with`. GPUI was already correct.

**Fold ops mutate the background buffer's folds.** (PIC.2, contained) `self.folds`
is a hot-slot keyed to `document_buffer_id`, never swapped when a popup takes
focus, and the org-cycle ops (`z<Space>` / `z<Tab>`) were absent from the
read-only guard's mutation set — so they reached their handlers and mutated the
*background* document's folds at the popup's cursor line. Contained by adding them
to `action_is_document_mutation` (the guard now consumes every fold op in a
read-only popup). The deeper fix — swapping `self.folds` to the focused buffer so
a *writable* popup could fold — belongs with the generalisation.

**Global chords escape a focused popup.** (PIC.2, fixed) `gt` / `<C-w>…` are
universal `Builtin` chords with no popup gate, so they switched tabs / reshaped
panes behind the overlay (and dragged the single-slot `popup_buffer` onto the new
tab). The keymap trie has no consume-all and `HelpMode` binds nothing, so
exclusive capture is a **dispatch-seam** concern, not a mode keymap layer:
`action_escapes_focused_popup` + a guard in `handle_action` consume tab/pane nav
while a popup is focused (alongside the read-only guard). The generalisation
inherits the gate there.

**`active_cursor()` returned stale cursor in State B.** (fixed 2026-07-22) The old
code routed through `popup_help()?.cursor` which returned `self.popup_cursor` —
the initial open-time stash, not the live cursor after motions. Now returns
`self.cursor` directly (in State B, `self.cursor` IS the live popup cursor;
`open_popup_buffer(Steal)` copies the initial position, then motions update it).

**`active_text()` gated on `popup_help()` only.** (fixed 2026-07-22) The old code
called `popup_help()` which returned `None` for any non-Help buffer, falling
through to the match which returned the *background* document's content. Now
`popup_buffer_content()` uses `document_handle(id)` → `.snapshot().buffer`,
working for any document-shaped popup buffer (Help, Document, Messages,
Multibuffer, Dashboard).

## 6. Alternatives rejected

**Picker source (`PickerSource` + `RoutingPayload::ResolvePermission`).** The
cheapest in new primitives, and there is precedent (`ResolveDiff`, `OpenAiLog`,
and the LSP `showMessageRequest` flow, which is shape-identical to an ACP
permission request). Rejected on the standing mode-ownership rule: it requires
feature-specific variants in three host-side closed enums, leaving the host
wiring the action while the mode owns the body. Precedent is data, not
justification.

**Transient keymap layer over the inline block.** `push_layer` /
`pop_layer(PushLayerKind::MinorMode(..))` at runtime, exactly as the completion
popup does. Smallest host surface — one generic service, no `Effect`, no WIT
change, no new render path — and it works in Insert, so it can capture keys
mid-typing. Rejected on UX: the options render inline in the transcript with no
visible affordance that keys are now captured. It remains the right fallback if
the popup slice proves too large, and the two share nothing, so neither blocks
the other.

**A `HelpContent`-shaped special case for ACP.** Rejected on heuristic #1: it is
the easy implementation, not the better long-term design, and it entrenches the
`BufferKind::Help` branch that this document exists to remove.

## 7. Testing

Host, in `lattice-host`:

- `open_popup_buffer` with an arbitrary registered buffer id shows it; the
  buffer's own major mode is active and receives keys (State B)
- `PopupFocus::Passive` leaves `active_buffer` and the caret untouched (State A)
- dismiss restores buffer, cursor, scroll **and modal state** (the §5 bug)
- dismiss from State A over a non-Document buffer restores that buffer, not
  `BufferKind::Document` (the §5 bug)
- dismiss is idempotent; a popup over a deleted buffer paints nothing
- `Effect::OpenPopup` / `Effect::DismissPopup` round-trip the WIT boundary

Help regression, in `lattice-help` / `lattice-ui-tui`:

- `:help` still opens centred, `q` and `<Esc>` still close it
- the in-help back-stack (`<C-o>`) still walks, now owned by help
- hover / signature help still float without stealing focus

Renderer parity:

- the popup interior composes identically in both peers for a non-help buffer
