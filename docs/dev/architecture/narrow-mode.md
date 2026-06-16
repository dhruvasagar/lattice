# Narrow Mode

Design fragment for Lattice's narrow mode: a focused, editable multibuffer
view of a single source-buffer excerpt. Edits propagate to the underlying
file through the standard M.3 edit path. Slice sequencing lives in
[`../operations/slice-plans/narrow-mode.md`](../operations/slice-plans/narrow-mode.md).

---

## 1. Design goal

Narrow mode restricts the editing surface to a region of the active file. The
region is chosen the vim-native way: a **`zn` operator** that composes with any
motion or text object (`znip` paragraph, `zni{` inside braces, `znG` to EOF,
`znaf` a function via the tree-sitter text objects) — plus `:narrow {range}`
for explicit line ranges and Visual selections. See §6 for the operator. Inside
the narrow view:

- Every vim motion, operator, text object, macro, and undo works identically
  to any other buffer. Grammar completeness is non-negotiable.
- `:w` saves the underlying source file — not a synthetic buffer, not a copy.
- The gutter shows **source** line numbers (42, 43, … not 1, 2, …) so the
  user always knows where they are in the original file.
- Syntax highlighting, LSP, completions, diagnostics, folding, and decorations
  are fully operative. The narrow view IS a Document from every subsystem's
  perspective.
- Opening multiple narrows to different regions of the same source file is
  legal and encouraged. M.4 keeps both views live-synced with one another and
  with any other pane showing the source buffer.

---

## 2. Reference points

**Emacs `narrow-to-region` / `narrow-to-defun`** — the UX precedent. Emacs
restricts the buffer window so only the selected region is visible and editable.
Lattice provides the same user-facing property through a different mechanism.

Why Lattice's approach is strictly better:

| Property | Emacs narrow | Lattice narrow |
|---|---|---|
| Multiple simultaneous narrows of the same file | One per buffer | Unlimited — each narrow is its own BufferId |
| Parallel pane edits stay live-synced | Not possible (single-buffer view) | M.4 event-driven recompose |
| Stacked narrow depth | N hops (fragile) | Always 1 hop — N.1.4 transparent stacking |
| Vim grammar in narrow view | Breaks some Emacs commands | Grammar-complete: motions, operators, macros, undo all work |
| `:w` saves the real file | Requires `widen` + `save-buffer` sequence | Directly; NarrowMinorMode `:w` override |
| Region selection | `C-x n n` / `n d` / `n w` chords | `zn` operator over the **whole vim grammar** — any motion or text object (`znip`, `zni{`, `znaf`, `znG`, `3znj`) |
| Tree-sitter targets | `narrow-to-defun` only (defun + emacs symbol-at-point) | `znaf` / `znac` via textobjects.scm text objects — same objects power `daf` / `vic` |

**Vim `narrow.vim`** — fakes narrow by yanking a region to a scratch buffer
and patching back on `:w`. Fragile (diverges if source changes mid-session),
no live propagation. Not a reference for anything beyond the UX goal.

**Helix / Zed** — no narrow equivalent.

---

## 3. Architecture

Narrow mode is a **one-excerpt multibuffer** with a **minor-mode save
override**. No new Document type, no renderer changes, no grammar changes.

```
User: `znaf`  (or `:narrow`, or `znip` / `zni{` / `znG` / …)
         │
         ▼
narrow operator `apply` (lattice-multibuffer::providers::narrow) — §6
  │  operator-pending machinery already resolved OperatorContext.range
  │         from the motion/text-object (`af` → function scope, etc.)
  │  converts the ProtoRange to a (start_line, end_line) span
  │  emits Effect::Action(AppEffect …) carrying narrow ActionId + span
  ▼
narrow ActionHandlerRegistry closure (same crate) — M.10.1
  │  calls create_narrow_view(activator, source_id, start_line, end_line)
  │         (N.1.1 — one-excerpt MultibufferDocumentHandle)
  │  activates NarrowMinorMode on the new view
  │  returns Effect::OpenBuffer { id: narrow_buffer_id }
  ▼
MultibufferDocumentHandle {
  sources: { source_id → source_handle },
  excerpts: [ Excerpt { source, start_line, end_line } ],   ← one excerpt
}
  + NarrowMinorMode (minor, activated on this view's BufferId)
      registers: `:widen` / `:w` override / keymap layer
```

The narrow view is `BufferKind::Multibuffer` — the same kind as project-search
results and LSP reference views. There is no `BufferKind::Narrow`. The
NarrowMinorMode is the only discriminator that a multibuffer carries narrow
semantics.

**Everything-is-a-buffer holds.** `feedback_buffers_no_special_case` is
satisfied: the renderer, motion engine, and grammar see a normal Document. The
minor mode contributes the narrow-specific `:w` / `:widen` handlers and the
headerline. No `match buffer_kind { Narrow => … }` ever appears.

**Mode owns its full surface** (`feedback_mode_owns_its_surface`). The narrow
provider crate registers, from its own boot fn (§6): the `zn` **operator**
(spec + `apply`), its narrow **ActionId**, the **handler closure** that calls
`create_narrow_view`, and the `zn` operator-pending **keybinding**. The
secondary entry ex-commands (`:narrow {range}`, `:widen`) register the same
way. The in-view `:w` override / `:widen` / `q` register in
`NarrowMinorMode::on_activate` and unregister when the Guard drops. Zero
`Editor::do_narrow_*` methods; zero new host `Action`/`Effect` variants — the
operator emits the generic `Effect::Action(AppEffect)` routed through the
`ActionHandlerRegistry` (M.10.1). This passes the mode-ownership acid test:
binding *and* handler body both ship from `lattice-multibuffer`.

---

## 4. Tree-sitter text object infrastructure (N.1.0)

### 4.1 New query files: `textobjects.scm`

One file per supported language under
`crates/lattice-syntax/queries/<lang>/textobjects.scm`. Capture convention
follows Helix/nvim-treesitter for portability.

```scheme
; rust/textobjects.scm
(function_item) @function.outer
(closure_expression) @function.outer

(struct_item) @class.outer
(enum_item) @class.outer
(trait_item) @class.outer
(impl_item) @class.outer

(block) @block.outer
(if_expression) @block.outer
(loop_expression) @block.outer
(while_expression) @block.outer
(for_expression) @block.outer
(match_expression) @block.outer
(match_arm) @block.outer
```

```scheme
; python/textobjects.scm
(function_definition) @function.outer
(lambda) @function.outer

(class_definition) @class.outer

(block) @block.outer
(if_statement) @block.outer
(for_statement) @block.outer
(while_statement) @block.outer
(with_statement) @block.outer
(match_statement) @block.outer
```

```scheme
; javascript/textobjects.scm
(function_declaration) @function.outer
(function_expression) @function.outer
(arrow_function) @function.outer
(method_definition) @function.outer

(class_declaration) @class.outer
(class_expression) @class.outer

(statement_block) @block.outer
(if_statement) @block.outer
(for_statement) @block.outer
(for_in_statement) @block.outer
(while_statement) @block.outer
(do_statement) @block.outer
(switch_statement) @block.outer
(try_statement) @block.outer
```

### 4.2 `LangConfig` extension

```rust
pub(crate) struct LangConfig {
    pub(crate) language: Language,
    pub(crate) highlight: HighlightConfiguration,
    pub(crate) injections: Option<HighlightConfiguration>,
    pub(crate) folds: Option<Query>,
    pub(crate) symbols: Option<Query>,
    pub(crate) textobjects: Option<Query>,   // ← N.1.0
}
```

`LangRegistry::textobjects_query(lang: &str) -> Option<&Query>` — parallel to
the existing `LangRegistry::folds_query` / `LangRegistry::symbols_query`.

### 4.3 `SyntaxSnapshot::scope_at_cursor`

As shipped in N.1.0 (on `SyntaxSnapshot`, with a `Syntax` pass-through):

```rust
impl SyntaxSnapshot {
    /// Find the innermost tree-sitter node matching `capture_suffix`
    /// (e.g. `"function.outer"`, `"class.outer"`, `"block.outer"`) that
    /// contains the cursor at (`line`, `col_byte`).
    ///
    /// Returns `(start_line, end_line)` inclusive, 0-indexed.
    ///
    /// Returns `None` when: no parse, no textobjects query, no matching
    /// node contains the cursor.
    ///
    /// Algorithm:
    ///  1. Convert (line, col_byte) to a byte offset.
    ///  2. Run textobjects query; among captures whose name ends with
    ///     `capture_suffix` and whose byte span contains the cursor byte,
    ///     pick the one with the **smallest** byte span (innermost).
    ///  3. Map winner's start/end bytes to 0-indexed line numbers.
    ///
    /// O(matches) — in practice O(1) for typical nesting depth.
    pub fn scope_at_cursor(
        &self,
        line: u32,
        col_byte: u32,
        capture_suffix: &str,
    ) -> Option<(u32, u32)>
}
```

`capture_suffix` decouples query capture naming from the consumer. The
tree-sitter text objects pass it: the `function` object resolves
`"function.outer"`, `class` → `"class.outer"`, `block` → `"block.outer"`. Those
objects then compose with **any** operator — `znaf` narrows, `daf` deletes,
`vic` selects. Additional objects (`"parameter.outer"`, `"comment.outer"`) need
only a new textobjects.scm line + a text-object registration; the `zn` operator
picks them up for free. See §6.3.

---

## 5. NarrowMinorMode

Lives in `crates/lattice-multibuffer/src/providers/narrow.rs`.

### 5.1 Mode struct

```rust
pub struct NarrowMinorMode {
    source_buffer_id: BufferId,
    view_buffer_id:   BufferId,
}

pub struct NarrowModeGuard {
    _handler_tokens: Vec<lattice_mode::ActionRegistrationToken>,
}

impl Mode for NarrowMinorMode {
    type Guard = NarrowModeGuard;
    fn mode_id() -> ModeId { ModeId::from("narrow-minor-mode") }
    fn target_buffer_kind() -> Option<BufferKind> { None }  // manual activation only
    fn options() -> ModeOptions { ModeOptions { read_only: false, no_file: true } }
    fn on_activate(&self, ctx: &mut ModeActivationCtx) -> NarrowModeGuard { … }
}
```

### 5.2 Handlers registered in `on_activate`

| Ex-command / chord | Behavior |
|---|---|
| `:widen` | `Effect::CloseBuffer { id: view_buffer_id }` |
| `q` (Normal, narrow keymap layer) | Same as `:widen` |
| `:w` / `:write` | `source_handle.save()` then `Effect::Echo("Saved <path>")` |

The `zn` operator (§6) is universal, so it also fires **inside** a narrow
view; the stacking invariant (§7) resolves its target against the original
source. NarrowMinorMode registers no narrow-trigger handlers of its own — only
the in-view save / widen surface above.

### 5.3 Entry surfaces (summary)

Two surfaces, both registered from the narrow provider crate:

- **`zn` operator** (primary) — composes with any motion / text object; full
  design in §6. `znaf` / `znac` reach the tree-sitter targets; `znip` /
  `zni{` / `znG` / `3znj` reach any classic vim object or motion.
- **Ex-commands** (secondary — explicit ranges + discoverability), registered
  at boot by `register_narrow_ex_commands(registry, …)`:

| Ex-command | Range support | Behavior |
|---|---|---|
| `:narrow` | `:42,67narrow` | Narrow to explicit range; or Visual selection (line-rounded); or — bare, from Normal — the current paragraph (`:narrow-to-paragraph`) |
| `:widen` | — | Close the active narrow view |

The per-target `:narrow-function` / `:narrow-class` / `:narrow-block` commands
from the original sketch are **subsumed** by `znaf` / `znac` / `zn`+a-block-object
— the operator over the tree-sitter text objects is strictly more general. They
may ship later as thin convenience aliases if discoverability demands, but they
are not the primary surface.

### 5.4 Headerline

Immediately on view construction:

```
[narrow]  src/editor.rs : 102–167
```

Set via `set_headerline(HeaderlineStatus::Complete { summary })`. No
`InProgress` phase — narrow is instantaneous. The `MultibufferStatusProvider`
renders this as the sticky headerline row above the excerpt.

---

## 6. The narrow operator (`zn`)

The primary entry point is a vim **operator**, `zn`, that composes with any
motion or text object — the same way `d`/`c`/`y` do. This is paramount goal #3
made concrete: the grammar IS the public API, and narrow is the first operator
contributed beyond the vim-native set.

### 6.1 Grammar mechanics — it reuses the operator-pending machinery verbatim

Operators in `lattice-grammar` are **registered**, not a hard-coded enum:

```rust
// lattice-grammar::registry
pub fn register_operator(&mut self, name: &str, doc: &str, spec: OperatorSpec) -> OperatorId;

pub struct OperatorSpec {
    pub repeatable: bool,           // narrow: false (it opens a view, nothing to `.`-repeat)
    pub apply: OperatorFn,          // Fn(&mut OperatorContext) -> GrammarResult<Effect>
    pub args_schema: Vec<ArgSpec>,  // empty
    pub blockwise_per_row: bool,    // false — a block-visual narrow collapses to one contiguous range
}

pub struct OperatorContext<'a> {
    pub document: &'a mut Document,
    pub range:    ProtoRange,       // ← already resolved from the motion / text object
    pub linewise: bool,
    pub register: Register, pub count: Count, pub args: Args,
    pub cancel:   &'a CancellationToken,
}
```

The decisive property: **`OperatorContext.range` is handed to the operator
already resolved** from whatever motion/text-object the user typed. The narrow
operator does not parse motions — the existing operator-pending state machine
does, exactly as it does for `d`/`y`/`c`. `zn` only consumes the resulting
range.

Key sequencing — `zn` is a two-key operator prefix, structurally identical to
the case operators `gu` / `gU` / `g~`:

```
Normal --z--> AfterZ --n--> OperatorPending(narrow) --{motion|text-object}--> dispatch
   (cf.  Normal --g--> AfterG --u--> OperatorPending(lowercase) --motion--> dispatch)
```

`z` is already a pending prefix (`AfterZ`, shared with the fold/scroll family).
`zn` is unbound in Lattice today (the fold family uses `zi` for foldenable, not
vim's `zn`), so the slot is free and lands narrow squarely in the fold/focus
family — its principled home (narrow is fold's bigger sibling: hide-the-rest /
focus). `count` is honoured: `3znj` narrows the cursor line + 3.

### 6.2 Where it lives + the dispatch flow

The narrow **provider crate owns the whole surface** —
`lattice-multibuffer::providers::narrow` registers, from its boot fn:

1. the operator spec via `register_operator("operator:narrow", …)`,
2. a narrow `ActionId` + a handler closure via `ActionHandlerRegistry` (M.10.1),
3. the `zn` operator keybinding, via the host-exposed generic primitive
   `keymap_normal::register_operator_bindings(handle, op_prefix, operator_id, …)`
   called from boot (it needs the host-resolved `Builtins`). This one call wires
   the operator across BOTH modes from a single registration — the Normal
   operator-pending family (`znn`, `zn{motion|object}`) AND the Visual
   selection-bind (`op.with_range(Range::Selection)`), because an operator acts
   on the selection by design (see `keymap-architecture.md` §7.2, upgrade 3).
   Exposing operator-binding registration to providers is itself a paramount-#3
   win. *(The earlier sketch named a narrower `register_operator_pending_chord`
   Normal-only seam; the cross-mode `register_operator_bindings` subsumed it.)*

The operator can only emit an `Effect` (its `apply` sees only `&mut Document`),
and minting a buffer needs host services — so `apply` emits the **generic**
`Effect::Action(AppEffect …)` carrying the narrow ActionId + the line span; the
host routes it through the `ActionHandlerRegistry` to narrow's closure, which
calls `create_narrow_view`. **No narrow-specific `Effect`/`Action` variant in
core.**

```
zn{motion}                          (any buffer; inside a narrow view too — §7)
  │  operator-pending resolves OperatorContext.range from {motion}
  ▼
operator:narrow  apply(ctx)         (lattice-multibuffer::providers::narrow)
  │  span = lines_of(ctx.range)     ; whole-line in v1 (N.2 = sub-line)
  └─ Effect::Action(AppEffect{ id: narrow_action_id, span })
        ▼
narrow handler closure              (same crate; registered via ActionHandlerRegistry)
  │  if active doc is a multibuffer → translate_composed_to_source (§7)
  │  create_narrow_view(activator, source_id, source_handle, start, end, label)
  └─ Effect::OpenBuffer { id: narrow_buffer_id }
```

`create_narrow_view` is the shared sink — every entry surface (operator,
`:narrow`, Visual) funnels through it:

```rust
/// Allocate a one-excerpt multibuffer view and activate NarrowMinorMode on it.
/// `label` → headerline "[narrow] <label> <path>:<start+1>-<end+1>"
/// (function/class name from collect_symbol_locations when known, else "").
pub fn create_narrow_view(
    activator: &mut dyn ModeActivator,
    source_id: BufferId,
    source_handle: Arc<dyn Document>,
    start_line: u32,
    end_line: u32,
    label: &str,
) -> BufferId
```

Steps inside `create_narrow_view`:

1. Build `MultibufferDocumentHandle` with one `Excerpt { source: source_id, start_line, end_line }`.
2. Call `create_multibuffer_view(activator, sources, excerpts, name, flags)` (existing M.2.b.2 API).
3. Set headerline `Complete { summary: "[narrow] <label> <path>:<start+1>–<end+1>" }`.
4. `activator.activate_minor(view_id, NarrowMinorMode { source_buffer_id, view_buffer_id })`.

`create_multibuffer_view` / `activate_minor` are existing M.2.b.2 / M-async
primitives. The only new host primitive is the operator-pending registration in
step 3 above.

### 6.3 Tree-sitter text objects (separate, universal feature)

`znaf` / `zniC` need the tree-sitter scopes registered as first-class grammar
**text objects** (`af`/`ac`/`aC`/…). That is a separate, *universal* feature —
once registered, **every** operator gets them (`daf`, `vic`, `yaf`), not just
`zn`. They are owned by `lattice-syntax`, not narrow-mode, and register through
the existing `register_text_object` API plus one new `ScopeResolver` seam in
`lattice-grammar`.

Full design — the locked keybinding catalog (`af`/`if`, `ac`/`ic`, `aa`/`ia`,
`al`/`il`, `aC`/`iC`), the `ScopeResolver` trait, and the three-crate ownership
split — lives in
[`tree-sitter-text-objects.md`](tree-sitter-text-objects.md). The split is the
natural slice boundary:

- The `zn` operator (§6.1–6.2) composes with **existing** vim motions/objects
  immediately: `znip`, `zni{`, `znG`, `3znj` work the moment it lands (N.1.3).
- The tree-sitter objects (N.1.4) add `af`/`ac`/`aa`/`al`/`aC` (+ `.inner`),
  which `zn` — and every other operator — then pick up for free.

### 6.4 Widen

There is no `zN` operator. Widening is **closing the narrow buffer**, not an
operation over a motion — handled by `:widen` / `q` inside the view (§5.2). The
asymmetry is correct: `zn` produces a buffer; you dispose of it like any buffer
(`:bd`, `:widen`, `q`), not by "un-operating."

---

## 7. Stacked narrow — transparent one-hop invariant

When `znaf` (or any `zn{motion|object}`) fires from a buffer that is already a
multibuffer view (narrow, search results, LSP references, project-diff — any
kind):

1. Read cursor in composed coordinates.
2. Call `handle.translate_composed_to_source(cursor)` → `(real_source_id, real_cursor)`.
3. Resolve the target scope at `real_cursor` in `real_source_id`.
4. Create a NEW narrow view on `real_source_id` — the **original file**.

The intermediate view is neither read from nor written to. Every narrow view
is always exactly one hop from a `RopeDocumentHandle`.

> **Implementation note (N.1.5, 2026-06-10).** The steps split across two layers:
> the text object resolves *inside* the multibuffer's own dispatch (a
> `ComposedScopeResolver` maps composed↔source against the per-excerpt syntax, so
> step 3 happens there rather than as a handler re-resolve), and the host handler
> (`Editor::resolve_narrow_target`) translates the resulting line range to the
> source and creates the view (steps 2 + 4). This also unlocks `daf` / `yaf`
> inside *search* views, not just narrow. See the N.1.5 slice plan for the
> mechanism.

**Why this is the correct long-term design (heuristic #1).** O(n)-hop chains
create a translation stack that diverges when an inner-hop edit hasn't propagated
before an outer-hop reads composed coordinates. Transparent stacking eliminates
the problem structurally. The alternative — a `MultibufferDocumentHandle` whose
source is another `MultibufferDocumentHandle` — requires a new code path in
M.3's `apply_edit` and `build_source_edit`, with no concrete demand at v1.

---

## 8. Save semantics

`:w` in a narrow view triggers:

```rust
// registered by NarrowMinorMode::on_activate
let token = action_registry.register(
    ActionIds::write_buffer(),
    move |ctx: ActionContext| {
        let source = ctx.services
            .get::<BufferStoreHandle>()?
            .handle_for(source_buffer_id)?;
        // block_on bridge already exists in lattice-host
        source.save().block_on()?;
        Some(Effect::Echo(
            format!("Saved {}", source.path()?.display())
        ))
    }
);
```

The handler shadows the global `:w` handler for this buffer only, via the
per-buffer `ActionHandlerRegistry` introduced in M.10.1. The source buffer is
saved, not the narrow view. The narrow view's edits already propagated to the
source buffer async via M.3 / M.11.

`:wa` (write all) fans out across all dirty `path`-bearing buffers in the
registry. Narrow views have no `path()`, so `:wa` skips them. The underlying
source buffers ARE path-bearing and will be saved by `:wa` if dirty. This is
the correct semantic.

---

## 9. Partial-line narrow (N.2 — deferred)

v1 narrows to **whole lines** only. `start_line` is the first line of the
region; `end_line` is the last.

N.2 would add `ExcerptEdgeMode::Strict`:
- A read-only filler prefix of `start.col` bytes on the first row.
- A read-only filler suffix after `end.col` bytes on the last row.
- `build_source_edit` (M.3) maps composed column to source column by
  subtracting the filler offset.

Deferred: tree-sitter scope targets are line-aligned by definition; visual
narrow from Normal/Visual mode targets full lines in the common case; the clip
logic in M.3 is non-trivial. Land when a concrete use case demands sub-line
precision.

---

## 10. Performance posture

| Operation | Cost | Budget |
|---|---|---|
| `scope_at_cursor` (tree query) | O(matches at cursor) ≈ O(1) in practice | < 1 ms p99 |
| `create_narrow_view` | One `MultibufferDocumentHandle::new` + mode activation | < 5 ms p99 |
| Edit propagation in narrow view | M.3 local-first + async source forward | Covered by M.3 gate |
| Recompose on source edit (M.4) | O(1) — one excerpt | Covered by M.4 gate |

No new bench gate. N.1 is a composed primitive; all hot paths are covered by
existing M-series benches. Add `narrow_scope_at_cursor_p99_us` if profiling
reveals tree-sitter overhead > 100 µs p99 on a real codebase.

---

## 11. Rejected alternatives

**Option B — viewport restriction in the renderer.** Clamp `scroll_top` /
`scroll_bottom`; intercept motions that cross the boundary. Rejected:

- Grammar exceptions: `G`, `gg`, `*`, search wrap-around, marks outside the
  narrow range all need special handling. The list of exceptions grows without
  bound.
- Architecture: kind-specific renderer logic violates
  `feedback_buffers_no_special_case`.
- Doesn't compose: parallel narrows would require per-pane clamping state,
  not per-buffer state. Cross-pane coherence breaks.

**Option C — dedicated `NarrowDocument` struct** (custom Document impl wrapping
a source handle, clamp edits to range). Rejected:

- Duplicates edit propagation (M.3), live-update subscription (M.4),
  headerline status (M.6.5), per-excerpt syntax highlighting (K.4.7), fold
  providers (M.7), and excerpt motions (M.2.b.3). That is the entire M-series.
  Heuristic #1 forbids keeping an inferior design because "M was a lot of work."
- Cannot serve as a source buffer in a larger multibuffer (project-diff,
  AI-edits). The one-excerpt multibuffer approach can.

**Option D — open in a pane split instead of a new buffer.** Rejected:

- `:widen` semantics become "unsplit" not "close buffer". Users expect
  `:bn` / `:bp` / `:ls` to list the narrow buffer and `:bd` to close it.
- Pane layout is orthogonal to narrowing: the user may want the narrow view in
  a new window, a side-by-side split, or a replacement of the current pane.
  NarrowMode should not dictate pane layout; `Effect::OpenBuffer` lets the
  host apply whatever pane placement policy is in effect.
