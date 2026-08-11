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

// ── SS.3: verify at save ─────────────────────────────────────────────

/// Edit an excerpt, `:w`, source written. The baseline case — the guard
/// must not get in the way of the ordinary path.
#[tokio::test]
async fn an_unchanged_source_still_saves() {
    let path = temp_path("save-ok");
    let (id, src) = file_source(&path, "one\ntwo\nthree\n");
    let mut sources: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
    sources.insert(id, src.clone());
    let view =
        MultibufferDocumentHandle::new(sources, vec![Excerpt::new(id, 0, 2)], registry()).unwrap();

    src.apply_edit(lattice_protocol::Edit::insert(
        lattice_protocol::position::Position { line: 0, byte: 0 },
        "X".to_string(),
    ))
    .await
    .unwrap();

    assert!(view.save().await.is_ok(), "an untouched file must save");
    assert!(std::fs::read_to_string(&path).unwrap().starts_with('X'));
    let _ = std::fs::remove_file(&path);
}

/// THE regression. Change the file on disk behind the view, then save:
/// the source must NOT be written, and the external content must
/// survive intact. Asserting the disk content — not merely that an
/// error fired — is the point; a guard that warns and clobbers anyway
/// would pass a weaker test.
#[tokio::test]
async fn a_source_changed_on_disk_is_not_overwritten() {
    let path = temp_path("save-stale");
    let (id, src) = file_source(&path, "original\n");
    let mut sources: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
    sources.insert(id, src.clone());
    let view =
        MultibufferDocumentHandle::new(sources, vec![Excerpt::new(id, 0, 0)], registry()).unwrap();

    // The user edits the excerpt.
    src.apply_edit(lattice_protocol::Edit::insert(
        lattice_protocol::position::Position { line: 0, byte: 0 },
        "mine ".to_string(),
    ))
    .await
    .unwrap();

    // Someone else rewrites the file: a rebase, a formatter, another pane.
    std::fs::write(&path, "THEIRS — must survive\n").unwrap();

    let err = view.save().await.expect_err("a stale source must refuse");
    assert!(
        matches!(
            err,
            lattice_runtime::RuntimeError::SourcesChangedOnDisk { .. }
        ),
        "expected the typed stale-source error, got {err:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "THEIRS — must survive\n",
        "the external change must be intact — this is the data loss the guard exists to stop"
    );
    let _ = std::fs::remove_file(&path);
}

/// One stale source must not cost the user the other 29. Partial save:
/// the clean source persists, the stale one is reported.
#[tokio::test]
async fn a_stale_source_does_not_block_its_clean_neighbour() {
    let stale_path = temp_path("partial-stale");
    let clean_path = temp_path("partial-clean");
    let (stale_id, stale_src) = file_source(&stale_path, "old\n");
    let (clean_id, clean_src) = file_source(&clean_path, "fine\n");
    let mut sources: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
    sources.insert(stale_id, stale_src.clone());
    sources.insert(clean_id, clean_src.clone());
    let view = MultibufferDocumentHandle::new(
        sources,
        vec![Excerpt::new(stale_id, 0, 0), Excerpt::new(clean_id, 0, 0)],
        registry(),
    )
    .unwrap();

    for src in [&stale_src, &clean_src] {
        src.apply_edit(lattice_protocol::Edit::insert(
            lattice_protocol::position::Position { line: 0, byte: 0 },
            "Z".to_string(),
        ))
        .await
        .unwrap();
    }
    std::fs::write(&stale_path, "CHANGED\n").unwrap();

    let err = view.save().await.expect_err("the stale source is reported");
    match err {
        lattice_runtime::RuntimeError::SourcesChangedOnDisk { paths } => {
            assert_eq!(paths.len(), 1, "only the stale one is named");
        }
        other => panic!("expected SourcesChangedOnDisk, got {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(&stale_path).unwrap(),
        "CHANGED\n",
        "the stale source keeps its external content"
    );
    assert!(
        std::fs::read_to_string(&clean_path)
            .unwrap()
            .starts_with('Z'),
        "the clean source must still have persisted — one stale file \
         must not cost the user everything else"
    );
    let _ = std::fs::remove_file(&stale_path);
    let _ = std::fs::remove_file(&clean_path);
}

/// A bare `touch` bumps mtime without changing bytes. The content hash
/// is authoritative, so this must NOT be treated as stale — otherwise
/// the pre-gate would be making the decision.
#[tokio::test]
async fn a_touch_without_a_content_change_still_saves() {
    let path = temp_path("save-touch");
    let (id, src) = file_source(&path, "same bytes\n");
    let mut sources: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
    sources.insert(id, src.clone());
    let view =
        MultibufferDocumentHandle::new(sources, vec![Excerpt::new(id, 0, 0)], registry()).unwrap();

    src.apply_edit(lattice_protocol::Edit::insert(
        lattice_protocol::position::Position { line: 0, byte: 0 },
        "Q".to_string(),
    ))
    .await
    .unwrap();

    // Rewrite identical bytes: mtime moves, content does not.
    std::thread::sleep(std::time::Duration::from_millis(10));
    std::fs::write(&path, "same bytes\n").unwrap();

    assert!(
        view.save().await.is_ok(),
        "identical bytes are not a change — content hash is authoritative"
    );
    let _ = std::fs::remove_file(&path);
}
