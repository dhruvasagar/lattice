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

/// 5.5.LSP.5: project an LSP `SymbolInformation` (legacy outline
/// + workspace-symbol shape) into a `SymbolRow`. Returns `None`
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
