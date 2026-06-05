//! I.5 ratchet — enforced ceiling on the per-keystroke publish cost.
//!
//! Companion to the `dispatch_publish::keystroke_publish_*` criterion
//! bench (which records the number on `main`). This test is the CI
//! GATE: it boots a content-loaded editor and asserts a single
//! `publish_render_state` over a keystroke stays under a generous
//! absolute ceiling.
//!
//! Per `ci.yml` (GitHub-hosted runners have ~20% bench variance, so
//! tight statistical gating flaps), the ceiling is intentionally
//! orders of magnitude above the real cost: it catches a *gross*
//! regression — an `O(file)` term creeping back onto the
//! keystroke→glyph publish path (paramount goal #1) — without
//! tripping on runner jitter. As slice I.5 lands per-substate
//! publication (a keystroke publishes only the active-document
//! substate instead of the whole-world `build_render_state`), the
//! real cost drops toward the imperceptibility bar and this ceiling
//! ratchets down with it.

use std::time::{Duration, Instant};

use lattice_cells::EditDelta;
use lattice_core::Document;
use lattice_host::editor::Editor;

/// Generates `line_count` synthetic Rust-ish lines (~80 chars each)
/// so the active document the publisher snapshots + the cell-builder
/// windows over has realistic line lengths. Mirrors the helper in
/// `benches/dispatch_publish.rs` / `benches/cells_worker.rs`.
fn synthetic_rust_doc(line_count: usize) -> Document {
    let body: String = (0..line_count)
        .map(|i| format!("fn handler_{i:04}(input: &str) -> Result<Output, Error> {{ Ok(()) }}\n"))
        .collect();
    Document::from_text(&body)
}

/// Boots a 2000-line editor (the descriptive perf-reference size),
/// a 60-line viewport scrolled to the document interior, then times
/// the median per-keystroke `publish_render_state` over a run of
/// intra-line edits.
#[test]
fn keystroke_publish_stays_within_one_frame_ceiling() {
    let mut editor = Editor::boot(synthetic_rust_doc(2_000));
    editor.viewport_height = 60;
    editor.scroll = 1_000 - 30;
    editor.cursor.line = 1_000;
    editor.publish_render_state(); // warm the sub-state caches

    let iters = 200usize;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        // Mimic the per-keystroke publish: an intra-line edit on the
        // cursor line (`EditDelta {_, 0, 0}`) drives the B2.3 windowed
        // sync rebuild inside `publish_render_state`, alongside the
        // whole-world `build_render_state` the publish runs today.
        editor.last_edit_for_cells = Some(EditDelta {
            start_line: editor.cursor.line,
            lines_removed: 0,
            lines_added: 0,
        });
        let t = Instant::now();
        editor.publish_render_state();
        samples.push(t.elapsed());
    }
    samples.sort_unstable();
    let median = samples[iters / 2];

    eprintln!("[i.5-ratchet] keystroke publish median (2000 lines, debug): {median:?}");

    // Generous debug-mode ceiling: the real release cost is sub-ms;
    // debug (LTO off, opt-level 0 for workspace crates) inflates it,
    // and CI runners add jitter. 25 ms is orders of magnitude above
    // the measured cost yet well under the tens of ms an O(file)
    // regression would produce on a 2k-line file. Tighten as I.5
    // drives the median down.
    assert!(
        median < Duration::from_millis(25),
        "per-keystroke publish median was {median:?}; expected < 25 ms — \
         a gross regression in the publish path (likely an O(file) term \
         back on the keystroke→glyph path, paramount goal #1). \
         See slice-plans/input-latency.md I.5 + the dispatch_publish bench."
    );
}
