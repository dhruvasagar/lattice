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
use std::time::Duration;

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_host::wasm_media::{PROVISIONAL_ROWS, WasmMediaState, media_virtual_row_provider_id};
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
    MediaBlockRequest {
        anchor_line: line,
        path: std::path::PathBuf::from("/tmp/shot.png"),
        alt: Some("a shot".into()),
        fit: lattice_cells::MediaFit::Contain,
    }
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
