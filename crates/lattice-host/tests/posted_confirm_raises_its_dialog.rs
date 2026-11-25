//! A confirm a plugin posts from the background raises its dialog.
//!
//! A post reaches the editor through an inbound bus, which is a tick
//! callback: `run_tick_pending` drains it and hands each effect to
//! `handle_effect`, off-keystroke, where the renderer peers' effect arms never
//! run. A confirm the peers applied would therefore be dropped, so it is
//! applied host-side (`Editor::open_confirm`) and both peers no-op it.
//!
//! Driven the way a post arrives: a registered tick callback yields the
//! effect once, and the tick drains it with no key pressed. Then `y` answers
//! it, through the key.

#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use lattice_core::Document as CoreDocument;
use lattice_grammar::{ArgValue, Args, Effect};
use lattice_host::editor::Editor;
use lattice_mode::{ActionHandlerRegistryHandle, TickCallbackRegistryHandle};
use lattice_protocol::chord::KeyChord;

type Seen = Arc<Mutex<Option<Args>>>;

/// An editor with `run-the-blocks` registered, taking one string, and a
/// handler that records what it was fired with.
fn boot() -> (Editor, Seen, lattice_mode::ActionHandlerRegistration) {
    let editor = Editor::boot(CoreDocument::from_text("* One\n"));
    let mut registry = (**editor.registry.load()).clone();
    registry.register_action(
        "run-the-blocks",
        "records the args it fired with (test)",
        lattice_grammar::registry::ActionSpec {
            args_schema: vec![lattice_grammar::ArgSpec {
                name: "file".into(),
                kind: lattice_grammar::args::ArgKind::String,
                doc: "which file".into(),
                prompt: "file:".into(),
                default: lattice_grammar::args::ArgDefault::None,
                completion: None,
                picker: None,
            }],
            apply: Arc::new(|_| Ok(lattice_grammar::Effect::None)),
        },
    );
    let cmd = registry.id_by_name("run-the-blocks").unwrap();
    editor.registry.store(Arc::new(registry));

    let seen: Seen = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&seen);
    let handlers = editor
        .services
        .get::<ActionHandlerRegistryHandle>()
        .unwrap();
    let reg = (*handlers).register(
        cmd,
        Arc::new(move |ctx: &lattice_mode::ActionContext| {
            *sink.lock().unwrap() = Some(ctx.args.clone());
            None
        }),
    );
    (editor, seen, reg)
}

/// Queue `effect` the way a post does: a tick callback that yields it once.
fn post(editor: &Editor, effect: Effect) -> lattice_mode::TickCallbackRegistration {
    let ticks = editor.services.get::<TickCallbackRegistryHandle>().unwrap();
    let mut once = Some(effect);
    ticks.register(Box::new(move || once.take().into_iter().collect()))
}

fn dialog_open(editor: &Editor) -> bool {
    editor
        .picker
        .as_ref()
        .is_some_and(|p| p.transient.is_some())
}

#[test]
fn a_posted_confirm_opens_without_a_keypress_and_y_fires_its_action() {
    let (mut editor, seen, _reg) = boot();
    let _posted = post(
        &editor,
        Effect::Confirm {
            prompt: "Run 3 code blocks in notes.org?".to_string(),
            yes_action: "run-the-blocks".to_string(),
            args: Args::List(vec![ArgValue::String("notes.org".to_string())]),
        },
    );

    editor.run_tick_pending();
    assert!(dialog_open(&editor), "the dialog is up with no key pressed");
    assert!(
        seen.lock().unwrap().is_none(),
        "nothing runs before the answer"
    );

    let mut partial: Vec<KeyChord> = Vec::new();
    let _ = editor.dispatch_chord(KeyChord::char('y'), &mut partial);
    assert_eq!(
        *seen.lock().unwrap(),
        Some(Args::List(vec![ArgValue::String("notes.org".to_string())])),
        "`y` runs the action with the arguments the question named"
    );
}

/// A yes-action nobody registered opens nothing and says so, rather than a
/// dialog whose `y` would do nothing.
#[test]
fn a_posted_confirm_for_an_unknown_action_says_so() {
    let (mut editor, _seen, _reg) = boot();
    let _posted = post(
        &editor,
        Effect::Confirm {
            prompt: "Run?".to_string(),
            yes_action: "no-such-action".to_string(),
            args: Args::None,
        },
    );
    editor.run_tick_pending();
    assert!(!dialog_open(&editor));
    assert!(
        editor
            .last_message
            .as_ref()
            .is_some_and(|m| m.text.contains("no-such-action")),
        "{:?}",
        editor.last_message
    );
}
