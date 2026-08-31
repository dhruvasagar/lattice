//! PD.7c — what the project diff does once you edit inside it.
//!
//! The view is editable on purpose: you are reviewing thirty files, you spot
//! a typo in file nineteen, you fix it where you found it. The moment you do,
//! everything the scan derived from that file's content stops describing it.
//! The classification is in SOURCE line coordinates and an in-excerpt insert
//! does not resize the excerpt (`slide_anchors_for_source` only slides
//! excerpts an edit sits above), so the composed rows keep their indices while
//! the text under them moves — every tint below your edit then paints the
//! wrong line, and the deletion ghosts anchor a row out.
//!
//! Nothing announced that. The policy: drop the styling for the file you
//! edited, say so in the headerline, leave every other file alone, and let
//! `gr` rebuild.
//!
//! These drive the real path — a real edit to a real source document, with
//! the mode active — because the failure being prevented is a wiring one. A
//! test that called `mark_source_edited` directly would pass against a build
//! where the subscription was never made.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use lattice_core::{BufferFlags, BufferId, DocumentBuilder};
use lattice_host::editor::Editor;
use lattice_magit::providers::project_diff::{
    MagitProjectDiffMode, ProjectDiffComparison, ProjectDiffServiceHandle, ProjectDiffState,
};
use lattice_mode::ModeActivator;
use lattice_multibuffer::{Excerpt, HeaderlineStatus, create_multibuffer_view};
use lattice_runtime::spawn_document;

/// Two files, two excerpts each — enough that "clears the edited file" and
/// "leaves the others alone" are different assertions.
fn boot_view(
    editor: &mut Editor,
) -> (
    BufferId,
    BufferId,
    BufferId,
    Arc<dyn lattice_runtime::Document>,
) {
    let registry: lattice_grammar::CommandRegistryHandle = editor.registry.clone();
    let mut sources: HashMap<BufferId, Arc<dyn lattice_runtime::Document>> = HashMap::new();
    let mut excerpts = Vec::new();
    let mut first: Option<Arc<dyn lattice_runtime::Document>> = None;
    let ids = [BufferId(700), BufferId(701)];
    for (f, id) in ids.iter().enumerate() {
        let text: String = (0..12).map(|i| format!("f{f}-line{i}\n")).collect();
        let doc = DocumentBuilder::default()
            .with_text(&text)
            .with_path(std::path::PathBuf::from(format!("/repo/src/file{f}.rs")))
            .build();
        let handle: Arc<dyn lattice_runtime::Document> =
            Arc::new(spawn_document(*id, doc, registry.clone()));
        if *id == ids[0] {
            first = Some(Arc::clone(&handle));
        }
        sources.insert(*id, handle);
        excerpts.push(Excerpt::new(*id, 0, 2));
        excerpts.push(Excerpt::new(*id, 6, 8));
    }
    let registry_for_view = editor.registry.clone();
    let view = create_multibuffer_view(
        editor,
        sources,
        excerpts,
        Some("*test:project-diff*".into()),
        BufferFlags::default(),
        registry_for_view,
        None,
        lattice_multibuffer::FoldGrouping::SourceFile,
    );
    editor.activate_document(view);
    (view, ids[0], ids[1], first.expect("file A was spawned"))
}

/// Stand in for a completed scan: both files classified, plus the summary
/// the scan would have settled on.
fn seed_scan(svc: &ProjectDiffServiceHandle, view: BufferId, sources: &[BufferId]) {
    svc.begin_styling(view, None, None);
    for id in sources {
        svc.record_source_styling(
            view,
            *id,
            lattice_diff::overlay::DiffSignMap::default(),
            Vec::new(),
        );
    }
    svc.record_summary(
        view,
        "[project-diff: working tree] 4 hunks in 2 files".to_string(),
    );
}

fn headerline(editor: &Editor, view: BufferId) -> String {
    let Some(handle) = editor
        .services
        .get::<lattice_multibuffer::MultibufferRegistryHandle>()
        .and_then(|r| r.handle(view))
    else {
        return String::new();
    };
    match &*handle.headerline() {
        HeaderlineStatus::Complete { summary, .. } => summary.clone(),
        HeaderlineStatus::InProgress { label, .. } => label.clone(),
        other => format!("{other:?}"),
    }
}

/// Wait for the staleness policy to land, WITHOUT dispatching anything.
/// A test that pressed a key first would pass against a build whose
/// subscriber never fired.
async fn settle_until(editor: &Editor, view: BufferId, wanted: &str) -> String {
    for _ in 0..100 {
        let h = headerline(editor, view);
        if h.contains(wanted) {
            return h;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    headerline(editor, view)
}

#[tokio::test]
async fn editing_a_file_clears_that_files_diff_styling_and_says_so() {
    let mut editor = Editor::boot(lattice_core::Document::from_text("scratch\n"));
    let (view, file_a, file_b, file_a_doc) = boot_view(&mut editor);

    let svc = editor
        .services
        .get::<ProjectDiffServiceHandle>()
        .expect("magit boot registers the project-diff service");
    svc.set_state(
        view,
        ProjectDiffState {
            workdir: std::path::PathBuf::from("/repo"),
            comparison: ProjectDiffComparison::WorkingTree,
        },
    );
    seed_scan(&svc, view, &[file_a, file_b]);

    editor.activate_minor_by_id(view, MagitProjectDiffMode::mode_id());

    // The user types in file A's excerpt: the edit lands in the source
    // document, and the host publishes `DocumentChanged` for it — which is
    // what the multibuffer's own M.4 subscription turns into
    // `MultibufferSourceEdited`. Driving it from that event rather than
    // from a keystroke keeps the test to THIS slice's chain (substrate
    // translation → the mode's subscriber → policy → headerline) instead
    // of re-testing edit propagation.
    let edit = lattice_protocol::edit::Edit::insert(
        lattice_protocol::position::Position::new(1, 0),
        "inserted\n".to_string(),
    );
    let applied = file_a_doc.apply_edit(edit).await.expect("edit applies");
    editor
        .event_bus
        .publish(lattice_protocol::Event::DocumentChanged {
            id: file_a_doc.id(),
            path: Some(std::path::PathBuf::from("/repo/src/file0.rs")),
            version: 2,
            edits: vec![lattice_protocol::event::AppliedEdit {
                original_range: applied.original_range,
                inserted_range: applied.inserted_range,
                replaced_text: applied.replaced_text.clone(),
                inserted_text: applied.inserted_text.clone(),
            }],
        });

    let header = settle_until(&editor, view, "gr to refresh").await;
    assert!(
        header.contains("1 edited file"),
        "the headerline must name what went stale; got {header:?}"
    );
    assert!(
        header.contains("gr to refresh"),
        "…and the way back; got {header:?}"
    );
    assert!(
        header.contains("4 hunks in 2 files"),
        "the note EXTENDS what the view says it is rather than replacing it; \
         got {header:?}"
    );

    assert!(
        svc.is_source_edited(view, file_a),
        "the edited file is marked"
    );
    assert!(
        !svc.is_source_edited(view, file_b),
        "a file the user did not touch keeps its colouring — clearing the \
         whole view would punish every other file for one edit"
    );
}

/// A second keystroke in the same file must not redo the work: the styling
/// is already gone, and re-publishing per keystroke would be work for no
/// change on the hot path the user is typing on.
#[tokio::test]
async fn further_edits_to_the_same_file_are_not_reprocessed() {
    let mut editor = Editor::boot(lattice_core::Document::from_text("scratch\n"));
    let (view, file_a, file_b, _doc) = boot_view(&mut editor);
    let svc = editor
        .services
        .get::<ProjectDiffServiceHandle>()
        .expect("service");
    seed_scan(&svc, view, &[file_a, file_b]);

    // First edit marks it; the second finds it already marked.
    assert!(
        svc.mark_source_edited(view, file_a).is_some(),
        "first edit reports a new headerline"
    );
    assert!(
        svc.mark_source_edited(view, file_a).is_none(),
        "the second edit to the same file has nothing left to clear"
    );
    // …and a DIFFERENT file still reports, with the count grown.
    let second = svc
        .mark_source_edited(view, file_b)
        .expect("a second file going stale is news");
    assert!(
        second.contains("2 edited files"),
        "the count distinguishes one slip from a working session; got {second:?}"
    );
}
