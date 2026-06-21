//! Perf plan B.4: dispatch publish-path bench.
//!
//! Measures `Editor::publish_render_state` across several regimes:
//!
//! - `keystroke_publish_{2000,100000}` (I.5 ratchet) — the
//!   per-keystroke publish cost on a content-loaded document: the
//!   whole-world `build_render_state` (active-document rebuild +
//!   `build_cells_panes` + the B2.3 windowed sync `DisplayMatrix`
//!   rebuild) that slice I.5 retires in favour of per-substate
//!   publication. This is the bar the ratchet drives **down** as
//!   the active-document cell split lands; the 2k vs 100k rows
//!   prove the cost stays O(viewport), flat across file size.
//!   Enforced ceiling: `tests/keystroke_publish_ratchet.rs`.
//!
//! - `steady_state` — publish N times back-to-back with no
//!   intervening mutation. Every cached sub-state (`panes` /
//!   `modes` / `buffer_locals` / inner `pane_highlights` / inner
//!   `lsp.progress`) MUST be reused via Arc-identity on every
//!   tick after the first. Captures the post-B.4 floor cost of a
//!   no-op publish.
//!
//! - `mutated_modes` — publish, mutate `active_modes` once,
//!   publish. The `modes` cache miss rebuilds; the others stay
//!   cached. Captures the cost of a single targeted cache
//!   invalidation.
//!
//! - `mutated_all` — publish, dirty all five cached fields,
//!   publish. Worst case for the cache (every slot rebuilds).
//!   Note: this row also pays for the bench-loop mutation work
//!   (5 HashMap insert/remove ops per iteration) so it
//!   over-counts vs the true pre-B.4 cost. Use `unmemoised`
//!   below for the clean comparison.
//!
//! - `unmemoised` — publish, then clear the cache before the
//!   next publish so every slot misses. No per-iteration
//!   mutation work; this approximates the pre-B.4 cost of an
//!   unconditional rebuild as closely as the bench can without
//!   reverting the cache implementation.
//!
//! Run:
//!
//!   cargo bench -p lattice-host --bench dispatch_publish
//!
//! The `steady_state` row should be a small fraction of the
//! `mutated_all` row when the cache is doing its job. Compare
//! before/after by stashing baseline numbers in
//! `docs/dev/operations/gpui-perf-plan.md`.

use criterion::{Criterion, black_box, criterion_group, criterion_main};

use lattice_cells::EditDelta;
use lattice_core::BufferId;
use lattice_core::Document;
use lattice_core::ui::pane::{PaneState, PaneTree, SplitOrientation};
use lattice_host::editor::Editor;
use lattice_host::versioned::Versioned;
use lattice_mode::{ActiveModes, BufferLocals};

/// Build an editor with a populated state so per-publish costs are
/// non-trivial: a multi-pane tree, several buffers' worth of
/// active_modes + buffer_locals, a non-empty lsp_progress map,
/// plus B.4.b-relevant load: 20 `buffer_uris`
/// (matching the active_modes count — typical LSP-attached session)
/// and 4 open tabs. Mirrors a mid-size editing session.
fn populated_editor() -> Editor {
    let mut editor = Editor::default();
    let mut tree = PaneTree::single(PaneState::default());
    tree.split_active(SplitOrientation::Vertical);
    tree.split_active(SplitOrientation::Horizontal);
    editor.pane_tree = Versioned::new(tree);

    for i in 1..=20u32 {
        let id = BufferId(i);
        editor.active_modes.insert(id, ActiveModes::default());
        editor.buffer_locals.insert(id, BufferLocals::new());
        // B.4.b: realistic buffer_uris population. Skips the
        // synthetic / unnamed scratch buffers but covers any
        // file-backed buffer the LSP would attach to.
        let uri = <lattice_lsp::Uri as std::str::FromStr>::from_str(&format!(
            "file:///tmp/bench/file_{}.rs",
            i
        ))
        .expect("synthetic file URI parses");
        editor.buffer_uris.insert(id, uri);
    }

    // ML.3c: the `$/progress` publish-cache slot is gone — progress is
    // accumulated in the lattice-lsp `LspProgressStore`, not the host
    // render snapshot — so the bench no longer seeds `editor.lsp_progress`.

    // B.4.b: 4 tabs (1 default + 3 extras) so the `tabs` cache
    // saves the build_tabs_render_state walk on no-op publishes.
    for _ in 0..3 {
        editor.tabs.push(lattice_core::ui::tab::TabSlot::new());
    }

    editor
}

/// Generates `line_count` synthetic Rust-ish lines (~80 chars
/// each) so the active document the publisher snapshots + the
/// cell-builder windows over has realistic line lengths rather
/// than an empty buffer. Mirrors `cells_worker.rs`'s helper.
fn synthetic_rust_doc(line_count: usize) -> Document {
    let body: String = (0..line_count)
        .map(|i| format!("fn handler_{i:04}(input: &str) -> Result<Output, Error> {{ Ok(()) }}\n"))
        .collect();
    Document::from_text(&body)
}

/// A booted editor with a `line_count`-line active document, a
/// 60-line viewport scrolled to the document's interior (so
/// `build_cells_panes` windows a realistic mid-file region, not
/// the cheap top-of-file case), and the cursor on the middle line.
fn editor_with_doc(line_count: usize) -> Editor {
    let mut editor = Editor::boot(synthetic_rust_doc(line_count));
    editor.viewport_height = 60;
    let mid = (line_count as u32) / 2;
    editor.scroll = mid.saturating_sub(30);
    editor.cursor.line = mid;
    editor.cursor.byte = 0;
    editor
}

/// I.5 ratchet baseline — the per-keystroke `publish_render_state`
/// cost on a content-loaded document. This is the whole-world
/// `build_render_state` (active-document rebuild + `build_cells_panes`
/// + the B2.3 windowed sync `DisplayMatrix` rebuild) that I.5
/// retires in favour of per-substate publication; the number here
/// is the bar the ratchet drives **down** as the active-document
/// cell split lands.
///
/// Two sizes prove the property I.5 must protect: the cost stays
/// **O(viewport)**, flat from 2k to 100k lines — a regression that
/// reintroduces an O(file) term shows up as the 100k row diverging
/// from the 2k row (and trips `tests/keystroke_publish_ratchet.rs`).
fn keystroke_publish(c: &mut Criterion) {
    let mut group = c.benchmark_group("dispatch_publish");
    for &n in &[2_000usize, 100_000usize] {
        let mut editor = editor_with_doc(n);
        editor.publish_render_state(); // warm the sub-state caches
        let edit_line = editor.cursor.line;
        group.bench_function(format!("keystroke_publish_{n}"), |b| {
            b.iter(|| {
                // Mimic the per-keystroke publish: an intra-line edit on the
                // cursor line (`EditDelta {_, 0, 0}`) drives the B2.3 windowed
                // sync rebuild inside `publish_render_state`, alongside the
                // whole-world `build_render_state` the publish runs today.
                editor.last_edit_for_cells = Some(EditDelta {
                    start_line: edit_line,
                    lines_removed: 0,
                    lines_added: 0,
                });
                editor.publish_render_state();
                black_box(editor.render_state.load_full());
            });
        });
    }
    group.finish();
}

fn steady_state(c: &mut Criterion) {
    let mut group = c.benchmark_group("dispatch_publish");
    let mut editor = populated_editor();
    editor.publish_render_state();

    group.bench_function("steady_state", |b| {
        b.iter(|| {
            editor.publish_render_state();
            black_box(editor.render_state.load_full());
        });
    });
    group.finish();
}

fn mutated_modes(c: &mut Criterion) {
    let mut group = c.benchmark_group("dispatch_publish");
    let mut editor = populated_editor();
    editor.publish_render_state();

    let toggle_id = BufferId(9_999);
    group.bench_function("mutated_modes", |b| {
        b.iter(|| {
            if editor.active_modes.contains_key(&toggle_id) {
                editor.active_modes.remove(&toggle_id);
            } else {
                editor
                    .active_modes
                    .insert(toggle_id, ActiveModes::default());
            }
            editor.publish_render_state();
            black_box(editor.render_state.load_full());
        });
    });
    group.finish();
}

fn mutated_all(c: &mut Criterion) {
    let mut group = c.benchmark_group("dispatch_publish");
    let mut editor = populated_editor();
    editor.publish_render_state();

    let toggle_id = BufferId(9_999);
    group.bench_function("mutated_all", |b| {
        b.iter(|| {
            // pane_tree: cheap version-bumping mutation that keeps
            // the tree shape stable.
            let leaves = editor.pane_tree.leaves().len();
            editor.pane_tree.set_active(leaves.saturating_sub(1));
            if editor.active_modes.contains_key(&toggle_id) {
                editor.active_modes.remove(&toggle_id);
            } else {
                editor
                    .active_modes
                    .insert(toggle_id, ActiveModes::default());
            }
            if editor.buffer_locals.contains_key(&toggle_id) {
                editor.buffer_locals.remove(&toggle_id);
            } else {
                editor.buffer_locals.insert(toggle_id, BufferLocals::new());
            }
            // ML.3c: progress no longer flows through publish, so the
            // per-iteration `$/progress` toggle is gone.
            editor.publish_render_state();
            black_box(editor.render_state.load_full());
        });
    });
    group.finish();
}

fn unmemoised(c: &mut Criterion) {
    let mut group = c.benchmark_group("dispatch_publish");
    let mut editor = populated_editor();
    editor.publish_render_state();

    group.bench_function("unmemoised", |b| {
        b.iter(|| {
            // Force every cached sub-state to miss without doing
            // any per-iteration mutation work — pre-B.4 equivalent.
            editor
                .publish_cache
                .lock()
                .expect("publish_cache mutex poisoned")
                .clear();
            editor.publish_render_state();
            black_box(editor.render_state.load_full());
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    keystroke_publish,
    steady_state,
    mutated_modes,
    mutated_all,
    unmemoised
);
criterion_main!(benches);
