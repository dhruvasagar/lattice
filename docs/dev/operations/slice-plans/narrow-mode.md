# Narrow Mode — Slice Plan

> **Status: 🚧 In progress.** N.1.0 ✅ landed (2026-06-10). N.1.1+ pending.

Sequencing companion to
[`docs/dev/architecture/narrow-mode.md`](../../architecture/narrow-mode.md).
The design fragment is the source of truth for *what* and *why*; this file
owns *when* and *in what order*.

> **2026-06-10 reshape.** Narrow's primary entry is now the **`zn` operator**
> (composes with any motion / text object), not three hard-coded
> `:narrow-function` / `:narrow-class` / `:narrow-block` ex-commands. The
> operator (N.1.3) + the tree-sitter text objects as first-class grammar
> objects (N.1.4) subsume them. See §6 of the design fragment.

---

## Prerequisites

- **M.3** (edit propagation) ✅ — load-bearing; edits in the narrow view must
  propagate to the source.
- **M.4** (live source-edit propagation) ✅ — required for cross-pane
  coherence between the narrow view and other panes showing the same source.
- **M.10.1** (`ActionHandlerRegistry`) ✅ — required for in-view `:w` /
  `:widen` handlers AND for routing the operator's `Effect::Action(AppEffect)`
  to narrow's handler closure, both without host `Action` enum variants.
- **K.4.7** (per-excerpt syntax highlighting) ✅ — narrow view gets syntax
  for free since it IS a single-excerpt multibuffer.
- **Grammar operator/text-object registry** (`register_operator` /
  `register_text_object`, `OperatorContext.range`) ✅ — already in
  `lattice-grammar`; narrow is the first contributed operator beyond the
  vim-native set (paramount goal #3).

---

## Slices

| Slice     | Title                                                                                 | Status         |
|-----------|---------------------------------------------------------------------------------------|----------------|
| **N.1.0** | `textobjects.scm` query infrastructure + `scope_at_cursor`                            | ✅             |
| **N.1.1** | `create_narrow_view` + NarrowMinorMode + `:narrow {range}` + `:widen`                 | ✅ (core)      |
| **N.1.2** | `:narrow` from Visual selection / cursor paragraph (no explicit range)                | ✅             |
| **N.1.3** | The **`zn` narrow operator** (operator-pending; composes with any motion/text-object) | ✅             |
| **N.1.4** | Tree-sitter text objects as grammar objects (`af`/`if`/`ac`/`ic`)                     | 🚧 (N.1.4a ✅, N.1.4b ✅) |
| **N.1.5** | Stacked narrow — transparent one-hop invariant                                        | 🗒              |

---

### N.1.0 — `textobjects.scm` query infrastructure ✅ (2026-06-10)

**Landed:** `textobjects.scm` for rust/python/javascript (`@function.outer`,
`@class.outer`, `@block.outer`); `LangConfig.textobjects: Option<Query>` +
`LangRegistry::textobjects_query`; `SyntaxSnapshot::scope_at_cursor(line,
col_byte, capture_suffix) -> Option<(u32,u32)>` (innermost-wins, half-open
containment, byte-range-restricted query) + a `Syntax` pass-through. 10 tests
(8 `scope_at_cursor` incl. nested-innermost / outside-scope / no-query /
no-parse / python; 2 registry). `lattice-syntax` green (100 tests), clippy
clean. `scope_at_cursor` is consumed by N.1.4 (the text objects), not by
bespoke commands.

**What landed (original plan, for reference):**

Three new query files:
- `crates/lattice-syntax/queries/rust/textobjects.scm`
- `crates/lattice-syntax/queries/python/textobjects.scm`
- `crates/lattice-syntax/queries/javascript/textobjects.scm`

`LangConfig` gains `textobjects: Option<Query>` (compiled at registry
construction, parallel to `folds` and `symbols`).

`LangRegistry::textobjects_query(lang: &str) -> Option<&Query>` — new accessor.

`SyntaxSnapshot::scope_at_cursor(line: u32, col_byte: u32, capture_suffix: &str)
-> Option<(u32, u32)>`:
- Locates the innermost textobjects-query capture whose name ends with
  `capture_suffix` and whose byte span contains the cursor byte.
- Returns `(start_line, end_line)` inclusive, 0-indexed.
- `None` on: no parse, no textobjects query, no matching capture.

`SyntaxInner::scope_at_cursor` is the concrete implementation; the public
`SyntaxSnapshot` API delegates.

**Tests:**
- `scope_at_cursor_rust_fn_returns_correct_range` — cursor inside a function
  body returns the function's start/end lines.
- `scope_at_cursor_selects_innermost_when_nested` — cursor inside a closure
  inside a function returns the closure's range, not the outer function's.
- `scope_at_cursor_returns_none_outside_any_scope` — cursor on a `use`
  declaration returns `None` for `"function.outer"`.
- `scope_at_cursor_class_rust_struct` — cursor inside a struct definition
  returns the struct's range for `"class.outer"`.
- `scope_at_cursor_block_if_expression` — cursor inside an `if` body returns
  the `if_expression` range for `"block.outer"`.
- `scope_at_cursor_none_when_no_textobjects_query` — language with no
  `textobjects.scm` returns `None` without panicking.

**Crate boundary note:** `lattice-syntax` does NOT depend on
`lattice-multibuffer`. `scope_at_cursor` returns a plain `(u32, u32)` —
no multibuffer types leak into the syntax crate.

---

### N.1.1 — `create_narrow_view` + NarrowMinorMode + `:narrow {range}` + `:widen`

> **✅ Core landed 2026-06-10.** `lattice-multibuffer::providers::narrow`
> (`create_narrow_view`, `NarrowMinorMode` marker, `register_narrow_mode`,
> `register_narrow_ex_commands`) + `AppEffect::NarrowTrigger`/`NarrowWiden`
> (lattice-grammar) + host arms (range resolver, guarded `:widen`) + boot
> wiring. 5 integration tests green; workspace builds clean.
>
> **✅ `:w`-saves-source — RESOLVED 2026-06-10** (generic multibuffer save).
> The original design §8 assumed `:w` could be intercepted via
> `ActionHandlerRegistry`, but `:w` is an *ex-command* (`ex:write` →
> `Effect::SaveBuffer` → `save_blocking` → `document.save()`), not an action.
> The clean fix was therefore generic, not narrow-specific:
> `MultibufferDocumentHandle::save()` now **flushes the source-forwarder then
> saves every dirty source** (was hard-`Err(ReadOnly)`). Because the host calls
> `document.save()` uniformly, `:w` now persists the underlying files from
> **any** multibuffer view — narrow, project-search, future diff/references.
> The forwarder flush (a barrier through the FIFO source-forward channel) is
> load-bearing: without it `:w` would race the async propagation and drop the
> last keystrokes. Tests: `save_persists_view_edits_to_the_source_file` (real
> file-on-disk) + `save_is_readonly_for_a_pathless_view`.
>
> **Deferred to N.1.1.b:**
> - **`q` → widen chord** — needs `action:narrow-widen` in `actions::populate`
>   + a `NarrowMinorMode` keymap + ActionHandlerRegistry handler. `:widen`
>   (guarded ex-command) covers the close path for now.
> - **headerline label-from-path** — N.1.1 uses the buffer *name*; a basename /
>   symbol label is a polish follow-up.

**What lands:**

`crates/lattice-multibuffer/src/providers/narrow.rs` (new file, no feature gate).

```rust
pub fn create_narrow_view(
    activator: &mut dyn ModeActivator,
    source_id: BufferId,
    source_handle: Arc<dyn Document>,
    start_line: u32,
    end_line: u32,
    label: &str,
) -> BufferId
```

Builds a one-excerpt `MultibufferDocumentHandle`, calls
`create_multibuffer_view`, sets headerline to
`Complete { summary: "[narrow] <label> <path>:<start+1>–<end+1>" }`,
activates `NarrowMinorMode`.

`NarrowMinorMode` registers:
- `:widen` ex-command handler → `Effect::CloseBuffer`.
- `q` key in `KeymapLayer::MinorMode(narrow-minor-mode)` → same.
- `:w` / `:write` handler → `source_handle.save()` + echo.

`register_narrow_ex_commands(registry, …)` boot helper registers:
- `:narrow` with optional `{start},{end}` range argument.
  Without a range argument, the handler reads `editor.visual_selection_rows()`
  — if in Visual mode, uses those rows; if in Normal mode, uses the current
  paragraph (blank-line-delimited, like `ip` motion).

`lattice-host::boot` calls `register_narrow_ex_commands` alongside the
existing multibuffer boot wiring.

**Tests:**
- `narrow_renders_only_the_requested_range` — view's composed line count
  equals `end_line - start_line + 1`.
- `narrow_edits_propagate_to_source` — apply an edit to the narrow view;
  source buffer snapshot reflects the change.
- `widen_closes_the_view_buffer` — `:widen` removes the narrow BufferId from
  the buffer registry.
- `w_saves_source_not_narrow_view` — `:w` calls `source.save()`; narrow view
  has no path and does not attempt to save itself.
- `headerline_shows_source_path_and_range` — `headerline()` returns
  `Complete { summary }` with the expected path and 1-indexed line range.
- `narrow_with_explicit_range_argument` — `:42,67narrow` creates a view
  spanning rows 41–66 (0-indexed).

---

### N.1.2 — `:narrow` from Visual selection or cursor line

**What lands:**

The `:narrow` handler (registered in N.1.1) is extended to read the active
selection range when no explicit range argument is given:

- **In Visual mode:** `selection.primary().start.line` through
  `selection.primary().end.line`, rounded to whole lines. Single-line Visual
  selection produces a one-line narrow.
- **In Normal mode (no range argument):** find blank-line boundaries around the
  cursor (paragraph `ip` logic), narrow to that range. Matches Emacs'
  `narrow-to-paragraph`.
- **With explicit `{start},{end}` range** (landed in N.1.1): uses those lines.

The handler exits Visual mode after creating the view (equivalent to `<Esc>`
before the narrow opens).

**Tests:**
- `narrow_from_visual_selection_uses_selection_boundaries`.
- `narrow_from_visual_single_line_creates_one_row_view`.
- `narrow_from_normal_mode_uses_paragraph_boundaries` — cursor on line 5 of a
  paragraph spanning lines 3–8 creates a 6-row narrow.
- `narrow_exits_visual_mode_after_creating_view`.

---

### N.1.3 — The `zn` narrow operator

> **✅ Landed 2026-06-10.** `register_narrow_operator → OperatorId` (spec +
> `apply`, owned by the narrow provider; `apply` maps the resolved range to a
> whole-line span via `range_to_narrow_lines` and emits
> `AppEffect::NarrowLines`). Host: the `NarrowLines` arm narrows the active
> buffer; `register_operator_pending` made `pub` and called from boot to wire
> the `zn` chord at the **universal** operator-pending layer (`znn` = current
> line, `zn{motion|object}` = that span) — owner split per your direction:
> operator owned by the narrow crate, chord-wiring in lattice-host (it needs
> the host-resolved `Builtins`). 5 tests (4 range-conversion incl. the
> half-open-end off-by-one + registration); workspace builds clean. `znaf` etc.
> need the tree-sitter text objects (N.1.4).

**Depends on: N.1.1** (`create_narrow_view`). Independent of N.1.0/N.1.4 —
composes with *existing* vim motions/objects on its own. Design: fragment §6.

**What lands** (all in `lattice-multibuffer::providers::narrow` + one host seam):

- `register_operator("operator:narrow", …, OperatorSpec { repeatable: false,
  blockwise_per_row: false, apply: narrow_apply, args_schema: vec![] })`.
  `narrow_apply(ctx)` reads `ctx.range: ProtoRange`, converts to a whole-line
  `(start_line, end_line)` span, returns
  `Effect::Action(AppEffect { id: narrow_action_id, span })`.
- A narrow `ActionId` + handler closure via `ActionHandlerRegistry` (M.10.1):
  resolves the source (base case `RopeDocumentHandle`; stacking → N.1.5), calls
  `create_narrow_view`, returns `Effect::OpenBuffer`. **No host `Action`/`Effect`
  variant.**
- **New host seam** `register_operator_pending_chord(handle, chord,
  operator_id)` exposing the operator-pending binding mechanism (today hard-
  wired in `keymap_normal.rs`) so a provider can bind an operator key. Narrow
  binds `zn` (`z`→`AfterZ`→`n`→`OperatorPending(narrow)`, structurally a peer of
  `gu`). The primitive is itself a paramount-#3 deliverable.
- `lattice-host::boot` calls narrow's `register_narrow_operator(…)` alongside
  the existing multibuffer wiring.

Makes `znip` / `zniw` / `zni{` / `znG` / `zn}` / `3znj` work immediately against
the classic vim objects/motions. `znaf` waits on N.1.4.

**Tests:**
- `zn_paragraph_narrows_to_paragraph_lines` (`znip`).
- `zn_inside_braces_narrows_brace_block` (`zni{`).
- `zn_to_eof_narrows_cursor_to_end` (`znG`).
- `zn_count_narrows_cursor_plus_n` (`3znj`).
- `zn_emits_open_buffer_via_action_handler` — apply emits `Effect::Action`; the
  handler closure produces the view; assert no host variant added.
- `narrow_operator_registered_by_provider_not_host` — operator + chord come from
  the narrow crate's boot fn; zero `Editor::do_narrow_*`, zero new host `Action`.

---

### N.1.4 — Tree-sitter text objects (first-class grammar objects)

**Depends on: N.1.0** (`scope_at_cursor`). Independent of N.1.3 — `daf` / `vac`
work before the operator exists. Design:
[`tree-sitter-text-objects.md`](../../architecture/tree-sitter-text-objects.md).

Owned by `lattice-syntax`; registered through the existing `register_text_object`
API; first-class (composes with every operator, visible to `:describe-key`).
Locked keybinding catalog:

| Object | outer | inner | suffix |
|---|---|---|---|
| function | `af` | `if` | `function.outer` / `.inner` |
| class / type | `ac` | `ic` | `class.outer` / `.inner` |
| parameter | `aa` | `ia` | `parameter.outer` / `.inner` |
| loop | `al` | `il` | `loop.outer` / `.inner` |

**Comment (`aC`/`iC`) is NOT in this slice** — it's text/`commentstring`-driven,
not tree-sitter, and belongs in `lattice-grammar`; it gets its own slice (needs
a comment-leader descriptor). `call` / `conditional` / `block` keys are also
deferred (no clean free key; reachable via classic `i(` / `i{` + `zn`+motion) —
see the design fragment §3.

**Sub-slices:**

- **N.1.4a — the `ScopeResolver` seam (`lattice-grammar`). ✅ landed 2026-06-10.**
  `trait ScopeResolver { fn scope_at(line, col, suffix) -> Option<(u32,u32)>; }`
  + `TextObjectContext.scope_resolver: Option<&dyn ScopeResolver>`, threaded
  through `execute_text_object` + `execute_operator → resolve_target` (both
  context sites). `execute()` kept as a 6-arg wrapper delegating `None`; the new
  `execute_with_scope_resolver()` (7-arg) carries the resolver — so the ~24
  existing grammar/test callers are untouched and N.1.4a is a behavioural no-op
  (193 grammar tests pass, workspace builds). `ScopeResolver` +
  `execute_with_scope_resolver` re-exported for N.1.4b.
- **N.1.4b — host wiring (`lattice-runtime` + `lattice-host`). ✅ landed 2026-06-10.**
  `impl lattice_grammar::ScopeResolver for SyntaxSnapshot` (lattice-syntax →
  lattice-grammar dep; forwards to `scope_at_cursor`). The resolver reaches the
  grammar via a new `Document::dispatch_with_scope_resolver(inv, cursor, cancel,
  Option<ScopeResolverHandle>)` trait method (default impl delegates to
  `dispatch_with_cancel`, so the multibuffer + future kinds are a no-op until
  wired); `RopeDocumentHandle` overrides it to put the resolver on
  `ActorMsg::Dispatch`, and the actor calls `execute_with_scope_resolver`.
  `ScopeResolverHandle = Arc<dyn ScopeResolver + Send + Sync>` crosses the actor
  channel as one Arc bump (immutable snapshot, wait-free read — paramount #1).
  The host's `dispatch_blocking` reads the existing `self.syntax` hot-path slot
  (no new accessor needed) and passes it down; non-Document buffers pass `None`
  or hit the default, so **no `BufferKind` branch** ([[feedback_buffers_no_special_case]]).
  Note: the original sketch (a `syntax_handle_for(id)` buffer-store accessor +
  "put snapshot into the context") was superseded — the snapshot is a per-dispatch
  input, so a dispatch-time trait param is the honest model, not a side-channel
  accessor. Narrow/multibuffer in-view text objects (per-excerpt source snapshot
  + composed→source translation) are deferred to **N.1.5**. Wire proven by
  `dispatch_with_scope_resolver_threads_resolver_to_text_object` (actor test:
  resolver Some → text object sees the mock range; None → sees None). No
  text-object keys are bound yet — that is N.1.4c.
- **N.1.4c — `.outer` objects (`lattice-syntax`).** New `lattice-syntax →
  lattice-grammar` dep; `register_syntax_text_objects(&mut registry)` registers
  `af`/`ac`/`aa`/`al` (`.outer`). Boot calls it. (Comment is a separate slice.)
- **N.1.4d — `.inner` objects.** Author `@function.inner` / `@class.inner` /
  `@parameter.inner` / `@loop.inner` captures in the `.scm` files; register
  `if`/`ic`/`ia`/`il`; `iC` strips markers from `comment.outer`.

**Tests:**
- `af_selects_whole_function` (operator-agnostic via `d`/`v`).
- `ac_selects_struct`, `aa_selects_argument`, `al_selects_loop`.
- `if_selects_function_body` (`.inner`).
- `af_innermost_targets_closure_in_fn` — innermost-wins via `scope_at_cursor`.
- `af_fails_when_cursor_outside_any_fn` — operator no-op / bell, no edit.
- `af_fails_on_plain_buffer` — `scope_resolver` is `None`; no panic.
- `znaf_narrows_function` — operator + object end-to-end (with N.1.3).
- `daf_deletes_function` — proves universality (works with `d`, not just `zn`).

---

### N.1.5 — Stacked narrow: transparent one-hop invariant

**Depends on: N.1.3** (operator), **N.1.4** (text objects — for `znaf` in-view).

**What lands:**

The narrow handler closure (N.1.3) resolves the source when the active document
is already a multibuffer, so the operator targets the ORIGINAL file:

```rust
let (source_id, source_cursor) = if let Some(mb_handle) =
    services.get::<MultibufferRegistryHandle>()
        .and_then(|r| r.handle_for(active_buffer_id))
{
    mb_handle
        .translate_composed_to_source(cursor)
        .unwrap_or((active_buffer_id, cursor))
} else {
    (active_buffer_id, cursor)
};
```

`source_id` is always a `RopeDocumentHandle`; `create_narrow_view` is never
called with an intermediate multibuffer's id. The `zn` operator is universal
(Builtin layer), so it already fires inside a narrow view — no per-mode chord
re-registration is needed; only the handler's source-resolution step is added.

**Tests:**
- `stacked_zn_targets_original_source` — narrow a file; from the narrow view
  `znaf`; the new narrow's source is the original `RopeDocumentHandle`, not the
  intermediate narrow.
- `zn_within_search_results_targets_real_source` — `znaf` from a search-result
  multibuffer row opens a narrow on the source file.
- `two_narrows_same_source_live_synced` — edit through narrow A; narrow B
  updates on the next recompose tick (M.4).
- `narrow_depth_is_always_one` — 3× `znaf`; every resulting narrow view has a
  single `RopeDocumentHandle` source.

---

## Slice sequencing

```
N.1.0 ✅ (scope_at_cursor — landed)
   │
   ├───────────────────────────────────────┐
   │                                       │
N.1.1 (core view + `:narrow {range}`)   N.1.4 (text objects `af`/`ac`
   │     │                                  reading scope_at_cursor)
   │     ├─ N.1.2 (`:narrow` Visual/Normal)        │
   │     │                                         │
   │     └─ N.1.3 (`zn` operator — composes        │
   │           with existing motions/objects)      │
   │                 │                             │
   │                 └────────────┬────────────────┘
   │                              │
   │                       N.1.5 (stacking — needs the operator
   │                              + text objects for `znaf` in-view)
```

- **N.1.1** lands standalone (line-range `:42,67narrow` is useful without
  N.1.0).
- **N.1.3** (operator) needs only N.1.1 — `znip` / `zni{` / `znG` work against
  classic vim objects immediately.
- **N.1.4** (text objects) needs only N.1.0 — `daf` / `vac` work even before
  the operator. `znaf` needs both N.1.3 and N.1.4.
- **N.1.5** (stacking) needs N.1.3 + N.1.4.

---

## Acid test (post-N.1.5)

1. Open a Rust source file.
2. Position cursor inside a function body.
3. `znaf` → narrow view opens spanning exactly the function.
4. Edit a line. Gutter shows source line numbers. Syntax highlighting is live.
5. `:w` in the narrow view → source file saved on disk.
6. `znac` from inside the narrow → new narrow on the struct/impl in the
   **original** file (one-hop invariant holds).
7. Two narrows to different regions of the same file, side by side → edit
   through one; the other updates.
8. `:widen` (or `q`) → narrow buffer closed.
9. `znip` / `zni{` / `znG` narrow against classic vim objects/motions (no
   tree-sitter needed — proves the operator is grammar-general).
10. `daf` deletes a function, `vac` selects a class — the tree-sitter objects
    work with every operator, not just `zn`.

Zero `Editor::do_narrow_*` methods in `lattice-host`. Zero new `Action`/`Effect`
variants in core (the operator emits the generic `Effect::Action(AppEffect)`). A
new narrow/text-object target needs only a new textobjects.scm line + a
`register_text_object` call — no host edits.
