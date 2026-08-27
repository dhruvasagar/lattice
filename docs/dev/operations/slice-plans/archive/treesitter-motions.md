# Tree-sitter Structural Motions — Implementation Plan (slice plan)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add tree-sitter-backed next/previous structural motions (`]f [f ]F [F`, `]c [c ]C [C`, `]a [a ]A [A`, `]l [l ]L [L`) that jump between functions / classes / parameters / loops and compose with every operator, count, and Visual selection.

**Architecture:** Extend the existing `ScopeResolver` trait (currently text-object-only) with a `scope_toward(...)` navigation query; add a `scope_resolver` field to `MotionContext` and thread the buffer's `SyntaxSnapshot` into it exactly as the text objects already do. `lattice-syntax` owns the motions (new `motions.rs`, mirroring `text_objects.rs`); the host binds the 16 chords in the Builtin keymap layer. No new `.scm` files — reuses the `@function.outer` / `@class.outer` / `@parameter.outer` / `@loop.outer` captures.

**Tech Stack:** Rust, tree-sitter (`QueryCursor::set_byte_range`), ropey, the `lattice-grammar` command registry.

**Design fragment:** [`../../../../architecture/treesitter-motions.md`](../../../../architecture/treesitter-motions.md) — the *what* and *why* (semantics tables, resolver-seam rationale, rejected alternatives). This plan is the *when* and *how*.

## Global Constraints

- **Paramount #1 (perf):** `scope_toward` runs on the core/actor thread on a deliberate keypress — NEVER in `Render::render`, never per-frame. Bound the tree query with `QueryCursor::set_byte_range` to the relevant half of the file. Verified by a bench in TSM.5.
- **Paramount #3 (extensibility):** `lattice-grammar` stays tree-sitter-free — the trait lives there, the impl lives in `lattice-syntax`. No `grammar → syntax` dependency.
- **Graceful failure (heuristic #5):** no tree / no match / count overshoot → the motion returns `from` unchanged (no-op); never panic, never edit.
- **Four artefacts ship together:** design doc (done, TSM.0), tests (each code slice), bench (TSM.5), graceful error handling (TSM.2/3).
- **Mode ownership:** motions register at `KeymapLayer::Builtin` (universal vim grammar, like the tree-sitter text objects) — NOT a MinorMode. The `SyntaxMotionIds` are minted by `lattice-syntax`; the host only wires chord → id.
- **Commits:** conventional format, `feat(ts-motions): TSM.N — …`. Attribution is disabled globally — no `Co-Authored-By` trailer.
- **TUI/GPUI parity:** this slice touches grammar + keymap + syntax only — no renderer match arms — so no GPUI peer edit is required. (If TSM.4 ends up touching any `lattice-ui-tui` render path, mirror it in `lattice-ui-gpui` in the same slice.)

---

## File Structure

| File | Responsibility | Slice |
|---|---|---|
| `crates/lattice-grammar/src/registry.rs` | `NavDir`/`NavBoundary` enums; `scope_toward` on `ScopeResolver`; `scope_resolver` field on `MotionContext` | TSM.1 |
| `crates/lattice-grammar/src/dispatcher.rs` | thread `env.scope_resolver` into the 3 `MotionContext` build sites | TSM.1 |
| `crates/lattice-grammar/src/lib.rs` | re-export `NavDir`/`NavBoundary` | TSM.1 |
| `crates/lattice-runtime/src/actor.rs` | update `MockResolver` impl | TSM.1 |
| `crates/lattice-multibuffer/src/lib.rs` | update `ComposedScopeResolver` impl (delegate) | TSM.1 |
| `crates/lattice-syntax/src/syntax.rs` | `SyntaxSnapshot::scope_toward` real tree walk | TSM.2 |
| `crates/lattice-syntax/src/motions.rs` | **new** — `register_syntax_motions` + `SyntaxMotionIds` (16 `MotionSpec`s) | TSM.3 |
| `crates/lattice-syntax/src/lib.rs` | `pub mod motions;` + re-exports | TSM.3 |
| `crates/lattice-host/src/editor_boot.rs` | call `register_syntax_motions` at boot | TSM.4 |
| `crates/lattice-host/src/keymap_normal.rs` | `syntax_motion_rows` table + Normal & op-pending binding | TSM.4 |
| `crates/lattice-host/src/keymap_visual.rs` | Visual-mode binding | TSM.4 |
| `crates/lattice-host/benches/…` | `scope_toward` bench | TSM.5 |
| `BENCHMARKS.md`, `docs/dev/operations/implementation.md` | ledger + status | TSM.5 |

---

## TSM.0 — Land the design docs ✅

**Files:**
- Commit: `docs/dev/architecture/treesitter-motions.md` (design fragment, already written)
- Commit: `docs/dev/operations/slice-plans/treesitter-motions.md` (this file)

- [ ] **Step 1: Verify both docs exist and are complete**

Run: `ls -la docs/dev/architecture/treesitter-motions.md docs/dev/operations/slice-plans/treesitter-motions.md`
Expected: both files present, non-empty.

- [ ] **Step 2: Commit the docs**

```bash
git add docs/dev/architecture/treesitter-motions.md docs/dev/operations/slice-plans/treesitter-motions.md
git commit -m "docs(ts-motions): TSM.0 — design fragment + slice plan for tree-sitter structural motions"
```

---

## TSM.1 — Grammar seam: `scope_toward` + `MotionContext.scope_resolver` ✅

Adds the navigation query to the trait and the resolver field to the motion
context, threading `env.scope_resolver` through the dispatcher. Ships GREEN with
a **stub** `scope_toward` on every impl (returns `None`) — no behavior yet.

**Files:**
- Modify: `crates/lattice-grammar/src/registry.rs`
- Modify: `crates/lattice-grammar/src/dispatcher.rs`
- Modify: `crates/lattice-grammar/src/lib.rs`
- Modify: `crates/lattice-runtime/src/actor.rs`
- Modify: `crates/lattice-multibuffer/src/lib.rs`
- Test: `crates/lattice-grammar/src/dispatcher.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Produces: `pub enum NavDir { Forward, Backward }`, `pub enum NavBoundary { Start, End }` in `lattice_grammar`; `ScopeResolver::scope_toward(&self, line: u32, col_byte: u32, suffix: &str, dir: NavDir, boundary: NavBoundary, count: u32) -> Option<lattice_protocol::Position>`; `MotionContext.scope_resolver: Option<&'a dyn ScopeResolver>`.

- [ ] **Step 1: Write the failing test (motion sees the resolver)**

Add to `crates/lattice-grammar/src/dispatcher.rs` `#[cfg(test)]` module:

```rust
#[test]
fn motion_context_carries_scope_resolver() {
    use crate::registry::{NavBoundary, NavDir, ScopeResolver};

    struct FixedResolver;
    impl ScopeResolver for FixedResolver {
        fn scope_at(&self, _l: u32, _c: u32, _s: &str) -> Option<lattice_protocol::position::Range> {
            None
        }
        fn scope_toward(
            &self, _l: u32, _c: u32, _s: &str,
            _d: NavDir, _b: NavBoundary, _n: u32,
        ) -> Option<lattice_protocol::Position> {
            Some(lattice_protocol::Position::new(7, 0))
        }
    }

    // A motion whose apply forwards to the resolver and returns its target.
    let mut reg = CommandRegistry::new();
    let m = reg.register_motion(
        "motion:test-nav",
        "test",
        crate::registry::MotionSpec {
            jump: false,
            exclusive: true,
            args_schema: Vec::new(),
            apply: Box::new(|ctx| {
                let p = ctx
                    .scope_resolver
                    .and_then(|r| r.scope_toward(ctx.from.line, ctx.from.byte,
                        "function.outer", NavDir::Forward, NavBoundary::Start, 1))
                    .unwrap_or(ctx.from);
                Ok(crate::registry::MotionResult { target: p, linewise: false })
            }),
        },
    );

    let doc = Document::from_text("fn a() {}\n");
    let resolver = FixedResolver;
    let env = crate::registry::TextObjectEnv {
        scope_resolver: Some(&resolver),
        comment_syntax: None,
    };
    let eff = dispatch_with_env(
        doc.registry(), &doc, BufferId(0), Position::ZERO,
        CommandInvocation::of(m.0), &CancellationToken::never(), env,
    )
    .unwrap();
    // The motion resolved to row 7 via the resolver.
    match eff {
        Effect::SelectionChange(sels) => {
            assert_eq!(sels.primary().head().line, 7);
        }
        other => panic!("expected SelectionChange, got {other:?}"),
    }
}
```

> Note: adapt `doc.registry()` / `dispatch_with_env` argument order to the actual signatures in this file (see the existing text-object dispatch tests in the same module for the exact call shape).

- [ ] **Step 2: Run the test — expect a COMPILE failure**

Run: `cargo test -p lattice-grammar motion_context_carries_scope_resolver`
Expected: FAIL to compile — `scope_toward` not a member of `ScopeResolver`, `MotionContext` has no field `scope_resolver`.

- [ ] **Step 3: Add the enums + trait method**

In `crates/lattice-grammar/src/registry.rs`, near the `ScopeResolver` trait (~line 183):

```rust
/// Direction of travel for a structural motion. `Forward` scans toward EOF,
/// `Backward` toward BOF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavDir {
    Forward,
    Backward,
}

/// Which boundary of the target node the motion lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavBoundary {
    Start,
    End,
}
```

Add to the `ScopeResolver` trait body:

```rust
    /// The `count`-th node whose capture name ends with `suffix`, in `dir`,
    /// targeting the node's `boundary`. Respects the enclosing-object rule
    /// (see treesitter-motions.md §4.1): `(Forward, Start)` / `(Backward, End)`
    /// skip the object the cursor is inside; `(Backward, Start)` / `(Forward, End)`
    /// may land on the current object's own boundary. Returns the target
    /// position, or `None` (no tree / no match / fewer than `count` candidates).
    fn scope_toward(
        &self,
        line: u32,
        col_byte: u32,
        suffix: &str,
        dir: NavDir,
        boundary: NavBoundary,
        count: u32,
    ) -> Option<lattice_protocol::Position>;
```

- [ ] **Step 4: Add the `scope_resolver` field to `MotionContext`**

In `crates/lattice-grammar/src/registry.rs`, in `struct MotionContext<'a>` (~line 65), add after `cancel`:

```rust
    /// N.1.4-motions: the active buffer's tree-sitter resolver for structural
    /// motions (`]f`/`[c`/…). `None` on Plain buffers with no parse — the
    /// motion then no-ops. Threaded by the host, identical to `TextObjectContext`.
    pub scope_resolver: Option<&'a dyn ScopeResolver>,
```

- [ ] **Step 5: Thread `env.scope_resolver` into all 3 `MotionContext` build sites**

In `crates/lattice-grammar/src/dispatcher.rs`:

- `execute_motion` (~line 189): add an `env: crate::registry::TextObjectEnv<'_>` parameter, add `scope_resolver: env.scope_resolver` to the `MotionContext { … }` (~line 198), and pass `env` at its call site in `dispatch_with_env` (~line 89: `execute_motion(document, buffer_id, cursor, &invocation, entry, cancel, env)`).
- The op-pending motion `MotionContext` (~line 615, inside `resolve_target`): add `scope_resolver: env.scope_resolver` (that function already receives `env`).
- `execute_motion_only` (~line 141, a public fn): add an `env: crate::registry::TextObjectEnv<'_>` parameter and `scope_resolver: env.scope_resolver` (~line 159). Update its callers to pass `TextObjectEnv::default()` where no syntax is available, or the real env where it is (grep `execute_motion_only(` across the workspace).

- [ ] **Step 6: Re-export the enums**

In `crates/lattice-grammar/src/lib.rs`, add `NavDir`, `NavBoundary` to the `pub use registry::{…}` (or `pub use` line that already exports `MotionSpec`, ~line 59).

- [ ] **Step 7: Add stub `scope_toward` to the other two impls**

`crates/lattice-runtime/src/actor.rs` `MockResolver` (~line 447):

```rust
    fn scope_toward(
        &self, _l: u32, _c: u32, _s: &str,
        _d: lattice_grammar::NavDir, _b: lattice_grammar::NavBoundary, _n: u32,
    ) -> Option<lattice_protocol::Position> {
        None
    }
```

`crates/lattice-multibuffer/src/lib.rs` `ComposedScopeResolver` (~line 1767) — delegate to the active excerpt's resolver if it already delegates `scope_at`, else return `None`:

```rust
    fn scope_toward(
        &self, line: u32, col_byte: u32, suffix: &str,
        dir: lattice_grammar::NavDir, boundary: lattice_grammar::NavBoundary, count: u32,
    ) -> Option<lattice_protocol::Position> {
        // Mirror however scope_at delegates in this impl; if scope_at returns
        // None unconditionally here, do the same for scope_toward.
        self.inner_scope_toward(line, col_byte, suffix, dir, boundary, count)
    }
```

> Read the existing `scope_at` body in `ComposedScopeResolver` first and mirror its delegation exactly. If it just returns `None`, return `None` here too.

- [ ] **Step 8: Run the test — expect PASS + workspace builds**

Run: `cargo test -p lattice-grammar motion_context_carries_scope_resolver`
Expected: PASS.
Run: `cargo build --workspace`
Expected: clean (all `ScopeResolver` impls satisfied).

- [ ] **Step 9: Commit**

```bash
git add crates/lattice-grammar crates/lattice-runtime crates/lattice-multibuffer
git commit -m "feat(ts-motions): TSM.1 — scope_toward seam + MotionContext.scope_resolver (stubbed)"
```

---

## TSM.2 — `SyntaxSnapshot::scope_toward` real tree walk ✅

Replaces the stub with the actual navigation, modeled on `scope_at_cursor`
(`crates/lattice-syntax/src/syntax.rs:727`).

**Files:**
- Modify: `crates/lattice-syntax/src/syntax.rs`
- Test: `crates/lattice-syntax/src/syntax.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Consumes: `NavDir`, `NavBoundary` from `lattice_grammar`; the existing `self.registry.textobjects_query(...)`, `self.line_starts`, `self.source`, `self.tree`.
- Produces: `impl ScopeResolver for SyntaxSnapshot`'s real `scope_toward` body.

- [ ] **Step 1: Write the failing tests (rust fixture)**

Add to the `#[cfg(test)]` module in `syntax.rs` (mirror the existing `scope_at_cursor_*` tests ~line 1435 for snapshot construction):

```rust
// Source: 3 top-level fns at rows 0, 2, 4.
//   row 0: fn a() {}
//   row 2: fn b() {}
//   row 4: fn c() {}
fn three_fns() -> SyntaxSnapshot {
    snapshot_rust("fn a() {}\n\nfn b() {}\n\nfn c() {}\n")
}

#[test]
fn scope_toward_forward_start_skips_enclosing() {
    let s = three_fns();
    // Cursor inside fn a (row 0) → next function START is fn b (row 2).
    let p = s.scope_toward(0, 3, "function.outer", NavDir::Forward, NavBoundary::Start, 1);
    assert_eq!(p, Some(lattice_protocol::Position::new(2, 0)));
}

#[test]
fn scope_toward_forward_start_count_two() {
    let s = three_fns();
    // From row 0, 2nd next function start is fn c (row 4).
    let p = s.scope_toward(0, 3, "function.outer", NavDir::Forward, NavBoundary::Start, 2);
    assert_eq!(p, Some(lattice_protocol::Position::new(4, 0)));
}

#[test]
fn scope_toward_backward_start_lands_on_current() {
    let s = three_fns();
    // Cursor inside fn b past its start (row 2, col 5) → prev START is fn b's OWN
    // start (row 2, col 0), per the enclosing rule.
    let p = s.scope_toward(2, 5, "function.outer", NavDir::Backward, NavBoundary::Start, 1);
    assert_eq!(p, Some(lattice_protocol::Position::new(2, 0)));
}

#[test]
fn scope_toward_forward_end_lands_on_current_end() {
    let s = three_fns();
    // Cursor inside fn b (row 2, col 5) → next END is fn b's own closing brace.
    // "fn b() {}" — end_position is row 2, col 9 (one past the `}`).
    let p = s.scope_toward(2, 5, "function.outer", NavDir::Forward, NavBoundary::End, 1);
    assert_eq!(p, Some(lattice_protocol::Position::new(2, 9)));
}

#[test]
fn scope_toward_stops_at_boundary() {
    let s = three_fns();
    // From inside the LAST fn, forward-start has no next → None (no wrap).
    let p = s.scope_toward(4, 3, "function.outer", NavDir::Forward, NavBoundary::Start, 1);
    assert_eq!(p, None);
}

#[test]
fn scope_toward_none_without_tree() {
    let s = snapshot_plain("plain text no tree\n");
    let p = s.scope_toward(0, 0, "function.outer", NavDir::Forward, NavBoundary::Start, 1);
    assert_eq!(p, None);
}
```

> Reuse whatever snapshot-construction helper the existing `scope_at_cursor` tests use (e.g. a `snapshot_rust(src)` / `snapshot_plain(src)` helper); if none exists, factor one from an existing test's setup. Add one python fixture test (`def a(): pass` × 3) to prove language-agnosticism.

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p lattice-syntax scope_toward`
Expected: FAIL — stub returns `None` for the positive cases.

- [ ] **Step 3: Implement `scope_toward`**

Replace the TSM.1 stub in `impl ScopeResolver for SyntaxSnapshot` (`syntax.rs:494`) with a real body (add an inherent `SyntaxSnapshot::scope_toward` method and forward the trait method to it, matching how `scope_at` forwards to `scope_at_cursor`):

```rust
pub fn scope_toward(
    &self,
    line: u32,
    col_byte: u32,
    suffix: &str,
    dir: lattice_grammar::NavDir,
    boundary: lattice_grammar::NavBoundary,
    count: u32,
) -> Option<lattice_protocol::Position> {
    use lattice_grammar::{NavBoundary, NavDir};
    if count == 0 {
        return None;
    }
    let tree = self.tree.as_ref()?;
    let query = self.registry.textobjects_query(self.lang.name())?;
    let line_start = self.line_starts.get(line as usize).copied()?;
    let cursor_byte = (line_start + col_byte as usize).min(self.source.len());

    // Restrict the query to the half of the file we scan (perf: bounds the
    // match set on large files — Global Constraint / paramount #1).
    let mut cursor = QueryCursor::new();
    match dir {
        NavDir::Forward => cursor.set_byte_range(cursor_byte..self.source.len()),
        NavDir::Backward => cursor.set_byte_range(0..cursor_byte.saturating_add(1)),
    }

    let names = query.capture_names();
    // Collect candidate boundary bytes + their (row, col) positions.
    let mut cands: Vec<(usize, lattice_protocol::Position)> = Vec::new();
    let mut matches = cursor.matches(query, tree.root_node(), &self.source[..]);
    while let Some(m) = matches.next() {
        for cap in m.captures {
            if !names[cap.index as usize].ends_with(suffix) {
                continue;
            }
            let n = cap.node;
            let (b, pt) = match boundary {
                NavBoundary::Start => (n.start_byte(), n.start_position()),
                NavBoundary::End => (n.end_byte(), n.end_position()),
            };
            // Enclosing-object rule (treesitter-motions.md §4.1):
            let keep = match (dir, boundary) {
                // Skip the object the cursor is inside — strictly past cursor.
                (NavDir::Forward, NavBoundary::Start) => b > cursor_byte,
                (NavDir::Backward, NavBoundary::End) => b < cursor_byte,
                // May land on the current object's own boundary.
                (NavDir::Backward, NavBoundary::Start) => b <= cursor_byte,
                (NavDir::Forward, NavBoundary::End) => b >= cursor_byte,
            };
            if keep {
                cands.push((b, lattice_protocol::Position::new(pt.row as u32, pt.column as u32)));
            }
        }
    }
    // Sort in the direction of travel; dedup by byte (a node can be captured
    // by multiple patterns).
    cands.sort_by_key(|(b, _)| *b);
    cands.dedup_by_key(|(b, _)| *b);
    let ordered: Vec<_> = match dir {
        NavDir::Forward => cands,
        NavDir::Backward => cands.into_iter().rev().collect(),
    };
    ordered.get((count as usize).saturating_sub(1)).map(|(_, p)| *p)
}
```

Then make the trait method forward to it:

```rust
    fn scope_toward(
        &self, line: u32, col_byte: u32, suffix: &str,
        dir: lattice_grammar::NavDir, boundary: lattice_grammar::NavBoundary, count: u32,
    ) -> Option<lattice_protocol::Position> {
        self.scope_toward(line, col_byte, suffix, dir, boundary, count)
    }
```

> `NavBoundary::End` returning `end_position` (one past the last byte) matches the inclusive-end contract (`d]F` eats the `}`); the operator's inclusive handling adds the final byte. Confirm against the `scope_at_cursor` `end_position` comment (`syntax.rs:773`).

- [ ] **Step 4: Run — expect PASS**

Run: `cargo test -p lattice-syntax scope_toward`
Expected: PASS (all cases incl. python fixture + no-tree).

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-syntax/src/syntax.rs
git commit -m "feat(ts-motions): TSM.2 — SyntaxSnapshot::scope_toward tree walk + tests"
```

---

## TSM.3 — `register_syntax_motions` (16 MotionSpecs) ✅

New `motions.rs` mirroring `text_objects.rs`.

**Files:**
- Create: `crates/lattice-syntax/src/motions.rs`
- Modify: `crates/lattice-syntax/src/lib.rs`
- Test: inline `#[cfg(test)]` in `motions.rs`

**Interfaces:**
- Consumes: `CommandRegistry::register_motion`, `MotionSpec`, `NavDir`, `NavBoundary`, `MotionContext.scope_resolver`.
- Produces: `pub struct SyntaxMotionIds { next_function_start, prev_function_start, next_function_end, prev_function_end, next_class_start, … , prev_loop_end }` (16 `MotionId` fields); `pub fn register_syntax_motions(&mut CommandRegistry) -> SyntaxMotionIds`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn registers_sixteen_distinct_motions() {
    let mut registry = CommandRegistry::new();
    let ids = register_syntax_motions(&mut registry);
    let all = [
        ids.next_function_start, ids.prev_function_start, ids.next_function_end, ids.prev_function_end,
        ids.next_class_start,    ids.prev_class_start,    ids.next_class_end,    ids.prev_class_end,
        ids.next_parameter_start,ids.prev_parameter_start,ids.next_parameter_end,ids.prev_parameter_end,
        ids.next_loop_start,     ids.prev_loop_start,     ids.next_loop_end,     ids.prev_loop_end,
    ];
    let mut seen = std::collections::HashSet::new();
    for id in all {
        assert!(seen.insert(id.0), "duplicate motion id");
        let spec = registry.lookup(id.0).expect("registered");
        assert_eq!(spec.kind, lattice_grammar::CommandKind::Motion);
    }
    assert_eq!(seen.len(), 16);
    // Named + jump=true; start-targets exclusive, end-targets inclusive.
    assert!(registry.id_by_name("motion:next-function-start").is_some());
    assert!(registry.id_by_name("motion:prev-loop-end").is_some());
}
```

- [ ] **Step 2: Run — expect compile FAIL**

Run: `cargo test -p lattice-syntax registers_sixteen_distinct_motions`
Expected: FAIL — `register_syntax_motions` undefined.

- [ ] **Step 3: Write `motions.rs`**

```rust
//! Tree-sitter structural motions — `]f`/`[f`/`]F`/`[F` (function),
//! `]c`/`[c`/`]C`/`[C` (class), `]a`/`[a`/`]A`/`[A` (parameter),
//! `]l`/`[l`/`]L`/`[L` (loop). The motion counterpart to the structural
//! text objects (`text_objects.rs`); both read the same textobjects.scm
//! captures. See docs/dev/architecture/treesitter-motions.md.

use lattice_grammar::registry::{CommandRegistry, MotionResult, MotionSpec};
use lattice_grammar::{MotionId, NavBoundary, NavDir};

#[derive(Debug, Clone, Copy)]
pub struct SyntaxMotionIds {
    pub next_function_start: MotionId, pub prev_function_start: MotionId,
    pub next_function_end: MotionId,   pub prev_function_end: MotionId,
    pub next_class_start: MotionId,    pub prev_class_start: MotionId,
    pub next_class_end: MotionId,      pub prev_class_end: MotionId,
    pub next_parameter_start: MotionId,pub prev_parameter_start: MotionId,
    pub next_parameter_end: MotionId,  pub prev_parameter_end: MotionId,
    pub next_loop_start: MotionId,     pub prev_loop_start: MotionId,
    pub next_loop_end: MotionId,       pub prev_loop_end: MotionId,
}

pub fn register_syntax_motions(registry: &mut CommandRegistry) -> SyntaxMotionIds {
    SyntaxMotionIds {
        next_function_start: reg(registry, "motion:next-function-start",
            "Go to the start of the next function (`]f`). `@function.outer`.",
            "function.outer", NavDir::Forward, NavBoundary::Start),
        prev_function_start: reg(registry, "motion:prev-function-start",
            "Go to the start of the previous (or enclosing) function (`[f`).",
            "function.outer", NavDir::Backward, NavBoundary::Start),
        next_function_end: reg(registry, "motion:next-function-end",
            "Go to the end of the current/next function (`]F`).",
            "function.outer", NavDir::Forward, NavBoundary::End),
        prev_function_end: reg(registry, "motion:prev-function-end",
            "Go to the end of the previous function (`[F`).",
            "function.outer", NavDir::Backward, NavBoundary::End),

        next_class_start: reg(registry, "motion:next-class-start",
            "Go to the start of the next class/type (`]c`). `@class.outer`.",
            "class.outer", NavDir::Forward, NavBoundary::Start),
        prev_class_start: reg(registry, "motion:prev-class-start",
            "Go to the start of the previous (or enclosing) class (`[c`).",
            "class.outer", NavDir::Backward, NavBoundary::Start),
        next_class_end: reg(registry, "motion:next-class-end",
            "Go to the end of the current/next class (`]C`).",
            "class.outer", NavDir::Forward, NavBoundary::End),
        prev_class_end: reg(registry, "motion:prev-class-end",
            "Go to the end of the previous class (`[C`).",
            "class.outer", NavDir::Backward, NavBoundary::End),

        next_parameter_start: reg(registry, "motion:next-parameter-start",
            "Go to the start of the next parameter/argument (`]a`). `@parameter.outer`.",
            "parameter.outer", NavDir::Forward, NavBoundary::Start),
        prev_parameter_start: reg(registry, "motion:prev-parameter-start",
            "Go to the start of the previous parameter (`[a`).",
            "parameter.outer", NavDir::Backward, NavBoundary::Start),
        next_parameter_end: reg(registry, "motion:next-parameter-end",
            "Go to the end of the current/next parameter (`]A`).",
            "parameter.outer", NavDir::Forward, NavBoundary::End),
        prev_parameter_end: reg(registry, "motion:prev-parameter-end",
            "Go to the end of the previous parameter (`[A`).",
            "parameter.outer", NavDir::Backward, NavBoundary::End),

        next_loop_start: reg(registry, "motion:next-loop-start",
            "Go to the start of the next loop (`]l`). `@loop.outer`.",
            "loop.outer", NavDir::Forward, NavBoundary::Start),
        prev_loop_start: reg(registry, "motion:prev-loop-start",
            "Go to the start of the previous (or enclosing) loop (`[l`).",
            "loop.outer", NavDir::Backward, NavBoundary::Start),
        next_loop_end: reg(registry, "motion:next-loop-end",
            "Go to the end of the current/next loop (`]L`).",
            "loop.outer", NavDir::Forward, NavBoundary::End),
        prev_loop_end: reg(registry, "motion:prev-loop-end",
            "Go to the end of the previous loop (`[L`).",
            "loop.outer", NavDir::Backward, NavBoundary::End),
    }
}

/// Register one structural motion. `boundary == Start` ⇒ exclusive; `End` ⇒
/// inclusive (so `d]F` eats the closing brace). All are jumps (`<C-o>` returns);
/// jump recording is gated to standalone Normal use by the dispatcher.
fn reg(
    registry: &mut CommandRegistry,
    name: &str,
    doc: &str,
    suffix: &'static str,
    dir: NavDir,
    boundary: NavBoundary,
) -> MotionId {
    let exclusive = matches!(boundary, NavBoundary::Start);
    registry.register_motion(name, doc, MotionSpec {
        jump: true,
        exclusive,
        args_schema: Vec::new(),
        apply: Box::new(move |ctx| {
            let target = ctx
                .scope_resolver
                .and_then(|r| r.scope_toward(
                    ctx.from.line, ctx.from.byte, suffix, dir, boundary,
                    ctx.count.get().max(1),
                ))
                .unwrap_or(ctx.from); // graceful no-op: no tree / no match / boundary
            Ok(MotionResult { target, linewise: false })
        }),
    })
}
```

> `ctx.count.get()` — adapt to the actual `Count` accessor in `MotionContext` (see how a builtin motion like `paragraph_forward` reads its count in `builtins.rs`). If `Count` has no `.get()`, use the same idiom the builtins use.

- [ ] **Step 4: Wire the module**

In `crates/lattice-syntax/src/lib.rs`: add `pub mod motions;` and `pub use motions::{register_syntax_motions, SyntaxMotionIds};` (mirror the `text_objects` re-export lines).

- [ ] **Step 5: Run — expect PASS**

Run: `cargo test -p lattice-syntax registers_sixteen_distinct_motions`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/lattice-syntax/src/motions.rs crates/lattice-syntax/src/lib.rs
git commit -m "feat(ts-motions): TSM.3 — register_syntax_motions (16 structural motions)"
```

---

## TSM.4 — Host wiring: boot registration + 16 chord bindings ✅

Registers the motions at boot and binds `]x`/`[x` in Normal, operator-pending,
and Visual — the three surfaces sourced from one table so they never drift.

**Files:**
- Modify: `crates/lattice-host/src/editor_boot.rs`
- Modify: `crates/lattice-host/src/keymap_normal.rs`
- Modify: `crates/lattice-host/src/keymap_visual.rs`
- Modify: `crates/lattice-host/src/keymap_select.rs` (threads `SyntaxMotionIds` through, like `SyntaxTextObjectIds`)
- Test: `crates/lattice-host/tests/` (new integration test file, or extend an existing motion/dispatch test)

**Interfaces:**
- Consumes: `lattice_syntax::{register_syntax_motions, SyntaxMotionIds}`; `handle.bind(layer, mode, &[ChordPattern], CommandInvocation, source())`; `Target::Motion`.
- Produces: `syntax_motion_rows(&SyntaxMotionIds) -> Vec<(Vec<ChordPattern>, MotionId)>` (each `Vec<ChordPattern>` is the full 2-key sequence, e.g. `[lit_char(']'), lit_char('f')]`).

- [ ] **Step 1: Write the failing integration test**

Create `crates/lattice-host/tests/treesitter_motions.rs` (model setup on an existing host dispatch test — find one that builds an `Editor` over a rust buffer and drives chords):

```rust
// Drives ]f / d]f / v]c over a real rust buffer through the host dispatcher.
// Fixture (rows): 0 `fn a() {}` … 2 `fn b() {}` … 4 `fn c() {}`
#[test]
fn bracket_f_moves_to_next_function() {
    let mut ed = editor_with_rust("fn a() {}\n\nfn b() {}\n\nfn c() {}\n");
    // cursor at row 0; type ] then f
    feed_keys(&mut ed, "]f");
    assert_eq!(ed.cursor.line, 2, "]f → next function start");
}

#[test]
fn d_bracket_f_deletes_to_next_function() {
    let mut ed = editor_with_rust("fn a() {}\n\nfn b() {}\n");
    feed_keys(&mut ed, "d]f");
    // Deletes row 0..row 2 start (exclusive) → fn b becomes the first line.
    assert!(ed.line_text(0).contains("fn b"), "d]f deletes up to next fn start");
}

#[test]
fn plain_buffer_bracket_f_noops() {
    let mut ed = editor_with_plain("just text\nmore text\n");
    feed_keys(&mut ed, "]f");
    assert_eq!(ed.cursor.line, 0, "no tree → no-op");
}
```

> Use the harness helpers the existing host tests use (`editor_with_rust`, `feed_keys`, …). If they don't exist under those names, mirror the closest existing dispatch test's setup — the point is: build an Editor with a parsed rust buffer, feed the chord, assert cursor/text.

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p lattice-host bracket_f_moves_to_next_function`
Expected: FAIL — `]f` unbound, cursor stays at row 0.

- [ ] **Step 3: Register motions at boot**

In `crates/lattice-host/src/editor_boot.rs`, next to the text-object registration (`register_syntax_text_objects(boot.commands_mut())`, ~line 596):

```rust
let syntax_motions = lattice_syntax::register_syntax_motions(boot.commands_mut());
```

Thread `syntax_motions` to wherever `syntax_textobjects` is threaded into the keymap builders (follow the existing `SyntaxTextObjectIds` value through boot → keymap setup; add a parallel `&SyntaxMotionIds` parameter alongside it).

- [ ] **Step 4: Add the `syntax_motion_rows` table**

In `crates/lattice-host/src/keymap_normal.rs`, next to `motion_rows` (~line 1438):

```rust
/// The 16 tree-sitter structural motions as full 2-key sequences. Shared by the
/// Normal binder, the operator-pending resolver, and the Visual binder so the
/// three surfaces never drift (same discipline as `motion_rows` / `text_object_rows`).
pub(crate) fn syntax_motion_rows(
    m: &SyntaxMotionIds,
) -> Vec<(Vec<ChordPattern>, lattice_grammar::registry::MotionId)> {
    vec![
        (vec![lit_char(']'), lit_char('f')], m.next_function_start),
        (vec![lit_char('['), lit_char('f')], m.prev_function_start),
        (vec![lit_char(']'), lit_char('F')], m.next_function_end),
        (vec![lit_char('['), lit_char('F')], m.prev_function_end),
        (vec![lit_char(']'), lit_char('c')], m.next_class_start),
        (vec![lit_char('['), lit_char('c')], m.prev_class_start),
        (vec![lit_char(']'), lit_char('C')], m.next_class_end),
        (vec![lit_char('['), lit_char('C')], m.prev_class_end),
        (vec![lit_char(']'), lit_char('a')], m.next_parameter_start),
        (vec![lit_char('['), lit_char('a')], m.prev_parameter_start),
        (vec![lit_char(']'), lit_char('A')], m.next_parameter_end),
        (vec![lit_char('['), lit_char('A')], m.prev_parameter_end),
        (vec![lit_char(']'), lit_char('l')], m.next_loop_start),
        (vec![lit_char('['), lit_char('l')], m.prev_loop_start),
        (vec![lit_char(']'), lit_char('L')], m.next_loop_end),
        (vec![lit_char('['), lit_char('L')], m.prev_loop_end),
    ]
}
```

Add `use lattice_syntax::SyntaxMotionIds;` next to the existing `SyntaxTextObjectIds` import.

- [ ] **Step 5: Bind in Normal mode**

In the Normal-mode binder that consumes `motion_rows` (the `for (chord, motion) in motion_rows(builtins)` loop, ~line 113), add a sibling loop:

```rust
for (seq, motion) in syntax_motion_rows(syntax_motions) {
    handle.bind(
        KeymapLayer::Builtin,
        BindingMode::Normal,
        &seq,
        CommandInvocation::of(motion.0),
        source(),
    );
}
```

(Thread `syntax_motions: &SyntaxMotionIds` into this function's signature alongside `builtins` / `syntax_textobjects`.)

- [ ] **Step 6: Bind in operator-pending**

In the operator-pending resolver (the section ~line 1266 that maps motion chords to `Invoke(op, Target::Motion(motion))` from `motion_rows`), add the sibling loop over `syntax_motion_rows`, binding each `seq` under the pending prefix to `CommandInvocation::of(op.0).with_target(Target::Motion(motion, Args::None))`. Mirror the exact call shape used for `motion_rows` entries there.

- [ ] **Step 7: Bind in Visual mode**

In `crates/lattice-host/src/keymap_visual.rs`, wherever Visual binds motion chords (it already consumes `motion_rows` / `text_object_rows`), add the sibling loop over `syntax_motion_rows(syntax_motions)` binding each `seq` to `CommandInvocation::of(motion.0)` in `BindingMode::Visual`. Thread `&SyntaxMotionIds` into that binder like `&SyntaxTextObjectIds`.

- [ ] **Step 8: Run — expect PASS + full host suite green**

Run: `cargo test -p lattice-host bracket_f_moves_to_next_function d_bracket_f_deletes_to_next_function plain_buffer_bracket_f_noops`
Expected: PASS.
Run: `cargo test -p lattice-host`
Expected: green (no keymap regressions — the `]`/`[` prefixes previously fell through to the bracket text objects only after `a`/`i`, so bare `]f` was unbound; confirm no existing test bound bare `]`/`[` in Normal).

- [ ] **Step 9: Commit**

```bash
git add crates/lattice-host
git commit -m "feat(ts-motions): TSM.4 — bind ]f/[f/…/[L in Normal, op-pending, Visual"
```

---

## TSM.5 — Bench + docs finalization ✅

**Files:**
- Create: `crates/lattice-syntax/benches/scope_toward.rs` (or extend an existing syntax bench)
- Modify: `crates/lattice-syntax/Cargo.toml` (`[[bench]]` entry if new)
- Modify: `BENCHMARKS.md`
- Modify: `docs/dev/operations/implementation.md` (ledger entry)
- Modify: `docs/dev/operations/slice-plans/treesitter-motions.md` (flip status icons to ✅)

- [ ] **Step 1: Write the bench**

Model on an existing `lattice-syntax` criterion bench. Parse a large generated rust source (e.g. 2,000 functions), then bench `scope_toward(mid, 0, "function.outer", Forward, Start, 1)` from the file midpoint, and a `Backward` variant:

```rust
fn bench_scope_toward(c: &mut Criterion) {
    let src = (0..2000).map(|i| format!("fn f{i}() {{ let x = {i}; }}\n")).collect::<String>();
    let snap = snapshot_rust(&src);
    let mid = (src.lines().count() / 2) as u32;
    c.bench_function("scope_toward/fwd_start", |b| {
        b.iter(|| snap.scope_toward(black_box(mid), 0, "function.outer",
            NavDir::Forward, NavBoundary::Start, 1))
    });
    c.bench_function("scope_toward/back_start", |b| {
        b.iter(|| snap.scope_toward(black_box(mid), 0, "function.outer",
            NavDir::Backward, NavBoundary::Start, 1))
    });
}
```

- [ ] **Step 2: Run the bench**

Run: `cargo bench -p lattice-syntax scope_toward`
Expected: completes; record the p50 numbers.

- [ ] **Step 3: Record results in `BENCHMARKS.md`**

Add a row for `scope_toward/fwd_start` + `scope_toward/back_start` with the measured times, noting the byte-range restriction keeps `Backward` scanning only `[0, cursor)` and `Forward` only `[cursor, EOF)`.

- [ ] **Step 4: Update the implementation ledger**

Add a `docs/dev/operations/implementation.md` entry: "Tree-sitter structural motions (`]f`/`[c`/…) — done. Design: `architecture/treesitter-motions.md`; slices: `operations/slice-plans/treesitter-motions.md` (TSM.0–TSM.5)."

- [ ] **Step 5: Flip status icons**

In this file, change every `📝` in the slice headers to `✅`.

- [ ] **Step 6: Commit**

```bash
git add crates/lattice-syntax BENCHMARKS.md docs/dev/operations/implementation.md docs/dev/operations/slice-plans/treesitter-motions.md
git commit -m "feat(ts-motions): TSM.5 — scope_toward bench + docs/ledger finalization"
```

---

## Self-review notes

- **Spec coverage:** §1 goal → whole plan; §3 catalog (16 chords) → TSM.3/TSM.4; §4.1 enclosing rule → TSM.2 `keep` match + tests; §4.3 exclusive/inclusive → TSM.3 `reg`; §4.4 stop → TSM.2 `scope_toward_stops_at_boundary`; §4.5 jump=true → TSM.3; §4.6 graceful failure → TSM.2 no-tree test + TSM.3 `unwrap_or(ctx.from)` + TSM.4 plain-buffer test; §5 resolver seam → TSM.1; §6 resolution/byte-range → TSM.2; §8 bench → TSM.5.
- **`]c` non-collision:** no task touches `lattice-diff`; the Builtin `]c` binding (TSM.4) coexists with diff-mode's MinorMode `]c` by layer precedence — verified implicitly by TSM.4's full-suite run (diff tests must stay green).
- **Type consistency:** `scope_toward(line, col_byte, suffix, dir, boundary, count) -> Option<Position>` identical across TSM.1 (trait), TSM.2 (impl), TSM.3 (call). `SyntaxMotionIds` field names identical across TSM.3 (def) and TSM.4 (`syntax_motion_rows`). `NavDir`/`NavBoundary` from `lattice_grammar` throughout.
- **Open adaptation points flagged inline** (not placeholders — real "match the existing idiom" instructions): exact `dispatch_with_env` arg order (TSM.1), `Count` accessor (TSM.3), host test-harness helper names (TSM.4), `ComposedScopeResolver` delegation shape (TSM.1).
