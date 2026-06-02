# Slice plan: K.4 — Multibuffer is a regular buffer

**Design:** [multibuffer-is-a-regular-buffer.md](../../architecture/multibuffer-is-a-regular-buffer.md).

**Status:** 🚧 in progress. Audit + round-1 fixes landed
(commit `8bc77e4` — K.4.0 / K.4.2 / K.4.3 / K.4.4). New
sub-slices added from user-testing findings between K.4.0
and 2026-06-01 wrap-up.

**Why:** M.2.b shipped declaring Multibuffer integration but
no test exercised end-to-end behavior. Four latent failures
surfaced during M.6 testing (silent EventBus, current_thread
freeze, `contains_document` gap, vim grammar broken on
multibuffer views). The K.4 slice closes the integration
verification gap so future kind additions (Diagnostics,
LSPReferences, AIProposedEdits providers per
`multibuffer-views.md` §A) inherit the bar.

## Sequencing

### K.4.0 — Audit doc ✅ (commit `8bc77e4`)

Landed. See
[multibuffer-is-a-regular-buffer.md](../../architecture/multibuffer-is-a-regular-buffer.md).
35 seam sites classified Aligned / ❌ Bug / ⚠ Unclear.
Renderer `lattice-ui-tui` has zero `BufferKind::Multibuffer`
mentions → silent integration failures (the design
fragment §3 calls this out as the danger pattern).

### K.4.1 — Integration test scaffold 🗒

`crates/lattice-host/tests/multibuffer_is_a_regular_buffer.rs`
(new). Drives a real `Editor` (built via `Editor::boot`)
through the standard dispatch pipeline, exercising
Multibuffer specifically.

Required test coverage (each becomes its own `#[test]`,
so failures pinpoint a specific seam):

- `motion_j_advances_cursor` — open multibuffer, send
  `j` chord, assert `self.cursor.line` advances.
- `motion_k_retreats_cursor` — same with `k`.
- `motion_gg_jumps_to_top` — `gg` lands cursor at line 0.
- `motion_G_jumps_to_bottom` — `G` lands cursor at last
  line.
- `motion_w_advances_word` — `w` advances within an
  excerpt.
- `motion_excerpt_next_advances_to_next_excerpt` — `]e`
  advances cursor to the next excerpt's start row in the
  composed view.
- `visual_mode_enter_works` — `v` enters Visual.
- `visual_selection_renders` — selection cells show the
  visual-highlight attribute (K.4.5 dependency).
- `insert_mode_blocked_when_readonly` — `i` on a
  `ReadOnly = true` minor produces the "buffer is
  read-only" echo.
- `cells_matrix_populated_for_view` — after activation,
  `editor.cells_matrix_for(view_id)` returns a non-empty
  matrix.
- `virtual_row_matrix_carries_excerpt_headers` — for a
  view with N excerpts, the virtual-row matrix contains N
  header rows whose text matches the excerpt header
  payloads (K.4.6 dependency).
- `syntax_highlights_per_excerpt_use_source_language` —
  K.4.7 dependency; assert that excerpt rows from a
  `.rs` source carry rust-mode highlight spans, and rows
  from a `.md` source carry markdown spans.

The K.4.0 audit predicts which of these pass / fail today;
K.4.1 is the test, not the fix.

**Sub-slices:**

- **K.4.1.a** Test harness — minimal `Editor::boot` setup;
  helper to create a multibuffer view with synthetic
  excerpts; helper to drive a chord through
  `run_invocation` + read back `cursor` / `active_buffer` /
  matrices.
- **K.4.1.b** Motion tests (`j`, `k`, `gg`, `G`, `w`)
  + Visual mode (`v`) + partial-chord lifecycle ✅
  (commit pending). Backed by a new public
  `Editor::dispatch_chord(chord, &mut partial_chord) -> Action`
  API in `lattice-host` — programmatic chord dispatch
  that builds a `TranslateContext` from editor state,
  calls host `translate`, manages the partial-chord
  buffer (push on `AbsorbPartialChord`, exempt
  `PushDigit` / `EnsureCursorVisible`, clear otherwise),
  routes the action through `handle_action`, and drains
  `out.next_actions` to closure so deferred `AppEffect`s
  (notably `EnterVisual` via `action:enter-visual-*`)
  land synchronously. Same pipeline as the TUI's input
  layer, minus App-only surface state (picker, snippet,
  terminal modes default to "not active"). Public per
  the extensibility principle — plugins / `init.rs` /
  scripted automation get the same affordance the TUI
  uses. `]e` / `[e` excerpt-boundary motions deferred
  to K.4.6 (excerpt-header virtual-row pipeline is the
  prerequisite for `MultibufferDocumentHandle` to know
  where excerpt boundaries are in matrix-row terms).
- **K.4.1.c** Visual + insert mode tests — Visual now
  covered by K.4.1.b; Insert mode tests deferred to a
  follow-up if/when an Insert-on-multibuffer policy
  lands (today excerpts are read-only).
- **K.4.1.d** Render-state tests (cells / virtual-row
  matrix population, excerpt headers in matrix, per-
  excerpt syntax spans).
- **K.4.1.e** CI gate — test runs in default `cargo test`,
  not gated behind `--features search` (uses
  `create_multibuffer_view` directly, not the
  search provider's async path).

**Risk:** `Editor::boot` setup is heavy (tokio runtime,
mode registry, services, …). Mitigation: build a
`test_support` helper inside `lattice-host` so subsequent
test additions are cheap.

### K.4.2 — `build_cells_panes` kind gate ✅ (commit `8bc77e4`)

Extended both matchers in `dispatch.rs:8534` from
`Document` only to `Document | Messages | Multibuffer`:

- `active_doc_active` matcher at line 8541 — gates
  whether the focused buffer is treated as the active
  doc.
- Per-leaf filter at line 8566 — pre-K.4.2 every
  non-Document leaf was *continued past*, so multibuffer
  panes never got a `PaneCellsInputs` entry and the cells
  worker had nothing to recompute.

Single highest-impact open question from the audit (§2.7)
answered: cells worker is kind-agnostic (zero `BufferKind`
mentions in 3,161 LOC); the upstream that *builds* the
worker's inputs was the gate.

### K.4.3 — Renderer syntax-cell gate ✅ (commit `8bc77e4`)

`render.rs:2708` — `active_doc_id` resolution matcher
extended same as K.4.2. Pre-K.4.3 multibuffer panes hit
the empty-highlights fallback and rendered unstyled.

GPUI peer audited: shares the cells matrices from host;
no per-peer fix required (parity maintained per
`feedback_tui_gpui_parity`).

### K.4.4 — `dispatch_blocking` host-side grammar for Multibuffer ✅ (commit `8bc77e4`)

The actual root cause for *"vim grammar broken / cursor
doesn't move at all"*:
`MultibufferDocumentHandle::dispatch_with_cancel`
(`crates/lattice-multibuffer/src/lib.rs:942`) returns
`RuntimeError::ReadOnly` with a design comment claiming
*"grammar dispatch runs at the host layer against the
composed snapshot."* That host-layer work was never
wired. Pre-K.4.4 every motion / operator on a multibuffer
view bounced with ReadOnly and `self.cursor` never
updated.

Now: when `active_buffer == Multibuffer`,
`dispatch_blocking` runs `lattice_grammar::execute`
against a scratch `lattice_core::Document` built from
the composed snapshot. Motions return cursor Effects
(flow through `apply_effect_host`); operators return
composed-coordinate Edits (flow through
`apply_edit_blocking` → multibuffer's `apply_edit` →
source-coordinate translation per M.3).

Contained kind branch — the only one in
`dispatch_blocking`. Architectural follow-up = K.4.11.

### K.4.5 — Visual-mode highlight rendering ✅ (commit pending)

**Root cause was NOT a renderer kind-gate.** Investigation
ruled out the suspected matcher-extension shape. The real
issue lived at the multibuffer's `Document::set_selections`
impl: it returned `Pending::ready(Err(RuntimeError::ReadOnly))`,
which left the multibuffer snapshot's `selections` permanently
at `SelectionSet::default()`. `Editor::visual_selection_range`
reads `self.document.selections().primary()` uniformly across
BufferKinds (paramount-#3 — no kind-special-casing); when the
multibuffer's primary selection stayed pinned at `(0,0)`,
visual-mode painting got a degenerate `(0,0)..(0,1)` range
and rendered as a single-cell highlight at the top-left.

**Fix:** make `MultibufferDocumentHandle::set_selections`
properly store the SelectionSet in `MultibufferState` and
republish the snapshot. `compose_snapshot` gained a
`selections: Arc<SelectionSet>` parameter; every recompose
path threads `state.selections.clone()` so excerpt
mutations preserve the user's selection across recomposes.

**Lesson:** the slice plan's "likely small fix, single matcher
extension" hypothesis was wrong, but the correct fix was
ultimately just as small (~30 LOC change) AND honoured the
paramount-#3 "no buffer-kind-special logic" principle in a way
a matcher extension would NOT have. Renderer-side matcher
extensions would have entrenched the BufferKind branch
([[feedback_buffers_no_special_case]]); fixing the Document
trait impl makes Multibuffer behave uniformly with Document.

**Tests:**
- `lattice-multibuffer::tests::set_selections_stores_composed_selections_post_k_4_5`
  — unit test: set_selections → snapshot reflects → recompose
  preserves.
- `lattice-host::tests::multibuffer_is_a_regular_buffer::visual_selection_renders_for_multibuffer`
  — integration test: Visual + `lll j` on multibuffer view →
  `visual_selection_range` returns a non-degenerate range.
- Updated `save_and_set_selections_still_rejected_post_m3` →
  `save_still_rejected_post_m3` (save remains ReadOnly;
  set_selections drops out of the rejected-paths list).

### K.4.6 — Excerpt-header virtual-row pipeline 🗒 architectural

**Bigger than a single matcher extension.** Data IS
attached (`ExcerptHeader::new(format!("{}", path.display()))`
per `crates/lattice-multibuffer/src/providers/search.rs:449`);
the pipeline that publishes virtual rows for a multibuffer
view is missing two pieces, and there's a third
architectural question to resolve:

**(a) `MultibufferHeaderProvider` impl of `VirtualRowProvider`.**
Lives in `lattice-multibuffer`. Walks the multibuffer's
`state.excerpts`, emits one `VirtualRow` per excerpt at
`AnchorPosition::Above` of the excerpt's first composed
row, content = the excerpt's `ExcerptHeader.label`
(file path).

- `id()` — stable `ProviderId` for the multibuffer's
  header lane.
- `version()` — bumps when excerpts change; reuse the
  composed snapshot's `version_id` (already monotonic on
  any state change).
- `collect() -> Vec<VirtualRow>` — locks state briefly,
  walks excerpts in display order.

**(b) Registration seam.** `create_multibuffer_view`
needs to register the provider against the new
`BufferId` on `editor.virtual_row_providers`. Two
options:

- **(b.i)** Extend `ModeActivator` trait with a
  `register_virtual_row_provider(buffer, provider)`
  method. `lattice-host`'s `Editor` impl delegates.
  Clean but touches a public trait.
- **(b.ii)** Host-side hook in `do_search` — after
  `project_search` returns the `view_id`, host fetches
  the `MultibufferDocumentHandle`, constructs the
  header provider, registers it. Less invasive but adds
  a per-provider host-side wiring step.

Pick (b.i) for the universal `VirtualRowProvider` access
pattern — future providers (fold ranges per M.7, diff
hunks per `multibuffer-views.md` §A.1, …) need the same
seam.

**(c) Renderer active-cell vs per-pane matrix
resolution.** `Editor::virtual_rows_matrix_cell` is the
single active cell read by the renderer at
`dispatch.rs:1137` and by `lattice-ui-tui/src/render.rs`
at 2762 etc. The cell is initialised to
`document_buffer_id`'s matrix at boot (`editor_boot.rs:761,
783`) and never repointed on `activate_document` for any
non-original-buffer kind. There IS a per-pane registry
(`virtual_rows_matrices: HashMap<BufferId, Arc<ArcSwap<…>>>`)
+ a published `virtual_rows_pane_matrices` for the
renderer to resolve non-active panes — but the active
pane reads the single cell, which points at the wrong
buffer for a multibuffer view.

Two fix paths:

- **(c.i) Repoint on activation** — `activate_document`
  swaps `virtual_rows_matrix_cell`'s contents to the
  newly-active buffer's matrix. Simple but requires the
  cell to be `Arc<ArcSwap<Arc<VirtualRowMatrix>>>` (a
  pointer-to-pointer) instead of the current
  `Arc<ArcSwap<VirtualRowMatrix>>`. Touches every
  reader.
- **(c.ii) Renderer prefers active-pane matrix from
  `pane_matrices`** — when the active pane's buffer id
  has its own entry in `pane_matrices`, use that;
  otherwise fall back to the cell. Lower-impact change,
  no signature breakage on the single-cell readers, but
  the cell becomes stale on multibuffer (a smell —
  audit-comment that it's a fallback).

Recommend **(c.ii)** — smaller patch, doesn't disrupt
the single-active-doc fast path. Audit-comment naming
the fallback.

**Risk:** this is the largest individual K.4 sub-slice.
Estimated ~300-500 LOC spread across
`lattice-multibuffer/src/header_provider.rs` (new),
`lattice-mode::ModeActivator` trait (b.i),
`lattice-host` impl, `lattice-ui-tui/src/render.rs`
(c.ii).

### K.4.7 — Per-excerpt syntax highlighting 🗒 design slice

**New finding (2026-06-01 user testing).** Excerpt body
text renders unstyled — the multibuffer view's filename
(`*search:spawn*`) detects as `Lang::Plain` so
tree-sitter returns nothing for the composed snapshot,
even after K.4.3 plumbed the syntax cell through.

This is a **design slice**, not a one-liner. Right shape:

1. **Composer-side language tracking.** Each
   `Excerpt` records its source `Lang` (resolved at
   excerpt-creation time from
   `Lang::detect_from_path(source.path)`).
2. **Per-excerpt highlight cache.** The composer (or
   the multibuffer's snapshot publisher) maintains a
   `Vec<StyledSpan>` per excerpt, each excerpt parsed
   against its own language.
3. **Composed-coordinate span shifting.** When the
   renderer asks for highlights for the multibuffer's
   composed snapshot, the multibuffer slices each
   excerpt's per-language spans and shifts them into
   composed-row coordinates.
4. **Renderer integration.** Today's renderer pulls
   highlights from the host's `pane_highlights` cell
   (a single buffer's spans). For multibuffer, pull
   from the multibuffer's per-excerpt aggregated spans
   instead.

**Architecture artefact required before code lands.**
Write `docs/dev/architecture/multibuffer-syntax.md`
fragment + companion slice plan. Decisions to lock:

- Where the per-excerpt parses live (composer vs.
  per-source-buffer reuse — source buffers already
  have their own syntax cells; can the multibuffer
  ride those instead of re-parsing?).
- Incremental update on source edits (M.3 already
  propagates edits to sources; the source's syntax
  cell updates; the multibuffer just needs to
  re-resolve when source changes).
- Performance budget — paramount goal #1 still applies.

Likely-cleanest path: **multibuffer rides per-source
syntax cells.** Each source buffer already maintains a
tree-sitter parse via the syntax worker. The
multibuffer's render path reads each source's current
span set and shifts to composed coordinates on the fly.
No re-parsing, no separate cache.

Slice this as **K.4.7.0 design fragment**, **K.4.7.1
composer language-tracking changes**, **K.4.7.2
renderer per-excerpt span resolution**, **K.4.7.3
benchmark coverage**, **K.4.7.4 test additions to
K.4.1**.

### K.4.8 — `:ls` listing format polish ✅ (commit `6299564`)

Landed. The actual fold was at `build_list_buffers_content`
(`dispatch.rs:21245` — the slice plan's earlier line ref
21056 was stale, it pointed at the option-cascade code),
combined arm `BufferKind::Messages | BufferKind::Multibuffer`
with `msg` label + `*messages*` default. Split into two
distinct arms; Multibuffer gets `mb` + `*multibuffer*` default
matching the picker-source rendering at
`picker_buffer_entry_for` (~23470).

Summary header gained a multibuffer count alongside document /
tree / help / message counts. `BufferRegistry::multibuffer_ids_sorted`
added to mirror the existing `messages_ids_sorted` pattern
(symmetric API surface across kinds).

### K.4.9 — Audit comment pass ✅ (commit `4c4631d`)

The one remaining `Document => ...; _ => fallback` pattern
in the renderer (`lattice-ui-tui::render`'s popup-anchor
cursor/scroll matcher around line 1867) now carries an
explicit Messages / Multibuffer / FileTree / Oil / Terminal /
Help enumeration on the fallback branch — readers see the
exhaustive list without having to verify by running the
integration test.

Other K.4.0 audit pattern-(a) sites already have explicit
enumerations from earlier slices:
- K.4.2 (commit `8bc77e4`): `build_cells_panes` matchers at
  `dispatch.rs:8534` + `:8566`.
- K.4.3 (commit `8bc77e4`): syntax-cell gate at
  `render.rs:2708`.

So this slice's scope reduced to the one previously-
undocumented fallback.

### K.4.10 — Convention codification ✅ (memory updated 2026-06-02)

`feedback_buffers_no_special_case` memory updated with a
"2026-06-02 — K.4.1.a / K.4.8 / K.4.9 / K.4.10 landed"
section recording concrete enforcement status:

- K.4.1.a foundation slice landed (commit `6a14732`) — the
  integration test path is real, not just plan'd.
- K.4.8 listing split landed with commit ref.
- K.4.9 audit comment pass landed with commit ref.
- K.4.10 (this update) codifies the above as the current
  enforcement state.

Also recorded a sibling pattern surfaced 2026-06-02: K.3.5
(commit `83df46d`) was the same "code shipped but the
integration path was never tested end-to-end" shape — K.3.2
bindings invoked required-arg commands without auditing the
empty-args case. Fix: public `Editor::arm_missing_arg_prompt`
API both cmdline-submit and keymap bindings call. Lesson
codified: "before wiring a keymap binding (or any user-facing
surface) to existing functionality, audit whether the
function-being-called has an explicit public API. If not, the
bug is 'the API doesn't exist yet' not 'the binding is
incomplete.'"

Reference: K.4.5 (audit-comment pass) in earlier memory text
corrected to K.4.9 (the actual slice number per this plan).

### K.4.11 — `dispatch_with_cancel` proper impl on `MultibufferDocumentHandle` 🗒 architectural follow-up

K.4.4 wired the multibuffer's grammar dispatch as a
host-side kind branch in `Editor::dispatch_blocking`.
The architecturally cleaner shape is to implement
`Document::dispatch_with_cancel` on
`MultibufferDocumentHandle` properly so the kind branch
in host code disappears. Requires:

- `CommandRegistry` threading through
  `create_multibuffer_view` (the multibuffer needs a
  registry to run grammar). Three options:
  - **(a)** Add `Arc<CommandRegistry>` parameter to
    `create_multibuffer_view`.
  - **(b)** Expose the registry via `ServiceRegistry`
    so any code that already pulls
    `activator.services()` can also pull the registry.
  - **(c)** Have `MultibufferDocumentHandle`
    construct itself with an `Arc<CommandRegistry>`
    field set later via a `with_registry` builder
    method.
- Replace the `Pending::ready(Err(RuntimeError::ReadOnly))`
  body of `dispatch_with_cancel` (`lib.rs:942-953`)
  with the host-side body now in
  `Editor::dispatch_blocking`.
- Delete the kind branch in `dispatch_blocking`.

Recommend (b) — service-registry route — for
consistency with how other Document handles (LSP,
ProjectSearchService) reach host wiring.

## Risk + roll-back

- **Cumulative risk:** K.4 expanded from a 7-slice audit
  to an 11-slice arc once user testing surfaced the
  K.4.5 / K.4.6 / K.4.7 gaps. K.4.6 and K.4.7 are the
  largest individual slices; K.4.6 is the next "must
  land" for user-readable search results.
- **Roll-back:** each landed sub-slice (K.4.0, K.4.2,
  K.4.3, K.4.4) is independently revertible. Pending
  sub-slices are additive.

## Cross-references

- Design: [multibuffer-is-a-regular-buffer.md](../../architecture/multibuffer-is-a-regular-buffer.md).
- Triggered by: M.6.X retro
  ([multibuffer-views.md M.6.X row](./multibuffer-views.md)) +
  user reports during M.6 testing
  (vim grammar broken, no file labels visible).
- Convention this slice canonicalises:
  `feedback_buffers_no_special_case`.
- Companion of: `kind-agnostic-buffers.md` (H-series) for
  generic kind infrastructure; K.4 is the specific
  verification of Multibuffer-as-regular.
