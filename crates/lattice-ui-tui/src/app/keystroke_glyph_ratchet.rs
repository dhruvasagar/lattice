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
//! Cost is **not** stable across a typing run. The async cells worker
//! publishes its first `DisplayMatrix` only after a number of keystrokes
//! proportional to document size; until it exists the incremental
//! rebuild has nothing to reuse and no-ops, so early keystrokes are
//! cheap and unrepresentative. [`WARMUP`] burns them.
//!
//! ## Pane geometry must be set, not just `Editor::viewport_height`
//!
//! [`booted`] sets the **pane's** `viewport_height`. The cells worker's
//! chunked-mode threshold reads that, and `pick_chunk_size` returns
//! `WholeDoc` whenever it is 0 — so a harness that sets only the editor
//! field silently measures one whole-document chunk at every size. That
//! mistake produced a first round of numbers from this file that were
//! pure artefact (10 000 lines "measured" 34.9 ms; it is 1.6 ms).
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
//!   [`keystroke_to_glyph_is_flat_across_file_size`]. It passes on a
//!   quiet machine with ~12% headroom and is still skipped by CI's
//!   `keystroke-ratchet` job — see its docs for the residual
//!   `O(covered lines)` term that eats the rest.

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
/// line, commit the new value with the change that earned it. `SMALL`
/// sat at 9 ms while the guide layer read its covered range one rope
/// descent per line; streaming that range (2026-08-18) brought it to
/// ~1.6 ms p99 and this constant down with it. It remains the dearest
/// row because the residual `O(covered)` term described in
/// [`keystroke_to_glyph_is_flat_across_file_size`] still makes a
/// fully-resident 2 000-line file do more work than a windowed
/// 100 000-line one.
const BASELINE_P99_SMALL: Duration = Duration::from_millis(2);
/// [`SMALL_LINES`] of *nested* source — the corpus that exercises the
/// indent-guide layer's per-row resolution. See
/// [`keystroke_to_glyph_nested_within_baseline`].
const BASELINE_P99_NESTED: Duration = Duration::from_millis(3);

const BASELINE_P99_MEDIUM: Duration = Duration::from_millis(3);
const BASELINE_P99_LARGE: Duration = Duration::from_millis(2);

/// The floor with `display.indent-guides=off` — measured ~760 us p50 at
/// every corpus size. Shared by both sizes in
/// [`keystroke_to_glyph_floor_without_indent_guides`] precisely because
/// it should NOT vary with file size.
const BASELINE_P99_FLOOR: Duration = Duration::from_millis(2);

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

/// `line_count` lines of *nested* Rust-ish source, three levels deep,
/// with blank lines inside every block.
///
/// [`synthetic_rust_source`] is flat — every line starts at column 0 —
/// which makes it blind to anything whose cost scales with indentation
/// structure. That blindness was load-bearing once: the indent-guide
/// layer's per-row resolution was `O(covered lines × blocks)`, and the
/// flat corpus has no blocks, so the gates below could not see it at all
/// (it showed up in `benches/indent_guides.rs` instead). Real source is
/// nested; the ratchet needs one corpus that is.
///
/// Sized at [`SMALL_LINES`] deliberately — that is the band where the
/// display matrix covers the whole document, so the guide layer is
/// rebuilt over every line on every keystroke.
fn synthetic_nested_source(line_count: usize) -> String {
    let mut out = String::new();
    for i in 0..line_count.div_ceil(10) {
        out.push_str(&format!("fn nested_{i:04}(input: &str) -> usize {{\n"));
        out.push_str("    let mut total = input.len();\n");
        out.push('\n');
        out.push_str("    if total > 0 {\n");
        out.push_str("        for byte in input.bytes() {\n");
        out.push_str("            total += byte as usize;\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
        out.push_str("    total\n");
        out.push_str("}\n");
    }
    out
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
    booted_over(&synthetic_rust_source(lines), lines)
}

/// [`booted`] over a caller-supplied corpus. `lines` is what the cursor
/// and scroll are positioned by, so it must describe `src`.
fn booted_over(src: &str, lines: usize) -> App {
    let mut app = app_with(src, 60);
    app.mutate_editor(move |e| {
        // Pane geometry, NOT just `Editor::viewport_height`. The cells
        // worker's chunked-mode threshold reads the PANE's height
        // (`PaneCellsInputs::viewport_height`, sourced from
        // `PaneState`), and `pick_chunk_size` returns `WholeDoc`
        // whenever that is 0 — so a harness that only sets the editor
        // field measures a single whole-document chunk and an O(file)
        // rebuild per keystroke that production never performs. In
        // production the renderer supplies this via
        // `EditorCommand::SetPaneViewport` from the terminal size.
        {
            let pane = e.pane_tree.active_mut();
            pane.viewport_height = 60;
            pane.viewport_width = 200;
        }
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

/// The **floor**: what the keystroke path costs with the one known
/// `O(file)` consumer switched off.
///
/// This is the number the indent-guide fix is measured against, pinned
/// so the gate keeps its teeth while the residual `O(covered)` term
/// stands. Without it the only live bars are the guide-inclusive ones,
/// and a *second* regression could hide inside their headroom — which
/// is exactly what 9 ms of slack at [`SMALL_LINES`] did until the
/// streaming read brought that baseline to 2 ms.
///
/// It also asserts the property the inflated gates cannot: with guides
/// off the path is **flat across file size**, so this is the live
/// evidence that everything else on the keystroke path really is
/// `O(viewport)` and the guide pass is the sole offender.
#[test]
#[ignore = "perf gate: needs a quiet machine. Run via ci.yml's keystroke-ratchet \
            job, or locally with `cargo test -p lattice-ui-tui --lib \
            keystroke_glyph_ratchet -- --ignored --test-threads=1`"]
fn keystroke_to_glyph_floor_without_indent_guides() {
    let mut small = booted(SMALL_LINES);
    let mut large = booted(LARGE_LINES);
    for app in [&mut small, &mut large] {
        let _ = app
            .editor
            .config
            .set_typed::<lattice_config::core_options::IndentGuides>(false);
        app.editor.rebuild_option_cache();
    }

    let small_samples = sample(&mut small);
    let large_samples = sample(&mut large);
    let (small_p50, small_p99) = (
        percentile(&small_samples, 0.50),
        percentile(&small_samples, 0.99),
    );
    let (large_p50, large_p99) = (
        percentile(&large_samples, 0.50),
        percentile(&large_samples, 0.99),
    );

    eprintln!(
        "[ratchet] keystroke_to_glyph floor (indent-guides=off) build=debug \
         {SMALL_LINES}: p50={small_p50:?} p99={small_p99:?} | \
         {LARGE_LINES}: p50={large_p50:?} p99={large_p99:?}"
    );

    gate(SMALL_LINES, small_p99, BASELINE_P99_FLOOR);
    gate(LARGE_LINES, large_p99, BASELINE_P99_FLOOR);

    // Flatness, which the guide-inflated gates cannot assert. Same
    // process, same machine, so the ratio is a property of the code.
    let ratio = small_p50.as_secs_f64() / large_p50.as_secs_f64().max(f64::EPSILON);
    assert!(
        ratio <= SCALE_TOLERANCE,
        "with indent guides off, keystroke→glyph still scaled {ratio:.2}× from \
         {LARGE_LINES} to {SMALL_LINES} lines (tolerance {SCALE_TOLERANCE}×). A \
         SECOND O(file) term has joined the keystroke path — the guide pass was \
         the only known one when this gate was written."
    );
}

perf_gate!(
    keystroke_to_glyph_small_within_baseline,
    SMALL_LINES,
    BASELINE_P99_SMALL
);

/// The same size as [`keystroke_to_glyph_small_within_baseline`], over
/// *nested* source — see [`synthetic_nested_source`] for why a flat
/// corpus cannot stand in for one.
///
/// This is the gate that watches the indent-guide layer's per-row
/// resolution, and the reason it is worth its own corpus is the size of
/// what it was hiding. That resolution tested every block on every
/// covered row; at 2 000 nested lines (~600 blocks) it cost **14.69 ms
/// p50** — nearly two frames at 60 Hz, per keystroke — while the flat
/// corpus at the same line count read 1.41 ms and reported nothing
/// wrong. The active-set sweep (2026-08-18) brought it to **2.10 ms**.
///
/// Nested still costs ~50% more than flat here, and that residue is
/// real work: this corpus has guides to resolve, mark vectors to
/// allocate and glyphs to substitute, where the flat one has none.
#[test]
#[ignore = "perf gate: needs a quiet machine. Run via ci.yml's keystroke-ratchet \
            job, or locally with `cargo test -p lattice-ui-tui --lib \
            keystroke_glyph_ratchet -- --ignored --test-threads=1`"]
fn keystroke_to_glyph_nested_within_baseline() {
    let mut app = booted_over(&synthetic_nested_source(SMALL_LINES), SMALL_LINES);
    let samples = sample(&mut app);
    let (p50, p95, p99) = (
        percentile(&samples, 0.50),
        percentile(&samples, 0.95),
        percentile(&samples, 0.99),
    );
    eprintln!(
        "[ratchet] keystroke_to_glyph nested lines={SMALL_LINES} build=debug \
         p50={p50:?} p95={p95:?} p99={p99:?}"
    );
    gate(SMALL_LINES, p99, BASELINE_P99_NESTED);
}
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
/// and **the one with the least headroom**.
///
/// ## The finding: indent guides are O(covered lines) per keystroke
///
/// `sync_rebuild_pane_on_edit` calls `publish_indent_guides` over the
/// whole covered matrix on **every keystroke**. Coverage is the whole
/// document below `cells_worker::WINDOW_CAP_LINES` (2048) and a
/// viewport window above it, so the cost is O(file) for the file sizes
/// most source code actually occupies.
///
/// Toggling `display.indent-guides` is the controlled experiment
/// (debug, serial, p50):
///
/// ```text
///                  guides on   guides off
///    2 000 lines     7.86 ms      762 us    (2026-08-18, before)
///   10 000 lines     1.63 ms      752 us
///
///    2 000 lines     1.38 ms      812 us    (2026-08-18, after)
///  100 000 lines      639 us      633 us
/// ```
///
/// **What was fixed.** The layer read its covered range one line at a
/// time — one `O(log n)` rope descent per line, thousands per
/// keystroke. It now reads that range from a single `Lines` cursor
/// (`Buffer::line_shapes_from`), which is one descent plus a linear
/// walk. The per-line constant fell ~11×, the 2 000-line corpus went
/// 7.86 ms → 1.38 ms, and the spread across the three corpora went
/// ~12× → 2.2×, inside [`SCALE_TOLERANCE`].
///
/// **What was not.** The pass is still `O(covered lines)` and coverage
/// is still the whole document below the window cap, so a resident
/// 2 000-line file still does more work per keystroke than a windowed
/// 100 000-line one. That is the whole of the remaining 2.2×, and it is
/// why this gate is still skipped in CI (see `ci.yml`): 2.2 against a
/// 2.5 tolerance is passing on a quiet machine and flapping on a
/// shared-tenant runner, and a gate that flaps stops being read.
///
/// ## What the rest of the fix has to respect
///
/// `indent-guides.md` deliberately produces the layer beside the pane's
/// `DisplayMatrix` in the same `cells_worker` pass, because that is what
/// gives every visible pane coverage and keeps the active-block
/// highlight off the worker. A fix must keep that placement and make
/// the *extent computation* incremental or viewport-scoped — not move
/// guide production somewhere that reintroduces the problems that
/// placement solved.
#[test]
#[ignore = "perf gate: needs a quiet machine. Run via ci.yml's keystroke-ratchet \
            job, or locally with `cargo test -p lattice-ui-tui --lib \
            keystroke_glyph_ratchet -- --ignored --test-threads=1`"]
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
         (spread {spread:.1}x, tolerance {SCALE_TOLERANCE})"
    );

    assert!(
        spread <= SCALE_TOLERANCE,
        "keystroke->glyph cost spread {spread:.1}x across {SMALL_LINES}-{LARGE_LINES} \
         lines (tolerance {SCALE_TOLERANCE}x). The keystroke path must be \
         O(viewport), not O(file) — paramount goal #1, and the property that keeps \
         latency flat as files grow."
    );
}
