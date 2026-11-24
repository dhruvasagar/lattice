//! OB.3a — `Effect::InvokeCommand` runs the command it names, and only that.
//!
//! A plugin's command name is checked at the boundary (`lattice-plugin-host`'s
//! command gate): it must be registered, and open to plugins. That check is
//! only worth something if the host then runs the command it checked. It did
//! not: a name that was not an action was rendered with its args as an ex line
//! and parsed, so the line's own syntax chose the command. A plugin registering
//! a command NAMED `g/x/d` and invoking it got `:global` deleting every line
//! matching `x` — or running whatever `:global` was told to run,
//! `shell-command` included; one named `%d` emptied the buffer.
//!
//! Each test asserts on what ran (a recorder) AND on the buffer, so "the right
//! command ran" and "the wrong one did not" are both pinned.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_core::Document as CoreDocument;
use lattice_grammar::{Args, Effect};
use lattice_host::editor::Editor;

type Calls = Arc<Mutex<Vec<String>>>;

/// Register an ex-command under `name` that records its argument — standing
/// in for a plugin's own command, which is the kind a plugin may name.
fn register_recorder(editor: &Editor, name: &'static str, calls: Calls) {
    let reg = editor
        .services
        .get::<lattice_grammar::CommandRegistryHandle>()
        .expect("the command registry is a boot service");
    let mut next = (**reg.load()).clone();
    next.register_plugin_ex_command(
        7,
        name,
        "OB.3a fixture",
        lattice_grammar::registry::ExCommandSpec {
            latency_class: lattice_grammar::command::LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(|rest: &str, _bang: bool| Ok(Args::String(rest.to_string()))),
            apply: Arc::new(move |ctx| {
                let arg = match &ctx.args {
                    Args::String(s) => s.clone(),
                    _ => String::new(),
                };
                calls.lock().unwrap().push(arg);
                Ok(Effect::None)
            }),
            args_schema: vec![],
            surface_form: lattice_grammar::registry::SurfaceForm::Keyword,
        },
    );
    reg.store(Arc::new(next));
}

fn text(editor: &Editor) -> String {
    editor.document.snapshot().text().to_string()
}

/// A command whose NAME is front-end syntax runs as itself. Before OB.3a the
/// name was parsed as a line, so a plugin that registered `g/drop/d` — a name
/// the gate rightly lets it invoke, being its own — got `:global` instead.
#[test]
fn a_command_named_like_global_is_not_global() {
    let mut editor = Editor::boot(CoreDocument::from_text("keep\ndrop\nkeep\n"));
    let calls = Calls::default();
    register_recorder(&editor, "g/drop/d", calls.clone());

    editor.handle_effect(Effect::InvokeCommand {
        id: "g/drop/d".to_string(),
        args: Args::None,
    });

    assert_eq!(calls.lock().unwrap().len(), 1, "the named command ran");
    assert_eq!(
        text(&editor),
        "keep\ndrop\nkeep\n",
        ":global must not have run"
    );
}

/// The same for a range: `%d` named is a command, not "delete every line".
#[test]
fn a_command_named_like_a_range_is_not_a_range() {
    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\n"));
    let calls = Calls::default();
    register_recorder(&editor, "%d", calls.clone());

    editor.handle_effect(Effect::InvokeCommand {
        id: "%d".to_string(),
        args: Args::None,
    });

    assert_eq!(calls.lock().unwrap().len(), 1, "the named command ran");
    assert_eq!(text(&editor), "a\nb\n", "no line was deleted");
}

/// Args reach the named command verbatim — a range or a bang in them is the
/// command's to read, not the line parser's.
#[test]
fn args_are_the_named_commands_args() {
    let mut editor = Editor::boot(CoreDocument::from_text("a\nb\n"));
    let calls = Calls::default();
    register_recorder(&editor, "ob3a-recorder", calls.clone());

    for args in ["%d", "!sh -c id"] {
        editor.handle_effect(Effect::InvokeCommand {
            id: "ob3a-recorder".to_string(),
            args: Args::String(args.to_string()),
        });
    }

    assert_eq!(*calls.lock().unwrap(), vec!["%d", "!sh -c id"]);
    assert_eq!(text(&editor), "a\nb\n", "nothing else ran");
}
