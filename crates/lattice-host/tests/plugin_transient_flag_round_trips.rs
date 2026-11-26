//! A plugin menu's `flag` row, from open to the fired command.
//!
//! The WIT crosses `transient-item-kind.flag`, and
//! `spec_from_wit` turns it into the host's own `TransientItemKind::Flag`
//! (pinned by `lattice-plugin-host`'s `a_flag_row_crosses_with_its_name_and_default`).
//! This pins the other half with that native row, seated the way a guest
//! menu is seated (`open_transient`): the guest's default reaches the state,
//! the key flips it, and the command fired from the menu receives it in its
//! `args-schema` slot by name. Org export's `C-s` / `C-b` are this shape.
//!
//! Driven through the KEY, not the fire helper (`transient_row_carries_its_own_args.rs`).

#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use lattice_core::Document as CoreDocument;
use lattice_grammar::{ArgValue, Args};
use lattice_host::editor::Editor;
use lattice_mode::ActionHandlerRegistryHandle;
use lattice_picker::{TransientGroup, TransientItem, TransientItemKind, TransientSpec};
use lattice_protocol::chord::KeyChord;

type Seen = Arc<Mutex<Option<Args>>>;

fn bool_arg(name: &'static str) -> lattice_grammar::ArgSpec {
    lattice_grammar::ArgSpec {
        name: name.into(),
        kind: lattice_grammar::args::ArgKind::Bool,
        doc: "a switch".into(),
        prompt: format!("{name}:").into(),
        default: lattice_grammar::args::ArgDefault::None,
        completion: None,
        picker: None,
    }
}

/// An editor with `export-to-html` registered with a two-switch schema, in
/// the ORDER `body-only, subtree`: the reverse of the menu's rows, so a
/// projection by position rather than by name would swap them.
fn boot() -> (
    Editor,
    lattice_protocol::ids::CommandId,
    Seen,
    lattice_mode::ActionHandlerRegistration,
) {
    let editor = Editor::boot(CoreDocument::from_text("* One\n"));
    let mut registry = (**editor.registry.load()).clone();
    registry.register_action(
        "export-to-html",
        "records the switches it fired with (test)",
        lattice_grammar::registry::ActionSpec {
            args_schema: vec![bool_arg("body-only"), bool_arg("subtree")],
            apply: Arc::new(|_| Ok(lattice_grammar::Effect::None)),
        },
    );
    let cmd = registry.id_by_name("export-to-html").unwrap();
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
    (editor, cmd, seen, reg)
}

/// What the guest's menu converts to: two switches, `subtree` defaulting ON
/// (as a guest restoring it from its store would), and one action row with
/// no args of its own.
fn open_menu(editor: &mut Editor, cmd: lattice_protocol::ids::CommandId) {
    let row = |key: &str, label: &str, kind| TransientItem {
        key: vec![key.to_string()],
        label: label.to_string(),
        description: String::new(),
        kind,
    };
    let flag = |name: &str, default| TransientItemKind::Flag {
        name: name.to_string(),
        default,
    };
    editor.open_transient(TransientSpec {
        title: "Export".into(),
        groups: vec![
            TransientGroup {
                label: "Options".into(),
                items: vec![
                    row("s", "subtree", flag("subtree", true)),
                    row("b", "body only", flag("body-only", false)),
                ],
            },
            TransientGroup {
                label: "HTML".into(),
                items: vec![row(
                    "h",
                    "to file",
                    TransientItemKind::Action {
                        command: cmd,
                        args: Args::None,
                    },
                )],
            },
        ],
        preview: None,
        footer: None,
    });
}

fn press(editor: &mut Editor, c: char) {
    let mut partial: Vec<KeyChord> = Vec::new();
    let _ = editor.dispatch_chord(KeyChord::char(c), &mut partial);
}

fn fired(seen: &Seen) -> Vec<ArgValue> {
    let args = seen.lock().unwrap().clone().expect("the action fired");
    args.as_list().expect("a positional list").to_vec()
}

/// Untouched, each switch arrives as the guest set it, `true` included: the
/// projection's fallback for a missing slot is `false`, so a default that
/// never reached the state would read as off.
#[test]
fn the_guests_defaults_reach_the_command() {
    let (mut editor, cmd, seen, _reg) = boot();
    open_menu(&mut editor, cmd);
    press(&mut editor, 'h');
    assert_eq!(
        fired(&seen),
        vec![ArgValue::Bool(false), ArgValue::Bool(true)],
        "body-only off, subtree on, in the schema's order"
    );
}

/// A key flips its switch in place, the menu stays open, and the command
/// fired next receives the flipped values by name.
#[test]
fn a_toggle_flips_the_switch_the_command_receives() {
    let (mut editor, cmd, seen, _reg) = boot();
    open_menu(&mut editor, cmd);
    press(&mut editor, 's');
    press(&mut editor, 'b');
    assert!(
        editor
            .picker
            .as_ref()
            .is_some_and(|p| p.transient.is_some()),
        "a switch leaves the menu open"
    );
    assert!(seen.lock().unwrap().is_none(), "nothing fired yet");
    press(&mut editor, 'h');
    assert_eq!(
        fired(&seen),
        vec![ArgValue::Bool(true), ArgValue::Bool(false)],
        "body-only on, subtree off"
    );
}
