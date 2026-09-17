//! NOTIF.2 (N1): an open `*notifications*` buffer follows `ui.nerd_fonts`.
//!
//! The corner reads the option every frame, so it needs no help. The
//! buffer is text written once when it opens, so without a refresh it
//! keeps the old palette's icons after a toggle. That is the icon rule's
//! failure: both palettes must be usable, and a toggle must re-render
//! every surface that shows them.
//!
//! The refresh lands off-keystroke. The test waits on the store's own
//! wake and never presses a key, so a refresh that only showed up after
//! the next key would fail here.

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_host::ui::theme_options::UiNerdFonts;
use lattice_notify::{NotificationLevel, NotificationStoreHandle};

fn buffer_text(editor: &Editor) -> String {
    let id = editor
        .buffers
        .by_name(lattice_notify::mode::BUFFER_NAME)
        .expect("the buffer is open");
    editor
        .buffers
        .document_handle(id)
        .map(|h| h.snapshot().buffer.as_string())
        .unwrap_or_default()
}

/// Settle until `pred` holds or the deadline passes, draining the
/// editor's off-keystroke work each time it wakes. Polls rather than
/// waits once: the mode's first render and the refresh are two separate
/// async steps.
async fn settle_until(editor: &mut Editor, pred: impl Fn(&Editor) -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        editor.run_tick_pending();
        if pred(editor) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        let _ =
            tokio::time::timeout(Duration::from_millis(50), editor.async_landed.notified()).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn toggling_nerd_fonts_rerenders_an_open_notifications_buffer() {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let store = editor
        .services
        .get::<NotificationStoreHandle>()
        .map(|s| (*s).clone())
        .unwrap();
    store.post(NotificationLevel::Error, "push failed");

    // The host seam `:notifications`' `Effect::OpenSyntheticBuffer`
    // resolves to.
    editor.open_synthetic_buffer(
        lattice_notify::mode::BUFFER_NAME,
        lattice_notify::mode::NotificationsMode::mode_id().as_str(),
    );

    let fallback = NotificationLevel::Error.glyph(false);
    let nerd = NotificationLevel::Error.glyph(true);
    assert!(
        settle_until(&mut editor, |e| buffer_text(e).contains(fallback)).await,
        "the buffer opens with the fallback palette: {:?}",
        buffer_text(&editor)
    );

    editor.config.set_typed::<UiNerdFonts>(true).unwrap();

    assert!(
        settle_until(&mut editor, |e| buffer_text(e).contains(nerd)).await,
        "flipping ui.nerd_fonts must re-render the open buffer, got {:?}",
        buffer_text(&editor)
    );
    assert!(
        !buffer_text(&editor).contains(fallback),
        "and the old palette's icon is gone"
    );
}

/// NC.2: two repositories finishing the same operation must not read
/// the same. The scope travels on the event, so this goes through the
/// real subscriber rather than posting directly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_task_event_carries_its_scope_to_the_notification() {
    use lattice_protocol::event::{Event, TaskOutcome};

    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let store = editor
        .services
        .get::<NotificationStoreHandle>()
        .map(|s| (*s).clone())
        .unwrap();
    for scope in ["lattice", "dotfiles"] {
        editor.event_bus.publish(Event::BackgroundTaskFinished {
            source: "magit".into(),
            scope: Some(scope.into()),
            label: "push main".into(),
            outcome: TaskOutcome::Succeeded {
                summary: String::new(),
            },
        });
    }

    assert!(
        settle_until(&mut editor, |_| store.visible().len() == 2).await,
        "both completions are posted"
    );
    let live = store.visible();
    let scopes: Vec<_> = live.iter().map(|n| n.scope.as_deref()).collect();
    assert!(scopes.contains(&Some("lattice")), "{scopes:?}");
    assert!(scopes.contains(&Some("dotfiles")), "{scopes:?}");
    assert!(live.iter().all(|n| n.level == NotificationLevel::Success));
}
