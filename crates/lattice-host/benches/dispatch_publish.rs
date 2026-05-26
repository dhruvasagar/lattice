//! Perf plan B.4: dispatch publish-path bench.
//!
//! Measures `Editor::publish_render_state` across three regimes:
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

use lattice_core::BufferId;
use lattice_core::ui::pane::{PaneState, PaneTree, SplitOrientation};
use lattice_host::editor::Editor;
use lattice_host::versioned::Versioned;
use lattice_mode::{ActiveModes, BufferLocals};

/// Build an editor with a populated state so per-publish costs are
/// non-trivial: a multi-pane tree, several buffers' worth of
/// active_modes + buffer_locals, a non-empty pane_highlights /
/// lsp_progress map, plus B.4.b-relevant load: 20 `buffer_uris`
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
        let uri = <lattice_lsp::Uri as std::str::FromStr>::from_str(
            &format!("file:///tmp/bench/file_{}.rs", i),
        )
        .expect("synthetic file URI parses");
        editor.buffer_uris.insert(id, uri);
    }

    for pane_idx in 0..3 {
        let spans = vec![Vec::new(); 60];
        editor.pane_highlights.insert(pane_idx, spans);
    }

    for i in 0..6 {
        let key = (
            std::sync::Arc::<str>::from("rust-analyzer"),
            format!("progress-{}", i),
        );
        editor.lsp_progress.insert(
            key,
            lattice_lsp::LspProgressUpdate {
                server_id: std::sync::Arc::<str>::from("rust-analyzer"),
                token: format!("progress-{}", i),
                kind: lattice_lsp::LspProgressKind::Report,
                title: Some(format!("Indexing {}", i)),
                message: None,
                percentage: Some(50),
                cancellable: false,
            },
        );
    }

    // B.4.b: 4 tabs (1 default + 3 extras) so the `tabs` cache
    // saves the build_tabs_render_state walk on no-op publishes.
    for _ in 0..3 {
        editor
            .tabs
            .push(lattice_core::ui::tab::TabSlot::new());
    }

    editor
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
                editor.active_modes.insert(toggle_id, ActiveModes::default());
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
    let toggle_pane: usize = 9_999;
    let toggle_progress = (
        std::sync::Arc::<str>::from("benchmark"),
        "toggle".to_string(),
    );
    group.bench_function("mutated_all", |b| {
        b.iter(|| {
            // pane_tree: cheap version-bumping mutation that keeps
            // the tree shape stable.
            let leaves = editor.pane_tree.leaves().len();
            editor.pane_tree.set_active(leaves.saturating_sub(1));
            if editor.active_modes.contains_key(&toggle_id) {
                editor.active_modes.remove(&toggle_id);
            } else {
                editor.active_modes.insert(toggle_id, ActiveModes::default());
            }
            if editor.buffer_locals.contains_key(&toggle_id) {
                editor.buffer_locals.remove(&toggle_id);
            } else {
                editor.buffer_locals.insert(toggle_id, BufferLocals::new());
            }
            if editor.pane_highlights.contains_key(&toggle_pane) {
                editor.pane_highlights.remove(&toggle_pane);
            } else {
                editor.pane_highlights.insert(toggle_pane, Vec::new());
            }
            if editor.lsp_progress.contains_key(&toggle_progress) {
                editor.lsp_progress.remove(&toggle_progress);
            } else {
                editor.lsp_progress.insert(
                    toggle_progress.clone(),
                    lattice_lsp::LspProgressUpdate {
                        server_id: std::sync::Arc::<str>::from("benchmark"),
                        token: "toggle".to_string(),
                        kind: lattice_lsp::LspProgressKind::Report,
                        title: Some("toggle".to_string()),
                        message: None,
                        percentage: None,
                        cancellable: false,
                    },
                );
            }
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
    steady_state,
    mutated_modes,
    mutated_all,
    unmemoised
);
criterion_main!(benches);
