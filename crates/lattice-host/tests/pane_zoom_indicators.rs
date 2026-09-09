//! Pane zoom indicators (ZP.4) — modeline element + tabline marker.
//!
//! Design: `docs/dev/architecture/pane-zoom.md` §6.
//!
//! Both surfaces are gated by one option, `pane.zoom-indicator`, so
//! every test here sweeps the value rather than checking the default
//! and trusting the other three arms.

use lattice_core::Document as CoreDocument;
use lattice_core::ui::pane::{SplitOrientation, ZoomIndicator};
use lattice_host::editor::Editor;
use lattice_host::modeline::{CORE_ZOOM, resolve_builtin_content};

fn boot_split() -> Editor {
    let mut editor = Editor::boot(CoreDocument::from_text("line-0\nline-1\n"));
    editor.pane_tree.split_active(SplitOrientation::Vertical);
    editor
}

fn set_indicator(editor: &Editor, value: ZoomIndicator) {
    editor
        .config
        .parse_and_set_command(&format!("pane.zoom-indicator={}", value.label()))
        .expect("pane.zoom-indicator is a registered option");
}

/// The zoomed pane's modeline text for `core.zoom`, as the renderers
/// would resolve it.
fn zoom_element_text(editor: &mut Editor) -> String {
    editor.publish_render_state();
    let rs = editor.render_state.load();
    let idx = rs.panes.tree.active_index();
    let pane = rs.panes.tree.leaves()[idx];
    resolve_builtin_content(CORE_ZOOM, &pane, true, &rs, None).plain()
}

fn tabline_texts(editor: &mut Editor) -> Vec<String> {
    editor.publish_render_state();
    let rs = editor.render_state.load();
    rs.tabs
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| item.tabline_text(i))
        .collect()
}

#[test]
fn an_unzoomed_pane_has_no_modeline_marker() {
    let mut e = boot_split();
    assert_eq!(zoom_element_text(&mut e), "", "nothing to mark");
}

#[test]
fn the_modeline_marker_follows_the_option() {
    for (value, want_marker) in [
        (ZoomIndicator::Both, true),
        (ZoomIndicator::Modeline, true),
        (ZoomIndicator::Tabline, false),
        (ZoomIndicator::None, false),
    ] {
        let mut e = boot_split();
        set_indicator(&e, value);
        e.pane_tree.toggle_zoom();

        let text = zoom_element_text(&mut e);
        assert_eq!(
            !text.is_empty(),
            want_marker,
            "pane.zoom-indicator={} — got {text:?}",
            value.label()
        );
    }
}

#[test]
fn the_tabline_marker_follows_the_option() {
    for (value, want_marker) in [
        (ZoomIndicator::Both, true),
        (ZoomIndicator::Tabline, true),
        (ZoomIndicator::Modeline, false),
        (ZoomIndicator::None, false),
    ] {
        let mut e = boot_split();
        set_indicator(&e, value);
        e.pane_tree.toggle_zoom();

        let texts = tabline_texts(&mut e);
        assert_eq!(
            texts[0].contains(" Z "),
            want_marker,
            "pane.zoom-indicator={} — got {texts:?}",
            value.label()
        );
    }
}

/// Unzooming has to take the marker with it. A stale `Z` is worse
/// than no marker: it asserts something false about the layout.
#[test]
fn unzooming_clears_both_markers() {
    let mut e = boot_split();
    e.pane_tree.toggle_zoom();
    assert!(!zoom_element_text(&mut e).is_empty(), "precondition");
    assert!(tabline_texts(&mut e)[0].contains(" Z "), "precondition");

    e.pane_tree.toggle_zoom();
    assert_eq!(zoom_element_text(&mut e), "");
    assert!(!tabline_texts(&mut e)[0].contains(" Z "));
}

/// The tabline marker's whole reason to exist: it is the only surface
/// that reports a tab you are not looking at. Zoom rides on the
/// `PaneTree` that `TabSlot` stashes, so a background tab keeps it —
/// and the publisher has to read the STASHED tree for inactive tabs,
/// not the live one.
#[test]
fn a_background_tab_reports_its_zoom_in_the_tabline() {
    let mut e = boot_split();
    e.pane_tree.toggle_zoom();

    e.do_new_tab();
    assert!(
        !e.pane_tree.is_zoomed(),
        "precondition: the new tab is not zoomed"
    );

    let texts = tabline_texts(&mut e);
    assert_eq!(texts.len(), 2, "two tabs open");
    assert!(
        texts[0].contains(" Z "),
        "the backgrounded tab still reports its zoom: {texts:?}"
    );
    assert!(
        !texts[1].contains(" Z "),
        "the foreground tab is not zoomed: {texts:?}"
    );
}
