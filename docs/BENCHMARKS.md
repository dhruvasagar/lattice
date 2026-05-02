# Benchmark Results

Captured numbers from the criterion suite, indexed against the
performance commitments in DESIGN.md §5.2.5, §5.6.8, §8.2.

This document is a snapshot, not a moving record -- update it when a
deliberate perf change lands or a new bench is added; do not bump
numbers on every routine run. Commit history is the moving record.

> Run `cargo bench --workspace` to reproduce. Times shown are
> criterion's median estimate (the middle value of the bracket
> output). Each row's outer `[low high]` bracket is the 95%
> confidence interval; we report the median for stability.

---

## Environment

- Date: 2026-05-02
- Host: WSL2 (Ubuntu) on x86_64
- Toolchain: Rust 1.94.0 stable
- Build profile: `bench` (`opt-level = 3`)
- Concurrency: criterion default (single-threaded benches)

WSL2 adds ~5-15% overhead vs. native Linux on syscall-heavy paths.
Numbers below are conservative; native CI runners should land
better.

---

## §8.2 commitments at a glance

| §8.2 row | Target (p99) | Bench(es) | Status |
|---|---|---|---|
| Keystroke -> buffer mutation | <100µs | `runtime::apply_edit_round_trip` | ✅ ~80µs constant across sizes |
| Snapshot publish (actor side) | <10µs | bundled with apply_edit | ⚠️ ~85µs end-to-end (mailbox + work + publish) |
| Snapshot load (renderer side) | <5ns aspirational | `runtime::snapshot_load` | ⚠️ ~17ns (`load_full` Arc bump) |
| Reflex motion / operator | <2ms | `motion::*`, `operator::*` | ✅ all under budget |
| Search (literal substring) | -- | `search::*` | ⚠️ 200k-line buffers exceed Reflex |

---

## Runtime / actor (`crates/lattice-runtime/benches/actor.rs`)

The load-bearing async primitives (DESIGN.md §5.2.1, §5.6.8, §5.7).

| Benchmark | 10 lines | 1k lines | 50k lines | Budget |
|---|---|---|---|---|
| `apply_edit` round-trip (block_on) | **83.1µs** | **82.6µs** | **84.0µs** | <100µs p99 |
| Snapshot publish via apply_edit | 85.5µs | 86.3µs | 85.6µs | -- |
| Snapshot load (`Cache::load_full`) | **17.4ns** | -- | -- | <5ns aspirational |
| Snapshot post-publish read | 91.1ns | 18.8ns | 20.8ns | -- |

**Round-trip is constant across buffer sizes** -- mailbox + oneshot +
Arc clone, not a buffer walk. The ~85µs publish-via-apply-edit is the
*end-to-end* cost; the snapshot-construct + arc-swap-store is a small
fraction (sub-microsecond) bundled inside the round-trip.

**snapshot_load at 17ns** is `ArcSwap::load_full()` -- one atomic
acquire-load + one Arc refcount bump. The wait-free `Cache::load`
(DESIGN.md §5.6.8 names it explicitly) is ~2ns but needs per-thread
state, awkward across actor task boundaries. See "Improvement paths"
below.

---

## Motions (`crates/lattice-grammar/benches/motions.rs`)

Reflex-class. All under the <2ms p99 §8.2 budget.

| Motion | 10 lines | 1k lines | 50k lines |
|---|---|---|---|
| `word_forward` | 278ns | 12.2µs | 700µs |
| `word_backward` | 1.62µs | 2.17µs | 117µs |
| `word_end` | 1.34µs | 1.80µs | 121µs |
| `first_non_blank` (50k indented) | -- | -- | 268µs |
| `word_forward` (count=50, 100x buffer) | 713ns | -- | -- |
| `find_char_forward` (900-char wide line) | 322ns | -- | -- |

Worst case `word_forward/50000` at 700µs has 1.3ms of Reflex
headroom. `word_backward` is structurally faster than forward
because the rope's reverse iterator skips trailing whitespace
faster.

---

## Operators (`crates/lattice-grammar/benches/operators.rs`)

Reflex-class. All under the <2ms p99 §8.2 budget.

| Operator | 10 lines | 1k lines | 50k lines |
|---|---|---|---|
| `dw` (delete word) | 5.18µs | 17.2µs | 876µs |
| `dd` (delete line) | 5.84µs | 16.3µs | 869µs |
| `d_whole` (delete entire buffer) | 5.57µs | 23.4µs | **1.23ms** |
| `yw` (yank word) | 2.47µs | 16.4µs | 828µs |
| `cw` (change word) | 5.17µs | 14.2µs | 711µs |
| `diw` (delete inner word) | 6.13µs | 3.65µs | 161µs |
| `di_paren` (deep arg list) | 8.42µs | -- | -- |

**`d_whole/50000` at 1.23ms** is the closest to the Reflex budget.
Realistically nobody runs `d%` (delete the entire 50k-line file) in
a tight loop -- but the 770µs of headroom is the smallest margin in
the suite.

---

## Buffer ops (`crates/lattice-core/benches/buffer.rs`)

Direct rope mutations. Below the keystroke budget; this is what the
actor's `apply_edit` wraps.

| Operation | 10 lines | 1k lines | 100k lines |
|---|---|---|---|
| `insert_at_origin` | 2.06µs | 1.38µs | 80.8µs |
| `insert_at_middle` | 2.34µs | 1.75µs | 72.8µs |
| `delete_one_byte` | 2.54µs | 1.68µs | 77.3µs |
| `position_byte_round_trip` | 1.12µs | 419ns | **390ns** |

Notable: `position_byte_round_trip` is *faster* on bigger ropes.
ropey's B-tree packs better at scale; small ropes have more
overhead per node.

---

## Search (`crates/lattice-core/benches/search.rs`)

Literal substring matching with vim-style wrap. **Several rows
miss DESIGN budgets on adversarial buffer sizes.**

| Search | 10 | 1k | 50k | 200k lines |
|---|---|---|---|---|
| `forward_first_match` | 1.98µs | 1.80µs | 109µs | **1.00ms** |
| `forward_last_match` | 3.19µs | 87.5µs | 4.43ms | **21.06ms** |
| `no_match_with_wrap` | 2.35µs | 150µs | 8.70ms | **34.07ms** |
| `backward` | 2.14µs | 2.80µs | 127µs | -- |

A 200k-line code corpus is roughly 13MB. The current implementation
walks the rope char-by-char with no SIMD prefilter and no skip
table -- worst-case `O(n × pattern_len)`. See "Improvement paths"
below.

The 200k-line size is unusual but real (build logs, generated
diagnostics, vendored dependencies); fixing the gap is required for
DESIGN's §8.2 promise that motions degrade gracefully (cancel, not
blow) on adversarial input.

---

## "Performance has regressed" warnings

Criterion reports several regressions vs. its stored baseline. The
baseline is from before the actor refactor (commit `6d1bb24`):
buffer mutations now route through the actor (~80µs mailbox round-
trip) instead of a direct sync `apply_edit` (~5µs).

**This is the architectural cost we accepted**, not a code
regression to fix. Specifically:

- Small-file motions / operators (10-line buffers) show 15-30%
  regression because the 80µs actor overhead dominates the few-µs
  buffer-walk cost.
- Large-file (50k-line) regressions compress (or vanish) because
  buffer work dominates.

Phase 4-7 work (LSP, plugin host) requires the actor; the regression
is what enabled them. The motions/operators benches measure the
*full path* through the dispatcher; once cancellation tokens
(§5.2.5) and per-class deadline timers land, we'll add direct
"in-actor" benchmarks that exclude the round-trip cost.

After the actor refactor lands as the new baseline, future runs
should not show these regressions; criterion's stored baseline can
be reset by deleting `target/criterion/`.

---

## Improvement paths

Not yet pursued -- these are the known opportunities, in priority
order.

1. **Search SIMD prefilter** (`forward_first_match`,
   `forward_last_match`, `no_match_with_wrap` on 200k lines).
   Replace the char-by-char walk with `memchr`-driven byte scans
   (~2GB/s on AVX2) or delegate to `regex` with `regex::escape`.
   Target: <2ms p99 on 200k lines (current: 21-34ms).

2. **Snapshot load via `ArcSwap::load()`** (Guard) for read-only
   paths instead of `load_full()` (Arc). Estimated 17ns -> ~5ns.
   `Cache::load` (~2ns) for the renderer's frame-loop loads when a
   single-threaded renderer cache is feasible.

3. **Per-class bench groups** when `LatencyClass` becomes
   enforceable. Right now criterion benches are organised by crate;
   §5.2.5 wants assertions like "every Reflex command meets <2ms"
   without listing each. A single `cargo bench --bench latency_class
   -- reflex` style runner would synthesise the assertion.

4. **Allocation discipline** (DESIGN.md §A.6). `dhat-rs` per-
   keystroke alloc count. Catches refactor-introduced
   `format!`/`Vec::new` regressions before wall-clock benches do.

5. **Long-running session bench** (DESIGN.md §A.6). 10K random
   invocations; assert no monotonic memory growth.

---

## What's NOT here

Benches we'd want before claiming §8.2 coverage but haven't built
yet:

- Frame render (`compose_visible_lines` itself, not just the
  per-line work it composes from).
- Cmdline completion popup (vertico-style live filter).
- Tree-sitter incremental reparse (`<1ms` target on 50k-line files).
- Open-100MB-log-file (`<100ms` first paint, `<500ms` full ready).
- Dispatch round-trip via `DocumentHandle::dispatch` (motion +
  effect commit path; today only `apply_edit` is benched).
