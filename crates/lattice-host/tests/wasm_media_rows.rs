//! IM.7 — the wire from the media cache to the screen.
//!
//! `maybe_refresh_wasm_media` filled a per-buffer cache and
//! `MediaVirtualRowProvider` read one, and nothing ever connected them: no
//! provider was constructed anywhere outside the provider's own unit tests, so
//! no inline image has ever reached a frame. These tests pin the connection.
//!
//! No WASM here: a native stub `AsyncMediaSource` stands in for a plugin
//! producer, exactly as `wasm_decoration_cache.rs` does for the decoration
//! twin. The WASM producer and the drain that registers it are proven in
//! `lattice-plugin-loader`.
//!
//! Note what the assertions deliberately do NOT do: dispatch a key. The rows
//! must exist after the produce lands, with nothing pressed — a test that
//! pressed something first would pass against the unwired version too.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_host::per_buffer_cache::PerBufferCacheExt;
use lattice_host::wasm_media::{
    PROVISIONAL_ROWS, UNREADABLE_ROWS, WasmMediaState, media_virtual_row_provider_id,
};
use lattice_mode::{
    AsyncMediaSource, MediaBlockRequest, MediaFuture, MediaSourceRegistry,
    MediaSourceRegistryHandle,
};

#[derive(Debug)]
struct StubProducer {
    id: u64,
    blocks: Vec<MediaBlockRequest>,
}

impl AsyncMediaSource for StubProducer {
    fn source_id(&self) -> u64 {
        self.id
    }
    fn produce(
        &self,
        _buffer: u64,
        _path: Option<std::path::PathBuf>,
        _lines: u32,
        _text: String,
    ) -> MediaFuture<'_> {
        let blocks = self.blocks.clone();
        Box::pin(async move { Ok(blocks) })
    }
}

fn block_at(line: u32) -> MediaBlockRequest {
    block_for(line, std::path::PathBuf::from("/tmp/shot.png"))
}

fn block_for(line: u32, path: std::path::PathBuf) -> MediaBlockRequest {
    MediaBlockRequest {
        anchor_line: line,
        path,
        alt: Some("a shot".into()),
        fit: lattice_cells::MediaFit::Contain,
    }
}

/// A real PNG, so the sizing pass has a genuine header to read.
fn write_png(dir: &std::path::Path, name: &str, w: u32, h: u32) -> std::path::PathBuf {
    let path = dir.join(name);
    image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]))
        .save(&path)
        .expect("write png");
    path
}

/// Publish the cell geometry a drawing peer would, and give the pane a width
/// in columns, so `media_geometry` resolves.
fn with_metrics(editor: &mut Editor, row_px: f32, col_px: f32, cols: u32) {
    editor.pane_tree.active_mut().viewport_width = cols;
    editor.dispatch(lattice_host::action::Action::SetCellMetrics { row_px, col_px });
}

fn registry_with(producer: StubProducer) -> MediaSourceRegistryHandle {
    let mut r = MediaSourceRegistry::new();
    r.register(Arc::new(producer));
    Arc::new(arc_swap::ArcSwap::from_pointee(r))
}

/// Drain notifies accumulated during boot so `landed` measures only the
/// refresh under test.
async fn settle(editor: &Editor) {
    while tokio::time::timeout(Duration::from_millis(100), editor.async_landed.notified())
        .await
        .is_ok()
    {}
}

/// Wait for the cache to hold blocks measured against `geometry`.
///
/// A single `async_landed` await is not enough here: by the second refresh
/// there are other async producers in the editor firing the same wake, so one
/// notification proves only that *something* landed. Poll the thing actually
/// under test instead — this is an eventually-consistent path by design, and
/// the assertion should say so.
async fn measured_at(
    editor: &Editor,
    buffer: lattice_core::BufferId,
    geometry: (f32, f32),
) -> bool {
    for _ in 0..40 {
        if editor
            .wasm_media
            .cache
            .get_for(buffer)
            .and_then(|c| c.geometry)
            == Some(geometry)
        {
            return true;
        }
        let _ =
            tokio::time::timeout(Duration::from_millis(50), editor.async_landed.notified()).await;
    }
    false
}

async fn landed(editor: &Editor) -> bool {
    tokio::time::timeout(Duration::from_secs(2), editor.async_landed.notified())
        .await
        .is_ok()
}

/// The produce lands and the rows exist — with no keystroke in between.
#[tokio::test]
async fn a_produced_block_becomes_virtual_rows_without_a_keystroke() {
    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\nc\nd\ne\n"));
    let buffer = editor.document_buffer_id;
    editor.wasm_media = WasmMediaState::with_registry(registry_with(StubProducer {
        id: 1,
        blocks: vec![block_at(2)],
    }));
    settle(&editor).await;

    editor.maybe_refresh_wasm_media();
    assert!(landed(&editor).await, "the produce must wake the paint");

    let providers = editor.virtual_row_providers.snapshot(buffer);
    assert_eq!(
        providers.len(),
        1,
        "the pump must register the buffer's media provider — without it the \
         cache it fills has no reader and no image ever draws"
    );
    assert_eq!(providers[0].id(), media_virtual_row_provider_id(buffer));

    let rows = providers[0].collect();
    assert_eq!(rows.len(), usize::from(PROVISIONAL_ROWS));
    assert!(
        rows.iter()
            .all(|r| r.anchor_line == 2 && r.kind == lattice_cells::VirtualRowKind::MediaBlock)
    );
}

/// IM.7a — a block is MEASURED, not left provisional.
///
/// Until this landed, `intrinsic` and `height_lh` stayed `None` forever:
/// `lattice_media::probe` and `block_geometry` had no production caller at
/// all. GPUI collects a media row for painting only when `height_lh` is
/// `Some`, so no block was ever collected, no decode was ever dispatched, and
/// every inline image in the editor was eight blank rows with its file name
/// in the middle.
#[tokio::test]
async fn a_measured_block_carries_its_intrinsic_size_and_drawn_height() {
    let dir = tempfile::tempdir().unwrap();
    // 200 wide in a 400px pane: `Contain` never upscales, so it draws at its
    // natural 100px, which is five 20px line-heights.
    let png = write_png(dir.path(), "shot.png", 200, 100);

    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\n"));
    let buffer = editor.document_buffer_id;
    with_metrics(&mut editor, 20.0, 10.0, 40);
    editor.wasm_media = WasmMediaState::with_registry(registry_with(StubProducer {
        id: 1,
        blocks: vec![block_for(0, png)],
    }));
    settle(&editor).await;

    editor.maybe_refresh_wasm_media();
    assert!(landed(&editor).await);

    let rows = editor.virtual_row_providers.snapshot(buffer)[0].collect();
    let block = rows[0]
        .media
        .as_ref()
        .expect("a media row carries its block");
    assert_eq!(block.intrinsic, Some((200, 100)), "the header was read");
    assert_eq!(
        block.height_lh,
        Some(5.0),
        "100px drawn at a 20px line height is five line-heights"
    );
    assert_eq!(
        rows.len(),
        5,
        "the reservation follows the measurement, not PROVISIONAL_ROWS"
    );
}

/// A wider pane draws a `Contain` image no larger — it never upscales — but a
/// NARROWER one shrinks it, and the reservation has to follow. Otherwise a
/// resize leaves the image drawn inside a box reserved for the old size.
#[tokio::test]
async fn resizing_the_pane_re_measures_the_block() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_png(dir.path(), "shot.png", 400, 200);

    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\n"));
    let buffer = editor.document_buffer_id;
    // 400px pane, 400px-wide image: drawn at natural size, 200px = 10 rows.
    with_metrics(&mut editor, 20.0, 10.0, 40);
    editor.wasm_media = WasmMediaState::with_registry(registry_with(StubProducer {
        id: 1,
        blocks: vec![block_for(0, png)],
    }));
    settle(&editor).await;
    editor.maybe_refresh_wasm_media();
    assert!(landed(&editor).await);
    assert_eq!(
        editor.virtual_row_providers.snapshot(buffer)[0]
            .collect()
            .len(),
        10
    );

    // Halve the pane: the image scales to 200×100, which is five rows.
    with_metrics(&mut editor, 20.0, 10.0, 20);
    editor.maybe_refresh_wasm_media();
    assert!(
        measured_at(&editor, buffer, (20.0, 200.0)).await,
        "a resize must re-measure; the document version did not change"
    );
    assert_eq!(
        editor.virtual_row_providers.snapshot(buffer)[0]
            .collect()
            .len(),
        5
    );
}

/// A measurement is taken once per path, not once per keystroke.
///
/// The pump refreshes on every document version, so a naive sizing pass opens
/// every referenced image on every keypress. Proven by DELETING the file after
/// the first measurement: a block that still knows its size cannot have
/// re-read the header.
#[tokio::test]
async fn a_measurement_is_not_retaken_on_every_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_png(dir.path(), "shot.png", 200, 100);

    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\n"));
    let buffer = editor.document_buffer_id;
    with_metrics(&mut editor, 20.0, 10.0, 40);
    editor.wasm_media = WasmMediaState::with_registry(registry_with(StubProducer {
        id: 1,
        blocks: vec![block_for(0, png.clone())],
    }));
    settle(&editor).await;
    editor.maybe_refresh_wasm_media();
    assert!(landed(&editor).await);
    assert_eq!(
        editor.virtual_row_providers.snapshot(buffer)[0]
            .collect()
            .len(),
        5
    );

    std::fs::remove_file(&png).unwrap();
    // A resize forces a fresh sizing pass without touching the document.
    with_metrics(&mut editor, 20.0, 10.0, 20);
    editor.maybe_refresh_wasm_media();
    assert!(measured_at(&editor, buffer, (20.0, 200.0)).await);

    let rows = editor.virtual_row_providers.snapshot(buffer)[0].collect();
    assert_eq!(
        rows[0].media.as_ref().unwrap().intrinsic,
        Some((200, 100)),
        "the intrinsic size was remembered, so the header was not re-read"
    );
    assert_eq!(rows.len(), 5, "and the re-fit is arithmetic on it");
}

/// A refresh that produces the same blocks must not ask for a repaint.
///
/// The pump runs on every document version — every keystroke — and a buffer's
/// pictures are the same after almost all of them. Bumping the generation
/// moves the provider's fingerprint, which rebuilds the virtual rows, and the
/// wake publishes render state and requests a paint. Doing that per keystroke
/// for an unchanged image is the per-keystroke work paramount #1 forbids.
#[tokio::test]
async fn an_unchanged_refresh_does_not_bump_the_paint_generation() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_png(dir.path(), "shot.png", 200, 100);

    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\n"));
    let buffer = editor.document_buffer_id;
    with_metrics(&mut editor, 20.0, 10.0, 40);
    editor.wasm_media = WasmMediaState::with_registry(registry_with(StubProducer {
        id: 1,
        blocks: vec![block_for(0, png)],
    }));
    settle(&editor).await;
    editor.maybe_refresh_wasm_media();
    assert!(landed(&editor).await);
    let generation = editor.wasm_media.generation.load(Ordering::Relaxed);

    // A fresh document version with the SAME image at the same anchor.
    let _ = editor
        .document
        .apply_edit(lattice_protocol::edit::Edit::insert(
            lattice_protocol::position::Position::new(1, 0),
            "c\n",
        ));
    editor.maybe_refresh_wasm_media();
    assert!(
        cached_version_advanced(&editor, buffer).await,
        "the refresh must still run and re-stamp the version"
    );
    assert_eq!(
        editor.wasm_media.generation.load(Ordering::Relaxed),
        generation,
        "same blocks ⇒ no fingerprint move, no rebuild, no paint request"
    );
}

/// Wait for the cache to carry a document version newer than the buffer had
/// when the last refresh landed.
async fn cached_version_advanced(editor: &Editor, buffer: lattice_core::BufferId) -> bool {
    let want = editor.document.snapshot().version;
    for _ in 0..40 {
        if editor
            .wasm_media
            .cache
            .get_for(buffer)
            .is_some_and(|c| c.document_version == want)
        {
            return true;
        }
        let _ =
            tokio::time::timeout(Duration::from_millis(50), editor.async_landed.notified()).await;
    }
    false
}

/// A file that cannot be measured is not a pending answer — it IS the answer.
/// One row, so the alt text has somewhere to sit, rather than eight blank
/// ones held for a picture that is never coming.
#[tokio::test]
async fn an_unreadable_file_reserves_one_row_for_its_alt_text() {
    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\n"));
    let buffer = editor.document_buffer_id;
    with_metrics(&mut editor, 20.0, 10.0, 40);
    editor.wasm_media = WasmMediaState::with_registry(registry_with(StubProducer {
        id: 1,
        blocks: vec![block_for(0, "/nowhere/missing.png".into())],
    }));
    settle(&editor).await;

    editor.maybe_refresh_wasm_media();
    assert!(landed(&editor).await);

    let rows = editor.virtual_row_providers.snapshot(buffer)[0].collect();
    assert_eq!(rows.len(), usize::from(UNREADABLE_ROWS));
    let block = rows[0].media.as_ref().unwrap();
    assert_eq!(
        block.height_lh, None,
        "unmeasured, so the peer draws alt text"
    );
    assert!(!block.alt.is_empty());
}

/// A peer that publishes no cell metrics — the TUI — keeps the provisional
/// reservation AND reads no image header at all. The path here does not
/// exist: if anything probed it, the block would have come back as
/// `UNREADABLE_ROWS`.
#[tokio::test]
async fn without_cell_metrics_nothing_is_measured_and_no_file_is_read() {
    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\n"));
    let buffer = editor.document_buffer_id;
    editor.wasm_media = WasmMediaState::with_registry(registry_with(StubProducer {
        id: 1,
        blocks: vec![block_for(0, "/nowhere/missing.png".into())],
    }));
    settle(&editor).await;

    editor.maybe_refresh_wasm_media();
    assert!(landed(&editor).await);

    let rows = editor.virtual_row_providers.snapshot(buffer)[0].collect();
    assert_eq!(
        rows.len(),
        usize::from(PROVISIONAL_ROWS),
        "no metrics means no measurement, so the provisional reservation stands"
    );
    assert_eq!(rows[0].media.as_ref().unwrap().intrinsic, None);
}

/// A second refresh must not register a second provider. The registry refuses
/// a duplicate id, so a leak here would be silent rather than loud.
#[tokio::test]
async fn refreshing_again_does_not_add_a_second_provider() {
    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\n"));
    let buffer = editor.document_buffer_id;
    editor.wasm_media = WasmMediaState::with_registry(registry_with(StubProducer {
        id: 1,
        blocks: vec![block_at(0)],
    }));
    settle(&editor).await;

    editor.maybe_refresh_wasm_media();
    assert!(landed(&editor).await);
    editor.maybe_refresh_wasm_media();

    assert_eq!(editor.virtual_row_providers.snapshot(buffer).len(), 1);
}

/// The last producer going away (`:plugin-unload`) takes the provider with it.
/// Leaving one registered costs the virtual-rows worker a call per wake to be
/// told there is nothing to draw.
#[tokio::test]
async fn unloading_the_last_producer_unregisters_the_provider() {
    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\n"));
    let buffer = editor.document_buffer_id;
    let registry = registry_with(StubProducer {
        id: 1,
        blocks: vec![block_at(0)],
    });
    editor.wasm_media = WasmMediaState::with_registry(registry.clone());
    settle(&editor).await;

    editor.maybe_refresh_wasm_media();
    assert!(landed(&editor).await);
    assert_eq!(editor.virtual_row_providers.snapshot(buffer).len(), 1);

    registry.store(Arc::new(MediaSourceRegistry::new()));
    editor.maybe_refresh_wasm_media();

    assert!(
        editor.virtual_row_providers.snapshot(buffer).is_empty(),
        "the provider must go when the last producer does"
    );
}
