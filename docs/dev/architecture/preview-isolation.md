# Preview isolation (PI)

**Status:** 📝 design only — not started (2026-07-03). Captures the contract and
data model for making in-pane picker preview *isolated*: previewing buffer B in a
pane must never mutate the pane's committed buffer A, the global active-buffer hot
state, or A's resolved options. Slice plan:
`docs/dev/operations/slice-plans/preview-isolation.md` (PI series).

Distinct from `picker-preview-highlight.md`, which colors source code *inside picker
rows*. This doc is about the **in-pane live preview** — the full-size buffer the
picker shows in the source pane as the selection moves (`:picker files` hovering a
candidate, grep hits, `gr`, buffer switcher, etc.).

## 1. The problem

Preview is not isolated today: it makes the previewed buffer the **real active
buffer**. `preview_picker_selection` (`lattice-host/src/dispatch.rs:17145`) calls
`activate_buffer(previewed_id)` under a `self.previewing = true` flag, which routes
through `activate_document` → `activate_buffer_state`
(`dispatch.rs:13917`). That path, for the previewed buffer:

- swaps the global `document_buffer_id` **and** the active pane's committed
  `PaneState.buffer_id` to the previewed buffer (`dispatch.rs:26883-26889`),
- resets `self.cursor` / `self.scroll` (`dispatch.rs:26893-26894`),
- **activates the previewed buffer's major mode** via
  `activate_major_for_buffer_kind` — so previewing a `.rs` file runs `rust-mode`
  and applies its `Mode::options()` overrides,
- **rebuilds the single global `option_cache`** from the previewed buffer's
  resolved options (`recompute_options_for_buffer` + `rebuild_option_cache`,
  `dispatch.rs:13936-13939`).

The origin buffer (A) is reconstructed only by a *symmetric re-activation* on
restore (`preview_origin` / `pending_picker_preview_origin`, the `restore_preview_origin`
closure at `dispatch.rs:20953`). Preview is therefore a swap-and-restore, and any
asymmetry leaks A's rendered state.

### Two concrete leaks (the reported bug)

1. **Centering collapses.** The renderer special-cases the dashboard's
   `content_left_pad` to the active buffer only —
   `if buffer_id == app.ad().document_buffer_id { … } else { 0 }`
   (`lattice-ui-tui/src/render.rs:219`). The instant `document_buffer_id` points at
   the previewed file while the pane still shows the dashboard, centering drops to
   `0` → everything left-aligns.
2. **Mode options revert to defaults.** `Number` / `ReadOnly` / `CursorLine` for
   the origin resolve from its `resolved_options` entry + mode stack, which the
   swap-and-restore must reconstruct intact. When it doesn't, the dashboard renders
   with defaults (`Number = true`, gutter on). `:dashboard` fully recomposes and
   re-activates `dashboard-mode`, which is why it "fixes" it.

The general failure: **rust-mode's overrides get activated against shared global
state during preview; the markdown origin underneath is swapped out and only
approximately restored.**

## 2. The isolation contract

Previewing buffer B in a pane whose committed buffer is A MUST NOT:

- change `document_buffer_id`, `self.cursor`, `self.scroll`, `self.syntax`, or the
  global `option_cache`;
- change A's `PaneState.buffer_id` (the pane's *committed* buffer);
- run B's `Mode::on_activate` / `activate_major_for_buffer_kind` against, or write
  B's mode contributions into, A's `active_modes` / `resolved_options`;
- require any state to be snapshotted from A and restored to A. **Exiting preview is
  clearing an `Option`, not reconstructing a buffer.**

B's own overrides (rust-mode's `Number`, tabstop, …) live on **B's** `resolved_options`
entry and drive **B's** render only. A is never touched, so nothing bleeds.

## 3. Paramount-goal alignment

| Goal | This feature |
|---|---|
| #1 perf | **Improved.** Preview stops running mode-activation + a full global `option_cache` rebuild on every selection move; it points the pane's render at another buffer's already-resolved options. Exit is O(1) (clear an `Option`), not a re-activation. |
| #2 extensibility / everything-is-a-buffer | Preview becomes a **read-only projection** of a registry buffer through the *existing* per-buffer render path (`RenderView::for_buffer`, `PaneCellsInputs`). No parallel rendering path, no kind-branch. |
| #3 grammar | Neutral. Preview is not an editing surface; B is read-only while previewed. |
| #4 async | The cells/display workers already build a `DisplayMatrix` per pane keyed on `PaneCellsInputs.buffer_id` off the UI thread; preview reuses that, no new UI-thread work. |

**UX (higher court):** the user still sees B full-size in the source pane exactly as
today — this changes only *how* it is rendered (isolated projection vs. hijack), not
what appears. No layout change, no flicker; strictly removes the origin-glitch.

## 4. The key asset: per-buffer rendering already exists

Inactive panes already render fully isolated, per-buffer:

- `RenderView::for_buffer` (`render.rs:203`) resolves `Number`, `RelativeNumber`,
  `Wrap`, `SignColumn`, folds, and every `lsp-*-mode` gate **per `buffer_id`** from
  that buffer's own resolved options — never from the global `option_cache`.
- `PaneCellsInputs` (`render_state.rs:1193`) already carries a per-pane
  `buffer_id` (the worker's registry key), `snapshot`, `syntax_handle`,
  `inlay_hints`, `folds`, and its own `matrix` / `display_matrix` /
  `virtual_rows_matrix` output cells. The cells worker builds each pane's
  `DisplayMatrix` from *its* buffer_id, not the active document's.

So the machinery to render a buffer in a pane **without that buffer being globally
active** is already present and battle-tested for splits. Preview should ride it.

The one gap is the `content_left_pad` special-case (`render.rs:219`), which is a
latent bug independent of preview: centering should follow the *buffer that carries
`CenterContentWidth`*, not the active-buffer identity. It must resolve per-buffer
(from B's — or A's — `CenterContentWidth` local + that pane's `viewport_width`).

## 5. Data model

A pane gains an ephemeral preview override — the *displayed* buffer, decoupled from
the *committed* buffer.

```rust
// On PaneState (lattice-core::ui::pane), or a host-side sidecar map keyed by
// PaneId if PaneState must stay Copy/geometry-only — decided in PI.1.
/// Ephemeral: the buffer this pane is *previewing*. The pane's committed
/// `buffer_id` (what a real switch/accept commits, what `:ls` and the modeline
/// report) is unchanged. `None` = not previewing. Cleared on accept / dismiss /
/// selection-cleared; never persisted, never snapshotted.
preview_buffer_id: Option<BufferId>,
```

Render/worker read rule: a pane's **displayed** buffer is
`preview_buffer_id.unwrap_or(buffer_id)`; its **committed** buffer stays `buffer_id`.
`PaneCellsInputs.buffer_id` (the worker key) and `RenderView::for_buffer` use the
displayed buffer; the modeline `core.path` / `core.position`, `:ls`, and dispatch use
the committed buffer.

## 6. Option resolution for the preview buffer

B is a real registry buffer. When preview first mounts B into a pane, the host
computes **B's** `resolved_options` once — `recompute_options_for_buffer(B)` — against
a read-only mode stack, WITHOUT touching `document_buffer_id` or the global
`option_cache`. B's entry then feeds `for_buffer(B)` exactly like an inactive split.

**Read-only / ephemeral marking.** Preview needs B to render read-only (no cursorline
editing affordances) and to be flagged as a preview. Two candidate shapes (decided in
PI.2):

- **(a) `preview-mode` minor** — a real minor on B's own mode stack carrying
  `ReadOnly = true` + the ephemeral marker; introspectable via `:describe-mode`;
  matches the "dedicated preview-mode" mental model. Cost: a mode whose lifecycle is
  driven by preview enter/exit rather than the user.
- **(b) read-only render flag on the override** — no new mode; the override carries a
  `read_only` bit and B renders through its *existing* mode options. Lighter; less
  explicit that "this is a preview."

Either way the invariant holds: B's overrides never write into A.

## 7. Lifecycle

- **Enter / move selection:** set the active pane's `preview_buffer_id = Some(B)`;
  ensure B is in the registry with computed `resolved_options`; trigger a cells/
  display rebuild for the pane (worker keys on the displayed buffer). No
  `activate_buffer`, no global-state mutation. Replacing B with B' just reassigns the
  `Option`.
- **Dismiss (`<Esc>` / no match / picker closed):** `preview_buffer_id = None`. The
  pane snaps back to A with zero reconstruction — A was never disturbed. The
  `preview_origin` / `pending_picker_preview_origin` / `restore_preview_origin` /
  `self.previewing` machinery is **deleted**.
- **Accept:** commit — a real `activate_buffer(B)` / split / tab (the existing accept
  paths), then clear the override. This is the one place B legitimately becomes
  active.

## 8. Cross-renderer parity

TUI and GPUI move in lockstep (per the cross-renderer rule). Both already read
per-pane matrices and per-buffer options:

- TUI: `RenderView::for_buffer` + the `content_left_pad` per-buffer fix (§4).
- GPUI: the mirror pane-render path in `window.rs` (per-pane cursor/scroll/options
  reads at `window.rs:1604`, `2439`, `2530`) resolves the displayed buffer the same
  way. End-of-slice audit: `grep -rn "preview_buffer_id" crates/lattice-ui-gpui/`
  must be non-empty once PI.4 lands.

## 9. Rejected alternatives

- **(B) Keep in-place activation + lossless snapshot/restore of the origin.** Still
  mutates global state and depends on restore symmetry — the exact fragility this
  removes. Violates heuristic #1 (symptom-patch kept out of risk-aversion) and the §2
  contract ("exit is clearing an Option, not reconstructing a buffer"). Rejected.
- **(C) Narrow render fix only** (`content_left_pad` per-buffer + never leave
  `document_buffer_id` dangling). Fixes the *visible dashboard glitch* cheaply but
  does NOT deliver isolation: rust-mode still activates against global state during
  preview and A is still swapped out. Does not meet the stated requirement. Kept only
  as PI.0 (the render fix is a real bug and a prerequisite either way); not the
  endpoint.

## 10. Open questions (resolve during slicing)

1. **§5 storage:** `preview_buffer_id` on `PaneState` (currently `Copy`,
   geometry-only) vs. a host-side `HashMap<PaneId, BufferId>` sidecar. Leaning
   sidecar to keep `PaneState` `Copy` and free of buffer lifecycle.
2. **§6 marking:** `preview-mode` minor (a) vs. read-only render flag (b).
3. **Preview buffer creation:** file/grep previews resolve a routing payload to a
   buffer id today. Confirm every preview source yields (or can cheaply materialize) a
   registry buffer whose `resolved_options` can be computed read-only without I/O on
   the UI thread (giant/binary-file guard already exists in the preview classifier,
   `dispatch.rs:17278+`).
4. **Non-Document preview kinds** (help/oil/tree candidates): whether they preview at
   all, and if so whether the override extends beyond Document panes.

## 11. Deliverables (heuristic #5)

Ships as four artefacts: this design fragment, the slice plan, test coverage (the
isolation acid test — preview a rust buffer over a markdown buffer, assert the
markdown buffer's `resolved_options` + `active_modes` are byte-identical before and
after; the dashboard-glitch regression), a per-frame preview bench delta (preview
should be *cheaper* than today), and graceful handling (missing/again-closed preview
buffer → clear the override, never panic).
