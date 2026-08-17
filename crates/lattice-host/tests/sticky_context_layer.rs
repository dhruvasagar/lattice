//! TC.3b — the pinned context strip: host resolution, per-pane keying, and the
//! reservation the scroll model makes for it.
//!
//! The load-bearing test here is `two_panes_on_one_buffer_resolve_different_context`.
//! Every other per-pane layer in the editor is keyed by `BufferId`, and this one
//! is keyed by `PaneId` precisely because the rows differ per pane rather than
//! just which row is emphasised. If the keying ever regresses to `BufferId`,
//! that is the only test that goes red — and the bug would otherwise be
//! invisible until someone opened a split.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use lattice_cells::context::ContextScope;
use lattice_core::Document as CoreDocument;
use lattice_core::ui::pane::SplitOrientation;
use lattice_host::editor::Editor;
use lattice_host::per_buffer_cache::PerBufferCacheExt;
use lattice_host::wasm_context::ContextScopeCache;

fn scope(start: u32, end: u32) -> ContextScope {
    ContextScope {
        scope_start: start,
        scope_end: end,
        header_start: start,
        header_end: start,
    }
}

/// A 200-line document with two nested scopes: an outer at 10..=180 and an
/// inner at 100..=150.
fn editor_with_scopes() -> Editor {
    let text: String = (0..200).map(|i| format!("line {i}\n")).collect();
    let editor = Editor::boot(CoreDocument::from_text(&text));
    let buffer = editor.document_buffer_id;
    editor.wasm_context.cache.insert_for(
        buffer,
        ContextScopeCache {
            parse_version: 0,
            scopes: vec![scope(10, 180), scope(100, 150)],
        },
    );
    editor
}

#[test]
fn a_pane_pins_the_enclosing_scopes_whose_headers_scrolled_away() {
    let editor = editor_with_scopes();
    let buffer = editor.document_buffer_id;

    // Cursor deep inside both scopes, view starting below both headers.
    let lines = editor.resolve_sticky_context_lines(buffer, 120, 110, 40);
    assert_eq!(
        &*lines,
        &[10, 100],
        "outermost first — the row nearest the text is the nearest scope"
    );
}

#[test]
fn a_visible_header_is_not_pinned() {
    let editor = editor_with_scopes();
    let buffer = editor.document_buffer_id;

    // View starts at 95: the inner header (100) is on screen, the outer (10)
    // is not. Pinning the inner one would spend a row duplicating a line the
    // user can already read.
    let lines = editor.resolve_sticky_context_lines(buffer, 120, 95, 40);
    assert_eq!(&*lines, &[10]);
}

#[test]
fn a_buffer_with_no_cached_scopes_pins_nothing() {
    let text: String = (0..50).map(|i| format!("line {i}\n")).collect();
    let editor = Editor::boot(CoreDocument::from_text(&text));
    let buffer = editor.document_buffer_id;

    // The overwhelmingly common case: no context plugin loaded. This is the
    // fast path, and it must cost nothing and show nothing.
    let lines = editor.resolve_sticky_context_lines(buffer, 20, 10, 40);
    assert!(lines.is_empty());
}

/// THE pane-keying proof. One buffer, two panes, cursors in different scopes →
/// different strips. This fails, and only this fails, if
/// `Editor::sticky_context_for` is ever re-keyed on `BufferId`.
#[test]
fn two_panes_on_one_buffer_resolve_different_context() {
    let mut editor = editor_with_scopes();
    let buffer = editor.document_buffer_id;
    editor.pane_tree.split_active(SplitOrientation::Vertical);

    let ids: Vec<_> = editor.pane_tree.leaves().iter().map(|l| l.id).collect();
    assert_eq!(ids.len(), 2, "precondition: two panes");

    // The cells are distinct objects, one per pane — not one shared per buffer.
    let a = editor.sticky_context_for(ids[0]);
    let b = editor.sticky_context_for(ids[1]);
    assert!(
        !Arc::ptr_eq(&a, &b),
        "each pane owns its own strip cell; sharing one per buffer would make \
         a split show the wrong context in one half"
    );
    // Idempotent per pane, so worker writes and renderer reads stay coherent.
    assert!(Arc::ptr_eq(&a, &editor.sticky_context_for(ids[0])));

    // Pane A sits inside both scopes; pane B is outside the inner one. Same
    // buffer, same cached scopes, different answers.
    let a_lines = editor.resolve_sticky_context_lines(buffer, 120, 110, 40);
    let b_lines = editor.resolve_sticky_context_lines(buffer, 170, 160, 40);
    assert_eq!(&*a_lines, &[10, 100]);
    assert_eq!(
        &*b_lines,
        &[10],
        "the second pane's cursor is past the inner scope, so it pins only the \
         outer one — a buffer-keyed layer could not represent this"
    );
}

/// The strip is pinned above the scroll window, so it must shrink the window
/// the cursor is kept inside. Otherwise the cursor can settle visually behind
/// the strip — the same class of bug the headerline's `sticky_count` reservation
/// already exists to prevent.
#[test]
fn the_reservation_shrinks_the_window_the_cursor_is_kept_in() {
    let mut editor = editor_with_scopes();
    let pane_id = editor.pane_tree.active().id;

    // Publish a two-row strip the way the worker would.
    editor.sticky_context_for(pane_id).store(Arc::new(
        lattice_host::sticky_context::StickyContext {
            rows: vec![
                lattice_host::sticky_context::StickyContextRow {
                    source_line: 10,
                    cells: Arc::from([] as [lattice_cells::Cell; 0]),
                },
                lattice_host::sticky_context::StickyContextRow {
                    source_line: 100,
                    cells: Arc::from([] as [lattice_cells::Cell; 0]),
                },
            ],
            version: Default::default(),
            bg: None,
        },
    ));

    editor.viewport_height = 10;
    editor.cursor.line = 199;
    editor.scroll = 0;
    editor.ensure_cursor_visible();
    let with_strip = editor.scroll;

    // Same geometry, no strip: the cursor needs less scrolling because the
    // window is two rows taller.
    editor.sticky_context_for(pane_id).store(Arc::new(
        lattice_host::sticky_context::StickyContext::empty(),
    ));
    editor.scroll = 0;
    editor.ensure_cursor_visible();
    let without_strip = editor.scroll;

    assert!(
        with_strip > without_strip,
        "two pinned rows must cost two rows of window: scrolled to {with_strip} \
         with the strip vs {without_strip} without it"
    );
    assert_eq!(
        with_strip - without_strip,
        2,
        "exactly the reservation, never more — an over-reservation would jump \
         the view for no visible reason"
    );
}

// ── `:customize` sees runtime (plugin) options ──────────────────────────────

/// `:customize` was built entirely on the compile-time `linkme` decl slices,
/// which only native options join — so a plugin option reached `:set` and
/// `:describe-option` (both read the live registry) but could never appear in
/// the one surface built for BROWSING options.
///
/// The namespace is the group, which is what native groups already are
/// (`ai.log` + `ai.log_level` → `ai`), and the host assigns the prefix, so a
/// plugin cannot land in another's group.
#[test]
fn customize_groups_runtime_plugin_options_by_namespace() {
    use lattice_config::ConfigRegistry;

    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    // Register options the way the plugin config seam does: namespaced by
    // plugin id, at runtime, with no compile-time declaration.
    let cfg = ConfigRegistry::default();
    for (name, default) in [("max-lines", "0"), ("anchor", "cursor")] {
        cfg.register(lattice_config::option::Option::new(
            format!("treesitter-context.{name}"),
            default.to_string(),
            "A plugin-contributed option.",
        ));
    }
    editor.config = std::sync::Arc::new(cfg);

    let group = editor
        .build_customize_group_content("treesitter-context")
        .expect("a plugin namespace is a group even with no compile-time decl");
    let text = group.lines().join("\n");
    assert!(
        text.contains("treesitter-context.max-lines"),
        "the plugin's options are listed: {text}"
    );
    assert!(text.contains("treesitter-context.anchor"));

    // And the picker offers the namespace as a navigable group.
    let picker = editor.build_customize_picker_content();
    let ptext = picker.lines().join("\n");
    // Assert the RENDERED name, not the link target: `HelpContent` parses
    // markup out of `lines()`, so `customize:<group>` never appears there.
    assert!(
        ptext.contains("- treesitter-context (2)"),
        "the group is listed in the picker with its option count: {ptext}"
    );
}

/// Losing focus to `:` must be a DIMMING change and nothing else.
///
/// While a prompt is focused the document pane is still `pane_tree.active()`,
/// but the live `self.cursor` / `self.scroll` have moved to the command-line
/// buffer. Reading them for the document pane resolved it at cursor 0 /
/// scroll 0 — nothing is above line 0, so the strip emptied the instant `:`
/// opened.
#[test]
fn opening_the_command_line_does_not_empty_the_strip() {
    let mut editor = editor_with_scopes();
    let buffer = editor.document_buffer_id;

    // Scrolled into both scopes, the state the strip is for.
    editor.cursor.line = 120;
    editor.scroll = 110;
    {
        let pane = editor.pane_tree.active_mut();
        pane.cursor.line = 120;
        pane.scroll = 110;
        pane.viewport_height = 40;
        pane.viewport_width = 100;
    }
    let before = editor.resolve_sticky_context_lines(buffer, 120, 110, 40);
    assert_eq!(&*before, &[10, 100], "precondition: the strip is showing");

    // Open `:`. The pane keeps its stashed view; the live cursor/scroll go to
    // the command line.
    editor.open_command_line("");

    // The document pane's inputs must still resolve from ITS view, not the
    // command line's. `build_cells_panes` is private, so assert through the
    // same values it now passes: the pane's stashed cursor and scroll.
    let leaf = editor
        .pane_tree
        .leaves()
        .iter()
        .find(|l| l.buffer_id == buffer)
        .expect("the document pane is still open behind the prompt")
        .clone();
    assert_eq!(
        leaf.cursor.line, 120,
        "the pane's stashed cursor survives the focus change"
    );
    assert_eq!(leaf.scroll, 110, "and so does its scroll");

    let after = editor.resolve_sticky_context_lines(buffer, leaf.cursor.line, leaf.scroll, 40);
    assert_eq!(
        &*after,
        &[10, 100],
        "the strip is unchanged by losing focus — dimming only"
    );
}
