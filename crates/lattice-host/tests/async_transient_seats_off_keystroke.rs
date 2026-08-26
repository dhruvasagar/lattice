//! TR.2a — a transient whose builder answers off-thread still opens.
//!
//! A native menu's builder is a pure function of the open context and seats in
//! the frame the chord fired. A guest-backed one cannot be: `build` is an async
//! call on the plugin's own actor task, and blocking the editor actor on it
//! would violate paramount #4. So the registry's builders answer a
//! [`TransientBuild`] — `Ready` or `Future` — and the host parks on the future.
//!
//! The failure this pins is the one CLAUDE.md names outright: an async result
//! that reaches the screen only on the NEXT keystroke. So every assertion here
//! is made WITHOUT dispatching another action — the menu must arrive because
//! `async_landed` fired and the actor drained, not because the user pressed
//! something.
//!
//! Design: `docs/dev/architecture/plugin-transients.md` §4.

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_picker::{
    TransientContext, TransientGroup, TransientItem, TransientItemKind,
    TransientSourceRegistryHandle, TransientSpec,
};

fn spec(title: &str) -> TransientSpec {
    TransientSpec {
        title: title.to_string(),
        groups: vec![TransientGroup {
            label: "Actions".into(),
            items: vec![TransientItem {
                key: vec!["q".into()],
                label: "quit".into(),
                description: String::new(),
                kind: TransientItemKind::Dismiss,
            }],
        }],
        preview: None,
        footer: None,
    }
}

fn booted() -> (Editor, TransientSourceRegistryHandle) {
    let editor = Editor::boot(CoreDocument::from_text("hi\n"));
    let registry = editor
        .services
        .get::<TransientSourceRegistryHandle>()
        .unwrap();
    (editor, (*registry).clone())
}

/// Drain wakes accumulated during boot so a later wait measures only the
/// build under test.
async fn quiesce(editor: &Editor) {
    while tokio::time::timeout(Duration::from_millis(100), editor.async_landed.notified())
        .await
        .is_ok()
    {}
}

/// The headline: an async builder's menu seats without a second keystroke.
///
/// The wake is asserted first (that is the mechanism), then the drain the
/// actor's `async_landed` arm runs is called ONCE and the menu must be open.
#[tokio::test]
async fn an_async_builder_opens_the_menu_off_keystroke() {
    let (mut editor, registry) = booted();
    registry.register_async("fixture-menu", |_| Box::pin(async { Ok(spec("Fixture")) }));
    quiesce(&editor).await;

    assert!(
        editor
            .open_named_transient("fixture-menu".into(), lattice_grammar::Args::None)
            .is_empty()
    );
    assert!(
        editor.picker.is_none(),
        "an async build must not seat a menu synchronously"
    );

    assert!(
        tokio::time::timeout(Duration::from_secs(2), editor.async_landed.notified())
            .await
            .is_ok(),
        "the landed build must fire async_landed — without it the menu waits \
         for the next keypress, which reads as a chord that did nothing"
    );
    editor.drain_pending_transient_build();

    let picker = editor.picker.as_ref().expect("the menu is seated");
    let transient = picker.transient.as_ref().expect("in transient mode");
    assert_eq!(transient.title, "Fixture");
}

/// A guest `err` leaves the menu closed and echoes, NAMING the source. A menu
/// that opens empty is worse than one that says why it did not.
#[tokio::test]
async fn a_failed_build_echoes_the_source_and_opens_nothing() {
    let (mut editor, registry) = booted();
    registry.register_async("fixture-menu", |_| {
        Box::pin(async { Err("no templates configured".to_string()) })
    });
    quiesce(&editor).await;

    editor.open_named_transient("fixture-menu".into(), lattice_grammar::Args::None);
    assert!(
        tokio::time::timeout(Duration::from_secs(2), editor.async_landed.notified())
            .await
            .is_ok()
    );
    editor.drain_pending_transient_build();

    assert!(editor.picker.is_none(), "a failed build opens nothing");
    let msg = editor.last_message.as_ref().expect("an echo").text.clone();
    assert!(
        msg.contains("fixture-menu") && msg.contains("no templates configured"),
        "the echo must name both the menu and the reason, got: {msg}"
    );
}

/// A native (synchronous) builder is untouched by TR.2 — it still seats in the
/// same call, with nothing parked behind it.
#[tokio::test]
async fn a_sync_builder_still_seats_immediately() {
    let (mut editor, registry) = booted();
    registry.register("native-menu", |_| spec("Native"));

    editor.open_named_transient("native-menu".into(), lattice_grammar::Args::None);
    assert!(
        editor.pending_transient_build.is_none(),
        "a sync build parks nothing"
    );
    let title = editor
        .picker
        .as_ref()
        .and_then(|p| p.transient.as_ref())
        .map(|t| t.title.clone());
    assert_eq!(title.as_deref(), Some("Native"));
}

/// A second open supersedes the first: the user pressed another chord, and
/// seating the stale menu on top of the newer one would be the wrong menu.
#[tokio::test]
async fn a_second_open_supersedes_the_first() {
    let (mut editor, registry) = booted();
    registry.register_async("slow", |_| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(250)).await;
            Ok(spec("Slow"))
        })
    });
    registry.register_async("fast", |_| Box::pin(async { Ok(spec("Fast")) }));
    quiesce(&editor).await;

    editor.open_named_transient("slow".into(), lattice_grammar::Args::None);
    editor.open_named_transient("fast".into(), lattice_grammar::Args::None);

    // Give the slow build time to finish and try to publish.
    tokio::time::sleep(Duration::from_millis(500)).await;
    editor.drain_pending_transient_build();

    let title = editor
        .picker
        .as_ref()
        .and_then(|p| p.transient.as_ref())
        .map(|t| t.title.clone());
    assert_eq!(
        title.as_deref(),
        Some("Fast"),
        "the superseded build must be cancelled, not seated late over the newer menu"
    );
}

/// The context reaches the builder before it is spawned. Without this the
/// signature would be satisfiable by a builder that ignores where it was
/// opened from, and every gated row would silently ungate.
#[tokio::test]
async fn the_open_context_reaches_an_async_builder() {
    let (mut editor, registry) = booted();
    registry.register_async("ctx-menu", |ctx: &TransientContext| {
        let major = ctx.major_mode.clone().unwrap_or_else(|| "none".into());
        Box::pin(async move { Ok(spec(&major)) })
    });
    quiesce(&editor).await;

    let expected = editor
        .transient_open_context(lattice_grammar::Args::None)
        .major_mode
        .unwrap_or_else(|| "none".into());
    editor.open_named_transient("ctx-menu".into(), lattice_grammar::Args::None);
    assert!(
        tokio::time::timeout(Duration::from_secs(2), editor.async_landed.notified())
            .await
            .is_ok()
    );
    editor.drain_pending_transient_build();

    let title = editor
        .picker
        .as_ref()
        .and_then(|p| p.transient.as_ref())
        .map(|t| t.title.clone());
    assert_eq!(title.as_deref(), Some(expected.as_str()));
}

/// TR.3a — the arguments an open carried reach the builder.
///
/// This is what lets a menu DRILL DOWN. Org's capture menu has a row per
/// template; the fields menu that row opens has to know which template it is
/// collecting for, and `TransientContext`'s other fields say only where the
/// menu was opened, never what it was opened FOR.
///
/// The alternative is the guest remembering the subject between the two opens,
/// and `<Esc>` dispatches nothing at all — so nothing would ever clear it and
/// the next open would inherit the last one's subject.
#[tokio::test]
async fn the_args_an_open_carried_reach_the_builder() {
    let (mut editor, registry) = booted();
    registry.register_async("fields", |ctx: &TransientContext| {
        let subject = format!("{:?}", ctx.args);
        Box::pin(async move { Ok(spec(&subject)) })
    });
    quiesce(&editor).await;

    editor.open_named_transient(
        "fields".into(),
        lattice_grammar::Args::String("vocab-french".into()),
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(2), editor.async_landed.notified())
            .await
            .is_ok()
    );
    editor.drain_pending_transient_build();

    let title = editor
        .picker
        .as_ref()
        .and_then(|p| p.transient.as_ref())
        .map(|t| t.title.clone())
        .expect("the menu seated");
    assert!(
        title.contains("vocab-french"),
        "the builder saw what the open was for, got: {title}"
    );
}

/// A plain open carries no args, which is every native menu. The builder sees
/// `Args::None` rather than a stale subject from a previous open.
#[test]
fn a_plain_open_carries_no_args() {
    let (editor, _registry) = booted();
    let ctx = editor.transient_open_context(lattice_grammar::Args::None);
    assert!(matches!(ctx.args, lattice_grammar::Args::None));
}
