//! YR.6 — `<C-x><C-o>` opens the picker an argument declares, and the pick
//! REPLACES what was already typed for that argument.
//!
//! The second `FillCaller` consumer, and it exercises the half YR.5 did not:
//! YR.5's `<C-r><C-r>` inserts at the cursor because nothing is being
//! replaced. An argument picker is opened part-way through typing the
//! argument, so a plain insert turns `:magit-checkout ma` + pick `main` into
//! `:magit-checkout mamain` — silently, and only for users who typed a prefix
//! first. A test that opens the picker on an empty argument passes against
//! that bug, so the prefix case is pinned explicitly below.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_grammar::{ArgKind, ArgSpec, CommandRegistry, Effect, ExCommandSpec, LatencyClass};
use lattice_grammar::{Args, SurfaceForm};
use lattice_host::editor::Editor;
use lattice_picker::{FillTarget, PickerAcceptOutcome};
use std::sync::Arc;

/// A command whose single argument declares a picker, plus one whose
/// argument declares none — the difference is what the tests turn on.
fn register_commands(registry: &mut CommandRegistry) {
    for (name, picker) in [("yr6-with", true), ("yr6-without", false)] {
        let mut arg = ArgSpec::required("rev", ArgKind::String, "the revision");
        if picker {
            arg = arg.with_picker(lattice_picker::YANK_RING_SOURCE);
        }
        registry.register_ex_command(
            name,
            "YR.6 fixture command.",
            ExCommandSpec {
                latency_class: LatencyClass::Display,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|line: &str, _bang: bool| {
                    Ok(Args::String(line.trim().to_string()))
                }),
                apply: Arc::new(|_| Ok(Effect::None)),
                args_schema: vec![arg],
                surface_form: SurfaceForm::Keyword,
            },
        );
    }
}

/// The fixture's argument names the **yank-ring** picker rather than an
/// invented source id. An id nothing has registered makes `open_picker`
/// refuse, which correctly rolls the capture back — so every assertion about
/// the capture would read `None` and the test would be measuring the
/// rollback, not the feature. (That is how this harness was first written,
/// and the rollback caught it.) The ring is seeded below for the same reason:
/// the source refuses to open on an empty one.
fn boot() -> Editor {
    let mut editor = Editor::boot(CoreDocument::from_text("hello\n"));
    editor.store_yank(
        lattice_grammar::Register::Unnamed,
        "main".to_string(),
        lattice_grammar::YankKind::Charwise,
        true,
    );
    editor.registry.rcu(|current| {
        let mut next = (**current).clone();
        register_commands(&mut next);
        Arc::new(next)
    });
    editor
}

fn echo(editor: &Editor) -> String {
    editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default()
}

/// Type a `:` line verbatim.
fn type_cmdline(editor: &mut Editor, text: &str) {
    editor.modal = lattice_grammar::ModalState::Command;
    editor.set_command_line_text(text);
}

fn fill(editor: &mut Editor, text: &str) {
    let _ = editor.apply_picker_outcome(PickerAcceptOutcome::FillCaller {
        text: text.to_string(),
    });
}

/// The bug this slice would otherwise have shipped.
#[test]
fn a_pick_replaces_the_prefix_already_typed_for_that_argument() {
    let mut editor = boot();
    type_cmdline(&mut editor, "yr6-with ma");

    let _ = editor.do_open_arg_picker();
    assert_eq!(
        editor.picker_fill_target,
        Some(FillTarget::CommandLine),
        "the target is captured at open"
    );
    assert!(
        editor.picker_fill_replace.is_some(),
        "and so is the range the pick replaces"
    );

    fill(&mut editor, "main");
    assert_eq!(
        editor.command_line(),
        "yr6-with main",
        "the pick replaces `ma` rather than being appended to it"
    );
}

/// The case that passes against the broken version — kept so the pair is
/// visible, not because it proves much on its own.
#[test]
fn a_pick_on_an_empty_argument_still_lands() {
    let mut editor = boot();
    type_cmdline(&mut editor, "yr6-with ");

    let _ = editor.do_open_arg_picker();
    fill(&mut editor, "main");
    assert_eq!(editor.command_line(), "yr6-with main");
}

#[test]
fn an_argument_with_no_picker_says_so_rather_than_doing_nothing() {
    let mut editor = boot();
    type_cmdline(&mut editor, "yr6-without ma");

    let _ = editor.do_open_arg_picker();
    assert!(
        editor.picker_fill_target.is_none(),
        "nothing was captured, so no later fill can be consumed by it"
    );
    assert!(
        echo(&editor).contains("no picker"),
        "the user is told why nothing opened, got: {:?}",
        echo(&editor)
    );
}

#[test]
fn the_chord_is_a_no_op_off_the_command_line() {
    let mut editor = boot();
    let _ = editor.do_open_arg_picker();
    assert!(editor.picker_fill_target.is_none());
    assert!(
        echo(&editor).contains("`:` line"),
        "got: {:?}",
        echo(&editor)
    );
}

/// The capture must not outlive the picker that made it. Without this, a
/// dismissed argument picker hands its `CommandLine` target and its replace
/// range to the next thing that emits `FillCaller` without setting its own —
/// landing a value in a line that stopped asking for one.
#[test]
fn dismissing_the_picker_drops_the_capture() {
    let mut editor = boot();
    type_cmdline(&mut editor, "yr6-with ma");

    let _ = editor.do_open_arg_picker();
    assert!(editor.picker_fill_target.is_some());

    let _ = editor.do_picker_dismiss();
    assert!(
        editor.picker_fill_target.is_none(),
        "the fill target goes with the picker that captured it"
    );
    assert!(
        editor.picker_fill_replace.is_none(),
        "and so does the replace range"
    );
}

/// The magit arguments this slice exists for actually declare their pickers.
/// Asserted through the registry rather than by reading the source, so a
/// renamed picker source or a dropped `.with_picker` fails here.
#[test]
fn magit_revision_arguments_declare_a_picker() {
    let editor = boot();
    let reg = editor.registry.load();
    let Some(id) = reg.id_by_name("magit-file-checkout") else {
        eprintln!("SKIP: magit commands not registered in this harness");
        return;
    };
    let Some(spec) = reg.lookup(id) else {
        panic!("magit-file-checkout resolves");
    };
    let rev = spec
        .args_schema
        .iter()
        .find(|a| a.name == "rev")
        .expect("magit-file-checkout takes a rev");
    assert_eq!(
        rev.picker.as_deref(),
        Some("magit-revision"),
        "the rev argument offers the revision picker"
    );
}
