# Preview isolation — slice plan (PI)

Sequencing for the design in
`docs/dev/architecture/preview-isolation.md`. Goal: in-pane picker preview renders
buffer B as an **isolated read-only projection** in a pane, never mutating the
committed buffer A, the global active-buffer hot state, or A's resolved options /
mode stack.

**Status legend:** 📝 planned · 🚧 in progress · ✅ landed

All slices land green (build + touched suites) and keep TUI/GPUI in lockstep.

| Slice | Status | Summary |
|---|---|---|
| PI.0 | 📝 | Per-buffer `content_left_pad` + failing isolation/regression tests |
| PI.1 | 📝 | Pane preview-override plumbing (state + render_state + worker key) |
| PI.2 | 📝 | Preview buffer resolved-options isolation (read-only marking) |
| PI.3 | 📝 | Cut picker preview dispatch over to the override |
| PI.4 | 📝 | GPUI parity |
| PI.5 | 📝 | Delete the swap/restore preview machinery; docs + bench |

---

## PI.0 — Render fix + failing tests (prerequisite, self-standing)

Implements design §4 (the `content_left_pad` latent bug) and lands the acid tests
red so every later slice has a target.

- Fix `RenderView::for_buffer` (`lattice-ui-tui/src/render.rs:219`): resolve
  `content_left_pad` from the *rendered* buffer's `CenterContentWidth` local + that
  pane's `viewport_width`, not `== document_buffer_id`. Mirror in the GPUI centering
  read.
- **Isolation acid test** (`lattice-host`): open a markdown buffer (markdown-mode),
  preview a rust buffer (rust-mode overrides `Number`/tabstop), assert the markdown
  buffer's `resolved_options` **and** `active_modes` are byte-identical before/after,
  and `document_buffer_id` is unchanged. Red until PI.3.
- **Dashboard regression test:** dashboard active, drive a preview cycle, assert the
  dashboard buffer still resolves `Number = false` and its `content_left_pad` centers.
  Red until PI.3 (the `content_left_pad` half goes green here; the mode half at PI.3).

**Depends on:** none. **Unblocks:** all. Independently shippable (the render fix is a
real bug).

## PI.1 — Pane preview-override plumbing

Implements §5. No behavior change yet — pure capability + a manual/test-only setter.

- Add the preview override (open question §10.1): `HashMap<PaneId, BufferId>` host
  sidecar (preferred — keeps `PaneState` `Copy`) or a field on `PaneState`.
- Displayed-vs-committed rule: `build_cells_panes` sets each `PaneCellsInputs.buffer_id`
  to `preview_buffer_id.unwrap_or(committed)`; `snapshot` / `syntax_handle` / `folds`
  / matrices follow the displayed buffer.
- Renderer reads the displayed buffer for content + `for_buffer`; modeline / `:ls` /
  dispatch keep reading the committed buffer.
- Test: set an override in a pane by hand, assert the pane renders B's matrix while
  the committed `buffer_id`, `document_buffer_id`, and `option_cache` are unchanged.

**Depends on:** PI.0. **Unblocks:** PI.3.

## PI.2 — Preview buffer option isolation

Implements §6. Give the previewed buffer its own read-only resolved options without
touching global state.

- Host entry point `mount_preview(pane, B)`: ensure B in registry, compute
  `recompute_options_for_buffer(B)` against a read-only mode stack, **without**
  reassigning `document_buffer_id` or calling `rebuild_option_cache`.
- Resolve §10.2: `preview-mode` minor (a) vs. read-only render flag (b). If (a),
  register `preview-mode` (owns `ReadOnly = true` + ephemeral marker) in a host/preview
  module and activate it on B's own stack only.
- Test: `mount_preview` a rust buffer; assert B's `resolved_options` reflect
  rust-mode + read-only, and the origin's entry is untouched.

**Depends on:** PI.1. **Unblocks:** PI.3.

## PI.3 — Cut picker preview over to the override

The behavior switch. Implements §7.

- Rewrite `preview_picker_selection` (`dispatch.rs:17145`) and the no-candidate /
  origin-restore branches to `mount_preview(active_pane, B)` / clear-override instead
  of `activate_buffer` under `self.previewing`.
- Dismiss (`do_picker_dismiss`) and accept (`prepare_open_target_pane`,
  `apply_picker_outcome`) clear the override; accept then commits via the existing
  real-activation path.
- PI.0's isolation + dashboard tests go **green** here.
- Theme/colorscheme live-preview (`apply_picker_preview_outcome`) is orthogonal
  (it swaps the global theme, not a buffer) — leave it, retest it still restores on
  `<Esc>`.

**Depends on:** PI.1, PI.2. **Unblocks:** PI.5.

## PI.4 — GPUI parity

- Mirror the displayed-vs-committed read in the GPUI pane-render path
  (`lattice-ui-gpui/src/window.rs` per-pane reads at `:1604`, `:2439`, `:2530`) + the
  `content_left_pad` per-buffer fix.
- Audit: `grep -rn "preview_buffer_id" crates/lattice-ui-gpui/` non-empty.
- GPUI is feature-gated — verify with `cargo build -p lattice-ui-gpui --features window`
  (a plain `-p lattice-cli` build won't compile it; see CLAUDE.md).

**Depends on:** PI.3.

## PI.5 — Delete the old machinery + artefacts

- Remove `self.previewing`, `preview_origin`, `pending_picker_preview_origin`,
  `restore_preview_origin`, and every `previewing`-gated branch made dead by PI.3.
- Grep-guard: no remaining `activate_buffer` call reachable from a preview path.
- Per-frame preview bench delta in `benchmarks.md` (preview should be cheaper than the
  activate-based baseline).
- Flip design fragment status → ✅; update `implementation.md` ledger; update this
  plan's status column per slice as they land (per
  `feedback_update_slice_docs_per_slice`).

**Depends on:** PI.3, PI.4.

---

## Risk / sequencing notes

- **PI.3 is the one risky slice** (behavior switch across all preview sources: files,
  recent, grep, buffer switcher, `gr`, LSP pickers). PI.0–PI.2 are additive and safe;
  PI.5 is deletion after green. If PI.3 must be split, do it per preview *source*
  (buffer-switcher first — simplest routing — then file/grep).
- **Cursor/scroll in preview:** today `activate_buffer` zeroes them; the projection
  model must decide B's preview scroll (top, or the routing's target line for `gr` /
  grep). Carry it on the override, not on `self.scroll`.
- **Giant/binary guard** already exists in the preview classifier
  (`dispatch.rs:17278+`); keep it in `mount_preview`.
