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

/// 5.5.LSP.2: word-class byte predicate -- ASCII alphanumerics
/// and `_`. Mirrors the existing host-side `is_word_char_byte` in
/// `dispatch.rs`; kept module-local to `lsp_helpers` for use by
/// [`word_under_cursor`].
fn is_word_char_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 5.5.LSP.2: extract the word-class span straddling `cursor` on
/// the cursor's line. Returns `None` when the cursor is not on a
/// word character -- "no symbol under cursor" is preferable to a
/// label that jumps to a different identifier than the user
/// pointed at. Used by the LSP nav / references dispatchers to
/// label the tag-stack entry + the picker title.
pub fn word_under_cursor(buffer: &Buffer, cursor: Position) -> Option<String> {
    let line = buffer.line(cursor.line)?;
    let bytes = line.as_bytes();
    let byte_idx = cursor.byte as usize;
    if byte_idx >= bytes.len() || !is_word_char_byte(bytes[byte_idx]) {
        return None;
    }
    let mut start = byte_idx;
    while start > 0 && is_word_char_byte(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = byte_idx;
    while end < bytes.len() && is_word_char_byte(bytes[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes[start..end]).into_owned())
}

/// 5.5.LSP.2: flatten an LSP `GotoDefinitionResponse` (Scalar /
/// Array / Link) into a uniform `Vec<Location>`. The `Link` shape
/// carries richer per-result info (origin selection range used to
/// highlight the symbol the user clicked); we drop it for now and
/// keep the target location only -- the App's jump path is
/// position-only. When 4.2.d's picker buffer lands the link
/// metadata (e.g., `target_selection_range` for narrower jump
/// destinations) becomes useful and this function gains a richer
/// sibling.
pub fn definition_response_to_locations(
    resp: lsp_types::GotoDefinitionResponse,
) -> Vec<lsp_types::Location> {
    match resp {
        lsp_types::GotoDefinitionResponse::Scalar(loc) => vec![loc],
        lsp_types::GotoDefinitionResponse::Array(locs) => locs,
        lsp_types::GotoDefinitionResponse::Link(links) => links
            .into_iter()
            .map(|l| lsp_types::Location {
                uri: l.target_uri,
                // `target_selection_range` is the narrower symbol
                // range; `target_range` is the enclosing block.
                // Picker UX usually wants the narrower one.
                range: l.target_selection_range,
            })
            .collect(),
    }
}

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
