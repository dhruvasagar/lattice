//! PC.11 — a picker opened to ANSWER hands its value to an ex-command.
//!
//! Design:
//! [`docs/dev/architecture/project-commands.md`](../../../docs/dev/architecture/project-commands.md)
//! §9 H4. Slice plan: PC.11.
//!
//! ## Why this variant had to exist
//!
//! `FillCaller` already means "hand this value to whoever opened me", and
//! `FillTarget` already answers "who". Every answer it had was a HOST surface
//! — the document, the `:` line, a prompt, a transient argument, another
//! picker's query — and a plugin owns none of them. It owns an ex-command.
//!
//! So the risk here is not "does the value arrive" in the happy case. It is
//! the two ways a captured target goes wrong, both of which this repo has
//! already been bitten by once: a capture left behind by a picker that never
//! opened (YR.6), and a value delivered nowhere with no message at all.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_core::Document as CoreDocument;
use lattice_grammar::Effect;
use lattice_host::editor::Editor;
use lattice_picker::{FillTarget, PickerAcceptOutcome};

/// A command that records what it was handed. The assertion is on the
/// ARGUMENT, not on a side effect, because "the command ran" and "the command
/// ran with the picked value" are different claims and only the second one is
/// the feature.
fn register_recorder(editor: &mut Editor, name: &'static str) -> Arc<Mutex<Vec<String>>> {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let reg = editor
        .services
        .get::<lattice_grammar::CommandRegistryHandle>()
        .expect("the command registry is a boot service");
    let mut next = (**reg.load()).clone();
    next.register_ex_command(
        name,
        "test recorder",
        lattice_grammar::registry::ExCommandSpec {
            latency_class: lattice_grammar::command::LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(|rest: &str, _bang: bool| {
                Ok(lattice_grammar::Args::String(rest.to_string()))
            }),
            apply: Arc::new(move |ctx| {
                let arg = match &ctx.args {
                    lattice_grammar::Args::String(s) => s.clone(),
                    other => format!("{other:?}"),
                };
                sink.lock().expect("poisoned").push(arg);
                Ok(Effect::None)
            }),
            args_schema: vec![],
            surface_form: lattice_grammar::registry::SurfaceForm::Keyword,
        },
    );
    reg.store(Arc::new(next));
    seen
}

fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text("committed\n"))
}

/// The happy path, through the real seam: open with a fill action, accept a
/// `FillCaller`, and the command receives the value as its first argument.
#[test]
fn the_picked_value_arrives_as_the_actions_first_argument() {
    let mut editor = boot();
    let seen = register_recorder(&mut editor, "test-record-pick");

    let _ = editor.open_picker_for_effect(
        "buffers".to_string(),
        Vec::new(),
        None,
        Some("test-record-pick".to_string()),
        None,
    );
    assert!(editor.picker.is_some(), "precondition: the picker opened");

    let _ = editor.apply_picker_outcome(PickerAcceptOutcome::FillCaller {
        text: "/srv/chosen".to_string(),
    });

    assert_eq!(
        &*seen.lock().unwrap(),
        &["/srv/chosen".to_string()],
        "the command runs once, with the picked value"
    );
}

/// **A picker that never opened must leave no capture behind.**
///
/// YR.6 closed exactly this hole on the argument-picker path: a refused open
/// left `picker_fill_target` set, and the next unrelated `FillCaller` consumed
/// a target belonging to a picker the user never saw. Re-pinned here because
/// the rollback is a separate line in a separate function and nothing but a
/// test says it has to be there.
#[test]
fn a_refused_open_leaves_no_capture_behind() {
    let mut editor = boot();
    let seen = register_recorder(&mut editor, "test-record-orphan");

    let _ = editor.open_picker_for_effect(
        "no-such-source".to_string(),
        Vec::new(),
        None,
        Some("test-record-orphan".to_string()),
        None,
    );
    assert!(
        editor.picker.is_none(),
        "precondition: an unknown source does not open a picker"
    );
    assert!(
        editor.picker_fill_target.is_none(),
        "the capture was rolled back — leaving it set is what lets an \
         unrelated picker's value fire this action later"
    );

    // And prove it behaviourally, not just structurally: a later, unrelated
    // fill must not reach the action that never got its picker.
    let _ = editor.apply_picker_outcome(PickerAcceptOutcome::FillCaller {
        text: "/srv/unrelated".to_string(),
    });
    assert!(
        seen.lock().unwrap().is_empty(),
        "an action whose picker was refused must never receive a value"
    );
}

/// An action that is not registered REPORTS. A picked value that vanishes
/// silently is indistinguishable from a picker that did nothing, and the fix
/// — a command that was never registered, most likely a plugin that failed to
/// load — is one only the message can point at.
#[test]
fn an_unregistered_action_says_so_rather_than_dropping_the_value() {
    let mut editor = boot();

    let _ = editor.open_picker_for_effect(
        "buffers".to_string(),
        Vec::new(),
        None,
        Some("no-such-command".to_string()),
        None,
    );
    let _ = editor.apply_picker_outcome(PickerAcceptOutcome::FillCaller {
        text: "/srv/chosen".to_string(),
    });

    let message = editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(
        message.contains("no-such-command"),
        "the message must name the command that was missing, got {message:?}"
    );
}

/// Opening WITHOUT a fill action must not disturb a target some other surface
/// captured. `None` means "leave it alone", not "clear it" — the difference
/// matters because `<C-x><C-o>` captures a `CommandLine` target and then opens
/// a picker, and an unconditional clear here would break it.
#[test]
fn opening_without_a_fill_action_leaves_an_existing_capture_alone() {
    let mut editor = boot();
    editor.picker_fill_target = Some(FillTarget::CommandLine);

    let _ = editor.open_picker_for_effect("buffers".to_string(), Vec::new(), None, None, None);

    assert_eq!(
        editor.picker_fill_target,
        Some(FillTarget::CommandLine),
        "an open that asked for nothing must not clear someone else's capture"
    );
}

/// The `root` override still works through the same call — PC.1's behaviour is
/// not collateral damage of PC.11 routing both peers through one method.
#[test]
fn the_root_override_still_applies_and_still_clears() {
    let mut editor = boot();

    let _ = editor.open_picker_for_effect(
        "buffers".to_string(),
        Vec::new(),
        Some(std::path::PathBuf::from("/srv/project")),
        None,
        None,
    );
    assert_eq!(
        editor.picker_root,
        Some(std::path::PathBuf::from("/srv/project"))
    );

    // PC.1: the `None` write is what clears a previous override.
    let _ = editor.open_picker_for_effect("buffers".to_string(), Vec::new(), None, None, None);
    assert!(
        editor.picker_root.is_none(),
        "a later open with no root clears the stale one"
    );
}
