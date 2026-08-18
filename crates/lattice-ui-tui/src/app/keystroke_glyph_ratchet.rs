//! The keystroke→glyph ratchet — paramount goal #1's third clause.
//!
//! CLAUDE.md defines goal #1 in three examinable parts. The third is:
//!
//! > **Ratchet** — CI records the measured keystroke→glyph distribution
//! > and fails on regression; the bar only moves down, toward the
//! > I/O-hardware floor we can't beat.
//!
//! Until this file that clause had no implementation. `ci.yml`'s
//! `bench-baseline` job records a criterion artifact and explicitly
//! declines to gate on it ("shared-tenant runners have ~20% bench
//! variance"), and the one existing ceiling
//! (`lattice-host/tests/keystroke_publish_ratchet.rs`) covers only the
//! *publish* step, at one file size, at 25 ms — three frames.
//!
//! ## What is measured
//!
//! One full keystroke→glyph cycle, headless:
//!
//! ```text
//!   KeyEvent → input::translate → App::apply (dispatch + publish)
//!            → snapshot load → render::compose_visible_lines
//! ```
//!
//! Everything the editor controls. The terminal write (`Terminal::draw`)
//! is hardware-bound and excluded — the boundary `benches/render.rs`
//! already draws.
//!
//! The keystroke is an **inserted character**, the canonical case the
//! goal is written about ("the typed character appears immediately").
//! Each iteration types one char and deletes it *outside* the timed
//! region, so every sample sees an identically-sized line; timing a
//! growing line would make the distribution drift and the p99
//! meaningless.
//!
//! ## Steady state, not first keystroke — this is load-bearing
//!
//! Cost is **not** stable across a typing run. The first ~25–50
//! keystrokes are cheap; then an expensive path engages and stays
//! engaged. [`WARMUP`] burns those, so what is recorded is what a user
//! typing continuously actually experiences.
//!
//! A short measurement sees only the cheap prefix. That is very likely
//! how CLAUDE.md's descriptive "≈0.7 ms p50 (TUI, debug build,
//! 2300-line file)" arose: it matches this file's *pre-warm* number
//! almost exactly, and is ~10× under the steady state at that size.
//!
//! ## Why the gates are shaped this way
//!
//! `ci.yml`'s objection to bench gating is correct, so:
//!
//! - Timing tests are `#[ignore]`d and run in a dedicated serial CI job.
//!   Measured alongside this crate's other ~1690 tests, the same corpus
//!   read 0.64 ms serial and 35 ms parallel — a wall-clock gate cannot
//!   share a runner.
//! - Each corpus gates against its **own** recorded baseline with a
//!   loose factor. These catch further regression; they are not tight,
//!   because a shared runner cannot support tight absolute gating.
//! - The sharp statement lives in
//!   [`keystroke_to_glyph_is_flat_across_file_size`], which is
//!   `#[ignore]`d because **it fails today** — see its docs.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::App;
use crate::app::test_helpers::{app_with, press};

/// Keystrokes burned before sampling. Must exceed the ~50-keystroke
/// point where the expensive path engages (see the module docs), or the
/// recorded distribution describes a transient nobody types in.
const WARMUP: usize = 80;

/// Samples per corpus, after [`WARMUP`].
const ITERS: usize = 150;

/// Corpora spanning the anomaly documented in
/// [`keystroke_to_glyph_is_flat_across_file_size`]: two inside the
/// linear-cost band, one above the cutoff where the `O(file)` work stops
/// being observed.
const SMALL_LINES: usize = 2_000;
const MEDIUM_LINES: usize = 10_000;
const LARGE_LINES: usize = 100_000;

/// Recorded p99 baselines, **debug build**, measured 2026-08-18 on an
/// M1 Pro with `--test-threads=1`.
///
/// Debug is what `cargo test` runs and is 1–2 orders of magnitude slower
/// than the release path users get, so these are NOT user-facing latency
/// figures and must never be quoted as such. User-facing numbers come
/// from release benches — see `docs/dev/operations/benchmarks.md`.
///
/// **These move DOWN.** To lower one: run the job, read the `[ratchet]`
/// line, commit the new value with the change that earned it. Two of the
/// three are high because of the defect in
/// [`keystroke_to_glyph_is_flat_across_file_size`], not because the path
/// is inherently costly; fixing it should collapse them toward the
/// `LARGE` row.
const BASELINE_P99_SMALL: Duration = Duration::from_millis(9);
const BASELINE_P99_MEDIUM: Duration = Duration::from_millis(37);
const BASELINE_P99_LARGE: Duration = Duration::from_millis(2);

/// Headroom over a baseline before failing. Loose on purpose: `ci.yml`
/// measures ~20% variance on GitHub-hosted runners and a p99 amplifies
/// tail noise well past a median's, so a tight gate would flap and get
/// disabled — which is how a gate stops being read.
const REGRESSION_FACTOR: u32 = 2;

/// Maximum permitted spread between the cheapest and dearest corpus in
/// [`keystroke_to_glyph_is_flat_across_file_size`]. A viewport-bound
/// path sits near 1.0; the allowance covers rope depth (logarithmic in
/// document size, so legitimately non-flat) plus allocator noise.
const SCALE_TOLERANCE: f64 = 2.5;

/// `line_count` synthetic Rust-ish lines at ~80 chars. Mirrors the
/// corpus helpers in `benches/render.rs` and
/// `lattice-host/tests/keystroke_publish_ratchet.rs` so all three
/// measurements describe the same shape of document.
fn synthetic_rust_source(line_count: usize) -> String {
    (0..line_count)
        .map(|i| format!("fn handler_{i:04}(input: &str) -> Result<Output, Error> {{ Ok(()) }}\n"))
        .collect()
}

/// Nearest-rank percentile over a sorted sample vec.
fn percentile(sorted: &[Duration], p: f64) -> Duration {
    debug_assert!(!sorted.is_empty());
    let idx = ((sorted.len() as f64 * p).ceil() as usize).saturating_sub(1);
    sorted[idx.min(sorted.len() - 1)]
}

/// Boot an editor over `lines` of source, viewport 60, scrolled to the
/// interior, left in Insert mode.
///
/// Scrolling to the middle matters: composing at the top of a document
/// hits cache-warm, offset-free paths that would understate the cost.
fn booted(lines: usize) -> App {
    let mut app = app_with(&synthetic_rust_source(lines), 60);
    app.mutate_editor(move |e| {
        e.cursor.line = (lines / 2) as u32;
        e.cursor.byte = 0;
        e.scroll = (lines / 2).saturating_sub(30) as u32;
    });
    // Entering Insert is a mode transition, not a per-character
    // keystroke — do it outside every timed region.
    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
    );
    app
}

/// One timed cycle: type `c`, then compose the frame the user sees.
fn timed_keystroke(app: &mut App, c: char) -> Duration {
    let t = Instant::now();
    press(app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    // Re-load the snapshot each iteration rather than pinning one: the
    // runtime's frame path does `snapshot_cache.load_arc()` per draw,
    // and after an edit a pinned Arc is stale — composing it would
    // measure the wrong document.
    let snap = app.ad().snapshot.clone();
    let lines = crate::render::compose_visible_lines(app, &snap, 60, 200);
    let elapsed = t.elapsed();
    std::hint::black_box(lines);
    elapsed
}

/// Type one char and undo it, untimed. Keeps line length constant
/// between samples.
fn untimed_cycle(app: &mut App, c: char) {
    let _ = timed_keystroke(app, c);
    press(app, KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
}

/// [`WARMUP`] then [`ITERS`] samples, sorted ascending.
fn sample(app: &mut App) -> Vec<Duration> {
    for i in 0..WARMUP {
        untimed_cycle(app, char::from(b'a' + (i % 26) as u8));
    }
    let mut samples = Vec::with_capacity(ITERS);
    for i in 0..ITERS {
        let c = char::from(b'a' + (i % 26) as u8);
        samples.push(timed_keystroke(app, c));
        press(app, KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    }
    samples.sort_unstable();
    samples
}

/// Measure one corpus and print a machine-readable `[ratchet]` line —
/// the "records the measured distribution" half of the clause. Returns
/// `(p50, p99)`.
fn record(lines: usize) -> (Duration, Duration) {
    let mut app = booted(lines);
    let samples = sample(&mut app);
    let (p50, p95, p99) = (
        percentile(&samples, 0.50),
        percentile(&samples, 0.95),
        percentile(&samples, 0.99),
    );
    eprintln!(
        "[ratchet] keystroke_to_glyph lines={lines} build=debug \
         p50={p50:?} p95={p95:?} p99={p99:?}"
    );
    (p50, p99)
}

/// Gate `p99` against `baseline`, and flag a baseline gone slack —
/// a bar nobody lowers has stopped being a ratchet.
fn gate(lines: usize, p99: Duration, baseline: Duration) {
    let ceiling = baseline * REGRESSION_FACTOR;
    assert!(
        p99 <= ceiling,
        "{lines}-line corpus: keystroke→glyph p99 was {p99:?}, over the \
         {ceiling:?} ceiling ({baseline:?} baseline × {REGRESSION_FACTOR}). \
         Paramount goal #1 — the typed character must land within one display \
         frame under any load. Something joined the keystroke path: a parse, a \
         blocking read, an O(file) walk. Raising a baseline needs its \
         justification in the commit message; this ratchet moves DOWN."
    );
    if p99 * 3 < baseline {
        eprintln!(
            "[ratchet] NOTE: {lines}-line p99 {p99:?} is >3× under its \
             {baseline:?} baseline — lower the constant in this file."
        );
    }
}

macro_rules! perf_gate {
    ($name:ident, $lines:ident, $baseline:ident) => {
        #[test]
        #[ignore = "perf gate: needs a quiet machine. Run via ci.yml's \
                    keystroke-ratchet job, or locally with `cargo test -p \
                    lattice-ui-tui --lib keystroke_glyph_ratchet -- --ignored \
                    --test-threads=1`"]
        fn $name() {
            let (_, p99) = record($lines);
            gate($lines, p99, $baseline);
        }
    };
}

perf_gate!(
    keystroke_to_glyph_small_within_baseline,
    SMALL_LINES,
    BASELINE_P99_SMALL
);
perf_gate!(
    keystroke_to_glyph_medium_within_baseline,
    MEDIUM_LINES,
    BASELINE_P99_MEDIUM
);
perf_gate!(
    keystroke_to_glyph_large_within_baseline,
    LARGE_LINES,
    BASELINE_P99_LARGE
);

/// Scale invariance — the statement paramount goal #1 actually makes,
/// and **the one that fails today**.
///
/// Ignored so the suite stays green while the defect stays executable
/// rather than prose. Un-ignore when the finding below is fixed, and
/// collapse the three baselines above toward the `LARGE` row.
///
/// ## The finding
///
/// Steady-state keystroke→glyph cost is **linear in document line
/// count** across the range where most source files live, then stops
/// being observed above a cutoff between 20k and 50k lines. Measured
/// 2026-08-18, debug, serial, at iteration 199:
///
/// ```text
///   lines:  2 000   3 000   5 000   8 000  10 000  20 000  50 000  100 000
///   steady:  7.6ms  10.9ms  17.5ms  27.7ms  34.2ms  69.9ms  0.62ms   0.69ms
/// ```
///
/// A flat **~3.5 µs per line** from 2k to 20k. Two consequences:
///
/// 1. A 20 000-line file costs ~70 ms per keystroke in debug — eight
///    120 Hz frames — while a 100 000-line file costs 0.62 ms. The
///    common case pays; the exotic one does not.
/// 2. The cost engages only after ~25–50 keystrokes (before that every
///    size measures ~0.7 ms), so any short measurement misses it
///    entirely. [`WARMUP`] exists for this reason.
///
/// ## What is *not* yet established
///
/// The mechanism. `cells_worker::WINDOW_CAP_LINES` (2048) was the first
/// suspect and is **not** it — the linear band starts below that cap and
/// continues well past it, and `pick_chunk_size` only special-cases
/// `line_count <= 4 × viewport_height`. The leading hypothesis is that
/// the `O(file)` work is asynchronous and, above the cutoff, has simply
/// not *landed* within a keystroke — which would make 50k/100k's 0.62 ms
/// stale-cell work-not-yet-done rather than a virtue, and the defect
/// worse than the table suggests rather than better. Confirming that
/// needs a debugging pass, not more measurement.
#[test]
#[ignore = "FAILS: steady-state keystroke cost is O(file) between ~2k and ~20k \
            lines (~3.5us/line). See this test's docs; un-ignore when fixed."]
fn keystroke_to_glyph_is_flat_across_file_size() {
    let (small, _) = record(SMALL_LINES);
    let (medium, _) = record(MEDIUM_LINES);
    let (large, _) = record(LARGE_LINES);

    let lo = small.min(medium).min(large).as_secs_f64().max(f64::EPSILON);
    let hi = small.max(medium).max(large).as_secs_f64();
    let spread = hi / lo;

    eprintln!(
        "[ratchet] scale {SMALL_LINES}/{MEDIUM_LINES}/{LARGE_LINES} lines: \
         {small:?} / {medium:?} / {large:?} \
         (spread {spread:.1}×, tolerance {SCALE_TOLERANCE})"
    );

    assert!(
        spread <= SCALE_TOLERANCE,
        "keystroke→glyph cost spread {spread:.1}× across {SMALL_LINES}–{LARGE_LINES} \
         lines (tolerance {SCALE_TOLERANCE}×). The keystroke path must be \
         O(viewport), not O(file) — paramount goal #1, and the property that keeps \
         latency flat as files grow."
    );
}
