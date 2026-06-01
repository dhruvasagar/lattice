# Slice plan: K.4 — Multibuffer is a regular buffer

**Design:** [multibuffer-is-a-regular-buffer.md](../../architecture/multibuffer-is-a-regular-buffer.md).

**Status:** 🚧 in progress (K.4.0 audit doc landed, K.4.1+
pending user sign-off).

**Why:** M.2.b series shipped declaring Multibuffer
integration but no test exercised end-to-end behavior. Four
latent failures surfaced during M.6 testing (silent
EventBus, current_thread freeze, `contains_document` gap,
vim grammar broken on multibuffer views). The K.4 slice
closes the integration verification gap so future kind
additions (Diagnostics, LSPReferences, AIProposedEdits
providers per `multibuffer-views.md` §A) inherit the bar.

## Sequencing

### K.4.0 — Audit doc ✅

Landed. See
[multibuffer-is-a-regular-buffer.md](../../architecture/multibuffer-is-a-regular-buffer.md).
35 seam sites classified Aligned / ❌ Bug / ⚠ Unclear.
Renderer `lattice-ui-tui` has zero `BufferKind::Multibuffer`
mentions → silent integration failures.

### K.4.1 — Integration test

`crates/lattice-host/tests/multibuffer_is_a_regular_buffer.rs`
(new). Drives a real `Editor` (built via `Editor::boot`)
through the standard dispatch pipeline, exercising
Multibuffer specifically.

Required test coverage (each becomes its own `#[test]`,
so failures pinpoint a specific seam):

- **`motion_j_advances_cursor`** — open multibuffer, send
  `j` chord, assert `self.cursor.line` advances.
- **`motion_k_retreats_cursor`** — same with `k`.
- **`motion_gg_jumps_to_top`** — `gg` lands cursor at line 0.
- **`motion_G_jumps_to_bottom`** — `G` lands cursor at last line.
- **`motion_w_advances_word`** — `w` advances within an excerpt.
- **`motion_excerpt_next_advances_to_next_excerpt`** — `]e`
  advances cursor to the next excerpt's start row in the
  composed view.
- **`visual_mode_enter_works`** — `v` enters Visual.
- **`insert_mode_blocked_when_readonly`** — `i` on a
  `ReadOnly = true` minor produces the "buffer is read-only"
  echo (proves the read-only gate fires uniformly).
- **`cells_matrix_populated_for_view`** — after activation,
  `editor.cells_matrix_for(view_id)` returns a non-empty
  matrix.
- **`virtual_row_matrix_carries_excerpt_headers`** — for a
  view with N excerpts, the virtual-row matrix contains N
  header rows whose text matches the excerpt header
  payloads.

The K.4.0 audit predicts which of these pass / fail today;
K.4.1 is the test, not the fix.

**Sub-slices:**

- **K.4.1.a** Test harness — minimal `Editor::boot` setup;
  helper to create a multibuffer view with synthetic
  excerpts; helper to drive a chord through `run_invocation`
  + read back `cursor` / `active_buffer` / matrices.
- **K.4.1.b** Motion tests (`j`, `k`, `gg`, `G`, `w`,
  `]e`, `[e`).
- **K.4.1.c** Visual + insert mode tests.
- **K.4.1.d** Render-state tests (cells / virtual-row
  matrix population, excerpt headers in matrix).
- **K.4.1.e** CI gate — test runs in default `cargo test`,
  not gated behind `--features search` (uses
  `create_multibuffer_view` directly, not the
  search provider's async path).

### K.4.2 — Cells / virtual-row worker audit + fix

Investigate whether the cells worker
(`crates/lattice-host/src/cells_worker.rs`) and virtual-row
worker populate matrices for Multibuffer kind. Search for
`BufferKind` matches in the worker; categorize as
(a) Document-or-anything-else or (b) Document-only.

For (b) sites: extend or remove the kind gate so the
worker treats Multibuffer like Document. Tests from K.4.1.d
should flip from failing to passing.

### K.4.3 — Render syntax-cell gate (renderer)

Fix `crates/lattice-ui-tui/src/render.rs:2708` —
`active_doc_id` should be `Some(id)` for `Document |
Messages | Multibuffer`, not Document-only. Without this
the multibuffer's tree-sitter highlights never publish
into the renderer's cells matrix and the rendered text is
unstyled.

(GPUI peer parity: same change in `lattice-ui-gpui` if its
render path has the equivalent gate. Audit when fixing.)

### K.4.4 — `:ls` listing format

`dispatch.rs:21056` folds Multibuffer into Messages's
listing row with `msg` label. Give Multibuffer its own row
with `mb` label, distinct from Messages. Pure UX fix; no
behavior change.

### K.4.5 — Audit comment pass

For every renderer code path that's aligned-by-fallback
(pattern (a) per audit doc §2.6), add a comment
explicitly listing the kinds that hit the branch. Example:

```rust
match buffer_kind {
    BufferKind::Document => ...,
    // Messages / Multibuffer fall through here — same pane-
    // state semantics as Document; no per-kind override.
    _ => fallback,
}
```

The next code-reader / future kind author sees "Multibuffer
fall through here" instead of having to verify by running
the integration test. Prevents the renderer-side absence
trap that K.4 closes.

### K.4.6 — Convention codification

Update `feedback_buffers_no_special_case` memory with a
"this is what 'no special case' looks like in practice"
section per audit doc §5. Include:

- Pointer to the K.4.1 integration test.
- Rule: any new BufferKind must either pass the test
  verbatim or document each diverging chord.
- Rule: renderer paths gating on `BufferKind` must enumerate
  the kinds that hit each branch in a comment.

## Risk + roll-back

- **Risk:** K.4.1 test setup is heavy (full `Editor::boot`
  needs a tokio runtime, mode registry, services, ...).
  Mitigation: build a `test_support` helper inside
  `lattice-host` (or its tests/) so K.4.2+ test additions
  are cheap.
- **Risk:** K.4.2 / K.4.3 fixes might cascade into other
  renderer paths not covered by the audit. Mitigation: the
  integration test is the gate — fix a seam, run the test,
  iterate. No fix lands without a test assertion that
  proves it.
- **Roll-back:** each K.4.N is independently revertible
  (one test addition, one renderer fix, one labelling
  change). Audit doc has no rollback (pure docs).

## Cross-references

- Design: [multibuffer-is-a-regular-buffer.md](../../architecture/multibuffer-is-a-regular-buffer.md).
- Triggered by: M.6.X retro
  ([multibuffer-views.md M.6.X row](./multibuffer-views.md)) +
  user reports during M.6 testing
  (vim grammar broken, no file labels visible).
- Convention this slice canonicalises: `feedback_buffers_no_special_case`.
- Companion of: `kind-agnostic-buffers.md` (H-series) for
  generic kind infrastructure; K.4 is the specific
  verification of Multibuffer-as-regular.
