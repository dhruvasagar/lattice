//! The project diff has to arrive *coloured*, in a real boot.
//!
//! Its styling rides the generic synthetic-highlight store, which the
//! provider reaches through the service registry. It asked for the handle
//! alias where the host registers the bare type, so the lookup missed — and a
//! miss is, by design, not an error: a test host with no highlight service
//! gets an uncoloured view rather than a failure. So the production editor
//! got one too, every time, and nothing said so.
//!
//! The tests beside this one seed the styling service by hand
//! (`begin_styling(view, None, None)`) and so never took the lookup. This
//! opens the view the way `:magit-project-diff` does, over a real repository
//! with a real change, and reads the buffer's highlight local.

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use lattice_cells::Style;
use lattice_grammar::AppEffect;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_line_in_the_project_diff_is_styled_as_added() {
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path();
    let file = dir.join("notes.txt");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
    git(dir, &["init", "-q"]);
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "first"]);
    std::fs::write(&file, "one\ntwo changed\nthree\n").unwrap();

    let mut editor = Editor::boot(lattice_core::Document::open(&file).unwrap());
    let before = editor.active_buffer_id();
    let mut out = DispatchOutcome::default();
    editor.apply_app_effect(
        AppEffect::OpenProviderView {
            provider: lattice_magit::providers::project_diff::PROVIDER_NAME.to_string(),
            args: lattice_grammar::Args::None,
        },
        &mut out,
    );

    // The scan is off-thread and its spans land on a tick; nothing is
    // dispatched while waiting, which is how it has to work for a user.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        editor.run_tick_pending();
        let view = editor.active_buffer_id();
        let styles: Vec<Style> = editor
            .buffer_locals
            .get(&view)
            .and_then(|l| l.get::<lattice_host::modes::ExtraHighlights>())
            .map(|h| h.0.iter().flatten().map(|s| s.style).collect())
            .unwrap_or_default();
        if view != before && styles.contains(&Style::DiffAdd) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the project diff never gained a `DiffAdd` span; styles seen: {styles:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
