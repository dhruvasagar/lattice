# Tree-sitter Structural Motions

Design fragment for Lattice's **structural motions** — `]f` (next function),
`[c` (previous class), `]F` (next function end), `[a` (previous argument), etc.
— that jump the cursor between tree-sitter constructs. They are the **motion**
counterpart to the tree-sitter *text objects* (`af`/`ic`/`aa`/`al`, see
[`tree-sitter-text-objects.md`](./tree-sitter-text-objects.md)): the objects
answer "operate on the construct I'm *in*"; the motions answer "go to the
next/previous construct." Both read the same `textobjects.scm` captures against
the same parse tree.

They are first-class citizens of the vim grammar — registered through the same
`register_motion` API as `w`, `}`, `G` — so they compose with **every** operator,
count, and Visual selection for free (`d]f`, `y]F`, `c2]a`, `v]c`, `zn]f`). Slice
sequencing lives in
[`../operations/slice-plans/treesitter-motions.md`](../operations/slice-plans/treesitter-motions.md).

---

## 1. Design goal

The classic vim structural motions are `[[` / `]]` / `[m` / `]m` — section and
method jumps computed from unindented `{` in column 1 (a C-file heuristic that
misfires in Rust, Python, and every brace-free language). Tree-sitter motions
replace the heuristic with the real parse tree, the same one that drives
highlighting, folding, and the structural text objects:

- They compose with the whole grammar: any operator + any structural motion +
  any count. Adding one is a single `register_motion` call — paramount goal #3
  (the grammar IS the public API; new motions are first-class).
- They reuse the existing `textobjects.scm` captures (`@function.outer`,
  `@class.outer`, `@parameter.outer`, `@loop.outer`) — no new query files, no new
  capture vocabulary. A language with no `textobjects.scm` simply has no
  structural motions; the classic motions still work.
- They are **universal**, not narrow-scoped. `]f` works in any code buffer.

**Non-goals (deferred):** `next-usage` / reference navigation (LSP, not the parse
tree); `]d` diagnostic hop (owned by the LSP subsystem); comment / conditional /
block motions (land later as one `register_motion` line each if a need surfaces).

---

## 2. Reference points + the key-space

| Editor | Function | Class | Parameter | Direction keys | Basis |
|---|---|---|---|---|---|
| vim (builtin) | `]m`/`[m` | `]]`/`[[` | — | `]` fwd / `[` back | C-brace heuristic |
| nvim-treesitter-textobjects | `]m`/`]M` | `]]`/`][` | `]a` (via mapping) | `]`/`[`, caps = end | community standard |
| Helix | `]f`/`[f` | `]c` / `]t` | `]a`/`[a` | `]`/`[` | tree-sitter, built-in |

The `]`/`[` = forward/backward prefix is **unanimous** across every editor —
non-negotiable muscle memory (UX-convention rule). For the second key, Lattice
uses the **object letter that already names the matching text object** (`f`/`c`/
`a`/`l`), so the motion and the object share one mnemonic:

> `af` operates on a function ⟷ `]f` goes to the next function.

This is Helix's scheme, and it keeps the whole structural surface — objects and
motions — on one letter per construct. vim's `]m`/`]]` split (methods on `m`,
sections on `]`) is a C-era artifact that doesn't generalize; per heuristic #2 we
anchor on the paramount goal (one coherent grammar) over "vim does `]m`."

**Start vs end** follows nvim-treesitter's idea (there: `]m` start, `]M` end) but
expressed through the object letter's case: **lowercase = start, uppercase =
end**. `]f` = next function start, `]F` = next function end.

---

## 3. The keybinding catalog

`]` = forward (toward EOF), `[` = backward (toward BOF). Lowercase second key =
the construct's **start**; uppercase = its **end**.

| Construct | next start | prev start | next end | prev end | `capture_suffix` |
|---|---|---|---|---|---|
| **Function** | `]f` | `[f` | `]F` | `[F` | `function.outer` |
| **Class / type** | `]c` | `[c` | `]C` | `[C` | `class.outer` |
| **Parameter / arg** | `]a` | `[a` | `]A` | `[A` | `parameter.outer` |
| **Loop** | `]l` | `[l` | `]L` | `[L` | `loop.outer` |

16 chords, bound at the **Builtin keymap layer** (universal vim grammar, like the
tree-sitter text objects and every builtin motion) in the Normal, Visual, and
operator-pending tables.

**The `]c` / `[c` "collision" is not real.** `lattice-diff` binds `]c` / `[c` to
next/prev diff-hunk — but only at `KeymapLayer::MinorMode(diff-mode)`, gated by
K.1.c's per-keystroke filter to buffers where diff-mode is active. On a regular
code buffer the diff layer is absent, so the Builtin `]c` = next class resolves.
In a diff buffer the MinorMode layer wins → `]c` = next hunk. This exactly mirrors
vim, where `]c` is diff-mode-only and never means "class" in a normal buffer. No
change to `lattice-diff`.

**Deferred** (documented for users, not in v1): `comment` (`]/`?), `conditional`,
`block`, `call`. No clean free mnemonic under `]`/`[` for several, and low
marginal value; each lands as one `register_motion` line if a concrete need
surfaces. `next-diagnostic` (`]d`) and `next-reference` are LSP-owned, not
parse-tree motions — explicitly out of scope here.

---

## 4. Motion semantics — what makes them *proper motions*

A "proper motion" returns a target `Position`; the dispatcher does the rest —
Normal-mode cursor move, Visual-mode extend, and operator-pending range are all
computed from `(from, target)` by machinery that already exists. So `d]f`,
`v2]c`, `y]F`, and `zn]f` work with **zero** per-operator code.

### 4.1 The enclosing-object rule (vim `]m`/`[m`/`]M` parity)

Direction and boundary interact with the cursor's current enclosing object, so
the motions feel right whether you start inside a construct or between two:

| Chord | Target | Inside object A (past its start) |
|---|---|---|
| `]f` next start | start of first fn whose **start** > cursor | skips to the **next** fn B |
| `[f` prev start | start of nearest fn whose **start** < cursor | lands on **A's own start** |
| `]F` next end | end of nearest fn whose **end** > cursor | lands on **A's own end** (`}`) |
| `[F` prev end | end of nearest fn whose **end** < cursor | skips to the **previous** fn's end |

The asymmetry (backward-start and forward-end land on the *current* object first)
is deliberate and matches vim — it is what makes `[f` reliably reach "the top of
the function I'm in."

### 4.2 Count

`N]f` = the Nth next function start. `3]F` = the 3rd function end forward. Count
walks the sorted candidate list N steps; if fewer than N remain, see §4.4.

### 4.3 Exclusive / inclusive / linewise

- **Start-targets** (`]f`/`[f`/…) are `exclusive: true` — `d]f` deletes up to but
  not including the next function's first byte.
- **End-targets** (`]F`/`[F`/…) are `exclusive: false` (inclusive) — `d]F` eats
  through the closing `}` of the function.
- All are **charwise** (`MotionResult.linewise = false`): tree-sitter gives
  byte-precise `(line, col)`, so `]f` lands on the `f` of `fn`, not the line.

### 4.4 Stop at boundary — no wrap

At the last matching object in a direction (or when count overshoots), the motion
returns the **cursor unchanged** — a no-op, vim's `]m` behavior. Chosen over the
diff-hunk-style wrap because these are **operator targets**: a wrapping `d]f` at
the last function would delete from the cursor around to the file's *first*
function, which is a surprising, destructive range. Predictability wins for a
motion that composes with `d`/`c`/`y`.

### 4.5 Jump-list

`jump: true` — a standalone Normal-mode `]f` pushes the prior position so `<C-o>`
returns. This treats structural navigation like vim's `]]`/`[[` section jumps
(worth an undo), not like `]m` (which vim omits). Jump recording follows the
**existing dispatcher convention**: the jump is pushed only when the motion is the
top-level Normal-mode movement — when it is consumed as an operator target (`d]f`)
or a Visual extension it does **not** touch the jumplist, exactly as `G`/`}`/`%`
already behave. No new jumplist plumbing.

### 4.6 Graceful failure (heuristic #5)

No syntax tree (Plain buffer, no `textobjects.scm`) or no matching object in the
requested direction → the motion returns `from` unchanged. A paired operator then
no-ops. Never panics, never edits — the same convention as the tree-sitter text
objects' `scope_at` returning `None`.

---

## 5. Architecture — the resolver seam

`register_motion` already exists and is symmetric to `register_operator` /
`register_text_object`:

```rust
	// lattice-grammar::registry — ALREADY PRESENT
	pub fn register_motion(&mut self, name: &str, doc: &str, spec: MotionSpec) -> MotionId;
	type MotionFn = Box<dyn Fn(&MotionContext) -> GrammarResult<MotionResult> + Send + Sync>;
```

Two gaps: (1) `MotionContext` carries **no** scope resolver today (only
`TextObjectContext` does), and (2) the existing `ScopeResolver` trait answers only
"enclosing scope *at* the cursor" — motions need "nearest node *toward* a
direction." Both close with **one new method on the existing trait** plus one new
context field, mirroring the text-object seam exactly:

```rust
	// lattice-grammar — trait EXTENDED (not a new trait)
	pub enum NavDir { Forward, Backward }
	pub enum NavBoundary { Start, End }

	pub trait ScopeResolver {
		// N.1.4 — enclosing scope at cursor (text objects). UNCHANGED.
		fn scope_at(&self, line: u32, col_byte: u32, suffix: &str) -> Option<ProtoRange>;

		// NEW — the Nth node matching `suffix` in `dir`, targeting the node's
		// `boundary`, respecting the §4.1 enclosing-object rule. Returns the
		// target position, or None (no tree / no match / count overshoot).
		fn scope_toward(
			&self,
			line: u32,
			col_byte: u32,
			suffix: &str,
			dir: NavDir,
			boundary: NavBoundary,
			count: u32,
		) -> Option<ProtoPosition>;
	}

	pub struct MotionContext<'a> {
		// … existing fields (buffer, from, count, has_explicit_count, args, cancel) …
		pub scope_resolver: Option<&'a dyn ScopeResolver>,   // ← NEW
	}
```

Three-crate split (no `grammar → syntax` dependency, so no cycle — identical to
the text-object layering):

| Crate | Owns |
|---|---|
| **`lattice-grammar`** | the `scope_toward` method + `NavDir`/`NavBoundary` enums + the `MotionContext.scope_resolver` field. Stays syntax-agnostic. |
| **`lattice-syntax`** | **owns and defines the motions.** New `motions.rs`: `register_syntax_motions(&mut CommandRegistry) -> SyntaxMotionIds` registers the 16 `MotionSpec`s (each `apply` calls `ctx.scope_resolver?.scope_toward(…)`). `impl ScopeResolver for SyntaxSnapshot` gains the `scope_toward` body (the tree walk). |
| **`lattice-host`** | at boot calls `register_syntax_motions`; binds the 16 chords into the Normal / Visual / op-pending keymap tables; at dispatch threads the active buffer's `SyntaxSnapshot` into `MotionContext.scope_resolver` — the identical wiring already done for `TextObjectContext`. |

**Why extend `ScopeResolver`, not add a sibling trait.** `scope_at` and
`scope_toward` are the *same kind of query* — run a `textobjects.scm` capture,
return positions against the buffer's tree — so one cohesive "structural query
surface" is the genuinely-better long-term design (heuristic #1), not risk-
aversion. A sibling `StructuralNav` trait would add a second `Arc<dyn>` handle in
`lattice-runtime`, a second context field through the dispatch env, and a second
host coercion, all to separate two queries that share the same tree walk. The
existing `ScopeResolverHandle` threading is reused verbatim → **zero** new
plumbing types. (Rejected: renaming the trait to `SyntaxQuery` — cross-crate
churn for a cosmetic gain.)

---

## 6. Resolution — `scope_toward`

`SyntaxSnapshot::scope_toward` runs the language's `textobjects.scm` restricted by
`QueryCursor::set_byte_range` to the half we care about — `[cursor_byte, EOF)` for
`Forward`, `[0, cursor_byte)` for `Backward` — so the query never scans the far
half of the file. Among captures whose name ends with `suffix`:

1. Compute each candidate's **boundary byte** (`Start` → node start, `End` → node
   end).
2. Apply the §4.1 enclosing rule: for `(Forward, Start)` and `(Backward, End)`
   keep candidates strictly past the cursor (skip the enclosing object); for
   `(Backward, Start)` and `(Forward, End)` keep candidates at-or-containing the
   cursor too (so the current object's own start/end is reachable).
3. Sort by boundary byte in the direction of travel; return the `count`-th, mapped
   back to `(line, col)` via the rope. Fewer than `count` candidates → `None`
   (§4.4 stop).

Reuses the same query + capture-suffix matching as `scope_at_cursor`; no new
`.scm` authoring. Captures already shipped for rust / python / javascript (see
tree-sitter-text-objects.md §5.1).

---

## 7. Composition

Because they register as ordinary motions, they compose with every operator,
count, and Visual mode with zero per-operator work:

```
	]f    cursor → next function        d]f   delete to next function start
	[f    cursor → this/prev fn start   y]F   yank through this function's end
	3]c   → 3rd next class              c]a   change to next argument
	v]l   Visual-extend to next loop    zn]f  narrow from here to next function (zn = narrow op)
	<C-o> after ]f returns (jumplist)   d]F   delete through function end (inclusive)
```

The `zn` narrow operator (narrow-mode.md §6) consumes the motion's range like any
operator — no special casing.

---

## 8. Performance + error handling

| Operation | Cost | Note |
|---|---|---|
| `scope_toward` | O(matches in the scanned half) | byte-range-restricted query; one-shot per keypress, on the **core/actor thread**, never in render |
| chord dispatch | trie lookup | same path as every 2-key motion |

Paramount goal #1 is safe: the query runs on a deliberate keypress on the core
thread — never per-frame, never on the UI thread, no element fan-out. A **bench**
(`scope_toward` on a large multi-thousand-line file, forward + backward) gates it
in `BENCHMARKS.md` + CI; if it ever shows cost, a cached sorted capture index is
the follow-up (not needed for v1 — the byte-range restriction already bounds the
scan). `None` returns are the graceful no-op of §4.6 — never a panic, never an
edit.

---

## 9. Rejected alternatives

**vim's `]m`/`]]` letter scheme** (methods on `m`, sections on `]]`). Rejected:
it splits one construct across two unrelated keys, is a C-brace-era artifact, and
doesn't generalize to parameter/loop. The object-letter scheme (`]f`/`]c`/`]a`/
`]l`) keeps objects and motions on one mnemonic per construct (§2).

**Wrap at file boundary** (like diff `]c` hunk nav). Rejected for operator safety:
a wrapping `d]f` at the last function would delete a cursor-to-top-of-file range.
Stop-at-boundary is predictable for a motion that composes with `d`/`c`/`y` (§4.4).

**`jump: false`** (vim `]m` parity). Rejected: structural hops are worth an
`<C-o>` undo, matching vim's `]]`/`[[`. Recording is gated to standalone Normal-
mode use so operator/Visual composition is unaffected (§4.5).

**A sibling `StructuralNav` trait** (single-responsibility split from
`ScopeResolver`). Rejected: adds a second handle type, context field, and host
coercion for two queries that share one tree walk; extending the existing trait is
cohesive and reuses all the plumbing (§5).

**Start-only (8 chords), deferring end-targets.** Rejected in favor of full
`{start, end} × {fwd, back}` symmetry — end-targets (`]F`) give operator reach
(`d]F` through a function) and the uppercase-end mnemonic is cheap to add now;
deferring would strand `d]F`-style edits behind a later slice.

**LSP-backed navigation** (`next-usage`, `]d` diagnostic). Out of scope: those
read the LSP index / diagnostics, not the parse tree, and are owned by the LSP
subsystem. This fragment is parse-tree motions only.
