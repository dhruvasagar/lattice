//! IM.6b — the inline-media producer seam, driven through a real guest.
//!
//! Instantiates the `media-guest` fixture via
//! [`PluginHost::spawn_media_source`], drives its `media-blocks` producer
//! through the [`WasmMediaSource`] adapter + `MediaActor` bridge, and asserts
//! the native result — the whole seam end to end, OFF the render path:
//!
//!   - the owned `decoration-context` projection crosses in (the last block is
//!     keyed off `line_count`),
//!   - `list<media-block>` crosses back and converts to native requests,
//!   - a RELATIVE path resolves against the buffer's own directory,
//!   - an empty buffer degrades to a guest `err` the adapter surfaces, which
//!     the caller turns into "keep the prior blocks" rather than clearing.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier, WasmMediaSource};
use tempfile::TempDir;

fn guest_wasm() -> Option<&'static str> {
    let path = env!("MEDIA_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

async fn source(host: &PluginHost) -> WasmMediaSource {
    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .expect("compile media fixture");
    let manifest = PluginManifest::new("media-fixture", Vec::new(), CapabilitySet::empty());
    let (client, actor) = host
        .spawn_media_source(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::default(),
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
        )
        .await
        .expect("spawn media source");
    tokio::spawn(actor.run());
    WasmMediaSource::new(client)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_producer_crosses_context_and_returns_resolved_blocks() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: media fixture guest not built (add the wasm32-wasip2 target)");
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    let blocks = src
        .media_blocks(
            7,
            Some(std::path::Path::new("/home/u/notes/todo.org")),
            9,
            String::new(),
        )
        .await
        .expect("producer returns blocks");
    assert_eq!(blocks.len(), 2);

    // The relative path resolved against the BUFFER's directory — not the
    // editor's cwd, which is wherever the user happened to launch from.
    assert_eq!(
        blocks[0].path,
        std::path::Path::new("/home/u/notes/img/diagram.png")
    );
    assert_eq!(blocks[0].anchor_line, 1);
    assert_eq!(blocks[0].alt.as_deref(), Some("a wiring diagram"));
    assert_eq!(blocks[0].fit, lattice_cells::MediaFit::Contain);

    // Keyed off `line_count`, so this proves the context crossed in.
    assert_eq!(blocks[1].anchor_line, 8);
    assert_eq!(blocks[1].path, std::path::Path::new("/tmp/absolute.png"));
    assert_eq!(
        blocks[1].alt, None,
        "no alt ⇒ the file-name fallback applies"
    );
    assert_eq!(blocks[1].fit, lattice_cells::MediaFit::Width);
}

/// A guest that produces nothing this trigger returns a typed `err`, not a
/// trap. The caller keeps the buffer's prior blocks — a transient failure
/// mid-edit must not make every image in the document blink out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_buffer_degrades_to_a_guest_error_not_a_trap() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    let err = src
        .media_blocks(7, Some(std::path::Path::new("/a/b.org")), 0, String::new())
        .await
        .expect_err("empty buffer is a guest err");
    assert!(err.contains("empty buffer"), "got {err}");

    // And the source is still usable afterwards — an err is not a quarantine.
    let ok = src
        .media_blocks(7, Some(std::path::Path::new("/a/b.org")), 3, String::new())
        .await
        .expect("still alive after a guest err");
    assert_eq!(ok.len(), 2);
}

/// A relative path in a buffer with no path on disk is dropped rather than
/// guessed at: resolving against the cwd would open whatever happens to sit
/// beside the launch directory, and a wrong image is worse than none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relative_path_without_a_buffer_path_is_dropped() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    let blocks = src
        .media_blocks(7, None, 9, String::new())
        .await
        .expect("produces");
    assert_eq!(
        blocks.len(),
        1,
        "the relative block is dropped; the absolute one survives"
    );
    assert_eq!(blocks[0].path, std::path::Path::new("/tmp/absolute.png"));
}
