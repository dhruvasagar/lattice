# Tree-sitter context — slice plan

> **Status: Active.** Opened 2026-08-16, branch `dhruva/treesitter-context`.
> TC.1–TC.8 + TC.10 ✅. **NOT archivable:** TC.9 is ⛔
> deferred, and the completed-plans-only rule is explicit that deferred is open
> work, not done. Archiving now is exactly the mistake that rule exists to
> prevent.
> Implements [`../../architecture/treesitter-context.md`](../../architecture/treesitter-context.md)
> — sticky scope headers as a core bundled plugin, plus the two host seams it
> forces.

Design owns *what* and *why*; this file owns *when* and *in what order*.

## Status

| Slice | Title | Status |
|---|---|---|
| TC.1 | `ContextScope` + `resolve_context` in `lattice-cells` | ✅ |
| TC.2 | The `context` WIT seam + host quartet + fixture component | ✅ |
| TC.3a | Scope cache + reparse-driven refresh pump | ✅ |
| TC.3b | Pane-keyed layer — worker, reservation, **both renderers** | ✅ |
| TC.4 | The `theme` WIT seam — plugin-registered elements | ✅ |
| TC.5 | The `treesitter-context` plugin — queries, config, theme elements, bundling | ✅ |
| TC.6 | `treesitter-context-mode` — `[u`, `:context-toggle` | ✅ |
| TC.7 | Docs, benches, ratchet | ✅ |
| TC.8 | `context.line-numbers` — source line numbers in the context gutter | ✅ |
| TC.9 | Buffer-side capability sets — the mode-capability gate is half-built | ⛔ |
| TC.10 | `run-query-ranges` — the large-file fix, in the seam not the guard | ✅ |

## Sequencing

```
TC.1 (pure resolver) ─┐
                      ├─→ TC.3 (layer + renderers) ─→ TC.5 (plugin) ─→ TC.6 (mode) ─→ TC.7
TC.2 (seam + fixture)─┘                                  ↑
TC.4 (theme seam) ───────────────────────────────────────┘
```

TC.1 and TC.2 are independent and can land in either order; TC.1 first is
preferred because it is pure and provable with nothing else in place, so a
resolver bug surfaces as a unit-test failure rather than as a wrong strip.

TC.3 needs both — the resolver to produce line lists, the fixture component to
feed it scopes without a real grammar.

TC.4 is independent of everything before it and could land anywhere. It is
placed before TC.5 so the plugin registers its theme elements the right way the
first time rather than being retrofitted. **If the feature needs to be visible
sooner, TC.4 is the slice to defer** — TC.5 then carries hard-coded style
defaults and a follow-up moves them onto the seam. Say so in the commit if that
path is taken; a deferred TC.4 keeps this plan active.

**Verify before starting TC.6, not during it:** whether `Effect::CursorMoveIn`
and a position-history push both cross the WIT effect mirror
(`boundary_effect.rs`). If either is missing, TC.6 grows by an effect-mirror
addition and should be re-sliced rather than silently widened.

---

## TC.1 — The resolver ✅

> **Landed 2026-08-16.** Three things the design got wrong and the code
> corrected (all three fixed in the design fragment):
>
> 1. **The resolver needs `viewport_top`, not just the anchor.** The design
>    folded "is enclosing" and "has scrolled away" into one predicate,
>    `header_end < anchor`, glossed as "whose header has scrolled past". The
>    gloss was right and the predicate was not: cursor at 30, `impl` header at
>    10, view starting at 5 — the predicate holds while the header is plainly
>    on screen, and pinning it spends a row duplicating a visible line. Two
>    separate steps now, and `ContextOptions` carries `viewport_top`.
> 2. **`Vec<u32>`, not `SmallVec`.** `lattice-cells` is deliberately
>    near-dep-free (its manifest documents the one exception). A dependency is
>    not worth saving one allocation of at most `max_lines` elements.
> 3. **`O(n + d log d)`, not `O(log n + depth)`.** "Which intervals contain
>    line L" is not a binary search — scopes nest, but the siblings before `L`
>    still have to be rejected one at a time. Measured 204 ns / 2.66 µs /
>    21.8 µs at 100 / 5k / 50k scopes; the pathological end is 0.26% of a
>    120 Hz frame. Linear is the right trade today and the bench is the
>    ratchet that says when it stops being.
>
> One test also had to be corrected before it could go green: the
> still-visible case originally asserted `viewport_top: 25` left the `fn`
> header at 20 visible, which is false — 20 is above 25. The view has to start
> *between* the two headers (15) for one to be off-screen and the other on.



`ContextScope { scope_start, scope_end, header_start, header_end }` and
`resolve_context(scopes, anchor, opts) -> Vec<u32>` in
`lattice-cells`. Pure; no host types, no I/O.

Placed in `lattice-cells` rather than `lattice-core` because both renderers and
the cells worker already depend on it and none of them should reach further up
for a geometry primitive.

The algorithm is design §"The resolver, precisely" — scopes enclosing the
anchor, filtered to those whose header is above `viewport_top`, outermost
first, headers expanded to `multiline-threshold` rows, truncated to the row
budget from the `trim-scope` end.

**Tests.** Nesting depth; a scope whose header is still visible is excluded;
trim `outer` and trim `inner`; a multi-line header consuming more than one row
of the budget; anchor exactly on a header line; empty scope list; unsorted
input; scopes that overlap without nesting (malformed query output — must not
panic); the viewport-fraction guard at pane heights 3, 10 and 100.

**Bench.** `resolve_context` at depth 20 over 50k scopes. This is the one piece
of the feature on the keystroke path, so it gets a recorded number from the
start — a later change that makes it `O(scopes)` must fail CI, not review.

## TC.2 — The `context` seam ✅

> **Landed 2026-08-16.** Four things worth recording:
>
> 1. **The tree crosses as a call-scoped `borrow<tree-snapshot>`, and it
>    works.** This is the repo's FIRST `borrow<>` across an async export —
>    every existing one is in the sync grammar world — so it was an open
>    question whether wasmtime would keep a host resource lent across a guest
>    suspension. It does. The alternative, had it not, was a host import
>    handing the guest an *owned* snapshot by buffer id, which widens the
>    `tree-sitter` capability from "the tree you were handed" to "any buffer's
>    tree, any time". The fixture walks the tree for real (one scope per named
>    child of the root, asserted by exact line range) so a dead borrow fails
>    the test rather than silently returning constants.
> 2. **The registry shipped with the seam, not with TC.3.** The plan put
>    `ContextSourceRegistry` in the host slice, but the loader's seam match is
>    deliberately exhaustive ("a new seam variant must add its drain here — the
>    compiler enforces it rather than a silent skip"), and a drain needs
>    somewhere to register. A seam that cannot be drained is not a seam, so the
>    registry, the `drain_context`, the teardown surface, and the boot service
>    registration all belong here. This is the documented one-slice exception:
>    the neighbour was needed to compile.
> 3. **`context-request` does not carry `language` or `parse-version`.** The
>    guest reads the language off the snapshot it was handed (so the two cannot
>    disagree), and the parse version is host-side cache bookkeeping the guest
>    has no use for. It also does NOT reuse `decoration-context` despite the
>    fields coinciding today — sharing would make a field one seam needs into
>    ABI churn for the other.
> 4. **`AsyncContextSource::produce` takes the snapshot type-erased** as
>    `Option<Arc<dyn Any + Send + Sync>>`, the `ActionContext::syntax`
>    precedent, so `lattice-mode` stays free of `lattice-syntax` and the
>    plugin-host adapter does the downcast.


`wit/context.wit` (`context` interface + `context-plugin` world),
`context-request` / `context-scope` records in `wit/types.wit`, and the host
quartet mirroring the decoration one:

- `context_source.rs` — `WasmContextSource`, the adapter implementing the
  native `AsyncContextSource` (which lives in `lattice-mode`).
- `context_task.rs` — the per-plugin actor bridge; lends the tree as a
  call-scoped borrow and reclaims the table entry on every path.
- `context_host.rs` — the 7th `bindgen!`, reusing the grammar world's
  `tree-sitter` module so the host resources are not minted twice.
- `boundary_context.rs` — round-trip type mirror, reusing the `plugin` world's
  generated `types` via `with:` so crossed values are the same Rust types.

Gated on the existing `tree-sitter` editor capability. No new capability — the
seam is a place to *return* structure, not a new source of it.

Per-buffer `Arc<ContextScopes>` cache stamped with the parse version, published
via `ArcSwap`, woken through **`SubsystemBoot::inbound`**. Not a bare
`TickCallback`: the failure mode is a strip that only updates when the user
happens to press a key, and it reads like a rendering bug.

**Fixture component.** A WAT/Rust guest returning canned scopes, plus a
trapping variant. Both live under the existing plugin-host test fixtures.

**Tests.** Round-trip fidelity of `context-scope` across the boundary; the
cache is keyed by parse version and a stale response for a superseded parse is
dropped; a guest `err` leaves the previous scopes in place (not blanked); a
trapping guest leaves the previous scopes in place; the capability gate refuses
a component without `tree-sitter`; **the async result lands with no intervening
keypress**.

## TC.3a — Scope cache + refresh pump ✅

> **Landed 2026-08-17. Re-sliced:** TC.3 as planned bundled the producer
> drive, the per-pane layer, the reservation and both renderers into one
> commit. The drive is independently testable with no UI at all — a native stub
> `AsyncContextSource` exercises the whole cache mechanism — so it lands first
> and TC.3b keeps the renderer-lockstep bundle it actually needs.
>
> `wasm_context.rs` mirrors `wasm_decorations.rs` with **one deliberate
> difference: the staleness key is the PARSE version, not the document
> version.** Decorations are per-line marks tied to text; scopes describe the
> tree. Keying scopes on the document version would re-drive the guest on every
> keystroke — precisely the WASM-on-the-hot-path the scopes-not-rows split
> exists to prevent — and would also blank the strip during the window between
> an edit and its reparse.
>
> Five tests, including the async-landing one the standing rule requires
> (scopes reach the screen with no intervening keypress). One test comment was
> corrected after checking it: `a_second_refresh_…_does_not_re_drive_the_producer`
> pins the OUTCOME, and TWO independent guards deliver it (parse-version and
> single-flight), so it stays green with either one defeated. Verified by
> defeating each in turn; the comment now says so, because a green run there is
> not evidence that a guard someone just deleted was dead code.

## TC.3b — The layer + both renderers ✅

> **Landed 2026-08-17.** One commit, per the lockstep rule.
>
> - **Pane-keyed, and it is the only layer that is.** `sticky_contexts` is
>   keyed by `PaneId` where every sibling (`display_matrices`, `indent_guides`,
>   `cells_matrices`) is keyed by `BufferId`. `IndentGuides` gets away with
>   buffer keying by publishing extents and letting each renderer pick the
>   active block from its own cursor; context cannot, because the ROWS differ
>   per pane. `two_panes_on_one_buffer_resolve_different_context` is the only
>   test that fails if this regresses.
> - **The rows are built in the worker, not copied by the renderers.** A
>   context header is by definition above the viewport, and `CellMatrix` is
>   chunked above `4 × viewport_height`, so a header thousands of lines up is
>   routinely not resident. `SyntaxSnapshot::highlight_lines` gives the worker
>   real colour for any line; a renderer copying from the published matrix
>   would have found nothing and fallen back to unhighlighted text.
> - **The reservation reads the LAST published count, not a fresh resolve.**
>   Re-resolving in `ensure_cursor_visible` would use the pre-clamp scroll to
>   predict a strip the post-clamp scroll may not produce. Reading what is
>   currently painted is the only self-consistent choice; the publish path
>   stays authoritative, resolving once into a list both the worker and the
>   renderers read.
> - **Both peers adapt `StickyContextRow` to `VirtualRow`** and reuse
>   `render_virtual_row` / `push_virtual_row` rather than gaining a second
>   painter each — two implementations of "paint a pinned row of cells" across
>   two renderers is four things to keep in step.
>
> Context-gutter line numbers (`context.line-numbers`) are deferred to TC.5
> with the option that governs them; the strip currently paints a blank gutter
> exactly as every other virtual row does.



**One commit.** The TUI/GPUI lockstep rule is not negotiable here: a sticky
strip that exists in one peer and not the other is a visible divergence in the
feature's entire surface.

- `Editor::sticky_context_for(pane_id)` — a new **pane-keyed** ArcSwap map.
  Deliberately not `buffer_id`: two panes on one buffer need different rows.
- `PaneCellsInputs` gains `sticky_context_lines: SmallVec<[u32; 8]>`, resolved
  host-side on each pane-inputs publish.
- Scroll model reserves `sticky_context_lines.len()` on top of the existing
  `sticky_count` (`dispatch.rs`).
- Cells worker builds one row per line, in the pass that builds `DisplayMatrix`
  and `IndentGuides`, from the same snapshot with the same `MatrixVersion`.
  Skipped when the resolved list is unchanged.
- TUI `render.rs`: paint after the matrix sticky pre-pass.
- GPUI `editor_element.rs`: same order, same theme keys, plus the `host_theme`
  propagation for the new elements.

End-of-slice audit shortcut — an empty result means GPUI was missed:

```
grep -rn "sticky_context" crates/lattice-ui-gpui/ --include="*.rs"
```

**Tests.** Reservation count equals published line-list length across a matrix
of cases; scroll geometry with headerline and context both present; the strip
order is headerline-then-context and the headerline is never displaced; when
the headerline provider returns `None` context starts at row 0; **one buffer,
two panes, different cursors → different context** (the pane-keying proof — write
this one first, it is the only test that fails if the keying regresses);
context row cells are **identical** to the source rows' cells (the
highlighting-preservation proof); a header line outside the resident chunk
still renders highlighted (the chunking case that rules out renderer-side
resolution).

**Bench.** Worker context-row build cost; and the per-keystroke delta with the
layer active vs. disabled, so the ratchet sees it.

## TC.4 — The `theme` seam ✅

> **Landed 2026-08-17.** Closes `theme-system.md`'s deferred WIT
> element-registration item, which was designed there and waited for a real
> consumer.
>
> - **`family` and `weight` are deliberately absent from the WIT `style-spec`.**
>   `family` is an interned `FamilyId` a plugin cannot produce — crossing it
>   needs a name-to-id interning contract nothing has asked for — and `weight`
>   is a variable-font axis whose only users are native heading treatments.
>   Shipping half-designed fields to "size the ABI" is worse than adding them
>   when something needs them; the WIT is explicitly unstable until three real
>   plugins have exercised it.
> - **Unregistering tombstones the slot rather than deleting it.** `ElementId`
>   is an index into the element vector, so removing an entry would silently
>   re-point every later id at the wrong element. `unregister_element` drops
>   the NAME binding only — `id`, `describe` and `element_names` all resolve
>   through `by_name`, so the element vanishes from every observable surface
>   while ids already handed out stay valid. User overrides are left in place:
>   a reload should not discard the user's customisation.
> - **Plugin docs are leaked to `&'static str`.** The registry's doc field is
>   `&'static`, a plugin's doc is runtime data, and elements are declared once
>   at load. Bounded by the element count of loaded plugins (tens), so it is a
>   one-time cost rather than a growing leak — the alternative was widening a
>   native field for a case only plugins have.



`wit/theme.wit` — `register-element(name, doc, default: style-spec)` — plus
`theme_host.rs` mirroring `config_host.rs`. Elements insert into the same
registry builtins live in, under `SourceLayer::Plugin(id)` so unload reverses
them.

Closes the deferred WIT element-registration item in
[`../../architecture/theme-system.md`](../../architecture/theme-system.md).

**Tests.** A plugin-registered element resolves through the normal lookup path;
a theme file overrides it; `:customize` and `:describe-*` list it; unload
removes it; a duplicate name from a second plugin is refused with a clear
error; a malformed `style-spec` is refused without poisoning the registry.

## TC.5 — The plugin ✅

> **Landed 2026-08-17.** The slice where the feature stops being scaffolding:
> a real tree-sitter query runs inside WASM against a real parse tree.
>
> - **The header span comes from the node's `body` field**, not a second
>   `@context.end` capture. A scope's header is everything before its body, so
>   `fn f(\n  a: u32,\n) {` is three header lines. Deriving it from `body`
>   gives multi-line signatures for free and needs no per-language bookkeeping
>   — every query would otherwise have to get its own end-capture right.
> - **Branch arms are captured deliberately** (`if` / `else` / `match_arm` /
>   `case`). Knowing which branch you are inside is exactly what a long
>   function hides, and it is the case folds cannot serve because nobody folds
>   an `if`. This is also why scopes are not folds.
> - **Single-line scopes are dropped in the guest.** Their header can never
>   scroll away while the cursor is inside them, so caching them would only
>   lengthen the host's per-keystroke scan.
> - **The query is compiled per call, not cached.** The guest has no per-language
>   slot that survives a call, and this runs once per REPARSE — never per
>   keystroke, scroll, or frame. A cache would matter only if the producer were
>   re-driven more often, which the scopes-not-rows split exists to prevent.
> - Seven languages: rust, python, go, javascript, typescript/tsx, c/cpp,
>   markdown. Staged into `runtime/plugins` by `cargo xtask
>   build-core-plugins`, which now names two core plugins.



`plugins/treesitter-context/` — standalone workspace, `wasm32-wasip2`,
`crate-type = ["cdylib"]`, the `auto-pair` shape. Built to a component by
`lattice-plugin-host/build.rs`.

`plugin.toml`:

```toml
id = "treesitter-context"
provides = ["config", "theme", "context"]
editor_capabilities = ["tree-sitter"]
default_mode = "treesitter-context-mode"
```

`provides` order matters — the same constraint `auto-pair` documents: later
seams bind against names earlier ones registered.

- `queries/<lang>/context.scm` for Rust, Python, Go, JavaScript, TypeScript, C,
  Markdown. Embedded with `include_str!`; `@context` marks a pinned node,
  optional `@context.end` narrows the header span.
- The ten `context.*` options (design §"Configuration").
- The four `context.*` theme elements (design §"Theme elements").
- Compile each query once, cache by language; a compile failure logs `warn`
  once per (plugin, language) and that language contributes nothing.

Bundle into `runtime/plugins/treesitter-context/` alongside `auto-pair`.

**Tests.** Each shipped query compiles against its grammar; a representative
file per language produces the expected scopes (fixture files, asserted line
numbers); a language with no query contributes nothing and logs at `debug`, not
`warn`; a file over `max-file-lines` is skipped; `context.disabled-languages`
is honoured; `context.enabled = false` publishes an empty list rather than
skipping the publish, so turning it off clears the strip instead of freezing it.

## TC.6 — The mode, the chord, the commands ✅

> **Landed 2026-08-17.** The pre-flight check the plan mandated paid off in an
> unexpected direction, and one designed command did not survive.
>
> - **The effect mirror was already sufficient.** The plan flagged
>   `Effect::CursorMoveIn` + a position push as possibly missing. Neither is
>   needed: `record-jump` and `cursor-move` both already cross, round-tripped
>   and tested. `CursorMoveIn` is explicitly refused at the boundary and is the
>   wrong tool anyway — it exists for ASYNC producers whose target buffer may
>   no longer be focused, whereas `[u` resolves inline on the keystroke path
>   where the active buffer IS the target.
> - **`:context-up` was dropped.** `apply-ex-command` gets no tree handle (only
>   `apply-action` does) and no `Effect` re-dispatches a command, so an
>   ex-command cannot compute a jump target. Shipping one that silently did
>   nothing would be worse than not having it. `:context-toggle` survives
>   because it only reads and writes an option. Recorded in the design fragment
>   with the fix if a second consumer ever appears.
> - **How the bug surfaced is worth keeping.** Registering an action AND an
>   ex-command both named `context-up` made `id_by_name` resolve the
>   ex-command — whose body was a stub — so the chord silently did nothing. The
>   name collision hid the seam gap behind a plausible no-op.
> - **A multi-seam component must satisfy EVERY import on every linker it is
>   instantiated against.** Adding `theme` to the world broke loading outright
>   until `theme` was also added to the SYNC grammar linker, because the
>   grammar seam instantiates the same component there.



`treesitter-context-mode`, a **minor** mode with an `ActivationPolicy` spanning
every major with a tree-sitter grammar. Keymap at
`KeymapLayer::MinorMode(treesitter-context-mode)` — never `Builtin`.

- `[u` → `context-up`, count-aware.
- `:context-up` — the same handler, so chord and command cannot drift.
- `:context-toggle` — flips `context.enabled` buffer-locally.
- Handler body lives in the plugin. Targets the header of the innermost scope
  with `header_end < cursor_line`; pushes
  `push_position_history(cursor, PositionSource::PluginPush)` before moving, so
  `<C-o>` unwinds.

**Tests.** `[u` from inside a nested scope lands on the innermost enclosing
header; repeated `[u` walks outward and terminates at top level without
looping; a count jumps N levels and a count past the top clamps rather than
erroring; `<C-o>` returns to the pre-jump position and `<C-i>` re-does it; `[u`
in a buffer with no tree-sitter grammar is inert because the minor is not
active there; `:context-toggle` clears and restores the strip; the chord is
absent from `KeymapLayer::Builtin` (the regression that matters — a `[u` firing
in every buffer).

## TC.7 — Docs, benches, ratchet ✅

> **Landed 2026-08-17.** `docs/user/core-plugins.md` gains the plugin's own
> section (chord, options, languages, theme elements) and its row in the core
> table; `benchmarks.md` gains the TC.1 resolver numbers as the ratchet, with
> the linear shape stated so a later superlinear change fails there rather than
> in review.
>

## TC.9 — Buffer capability sets ⛔

**Found by running the editor, not by a test.** `treesitter-context-mode`
declared `TREE_SITTER` and failed to activate on every buffer:

```
WARN mode: activate(treesitter-context-mode) for buffer 9 failed:
     mode `treesitter-context-mode` requires capabilities
     `CapabilitySet(TREE_SITTER)` that the buffer lacks
```

The gate is half-built editor-wide. `ModeRegistry` enforces
`required_capabilities() - buffer_caps`, but **nothing populates the buffer
side**: every activation site in `lattice-host` passes `CapabilitySet::empty()`,
and no native mode had ever declared a requirement, so the enforcement half had
never been exercised. Any mode declaring any capability is unsatisfiable today.

This plugin works around it by declaring none (correct for it independently —
see the design fragment). But the next mode that declares one will hit the same
wall, and the failure is a `warn` at activation rather than anything that fails
a build or a test.

Closing it means deciding when a buffer gains each capability (`TREE_SITTER` at
first parse? at open with a known grammar? `LSP` at attach?) and re-running
activation when the set changes — a real feature with real timing questions,
which is why it is not folded into a context slice.

**No test guards this beyond the plugin's own** `required_capabilities() ==
empty()` assertion, which catches only a regression in this plugin.

## TC.8 — `context.line-numbers` in the gutter ✅

Landed in two commits, because the option could not reach the gutter until
options reached the host at all.

### TC.8a — the options were inert

`resolve_sticky_context_lines` built `ContextOptions::default()` and never read
the registry. Every knob the plugin registers — `max-lines`, `trim-scope`,
`multiline-threshold`, `max-viewport-fraction` — showed up in `:customize`,
answered `:set …?` with a value, and changed nothing. Worse than absent: the
editor reported a setting it was ignoring.

Found while wiring TC.8, which needs the same read for `line-numbers`.
Shipping one option honoured while its four neighbours stayed inert would have
been the odd state, so the read covers all of them.

Cached on `WasmContextState`, not read at use: the resolver runs at cursor rate
for every pane and `ConfigRegistry` reads take a `Mutex`. The refresh sits
BEFORE the producer gate — a plugin registers its options and its producer in
one load, and the order between them is not ours to rely on. Each option falls
back individually (five of six registered → five honoured), and an
unrecognised `trim-scope` keeps the default rather than erroring, because a
typo must not take the strip away.

Adds `ConfigRegistry::get_string_by_name`, the missing third of the by-name
trio, for dynamically-registered options that have no decl type to import.

### TC.8b — the gutter hook

`VirtualRow` gains `gutter_line: Option<u32>` — a GENERIC field, not a
sticky-row special case, so any producer that knows a real line number gets the
document gutter for free and neither renderer branches on `kind`. It is
deliberately separate from `anchor_line`: anchoring answers "where does this
row sit", `gutter_line` answers "what number does it show", and for most
virtual rows the honest answer is nothing (a deletion block has no current-side
line; a filler row has no line at all). A sticky context row is the case where
they differ the other way — anchored above the viewport, showing its own place
in the file.

`StickyContext` carries the resolved `line_numbers` flag beside `bg`, for the
same reason `bg` is resolved host-side: the strip is host chrome, so neither
renderer reads a plugin option. Both peers then reduce to one expression, which
is hard to get differently wrong in two places. The worker's early-return
comparison includes the flag, or toggling it would not repaint until the lines
themselves changed.

Two rules fell out of building it:

- **The pane's `number` wins.** `context.line-numbers` asks for numbers;
  `:set nonumber` says this pane shows none. A strip numbered above unnumbered
  code is a stray column. Enforced at both adapters, plus a width guard in the
  GPUI helper (`gutter_width == 0` is `nonumber` — there is no digit slot).
- **A number wider than the gutter is not truncated.** It costs a column of
  alignment for one frame (the gutter widens with the file's line count); a
  truncated line number is simply wrong.

The TUI gutter is built by calling `render_gutter` — the document formatter —
rather than formatting digits locally, so the two cannot drift; the test
asserts against `render_gutter`'s output, not a literal, because the property
that matters is "occupies the same columns as the code beneath", not "shows a
number". GPUI shapes a string, so its width invariance is asserted directly.

Also fixed here: `window.rs` had `sticky_context` set TWICE on the docs-popup
pseudo-pane and MISSING on the help-popup one, so `--features window` had not
compiled since TC.3b. The default TUI build does not put `lattice-ui-gpui` in
the dependency graph, which is exactly the blind spot the lockstep rule exists
for — the parity edit landed, the build that would have caught the typo never
ran.



- `docs/user/` entry for the feature: what it shows, the options, `[u`,
  `<C-o>`.
- `benchmarks.md` section with the TC.1 and TC.3 numbers.
- CI ratchet entries for the resolver and the worker build.
- Fix any status icons in this plan that drifted during the build, then decide
  archival. **Not archived while any slice is 📝 or ⛔** — including TC.4 if it
  was deferred.

---

## Notes

- **One slice, one commit.** TC.3 is the single deliberate multi-file commit
  (host layer + both renderers), because the lockstep rule forbids splitting
  it.
- Each slice runs `scripts/precommit.sh <touched-crate>...` to completion before
  committing — not a filtered subset, and not beside another cargo job.


## TC.10 — `run-query-ranges`: the large-file fix ✅

**The report.** The strip did not appear on `dispatch.rs` (36k lines). It was
not a rendering bug: `max-file-lines` was skipping the query at 5 000 lines,
because past ~20k the producer TRAPPED and a trap quarantines the plugin for
every buffer until reload. The guard was correct given the cost; the cost was
the defect.

**Why the cost was where it was.** `run-query` mints one `node` RESOURCE per
capture — a host table entry with its own snapshot bump and a guest-side drop —
and the producer then made two further host calls per capture (`byte-range`,
`child_by_field("body")`). A whole-file structural query has tens of thousands
of captures, so the boundary traffic, not the traversal, dominated and the call
went superlinear.

**The fix, in the seam.** `run-query-ranges` returns `capture-range { name,
match-index, range }` — extents, no resources — with the same host-side
predicate evaluation and the same graceful-empty on a grammar mismatch.
`match-index` groups captures from one pattern match, which is what lets a
query capture a construct AND its body (`@context` + `@context.end`) and the
guest pair them in one linear scan, with no containment test (ambiguous for
directly-nested constructs) and no second query.

The header derivation is unchanged; only where the body position comes from
changed — from a guest-side field lookup to a query capture. That is a return
to what the design fragment specified all along; the field lookup was a
build-time shortcut that traded per-language query bookkeeping for per-capture
boundary cost, and the trade was much worse than it looked.

**Measured** (Rust, release wasm, `dispatch.rs` and multiples of it):

|   lines | `run-query` | `run-query-ranges` |
|--------:|------------:|-------------------:|
|   1 000 |       25 ms |             4.6 ms |
|   2 500 |       83 ms |             6.0 ms |
|   5 000 |      287 ms |             9.0 ms |
|  10 000 |      1.18 s |              15 ms |
|  20 000 |        TRAP |              28 ms |
|  36 000 |        TRAP |              52 ms |
| 100 000 |           — |             135 ms |
| 400 000 |           — |             534 ms |

Linear (~1.4 us/line) with no cliff. `max-file-lines` accordingly moves 5 000 →
100 000: it now bounds background work per reparse instead of keeping users
away from a trap, and `0` (unlimited) becomes a defensible setting.

**Tests.**

- `tree_resource.rs` — the ranges API reports the SAME extents as the node API
  (two derivations of "where is this capture" is drift the plugin would
  silently inherit), and captures group by match.
- `treesitter_context_queries.rs` — every bundled query compiles against its
  REAL grammar. Load-bearing: `@context.end` is written against per-language
  field names, tree-sitter compiles all-or-nothing, and a wrong field name
  disables the strip for that language while looking exactly like "this
  language has no query".
- `treesitter_context_plugin.rs` — a wrapped signature still yields a
  multi-line header (the behaviour the switch had to preserve), and the
  large-file test INVERTS: `dispatch.rs` must now produce real context under
  the guard rather than be skipped by it.

**Files.** `wit/tree-sitter.wit`, `lattice-plugin-host/src/{tree_resource,lib}.rs`,
`plugins/treesitter-context/{src/lib.rs,queries/*.scm}`,
`docs/dev/architecture/treesitter-context.md`.
