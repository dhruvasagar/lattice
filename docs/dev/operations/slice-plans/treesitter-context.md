# Tree-sitter context — slice plan

> **Status: Active.** Opened 2026-08-16, branch `dhruva/treesitter-context`.
> Implements [`../../architecture/treesitter-context.md`](../../architecture/treesitter-context.md)
> — sticky scope headers as a core bundled plugin, plus the two host seams it
> forces.

Design owns *what* and *why*; this file owns *when* and *in what order*.

## Status

| Slice | Title | Status |
|---|---|---|
| TC.1 | `ContextScope` + `resolve_context` in `lattice-cells` | ✅ |
| TC.2 | The `context` WIT seam + host quartet + fixture component | 📝 |
| TC.3 | Pane-keyed sticky-context layer — worker, reservation, **both renderers** | 📝 |
| TC.4 | The `theme` WIT seam — plugin-registered elements | 📝 |
| TC.5 | The `treesitter-context` plugin — queries, config, theme elements, bundling | 📝 |
| TC.6 | `treesitter-context-mode` — `[u`, `:context-up`, `:context-toggle` | 📝 |
| TC.7 | Docs, benches, ratchet | 📝 |

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
`resolve_context(scopes, anchor, opts) -> SmallVec<[u32; 8]>` in
`lattice-cells`. Pure; no host types, no I/O.

Placed in `lattice-cells` rather than `lattice-core` because both renderers and
the cells worker already depend on it and none of them should reach further up
for a geometry primitive.

The algorithm is design §"The resolver, precisely" — enclosing scopes whose
`header_end < anchor`, outermost first, headers expanded to
`multiline-threshold` rows, truncated to `max-lines` rows from the
`trim-scope` end, then the viewport-fraction guard.

**Tests.** Nesting depth; a scope whose header is still visible is excluded;
trim `outer` and trim `inner`; a multi-line header consuming more than one row
of the budget; anchor exactly on a header line; empty scope list; unsorted
input; scopes that overlap without nesting (malformed query output — must not
panic); the viewport-fraction guard at pane heights 3, 10 and 100.

**Bench.** `resolve_context` at depth 20 over 50k scopes. This is the one piece
of the feature on the keystroke path, so it gets a recorded number from the
start — a later change that makes it `O(scopes)` must fail CI, not review.

## TC.2 — The `context` seam 📝

`wit/context.wit` (`context` interface + `context-plugin` world),
`context-request` / `context-scope` records in `wit/types.wit`, and the host
quartet mirroring the decoration one:

- `context_source.rs` — the native `ContextSource` trait the WASM wrapper
  implements.
- `context_task.rs` — the debounced off-thread driver; cancels in-flight work
  for a superseded parse (`cancellation.md`).
- `context_host.rs` — registry insert under `SourceLayer::Plugin(id)`.
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

## TC.3 — The layer + both renderers 📝

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

## TC.4 — The `theme` seam 📝

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

## TC.5 — The plugin 📝

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

## TC.6 — The mode, the chord, the commands 📝

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

## TC.7 — Docs, benches, ratchet 📝

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
