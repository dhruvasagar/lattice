//! A dropped editor takes its plugin host with it.
//!
//! Booting an editor stands up a plugin loader, and the loader owns a
//! `PluginHost`: a wasmtime engine, the epoch-ticker thread that makes its
//! deadlines fire, and wasmtime's cache-worker thread. Those are released when
//! the host is dropped — and for a long time it never was. The loader's
//! ex-commands captured it strongly, the command registry held those closures,
//! and the loader held the command registry: a cycle, described in the code as
//! benign because "both are app-lifetime boot services that never drop".
//!
//! That is true of a shipped editor, which boots once. It is not true of a
//! process that boots many, and the test suite is one: every editor stranded a
//! host and two threads. `lattice-ui-tui`'s suite climbed to ~2,900 live
//! threads, hit the per-process cap on macOS (323 tests failing with "failed
//! to spawn thread"), and on a four-core Linux runner spent an hour being
//! woken by a thousand tickers.
//!
//! The assertion is on the loader, not on a thread count: the count is the
//! symptom and varies by platform, while "nothing still holds the loader" is
//! the property, and it is the same everywhere.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

#[test]
fn the_plugin_loader_does_not_outlive_its_editor() {
    let editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let loader = {
        let handle = editor
            .services
            .get::<lattice_plugin_loader::PluginLoaderHandle>()
            .expect("the plugin loader is registered at boot");
        Arc::downgrade(&*handle)
    };
    assert!(
        loader.strong_count() > 0,
        "precondition: the editor holds its loader while it is alive"
    );

    drop(editor);

    // The loader's own background tasks hold it weakly and let go on the
    // shared runtime, so the last reference can lag the drop by a moment.
    let deadline = Instant::now() + Duration::from_secs(10);
    while loader.strong_count() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        loader.strong_count(),
        0,
        "something still holds the plugin loader after its editor was dropped; \
         its host's engine and threads are stranded with it"
    );
}
