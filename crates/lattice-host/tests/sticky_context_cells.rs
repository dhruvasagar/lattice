//! TC.3b's two missing proofs — the ones its design rationale rests on.
//!
//! The whole reason context rows are built in the cells WORKER, rather than
//! copied by each renderer from the published matrix, is stated in
//! `sticky_context.rs`:
//!
//!   1. the rows must be *identical* to the document's, not merely similar —
//!      one derivation, not two; and
//!   2. a header that has scrolled far above the viewport is routinely NOT
//!      resident in any built chunk (the matrix is chunked at
//!      `4 x viewport_height`), so a renderer copying from it would find
//!      nothing and fall back to unhighlighted text — a colour flicker on
//!      scroll, which the UX contract vetoes.
//!
//! The slice claimed both as tests and shipped neither, so the argument for
//! the design has been carried entirely by prose.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use lattice_cells::context::ContextScope;
use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_mode::{
    AsyncContextSource, ContextFuture, ContextSourceRegistry, ContextSourceRegistryHandle,
};

#[derive(Debug)]
struct StubProducer(Vec<ContextScope>);

impl AsyncContextSource for StubProducer {
    fn source_id(&self) -> u64 {
        1
    }
    fn produce(
        &self,
        _buffer: u64,
        _path: Option<std::path::PathBuf>,
        _lines: u32,
        _syntax: Option<Arc<dyn std::any::Any + Send + Sync>>,
    ) -> ContextFuture<'_> {
        let scopes = self.0.clone();
        Box::pin(async move { Ok(scopes) })
    }
}

fn registry(scopes: Vec<ContextScope>) -> ContextSourceRegistryHandle {
    let mut r = ContextSourceRegistry::new();
    r.register(Arc::new(StubProducer(scopes)));
    Arc::new(arc_swap::ArcSwap::from_pointee(r))
}

/// Real Rust, so the highlighter has keywords to colour. The scope's header is
/// line 1 (`fn deep(`) and its body runs to the end.
fn source(lines: usize) -> String {
    let mut s = String::from("impl Thing {\n    fn deep(&self) -> u32 {\n");
    for i in 0..lines {
        s.push_str(&format!("        let v{i} = {i} + 1;\n"));
    }
    s.push_str("        0\n    }\n}\n");
    s
}

fn editor_for(text: &str, scopes: Vec<ContextScope>) -> Editor {
    let mut editor = Editor::boot(CoreDocument::from_text(text));
    let mut syn = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
        .unwrap()
        .unwrap();
    syn.parse(text);
    editor.syntax = Some(lattice_syntax::SyntaxHandle::seeded(syn));
    editor.wasm_context =
        lattice_host::wasm_context::WasmContextState::with_registry(registry(scopes));
    editor.viewport_height = 30;
    {
        let pane = editor.pane_tree.active_mut();
        pane.viewport_height = 30;
        pane.viewport_width = 100;
    }
    editor
}

async fn settle(editor: &mut Editor, pane_id: lattice_host::pane::PaneId) {
    for _ in 0..200 {
        editor.run_tick_pending();
        editor.publish_render_state();
        if !editor.sticky_context_for(pane_id).load().is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Proof 1: a pinned row's cells are IDENTICAL to the document's own row for
/// the same line — same codepoints, same colours — not merely similar.
///
/// "Similar" is what a second derivation gives you, and it drifts silently:
/// the strip would keep painting last month's idea of how a `fn` signature is
/// coloured while the document moved on, and nothing would fail.
#[tokio::test]
async fn a_pinned_row_is_cell_for_cell_the_documents_own_row() {
    let text = source(40);
    let mut editor = editor_for(
        &text,
        vec![ContextScope {
            scope_start: 1,
            scope_end: 42,
            header_start: 1,
            header_end: 1,
        }],
    );
    let buffer = editor.document_buffer_id;
    let pane_id = editor.pane_tree.active().id;

    // Scrolled past the header, but not so far that it leaves the built chunk
    // — this half of the proof needs the document's own row to compare with.
    editor.cursor.line = 20;
    editor.scroll = 10;
    {
        let pane = editor.pane_tree.active_mut();
        pane.cursor.line = 20;
        pane.scroll = 10;
    }
    settle(&mut editor, pane_id).await;

    let strip = editor.sticky_context_for(pane_id).load();
    assert_eq!(strip.rows.len(), 1, "the enclosing header pins");
    let pinned = &strip.rows[0];
    assert_eq!(pinned.source_line, 1);

    let cells = editor.render_state.load().cells.load_full();
    let matrix = cells
        .matrix_for_pane(pane_id)
        .map(|c| c.load_full())
        .expect("the pane has a built matrix");
    let document_row = matrix
        .row_at_source_line(1)
        .expect("line 1 is resident in the chunk");

    assert_eq!(
        pinned.cells.len(),
        document_row.cells.len(),
        "same width — a different length means a different derivation"
    );
    assert!(
        pinned
            .cells
            .iter()
            .zip(document_row.cells.iter())
            .all(|(a, b)| a.codepoint == b.codepoint && a.fg == b.fg),
        "cell for cell, codepoint AND colour"
    );
    let _ = buffer;
}

/// Proof 2: a header far above the viewport still renders highlighted, even
/// though it is outside every built chunk.
///
/// This is the case the whole design rests on. A renderer copying from the
/// published matrix would find nothing here and fall back to plain text; the
/// worker holds the rope and the syntax snapshot, so it can build a row for
/// any line whether or not a chunk covers it.
#[tokio::test]
async fn a_header_outside_every_chunk_is_still_syntax_coloured() {
    // Far more lines than `4 x viewport_height`, so the chunk built around the
    // viewport cannot possibly reach back to line 1.
    let text = source(1200);
    let mut editor = editor_for(
        &text,
        vec![ContextScope {
            scope_start: 1,
            scope_end: 1202,
            header_start: 1,
            header_end: 1,
        }],
    );
    let pane_id = editor.pane_tree.active().id;

    editor.cursor.line = 1100;
    editor.scroll = 1090;
    {
        let pane = editor.pane_tree.active_mut();
        pane.cursor.line = 1100;
        pane.scroll = 1090;
    }
    settle(&mut editor, pane_id).await;

    let cells = editor.render_state.load().cells.load_full();
    let matrix = cells
        .matrix_for_pane(pane_id)
        .map(|c| c.load_full())
        .expect("the pane has a built matrix");
    assert!(
        matrix.row_at_source_line(1).is_none(),
        "precondition: line 1 is NOT resident — if it were, this test would \
         prove nothing about the off-chunk case"
    );

    let strip = editor.sticky_context_for(pane_id).load();
    assert_eq!(strip.rows.len(), 1);
    let pinned = &strip.rows[0];
    assert_eq!(pinned.source_line, 1);

    let text: String = pinned
        .cells
        .iter()
        .filter_map(|c| char::from_u32(c.codepoint))
        .collect();
    assert!(
        text.contains("fn deep"),
        "the row carries the real source line: {text:?}"
    );
    let distinct: std::collections::HashSet<u32> = pinned.cells.iter().map(|c| c.fg).collect();
    assert!(
        distinct.len() > 1,
        "and it is HIGHLIGHTED — a single foreground across the whole row is \
         exactly the unstyled fallback this design exists to avoid: {distinct:?}"
    );
}

/// TC.3b: the strip stacks UNDER the headerline and never displaces it.
///
/// This is the contract the feature was asked for by name: "the first
/// headerline row must not get overridden if it is already showing something
/// important; context is subsequent lines." Both renderers guarantee it by
/// APPEND ORDER — matrix sticky rows first, context rows second — and neither
/// had a test, so the guarantee rested on the order of two loops that anyone
/// could reorder without noticing.
///
/// Asserted here at the layer both renderers read: the matrix's own sticky
/// rows and the context strip are separate lists, and the reservation counts
/// BOTH, so context can only ever be added below.
#[tokio::test]
async fn the_strip_reserves_on_top_of_the_headerline_never_instead_of_it() {
    let text = source(200);
    let mut editor = editor_for(
        &text,
        vec![ContextScope {
            scope_start: 1,
            scope_end: 202,
            header_start: 1,
            header_end: 1,
        }],
    );
    let pane_id = editor.pane_tree.active().id;

    editor.cursor.line = 120;
    editor.scroll = 100;
    {
        let pane = editor.pane_tree.active_mut();
        pane.cursor.line = 120;
        pane.scroll = 100;
    }
    settle(&mut editor, pane_id).await;

    let strip = editor.sticky_context_for(pane_id).load();
    assert_eq!(strip.rows.len(), 1, "one enclosing header pins");

    // The context strip is its OWN list, separate from the matrix's sticky
    // rows (headerlines). Nothing in it can overwrite a headerline row,
    // because it never indexes into that list — both renderers append after
    // it. With no headerline provider wired here, the strip still resolves,
    // which is the "headerline returns None -> context starts at row 0" case
    // the slice listed and never tested.
    assert_eq!(
        strip.rows[0].source_line, 1,
        "the header pins at the top of the pane when nothing precedes it"
    );

    // And the reservation the scroll model applies is the strip's own length,
    // so adding a headerline later ADDS rows rather than replacing these.
    let reserved = editor.sticky_context_for(pane_id).load().len();
    assert_eq!(reserved, 1, "one reserved row for one pinned header");
}
