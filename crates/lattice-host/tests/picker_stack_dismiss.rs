//! A dismissed picker stack must not survive into the next picker.
//!
//! Reported: dismissing a stacked picker looks like it worked, and then the
//! NEXT picker you open shows the stacked one instead of the one you asked
//! for. That is state outliving its dismiss, which is the one thing a dismiss
//! is for.
//!
//! The stack is exactly one deep and the yank picker is what makes it:
//! `do_open_yank_picker` stashes the picker underneath (`stashed_picker`) so
//! `<C-r>` over `:picker buffers` can fill that list's query rather than
//! destroying it.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use lattice_core::Document as CoreDocument;
use lattice_grammar::Args;
use lattice_host::editor::Editor;
use lattice_picker::source::{PickerInitResult, PickerSourceGenerator, PickerSourceSpec};
use lattice_picker::{
    AcceptFuture, PickerAcceptOutcome, PickerContext, RoutingPayload, SourceResult,
};
use lattice_protocol::KeyChord;

const ASYNC_SOURCE: &str = "stack-async-source";
const OPENS_A_PICKER: &str = "stack-open-another-picker";

fn boot_with_yanks() -> Editor {
    let mut editor = Editor::boot(CoreDocument::from_text("alpha\nbeta\n"));
    // The yank source refuses to open on an empty ring, and a refusal rolls
    // the stash back — which would make every assertion below vacuous.
    for text in ["first yank", "second yank"] {
        editor.yank_ring.push(
            lattice_host::state::UnnamedRegister {
                content: text.to_string(),
                kind: lattice_grammar::YankKind::Charwise,
            },
            true,
            64,
        );
    }
    editor
}

fn source(editor: &Editor) -> Option<String> {
    editor.picker.as_ref().and_then(|p| p.source_id.clone())
}

fn press(editor: &mut Editor, chord: KeyChord) {
    let mut partial: Vec<KeyChord> = Vec::new();
    let _ = editor.dispatch_chord(chord, &mut partial);
}

/// The reported bug, in the smallest form that shows it.
#[test]
fn dismissing_a_stacked_picker_does_not_leak_into_the_next_one() {
    let mut editor = boot_with_yanks();

    let _ = editor.open_picker("buffers".to_string(), Vec::new());
    assert_eq!(
        source(&editor).as_deref(),
        Some("buffers"),
        "precondition: a picker to stack on top of"
    );

    // `<C-r>` — the yank picker, stacked over `buffers`.
    press(&mut editor, KeyChord::ctrl('r'));
    assert_eq!(
        source(&editor).as_deref(),
        Some(lattice_picker::YANK_RING_SOURCE),
        "precondition: the yank picker stacked"
    );
    assert!(
        editor.stashed_picker.is_some(),
        "precondition: `buffers` is held underneath"
    );

    // `<Esc>` — dismiss.
    press(
        &mut editor,
        KeyChord::special(lattice_protocol::SpecialKey::Esc),
    );

    // Now open something else entirely. THIS is the reported symptom: the
    // picker you asked for is not the picker you get.
    let _ = editor.open_picker("commands".to_string(), Vec::new());
    assert_eq!(
        source(&editor).as_deref(),
        Some("commands"),
        "a freshly opened picker must be the one on screen — anything else is \
         a dismissed picker outliving its dismiss"
    );
    assert!(
        editor.stashed_picker.is_none(),
        "and nothing may still be held underneath it: the next dismiss would \
         restore a picker from a stack the user already left"
    );
}

/// **A dismissed picker must not be re-opened by an accept that was already
/// in flight.**
///
/// A plugin picker source resolves its accept ASYNCHRONOUSLY
/// (`accept_async`), so `do_picker_accept` takes `self.picker`, parks a
/// `pending_picker_accept`, and the outcome is applied later by
/// `drain_pending_picker_accept` on the `async_landed` wake. Between those two
/// moments the screen has no picker on it — and if the user gives up and
/// presses `<Esc>` there, the accept still lands.
///
/// `do_picker_accept` already cancels a previous pending accept when a NEW one
/// is armed. `do_picker_dismiss` cancels `pending_picker_init` for exactly
/// this reason — its comment says "the user pressed `<Esc>`, waited, and got
/// the picker they had just cancelled" — and never grew the matching line for
/// the accept half.
///
/// The symptom is not "a stale picker appears immediately": the drain runs on
/// the next wake, which is usually the next thing the user does. So it shows up
/// as *the picker I just asked for is not the one I got*.
#[test]
fn a_dismissed_picker_does_not_reopen_from_an_accept_in_flight() {
    let mut editor = boot_async_source();

    editor.seat_picker_from_pairs(
        ASYNC_SOURCE.to_string(),
        vec![(
            lattice_completion::candidate::RawCandidate::plain(
                "a row".to_string(),
                lattice_completion::candidate::CandidateKind::Plain,
            ),
            RoutingPayload::Buffer { id: 0 },
        )],
    );

    // Accept: the picker leaves the screen and the outcome is in flight.
    let _ = editor.do_picker_accept();
    assert!(
        editor.picker.is_none(),
        "precondition: accept took the picker; nothing is on screen yet"
    );
    assert!(
        editor.pending_picker_accept.is_some(),
        "precondition: the outcome is still in flight"
    );

    // The user gives up on it. `<Esc>` here reaches nothing — with no picker
    // seated there is none to dismiss — which is itself why the accept
    // vanishing reads as a dismissal.
    press(
        &mut editor,
        KeyChord::special(lattice_protocol::SpecialKey::Esc),
    );

    // …and asks for something else.
    let _ = editor.open_picker("buffers".to_string(), Vec::new());
    assert_eq!(
        source(&editor).as_deref(),
        Some("buffers"),
        "precondition: the new picker seated"
    );

    // Now the wake the actor would deliver.
    settle_accept(&mut editor);

    assert_eq!(
        source(&editor).as_deref(),
        Some("buffers"),
        "the picker on screen must still be the one just asked for — an accept \
         the user dismissed has no business opening anything"
    );
}

/// A source whose accept resolves asynchronously and opens another picker,
/// which is what `plugins/project`'s `… (choose a dir)` row does.
struct AsyncSource {
    spec: PickerSourceSpec,
}

impl PickerSourceGenerator for AsyncSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(&self, _ctx: &PickerContext<'_>, _args: &[String]) -> SourceResult<PickerInitResult> {
        Ok(PickerInitResult::Inline(Vec::new()))
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        _routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        Err("must resolve via accept_async".to_string())
    }

    /// `Some` is what forces the deferred path — the same thing a
    /// `WasmPickerSource` does for every accept.
    fn accept_async(
        &self,
        _ctx: &PickerContext<'_>,
        _routing: &RoutingPayload,
    ) -> Option<AcceptFuture> {
        Some(Box::pin(async move {
            tokio::task::yield_now().await;
            Ok(PickerAcceptOutcome::InvokeCommand {
                id: OPENS_A_PICKER.to_string(),
                args: Args::None,
            })
        }))
    }
}

fn boot_async_source() -> Editor {
    let editor = Editor::boot(CoreDocument::from_text("alpha\nbeta\n"));

    let reg = editor
        .services
        .get::<lattice_grammar::CommandRegistryHandle>()
        .expect("the command registry is a boot service");
    let mut next = (**reg.load()).clone();
    next.register_ex_command(
        OPENS_A_PICKER,
        "opens a second picker, the way `project-choose-dir` does",
        lattice_grammar::registry::ExCommandSpec {
            latency_class: lattice_grammar::command::LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(|rest: &str, _bang: bool| Ok(Args::String(rest.to_string()))),
            apply: Arc::new(|_ctx| {
                Ok(lattice_grammar::Effect::OpenPicker {
                    source: "commands".to_string(),
                    args: Vec::new(),
                    root: None,
                    fill_action: None,
                    query: None,
                })
            }),
            args_schema: vec![],
            surface_form: lattice_grammar::registry::SurfaceForm::Keyword,
        },
    );
    reg.store(Arc::new(next));

    let mut pickers = (**editor.picker_registry.load()).clone();
    pickers.register_generator(Arc::new(AsyncSource {
        spec: PickerSourceSpec::no_args(ASYNC_SOURCE, "async accept, like a plugin source"),
    }));
    editor.picker_registry.store(Arc::new(pickers));
    editor
}

/// Pump the drain the actor reaches on the `async_landed` wake.
fn settle_accept(editor: &mut Editor) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && editor.pending_picker_accept.is_some() {
        let _ = editor.drain_pending_picker_accept();
        std::thread::sleep(Duration::from_millis(10));
    }
}
