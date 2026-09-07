//! A transient row fires on its OWN key — `<Space>`, `<CR>`, `<Tab>` alike.
//!
//! Org's agenda menu binds one. It rendered, `<C-n>` reached it and `<CR>`
//! fired it — but pressing space did nothing at all, because the dispatcher
//! built the typed key by pushing the raw `char`, producing `" "`, while the
//! spec spells it `<Space>` the way every other key surface in lattice does
//! (the chord parser accepts `<Space>`, `Display` emits it, the keymap indexes
//! it). Two spellings of one keypress, and the mismatch is silent: a menu row
//! that quietly cannot be typed.
//!
//! `<CR>` and `<Tab>` failed one layer earlier than `<Space>`: the picker
//! claimed them in `translate_picker` (accept-selected / select-next) before
//! any spec was consulted, so no amount of spelling agreement could help. The
//! precedence rule is now "a spec may claim a key, except the ones that
//! navigate or escape" — a menu you cannot leave is a worse failure than a key
//! you cannot bind.
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
use lattice_protocol::chord::{KeyChord, SpecialKey};

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
            items: vec![
                row("<Space>", "spacey"),
                row("a", "plain"),
                row("<CR>", "entered"),
                row("<Tab>", "tabbed"),
            ],
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

/// `<CR>` is the sharpest case: `translate_picker` turns it into
/// `PickerAccept` (fire the SELECTED row) long before any spec is consulted,
/// so a row keyed `<CR>` rendered and was unreachable by its own key. Same
/// silent shape as `<Space>`, one layer earlier.
#[test]
fn a_row_keyed_cr_fires_instead_of_accepting_the_selection() {
    let mut editor = menu();
    press(&mut editor, KeyChord::special(SpecialKey::Enter));
    assert!(
        flag_on(&editor, "entered"),
        "a spec that binds `<CR>` must get it, not lose it to accept-selected"
    );
}

/// `<Tab>` is normally select-next. A spec may claim it — arrows and
/// `<C-n>`/`<C-p>` still navigate, so the menu stays usable.
#[test]
fn a_row_keyed_tab_fires_instead_of_selecting_next() {
    let mut editor = menu();
    press(&mut editor, KeyChord::special(SpecialKey::Tab));
    assert!(
        flag_on(&editor, "tabbed"),
        "a spec that binds `<Tab>` gets it"
    );
}

/// The invariant that makes claiming safe: a menu can always be navigated and
/// always be left. `<Down>` must still move the selection even though this
/// spec claims other special keys — if a spec could take the navigation keys
/// it could strand the user in a menu with no way out, which is a worse
/// failure than a key it cannot bind.
#[test]
fn navigation_and_escape_are_never_claimable() {
    let mut editor = menu();
    let before = editor.picker.as_ref().unwrap().transient_selected;
    press(&mut editor, KeyChord::special(SpecialKey::Down));
    let after = editor
        .picker
        .as_ref()
        .expect("menu still up — `<Down>` must not fire a row")
        .transient_selected;
    assert_ne!(before, after, "`<Down>` still navigates");

    // And `<Esc>` still leaves rather than firing anything.
    press(&mut editor, KeyChord::special(SpecialKey::Esc));
    assert!(editor.picker.is_none(), "`<Esc>` still closes the menu");
}

/// A key the spec does NOT claim keeps its picker meaning. This is the
/// regression guard for retargeting: `<CR>` on a menu with no `<CR>` row must
/// still accept the selected row rather than fall into a dead branch.
#[test]
fn an_unclaimed_special_key_keeps_its_picker_meaning() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let mut picker = Picker::new("t", PickerSource::Buffers, PickerAction::OpenFile);
    // A spec claiming nothing special.
    picker.transient = Some(Arc::new(TransientSpec {
        title: "plainmenu".to_string(),
        groups: vec![TransientGroup {
            label: String::new(),
            items: vec![TransientItem {
                key: vec!["a".to_string()],
                label: "plain".to_string(),
                description: String::new(),
                kind: TransientItemKind::Flag {
                    name: "plain".to_string(),
                    default: false,
                },
            }],
        }],
        preview: None,
        footer: None,
    }));
    editor.picker = Some(picker);

    let action = press(&mut editor, KeyChord::special(SpecialKey::Enter));
    assert!(
        matches!(action, Action::PickerAccept),
        "an unclaimed `<CR>` must still mean accept-selected, got {action:?}"
    );
}
