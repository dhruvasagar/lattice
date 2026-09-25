//! `<CR>` fires the transient row that `<C-n>` / `<C-p>` selected.
//!
//! Typing a row's key always worked (it routes through `consume_transient_key`).
//! But scrolling to a row with `<C-n>`/`<C-p>` and pressing `<CR>` did nothing
//! in the GPUI peer: its `PickerAccept` interceptor called `do_picker_accept`
//! directly — a no-op for a transient — instead of the transient-aware accept
//! the TUI dispatch arm used. Both renderers now route through
//! `Editor::do_picker_or_transient_accept`, so `<CR>` fires the SELECTED row.
//!
//! Driven through `dispatch_chord` (the standing rule from
//! `transient_keys_reach_their_rows.rs`): a test that called the handler
//! directly would pass against the broken GPUI path too. This exercises the
//! shared accept method the fix funnels both renderers into. `Flag` rows make
//! firing observable as a toggled flag rather than a side effect.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_picker::{
    Picker, PickerAction, PickerSource, TransientGroup, TransientItem, TransientItemKind,
    TransientSpec, TransientValue,
};
use lattice_protocol::chord::{KeyChord, SpecialKey};

/// Three flag rows keyed `a` / `b` / `c` — none keyed `<CR>`, so `<CR>` means
/// accept-selected rather than firing a row by its own key.
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
        title: "t".to_string(),
        groups: vec![TransientGroup {
            label: String::new(),
            items: vec![row("a", "fa"), row("b", "fb"), row("c", "fc")],
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

fn flag_on(editor: &Editor, flag: &str) -> bool {
    editor
        .picker
        .as_ref()
        .and_then(|p| p.transient_state.get(flag))
        .map(|v| matches!(v, TransientValue::Bool(true)))
        .unwrap_or(false)
}

fn press_cr(editor: &mut Editor) {
    let mut partial: Vec<KeyChord> = Vec::new();
    let _ = editor.dispatch_chord(KeyChord::special(SpecialKey::Enter), &mut partial);
}

/// The reported bug: scroll to the third row and press `<CR>` — it must fire
/// THAT row, not the first, and not the other unselected rows.
#[test]
fn cr_fires_the_scrolled_to_row() {
    let mut editor = menu();
    // `<C-n>` twice → select the third row ("c").
    let p = editor.picker.as_mut().unwrap();
    p.transient_select_next();
    p.transient_select_next();

    press_cr(&mut editor);

    assert!(
        flag_on(&editor, "fc"),
        "<CR> must fire the row `<C-n>`/`<C-p>` scrolled to"
    );
    assert!(!flag_on(&editor, "fa"), "the first row must NOT fire");
    assert!(!flag_on(&editor, "fb"), "an unselected row must NOT fire");
}

/// With no scrolling, `<CR>` fires the first (default-selected) row.
#[test]
fn cr_fires_the_first_row_by_default() {
    let mut editor = menu();
    press_cr(&mut editor);
    assert!(
        flag_on(&editor, "fa"),
        "<CR> with nothing scrolled fires the default selection"
    );
    assert!(!flag_on(&editor, "fb"));
    assert!(!flag_on(&editor, "fc"));
}
