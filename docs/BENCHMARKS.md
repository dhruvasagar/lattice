# Benchmark Results

Captured numbers from the criterion suite, indexed against the
performance commitments in DESIGN.md §8.2 (Floor / Target / Today
/ Stretch).

This document is a snapshot, not a moving record -- update it when a
deliberate perf change lands or a new bench is added; do not bump
numbers on every routine run. Commit history is the moving record.

Each row points back at the §8.2 commitment it backs, so a
regression here is identifiable as a violation of a specific
target rather than just a slower number.

> Run `cargo bench --workspace` to reproduce. Times shown are
> criterion's median estimate. Each row's outer `[low high]`
> bracket is the 95% confidence interval; we report the median.

---

## Environment

- Date: 2026-05-03 (post §8.2 restructure + bench overhaul: standalone snapshot publish, status segment, dispatch round-trip, frame render, native highlight, large file open)
- Host: WSL2 (Ubuntu) on x86_64
- Toolchain: Rust 1.94.0 stable
- Build profile: `bench` (`opt-level = 3`)

WSL2 adds ~5-15% overhead vs. native Linux on syscall-heavy paths.
Numbers below are conservative; native CI runners should land
better.

---

## §8.2 commitments at a glance

| §8.2 row                                     | Target (v1) | Today            | Bench                                                 | Status                                               |
|----------------------------------------------|-------------|------------------|-------------------------------------------------------|------------------------------------------------------|
| Snapshot load (`load_full`)                  | <20ns       | **16ns**         | `runtime::snapshot_load`                              | ✅ at floor for `load_full` semantics                |
| Snapshot load (`Cache::load`, steady)        | <500ps      | **305ps**        | `runtime::snapshot_load_cached`                       | ✅ ~50× faster than `load_full`; sub-nanosecond      |
| Snapshot publish standalone                  | <500ns      | **101ns**        | `runtime::snapshot_publish_standalone`                | ✅ at the floor (~80ns)                              |
| Status segment update                        | <100ns      | **56ns**         | `runtime::status_segment_update`                      | ✅ at the floor                                      |
| Apply-edit round-trip                        | <100µs      | 85µs             | `runtime::apply_edit_round_trip`                      | ✅ scheduler-bound; sync fast-path is the next lever |
| Dispatch round-trip (small buffer)           | <100µs      | 78–86µs          | `runtime::dispatch_round_trip`                        | ✅ same envelope as apply-edit                       |
| Frame render TUI 80×24 (highlight + compose) | <500µs      | ~192µs           | `highlight::rust_viewport` + `render::frame_24_lines` | ✅ under target                                      |
| Frame render TUI 200×60                      | <800µs      | ~325µs           | `highlight::rust_viewport` + `render::frame_60_lines` | ✅ under target                                      |
| Open 100MB log (rope construction)           | <100ms      | 76ms             | `buffer::open_large/100mb`                            | ✅ under target                                      |
| Search literal worst-case 200k               | <2ms        | 659µs            | `search::no_match_with_wrap/200k`                     | ✅ under target                                      |
| Tree-sitter incremental reparse              | <500µs      | unmeasured       | not built (Tree::edit not yet threaded)               | ⚠️ gap                                                |
| Reflex motion / operator                     | <2ms        | all under budget | `motion::*`, `operator::*`                            | ✅                                                   |
| LSP framing parse (Content-Length)           | <500ns      | **77ns**         | `lsp::framing::parse_header_block`                    | ✅ Background-class                                  |
| LSP encode `didChange`                       | <2µs        | **208ns**        | `lsp::encode::did_change`                             | ✅ per-keystroke debounced outgoing                  |
| LSP decode `publishDiagnostics`              | <10µs       | **1.58µs**       | `lsp::decode::publish_diagnostics`                    | ✅ per-save inbound                                  |
| LSP utf-16 column conversion (CJK line)      | <1µs        | **23ns**         | `lsp::position::utf16_cjk_line`                       | ✅ never shows up in flame graphs                    |

---

## Runtime / actor (`crates/lattice-runtime/benches/actor.rs`)

The load-bearing async primitives (DESIGN.md §5.2.1, §5.6.8, §5.7).

| Benchmark                                       | 10 lines   | 1k lines   | 50k lines  | Floor / Target         | Improvement target                                                                             |
|-------------------------------------------------|------------|------------|------------|------------------------|------------------------------------------------------------------------------------------------|
| **Snapshot publish standalone** (new)           | **~101ns** | **~101ns** | **~97ns**  | ~80ns / <500ns         | ⏹️ at the practical floor (Arc::new + atomic). Constant across sizes -- buffer clone is O(1).   |
| `apply_edit` round-trip (block_on)              | 83.1µs     | 82.6µs     | 84.0µs     | ~50µs / <100µs         | 🔼 sync edit fast-path drops to ~5µs (DESIGN.md §8.2 stretch).                                 |
| **Dispatch round-trip** (motion) (new)          | **~78µs**  | **~86µs**  | **~513µs** | ~50µs / <100µs (small) | ⏹️ scheduler-bound on small bufs; large-buf cost is the motion walk itself.                     |
| Snapshot publish via apply_edit                 | 85.5µs     | 86.3µs     | 85.6µs     | same as apply_edit     | (envelope, not standalone publish)                                                             |
| Snapshot load (`load_full`)                     | **~16ns**  | --         | --         | ~16ns / <20ns          | ⏹️ at the floor (atomic acquire + Arc bump).                                                    |
| **Snapshot load (`Cache::load`, steady)** (new) | **~305ps** | --         | --         | ~300ps / <500ps        | ⏹️ sub-nanosecond. Per-thread cached; ~50× faster than `load_full`. Renderer's per-frame read.  |
| Snapshot post-publish read                      | 91.1ns     | 18.8ns     | 20.8ns     | --                     | 🔼 same path                                                                                   |
| **Status segment update** (new)                 | **~56ns**  | --         | --         | ~50ns / <100ns         | ⏹️ at the floor (snapshot load + small format).                                                 |

**Round-trip is constant across buffer sizes** -- mailbox + oneshot
+ Arc clone, not a buffer walk. The ~85µs publish-via-apply-edit is
the *end-to-end* cost; the snapshot-construct + arc-swap-store is
sub-microsecond, bundled inside the round-trip.

**snapshot_load_cached at ~305ps** is the renderer's hot path;
`SnapshotCache::load` returns a borrowed reference to the cached
`Arc` after a single `Relaxed` atomic compare against the
underlying `ArcSwap` pointer. When the writer hasn't published
since the last load (the common case mid-frame), the compiler
inlines the compare to a register read. The fallback path
(`load_full`) stays available for callers that need an owned
`Arc` outside the cache's borrow lifetime.

The renderer migration to `SnapshotCache` is the §5.6.8 read-side
floor. The actor-internal write path is separate (see
"snapshot_publish_standalone" above).

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

| Provider              | small          | medium      | large        | Improvement target                                                                                                              |
|-----------------------|----------------|-------------|--------------|---------------------------------------------------------------------------------------------------------------------------------|
| `compute_indent`      | 2.4µs (10 fns) | 33µs (200)  | 326µs (2000) | ⏹️ linear in line count; pure rust, no allocations beyond the result vec                                                         |
| `compute_markdown`    | 0.96µs (10)    | 6.3µs (100) | 37µs (500)   | ⏹️ linear; ATX-heading scan + nesting walk                                                                                       |
| `compute_syntax_rust` | 70µs (10)      | 3.9ms (200) | 323ms (2000) | 🔼 `QueryCursor::matches` traversal; sub-linear past 200 fns. Phase 5/9 incremental reparse + per-pattern caching is the lever. |

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

## Native highlight (`crates/lattice-syntax/benches/highlight.rs`)

Times `Syntax::highlight_lines_native` per language across the
realistic call shapes the renderer actually issues.

| Benchmark                          | size              | time      | Floor / Target  | Improvement target                                                                  |
|------------------------------------|-------------------|-----------|-----------------|-------------------------------------------------------------------------------------|
| `highlight::rust/10`               | 81 lines          | **142µs** | ~120µs / —      | ⏹️ small-buffer setup floor (one full QueryCursor traversal).                        |
| `highlight::rust/200`              | ~1600 lines       | **3.2ms** | ~3ms / <5ms     | 🔼 per-pattern caching + pruning never-folded captures (~1ms achievable).           |
| `highlight::rust/2000`             | ~16k lines        | 42ms      | --              | 🔼 outlier (single-file 50kloc); the renderer never asks for full-buffer highlight. |
| **`highlight::rust_viewport/24`**  | 24-line viewport  | **178µs** | ~150µs / <300µs | ⏹️ realistic frame call shape. The renderer's keystroke path lives here.             |
| **`highlight::rust_viewport/60`**  | 60-line viewport  | **289µs** | ~250µs / <500µs | ⏹️                                                                                   |
| **`highlight::rust_viewport/120`** | 120-line viewport | **388µs** | ~350µs / <800µs | ⏹️                                                                                   |

**The viewport-bounded numbers are the meaningful ones.** The
full-buffer rows characterise worst-case query traversal cost
but the renderer never asks for the whole document at once. At
24 lines (typical 80×24 terminal) we're at 178µs, well under the
§8.2 frame-render budget.

`tree_sitter::QueryCursor::set_byte_range` is a hint -- the
cursor still walks the entire tree to find captures that overlap
the requested range. The ~178µs viewport floor reflects this; a
true viewport-bounded query traversal (Helix's `LanguageLayer`
incremental approach) is post-1.0 work.

---

## Frame render (`crates/lattice-ui-tui/benches/render.rs`)

Times `compose_visible_lines` against pre-warmed highlight cache
-- the per-frame view-composition cost on top of the highlight
work above.

| Benchmark                     | size             | time     | Floor / Target | Improvement target         |
|-------------------------------|------------------|----------|----------------|----------------------------|
| `render::frame_24_lines/200`  | 80×24, 200 fns   | **13µs** | ~10µs / <50µs  | ⏹️ at the practical floor.  |
| `render::frame_60_lines/200`  | 200×60, 200 fns  | **42µs** | ~30µs / <100µs | ⏹️                          |
| `render::frame_120_lines/200` | 200×120, 200 fns | **78µs** | ~70µs / <150µs | ⏹️                          |

The frame_60 / frame_120 rows ticked down ~15% after the renderer
migrated to `Cache::load` + a single per-frame snapshot
(`632310d`). `compose_visible_lines` no longer pays an internal
`load_full` per call, and `closed_fold_display_span` (called per
fold heading) no longer pays one each either. frame_24 is
unchanged within noise -- the savings scale with viewport height
because more visible folds = more eliminated loads.

Combined with viewport-bounded highlight (178µs at 24 lines), the
total per-frame cost on the editor side is ~192µs -- well under
the §8.2 "Frame render TUI <500µs" target. The remaining cost is
ratatui's terminal write, which isn't measured here (hardware-
bound; not benchable without a real TTY).

**Typed-options migration** (75e2390 → 1bfee16): the initial
landing of `lattice-config` regressed render frames by 25-57%
because every per-frame option read (`app.show_line_numbers()`,
`app.foldmethod()`, ...) went through the registry's mutex +
`ArcSwap` + downcast (~33ns per read; benchmarked in
`config::get_bool_via_handle`). At 60-120 visible lines × 2-4
reads per line, the path added multiple microseconds per frame.
The follow-up (commit landing this entry) restores baseline by
caching the option values on `App.option_cache`, refreshed via
the `Event::OptionChanged` cascade in `apply_option_cascade`.
Reads become field accesses (~1ns); the canonical value still
lives in `lattice-config::ConfigRegistry`, the cache is a
derived projection. Numbers above reflect the post-fix state.

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

| Open-large benchmark | size  | time      | throughput | Floor / Target | Improvement target                                             |
|----------------------|-------|-----------|------------|----------------|----------------------------------------------------------------|
| `buffer::open_large` | 10MB  | **3.4ms** | 2.9 GiB/s  | ~2ms / <10ms   | ⏹️ near floor (memcpy-ish into ropey's internal buffer).        |
| `buffer::open_large` | 100MB | **76ms**  | 1.3 GiB/s  | ~50ms / <100ms | ⏹️ B-tree split cost compounds; under §8.2 first-paint target.  |

`position_byte_round_trip` is *faster* on bigger ropes -- ropey's
B-tree packs better at scale.

---

## Typed options (`crates/lattice-config/benches/options.rs`)

The §5.12 typed-options registry. Read paths (`config.get` /
`config.with`) appear inside the renderer's per-line gutter
checks; write paths (`config.set` / `parse_and_set_command`) are
cmdline / plugin / customize-buffer triggered (cold). Hot-path
reads are cached on `App.option_cache` (~1ns field access);
these benches measure the underlying registry costs.

| Bench                                 | Time      | Floor / Target | Notes                                                                                                    |
|---------------------------------------|-----------|----------------|----------------------------------------------------------------------------------------------------------|
| `config::get_bool_via_handle`         | **34ns**  | ~30ns / <50ns  | Mutex acquire + `Arc::clone` + `as_any().downcast_ref` + `ArcSwap::load_full` + `Arc<bool>` deref.       |
| `config::with_int_via_handle`         | **26ns**  | ~25ns / <50ns  | Skips one `Arc::clone` vs `get`; the cheaper closure-style read.                                         |
| `config::lookup_by_name`              | **35ns**  | ~30ns / <100ns | HashMap probe + `Arc::clone`. Cmdline path uses this; not on the per-frame render hot path.              |
| `config::set_no_publisher`            | **134ns** | ~100ns / <500ns | Validate + `ArcSwap::store`. No event publisher wired -- baseline cost of the typed write.              |
| `config::set_with_publisher`          | **145ns** | ~120ns / <500ns | Same as above plus the publisher closure -- registry's contribution to the §5.10 `OptionChanged` flow.  |
| `config::parse_and_set_command_bool`  | **220ns** | ~180ns / <1µs  | Full cmdline path: `parse_set` + lookup + parse_and_set + format echo + publish.                         |

The `App.option_cache` projection (commit landing this entry)
turns the per-frame renderer reads into ~1ns field accesses.
The numbers above gate the *write* path -- which is cold on
keystroke flame graphs (only `:set` and rare programmatic
writes hit it). All comfortably under their targets.

---

## LSP wire layer (`crates/lattice-lsp/benches/lsp.rs`)

Per DESIGN.md §5.4 + §5.2.5, LSP requests are **Background**-class
(no sync-prelude budget). The wire-layer benches don't gate any
per-keystroke commitment; they exist to prove the plumbing
itself never appears next to editor work in a flame graph.

| Bench                                 | Time       | Floor / Target | Notes                                                                                              |
|---------------------------------------|------------|----------------|----------------------------------------------------------------------------------------------------|
| `lsp::framing::parse_header_block`    | **77ns**   | ~50ns / <500ns | One ASCII header block, ≤200 bytes. Runs once per inbound message.                                 |
| `lsp::encode::did_change`             | **208ns**  | ~150ns / <2µs  | One `TextDocumentContentChangeEvent` with a small replacement. Runs once per debounced keystroke.  |
| `lsp::decode::publish_diagnostics`    | **1.58µs** | ~1µs / <10µs   | Diagnostic with code + range + source + message + severity. Inbound on save / idle.                |
| `lsp::decode::small_response`         | **364ns**  | ~250ns / <2µs  | initialize / hover response shape.                                                                 |
| `lsp::encode_decode::hover_request`   | **878ns**  | ~600ns / <5µs  | Encode + decode round-trip (no I/O) for a typical request.                                         |
| `lsp::position::utf8_passthrough`     | **1.0ns**  | ~1ns / <5ns    | utf-8 negotiated mode short-circuits to a branch + return.                                         |
| `lsp::position::utf16_cjk_line`       | **23ns**   | ~20ns / <500ns | Worst case: 64-char CJK-only line, mid-line offset. Walks prefix counting utf-16 code units.       |
| `lsp::position::utf16_to_byte_cjk`    | **43ns**   | ~30ns / <500ns | Reverse direction: utf-16 column → utf-8 byte. Used for ranges arriving FROM the server.           |
| `lsp::logging::log_info`              | **91ns**   | ~80ns / <500ns | Per-record cost: lock + push + format + tracing fan-out. Background-class.                         |
| `lsp::logging::log_trace_off`         | **9ns**    | ~5ns / <50ns   | Trace toggle off short-circuit -- a HashSet lookup + return. Hot path when trace stays disabled.  |
| `lsp::logging::log_trace_on`          | **99ns**   | ~80ns / <500ns | Trace toggle on -- includes the ring push. Negligible at editor pace; perceptible at indexer bursts. |

The full LSP feature matrix (per-method status) lives in
[`lsp-features.md`](lsp-features.md); the architecture in
[`lsp-architecture.md`](lsp-architecture.md). Per-feature
benches (request round-trip latency end-to-end through a real
server) land alongside their features in 4.2 / 4.3 / 4.4.

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
