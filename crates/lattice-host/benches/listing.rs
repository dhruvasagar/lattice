#![allow(clippy::unwrap_used, clippy::panic)]
//! DL.6: cost of opening and scrolling a large directory listing.
//!
//! DL.4/DL.5 moved oil and the file tree off bespoke paint paths that
//! walked `O(viewport)` rows by hand and onto the shared
//! cells/`DisplayMatrix` build, and DL.3b gave every row a leading
//! inlay whose colour is a `ResolvedTheme::get`. Both changes are
//! *expected* to be free — an indexed table read per row, and a build
//! the editor already runs for every document — but "expected to be
//! free" is exactly the claim a bench exists to stop anyone making on
//! trust. Paramount goal #1 is not upheld by reasoning about it.
//!
//! Three measurements:
//!
//! - `listing_open_oil` / `listing_open_file_tree` — cold open: read
//!   the directory, render the listing text, seed the Document,
//!   publish the icons. This is the one-shot cost the user pays on
//!   `:Oil` / `:Tree`, and it scales with the *directory*, not the
//!   viewport.
//! - `listing_scroll_publish/{500,5000}` — the per-frame cost that
//!   matters: scroll a full viewport in an already-open listing and
//!   republish. Swept over two directory sizes because the
//!   *comparison* is the assertion, not either number: this must stay
//!   flat in directory size, and a path that scales with the listing
//!   rather than the viewport breaks the §8.2 hot-path rule while
//!   looking perfectly fast at any single size.
//!
//! Run:
//!
//!   cargo bench -p lattice-host --bench listing

use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use lattice_core::Document;
use lattice_host::editor::Editor;

/// Entries in the benched directory. Large enough that anything
/// accidentally O(directory) per frame shows up against the
/// viewport-sized work, without making setup dominate the run.
const ENTRY_COUNT: usize = 5_000;

/// Sizes the per-frame bench sweeps. The POINT of the sweep is the
/// comparison, not either number: per-frame cost must be flat across
/// them. A per-frame path that scales with the directory is the §8.2
/// hot-path rule broken, and it would be invisible at a single size.
const SCROLL_SIZES: &[usize] = &[500, ENTRY_COUNT];

/// A directory of `entries` files, created once and reused across
/// runs.
///
/// Built outside the measured closure on purpose: creating it per
/// sample would measure the filesystem rather than the editor, and at
/// 5,000 files that cost would swamp everything under test.
fn dir_with(entries: usize) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lattice-bench-listing-{entries}"));
    let _ = std::fs::create_dir_all(&dir);
    // Idempotent across runs: only fill it if it is short.
    let existing = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
    if existing < entries {
        for i in 0..entries {
            // A spread of extensions so the icon lookup does real work
            // rather than hitting one match arm every time.
            let ext = ["rs", "md", "toml", "py", "ts", "txt"][i % 6];
            let _ = std::fs::write(dir.join(format!("entry{i:05}.{ext}")), "");
        }
    }
    dir
}

fn big_dir() -> std::path::PathBuf {
    dir_with(ENTRY_COUNT)
}

fn editor_with_viewport() -> Editor {
    let mut editor = Editor::boot(Document::from_text("scratch\n"));
    editor.viewport_height = 50;
    editor.pane_tree.active_mut().viewport_width = 120;
    editor
}

fn bench_open_oil(c: &mut Criterion) {
    let dir = big_dir();
    c.bench_function("listing_open_oil", |b| {
        b.iter_batched(
            editor_with_viewport,
            |mut editor| {
                black_box(editor.do_open_oil(Some(dir.clone())));
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_open_file_tree(c: &mut Criterion) {
    let dir = big_dir();
    c.bench_function("listing_open_file_tree", |b| {
        b.iter_batched(
            editor_with_viewport,
            |mut editor| {
                black_box(editor.do_open_file_tree(Some(dir.clone())));
            },
            BatchSize::SmallInput,
        );
    });
}

/// The per-frame half: scroll one viewport and republish, in an
/// already-open listing. Nothing here should scale with `ENTRY_COUNT`.
fn bench_scroll_publish(c: &mut Criterion) {
    let mut group = c.benchmark_group("listing_scroll_publish");
    for &size in SCROLL_SIZES {
        let dir = dir_with(size);
        let mut editor = editor_with_viewport();
        editor.do_open_oil(Some(dir));
        // Scroll within the smaller directory's bounds at both sizes,
        // so the two differ ONLY in how much listing sits off-screen —
        // which is the variable under test.
        let span = (SCROLL_SIZES[0] as u32).saturating_sub(100).max(50);

        group.bench_function(criterion::BenchmarkId::from_parameter(size), |b| {
            b.iter(|| {
                let next = editor.scroll + 50;
                editor.scroll = if next > span { 0 } else { next };
                editor.cursor.line = editor.scroll;
                black_box(editor.publish_render_state());
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_open_oil,
    bench_open_file_tree,
    bench_scroll_publish
);
criterion_main!(benches);
