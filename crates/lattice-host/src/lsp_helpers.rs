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

/// 5.5.LSP.5: single-character glyph for an LSP `SymbolKind`.
/// Picked to fit a fixed-width column in picker rows so the
/// marginalia column stays aligned. Falls back to `?` for kinds
/// we don't have a specific glyph for.
pub fn symbol_kind_glyph(kind: lattice_lsp::lsp_types::SymbolKind) -> &'static str {
    use lattice_lsp::lsp_types::SymbolKind as K;
    match kind {
        K::FILE => "📄",
        K::MODULE | K::NAMESPACE | K::PACKAGE => "📦",
        K::CLASS | K::INTERFACE => "🅒",
        K::METHOD | K::FUNCTION => "ƒ",
        K::CONSTRUCTOR => "🅒",
        K::PROPERTY | K::FIELD => "•",
        K::VARIABLE => "v",
        K::CONSTANT => "K",
        K::STRING | K::NUMBER | K::BOOLEAN | K::ARRAY | K::OBJECT => "≡",
        K::ENUM | K::ENUM_MEMBER => "🅔",
        K::STRUCT => "🅢",
        K::EVENT => "🅔",
        K::OPERATOR => "⊕",
        K::TYPE_PARAMETER => "T",
        _ => "?",
    }
}

/// 5.5.LSP.5: project an LSP `SymbolInformation` (legacy outline +
/// workspace-symbol shape) into a `SymbolRow`. Returns `None`
/// when the location's URI doesn't resolve to a path.
pub fn symbol_information_to_row(
    sym: &lattice_lsp::lsp_types::SymbolInformation,
) -> Option<lattice_lsp::cache::SymbolRow> {
    let path = lattice_lsp::actor::uri_to_path(&sym.location.uri)?;
    Some(lattice_lsp::cache::SymbolRow {
        name: sym.name.clone(),
        kind_glyph: symbol_kind_glyph(sym.kind),
        container: sym.container_name.clone(),
        depth: 0,
        path,
        line: sym.location.range.start.line,
        col: sym.location.range.start.character,
    })
}

/// 5.5.LSP.5: flatten an LSP `DocumentSymbolResponse` into a
/// pre-rendered `Vec<SymbolRow>`. The legacy
/// `Flat(Vec<SymbolInformation>)` variant is one row per symbol
/// with no nesting; the modern `Nested(Vec<DocumentSymbol>)`
/// variant carries `children: Vec<DocumentSymbol>`, walked
/// depth-first to preserve outline ordering.
pub fn flatten_document_symbol_response(
    resp: lattice_lsp::lsp_types::DocumentSymbolResponse,
    path: &std::path::Path,
    out: &mut Vec<lattice_lsp::cache::SymbolRow>,
) {
    match resp {
        lattice_lsp::lsp_types::DocumentSymbolResponse::Flat(syms) => {
            for sym in syms {
                if let Some(row) = symbol_information_to_row(&sym) {
                    out.push(row);
                }
            }
        }
        lattice_lsp::lsp_types::DocumentSymbolResponse::Nested(syms) => {
            fn walk(
                syms: Vec<lattice_lsp::lsp_types::DocumentSymbol>,
                path: &std::path::Path,
                depth: u32,
                out: &mut Vec<lattice_lsp::cache::SymbolRow>,
            ) {
                for sym in syms {
                    out.push(lattice_lsp::cache::SymbolRow {
                        name: sym.name.clone(),
                        kind_glyph: symbol_kind_glyph(sym.kind),
                        container: None,
                        depth,
                        path: path.to_path_buf(),
                        line: sym.selection_range.start.line,
                        col: sym.selection_range.start.character,
                    });
                    if let Some(children) = sym.children {
                        walk(children, path, depth + 1, out);
                    }
                }
            }
            walk(syms, path, 0, out);
        }
    }
}

/// 5.5.LSP.5: convert a modern (LSP 3.17+) `WorkspaceSymbol` into
/// a `SymbolRow`. When the symbol's `location` came back as the
/// `WorkspaceLocation` (URI-only) variant, fires
/// `workspaceSymbol/resolve` against the originating server to
/// upgrade to a real `Location` with `range`. Returns `None` when
/// the URI doesn't map to a path; resolve failures fall back to
/// `(0, 0)` so the row stays navigable.
pub async fn workspace_symbol_to_row(
    handle: &lattice_lsp::ServerHandle,
    sym: lattice_lsp::lsp_types::WorkspaceSymbol,
    token: &lattice_protocol::CancellationToken,
) -> Option<lattice_lsp::cache::SymbolRow> {
    use lattice_lsp::lsp_types::OneOf;
    let (path, line, col) = match &sym.location {
        OneOf::Left(loc) => (
            lattice_lsp::actor::uri_to_path(&loc.uri)?,
            loc.range.start.line,
            loc.range.start.character,
        ),
        OneOf::Right(wsl) => {
            let path = lattice_lsp::actor::uri_to_path(&wsl.uri)?;
            // Server's resolveProvider absent -> no point firing.
            // Fall back to (0, 0); the user can still navigate to
            // the file.
            if !handle.capabilities().workspace_symbol_resolve_provider() {
                (path, 0, 0)
            } else {
                match handle
                    .workspace_symbol_resolve(sym.clone(), token.clone())
                    .await
                {
                    Ok(resolved) => match resolved.location {
                        OneOf::Left(loc) => (
                            lattice_lsp::actor::uri_to_path(&loc.uri).unwrap_or(path),
                            loc.range.start.line,
                            loc.range.start.character,
                        ),
                        OneOf::Right(_) => (path, 0, 0),
                    },
                    Err(_) => (path, 0, 0),
                }
            }
        }
    };
    Some(lattice_lsp::cache::SymbolRow {
        name: sym.name,
        kind_glyph: symbol_kind_glyph(sym.kind),
        container: sym.container_name,
        depth: 0,
        path,
        line,
        col,
    })
}

/// 5.5.LSP.4: render an LSP `SignatureHelp` payload to a markdown
/// string the popup renderer can display. Picks the active
/// signature (server-supplied `active_signature` index, default
/// 0) and inlines the active parameter's documentation when
/// present. Returns the empty string when the response carries no
/// signatures -- the caller surfaces "no signature info".
pub fn signature_help_to_markdown(sh: &lattice_lsp::lsp_types::SignatureHelp) -> String {
    if sh.signatures.is_empty() {
        return String::new();
    }
    let active_sig_idx = sh.active_signature.unwrap_or(0) as usize;
    let sig = sh
        .signatures
        .get(active_sig_idx)
        .or_else(|| sh.signatures.first())
        .expect("non-empty checked above");
    let mut out = String::new();
    // Active signature's call form -- fenced code block so the
    // popup's markdown highlighter picks up syntax highlighting.
    out.push_str("```text\n");
    out.push_str(&sig.label);
    out.push_str("\n```\n");
    // Parameter highlight: append a short note pointing at the
    // active parameter's name.
    if let Some(active_param_idx) = sig.active_parameter.or(sh.active_parameter)
        && let Some(params) = sig.parameters.as_ref()
        && let Some(param) = params.get(active_param_idx as usize)
    {
        let label_str = match &param.label {
            lattice_lsp::lsp_types::ParameterLabel::Simple(s) => s.clone(),
            lattice_lsp::lsp_types::ParameterLabel::LabelOffsets(_) => String::new(),
        };
        if !label_str.is_empty() {
            out.push_str(&format!("\n**param:** `{label_str}`\n"));
        }
        if let Some(doc) = param.documentation.as_ref() {
            let doc_str = match doc {
                lattice_lsp::lsp_types::Documentation::String(s) => s.clone(),
                lattice_lsp::lsp_types::Documentation::MarkupContent(mc) => mc.value.clone(),
            };
            if !doc_str.is_empty() {
                out.push('\n');
                out.push_str(&doc_str);
                out.push('\n');
            }
        }
    }
    // Signature-level documentation when present.
    if let Some(doc) = sig.documentation.as_ref() {
        let doc_str = match doc {
            lattice_lsp::lsp_types::Documentation::String(s) => s.clone(),
            lattice_lsp::lsp_types::Documentation::MarkupContent(mc) => mc.value.clone(),
        };
        if !doc_str.is_empty() {
            out.push('\n');
            out.push_str(&doc_str);
            out.push('\n');
        }
    }
    out
}

/// 5.5.LSP.4: single-character glyph for an LSP
/// `CompletionItemKind`. Same shape as `symbol_kind_glyph` but
/// maps the completion-item kind enum (which is wider -- snippets,
/// keywords, folders, etc.). Used by the LSP completion picker /
/// insert-completion overlay row marginalia.
pub fn completion_kind_glyph(
    kind: Option<lattice_lsp::lsp_types::CompletionItemKind>,
) -> &'static str {
    use lattice_lsp::lsp_types::CompletionItemKind as K;
    match kind {
        Some(K::FUNCTION) | Some(K::METHOD) | Some(K::CONSTRUCTOR) => "ƒ",
        Some(K::VARIABLE) | Some(K::FIELD) | Some(K::PROPERTY) => "v",
        Some(K::CONSTANT) => "K",
        Some(K::CLASS) | Some(K::INTERFACE) => "🅒",
        Some(K::STRUCT) => "🅢",
        Some(K::ENUM) | Some(K::ENUM_MEMBER) => "🅔",
        Some(K::MODULE) => "📦",
        Some(K::FILE) | Some(K::FOLDER) => "📄",
        Some(K::SNIPPET) => "✂",
        Some(K::KEYWORD) => "K",
        Some(K::TEXT) => "≡",
        Some(K::REFERENCE) => "→",
        _ => "?",
    }
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
    resp: lattice_lsp::lsp_types::GotoDefinitionResponse,
) -> Vec<lattice_lsp::lsp_types::Location> {
    match resp {
        lattice_lsp::lsp_types::GotoDefinitionResponse::Scalar(loc) => vec![loc],
        lattice_lsp::lsp_types::GotoDefinitionResponse::Array(locs) => locs,
        lattice_lsp::lsp_types::GotoDefinitionResponse::Link(links) => links
            .into_iter()
            .map(|l| lattice_lsp::lsp_types::Location {
                uri: l.target_uri,
                // `target_selection_range` is the narrower symbol
                // range; `target_range` is the enclosing block.
                // Picker UX usually wants the narrower one.
                range: l.target_selection_range,
            })
            .collect(),
    }
}

/// The LSP range a request about a Visual selection covers.
///
/// Visual is head-inclusive: `vl` over `ab` selects both characters, so the
/// LSP range (end-exclusive) ends one character PAST the later end. Linewise
/// covers whole lines, start of the first to end of the last, which is what
/// an extract-function server wants from a `V` selection. Blockwise has no
/// LSP shape and asks about its bounding characters, as charwise does.
///
/// RF.1. The earlier inline version ended at the head, so an extract over
/// `a + b` asked about `a + ` -- and for a server, a different expression.
pub fn selection_lsp_range(
    buffer: &Buffer,
    anchor: Position,
    head: Position,
    kind: lattice_grammar::VisualKind,
) -> lattice_lsp::lsp_types::Range {
    let (first, last) = if (anchor.line, anchor.byte) <= (head.line, head.byte) {
        (anchor, head)
    } else {
        (head, anchor)
    };
    let origin = lattice_lsp::lsp_types::Position {
        line: 0,
        character: 0,
    };
    let (start, end) = if kind == lattice_grammar::VisualKind::Linewise {
        let end_byte = buffer
            .line(last.line)
            .map_or(0, |l| u32::try_from(l.len()).unwrap_or(u32::MAX));
        (
            Position {
                line: first.line,
                byte: 0,
            },
            Position {
                line: last.line,
                byte: end_byte,
            },
        )
    } else {
        // One character past the inclusive end; an empty line, or a head
        // already past the end, stays where it is.
        let past = buffer
            .line(last.line)
            .and_then(|l| {
                l.get(last.byte as usize..)
                    .and_then(|rest: &str| rest.chars().next())
                    .map(|c: char| last.byte + c.len_utf8() as u32)
            })
            .unwrap_or(last.byte);
        (
            first,
            Position {
                line: last.line,
                byte: past,
            },
        )
    };
    let start = app_to_lsp_position(buffer, start).unwrap_or(origin);
    let end = app_to_lsp_position(buffer, end).unwrap_or(start);
    lattice_lsp::lsp_types::Range { start, end }
}

/// Does `action` belong in a code-action picker narrowed to `only`?
/// Everything does when unfiltered; filtered, an action belongs when its
/// kind is in the family, and a bare `Command` (which has no kind) never
/// does. RF.1.
pub fn code_action_admitted(
    action: &lattice_lsp::lsp_types::CodeActionOrCommand,
    only: Option<lattice_grammar::CodeActionKindFilter>,
) -> bool {
    let Some(filter) = only else {
        return true;
    };
    match action {
        lattice_lsp::lsp_types::CodeActionOrCommand::Command(_) => false,
        lattice_lsp::lsp_types::CodeActionOrCommand::CodeAction(ca) => {
            ca.kind.as_ref().is_some_and(|k| filter.admits(k.as_str()))
        }
    }
}

/// Convert an editor-side `Position` (line + utf-8 byte column)
/// into the LSP-side `lattice_lsp::lsp_types::Position` (line + utf-16 code-
/// unit column). Returns `None` when the line index is past the
/// end of the buffer -- e.g. cursor on a sentinel row past EOF.
pub fn app_to_lsp_position(
    buffer: &Buffer,
    p: Position,
) -> Option<lattice_lsp::lsp_types::Position> {
    let line_text = buffer.line(p.line)?;
    let character = lattice_lsp::position::utf8_byte_to_utf16_column(&line_text, p.byte);
    Some(lattice_lsp::lsp_types::Position {
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
/// 4.5.c: does the given `range` cover the LSP `position`?
/// Inclusive on both ends (matches VSCode's click-through
/// semantics on a link's rightmost char). Used by `gx` to find
/// the first cached `documentLink` under the cursor. Phase
/// 5.8.AD.2.
pub fn range_covers(
    range: lattice_lsp::lsp_types::Range,
    position: lattice_lsp::lsp_types::Position,
) -> bool {
    let after_start =
        (range.start.line, range.start.character) <= (position.line, position.character);
    let before_end = (position.line, position.character) <= (range.end.line, range.end.character);
    after_start && before_end
}

/// Render one `CallHierarchyItem` (the caller / callee) as a
/// `SymbolRow` for the picker. The item's `detail` (if any)
/// plus the originating callable's name ride in `container`.
/// Phase 5.8.AD.2: hoisted from TUI App.
pub fn call_hierarchy_to_row(
    item: &lattice_lsp::lsp_types::CallHierarchyItem,
    related_to: &str,
) -> lattice_lsp::cache::SymbolRow {
    let path = lattice_lsp::actor::uri_to_path(&item.uri).unwrap_or_default();
    lattice_lsp::cache::SymbolRow {
        name: item.name.clone(),
        kind_glyph: symbol_kind_glyph(item.kind),
        container: Some(format!("in {related_to}")),
        depth: 0,
        path,
        line: item.selection_range.start.line,
        col: item.selection_range.start.character,
    }
}

/// 4.5.b: render one `TypeHierarchyItem` (a supertype or
/// subtype of the cursor's type) as a [`SymbolRow`] for the
/// picker. Same projection as `call_hierarchy_to_row` because
/// the item shape is identical. Phase 5.8.AD.2.
pub fn type_hierarchy_to_row(
    item: &lattice_lsp::lsp_types::TypeHierarchyItem,
    related_to: &str,
) -> lattice_lsp::cache::SymbolRow {
    let path = lattice_lsp::actor::uri_to_path(&item.uri).unwrap_or_default();
    lattice_lsp::cache::SymbolRow {
        name: item.name.clone(),
        kind_glyph: symbol_kind_glyph(item.kind),
        container: Some(format!("of {related_to}")),
        depth: 0,
        path,
        line: item.selection_range.start.line,
        col: item.selection_range.start.character,
    }
}

/// Map an LSP `CodeActionKind` to a one-char glyph used in the
/// picker margin (`🛠`, `♻`, `↗`, etc.). Unknown / custom kinds
/// and bare `Command` payloads land on `?`. Phase 5.8.AD.2.
pub fn code_action_kind_glyph(
    kind: Option<&lattice_lsp::lsp_types::CodeActionKind>,
) -> &'static str {
    use lattice_lsp::lsp_types::CodeActionKind as K;
    let Some(kind) = kind else {
        return "?";
    };
    if *kind == K::QUICKFIX {
        "🛠"
    } else if *kind == K::REFACTOR {
        "♻"
    } else if *kind == K::REFACTOR_EXTRACT {
        "↗"
    } else if *kind == K::REFACTOR_INLINE {
        "↘"
    } else if *kind == K::REFACTOR_REWRITE {
        "↺"
    } else if *kind == K::SOURCE {
        "★"
    } else if *kind == K::SOURCE_ORGANIZE_IMPORTS {
        "≡"
    } else if *kind == K::SOURCE_FIX_ALL {
        "✓"
    } else {
        "?"
    }
}

/// Extract a placeholder name from a `PrepareRenameResponse`, if
/// the server supplied one. Used when the user invoked `:rename`
/// without a name. Phase 5.8.AD.2.
pub fn prepare_rename_placeholder(
    resp: &lattice_lsp::lsp_types::PrepareRenameResponse,
) -> Option<String> {
    match resp {
        lattice_lsp::lsp_types::PrepareRenameResponse::RangeWithPlaceholder {
            placeholder, ..
        } => Some(placeholder.clone()),
        lattice_lsp::lsp_types::PrepareRenameResponse::Range(_) => None,
        lattice_lsp::lsp_types::PrepareRenameResponse::DefaultBehavior { .. } => None,
    }
}

/// Is the editor cursor inside the half-open LSP `range`?
/// Compares only by (line, character) in LSP utf-16 space because
/// the range is LSP-shape; the host cursor is assumed converted.
/// Phase 5.8.AD.2: hoisted from TUI App.
pub fn cursor_inside_range(
    cursor: lattice_protocol::Position,
    range: &lattice_lsp::lsp_types::Range,
) -> bool {
    let line = cursor.line;
    let col = cursor.byte;
    let start = (range.start.line, range.start.character);
    let end = (range.end.line, range.end.character);
    let here = (line, col);
    here >= start && here < end
}

/// 4.4.e: flatten an LSP `SelectionRange` (linked list via
/// `parent`) into a `Vec<Range>` ordered innermost-first.
/// Phase 5.8.AD.2: hoisted from TUI App.
pub fn flatten_selection_range_chain(
    head: &lattice_lsp::lsp_types::SelectionRange,
) -> Vec<lattice_lsp::lsp_types::Range> {
    let mut out = Vec::new();
    let mut cur = Some(head);
    while let Some(node) = cur {
        out.push(node.range);
        cur = node.parent.as_deref();
    }
    out
}

pub fn hover_contents_to_markdown(contents: &lattice_lsp::lsp_types::HoverContents) -> String {
    fn marked_to_markdown(m: &lattice_lsp::lsp_types::MarkedString) -> String {
        match m {
            lattice_lsp::lsp_types::MarkedString::String(s) => s.clone(),
            lattice_lsp::lsp_types::MarkedString::LanguageString(ls) => {
                format!("```{}\n{}\n```", ls.language, ls.value)
            }
        }
    }
    match contents {
        lattice_lsp::lsp_types::HoverContents::Scalar(m) => marked_to_markdown(m),
        lattice_lsp::lsp_types::HoverContents::Array(items) => items
            .iter()
            .map(marked_to_markdown)
            .collect::<Vec<_>>()
            .join("\n\n"),
        lattice_lsp::lsp_types::HoverContents::Markup(m) => m.value.clone(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_grammar::{CodeActionKindFilter, VisualKind};
    use lattice_lsp::lsp_types::{
        CodeAction, CodeActionKind, CodeActionOrCommand, Command, Position as LspPos, Range,
    };

    fn at(line: u32, byte: u32) -> Position {
        Position { line, byte }
    }

    fn lsp(sl: u32, sc: u32, el: u32, ec: u32) -> Range {
        Range {
            start: LspPos {
                line: sl,
                character: sc,
            },
            end: LspPos {
                line: el,
                character: ec,
            },
        }
    }

    /// `v` over `a + b` (head on the `b`) asks about all of `a + b`.
    #[test]
    fn a_charwise_selection_includes_the_character_under_the_head() {
        let buf = Buffer::from_text("let x = a + b;\n");
        let r = selection_lsp_range(&buf, at(0, 8), at(0, 12), VisualKind::Charwise);
        assert_eq!(r, lsp(0, 8, 0, 13));
        // Backwards (head before anchor) is the same range.
        let r = selection_lsp_range(&buf, at(0, 12), at(0, 8), VisualKind::Charwise);
        assert_eq!(r, lsp(0, 8, 0, 13));
    }

    /// The head's character is counted in UTF-16, as LSP columns are.
    #[test]
    fn the_inclusive_end_steps_over_a_whole_character() {
        // `é` is two UTF-8 bytes and one UTF-16 unit.
        let buf = Buffer::from_text("aé b\n");
        let r = selection_lsp_range(&buf, at(0, 0), at(0, 1), VisualKind::Charwise);
        assert_eq!(r, lsp(0, 0, 0, 2));
    }

    /// `V` asks about whole lines, wherever the cursor sat in them.
    #[test]
    fn a_linewise_selection_covers_whole_lines() {
        let buf = Buffer::from_text("fn f() {\n    one();\n    two();\n}\n");
        let r = selection_lsp_range(&buf, at(1, 6), at(2, 4), VisualKind::Linewise);
        assert_eq!(r, lsp(1, 0, 2, 10));
    }

    #[test]
    fn a_selection_on_an_empty_line_stays_in_bounds() {
        let buf = Buffer::from_text("a\n\nb\n");
        let r = selection_lsp_range(&buf, at(1, 0), at(1, 0), VisualKind::Charwise);
        assert_eq!(r, lsp(1, 0, 1, 0));
    }

    fn action(kind: Option<&str>) -> CodeActionOrCommand {
        CodeActionOrCommand::CodeAction(CodeAction {
            title: "t".into(),
            kind: kind.map(|k| CodeActionKind::from(k.to_string())),
            ..Default::default()
        })
    }

    /// Servers return kinds outside `context.only`; the client drops them.
    #[test]
    fn a_filter_admits_its_family_and_nothing_else() {
        let extract = Some(CodeActionKindFilter::Extract);
        assert!(code_action_admitted(
            &action(Some("refactor.extract.function")),
            extract
        ));
        assert!(!code_action_admitted(&action(Some("quickfix")), extract));
        assert!(!code_action_admitted(
            &action(Some("refactor.inline")),
            extract
        ));
        assert!(!code_action_admitted(&action(None), extract));
        let command = CodeActionOrCommand::Command(Command {
            title: "t".into(),
            command: "c".into(),
            arguments: None,
        });
        assert!(
            !code_action_admitted(&command, extract),
            "a Command has no kind"
        );
        // Unfiltered, everything stays.
        assert!(code_action_admitted(&command, None));
        assert!(code_action_admitted(&action(Some("quickfix")), None));
    }
}
