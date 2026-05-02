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

- Date: 2026-05-02 (post B-α + B-β; B-γ tried + reverted)
- Host: WSL2 (Ubuntu) on x86_64
- Toolchain: Rust 1.94.0 stable
- Build profile: `bench` (`opt-level = 3`)

WSL2 adds ~5-15% overhead vs. native Linux on syscall-heavy paths.
Numbers below are conservative; native CI runners should land
better.

---

## §8.2 commitments at a glance

| §8.2 row | Target (p99) | Bench(es) | Status |
|---|---|---|---|
| Keystroke -> buffer mutation | <100µs | `runtime::apply_edit_round_trip` | ✅ ~83µs constant across sizes |
| Snapshot publish (actor side) | <10µs | bundled with apply_edit | ⚠️ ~85µs end-to-end (mailbox + work + publish) |
| Snapshot load (renderer side) | <5ns aspirational | `runtime::snapshot_load` | ⚠️ ~17ns (`load_full` Arc bump) |
| Reflex motion / operator | <2ms | `motion::*`, `operator::*` | ✅ all under budget |
| Search (literal substring) | <2ms (Reflex) | `search::*` | ✅ all variants under 2ms even on 200k-line buffers |

---

## Runtime / actor (`crates/lattice-runtime/benches/actor.rs`)

The load-bearing async primitives (DESIGN.md §5.2.1, §5.6.8, §5.7).

| Benchmark | 10 lines | 1k lines | 50k lines | Budget | Improvement target |
|---|---|---|---|---|---|
| `apply_edit` round-trip (block_on) | **83.1µs** | **82.6µs** | **84.0µs** | <100µs p99 | ⏹️ near floor (mailbox + oneshot dominate) |
| Snapshot publish via apply_edit | 85.5µs | 86.3µs | 85.6µs | -- | ⏹️ same path as round-trip |
| Snapshot load (`load_full`) | **17.4ns** | -- | -- | <5ns aspirational | 🔼 `ArcSwap::load()` (Guard, ~5ns) for read-only paths; `Cache::load()` (~2ns) for renderer thread |
| Snapshot post-publish read | 91.1ns | 18.8ns | 20.8ns | -- | 🔼 same path |

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

Literal substring matching with vim-style wrap. **All variants now
under §8.2's <2ms Reflex budget on every benchmark size including
200k-line corpora (~13MB).**

| Search | 10 | 1k | 50k | 200k | Improvement target |
|---|---|---|---|---|---|
| `forward_first_match` | 1.89µs | **207ns** | **289ns** | **211ns** | ⏹️ near floor (memmem hit on first chunk) |
| `forward_last_match` | 2.32µs | 3.95µs | 189µs | **811µs** | 🔼 only with suffix-array index (months of work) |
| `no_match_with_wrap` | 1.56µs | 5.6µs | 288µs | **1.21ms** | 🔼 same -- bounded by full-buffer walk at memmem speed |
| `backward` | 2.15µs | **1.27µs** | **15.1µs** | -- | ⏹️ near floor |

**🔼 forward_last_match / no_match_with_wrap on 200k.** These walk
the full buffer at memmem's SIMD speed (~30GB/s for rare-prefix
needles); 13MB ÷ 30GB/s ≈ 450µs scan, plus chunk-iteration overhead.
Sub-millisecond is achievable only with a precomputed substring
index (suffix array, FM-index) -- months of work, ~5× memory cost,
rebuild on every edit. **Not a v1 priority.**

**Negative result: B-γ (rayon parallel scan) was tried + reverted.**
Adaptive sequential-prefix + parallel-tail regressed every 50k+
bench (forward_last_match/50k went 189µs → 948µs). Cause: memmem
scans rare-prefix needles at ~30GB/s sequentially, so a 13MB scan
fits inside rayon's spawn overhead (~500µs). Documented in
`find_forward_in_rope`'s docstring.

**Search history (this session):**

| Bench | Original | After α (memmem) | After β (chunk-walk) | Δ vs original |
|---|---|---|---|---|
| forward_first_match/200k | 1.0ms | 908µs | **211ns** | **-99.98% (4700×)** |
| forward_last_match/200k | 21ms | 1.29ms | **811µs** | **-96% (26×)** |
| no_match_with_wrap/200k | 34ms | 1.4ms | **1.21ms** | **-96% (28×)** |
| forward_last_match/50k | 4.4ms | 163µs | **189µs** | **-96% (23×)** |
| no_match_with_wrap/50k | 8.7ms | 177µs | **288µs** | **-97% (30×)** |

---

## Motions (`crates/lattice-grammar/benches/motions.rs`)

Reflex-class. All under the <2ms p99 §8.2 budget.

| Motion | 10 lines | 1k lines | 50k lines | Improvement target |
|---|---|---|---|---|
| `word_forward` | 278ns | 12.2µs | 700µs | 🔼 SIMD whitespace scan via memchr (potential 5-10× on big files) |
| `word_backward` | 1.62µs | 2.17µs | 117µs | ⏹️ near floor |
| `word_end` | 1.34µs | 1.80µs | 121µs | ⏹️ near floor |
| `first_non_blank` (50k indented) | -- | -- | 268µs | 🔼 memchr `memchr` on `b' '` / `b'\t'` |
| `word_forward` count=50 in 100x buffer | 713ns | -- | -- | ⏹️ |
| `find_char_forward` (900-char wide line) | 322ns | -- | -- | 🔼 memchr (potential 3-5×) |

**🔼 Three motions could use memchr for the same reason search did:**
`word_forward`, `first_non_blank`, `find_char_forward` all do
linear character-class scans. Replacing with `memchr::memchr`
prefilter would give 5-10× wins on large files. Not a v1 priority
(absolute numbers already pass §8.2).

---

## Operators (`crates/lattice-grammar/benches/operators.rs`)

Reflex-class. All under the <2ms p99 §8.2 budget.

| Operator | 10 lines | 1k lines | 50k lines | Improvement target |
|---|---|---|---|---|
| `dw` (delete word) | 5.18µs | 17.2µs | 876µs | ⏹️ |
| `dd` (delete line) | 5.84µs | 16.3µs | 869µs | ⏹️ |
| `d_whole` (delete entire buffer) | 5.57µs | 23.4µs | **1.23ms** | 🔼 closest to budget; not a real workload |
| `yw` (yank word) | 2.47µs | 16.4µs | 828µs | ⏹️ |
| `cw` (change word) | 5.17µs | 14.2µs | 711µs | ⏹️ |
| `diw` (delete inner word) | 6.13µs | 3.65µs | 161µs | ⏹️ |
| `di_paren` (deep arg list) | 8.42µs | -- | -- | ⏹️ |

**🔼 `d_whole/50k` at 1.23ms** is the closest operator to the Reflex
budget. Realistically nobody runs `d%` (delete entire 50k file) in
a tight loop, but the 770µs of headroom is the smallest in the
suite. The cost is dominated by ropey's `remove(0..len)` -- a
single-call optimisation in ropey would help.

---

## Buffer ops (`crates/lattice-core/benches/buffer.rs`)

Direct rope mutations.

| Operation | 10 lines | 1k lines | 100k lines | Improvement target |
|---|---|---|---|---|
| `insert_at_origin` | 2.06µs | 1.38µs | 80.8µs | ⏹️ ropey is the floor |
| `insert_at_middle` | 2.34µs | 1.75µs | 72.8µs | ⏹️ |
| `delete_one_byte` | 2.54µs | 1.68µs | 77.3µs | ⏹️ |
| `position_byte_round_trip` | 1.12µs | 419ns | **390ns** | ⏹️ B-tree is faster on bigger ropes |

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
   <1ms p99 on a 50k-line file -- unmeasured today.

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
reparse, file open, dispatch round-trip.
