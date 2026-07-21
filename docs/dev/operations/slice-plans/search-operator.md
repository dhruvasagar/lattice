# `g/` search operator — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons. Design
> fragment (contracts, rationale, rejected alternatives):
> [`../../architecture/search-operator.md`](../../architecture/search-operator.md).

Status icons: ✅ done · 🚧 in progress · 📝 planned. Every non-trivial slice
ships the four artefacts (doc / test / bench-or-note / graceful error handling).

**Status: 📝 all planned.** A native `g/` operator that extracts a motion /
text-object span and fires project search for it, reusing the existing
`AppEffect::SearchTrigger` seam.

| Slice | Status | Summary |
|---|---|---|
| SO.1 | 📝 | Grammar: `operator_search` (read-only) + `register_operator` + `Builtins.search` field; emits `Effect::AppAction(AppEffect::SearchTrigger { query })`. Grammar unit tests (`iw` / `i"` / visual → correct query; empty range → `Effect::None`). |
| SO.2 | 📝 | Host wiring: 9th `absorb_operator_search` operator-prefix action + `[g, /]` Builtin binding. Host test: chord arms operator-pending and the following motion routes to `SearchTrigger`. |
| SO.3 | 📝 | User docs: `docs/user/project-search.md` gains a `g/` section + use-case table, cross-ref `modal-editing.md`; flip design fragment + this plan to ✅. |

---

## SO.1 — grammar operator  📝

**Design:** fragment §1–§2. **Change:** `crates/lattice-grammar/src/builtins.rs`
— add `operator_search(ctx: &mut OperatorContext) -> Result<Effect, CommandError>`
(mirrors `operator_yank`'s read-only shape at ~:2178): early-return
`Effect::None` on empty/whitespace-only range; else
`let query = ctx.document.buffer().slice(ctx.range)?;` →
`Ok(Effect::AppAction(AppEffect::SearchTrigger { query }))`. Register via
`registry.register_operator("operator:search", "…(vim-ish `g/`)…", OperatorSpec
{ repeatable: false, apply: Arc::new(operator_search), args_schema: vec![],
blockwise_per_row: false })`, add `search` to the struct-construction list
(~:605) and a `pub search: OperatorId` field on `Builtins` (~:676).

**Artefacts:** *test* — in the `builtins.rs` `#[cfg(test)]` module: `execute(&r,
&mut doc, BufferId(0), cursor, CommandInvocation::of(b.search.0)
.with_target(Target::TextObject(b.inner_word, Args::None)), &cancel)` yields
`Effect::AppAction(AppEffect::SearchTrigger { query })` with the word text;
repeat for `inner_quote_double`; empty-range case → `Effect::None`. *bench* —
none (O(range) slice; covered by the existing operator bench — noted, not added).
*error handling* — empty-range no-op; `slice` error propagates as
`CommandError` (no panic). **Deps:** none.

## SO.2 — host wiring (action + binding)  📝

**Design:** fragment §3. **Change (two files):**
- `crates/lattice-host/src/actions.rs` — add `pub absorb_operator_search:
  CommandId` field (~:174) and register it via `register_operator_prefix(registry,
  "action:absorb-operator-search", "Vim-ish `g/`: arm operator-pending for
  project-search.", builtins.search)` (~:1030, next to the case operators).
- `crates/lattice-host/src/keymap_normal.rs` — `handle.bind(layer, mode,
  &[g.clone(), lit_char('/')], CommandInvocation::of(actions.absorb_operator_search),
  source())` next to the `g~` binding (~:484). Verified: `/` is **not** an
  existing `g`-prefix second key — no conflict.

**Artefacts:** *test* — host test (mirror an existing operator-pending dispatch
test): feed `g`, `/`, then a motion; assert the resolved invocation targets the
search operator and the produced effect is `SearchTrigger { query }` for the
motion span (or, at minimum, that `[g, /]` resolves to `absorb_operator_search`
and arms operator-pending). *bench* — none. *error handling* — inherits SO.1's
no-op; a motion that resolves no range is the existing operator-pending no-op.
**Deps:** SO.1.

## SO.3 — user docs  📝

**Change:** `docs/user/project-search.md` — add a "Search operator (`g/`)"
section with the use-case table from the design fragment §1, noting literal /
case-insensitive semantics and the single-line-text-object intent; cross-ref
`docs/user/modal-editing.md` (where `/` and `*` live) so the `/` ↔ `g/`
relationship is discoverable. Flip
[`../../architecture/search-operator.md`](../../architecture/search-operator.md)
and this plan to ✅. **Deps:** SO.1, SO.2.

---

## Sequence

```
SO.1 (grammar operator) ─► SO.2 (host action + g/ binding) ─► SO.3 (docs)
```

## Cross-references

- Design contracts: [`../../architecture/search-operator.md`](../../architecture/search-operator.md).
- Project-search path driven: [`../../architecture/multibuffer-views.md`](../../architecture/multibuffer-views.md) §3.7.
- Grammar/operator machinery: [`../../architecture/design.md`](../../architecture/design.md) §5.2.
