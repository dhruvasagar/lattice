//! A transient row keyed `<Space>` fires when you press space.
//!
//! Org's agenda menu binds one. It rendered, `<C-n>` reached it and `<CR>`
//! fired it — but pressing space did nothing at all, because the dispatcher
//! built the typed key by pushing the raw `char`, producing `" "`, while the
//! spec spells it `<Space>` the way every other key surface in lattice does
//! (the chord parser accepts `<Space>`, `Display` emits it, the keymap indexes
//! it). Two spellings of one keypress, and the mismatch is silent: a menu row
//! that quietly cannot be typed.
//!
//! Driven through `dispatch_chord`, per the standing rule in
//! `transient_esc_unwinds.rs` — the sibling bug there lived behind green unit
//! tests precisely because they called the handler instead of pressing a key.
//! A test that called `resolve_key(" ")` directly would pass against this bug
//! AND against the fix, since both halves of the mismatch are in the caller.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use lattice_core::Document as CoreDocument;
use lattice_host::action::Action;
use lattice_host::editor::Editor;
use lattice_picker::{
    Picker, PickerAction, PickerSource, TransientGroup, TransientItem, TransientItemKind,
    TransientSpec,
};
use lattice_protocol::chord::KeyChord;

/// One row per key spelling under test, each a flag so firing is observable
/// as a toggled flag rather than through a side effect.
fn spec() -> Arc<TransientSpec> {
    let row = |key: &str, flag: &str| TransientItem {
        key: vec![key.to_string()],
        label: flag.to_string(),
        description: String::new(),
        kind: TransientItemKind::Flag {
            name: flag.to_string(),
            default: false,
        },
    };
    Arc::new(TransientSpec {
        title: "agenda".to_string(),
        groups: vec![TransientGroup {
            label: String::new(),
            items: vec![row("<Space>", "spacey"), row("a", "plain")],
        }],
        preview: None,
        footer: None,
    })
}

fn menu() -> Editor {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let mut picker = Picker::new("t", PickerSource::Buffers, PickerAction::OpenFile);
    picker.transient = Some(spec());
    editor.picker = Some(picker);
    editor
}

fn press(editor: &mut Editor, chord: KeyChord) -> Action {
    let mut partial: Vec<KeyChord> = Vec::new();
    editor.dispatch_chord(chord, &mut partial)
}

/// Is `flag` currently on? A fired `Flag` row toggles it in the transient's
/// live state.
fn flag_on(editor: &Editor, flag: &str) -> bool {
    editor
        .picker
        .as_ref()
        .and_then(|p| p.transient_state.get(flag))
        .map(|v| matches!(v, lattice_picker::TransientValue::Bool(true)))
        .unwrap_or(false)
}

#[test]
fn a_row_keyed_space_fires_when_space_is_pressed() {
    let mut editor = menu();
    assert!(!flag_on(&editor, "spacey"), "sanity: starts off");

    press(&mut editor, KeyChord::char(' '));

    assert!(
        flag_on(&editor, "spacey"),
        "a row spelled `<Space>` must fire on the space key — the spelling \
         every other key surface in lattice uses"
    );
}

/// The regression risk of canonicalising the typed key: ordinary letters must
/// keep working exactly as before.
#[test]
fn an_ordinary_letter_row_still_fires() {
    let mut editor = menu();
    press(&mut editor, KeyChord::char('a'));
    assert!(
        flag_on(&editor, "plain"),
        "canonical spelling must leave plain-char rows untouched"
    );
}
