+++
title = "Embedded user-docs size budget"
+++


## Why this exists

User docs (`docs/user/*.md`) ship inside the lattice binary via
`include_str!` from `lattice-help::topics::builtin_topics()`. This
gives the editor a paramount-goal-aligned property: `:help` works
offline, with no filesystem layout assumption beyond the binary
itself.

The cost is linear in markdown volume. At 166 KB today, that cost
is ~0.5-1.5% of a typical Rust release binary — invisible. As the
doc set grows, the cost grows with it. Without a forcing function,
the embedding model would silently bloat the binary until it became
a real concern.

This doc + the
`embedded_user_docs_stay_under_size_budget` test in
`crates/lattice-help/src/topics.rs` together are that forcing
function.

## The budget

**512 KB** total across `docs/user/*.md`.

That's ~3× the current size (166 KB), giving room for roughly 30
docs of average size before the test fires. The number is
deliberately concrete: not "best effort," not "review periodically."
A CI run that crosses the line fails the build.

## What to do when the test fails

**Do not just bump the budget.** The budget is the trigger for
re-evaluating the embedding model, not a knob to relax until docs
fit.

The options, in increasing order of complexity:

### Option B — Compress embedded markdown

Wrap each `include_str!` in compressed deflate + decompress on
first access. Markdown compresses ~5-10×, so 1 MB raw → ~150 KB
embedded.

```rust
// Build-step (build.rs) gzips each docs/user/*.md into
// $OUT_DIR/<topic>.md.gz; topics.rs include_bytes!()s that.
static FOLDING: Lazy<String> = Lazy::new(|| {
    gunzip(include_bytes!(concat!(env!("OUT_DIR"), "/folding.md.gz")))
});
```

Tradeoffs:
- ✅ Self-contained binary preserved
- ✅ ~5× compression takes the budget another ~5× higher
- ❌ Adds `flate2` (or `miniz_oxide`) dependency
- ❌ Build script complexity
- ❌ First-access decompression cost (~µs per topic) — within `:help`
  open latency budget but worth measuring

### Option E — Compressed + lazy-decompress

Same as B but each topic's body lives in `OnceLock<String>`. Topics
never opened never decompress. For users who don't invoke `:help`
(most), runtime cost stays at zero.

Same tradeoffs as B; one extra wrapper.

### Option C — Feature-gated filesystem load

`#[cfg(feature = "external-docs")]` reads from
`$XDG_DATA_HOME/lattice/docs/` or a configurable path. Default
builds keep embedding; distros enable the feature and ship docs as
a separate package.

Tradeoffs:
- ✅ Distro-friendly
- ❌ Breaks self-contained binary for non-default builds
- ❌ Filesystem path resolution + handling complexity (XDG chain,
  permissions, fallbacks)
- ❌ Test / verification surface becomes environment-sensitive

### Rejected: Option D — Remote fetch

Embed names + summaries only; bodies served from a docs URL at
runtime. Rejected because it breaks the offline-works property
unconditionally.

## Recommended path when budget fires

**Option E (compressed + lazy).** It preserves every paramount-goal
property and is the smallest delta. The implementation is ~50 LOC
spread across `build.rs` and the registry constructor; the
dependency cost is `miniz_oxide` (~30 KB compiled, smaller than the
docs it lets us add).

Option C is the right fallback if distro packaging becomes a
priority and the maintenance surface for both modes is worth it.

## Why not act now

Current cost is below the noise floor. Pre-empting it would add a
dependency + a build step + a decompression code path for no
visible benefit today. Per CLAUDE.md heuristic #1 ("best long-term
fit beats easy implementation") — *and* its corollary "don't add
features beyond what the task requires" — the test + this doc are
the right answer until growth makes the work load-bearing.

## Anchor

- Test: `crates/lattice-help/src/topics.rs::embedded_user_docs_stay_under_size_budget`
- Embedding site: `crates/lattice-help/src/topics.rs::builtin_topics()`
- Doc coverage test: `every_user_doc_in_docs_user_is_registered_as_a_topic`
  (orthogonal — locks "every user doc IS reachable via :help"
  regardless of embedding strategy)
