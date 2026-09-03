//! OA.15a — a provider can re-open its own view from a place that returns no
//! effect, and the result reaches the screen without a keystroke.
//!
//! ## The gap this closes
//!
//! `AppEffect::OpenProviderView` already says "open my view", and every trigger
//! that RETURNS an effect keeps using it. What had no path at all was a
//! producer that returns nothing: a plugin's `on-event` handler is
//! `func(handler, ev)` by construction, so a guest could observe everything and
//! change nothing on screen.
//!
//! The consumer that made it visible is a guest MINOR MODE. A native mode
//! supplies its own behaviour — `scan-view-clockreport-mode` registers its
//! provider in `on_activate` and drops it on deactivation, which is what makes
//! the mode the single switch rather than a label beside one. A plugin mode is
//! DATA: the host builds it into a `PluginMode` whose `on_activate` is a no-op
//! (`mode_host.rs`). So `org-agenda-log-mode` could be toggled and change
//! nothing, because the guest had no way to answer its own `minor-activated`.
//!
//! ## What these tests are careful about
//!
//! The **wake** is the half that fails silently. A bare channel would make
//! every assertion here pass on a `run_tick_pending` the test itself calls —
//! and then, in use, the view would refresh only when the user next pressed a
//! key. So the wake is asserted directly, against the same `async_landed`
//! primitive the editor actor selects on, rather than inferred from the drain
//! working.

#![allow(clippy::unwrap_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lattice_core::Document as CoreDocument;
use lattice_grammar::Args;
use lattice_host::editor::Editor;
use lattice_mode::provider_view::ProviderViewRefreshRequested;
use lattice_mode::{ModeActivator, ProviderViewOutcome, ProviderViewRegistryHandle};

/// What one registered opener recorded: how often it ran, and with what.
#[derive(Default)]
struct Spy {
    calls: AtomicUsize,
    args: Mutex<Vec<Args>>,
}

/// Register `name`'s opener on the editor's real registry and return the spy.
///
/// Uses the boot-registered `ProviderViewRegistryHandle` rather than a
/// hand-built one: the drain looks the opener up through the service registry,
/// so a test with its own registry would exercise nothing.
fn register_spy_view(editor: &Editor, name: &str) -> Arc<Spy> {
    let spy = Arc::new(Spy::default());
    let reg = editor
        .services
        .get::<ProviderViewRegistryHandle>()
        .expect("the provider-view registry is a boot service");
    let recorder = spy.clone();
    let opened = editor.document_buffer_id;
    let registered = reg.register(
        name,
        Arc::new(move |_: &mut dyn ModeActivator, args: &Args| {
            recorder.calls.fetch_add(1, Ordering::SeqCst);
            recorder.args.lock().unwrap().push(args.clone());
            ProviderViewOutcome::Opened {
                view: opened,
                message: None,
            }
        }),
    );
    assert!(registered, "the spy view registered under `{name}`");
    spy
}

fn publish(editor: &Editor, provider: &str, args: &[&str]) {
    editor
        .event_bus
        .publish_typed(ProviderViewRefreshRequested {
            provider: provider.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        });
}

/// The bus delivers on its own task, so a published event is not readable the
/// instant `publish_typed` returns. Poll the drain rather than sleeping once.
fn settle_refresh(editor: &mut Editor, spy: &Spy, want: usize) -> bool {
    for _ in 0..200 {
        let _ = editor.drain_provider_view_refresh();
        if spy.calls.load(Ordering::SeqCst) >= want {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_published_refresh_re_opens_the_view() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let spy = register_spy_view(&editor, "spy-view");

    publish(&editor, "spy-view", &["", "log=closed,clock"]);

    assert!(
        settle_refresh(&mut editor, &spy, 1),
        "a published refresh must reach the registered opener"
    );
}

/// The args arrive in the SAME encoding `AppEffect::OpenProviderView` uses —
/// slot 0 is the host's own argument (the root), the tail is the provider's
/// scan args. An opener reads slot 0 as the root, so a list that omitted it
/// would shift every scan arg down one and the provider would silently answer
/// the wrong question.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_args_arrive_in_the_open_provider_view_encoding() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let spy = register_spy_view(&editor, "spy-view");

    publish(&editor, "spy-view", &["r", "span=7"]);
    assert!(settle_refresh(&mut editor, &spy, 1));

    let seen = spy.args.lock().unwrap().clone();
    match &seen[0] {
        Args::List(values) => {
            let strings: Vec<String> = values
                .iter()
                .map(|v| match v {
                    lattice_grammar::args::ArgValue::String(s) => s.clone(),
                    other => panic!("every slot crosses as a string, got {other:?}"),
                })
                .collect();
            assert_eq!(
                strings,
                vec![String::new(), "r".to_string(), "span=7".to_string()],
                "slot 0 is the root (empty — a refresh never re-points the \
                 view), then the guest's args verbatim"
            );
        }
        other => panic!("args must cross as the positional list form, got {other:?}"),
    }
}

/// **The half that fails silently.** The drain alone would pass every
/// assertion above while, in use, the view refreshed only on the next
/// keypress. `async_landed` is what the editor actor selects on to republish
/// render state off-keystroke, so the request must fire it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refresh_wakes_the_actor_without_a_keystroke() {
    let editor = Editor::boot(CoreDocument::from_text("x\n"));
    let _spy = register_spy_view(&editor, "spy-view");

    let woken = editor.async_landed.clone();
    // Arm BEFORE publishing: `Notify::notified()` only observes a permit
    // issued after it is created, so waiting afterwards could miss the wake
    // and pass for the wrong reason.
    let wait = tokio::spawn(async move { woken.notified().await });
    tokio::task::yield_now().await;

    publish(&editor, "spy-view", &["", "log=closed"]);

    let woke = tokio::time::timeout(std::time::Duration::from_secs(2), wait).await;
    assert!(
        woke.is_ok(),
        "a refresh request must fire `async_landed`, or the re-scan sits \
         until the user happens to press a key — the \"works, but only after \
         I hit something\" class"
    );
}

/// A name nobody registered is a warning and a skip, never a panic: a stale
/// view name after a plugin reload is an author mistake, not a reason to take
/// the editor down.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_view_name_is_survivable() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let spy = register_spy_view(&editor, "spy-view");

    publish(&editor, "no-such-view", &[""]);
    // Drain a few times so the unknown request is definitely consumed.
    for _ in 0..20 {
        let _ = editor.drain_provider_view_refresh();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        spy.calls.load(Ordering::SeqCst),
        0,
        "the registered view must not be opened by another view's request"
    );

    // …and the editor still serves a real one afterwards.
    publish(&editor, "spy-view", &[""]);
    assert!(
        settle_refresh(&mut editor, &spy, 1),
        "an unknown name must not poison the drain"
    );
}

/// Identical requests collapse. A mode toggled twice before a tick lands would
/// otherwise re-scan identically twice, and the scan is the expensive thing at
/// the end of this path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identical_requests_in_one_tick_collapse() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let spy = register_spy_view(&editor, "spy-view");

    for _ in 0..3 {
        publish(&editor, "spy-view", &["", "log=closed"]);
    }
    // Let all three land on the channel BEFORE the first drain, which is the
    // situation being tested — three requests in one tick.
    std::thread::sleep(std::time::Duration::from_millis(50));
    let _ = editor.drain_provider_view_refresh();

    assert_eq!(
        spy.calls.load(Ordering::SeqCst),
        1,
        "three identical requests in one tick are one re-scan"
    );
}

/// …but DIFFERENT args are different questions, and the last one is the one
/// the user asked. Collapsing them would drop a toggle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn distinct_requests_are_not_collapsed() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let spy = register_spy_view(&editor, "spy-view");

    publish(&editor, "spy-view", &["", "log=closed"]);
    publish(&editor, "spy-view", &[""]);
    std::thread::sleep(std::time::Duration::from_millis(50));
    let _ = editor.drain_provider_view_refresh();

    assert_eq!(
        spy.calls.load(Ordering::SeqCst),
        2,
        "log-on then log-off is two questions, not one"
    );
}
