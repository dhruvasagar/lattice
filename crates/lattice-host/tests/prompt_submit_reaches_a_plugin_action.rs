//! OC.3a — a prompt opened by a plugin must be able to fire the plugin's own
//! action on submit.
//!
//! `Effect::OpenPrompt` names an `on-submit-action` by string, and it is the
//! only multi-step input flow a plugin has: org's capture types a note, org's
//! `<leader>o:` types tags. `do_prompt_line_submit` resolves that name to a
//! `CommandId` and then looks the handler up in the **`ActionHandlerRegistry`**
//! — which native modes register into and the plugin seams do not. A plugin's
//! grammar action lives in the `CommandRegistry` with an `apply` closure, so
//! the lookup misses and the submit dies with "no handler registered".
//!
//! Every org test around capture dispatches the submit action directly instead
//! of going through the prompt, so nothing caught it: the seam is wired end to
//! end and the one path a user actually takes is the one that does not work.
//! (Same shape as `plugin_gates_hand_guests_throwaway_contexts`.)
//!
//! Design: `docs/dev/architecture/org-capture.md` §5.

#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

/// What the fired action saw.
type Seen = Arc<Mutex<Option<lattice_grammar::Args>>>;

/// Register `name` as a GRAMMAR action — a `CommandRegistry` entry with an
/// `apply` closure, which is exactly what the plugin host's grammar seam
/// produces and is deliberately NOT an `ActionHandlerRegistry` entry.
fn register_grammar_action(editor: &Editor, name: &str) -> Seen {
    let seen: Seen = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&seen);
    let mut registry = (**editor.registry.load()).clone();
    registry.register_action(
        name,
        "records the args it was dispatched with (test)",
        lattice_grammar::registry::ActionSpec {
            args_schema: Vec::new(),
            apply: Arc::new(move |ctx: &lattice_grammar::registry::ActionContext| {
                *sink.lock().unwrap() = Some(ctx.args.clone());
                Ok(lattice_grammar::Effect::None)
            }),
        },
    );
    editor.registry.store(Arc::new(registry));
    seen
}

/// The typed text is seeded through `initial` — `open_prompt_line` writes it
/// into the prompt buffer, and submit reads that buffer's first line, so this
/// is the same value the user would have typed.
fn submit(editor: &mut Editor) {
    let mut out = lattice_host::dispatch::DispatchOutcome::default();
    editor.do_prompt_line_submit(&mut out);
}

/// The headline: the plugin's action runs, with the typed text.
#[test]
fn a_prompt_submit_fires_a_plugin_grammar_action() {
    let mut editor = Editor::boot(CoreDocument::from_text("* One\n"));
    let seen = register_grammar_action(&editor, "org-capture-submit");

    editor.open_prompt_line(
        "Capture: ".to_string(),
        "call the bank".to_string(),
        "org-capture-submit".to_string(),
        None,
    );
    submit(&mut editor);

    let got = seen.lock().unwrap().clone();
    assert!(
        got.is_some(),
        "the plugin's action must fire on submit — it is the only multi-step \
         input flow a plugin has, and every org capture goes through it. \
         Editor said: {:?}",
        editor.last_message.as_ref().map(|m| m.text.clone())
    );
    let text = format!("{:?}", got.unwrap());
    assert!(
        text.contains("call the bank"),
        "and it receives what was typed, got: {text}"
    );
}

/// The state a caller smuggled through `buffer-name` comes back with the
/// submit. This is what the WIT documents the field for, and it is what a
/// multi-step flow (org's `%^{Prompt}` chain) needs: `<Esc>` dispatches nothing
/// at all, so a guest-side accumulator would never be cleared and the next
/// capture would inherit it. State on the payload, not in guest memory.
#[test]
fn the_smuggled_buffer_name_comes_back_with_the_submit() {
    let mut editor = Editor::boot(CoreDocument::from_text("* One\n"));
    let seen = register_grammar_action(&editor, "org-capture-submit");

    editor.open_prompt_line(
        "Capture (todo): ".to_string(),
        "call the bank".to_string(),
        "org-capture-submit".to_string(),
        Some("*org-capture:t*".to_string()),
    );
    submit(&mut editor);

    let got = format!("{:?}", seen.lock().unwrap().clone());
    assert!(
        got.contains("call the bank"),
        "the typed text is still first, got: {got}"
    );
    assert!(
        got.contains("*org-capture:t*"),
        "and the smuggled name rides alongside it, got: {got}"
    );
}

/// A native `ActionHandlerRegistry` handler still wins, unchanged: magit's
/// prompts all take that path and it hands the handler `prompt_value` plus the
/// prompt buffer's own id, which is a different (and still correct) contract.
#[test]
fn a_native_action_handler_still_takes_precedence() {
    let mut editor = Editor::boot(CoreDocument::from_text("* One\n"));
    let grammar_seen = register_grammar_action(&editor, "both-registered");

    let cmd_id = editor
        .registry
        .load()
        .id_by_name("both-registered")
        .expect("registered");
    let native_seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&native_seen);
    let handlers = editor
        .services
        .get::<lattice_mode::ActionHandlerRegistryHandle>()
        .unwrap();
    let _reg = (*handlers).register(
        cmd_id,
        Arc::new(move |ctx: &lattice_mode::ActionContext| {
            *sink.lock().unwrap() = ctx.prompt_value.map(str::to_string);
            None
        }),
    );

    editor.open_prompt_line(
        "p: ".to_string(),
        "typed".to_string(),
        "both-registered".to_string(),
        None,
    );
    submit(&mut editor);

    assert_eq!(
        native_seen.lock().unwrap().as_deref(),
        Some("typed"),
        "the native handler ran"
    );
    assert!(
        grammar_seen.lock().unwrap().is_none(),
        "and the grammar fallback did NOT also run — one submit, one action"
    );
}

/// An action name nobody registered still says so rather than failing quietly.
#[test]
fn an_unknown_submit_action_echoes() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    editor.open_prompt_line(
        "p: ".to_string(),
        "typed".to_string(),
        "no-such-action".to_string(),
        None,
    );
    submit(&mut editor);
    let msg = editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(
        msg.contains("no-such-action"),
        "the echo names the action: {msg}"
    );
}
