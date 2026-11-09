//! A file opened at a revision is highlighted in a language a plugin
//! registered AFTER boot — which is how org arrives.
//!
//! MG.26c parsed the blob with the `Arc<LangRegistry>` service. That `Arc`
//! was a snapshot taken at boot, and a plugin language registers by
//! REPLACING the live registry, so the snapshot knew `.org` by extension
//! (detection reads a separate table) but had no grammar for it: the org
//! file at a revision was plain while the same file opened directly was
//! coloured. Its own tests used Rust, which is bundled, so they could not
//! see it.
//!
//! This registers a language after the editor has booted, opens a file in
//! it with `:magit-find-file`, and waits — ticking only when the editor is
//! woken, as a user's editor would — for the buffer's spans.

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use lattice_host::dispatch::DispatchOutcome;
use lattice_host::editor::Editor;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(status.status.success(), "git {args:?}: {status:?}");
}

fn ex(editor: &mut Editor, line: &str) {
    let mut out = DispatchOutcome::default();
    editor.execute_ex_line(line, &mut out);
    for effect in std::mem::take(&mut out.effects) {
        let _ = editor.handle_effect(effect);
    }
}

/// Tick only when `async_landed` fires, so content that wakes nothing fails.
async fn settle_woken(editor: &mut Editor, mut done: impl FnMut(&Editor) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if done(editor) {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let notified = editor.async_landed.clone();
        if tokio::time::timeout(left, notified.notified())
            .await
            .is_err()
        {
            return done(editor);
        }
        editor.run_tick_pending();
    }
}

fn spans_of(editor: &Editor, id: lattice_core::BufferId) -> usize {
    editor
        .buffer_locals
        .get(&id)
        .and_then(|l| l.get::<lattice_host::modes::ExtraHighlights>())
        .map(|h| h.0.iter().map(Vec::len).sum())
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_at_a_revision_is_highlighted_in_a_language_registered_after_boot() {
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path();
    let file = dir.join("notes.testlang-rev");
    std::fs::write(&file, "pub fn answer() -> u32 {\n    42\n}\n").unwrap();
    git(dir, &["init", "-q"]);
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "first"]);

    let mut editor = Editor::boot(lattice_core::Document::open(&file).unwrap());
    let live = editor.active_buffer_id();

    // After boot, as the plugin loader registers org: a grammar the bundled
    // set does not have, under an extension nothing else claims.
    let spec = lattice_syntax::registry::GrammarSpec {
        grammar: tree_sitter_rust::LANGUAGE.into(),
        highlights: Some(tree_sitter_rust::HIGHLIGHTS_QUERY.to_string()),
        folds: None,
        injections: None,
        indents: None,
        textobjects: None,
        conceal_rules: Vec::new(),
    };
    lattice_syntax::plugin_lang::register_with_grammar(
        "testlang-rev",
        &["testlang-rev"],
        &spec,
        0x4E_5E_02,
    )
    .expect("the test language registers");

    ex(&mut editor, "magit-find-file HEAD notes.testlang-rev");

    let arrived = settle_woken(&mut editor, |e| {
        let id = e.active_buffer_id();
        id != live && spans_of(e, id) > 0
    })
    .await;
    let id = editor.active_buffer_id();
    let text = editor
        .buffers
        .document_handle(id)
        .map(|h| h.snapshot().text().to_string());
    assert!(
        arrived,
        "no syntax spans on the revision buffer; active {id:?} (live {live:?}), \
         text {text:?}, message {:?}",
        editor.last_message.as_ref().map(|m| m.text.clone())
    );
}
