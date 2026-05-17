//! Phase 5.5.LSP.1 step 2: pure LSP utility helpers, lifted from
//! `lattice_ui_tui::app`. These functions translate between the
//! editor's internal byte-indexed shapes and `lsp-types`'s wire
//! shapes (UTF-16 column positions, hover-content markdown). They
//! depend only on `lattice_core::Buffer`, `lattice_grammar::Position`,
//! and `lattice_lsp::position` -- all host-side -- so they have no
//! reason to live in the renderer crate.
//!
//! `lattice_ui_tui::app` re-exports both names under their original
//! `crate::app::` paths so the existing ~18 call sites (App-side LSP
//! request helpers) continue to compile unchanged through the rest
//! of the LSP cluster migration.

use lattice_core::Buffer;
use lattice_protocol::Position;

/// Convert an editor-side `Position` (line + utf-8 byte column)
/// into the LSP-side `lsp_types::Position` (line + utf-16 code-
/// unit column). Returns `None` when the line index is past the
/// end of the buffer -- e.g. cursor on a sentinel row past EOF.
pub fn app_to_lsp_position(buffer: &Buffer, p: Position) -> Option<lsp_types::Position> {
    let line_text = buffer.line(p.line)?;
    let character = lattice_lsp::position::utf8_byte_to_utf16_column(&line_text, p.byte);
    Some(lsp_types::Position {
        line: p.line,
        character,
    })
}

/// Render an LSP `HoverContents` payload to a markdown string the
/// renderer's hover popup pipeline can highlight via the markdown
/// grammar.
///
/// `MarkedString::String(s)` keeps `s` verbatim.
/// `MarkedString::LanguageString { language, value }` wraps
/// `value` in a fenced code block tagged with `language` so the
/// markdown injection picks it up. `MarkupContent` arrives pre-
/// rendered as either markdown or plaintext (we treat plaintext
/// as already-good markdown). `Array` joins each element with two
/// newlines so blocks separate cleanly.
pub fn hover_contents_to_markdown(contents: &lsp_types::HoverContents) -> String {
    fn marked_to_markdown(m: &lsp_types::MarkedString) -> String {
        match m {
            lsp_types::MarkedString::String(s) => s.clone(),
            lsp_types::MarkedString::LanguageString(ls) => {
                format!("```{}\n{}\n```", ls.language, ls.value)
            }
        }
    }
    match contents {
        lsp_types::HoverContents::Scalar(m) => marked_to_markdown(m),
        lsp_types::HoverContents::Array(items) => items
            .iter()
            .map(marked_to_markdown)
            .collect::<Vec<_>>()
            .join("\n\n"),
        lsp_types::HoverContents::Markup(m) => m.value.clone(),
    }
}
