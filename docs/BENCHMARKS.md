# Benchmark Results

Captured numbers from the criterion suite, indexed against the
performance commitments in DESIGN.md §5.2.5, §5.6.8, §8.2.

This document is a snapshot, not a moving record -- update it when a
deliberate perf change lands or a new bench is added; do not bump
numbers on every routine run. Commit history is the moving record.

Each metric is annotated with an **improvement target**: ⏹️ for
"already at the practical floor", 🔼 for "more headroom available
via known engineering paths". The target column states the
*credible* next step, not aspirational ceilings.

> Run `cargo bench --workspace` to reproduce. Times shown are
> criterion's median estimate. Each row's outer `[low high]`
> bracket is the 95% confidence interval; we report the median.

---

## Environment

- Date: 2026-05-03 (post Option B migration: tree-sitter parse + highlight on a single owned Tree; folds benchmarks added)
- Host: WSL2 (Ubuntu) on x86_64
- Toolchain: Rust 1.94.0 stable
- Build profile: `bench` (`opt-level = 3`)

WSL2 adds ~5-15% overhead vs. native Linux on syscall-heavy paths.
Numbers below are conservative; native CI runners should land
better.

---

## §8.2 commitments at a glance

| §8.2 row                      | Target (p99)      | Bench(es)                        | Status                                              |
|-------------------------------|-------------------|----------------------------------|-----------------------------------------------------|
| Keystroke -> buffer mutation  | <100µs            | `runtime::apply_edit_round_trip` | ✅ ~83µs constant across sizes                      |
| Snapshot publish (actor side) | <10µs             | bundled with apply_edit          | ⚠️ ~85µs end-to-end (mailbox + work + publish)       |
| Snapshot load (renderer side) | <5ns aspirational | `runtime::snapshot_load`         | ⚠️ ~17ns (`load_full` Arc bump)                      |
| Reflex motion / operator      | <2ms              | `motion::*`, `operator::*`       | ✅ all under budget                                 |
| Search (literal substring)    | <2ms (Reflex)     | `search::*`                      | ✅ all variants under 2ms even on 200k-line buffers |

---

## Runtime / actor (`crates/lattice-runtime/benches/actor.rs`)

The load-bearing async primitives (DESIGN.md §5.2.1, §5.6.8, §5.7).

| Benchmark                          | 10 lines   | 1k lines   | 50k lines  | Budget            | Improvement target                                                                                 |
|------------------------------------|------------|------------|------------|-------------------|----------------------------------------------------------------------------------------------------|
| `apply_edit` round-trip (block_on) | **83.1µs** | **82.6µs** | **84.0µs** | <100µs p99        | ⏹️ near floor (mailbox + oneshot dominate)                                                          |
| Snapshot publish via apply_edit    | 85.5µs     | 86.3µs     | 85.6µs     | --                | ⏹️ same path as round-trip                                                                          |
| Snapshot load (`load_full`)        | **17.4ns** | --         | --         | <5ns aspirational | 🔼 `ArcSwap::load()` (Guard, ~5ns) for read-only paths; `Cache::load()` (~2ns) for renderer thread |
| Snapshot post-publish read         | 91.1ns     | 18.8ns     | 20.8ns     | --                | 🔼 same path                                                                                       |

**Round-trip is constant across buffer sizes** -- mailbox + oneshot
+ Arc clone, not a buffer walk. The ~85µs publish-via-apply-edit is
the *end-to-end* cost; the snapshot-construct + arc-swap-store is
sub-microsecond, bundled inside the round-trip.

**🔼 snapshot_load at 17ns.** `ArcSwap::load_full()` does one atomic
acquire-load + one Arc refcount bump. The wait-free `Cache::load`
(DESIGN.md §5.6.8 names it explicitly) is ~2ns but needs per-thread
state. Adding a `with_snapshot<R>(f)` method on `DocumentHandle`
that uses `ArcSwap::load() -> Guard<T>` would drop read-only paths
to ~5ns; the renderer's frame-loop loads could be cached for ~2ns.
Deferred — not bottlenecking any keystroke today.

---

## Search (`crates/lattice-core/benches/search.rs`)

Now backed by [`fancy-regex`](https://docs.rs/fancy-regex). Patterns
without backrefs/lookarounds route through the `regex` crate's
RE2-style DFA + SIMD literal prefilter; backref patterns fall back
to a bounded NFA. **All literal-search variants under §8.2's <2ms
Reflex budget; the 200k-line worst case beats the prior
memmem-only path by 40-50%.**

| Search                | 10     | 1k         | 50k        | 200k       | Improvement target                                     |
|-----------------------|--------|------------|------------|------------|--------------------------------------------------------|
| `forward_first_match` | 2.40µs | 1.19µs     | 2.28µs     | 2.23µs     | ⏹️ near floor (regex setup dominates on tiny scans)     |
| `forward_last_match`  | 2.33µs | 2.23µs     | **103µs**  | **469µs**  | ⏹️ near floor (literal prefilter at L2 bandwidth)       |
| `no_match_with_wrap`  | 1.56µs | 2.84µs     | **156µs**  | **659µs**  | ⏹️ near floor                                           |
| `backward`            | 2.38µs | 1.38µs     | **15.0µs** | --         | ⏹️ near floor                                           |

| Regex feature             | 50k        | Improvement target                                                                                       |
|---------------------------|------------|----------------------------------------------------------------------------------------------------------|
| `alternation`             | 2.61µs     | ⏹️ regex literal-set extraction handles `(foo\|bar\|baz)`                                                 |
| `class_quantifier`        | 1.11ms     | ⏹️ general DFA path; under Reflex budget                                                                  |
| `backref` (pathological)  | **169ms**  | 🔼 fancy-regex backtracking; bounded by 1M-iteration recursion limit. Add per-search timeout for safety. |

**Implementation notes.**
- 128KB scan window amortises fancy-regex's per-call setup
  (~5µs/call × ~800 chunks → ~5µs/call × ~100 windows on 13MB).
- `from_utf8_unchecked` on the window: rope chunks are `&str`
  (valid UTF-8); the drain logic preserves codepoint alignment via
  `round_down_utf8_boundary`. The unsafe call is gated by a module-
  level `#![allow(unsafe_code)]` with the safety argument inline at
  the call site. Every other module remains `unsafe_code = "deny"`.
- `MAX_MATCH_LEN = 8KB` bridge: matches longer than this AND
  spanning a window boundary are missed. Generous for editor
  patterns; pathological matches that span >8KB are not a v1
  concern.

**🔼 backref pathological pattern (169ms on 50k).** fancy-regex's
default backtrack limit is 1M iterations; our
`(handler_\d+)\b.*\b\1` pattern hits ~150ms before terminating.
For an editor we'd want a stricter per-search timeout (target:
abort at 50ms, surface "search timed out" to the user) -- needs
the cancellation token contract (DESIGN.md §5.2.5) to land first.
Today the search just runs to completion or hits the recursion
cap.

**Negative result: B-γ (rayon parallel scan) was tried + reverted.**
Adaptive sequential-prefix + parallel-tail regressed every 50k+
bench. memmem / fancy-regex scan rare-prefix patterns at L2
bandwidth, so a 13MB scan fits inside rayon's spawn overhead.
Documented in `find_forward_in_rope`'s docstring.

**Search history (this session):**

| Bench                    | Original | After α (memmem) | After β (chunk-walk) | After regex+window | Δ vs original       |
|--------------------------|----------|------------------|----------------------|--------------------|---------------------|
| forward_first_match/200k | 1.0ms    | 908µs            | 211ns                | 2.23µs             | **-99.78% (450×)**  |
| forward_last_match/200k  | 21ms     | 1.29ms           | 811µs                | **469µs**          | **-97.8% (45×)**    |
| no_match_with_wrap/200k  | 34ms     | 1.4ms            | 1.21ms               | **659µs**          | **-98% (51×)**      |
| forward_last_match/50k   | 4.4ms    | 163µs            | 189µs                | **103µs**          | **-97.7% (43×)**    |
| no_match_with_wrap/50k   | 8.7ms    | 177µs            | 288µs                | **156µs**          | **-98.2% (56×)**    |

Notable: regex+window beats the literal-only memmem path on every
50k+ benchmark. fancy-regex's literal prefilter on a 128KB window
runs faster than memmem's tighter window because the per-call setup
cost amortises across more bytes. The "near-cursor" small-buffer
case (`forward_first_match/200k = 2.23µs`) is slightly slower than
the prior memmem-only number (211ns) but still trivially under
budget.

---

## Motions (`crates/lattice-grammar/benches/motions.rs`)

Reflex-class. All under the <2ms p99 §8.2 budget.

| Motion                                   | 10 lines | 1k lines | 50k lines | Improvement target                                                |
|------------------------------------------|----------|----------|-----------|-------------------------------------------------------------------|
| `word_forward`                           | 278ns    | 12.2µs   | 700µs     | 🔼 SIMD whitespace scan via memchr (potential 5-10× on big files) |
| `word_backward`                          | 1.62µs   | 2.17µs   | 117µs     | ⏹️ near floor                                                      |
| `word_end`                               | 1.34µs   | 1.80µs   | 121µs     | ⏹️ near floor                                                      |
| `first_non_blank` (50k indented)         | --       | --       | 268µs     | 🔼 memchr `memchr` on `b' '` / `b'\t'`                            |
| `word_forward` count=50 in 100x buffer   | 713ns    | --       | --        | ⏹️                                                                 |
| `find_char_forward` (900-char wide line) | 322ns    | --       | --        | 🔼 memchr (potential 3-5×)                                        |

**🔼 Three motions could use memchr for the same reason search did:**
`word_forward`, `first_non_blank`, `find_char_forward` all do
linear character-class scans. Replacing with `memchr::memchr`
prefilter would give 5-10× wins on large files. Not a v1 priority
(absolute numbers already pass §8.2).

---

## Operators (`crates/lattice-grammar/benches/operators.rs`)

Reflex-class. All under the <2ms p99 §8.2 budget.

| Operator                         | 10 lines | 1k lines | 50k lines  | Improvement target                        |
|----------------------------------|----------|----------|------------|-------------------------------------------|
| `dw` (delete word)               | 5.18µs   | 17.2µs   | 876µs      | ⏹️                                         |
| `dd` (delete line)               | 5.84µs   | 16.3µs   | 869µs      | ⏹️                                         |
| `d_whole` (delete entire buffer) | 5.57µs   | 23.4µs   | **1.23ms** | 🔼 closest to budget; not a real workload |
| `yw` (yank word)                 | 2.47µs   | 16.4µs   | 828µs      | ⏹️                                         |
| `cw` (change word)               | 5.17µs   | 14.2µs   | 711µs      | ⏹️                                         |
| `diw` (delete inner word)        | 6.13µs   | 3.65µs   | 161µs      | ⏹️                                         |
| `di_paren` (deep arg list)       | 8.42µs   | --       | --         | ⏹️                                         |

**🔼 `d_whole/50k` at 1.23ms** is the closest operator to the Reflex
budget. Realistically nobody runs `d%` (delete entire 50k file) in
a tight loop, but the 770µs of headroom is the smallest in the
suite. The cost is dominated by ropey's `remove(0..len)` -- a
single-call optimisation in ropey would help.

---

## Folds (`crates/lattice-ui-tui/benches/folds.rs`)

Three computed fold providers; each measured across small / medium /
large corpora so a regression on either small-file ergonomics or
large-file scaling surfaces. Folds recompute on every reparse, so
the budget is "stay sub-frame on realistic buffers."

| Provider                  | small   | medium    | large    | Improvement target                                                                                                              |
|---------------------------|---------|-----------|----------|---------------------------------------------------------------------------------------------------------------------------------|
| `compute_indent`          | 2.4µs (10 fns) | 33µs (200) | 326µs (2000) | ⏹️ linear in line count; pure rust, no allocations beyond the result vec                                                         |
| `compute_markdown`        | 0.96µs (10) | 6.3µs (100) | 37µs (500) | ⏹️ linear; ATX-heading scan + nesting walk                                                                                       |
| `compute_syntax_rust`     | 70µs (10) | 3.9ms (200) | 323ms (2000) | 🔼 `QueryCursor::matches` traversal; sub-linear past 200 fns. Phase 5/9 incremental reparse + per-pattern caching is the lever. |

**The syntax provider's 200-fn time (3.9ms) is the relevant ceiling**
for real-world Rust files (typical ≤500 LOC). 2000-fn buffers are an
outlier (~50kloc in one file). The bench pre-parses the source into a
`Syntax` instance so the timing measures only fold-query work, not the
underlying tree-sitter parse.

`compute_syntax_rust` covers `function_item`, `struct_item`,
`impl_item`, `if_expression`, `match_expression`, `block`, etc. (see
`queries/rust/folds.scm`). The query traversal cost grows with the
number of pattern alternatives; pruning the captures we don't fold
visibly (e.g. `parameters`, `arguments` for very-short ranges) is the
next available optimization if 3.9ms ever pushes uncomfortable.

---

## Tree-sitter consolidation (Option B migration)

**Architectural change, 2026-05-03.** `lattice-syntax` previously ran
`tree_sitter_highlight::Highlighter` (which parses internally) AND a
separate `tree_sitter::Parser` for the folds query — two parses per
edit on every recognised buffer. The Option B migration ([Steps 1–4](../docs/IMPLEMENTATION.md))
collapsed both onto a single `Parser` + `Tree` owned by `Syntax`:

- One parse per edit. Highlight, folds, and any future query consumer
  (textobjects.scm, indents.scm, locals.scm) all walk the same `Tree`.
- `tree-sitter-highlight` dropped from the dep tree; the hand-rolled
  pipeline reads `highlights.scm` directly via `QueryCursor` with
  later-pattern-wins overlap resolution.
- Markdown injections (block→inline + fenced code blocks) recurse one
  level: a `\`\`\`rust ... \`\`\`` block inside markdown reuses the rust
  highlights query.

The user-visible win lands when the document/syntax actor (§5.7) is
threaded through with `Tree::edit` deltas — the seam exists today, and
incremental reparse will further reduce per-keystroke cost on large
buffers. A dedicated highlight bench is on the "what's NOT here" list
below.

---

## Buffer ops (`crates/lattice-core/benches/buffer.rs`)

Direct rope mutations.

| Operation                  | 10 lines | 1k lines | 100k lines | Improvement target                  |
|----------------------------|----------|----------|------------|-------------------------------------|
| `insert_at_origin`         | 2.06µs   | 1.38µs   | 80.8µs     | ⏹️ ropey is the floor                |
| `insert_at_middle`         | 2.34µs   | 1.75µs   | 72.8µs     | ⏹️                                   |
| `delete_one_byte`          | 2.54µs   | 1.68µs   | 77.3µs     | ⏹️                                   |
| `position_byte_round_trip` | 1.12µs   | 419ns    | **390ns**  | ⏹️ B-tree is faster on bigger ropes  |

`position_byte_round_trip` is *faster* on bigger ropes -- ropey's
B-tree packs better at scale.

---

## "Performance has regressed" warnings

Criterion reports several regressions vs. its stored baseline. The
baseline was captured before the actor refactor (commit `6d1bb24`):
buffer mutations now route through a tokio mailbox (~80µs round-
trip) instead of a direct sync `apply_edit` (~5µs).

**This is the architectural cost we accepted**, not a code
regression to fix. Specifically:

- Small-file motions / operators (10-line buffers) show 15–30%
  regression because the 80µs actor overhead dominates the few-µs
  buffer-walk cost.
- Large-file (50k-line) regressions compress (or vanish) because
  buffer work dominates.

Phase 4–7 work (LSP, plugin host) requires the actor; the
regression is what enabled them. The motions/operators benches
measure the *full path* through the dispatcher.

After the actor refactor lands as the new baseline, future runs
should not show these regressions; criterion's stored baseline can
be reset by deleting `target/criterion/`.

---

## Improvement paths (prioritized)

1. **🔼 Motion SIMD prefilters.** `word_forward`, `first_non_blank`,
   `find_char_forward` would benefit from `memchr` the same way
   search did. ~30 LOC each. Not blocking §8.2 today; ladder for
   "decisively better than neovim" framing.

2. **🔼 `with_snapshot<R>(f)` API on `DocumentHandle`.** Read-only
   paths drop from 17ns to ~5ns via `ArcSwap::load() -> Guard<T>`.
   Renderer-side `Cache` for ~2ns post-cache loads. Worthwhile when
   GPU rendering arrives and per-frame snapshot overhead matters.

3. **🔼 Frame-render bench.** `compose_visible_lines` itself isn't
   measured; we only have its parts. Bench would close §8.2 row
   "Frame render (code, 1080p) <2ms".

4. **🔼 Cmdline completion popup bench.** Vertico-style live filter
   isn't on the bench. Should be sub-millisecond on the registered
   command set.

5. **🔼 Tree-sitter incremental reparse bench.** §8.2 commits to
   <1ms p99 on a 50k-line file -- unmeasured today. Now that the
   parser is owned by `Syntax` (post Option B), the seam for
   `Parser::parse(.., Some(&old_tree))` exists; need to thread
   `Tree::edit` deltas from the document actor first.

5a. **🔼 Native highlighter bench.** `Syntax::highlight_lines_native`
   isn't measured directly. Worth adding a per-language bench that
   isolates parse + query traversal so future regressions on the
   single-parse architecture surface in CI.

6. **🔼 Open-100MB-log file bench.** §8.2 commits to <100ms first
   paint, <500ms full ready -- unmeasured.

7. **🔼 Dispatch round-trip via `DocumentHandle::dispatch`.** The
   motion + effect commit path (vs. `apply_edit` alone). Closes the
   "what does a real keystroke cost?" question.

8. **🔼 Suffix-array search index** (months of work; deferred). The
   only credible path to microsecond full-buffer scans on 200k-line
   corpora. ~5× memory cost; rebuild on every edit.

9. **🔼 Allocation discipline check** (DESIGN.md §A.6). Per-keystroke
   alloc count via `dhat-rs`. Catches refactor regressions before
   wall-clock benches do.

10. **🔼 Long-running session bench** (§A.6). 10K random invocations;
	assert no monotonic memory growth.

---

## What's NOT here

Benches we'd want before claiming §8.2 coverage but haven't built
yet (marked 🔼 above): frame render, completion popup, tree-sitter
incremental reparse, native highlighter per-language timing, file
open, dispatch round-trip.
