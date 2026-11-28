//! `:reload-config` re-reads the config files, not only `init.rs`.
//!
//! Before, the command rebuilt `init.rs` and reconciled `[plugins]`; an edit
//! anywhere else in `lattice.toml` waited for a restart and nothing said so.
//! These drive the host's half of the reload end to end — the off-thread
//! reading, its landing on the editor, and the event the other owners of a
//! table re-read on — against real files.
//!
//! Every wait ticks only when `async_landed` fired, as the editor actor does:
//! a reading that lands without waking the editor would sit until the next
//! keypress, and a test that ticks on a timer would pass on that version.
//!
//! Design: `docs/dev/architecture/config-and-init.md` §6a.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

/// A config directory holding a user `lattice.toml`.
fn user_config(contents: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("lattice-reload-config-")
        .tempdir()
        .unwrap();
    let path = dir.path().join("lattice.toml");
    std::fs::write(&path, contents).unwrap();
    (dir, path)
}

/// Start a reload of `user` (and the project at `root`), the way
/// `Effect::ReloadConfig` does, with the files named.
fn reload(editor: &mut Editor, user: &Path, root: Option<&Path>) {
    let user = user.to_path_buf();
    let root = root.map(Path::to_path_buf);
    editor.start_config_reload_with(move || (Some(user), root));
}

/// Tick on each wake until the reading has landed and been applied.
async fn settle(editor: &mut Editor) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while editor.pending_config_reload_rx.is_some() {
        let left = deadline.saturating_duration_since(Instant::now());
        let woken = editor.async_landed.clone();
        if tokio::time::timeout(left, woken.notified()).await.is_err() {
            panic!("the reload never woke the editor");
        }
        let _ = editor.run_tick_pending();
    }
}

fn tabstop(editor: &Editor) -> Option<i64> {
    editor.config.get_int_by_name("tabstop")
}

#[tokio::test(flavor = "current_thread")]
async fn an_edited_line_applies_without_a_keypress() {
    let (_dir, user) = user_config("tabstop = 6\n");
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    assert_eq!(tabstop(&editor), Some(4), "the default before any reading");
    reload(&mut editor, &user, None);
    settle(&mut editor).await;
    assert_eq!(tabstop(&editor), Some(6));
}

#[tokio::test(flavor = "current_thread")]
async fn a_deleted_line_reverts_to_its_default() {
    let (_dir, user) = user_config("tabstop = 6\n");
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    reload(&mut editor, &user, None);
    settle(&mut editor).await;
    std::fs::write(&user, "").unwrap();
    reload(&mut editor, &user, None);
    settle(&mut editor).await;
    assert_eq!(tabstop(&editor), Some(4), "the line went, so the value did");
}

#[tokio::test(flavor = "current_thread")]
async fn a_value_set_since_survives_its_line_going() {
    let (_dir, user) = user_config("tabstop = 6\n");
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    reload(&mut editor, &user, None);
    settle(&mut editor).await;
    editor.config.parse_and_set_command("tabstop=2").unwrap();
    std::fs::write(&user, "").unwrap();
    reload(&mut editor, &user, None);
    settle(&mut editor).await;
    assert_eq!(tabstop(&editor), Some(2), "the :set is newer than the file");
}

#[tokio::test(flavor = "current_thread")]
async fn owners_hear_of_it_once_the_files_are_applied() {
    let (_dir, user) = user_config("tabstop = 6\n");
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let (tx, mut rx) =
        tokio::sync::mpsc::unbounded_channel::<lattice_config::ConfigFilesReloaded>();
    editor.event_bus.subscribe_typed(tx);
    let config = editor.config.clone();
    reload(&mut editor, &user, None);
    settle(&mut editor).await;
    let heard = rx.try_recv().expect("ConfigFilesReloaded was published");
    assert_eq!(heard.workspace_root, None);
    assert_eq!(
        config.get_int_by_name("tabstop"),
        Some(6),
        "an owner re-reading on the event sees the new values"
    );
    assert!(rx.try_recv().is_err(), "once per reload");
}

#[tokio::test(flavor = "current_thread")]
async fn a_deleted_per_language_section_reverts_too() {
    let (_dir, user) = user_config("[completion.per-language.markdown]\nauto_trigger = true\n");
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let markdown = |e: &Editor| e.per_language_completion["markdown"].auto_trigger;
    assert_eq!(markdown(&editor), Some(false), "the built-in default");
    reload(&mut editor, &user, None);
    settle(&mut editor).await;
    assert_eq!(markdown(&editor), Some(true));
    std::fs::write(&user, "").unwrap();
    reload(&mut editor, &user, None);
    settle(&mut editor).await;
    assert_eq!(
        markdown(&editor),
        Some(false),
        "back to the built-in default"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_bad_line_keeps_the_old_value() {
    let (_dir, user) = user_config("tabstop = 6\n");
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    reload(&mut editor, &user, None);
    settle(&mut editor).await;
    std::fs::write(&user, "tabstop = \"wide\"\n").unwrap();
    reload(&mut editor, &user, None);
    settle(&mut editor).await;
    assert_eq!(tabstop(&editor), Some(6), "a failed assignment is a no-op");
    assert!(
        editor.config.failed_assignment("tabstop").is_some(),
        "and the failure is recorded for :describe-option"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_changed_server_table_tells_that_server() {
    use lattice_host::dispatch::RendererSignal;
    let (_dir, user) =
        user_config("[lsp.rust-analyzer]\ncheckOnSave = true\n[lsp.gopls]\nstaticcheck = true\n");
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    reload(&mut editor, &user, None);
    settle(&mut editor).await;
    std::fs::write(
        &user,
        "[lsp.rust-analyzer]\ncheckOnSave = false\n[lsp.gopls]\nstaticcheck = true\n",
    )
    .unwrap();
    reload(&mut editor, &user, None);
    // Collect what the landing tick hands the renderer.
    let mut signals = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while editor.pending_config_reload_rx.is_some() {
        let left = deadline.saturating_duration_since(Instant::now());
        let woken = editor.async_landed.clone();
        if tokio::time::timeout(left, woken.notified()).await.is_err() {
            panic!("the reload never woke the editor");
        }
        signals.extend(editor.run_tick_pending());
    }
    let told: Vec<&str> = signals
        .iter()
        .filter_map(|s| match s {
            RendererSignal::LspConfigChanged(server) => Some(server.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        told,
        vec!["rust-analyzer"],
        "only the server whose table changed"
    );
}

/// What reached `*messages*` while `run` ran on this thread.
async fn messages_during<F: std::future::Future<Output = ()>>(run: F) -> Vec<String> {
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;
    let ring = Arc::new(Mutex::new(lattice_runtime::MessagesRing::with_capacity(64)));
    let bus = Arc::new(lattice_runtime::EventBus::new());
    let layer = lattice_runtime::MessagesLayer::new(ring.clone(), bus);
    let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(layer));
    run.await;
    ring.lock()
        .unwrap()
        .records()
        .iter()
        .map(|record| record.text.clone())
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn a_changed_startup_only_option_is_named_and_a_revert_is_reported() {
    let (_dir, user) = user_config("tabstop = 6\n");
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    reload(&mut editor, &user, None);
    settle(&mut editor).await;
    editor.config.mark_startup_only("tabstop");
    std::fs::write(&user, "").unwrap();
    let lines = messages_during(async {
        reload(&mut editor, &user, None);
        settle(&mut editor).await;
    })
    .await;
    assert!(
        lines
            .iter()
            .any(|l| l.contains("`tabstop` is no longer set") && l.contains("back to its default")),
        "the revert is reported: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("`tabstop` changed") && l.contains("next time lattice starts")),
        "the startup-only change is named: {lines:?}"
    );
}
