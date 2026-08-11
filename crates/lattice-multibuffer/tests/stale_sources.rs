//! SS.2/SS.3 (2026-08-11): a multibuffer must not persist a source
//! whose on-disk content changed after the view loaded it.
//!
//! Design: `docs/dev/architecture/multibuffer-stale-sources.md`.
//!
//! The bug these guard: sources are SNAPSHOTS taken at view creation,
//! and `Document::save` writes every dirty one back. Without a baseline
//! that silently discards whatever changed the file externally — a
//! rebase, a formatter, another pane's `:w`.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_core::BufferId;
use lattice_grammar::CommandRegistry;
use lattice_multibuffer::{Excerpt, MultibufferDocumentHandle};
use lattice_runtime::{Document, spawn_document};

fn temp_path(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "lattice-ss-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

fn registry() -> lattice_grammar::CommandRegistryHandle {
    Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()))
}

/// A file-backed source, loaded the way every provider loads one.
fn file_source(path: &std::path::Path, text: &str) -> (BufferId, Arc<dyn Document>) {
    std::fs::write(path, text).unwrap();
    let id = BufferId::next();
    let doc = lattice_core::DocumentBuilder::default()
        .with_text(text)
        .with_path(path.to_path_buf())
        .build();
    let handle = spawn_document(id, doc, registry());
    (id, Arc::new(handle) as Arc<dyn Document>)
}

#[tokio::test]
async fn a_file_backed_source_records_a_baseline() {
    let path = temp_path("baseline");
    let (id, src) = file_source(&path, "one\ntwo\nthree\n");
    let mut sources: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
    sources.insert(id, src);

    let view =
        MultibufferDocumentHandle::new(sources, vec![Excerpt::new(id, 0, 2)], registry()).unwrap();

    assert!(
        view.source_fingerprint(id).is_some(),
        "a file-backed source must record what it looked like on disk"
    );
    let _ = std::fs::remove_file(&path);
}

/// A synthetic source has no file to conflict with, so it records
/// nothing and can never be judged stale.
#[tokio::test]
async fn a_pathless_source_records_no_baseline() {
    let id = BufferId::next();
    let doc = lattice_core::DocumentBuilder::default()
        .with_text("scratch\n")
        .build();
    let handle = spawn_document(id, doc, registry());
    let mut sources: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
    sources.insert(id, Arc::new(handle) as Arc<dyn Document>);

    let view =
        MultibufferDocumentHandle::new(sources, vec![Excerpt::new(id, 0, 0)], registry()).unwrap();

    assert!(view.source_fingerprint(id).is_none());
}

/// A refresh must re-baseline: carrying the old map forward would leave
/// fingerprints for sources that are gone and none for the new ones.
#[tokio::test]
async fn replace_excerpts_rebaselines_the_new_source_set() {
    let path_a = temp_path("rebase-a");
    let path_b = temp_path("rebase-b");
    let (id_a, src_a) = file_source(&path_a, "aaa\nbbb\n");
    let mut sources: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
    sources.insert(id_a, src_a);
    let view = MultibufferDocumentHandle::new(sources, vec![Excerpt::new(id_a, 0, 1)], registry())
        .unwrap();
    assert!(view.source_fingerprint(id_a).is_some());

    // Refresh onto an entirely different source.
    let (id_b, src_b) = file_source(&path_b, "ccc\nddd\n");
    let mut next: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
    next.insert(id_b, src_b);
    view.replace_excerpts(next, vec![Excerpt::new(id_b, 0, 1)]);

    assert!(
        view.source_fingerprint(id_b).is_some(),
        "the new source must be baselined"
    );
    assert!(
        view.source_fingerprint(id_a).is_none(),
        "the departed source's baseline must not linger"
    );
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}
