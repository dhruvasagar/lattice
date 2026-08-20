//! MG.54 — the picker's selection-settle preview.
//!
//! Design + rationale: `docs/dev/operations/slice-plans/magit.md` (MG.54).
//!
//! The mechanism exists so a source whose preview costs a subprocess can be
//! **synchronous**. That is only sound if scrolling produces no calls at
//! all, so these tests assert the call COUNT, not merely the end state: an
//! implementation that fired per move and threw the results away would show
//! the right pane and still be the thing MG.54 refuses to ship.
//!
//! The settle also has to reach the screen with **no key pressed** — a test
//! that dispatches another action first would pass against a build where the
//! wake was never scheduled (the `async_landed` hole CLAUDE.md names).

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lattice_core::Document as CoreDocument;
use lattice_host::action::Action;
use lattice_host::editor::Editor;
use lattice_picker::source::{PickerInitResult, PickerSourceGenerator, PickerSourceSpec};
use lattice_picker::{
    PickerAcceptOutcome, PickerContext, PickerPreviewOutcome, RoutingPayload, SourceResult,
};

const SOURCE_ID: &str = "settle-test";

/// A source that counts how many times the host asks it to preview and
/// answers with a text buffer — the shape a git blob at a revision takes.
struct CountingSource {
    spec: PickerSourceSpec,
    calls: Arc<AtomicUsize>,
    debounce: Option<Duration>,
}

impl CountingSource {
    fn new(calls: Arc<AtomicUsize>, debounce: Option<Duration>) -> Self {
        Self {
            spec: PickerSourceSpec::no_args(SOURCE_ID, "MG.54 settle-preview test source"),
            calls,
            debounce,
        }
    }
}

impl PickerSourceGenerator for CountingSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(&self, _ctx: &PickerContext<'_>, _args: &[String]) -> SourceResult<PickerInitResult> {
        Ok(PickerInitResult::Inline(
            ["one", "two", "three"]
                .into_iter()
                .map(|name| {
                    (
                        lattice_completion::RawCandidate::plain(
                            name.to_string(),
                            lattice_completion::CandidateKind::Plain,
                        ),
                        RoutingPayload::InvokeCommand {
                            id: name.to_string(),
                            args: lattice_grammar::Args::None,
                        },
                    )
                })
                .collect(),
        ))
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        _routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        Ok(PickerAcceptOutcome::NoOp)
    }

    fn preview(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> Option<PickerPreviewOutcome> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let RoutingPayload::InvokeCommand { id, .. } = routing else {
            return None;
        };
        Some(PickerPreviewOutcome::Buffer {
            name: "*settle-preview*".to_string(),
            text: format!("blob for {id}\n"),
            syntax_path: None,
        })
    }

    fn preview_debounce(&self) -> Option<Duration> {
        self.debounce
    }
}

fn boot_with(debounce: Option<Duration>) -> (Editor, Arc<AtomicUsize>) {
    let mut editor = Editor::boot(CoreDocument::from_text("committed\ncontent\n"));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = (**editor.picker_registry.load()).clone();
    registry.register_generator(Arc::new(CountingSource::new(Arc::clone(&calls), debounce)));
    editor.picker_registry.store(Arc::new(registry));
    let _ = editor.open_picker(SOURCE_ID.to_string(), Vec::new());
    (editor, calls)
}

/// Whether a deferred preview is waiting. The state lives on the PICKER,
/// not the host — which is what makes "a settle outliving its picker"
/// unrepresentable rather than something two clear sites must remember.
fn settle_pending(editor: &Editor) -> bool {
    editor
        .picker
        .as_ref()
        .map(|p| p.preview_settle_pending())
        .unwrap_or(false)
}

/// What the active pane is currently DISPLAYING, as text. The committed
/// buffer when there is no preview projection.
fn displayed_text(editor: &Editor) -> String {
    let pane = editor.pane_tree.active().id;
    let id = editor
        .preview_overrides
        .get(&pane)
        .map(|o| o.buffer_id)
        .unwrap_or_else(|| editor.pane_tree.active().buffer_id);
    editor
        .buffers
        .document_handle(id)
        .map(|h| h.snapshot().buffer.as_string())
        .unwrap_or_default()
}

/// The core claim: arrowing through candidates spawns NOTHING. A source
/// that shells out per selection move is a paramount-#1 violation, and the
/// settle window is what makes a synchronous fetch legitimate — so "the
/// source was never asked" is the assertion, not "the preview looks right".
#[test]
fn a_debounced_source_is_not_asked_while_the_selection_moves() {
    // A window long enough that nothing in this test can settle.
    let (mut editor, calls) = boot_with(Some(Duration::from_secs(30)));
    let committed = displayed_text(&editor);

    for _ in 0..6 {
        editor.dispatch(Action::PickerSelectNext);
    }
    editor.dispatch(Action::PickerSelectPrev);

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "seven selection moves inside the settle window must ask the source zero times"
    );
    assert_eq!(
        displayed_text(&editor),
        committed,
        "with nothing previewed yet the pane still shows its committed buffer"
    );
    assert!(
        settle_pending(&editor),
        "the moves should have armed a settle deadline"
    );
}

/// The other half: a source that declares no window is asked inline, exactly
/// as before MG.54. Without this the first test passes on a build that
/// simply broke preview.
#[test]
fn an_undebounced_source_is_still_asked_on_every_move() {
    let (mut editor, calls) = boot_with(None);
    let before = calls.load(Ordering::SeqCst);

    editor.dispatch(Action::PickerSelectNext);
    editor.dispatch(Action::PickerSelectNext);

    assert_eq!(
        calls.load(Ordering::SeqCst) - before,
        2,
        "an undebounced source previews inline, once per move"
    );
    assert!(
        !settle_pending(&editor),
        "no window declared ⇒ no deadline armed"
    );
    assert!(
        displayed_text(&editor).contains("blob for"),
        "the inline preview is mounted in the pane; got {:?}",
        displayed_text(&editor)
    );
}

/// Settling fires the preview **once**, for the candidate the user stopped
/// on, and it reaches the pane with **no further keystroke** — the wake is
/// awaited, not simulated by dispatching something else.
#[tokio::test]
async fn settling_previews_the_final_candidate_exactly_once_without_a_keypress() {
    let (mut editor, calls) = boot_with(Some(Duration::from_millis(30)));

    // Land on "three": two moves from the seated first row.
    editor.dispatch(Action::PickerSelectNext);
    editor.dispatch(Action::PickerSelectNext);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "still inside the window");

    // The actor's off-keystroke arm: wait for the wake the arming
    // scheduled, then run the tick aggregator exactly as `run_actor` does.
    // A wake that was never scheduled times out here.
    let woke = tokio::time::timeout(Duration::from_secs(2), editor.async_landed.notified()).await;
    assert!(woke.is_ok(), "the settle must wake the actor on its own");
    // Each arming schedules its own wake, so earlier ones can arrive first
    // and find the deadline still in the future — keep ticking until the
    // deferred preview lands (bounded; the deadline is 30ms).
    let mut ticks = 0;
    while calls.load(Ordering::SeqCst) == 0 && ticks < 50 {
        editor.run_tick_pending();
        ticks += 1;
        if calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "two moves + a settle = one call, not one per move and not one per tick"
    );
    assert!(
        displayed_text(&editor).contains("blob for three"),
        "the pane shows the settled candidate's content; got {:?}",
        displayed_text(&editor)
    );
    assert!(
        !settle_pending(&editor),
        "the deadline is consumed by the firing, so a later wake can't re-run the fetch"
    );

    // Extra ticks must not re-ask: the fetch is per settle, not per tick.
    editor.run_tick_pending();
    editor.run_tick_pending();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "ticks after the settle fired must not re-run the source's preview"
    );
}

/// A settle armed by a picker that is then dismissed must not fire into
/// whatever comes next. The wake is already scheduled by then, so this is
/// the path where a stale deadline could outlive its picker — and the
/// reason the deadline is a field of `Picker` rather than of the host.
#[test]
fn dismissing_the_picker_disarms_a_pending_settle() {
    let (mut editor, calls) = boot_with(Some(Duration::from_millis(1)));
    editor.dispatch(Action::PickerSelectNext);
    assert!(settle_pending(&editor));

    editor.do_picker_dismiss();
    assert!(
        !settle_pending(&editor),
        "the deadline went with the picker"
    );

    std::thread::sleep(Duration::from_millis(5));
    editor.run_tick_pending();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a settle whose picker is gone must never run its fetch"
    );
}

/// The peer case: a picker REPLACED while its settle is pending. The
/// scheduled wake still arrives, and if the deadline were host state it
/// would fire the successor's preview undebounced — the successor never
/// asked for a preview at all here, since it declares no window.
#[test]
fn a_pending_settle_does_not_fire_into_the_picker_that_replaces_it() {
    let (mut editor, calls) = boot_with(Some(Duration::from_millis(1)));
    editor.dispatch(Action::PickerSelectNext);
    assert!(settle_pending(&editor));

    // A different picker takes the seat.
    editor.set_active_picker(lattice_picker::Picker::new(
        "successor",
        lattice_picker::PickerSource::Buffers,
        lattice_picker::PickerAction::OpenFile,
    ));

    std::thread::sleep(Duration::from_millis(5));
    editor.run_tick_pending();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the deadline belonged to the picker that is gone"
    );
    assert!(!settle_pending(&editor));
}
