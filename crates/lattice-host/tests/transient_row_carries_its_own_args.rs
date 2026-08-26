//! TR.2a — a transient row can carry its own arguments.
//!
//! Until now a row's arguments came from ONE place: the menu's
//! `TransientState`, projected through the fired command's `args_schema`
//! (MG.17a — the flags and arguments the user toggled before pressing the
//! key). That mechanism is per-MENU, so it cannot distinguish rows that differ
//! only in a parameter — which is precisely the shape a plugin-contributed
//! menu has. Org's capture menu is one action and N rows, one per template,
//! and the row is the only thing that knows which template it is.
//!
//! So `TransientItemKind::Action` gained an `args` slot. Native rows leave it
//! `Args::None` and behave exactly as before; a row that fills it wins over
//! the state projection, because the row's args were chosen when the row was
//! built and the state's were not.
//!
//! Tested through the KEY (`press`), not by calling the fire helper: the
//! standing rule, and the reason `<Esc>`-in-a-submenu shipped broken behind
//! green unit tests.
//!
//! Design: `docs/dev/architecture/plugin-transients.md` §4.

#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_mode::ActionHandlerRegistryHandle;
use lattice_picker::{
    Picker, PickerAction, PickerSource, TransientGroup, TransientItem, TransientItemKind,
    TransientSpec,
};
use lattice_protocol::chord::KeyChord;

/// What the fired action saw.
type Seen = Arc<Mutex<Option<lattice_grammar::Args>>>;

/// Boot an editor with one action registered in BOTH registries the fire path
/// consults — the command registry (which mints the `CommandId` and owns the
/// `args_schema` the state projection reads) and the action-handler registry
/// (which owns the body).
fn boot() -> (
    Editor,
    lattice_protocol::ids::CommandId,
    Seen,
    lattice_mode::ActionHandlerRegistration,
) {
    let editor = Editor::boot(CoreDocument::from_text("* One\n"));

    let mut registry = (**editor.registry.load()).clone();
    registry.register_action(
        "record-transient-args",
        "records the args its row fired with (test)",
        lattice_grammar::registry::ActionSpec {
            // A schema with one named argument, so the state projection has
            // something to produce and "the row wins" is a real contest
            // rather than a vacuous one.
            args_schema: vec![lattice_grammar::ArgSpec {
                name: "key".into(),
                kind: lattice_grammar::args::ArgKind::String,
                doc: "which template".into(),
                prompt: "key:".into(),
                default: lattice_grammar::args::ArgDefault::None,
                completion: None,
                picker: None,
            }],
            apply: Arc::new(|_| Ok(lattice_grammar::Effect::None)),
        },
    );
    let cmd_id = registry.id_by_name("record-transient-args").unwrap();
    editor.registry.store(Arc::new(registry));

    let seen: Seen = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&seen);
    let handlers = editor
        .services
        .get::<ActionHandlerRegistryHandle>()
        .unwrap();
    // The registration token is RAII — handed back so the caller keeps the
    // handler alive for the length of the test rather than unregistering it
    // the moment `boot` returns.
    let reg = (*handlers).register(
        cmd_id,
        Arc::new(move |ctx: &lattice_mode::ActionContext| {
            *sink.lock().unwrap() = Some(ctx.args.clone());
            None
        }),
    );

    (editor, cmd_id, seen, reg)
}

/// Seat a two-row menu: `t` and `n` fire the SAME command, differing only in
/// the args the row carries.
fn seat_keyed_menu(editor: &mut Editor, cmd_id: lattice_protocol::ids::CommandId) {
    let row = |key: &str, template: &str| TransientItem {
        key: vec![key.to_string()],
        label: template.to_string(),
        description: String::new(),
        kind: TransientItemKind::Action {
            command: cmd_id,
            args: lattice_grammar::Args::String(template.to_string()),
        },
    };
    let spec = TransientSpec {
        title: "Capture".into(),
        groups: vec![TransientGroup {
            label: "Templates".into(),
            items: vec![row("t", "todo"), row("n", "note")],
        }],
        preview: None,
        footer: None,
    };
    let mut picker = Picker::new("Capture", PickerSource::Buffers, PickerAction::OpenFile);
    picker.transient_state = lattice_picker::transient_initial_state(&spec);
    picker.transient = Some(Arc::new(spec));
    editor.picker = Some(picker);
}

fn press(editor: &mut Editor, c: char) {
    let mut partial: Vec<KeyChord> = Vec::new();
    let _ = editor.dispatch_chord(KeyChord::char(c), &mut partial);
}

/// The point of the slot: two rows, one command, and the key you pressed is
/// what decides the argument.
#[test]
fn each_row_fires_the_same_command_with_its_own_args() {
    let (mut editor, cmd_id, seen, _reg) = boot();

    seat_keyed_menu(&mut editor, cmd_id);
    press(&mut editor, 't');
    assert_eq!(
        seen.lock().unwrap().clone(),
        Some(lattice_grammar::Args::String("todo".into())),
        "`t` must fire with its own row's args"
    );

    *seen.lock().unwrap() = None;
    seat_keyed_menu(&mut editor, cmd_id);
    press(&mut editor, 'n');
    assert_eq!(
        seen.lock().unwrap().clone(),
        Some(lattice_grammar::Args::String("note".into())),
        "`n` must fire with ITS row's args — not the first row's, and not the \
         menu-wide state projection"
    );
}

/// A row carrying no args of its own is unchanged: the args still come from
/// the menu's state, projected through the command's schema. This is every
/// native menu in the editor, so it is the regression that matters most.
#[test]
fn a_row_without_args_still_reads_the_menu_state() {
    let (mut editor, cmd_id, seen, _reg) = boot();

    let spec = TransientSpec {
        title: "Flags".into(),
        groups: vec![TransientGroup {
            label: String::new(),
            items: vec![TransientItem {
                key: vec!["x".into()],
                label: "go".into(),
                description: String::new(),
                // The bare constructor — what every native row builds.
                kind: TransientItemKind::action(cmd_id),
            }],
        }],
        preview: None,
        footer: None,
    };
    let mut picker = Picker::new("Flags", PickerSource::Buffers, PickerAction::OpenFile);
    picker.transient_state = lattice_picker::transient_initial_state(&spec);
    // What the user toggled before pressing the key, under the schema's name.
    picker.transient_state.insert(
        "key".to_string(),
        lattice_picker::TransientValue::String("from-state".into()),
    );
    picker.transient = Some(Arc::new(spec));
    editor.picker = Some(picker);

    press(&mut editor, 'x');
    let got = seen.lock().unwrap().clone();
    assert!(
        format!("{got:?}").contains("from-state"),
        "a row with no args of its own must still receive the menu's state \
         projection, got: {got:?}"
    );
}
