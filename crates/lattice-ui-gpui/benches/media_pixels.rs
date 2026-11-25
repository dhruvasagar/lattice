//! PF.6b: what the GPUI pixel cache costs per frame while a PDF scrolls.
//!
//! Every frame that shows media, prepaint calls `MediaPixels::claim` for each
//! visible page and its two neighbours (PF.3b: the viewport plus one either
//! side), and paint calls `get` for each visible page. Both run on the UI
//! thread, so this is frame-budget work: it must stay a few hash lookups
//! however long the document is.
//!
//! - `frame_3_pages`: one frame with three pages on screen, all already
//!   drawn: three `get`s and nine refused `claim`s, the steady state while
//!   reading.
//! - `frame_3_pages_of_500_held`: the same frame with 500 pages held in the
//!   cache, to show the lookups do not grow with what is held.
//!
//! Run:
//!
//!   cargo bench -p lattice-ui-gpui --features window,bench-internals \
//!     --bench media_pixels

use std::sync::Arc;

use criterion::{Criterion, black_box, criterion_group, criterion_main};

use lattice_ui_gpui::editor_element::MediaPixels;

fn page(n: u32) -> lattice_cells::MediaSource {
    lattice_cells::MediaSource::PdfPage {
        path: "/d/paper.pdf".into(),
        page: n,
    }
}

fn block(n: u32) -> lattice_cells::MediaBlockRef {
    let mut b = lattice_cells::MediaBlock::from_source(page(n), None);
    b.height_lh = Some(40.0);
    b.revision = 1;
    Arc::new(b)
}

fn raster() -> Arc<gpui::RenderImage> {
    Arc::new(gpui::RenderImage::new(vec![image::Frame::new(
        image::RgbaImage::new(1, 1),
    )]))
}

/// A cache holding pages `0..held`, each drawn at height 800, revision 1.
fn holding(held: u32) -> MediaPixels {
    let pixels = MediaPixels::default();
    pixels.set_budget(usize::MAX);
    for n in 0..held {
        assert!(pixels.claim(&page(n), 800, 1));
        pixels.insert(page(n), raster(), 1, 800, 1);
    }
    pixels
}

/// What prepaint and paint ask of the cache for pages `first..first + 3`.
fn frame(pixels: &MediaPixels, blocks: &[lattice_cells::MediaBlockRef], first: u32) {
    pixels.begin_frame();
    for (i, b) in blocks.iter().enumerate() {
        let n = first + i as u32;
        for p in [Some(n), n.checked_add(1), n.checked_sub(1)]
            .into_iter()
            .flatten()
        {
            black_box(pixels.claim(&page(p), 800, 1));
        }
        black_box(pixels.get(b));
    }
}

fn bench_media_pixels(c: &mut Criterion) {
    let mut group = c.benchmark_group("media_pixels");

    let near = holding(5);
    let visible: Vec<_> = (1..4).map(block).collect();
    group.bench_function("frame_3_pages", |b| b.iter(|| frame(&near, &visible, 1)));

    let all = holding(500);
    let deep: Vec<_> = (250..253).map(block).collect();
    group.bench_function("frame_3_pages_of_500_held", |b| {
        b.iter(|| frame(&all, &deep, 250))
    });
    group.finish();
}

criterion_group!(benches, bench_media_pixels);
criterion_main!(benches);
