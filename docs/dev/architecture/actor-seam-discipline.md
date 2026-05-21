# Actor-seam discipline

How `App`-side code decides between **published RenderState reads**
and **actor-mailbox RPC** (`App::{read,mutate,mutate_with}_editor`).
Established by the slice arc `3c.atomic` → `3c.final.E.swap` →
B-extension → `3c.extension.fold-rs` → `3c.final.X.cleanup`.

## The rule

> **Paint paths must read RS, never `read_editor`. Keystroke
> paths must bound their `mutate_editor`/`mutate_editor_with`
> count. Cold paths route through the actor freely.**

The cost asymmetry that motivates this:

| Path | Mechanism | Per-call cost (WSL2) |
|------|-----------|------------------------|
| RS read (`app.ad().X`, `app.modes()`, `app.popup()`, …) | wait-free `ArcSwap::load` + Arc clone | ~10-100ns |
| Actor RPC (`app.read_editor(\|e\| ...)`) | mpsc send + actor dispatch + oneshot recv | ~94µs |

A single keystroke runs at most ~125 times per second at brisk
typing speeds. At ~94µs per RPC, six RPCs per keystroke is
already half of a 16ms 60Hz frame budget. Per-frame paint paths
run 60-120× per second; a single per-line RPC at 120 lines is
~11ms — half the frame budget consumed by one missed lift.

## Path classification

### Paint path → 0 RPCs

The compose loop, modeline builder, pane-status builder, gutter
renderer, fold-glyph resolver, severity-cell renderer, and every
function they call. These run per frame, often per visible line.

**Rule:** every read goes through a published RS sub-state
accessor:

```rust
let cursor   = app.ad().cursor;
let folds    = &app.ad().folds;
let modes    = app.modes().map.get(buf);
let popup    = app.popup().buffer_id;
let locals   = app.buffer_locals().map.get(buf);
let registry = app.options().config.get_typed::<X>();
let last_msg = &app.messages().last;
let cmdline  = &app.modeline().cmdline_text;
let buffers  = app.buffers().registry;
let lsp      = app.render_state.load().lsp;
let diags    = app.render_state.load().diagnostics.layer;
```

If a paint path needs derived per-frame data (e.g. a mode-gate
flag), cache it once at `FrameView::from_app` and read the
cached field per line. See `FrameView::lsp_diagnostics_enabled` /
`foldenable` for the shape.

**Enforced by**: `compose_visible_lines_makes_zero_actor_calls`,
`modeline_label_makes_zero_actor_calls`,
`pane_status_label_makes_zero_actor_calls` in
`crates/lattice-ui-tui/src/render.rs` (test module). The
counter is `crate::actor_call_counter` — a thread-local `Cell<u64>`
bumped on every `read_editor` / `mutate_editor` /
`mutate_editor_with` call.

### Keystroke path → bounded RPCs

`App::apply` is the keystroke entry point. It MUST hit the actor
at least once (the dispatch is a mutation); extra RPCs there
stack up at typing rate.

Current baseline: **5 RPCs per `Action::None`**, all on the apply
tail. Each is wait-free-checkable and could become "check RS,
mutate only on change" in a follow-up (`3c.extension.apply-tail-rs`).

**Enforced by**: `apply_noop_action_makes_bounded_actor_calls`
caps the delta at ≤ 6. Future additions that push past 6 should
RS-lift, not raise the bound.

### Cold path → freely uses actor

Cold paths fire at user-driven cadence rather than per-frame or
per-keystroke:

- LSP request bodies (hover, completion, code action, …) — fire
  on `gd` / `K` / `gA` / `<C-Space>` / etc.
- Picker accept tails — fire on `<CR>` in a picker.
- Ex-command bodies (`:customize-edit`, `:lsp-restart`, …) —
  fire on `<CR>` in cmdline.
- File-tree / Oil refresh — fire on directory change.
- Lifecycle (file open / save / quit) — fire once per file op.
- Completion-flow handlers — fire on completion trigger.

These freely route through `read_editor` / `mutate_editor` /
`mutate_editor_with`. The actor RPC cost (~94µs) is invisible
against the actual work (LSP roundtrip ~10-1000ms; file I/O
~1-100ms; picker fuzzy-match ~100µs-10ms).

**Audit baseline (2026-05-21)** — ~50 `read_editor` sites remain
across:

| File | Sites | Cold-path category |
|------|------:|--------------------|
| `app/lsp.rs` | 16 | LSP request handlers |
| `app/lifecycle.rs` | 6 | startup / file ops |
| `app/completion.rs` | 5 | completion flow |
| `app/file_tree.rs` | 4 | tree refresh |
| `app/options.rs` | 3 | `:customize-edit` / typed-options resolve |
| `app/motions.rs` | 3 | active_text/cursor/buffer_id helpers (search.rs caller) |
| `app/oil.rs` | 2 | oil-buffer refresh |
| `app/popup.rs` | 2 | popup_help + snapshot_current_popup |
| `app/help.rs` | 2 | customize-edit lookup |
| single-digit elsewhere | … | one-shot ex-command bodies |

If any of these surface in a flamegraph during typing or paint,
they're miscategorised — promote to keystroke or paint path and
RS-lift.

## The `cfg(test)` escape hatch

Migrating App-side accessors from `read_editor` → published RS
reads breaks tests that mutate `editor.X = Y` directly without
calling `publish_render_state()`. The prior `read_editor` path
read the live `&self.editor.X` field under `cfg(test)`, so
"direct field mutation is immediately visible" was a load-bearing
test convention across hundreds of tests.

The convention for migrated accessors:

```rust
pub(crate) fn cursor(&self) -> Position {
    // cfg(test) escape hatch: tests mutate editor fields directly
    // without `publish_render_state()`. Keep them reading live.
    #[cfg(test)]
    {
        self.editor.cursor
    }
    #[cfg(not(test))]
    {
        self.ad().cursor
    }
}
```

Production runs the wait-free path; tests preserve the direct-
mutation idiom. The two paths are semantically equivalent (the
RS sub-state mirrors the editor field every dispatch publish);
the divergence is purely about test ergonomics.

Migrated accessors that follow this pattern (slice
`3c.final.X.cleanup`):

- `App::cursor()`, `App::document_buffer_id()`,
  `App::command_line()`
- `App::foldenable()`, `App::line_inside_closed_fold(line)`,
  `App::fold_start_at(line)`, `App::fold_start_at_any(line)`
- `App::lsp_mode_enabled_for(buf)` plus the five sub-mode
  variants (`diagnostics`, `progress`, `document_highlight`,
  `inlay_hint`, `semantic_tokens`)

Cross-reference: `App::cursor()`'s docstring documents the
pattern in full; the others link back via `// see App::cursor()`.

## When in doubt

1. **Is this called per frame?** → paint path. Read via RS only.
2. **Is this called per keystroke?** → keystroke path. Bound
   RPCs; prefer RS reads.
3. **Is this called once per user action (`<CR>`, `:cmd`,
   click)?** → cold path. Actor RPC is fine.

If the answer is "I'm not sure", treat it as paint path. The
worst case of over-caution is a slightly slower cold-path call
(invisible); the worst case of under-caution is a per-frame RPC
that turns a 90µs paint into a 43ms paint (slice
`3c.extension.fold-rs` was exactly this scenario).
